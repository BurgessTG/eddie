//! Headless screenshot render for slice verification.
//!
//! Test-only and opt-in (`OPENREEL_SCREENSHOT=1`). The sandbox has
//! no compositor socket, no Xvfb, and no GPU, so this renders the
//! real panel code through a headless egui `Context` and rasterizes
//! the tessellated frame in software. It is the app's own UI code
//! and theme; only the window/raster backend differs from a desktop
//! capture. Output is PPM under /tmp (convert with ffmpeg).

use std::collections::HashMap;
use std::path::Path;

use eframe::egui::epaint::{ImageData, Primitive};
use eframe::egui::TexturesDelta;
use eframe::egui::{self, Color32, RawInput, Rect, TextureId, Vec2};
use openreel_engine::ClipKind;

use super::chat::{PendingChat, TimelineSnapshot};
use super::session::{ChatRole, ChatSession};
use super::EditorApp;

const W: usize = 1440;
const H: usize = 900;
const SEED_PATH: &str = "/tmp/openreel-shot/config/openreel/chat_session.json";

#[derive(Clone)]
struct Texture {
    size: [usize; 2],
    /// Un-multiplied sRGBA, 0..=1 per channel.
    pixels: Vec<[f32; 4]>,
}

fn to_f32(color: Color32) -> [f32; 4] {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ]
}

fn apply_textures(store: &mut HashMap<TextureId, Texture>, delta: &TexturesDelta) {
    for (id, image_delta) in &delta.set {
        let ImageData::Color(image) = &image_delta.image;
        let patch: Vec<[f32; 4]> = image.pixels.iter().map(|p| to_f32(*p)).collect();
        match image_delta.pos {
            None => {
                store.insert(
                    *id,
                    Texture {
                        size: image.size,
                        pixels: patch,
                    },
                );
            }
            Some([x0, y0]) => {
                let entry = store.entry(*id).or_insert_with(|| Texture {
                    size: [x0 + image.size[0], y0 + image.size[1]],
                    pixels: vec![[0.0; 4]; (x0 + image.size[0]) * (y0 + image.size[1])],
                });
                // Grow if the atlas expanded underneath the patch.
                let need = [x0 + image.size[0], y0 + image.size[1]];
                if entry.size[0] < need[0] || entry.size[1] < need[1] {
                    let new_size = [entry.size[0].max(need[0]), entry.size[1].max(need[1])];
                    let mut grown = vec![[0.0; 4]; new_size[0] * new_size[1]];
                    for y in 0..entry.size[1] {
                        for x in 0..entry.size[0] {
                            grown[y * new_size[0] + x] = entry.pixels[y * entry.size[0] + x];
                        }
                    }
                    entry.size = new_size;
                    entry.pixels = grown;
                }
                for y in 0..image.size[1] {
                    for x in 0..image.size[0] {
                        entry.pixels[(y0 + y) * entry.size[0] + (x0 + x)] =
                            patch[y * image.size[0] + x];
                    }
                }
            }
        }
    }
}

fn sample(texture: &Texture, u: f32, v: f32) -> [f32; 4] {
    let (w, h) = (texture.size[0] as f32, texture.size[1] as f32);
    if w < 1.0 || h < 1.0 {
        return [1.0; 4];
    }
    // Bilinear, texel-center addressing, clamped.
    let x = (u * w - 0.5).clamp(0.0, w - 1.0);
    let y = (v * h - 0.5).clamp(0.0, h - 1.0);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = (
        (x0 + 1).min(texture.size[0] - 1),
        (y0 + 1).min(texture.size[1] - 1),
    );
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let px = |x: usize, y: usize| texture.pixels[y * texture.size[0] + x];
    core::array::from_fn(|c| {
        (px(x0, y0)[c] * (1.0 - fx) + px(x1, y0)[c] * fx) * (1.0 - fy)
            + (px(x0, y1)[c] * (1.0 - fx) + px(x1, y1)[c] * fx) * fy
    })
}

fn rasterize(
    primitives: &[egui::ClippedPrimitive],
    textures: &HashMap<TextureId, Texture>,
) -> Vec<[f32; 3]> {
    let mut fb = vec![[9.0 / 255.0, 9.0 / 255.0, 11.0 / 255.0]; W * H];
    for prim in primitives {
        let Primitive::Mesh(mesh) = &prim.primitive else {
            continue;
        };
        let Some(texture) = textures.get(&mesh.texture_id) else {
            continue;
        };
        let clip = prim.clip_rect;
        let clip_min = [
            clip.min.x.floor().max(0.0) as usize,
            clip.min.y.floor().max(0.0) as usize,
        ];
        let clip_max = [
            (clip.max.x.ceil() as usize).min(W),
            (clip.max.y.ceil() as usize).min(H),
        ];
        let pos: Vec<[f32; 2]> = mesh.vertices.iter().map(|v| [v.pos.x, v.pos.y]).collect();
        let uv: Vec<[f32; 2]> = mesh.vertices.iter().map(|v| [v.uv.x, v.uv.y]).collect();
        let col: Vec<[f32; 4]> = mesh.vertices.iter().map(|v| to_f32(v.color)).collect();
        for tri in mesh.indices.chunks_exact(3) {
            let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            let (p0, p1, p2) = (pos[i0], pos[i1], pos[i2]);
            let area = (p1[0] - p0[0]) * (p2[1] - p0[1]) - (p2[0] - p0[0]) * (p1[1] - p0[1]);
            if area.abs() < 1e-9 {
                continue;
            }
            let min_x = p0[0].min(p1[0]).min(p2[0]).floor().max(0.0) as usize;
            let max_x = (p0[0].max(p1[0]).max(p2[0]).ceil() as usize).min(W);
            let min_y = p0[1].min(p1[1]).min(p2[1]).floor().max(0.0) as usize;
            let max_y = (p0[1].max(p1[1]).max(p2[1]).ceil() as usize).min(H);
            let (min_x, max_x) = (min_x.max(clip_min[0]), max_x.min(clip_max[0]));
            let (min_y, max_y) = (min_y.max(clip_min[1]), max_y.min(clip_max[1]));
            for y in min_y..max_y {
                for x in min_x..max_x {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let w0 = ((p1[0] - px) * (p2[1] - py) - (p2[0] - px) * (p1[1] - py)) / area;
                    let w1 = ((p2[0] - px) * (p0[1] - py) - (p0[0] - px) * (p2[1] - py)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                        continue;
                    }
                    let su = uv[i0][0] * w0 + uv[i1][0] * w1 + uv[i2][0] * w2;
                    let sv = uv[i0][1] * w0 + uv[i1][1] * w1 + uv[i2][1] * w2;
                    let texel = sample(texture, su, sv);
                    let vc = [
                        col[i0][0] * w0 + col[i1][0] * w1 + col[i2][0] * w2,
                        col[i0][1] * w0 + col[i1][1] * w1 + col[i2][1] * w2,
                        col[i0][2] * w0 + col[i1][2] * w1 + col[i2][2] * w2,
                        col[i0][3] * w0 + col[i1][3] * w1 + col[i2][3] * w2,
                    ];
                    let a = (texel[3] * vc[3]).clamp(0.0, 1.0);
                    let dst = &mut fb[y * W + x];
                    for c in 0..3 {
                        dst[c] = texel[c] * vc[c] * a + dst[c] * (1.0 - a);
                    }
                }
            }
        }
    }
    fb
}

fn write_ppm(path: &Path, fb: &[[f32; 3]]) {
    let mut bytes = format!("P6\n{W} {H}\n255\n").into_bytes();
    for pixel in fb {
        bytes.extend(pixel.iter().map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8));
    }
    std::fs::write(path, bytes).expect("write ppm");
}

fn run_frames(
    ctx: &egui::Context,
    app: &mut EditorApp,
    textures: &mut HashMap<TextureId, Texture>,
    clock: &mut f64,
) -> Vec<egui::ClippedPrimitive> {
    run_frames_with_events(ctx, app, textures, clock, Vec::new())
}

fn run_frames_with_events(
    ctx: &egui::Context,
    app: &mut EditorApp,
    textures: &mut HashMap<TextureId, Texture>,
    clock: &mut f64,
    event_sets: Vec<Vec<egui::Event>>,
) -> Vec<egui::ClippedPrimitive> {
    run_frames_n(ctx, app, textures, clock, event_sets, 14)
}

fn run_frames_n(
    ctx: &egui::Context,
    app: &mut EditorApp,
    textures: &mut HashMap<TextureId, Texture>,
    clock: &mut f64,
    event_sets: Vec<Vec<egui::Event>>,
    min_frames: usize,
) -> Vec<egui::ClippedPrimitive> {
    let total = event_sets.len().max(min_frames);
    let mut out = None;
    for frame in 0..total {
        *clock += 1.0 / 60.0;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(W as f32, H as f32),
            )),
            time: Some(*clock),
            max_texture_side: Some(4096),
            events: event_sets.get(frame).cloned().unwrap_or_default(),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            let mut action = app.menu_bar(ctx);
            app.status_bar(ctx);
            app.console_panel(ctx);
            app.timeline_panel(ctx);
            // Chat now lives in the right dock (Chat tab default).
            app.chat_settings_window(ctx);
            if let Some(a) = app.media_bin(ctx) {
                action = Some(a);
            }
            app.right_dock(ctx);
            app.central(ctx);
            let _ = action;
        });
        apply_textures(textures, &output.textures_delta);
        out = Some(ctx.tessellate(output.shapes, 1.0));
    }
    out.expect("frames ran")
}

fn seeded_session() -> ChatSession {
    let seed = Path::new(SEED_PATH);
    if seed.exists() {
        let session = ChatSession::load_from(seed);
        if !session.messages.is_empty() {
            return session;
        }
    }
    let mut session = ChatSession::new("openreel-project-1");
    for (role, text) in [
        (
            ChatRole::User,
            "Cut this down to a tight 20-second cut and put the title 'Highlights' at the start.",
        ),
        (
            ChatRole::Assistant,
            "Done — trimmed to a 20-second cut with a title.",
        ),
        (ChatRole::User, "Build it from the reaction shots."),
        (
            ChatRole::Assistant,
            "Built the cut from the reaction shots.",
        ),
        (ChatRole::User, "Use the part where he talks about pricing."),
        (ChatRole::Assistant, "Done — used the pricing section."),
        (ChatRole::User, "Reactions first"),
        (
            ChatRole::Assistant,
            "Done — arranged 6 sweep clips (no model call).",
        ),
    ] {
        session.push(role, text.to_owned());
    }
    session
}

#[test]
fn render_slice6_screenshots() {
    if std::env::var("OPENREEL_SCREENSHOT").as_deref() != Ok("1") {
        eprintln!("SKIP screenshot render: set OPENREEL_SCREENSHOT=1");
        return;
    }
    let ctx = egui::Context::default();
    let mut app = EditorApp::headless(&ctx);
    // A small representative edit so timeline/inspector have content.
    let video = app
        .timeline
        .add_clip("sample_video.mp4", ClipKind::Video, 1, 0.0, 20.0, 0.0);
    app.timeline
        .add_clip("sample_music.mp3", ClipKind::Audio, 2, 0.0, 14.4, 0.0);
    app.timeline
        .add_clip("Highlights", ClipKind::Title, 0, 1.0, 4.0, 0.0);
    app.selected = Some(video);
    app.chat_session = seeded_session();
    app.chat_undo = Some(TimelineSnapshot::capture(&app.timeline, app.selected, 0.0));
    app.status = "Session restored.".to_owned();

    let mut textures = HashMap::new();
    let mut clock = 0.0_f64;
    run_frames(&ctx, &mut app, &mut textures, &mut clock);
    let primitives = run_frames_n(&ctx, &mut app, &mut textures, &mut clock, Vec::new(), 2);
    let fb = rasterize(&primitives, &textures);
    write_ppm(Path::new("/tmp/openreel_slice6_long_session.ppm"), &fb);

    // Second scene: an in-flight model call (progress + Cancel).
    app.chat_pending = Some(PendingChat::for_test(
        "Build the montage",
        "Ranking clips…",
        &app.timeline,
        app.script_state_json(),
    ));
    app.chat_generation = 1;
    let primitives = run_frames_n(&ctx, &mut app, &mut textures, &mut clock, Vec::new(), 2);
    let fb = rasterize(&primitives, &textures);
    write_ppm(Path::new("/tmp/openreel_slice6_inflight.ppm"), &fb);
}

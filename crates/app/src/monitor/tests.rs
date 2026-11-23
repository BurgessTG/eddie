use openreel_engine::{BlendMode, ClipKind, Property};
use rune::{Key, Runtime};

use crate::app::Ui;
use crate::testing::{FRAME, click, frames, press, shown, window};

#[test]
fn double_clicking_a_caption_opens_its_text_for_typing() {
    let mut ui = Ui::headless();
    let text = "A person talking stays a person talking";
    let title = ui
        .editor
        .timeline
        .add_clip(text, ClipKind::Title, 0, 0.0, 4.0, 0.0);
    ui.editor.playhead = 1.0;
    ui.show_inspector = false;
    let mut rt = window(ui);
    let (x, y) = rt.find("monitor-title").expect("the caption is shown");
    rt.pointer_move(x, y);
    rt.frame(FRAME);
    for _ in 0..2 {
        rt.pointer_down(x, y);
        rt.frame(FRAME);
        rt.pointer_up(x, y);
        rt.frame(FRAME);
    }
    frames(&mut rt, 2);
    assert_eq!(rt.state.editor.selected, Some(title));
    assert!(rt.state.show_inspector, "Details opens for it");
    assert!(rt.state.typing(), "the text box has the keyboard");

    rt.key(rune::KeyEvent {
        key: Key::Text(" now".into()),
        mods: rune::Mods::default(),
    });
    frames(&mut rt, 2);
    assert_eq!(
        rt.state.editor.timeline.clip(title).unwrap().name,
        format!("{text} now"),
        "typing goes into the focused text box"
    );
}

/// Click the readout, type `spec` over the selected time, press Enter.
fn jump(rt: &mut Runtime<Ui>, spec: &str) {
    click(rt, "playhead-readout");
    assert!(shown(rt, "jump-box"), "a click opens the jump box");
    assert!(rt.state.typing());
    rt.paste(spec);
    rt.frame(FRAME);
    press(rt, Key::Enter);
}

#[test]
fn the_playhead_readout_jumps_to_a_typed_time() {
    let mut ui = Ui::headless();
    ui.editor
        .timeline
        .add_clip("Intro", ClipKind::Title, 0, 0.0, 8.0, 0.0);
    let outro = ui
        .editor
        .timeline
        .add_clip("Outro", ClipKind::Title, 0, 8.0, 4.0, 0.0);
    let mut rt = window(ui);
    jump(&mut rt, &format!("clip:{outro}@50%"));
    let playhead = rt.state.editor.playhead;
    assert!((playhead - 10.0).abs() < 1e-6, "got {playhead}");
    assert!(!shown(&rt, "jump-box"), "Enter closes the box");
    assert!(!rt.state.typing(), "and gives the keys back");

    jump(&mut rt, "clip:99");
    assert!(
        (rt.state.editor.playhead - 10.0).abs() < 1e-6,
        "a bad time does not move the playhead"
    );
    let status = &rt.state.editor.status;
    assert!(status.contains("no clip 99"), "{status}");

    // Escape leaves the playhead where it was.
    click(&mut rt, "playhead-readout");
    rt.paste("2");
    rt.frame(FRAME);
    press(&mut rt, Key::Escape);
    assert!(!shown(&rt, "jump-box"), "Esc closes the box");
    assert!((rt.state.editor.playhead - 10.0).abs() < 1e-6);
}

/// The scrubber spans the whole edit: a press a quarter of the way
/// along moves the playhead a quarter of the way through it.
#[test]
fn a_press_on_the_scrubber_seeks_in_proportion() {
    let mut ui = Ui::headless();
    ui.editor
        .timeline
        .add_clip("Intro", ClipKind::Title, 0, 0.0, 8.0, 0.0);
    let mut rt = window(ui);
    let ([x, y, w, h], _) = rt.presented("transport-scrub").expect("the scrubber");
    let y = y + h / 2.0;
    rt.pointer_move(x + w * 0.25, y);
    rt.frame(FRAME);
    rt.pointer_down(x + w * 0.25, y);
    rt.frame(FRAME);
    let playhead = rt.state.editor.playhead;
    assert!((playhead - 2.0).abs() < 0.1, "25% is 2 s, got {playhead}");
    assert!(rt.state.editor.transport_scrubbing, "held");

    rt.pointer_move(x + w * 0.75, y);
    rt.frame(FRAME);
    let playhead = rt.state.editor.playhead;
    assert!((playhead - 6.0).abs() < 0.1, "75% is 6 s, got {playhead}");

    rt.pointer_up(x + w * 0.75, y);
    frames(&mut rt, 2);
    assert!(
        !rt.state.editor.transport_scrubbing,
        "release ends the scrub"
    );
}

#[test]
fn flips_mirror_rows_and_columns() {
    use super::picture::flipped;
    // a b / c d, each pixel's four bytes standing for its letter.
    let image: Vec<u8> = [1, 2, 3, 4].into_iter().flat_map(|v: u8| [v; 4]).collect();
    let order = |rgba: Vec<u8>| {
        rgba.as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[0])
            .collect::<Vec<_>>()
    };
    assert_eq!(order(flipped(&image, 2, (false, false))), [1, 2, 3, 4]);
    assert_eq!(order(flipped(&image, 2, (true, false))), [2, 1, 4, 3]);
    assert_eq!(order(flipped(&image, 2, (false, true))), [3, 4, 1, 2]);
    assert_eq!(order(flipped(&image, 2, (true, true))), [4, 3, 2, 1]);
}

/// A mask lies on the picture as it shows: the cropped part, mirrored with
/// the flipped pixels the monitor holds.
#[test]
fn a_mask_lies_on_the_cropped_flipped_picture() {
    use super::picture::matte;
    use openreel_engine::{Mask, MaskShape};
    // A band four pixels wide (0.4 of the shorter side) 0.2 to the left of
    // the shown picture's middle, on a 100x10 frame whose left 30% is
    // cropped off.
    let band = Mask {
        x: -0.2,
        width: 0.4,
        height: 5.0,
        ..Mask::new(MaskShape::Rectangle)
    };
    let crop = [0.3, 0.0, 0.0, 0.0];
    let kept =
        |levels: Vec<u8>| -> Vec<usize> { (0..100).filter(|&x| levels[500 + x] > 128).collect() };
    // Shown: frame columns 30 to 100, 70 wide; the band's middle is 0.3 of
    // the way across that, at 51, so it keeps columns 49 to 52.
    assert_eq!(
        kept(matte(band, crop, (false, false), (100, 10))),
        [49, 50, 51, 52]
    );
    // Flipped, the frame's columns 0 to 70 show, mirrored: the band's
    // middle is 0.3 of the way in from their left, at 21.
    assert_eq!(
        kept(matte(band, crop, (true, false), (100, 10))),
        [19, 20, 21, 22]
    );
}

/// A 2 s 320x180 clip made with FFmpeg, removed with its folder.
struct Footage(std::path::PathBuf);

impl Footage {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "eddie-monitor-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let output = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("color=c=red:s=320x180:r=30:d=2")
            .args(["-c:v", "libx264", "-threads:v", "1", "-pix_fmt", "yuv420p"])
            .arg(dir.join("wide.mp4"))
            .output()
            .expect("generate footage");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self(dir)
    }

    fn path(&self) -> String {
        self.0.join("wide.mp4").to_string_lossy().into_owned()
    }

    /// Another 2 s 320x180 clip, all `color`.
    fn color(&self, name: &str, color: &str) -> String {
        let path = self.0.join(name);
        let output = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("color=c={color}:s=320x180:r=30:d=2"))
            .args(["-c:v", "libx264", "-threads:v", "1", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .output()
            .expect("generate footage");
        assert!(output.status.success());
        path.to_string_lossy().into_owned()
    }
}

impl Drop for Footage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run frames until `done` holds, for at most 30 s.
fn until(rt: &mut Runtime<Ui>, what: &str, done: impl Fn(&Ui) -> bool) {
    let started = std::time::Instant::now();
    while !done(&rt.state) {
        assert!(
            started.elapsed().as_secs() < 30,
            "timed out waiting for {what}"
        );
        rt.frame(FRAME);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    frames(rt, 2);
}

#[test]
fn a_moved_scaled_turned_picture_previews_where_export_puts_it() {
    let footage = Footage::new();
    let path = footage.path();
    let mut ui = Ui::headless();
    ui.editor.spawn_import(vec![path.clone().into()]);
    let mut rt = window(ui);
    until(&mut rt, "the import", |ui| {
        ui.editor.asset(&path).is_some() && !ui.editor.importing()
    });
    let editor = &mut rt.state.editor;
    editor.add_asset_to_timeline(&path, None, 0.0);
    let id = editor.timeline.clips[0].id;
    // Half size, its centre a quarter right and a quarter up: on the 320x180
    // canvas that is x 160-320 and y 0-90, the top-right quarter.
    editor.edit_clip_transform(id, |t| {
        t.scale = 0.5;
        t.x = 0.25;
        t.y = -0.25;
    });
    editor.playhead = 0.5;
    until(&mut rt, "the picture", |ui| ui.editor.preview_is_current());

    let share = |rt: &Runtime<Ui>, key: &str| {
        let ([fx, fy, fw, fh], _) = rt.presented("monitor").unwrap();
        let ([x, y, w, h], _) = rt.presented(key).unwrap();
        [(x - fx) / fw, (y - fy) / fh, w / fw, h / fh]
    };
    let close =
        |got: [f32; 4], want: [f32; 4]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.01);
    let placed = share(&rt, "monitor-lane0-placed");
    assert!(close(placed, [0.5, 0.0, 0.5, 0.5]), "{placed:?}");

    // A quarter turn about the same centre stands the box on end: 90x160
    // canvas pixels around (240, 45).
    rt.state
        .editor
        .edit_clip_transform(id, |t| t.rotation = 90.0);
    frames(&mut rt, 2);
    let turned = share(&rt, "monitor-lane0-placed");
    let want = [
        (240.0 - 45.0) / 320.0,
        (45.0 - 80.0) / 180.0,
        90.0 / 320.0,
        160.0 / 180.0,
    ];
    assert!(close(turned, want), "{turned:?} against {want:?}");

    // Cropping the left half keeps the right half, fitted: the box is
    // 80x90 and the whole picture inside it starts one box width left.
    rt.state.editor.edit_clip_transform(id, |t| {
        t.rotation = 0.0;
        t.crop_left = 0.5;
    });
    frames(&mut rt, 2);
    let boxed = share(&rt, "monitor-lane0-placed");
    let whole = share(&rt, "monitor-lane0-picture");
    assert!(close(boxed, [0.625, 0.0, 0.25, 0.5]), "{boxed:?}");
    assert!(close(whole, [0.375, 0.0, 0.5, 0.5]), "{whole:?}");
}

/// Scale and position keys: the monitor draws the picture where the keys
/// put it under the playhead, and a drag on a keyed picture sets a key
/// there that Esc takes back.
#[test]
fn a_keyed_picture_moves_with_the_playhead_and_a_drag_keys_it() {
    let footage = Footage::new();
    let path = footage.path();
    let mut ui = Ui::headless();
    ui.editor.spawn_import(vec![path.clone().into()]);
    let mut rt = window(ui);
    until(&mut rt, "the import", |ui| {
        ui.editor.asset(&path).is_some() && !ui.editor.importing()
    });
    let editor = &mut rt.state.editor;
    editor.add_asset_to_timeline(&path, None, 0.0);
    let id = editor.timeline.clips[0].id;
    let clip = editor.timeline.clip_mut(id).unwrap();
    clip.keyframes.set(Property::Scale, 0.0, 0.5);
    clip.keyframes.set(Property::Scale, 1.0, 1.0);
    clip.keyframes.set(Property::X, 0.0, -0.25);
    clip.keyframes.set(Property::X, 1.0, 0.25);
    editor.playhead = 0.25;
    until(&mut rt, "the picture", |ui| ui.editor.preview_is_current());

    let share = |rt: &Runtime<Ui>| {
        let ([fx, fy, fw, fh], _) = rt.presented("monitor").unwrap();
        let ([x, y, w, h], _) = rt.presented("monitor-lane0-placed").unwrap();
        [(x - fx) / fw, (y - fy) / fh, w / fw, h / fh]
    };
    let close =
        |got: [f32; 4], want: [f32; 4]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.01);
    // A quarter of the way: 62.5% wide, centred 12.5% left of the middle.
    let quarter = share(&rt);
    assert!(
        close(quarter, [0.0625, 0.1875, 0.625, 0.625]),
        "{quarter:?}"
    );
    rt.state.editor.playhead = 0.5;
    frames(&mut rt, 2);
    let half = share(&rt);
    assert!(close(half, [0.125, 0.125, 0.75, 0.75]), "{half:?}");

    // Dragging the picture right by a tenth of the monitor keys x there.
    let ([fx, fy, fw, fh], _) = rt.presented("monitor").unwrap();
    let (x, y) = (fx + fw / 2.0, fy + fh / 2.0);
    crate::testing::input_arrives();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.frame(FRAME);
    rt.pointer_move(x + fw * 0.1, y);
    rt.frame(FRAME);
    let keys = &rt.state.editor.timeline.clip(id).unwrap().keyframes;
    assert_eq!(keys.x.len(), 3);
    assert!((keys.x[1].value - 0.1).abs() < 0.01, "{:?}", keys.x);
    // Esc puts back the keys as they were.
    press(&mut rt, Key::Escape);
    rt.pointer_up(x + fw * 0.1, y);
    frames(&mut rt, 2);
    assert_eq!(
        rt.state.editor.timeline.clip(id).unwrap().keyframes.x.len(),
        2
    );
}

/// A half-size clip on video lane 0 over a whole-canvas clip on lane 1:
/// both show, the top one in its quarter, and a press takes hold of the
/// picture under it, the top one first.
#[test]
fn a_picture_in_picture_shows_over_the_lane_under_it() {
    let footage = Footage::new();
    let path = footage.path();
    let mut ui = Ui::headless();
    ui.editor.spawn_import(vec![path.clone().into()]);
    let mut rt = window(ui);
    until(&mut rt, "the import", |ui| {
        ui.editor.asset(&path).is_some() && !ui.editor.importing()
    });
    let editor = &mut rt.state.editor;
    editor.timeline.lanes.video = 2;
    let base = editor
        .timeline
        .add_media_clip("Base", ClipKind::Video, 1, 0.0, 2.0, 0.0, &path);
    let inset = editor
        .timeline
        .add_media_clip("Inset", ClipKind::Video, 0, 0.0, 2.0, 0.0, &path);
    editor.edit_clip_transform(inset, |t| {
        t.scale = 0.5;
        t.x = 0.25;
        t.y = -0.25;
    });
    editor.playhead = 0.5;
    until(&mut rt, "both pictures", |ui| {
        ui.editor.program_layers().len() == 2 && ui.editor.preview_is_current()
    });
    // The second video lane grows the timeline panel; let the monitor
    // finish shrinking.
    frames(&mut rt, 60);

    let share = |rt: &Runtime<Ui>, key: &str| {
        let ([fx, fy, fw, fh], _) = rt.presented("monitor").unwrap();
        let ([x, y, w, h], _) = rt.presented(key).unwrap();
        [(x - fx) / fw, (y - fy) / fh, w / fw, h / fh]
    };
    let close =
        |got: [f32; 4], want: [f32; 4]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.01);
    let under = share(&rt, "monitor-lane1-placed");
    let over = share(&rt, "monitor-lane0-placed");
    assert!(close(under, [0.0, 0.0, 1.0, 1.0]), "{under:?}");
    assert!(close(over, [0.5, 0.0, 0.5, 0.5]), "{over:?}");

    let editor = &rt.state.editor;
    assert_eq!(editor.picture_at((240.0, 45.0)), Some(inset));
    assert_eq!(editor.picture_at((80.0, 135.0)), Some(base));
}

/// Blue screened over red in the top-right quarter: the monitor lays both
/// lanes as one picture, magenta where they meet as export mixes them, and
/// draws the lanes on their own again once the blend is normal.
#[test]
fn a_blended_picture_mixes_with_the_lane_under_it_on_the_monitor() {
    let footage = Footage::new();
    let red = footage.path();
    let blue = footage.color("blue.mp4", "blue");
    let mut ui = Ui::headless();
    ui.editor
        .spawn_import(vec![red.clone().into(), blue.clone().into()]);
    let mut rt = window(ui);
    until(&mut rt, "the imports", |ui| {
        ui.editor.asset(&blue).is_some()
            && ui.editor.asset(&red).is_some()
            && !ui.editor.importing()
    });
    let editor = &mut rt.state.editor;
    editor.timeline.lanes.video = 2;
    editor
        .timeline
        .add_media_clip("Base", ClipKind::Video, 1, 0.0, 2.0, 0.0, &red);
    let top = editor
        .timeline
        .add_media_clip("Light", ClipKind::Video, 0, 0.0, 2.0, 0.0, &blue);
    editor.edit_clip_transform(top, |t| {
        t.scale = 0.5;
        t.x = 0.25;
        t.y = -0.25;
    });
    editor.set_clip_blend(top, BlendMode::Screen);
    editor.playhead = 0.5;
    until(&mut rt, "both pictures", |ui| {
        ui.editor.program_layers().len() == 2 && ui.editor.preview_is_current()
    });
    assert!(shown(&rt, "monitor-blended"), "the lanes are laid as one");
    assert!(!shown(&rt, "monitor-lane0-placed"));
    assert!(!shown(&rt, "monitor-lane1-placed"));
    let laid = super::blend::laid(&rt.state).expect("a laid picture");
    assert_eq!(
        (laid.width, laid.height),
        (320, 180),
        "the canvas's own size"
    );
    let at = |x: usize, y: usize| {
        let i = (y * laid.width + x) * 4;
        [laid.rgba[i], laid.rgba[i + 1], laid.rgba[i + 2]]
    };
    let [r, g, b] = at(240, 45);
    assert!(r > 230 && g < 30 && b > 230, "screened: {:?}", [r, g, b]);
    let [r, g, b] = at(80, 135);
    assert!(
        r > 230 && g < 30 && b < 30,
        "the red around it: {:?}",
        [r, g, b]
    );

    rt.state.editor.set_clip_blend(top, BlendMode::Normal);
    frames(&mut rt, 2);
    assert!(!shown(&rt, "monitor-blended"));
    assert!(shown(&rt, "monitor-lane0-placed") && shown(&rt, "monitor-lane1-placed"));
}

/// Picking a key colour: a click off the clip's picture says so and keeps
/// waiting; a click on it keys the colour under the pointer, moves
/// nothing, and the lane under the keyed picture shows.
#[test]
fn a_click_on_the_monitor_picks_the_key_colour() {
    let footage = Footage::new();
    let red = footage.path();
    let green = footage.color("screen.mp4", "0x00B140");
    let mut ui = Ui::headless();
    ui.editor
        .spawn_import(vec![red.clone().into(), green.clone().into()]);
    let mut rt = window(ui);
    until(&mut rt, "the imports", |ui| {
        ui.editor.asset(&green).is_some()
            && ui.editor.asset(&red).is_some()
            && !ui.editor.importing()
    });
    let editor = &mut rt.state.editor;
    editor.timeline.lanes.video = 2;
    editor
        .timeline
        .add_media_clip("Base", ClipKind::Video, 1, 0.0, 2.0, 0.0, &red);
    let screen =
        editor
            .timeline
            .add_media_clip("Screen", ClipKind::Video, 0, 0.0, 2.0, 0.0, &green);
    // Half size in the right half of the frame.
    editor.edit_clip_transform(screen, |t| {
        t.scale = 0.5;
        t.x = 0.25;
    });
    editor.selected = Some(screen);
    editor.playhead = 0.5;
    until(&mut rt, "both pictures", |ui| {
        ui.editor.program_layers().len() == 2 && ui.editor.preview_is_current()
    });
    rt.state.key_pick = Some(screen);
    let placed = rt.state.editor.timeline.clip(screen).unwrap().transform;
    let ([fx, fy, fw, fh], _) = rt.presented("monitor").unwrap();
    let tap = |rt: &mut Runtime<Ui>, x: f32, y: f32| {
        crate::testing::input_arrives();
        rt.pointer_move(x, y);
        rt.frame(FRAME);
        rt.pointer_down(x, y);
        rt.frame(FRAME);
        rt.pointer_up(x, y);
        frames(rt, 2);
    };

    tap(&mut rt, fx + fw * 0.2, fy + fh * 0.5);
    assert_eq!(rt.state.key_pick, Some(screen), "still waiting");
    assert!(
        rt.state.editor.status.contains("own picture"),
        "{}",
        rt.state.editor.status
    );

    tap(&mut rt, fx + fw * 0.75, fy + fh * 0.5);
    assert_eq!(rt.state.key_pick, None);
    let clip = rt.state.editor.timeline.clip(screen).unwrap();
    assert_eq!(clip.transform, placed, "a pick moves nothing");
    let openreel_engine::Rgb(r, g, b) = clip.chroma.expect("keyed").color;
    let near = |got: u8, want: u8| got.abs_diff(want) <= 6;
    assert!(
        near(r, 0) && near(g, 177) && near(b, 64),
        "picked {:?}",
        (r, g, b)
    );
    frames(&mut rt, 2);
    assert!(shown(&rt, "monitor-lane1-placed"), "the lane under shows");
    assert!(shown(&rt, "monitor-lane0-placed"));
}

#[test]
fn a_title_previews_where_its_look_puts_it_and_a_drag_moves_it() {
    let mut ui = Ui::headless();
    let title = ui
        .editor
        .timeline
        .add_clip("Drag me", ClipKind::Title, 0, 0.0, 4.0, 0.0);
    ui.editor.playhead = 1.0;
    let mut rt = window(ui);
    frames(&mut rt, 2);
    let (_, canvas_h) = rt.state.editor.canvas();
    let canvas_h = canvas_h as f32;
    let share = |rt: &Runtime<Ui>| {
        let ([fx, fy, fw, fh], _) = rt.presented("monitor").unwrap();
        let ([x, y, w, h], _) = rt.presented("monitor-title").unwrap();
        [(x - fx) / fw, (y - fy) / fh, w / fw, h / fh]
    };
    // A lower third's box: 42 px in and its text top 110 px up the canvas,
    // with 14 px of box round the text.
    let home = share(&rt);
    let canvas_w = rt.state.editor.canvas().0 as f32;
    assert!((home[0] - 28.0 / canvas_w).abs() < 0.005, "{home:?}");
    assert!(
        (home[1] - (canvas_h - 124.0) / canvas_h).abs() < 0.005,
        "{home:?}"
    );

    // Twice the size is twice as tall and grows upward: the line box keeps
    // its lower edge, so the box's moves only by the doubled padding (14
    // canvas pixels), not by the text's height.
    rt.state.editor.edit_title_text(title, |t| t.size = 2.0);
    frames(&mut rt, 60);
    let large = share(&rt);
    assert!(
        (large[3] / home[3] - 2.0).abs() < 0.1,
        "{large:?} against {home:?}"
    );
    let lower = (large[1] + large[3] - home[1] - home[3]) * canvas_h;
    assert!((lower - 14.0).abs() < 2.0, "{lower} px lower");
    rt.state.editor.edit_title_text(title, |t| t.size = 1.0);
    frames(&mut rt, 60);

    // A drag a tenth of the monitor right and down moves it a tenth of the
    // canvas, and selects it.
    let ([_, _, fw, fh], _) = rt.presented("monitor").unwrap();
    let ([x, y, w, h], _) = rt.presented("monitor-title").unwrap();
    let (x, y) = (x + w / 2.0, y + h / 2.0);
    crate::testing::input_arrives();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.frame(FRAME);
    rt.pointer_move(x + fw * 0.1, y + fh * 0.1);
    rt.frame(FRAME);
    rt.pointer_up(x + fw * 0.1, y + fh * 0.1);
    frames(&mut rt, 60);
    let text = rt.state.editor.timeline.clip(title).unwrap().text;
    assert!(
        (text.x - 0.1).abs() < 0.005 && (text.y - 0.1).abs() < 0.005,
        "{text:?}"
    );
    assert_eq!(rt.state.editor.selected, Some(title));
    let moved = share(&rt);
    assert!((moved[0] - home[0] - 0.1).abs() < 0.005, "{moved:?}");
    assert!((moved[1] - home[1] - 0.1).abs() < 0.005, "{moved:?}");

    // Esc during a drag puts it back where the drag started.
    let (x, y) = (x + fw * 0.1, y + fh * 0.1);
    rt.pointer_down(x, y);
    rt.frame(FRAME);
    rt.pointer_move(x - fw * 0.3, y);
    rt.frame(FRAME);
    assert!(rt.state.editor.timeline.clip(title).unwrap().text.x < 0.0);
    press(&mut rt, Key::Escape);
    rt.pointer_up(x - fw * 0.3, y);
    frames(&mut rt, 2);
    assert_eq!(rt.state.editor.timeline.clip(title).unwrap().text, text);
}

/// A caption of two lines previews them as export draws them: one Inter
/// line, round(38 x 1.20996) = 46 canvas pixels, apart, the last line's box
/// ending where a one-line caption's does (90 px up, plus its 46), and the
/// lines sharing their left edge 42 px in, their middle, or their right
/// edge 42 px from the right.
#[test]
fn a_title_of_several_lines_previews_them_lined_up_and_growing_upward() {
    use openreel_engine::{TextAlign, TextLook, TitleStyle};
    let mut ui = Ui::headless();
    let title = ui
        .editor
        .timeline
        .add_clip("HHHHHH\nHH", ClipKind::Title, 0, 0.0, 4.0, 0.0);
    ui.editor.timeline.clip_mut(title).unwrap().title_style = TitleStyle::Caption;
    ui.editor.playhead = 1.0;
    let mut rt = window(ui);
    frames(&mut rt, 2);
    let (canvas_w, canvas_h) = rt.state.editor.canvas();
    let (canvas_w, canvas_h) = (canvas_w as f32, canvas_h as f32);
    for align in TextAlign::ALL {
        rt.state.editor.edit_title_text(title, |t| {
            *t = TextLook {
                align: Some(align),
                ..TextLook::default()
            }
        });
        // One frame: the title lands at once, without springing.
        frames(&mut rt, 1);
        let ([fx, fy, fw, fh], _) = rt.presented("monitor").unwrap();
        let scale = fh / canvas_h;
        let line = |n: usize| rt.presented(&format!("monitor-title-line{n}")).unwrap().0;
        let (first, last) = (line(0), line(1));
        assert!(rt.presented("monitor-title-line2").is_none(), "two lines");
        assert!(
            (last[1] - first[1] - 46.0 * scale).abs() < 0.5,
            "{align:?}: {first:?} {last:?}"
        );
        let bottom = fy + (canvas_h - 90.0 + 46.0) * scale;
        assert!(
            (last[1] + last[3] - bottom).abs() < 1.0,
            "{align:?}: the last line ends at {}, not {bottom}",
            last[1] + last[3]
        );
        let near = |a: f32, b: f32| (a - b).abs() < 1.0;
        let (left, right) = (fx + 42.0 * scale, fx + fw - 42.0 * scale);
        let mid = |r: [f32; 4]| r[0] + r[2] / 2.0;
        let lined_up = match align {
            TextAlign::Left => near(first[0], left) && near(last[0], left),
            TextAlign::Center => near(mid(first), fx + fw / 2.0) && near(mid(last), mid(first)),
            TextAlign::Right => near(first[0] + first[2], right) && near(last[0] + last[2], right),
        };
        assert!(
            lined_up,
            "{align:?}: {first:?} {last:?} in {fw} of {canvas_w}"
        );
    }
}

/// A caption's shadow falls 0.4 x its 38 px letters at 30 degrees, 15.2
/// canvas pixels: (13.16, 7.6) right and down of its line, as export
/// draws it, and is the line's size: a copy moved right once wrapped its
/// last word onto a line of its own. A title without one has none.
#[test]
fn a_title_shadow_previews_where_its_angle_and_distance_put_it() {
    use openreel_engine::{Rgb, TextLook, TitleStyle};
    let mut ui = Ui::headless();
    let title = ui
        .editor
        .timeline
        .add_clip("The shadow falls", ClipKind::Title, 0, 0.0, 4.0, 0.0);
    ui.editor.timeline.clip_mut(title).unwrap().title_style = TitleStyle::Caption;
    ui.editor.playhead = 1.0;
    let mut rt = window(ui);
    frames(&mut rt, 2);
    assert!(rt.presented("monitor-title-shadow0").is_none(), "none yet");
    rt.state.editor.edit_title_text(title, |t| {
        *t = TextLook {
            shadow_color: Some(Rgb::BLACK),
            shadow_distance: 0.4,
            shadow_angle: 30.0,
            ..TextLook::default()
        }
    });
    frames(&mut rt, 1);
    let (_, canvas_h) = rt.state.editor.canvas();
    let ([_, _, _, fh], _) = rt.presented("monitor").unwrap();
    let scale = fh / canvas_h as f32;
    let ([lx, ly, lw, lh], _) = rt.presented("monitor-title-line0").unwrap();
    let ([sx, sy, sw, sh], _) = rt.presented("monitor-title-shadow0").unwrap();
    let (dx, dy) = ((sx - lx) / scale, (sy - ly) / scale);
    assert!(
        (dx - 13.16).abs() < 0.2 && (dy - 7.6).abs() < 0.2,
        "fell ({dx:.2}, {dy:.2}) canvas px"
    );
    assert!(
        (sw - lw).abs() < 0.5 && (sh - lh).abs() < 0.5,
        "the line's size: {sw}x{sh}, not {lw}x{lh}"
    );
}

/// Every copy an outline or shadow draws of a line stays one line: drawn
/// a little right of the line, a copy wrapped its last word onto a line of
/// its own, which showed as a stray word under the title.
#[test]
fn outline_and_shadow_copies_of_a_line_never_wrap() {
    use openreel_engine::{Rgb, TextLook, TitleFont, TitleStyle};
    let mut ui = Ui::headless();
    let title = ui.editor.timeline.add_clip(
        "SHARP SHADOW\nwith an outline",
        ClipKind::Title,
        0,
        0.0,
        4.0,
        0.0,
    );
    let clip = ui.editor.timeline.clip_mut(title).unwrap();
    clip.title_style = TitleStyle::Caption;
    clip.text = TextLook {
        font: Some(TitleFont::Oswald),
        bold: true,
        size: 1.8,
        box_opacity: Some(0.0),
        outline: Some(0.06),
        outline_color: Some(Rgb::BLACK),
        shadow_color: Some(Rgb(168, 85, 247)),
        shadow_distance: 0.12,
        shadow_angle: 60.0,
        shadow_blur: 0.0,
        ..TextLook::default()
    };
    ui.editor.playhead = 1.0;
    let mut rt = window(ui);
    frames(&mut rt, 2);
    for line in 0..2 {
        let ([_, _, _, height], _) = rt.presented(&format!("monitor-title-line{line}")).unwrap();
        for copy in ["line", "shadow"] {
            for step in 0..16 {
                let key = format!("monitor-title-{copy}{line}-stroke{step}");
                let ([_, _, _, h], _) = rt.presented(&key).unwrap_or_else(|| panic!("{key}"));
                assert!(
                    (h - height).abs() < 0.5,
                    "{key} is {h} tall, the line {height}"
                );
            }
        }
    }
}

/// A title in Lora Bold Italic previews in that face and in its line box,
/// as export draws it: the box is the font's 1.28 em line tall plus
/// padding, keeps the style's lower edge, and is as wide as the word's
/// advances in that face. The ems are read from the font files with
/// fontTools; no two candidates' widths are within 0.08 em.
#[test]
fn a_title_previews_in_its_own_font_weight_and_slant() {
    use openreel_engine::{TextLook, TitleFont};
    let mut ui = Ui::headless();
    let title = ui
        .editor
        .timeline
        .add_clip("Kaleidoscope", ClipKind::Title, 0, 0.0, 4.0, 0.0);
    ui.editor.timeline.clip_mut(title).unwrap().text = TextLook {
        font: Some(TitleFont::Lora),
        bold: true,
        italic: true,
        size: 2.0,
        ..TextLook::default()
    };
    ui.editor.playhead = 1.0;
    let mut rt = window(ui);
    frames(&mut rt, 2);
    let (_, canvas_h) = rt.state.editor.canvas();
    let ([_, fy, _, fh], _) = rt.presented("monitor").unwrap();
    let ([_, y, w, h], _) = rt.presented("monitor-title").unwrap();
    let scale = fh / canvas_h as f32;
    // A lower third at twice the size: 88 px letters, 28 px of padding.
    let (px, pad) = (88.0 * scale, 28.0 * scale);
    assert!(
        (h - 2.0 * pad - 1.28 * px).abs() < 1.5,
        "{h} tall for {px} px letters"
    );
    // Inter's 44 px line box ends 110 - 44 x 1.20996 = 56.76 px up the
    // canvas; this one's box ends its padding below that.
    let lower = fy + fh - 56.76 * scale + pad;
    assert!(
        (y + h - lower).abs() < 1.5,
        "ends at {}, not {lower}",
        y + h
    );
    let words = w - 2.0 * pad;
    // Lora Bold's 6.283 em is its advances' sum, 6283 units, not tau.
    #[allow(clippy::approx_constant)]
    let faces = [
        ("Inter", 6.407),
        ("Inter Bold Italic", 6.708),
        ("Lora", 6.196),
        ("Lora Bold", 6.283),
        ("Lora Italic", 5.845),
        ("Lora Bold Italic", 5.929),
    ];
    let nearest = faces
        .iter()
        .min_by(|a, b| {
            (a.1 * px - words)
                .abs()
                .total_cmp(&(b.1 * px - words).abs())
        })
        .unwrap();
    assert_eq!(
        nearest.0,
        "Lora Bold Italic",
        "{words} px wide, {:.1} em",
        words / px
    );
}

/// Halfway through a wipe to the left the incoming picture shows right of
/// the edge xfade puts at `int(320 * 0.5) + 1` = 161 of the 320 canvas
/// pixels and the outgoing one left of it; past the wipe one picture shows
/// alone.
#[test]
fn a_transition_previews_both_pictures_where_export_puts_them() {
    use openreel_engine::{Transition, TransitionKind};
    let footage = Footage::new();
    let path = footage.path();
    let mut ui = Ui::headless();
    ui.editor.spawn_import(vec![path.clone().into()]);
    let mut rt = window(ui);
    until(&mut rt, "the import", |ui| {
        ui.editor.asset(&path).is_some() && !ui.editor.importing()
    });
    let timeline = &mut rt.state.editor.timeline;
    timeline.add_media_clip("Out", ClipKind::Video, 0, 0.0, 1.0, 0.0, &path);
    let next = timeline.add_media_clip("In", ClipKind::Video, 0, 1.0, 1.0, 0.0, &path);
    timeline.clip_mut(next).unwrap().transition =
        Some(Transition::new(TransitionKind::WipeLeft, 0.6));
    rt.state.editor.seek(1.0);
    until(&mut rt, "both pictures", |ui| {
        ui.editor.transition_shown().is_some()
    });

    let share = |rt: &Runtime<Ui>, key: &str| {
        let ([fx, fy, fw, fh], _) = rt.presented("monitor").unwrap();
        let ([x, y, w, h], _) = rt.presented(key).unwrap();
        [(x - fx) / fw, (y - fy) / fh, w / fw, h / fh]
    };
    let close =
        |got: [f32; 4], want: [f32; 4]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.002);
    let from = share(&rt, "monitor-lane0-from");
    let to = share(&rt, "monitor-lane0-to");
    assert!(close(from, [0.0, 0.0, 161.0 / 320.0, 1.0]), "{from:?}");
    assert!(
        close(to, [161.0 / 320.0, 0.0, 159.0 / 320.0, 1.0]),
        "{to:?}"
    );

    rt.state.editor.seek(1.5);
    until(&mut rt, "the next picture", |ui| {
        ui.editor.preview_is_current()
    });
    assert!(!shown(&rt, "monitor-lane0-transition"));
    assert!(shown(&rt, "monitor-lane0-placed"));
}

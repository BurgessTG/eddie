//! One clip on the lanes: its body, the filmstrip or waveform inside it,
//! its name and the marks that show it selected, hovered or linked.

use eddie_core::editor::lanes::{self, ClipHit, HEADER_W, ViewRect};
use eddie_core::editor::media::BinAsset;
use openreel_engine::{Clip, ClipKind};
use rune::{Color, El, LineCap, Shape, el, row, text};

use super::{black, white};
use crate::app::{Thumb, Ui};
use crate::icons;
use crate::theme::{self, c};

/// How far a clip's element reaches past the lanes' edges. Only the part
/// in view is laid out, and its cut ends stay out of sight.
const OVERHANG: f32 = 8.0;

fn kind_color(kind: ClipKind) -> Color {
    let c = c();
    match kind {
        ClipKind::Video => c.clip_video,
        ClipKind::Audio => c.clip_audio,
        ClipKind::Title => c.clip_title,
    }
}

/// The kind colour × 1.3, under the pointer.
fn kind_hover(kind: ClipKind) -> Color {
    theme::hex(match kind {
        ClipKind::Video => "#3C608B",
        ClipKind::Audio => "#396D4E",
        ClipKind::Title => "#8B6C1F",
    })
}

/// Two interlocked rings marking a linked picture and sound pair, 16×16.
fn link_glyph() -> Shape {
    [-2.5_f32, 2.5]
        .into_iter()
        .fold(Shape::new().view_box(16.0, 16.0), |shape, dx| {
            icons::outline(shape, 7.5 + dx, 5.5, 7.0, 5.0, 2.0, 1.3, white(200))
        })
}

/// A clip's pictures: stills chosen by the source time under each tile,
/// so the strip follows trims as the waveform does.
fn filmstrip(
    mut body: El,
    mut art: Shape,
    clip: &Clip,
    thumbs: &[Thumb],
    px_per_sec: f32,
    left: f32,
    (w, h): (f32, f32),
) -> (El, Shape) {
    let Some(first) = thumbs.first() else {
        return (body, art);
    };
    let aspect = if first.height > 0 {
        first.width as f32 / first.height as f32
    } else {
        16.0 / 9.0
    };
    let tile_w = (h * aspect).max(12.0);
    let opacity = clip.opacity.clamp(0.0, 1.0) * 230.0 / 255.0;
    for tile in lanes::filmstrip_tiles(clip.source_in, clip.duration, px_per_sec, tile_w) {
        let x = left + tile.x;
        if x + tile.w < 0.0 || x > w {
            continue;
        }
        let index = thumbs
            .partition_point(|thumb| thumb.time <= tile.center_source)
            .saturating_sub(1);
        // A tile cut by the clip's edge hangs over it; the body clips it.
        body = body.child(
            el().absolute(x - tile.uv.0 * tile_w, 0.0)
                .size(tile_w, h)
                .pixels(&thumbs[index].pixels)
                .opacity(opacity),
        );
        // Separators, so the stills read as discrete frames.
        if tile.x > 0.5 {
            art = art.line([x, 0.0], [x, h], 1.0, LineCap::Butt, black(110));
        }
    }
    (body, art)
}

/// A clip's peaks, read through its source range so trims and splits show
/// the right part of the sound.
fn waveform(
    mut art: Shape,
    clip: &Clip,
    asset: &BinAsset,
    px_per_sec: f32,
    left: f32,
    full_w: f32,
    (w, h): (f32, f32),
) -> Shape {
    if asset.peaks.is_empty() || asset.info.duration <= 0.0 {
        return art;
    }
    let peaks_per_sec = asset.peaks.len() as f64 / asset.info.duration;
    // Below the name chip, so the label never sits on the peaks.
    let mid = h / 2.0 + 5.0;
    let max_amp = h / 2.0 - 8.0;
    // CLIP_AUDIO × 2.6.
    let color = theme::hex("#72DA9C");
    // Bars sit every 3 points from the clip's own edge; start at the first
    // one in view.
    let mut x = 2.0 + ((-left - 2.0) / 3.0).floor().max(0.0) * 3.0;
    while x < full_w - 2.0 && left + x <= w {
        let source_time = clip.source_in + f64::from(x) / f64::from(px_per_sec);
        if let Some(peak) = asset.peaks.get((source_time * peaks_per_sec) as usize) {
            // Square-root scale, so quiet dialogue stays visible beside
            // loud music.
            let amp = (peak.clamp(0.0, 1.0).sqrt() * max_amp).max(1.0);
            art = art.line(
                [left + x, mid - amp],
                [left + x, mid + amp],
                2.0,
                LineCap::Butt,
                color,
            );
        }
        x += 3.0;
    }
    art
}

/// One clip in the lanes' points, or `None` when it is out of view.
pub(super) fn clip_el(
    ui: &Ui,
    clip: &Clip,
    view: ViewRect,
    lanes_w: f32,
    hover: Option<ClipHit>,
) -> Option<El> {
    let c = c();
    let port = &ui.editor.viewport;
    let rect = lanes::clip_rect(port, view, clip);
    let (clip_left, clip_right) = (rect.left - HEADER_W, rect.right - HEADER_W);
    if clip_right < 0.0 || clip_left > lanes_w {
        return None;
    }
    let from = clip_left.max(-OVERHANG);
    let to = clip_right.min(lanes_w + OVERHANG);
    let (w, h) = (to - from, rect.height());
    // The clip's own left edge and width, in the element's points.
    let (left, full_w) = (clip_left - from, clip_right - clip_left);

    let selected = ui.editor.selected == Some(clip.id);
    let fill = if hover.is_some() && !selected {
        kind_hover(clip.kind)
    } else {
        kind_color(clip.kind)
    };
    let mut body = el()
        .key(format!("clip-{}", clip.id))
        .absolute(from, rect.top)
        .size(w, h)
        .radius(theme::R_SM)
        .fill(fill)
        .clip_rounded(theme::R_SM);

    let mut art = Shape::new().view_box(w, h);
    let source = clip.source.as_deref();
    match clip.kind {
        ClipKind::Video => {
            let thumbs = source.map_or(&[][..], |path| ui.thumbs(path));
            (body, art) = filmstrip(body, art, clip, thumbs, port.px_per_sec, left, (w, h));
        }
        ClipKind::Audio => {
            let asset = source.and_then(|path| {
                ui.editor
                    .assets
                    .iter()
                    .find(|asset| asset.info.path == path)
            });
            if let Some(asset) = asset {
                art = waveform(art, clip, asset, port.px_per_sec, left, full_w, (w, h));
            }
        }
        ClipKind::Title => {}
    }
    art = edges(art, hover, left, full_w, (w, h));
    body = body.child(el().absolute(0.0, 0.0).size(w, h).shape(art));

    if full_w > 40.0 {
        body = body.child(name_chip(clip, left, full_w));
    }

    let outline = if selected {
        Some((2.0, c.brand))
    } else {
        hover.map(|_| (1.0, white(90)))
    };
    if let Some((width, color)) = outline {
        body = body.child(
            el().absolute(0.0, 0.0)
                .size(w, h)
                .radius(theme::R_SM)
                .border(width, color),
        );
    }
    Some(body)
}

/// A lighter top edge, and a grip on the edge the pointer would trim.
fn edges(
    mut art: Shape,
    hover: Option<ClipHit>,
    left: f32,
    full_w: f32,
    (w, h): (f32, f32),
) -> Shape {
    let c = c();
    // A lighter top edge gives the body depth without an outline.
    let (top_from, top_to) = ((left + 4.0).max(0.0), (left + full_w - 4.0).min(w));
    if top_to > top_from {
        art = art.line(
            [top_from, 0.5],
            [top_to, 0.5],
            1.0,
            LineCap::Butt,
            white(40),
        );
    }
    // Trim affordance: a grip on the edge under the pointer.
    if let Some(hit @ (ClipHit::Left | ClipHit::Right)) = hover {
        let x = left
            + if hit == ClipHit::Left {
                3.5
            } else {
                full_w - 3.5
            };
        art = art.line(
            [x, h / 2.0 - 8.0],
            [x, h / 2.0 + 8.0],
            2.5,
            LineCap::Butt,
            c.foreground,
        );
    }
    art
}

/// The clip's name over its top-left corner, with a link mark when its
/// picture and sound are tied.
fn name_chip(clip: &Clip, left: f32, full_w: f32) -> El {
    let linked = clip.linked_clip.is_some();
    let link_w = if linked { 16.0 } else { 0.0 };
    let mut chip = row()
        .absolute(left + 3.0, 3.0)
        .h(16.0)
        .px(5.0)
        .radius(4.0)
        .center_y()
        .child(
            text(clip.name.clone())
                .font(theme::sized(11.0))
                .color(white(235))
                .single_line()
                .truncate()
                .max_w((full_w - 14.0 - link_w).max(8.0)),
        );
    // Media carries pictures or peaks under its name, so it gets a
    // dark chip; flat title bodies read without one.
    if clip.kind != ClipKind::Title {
        chip = chip.fill(black(140));
    }
    if linked {
        chip = chip.child(el().size(16.0, 16.0).shape(link_glyph()));
    }
    chip
}

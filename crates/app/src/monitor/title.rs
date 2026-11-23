//! The title at the playhead, drawn from the look export draws it with
//! (`openreel_media::clip_title_look`) scaled to the monitor, and a drag
//! that moves it.

use openreel_engine::{Clip, Rgb, TextAlign};
use rune::{Color, Cursor, El, Family, Phase, Span, TextStyle, col, el, rich, row};

use crate::app::Ui;

/// A drag that ends this close to the style's own place, in points, lands
/// on it.
const HOME_SNAP: f32 = 6.0;

fn color(rgb: Rgb, alpha: f32) -> Color {
    rune::color::hex(&rgb.hex())
        .expect("a colour from bytes")
        .alpha(alpha)
}

/// `title` on a `w` by `h` monitor showing a canvas `canvas_h` pixels tall.
/// Each line is its own element (`monitor-title-line{n}`), lined up as
/// export's drawtext lines them up.
pub(super) fn overlay(title: &Clip, playhead: f64, w: f32, h: f32, canvas_h: u32) -> El {
    let alpha = title.opacity.clamp(0.0, 1.0);
    let look = openreel_media::clip_title_look(title);
    let scale = h / canvas_h.max(1) as f32;
    // The face export draws, in the line box drawtext's `y_align=font`
    // lays it out in, one whole canvas line apart as FreeType spaces them.
    let face = look.face;
    let px = (look.font_size as f32 * scale).max(1.0);
    let line_h = look.line_height() as f32 * scale;
    let font = TextStyle::new(px, face.weight())
        .family(Family::Named(face.font().family()))
        .lh(line_h);
    let font = if face.italic() { font.italic() } else { font };
    let lines = openreel_media::title_lines(title);
    // A highlight caption is one line; the word being spoken is lit.
    let active = look.highlight.and(title.active_word(playhead));
    let lit = match (look.highlight, active) {
        (Some(highlight), Some(active)) => {
            let mut at = 0;
            title
                .name
                .split_whitespace()
                .enumerate()
                .find_map(|(index, word)| {
                    let start = at;
                    at += word.len() + 1;
                    (index == active).then(|| (start, start + word.len(), color(highlight, alpha)))
                })
        }
        _ => None,
    };
    let label = |line: &str, ink: Color, lit: Option<(usize, usize, Color)>| {
        let spans = lit
            .map(|(start, end, lit)| vec![Span::new(start, end).color(lit)])
            .unwrap_or_default();
        rich(line.to_owned(), spans)
            .font(font)
            .color(ink)
            .single_line()
    };
    let outline = look
        .outline
        .map(|(width, ink, opacity)| (width as f32 * scale, color(ink, opacity * alpha)));
    let pad = match look.text_box {
        Some(_) => look.padding as f32 * scale,
        None => outline.map_or(0.0, |(width, _)| width),
    };
    let mut block = col().key("monitor-title").padding(pad);
    block = match look.align {
        TextAlign::Left => block.items_start(),
        TextAlign::Center => block.items_center(),
        TextAlign::Right => block.items_end(),
    };
    if let Some((fill, opacity)) = look.text_box {
        block = block.fill(color(fill, opacity * alpha));
    }
    for (index, line) in lines.iter().enumerate() {
        let mut row = el().key(format!("monitor-title-line{index}"));
        if let Some((width, ink)) = outline {
            // Copies of the line round a circle under it: a round stroke,
            // as FreeType's stroker draws export's outline.
            let steps = if width > 1.5 { 16 } else { 8 };
            for step in 0..steps {
                let angle = std::f32::consts::TAU * step as f32 / steps as f32;
                row = row.child(
                    label(line, ink, None).absolute(width * angle.cos(), width * angle.sin()),
                );
            }
        }
        block = block.child(row.child(label(line, color(look.color, alpha), lit)));
    }
    let id = title.id;
    let block = block
        .cursor(Cursor::Move)
        .help("Drag to move this text; double-click to edit it")
        .on_double_press(move |ui: &mut Ui| {
            ui.editor.selected = Some(id);
            ui.show_inspector = true;
            ui.focus_title = true;
        })
        .on_drag(move |ui: &mut Ui, drag, _| dragged(ui, id, drag, (w, h)));
    let (dx, dy) = (look.offset.0 as f32 * w, look.offset.1 as f32 * h);
    let top = h - look.top(lines.len()) as f32 * scale - pad + dy;
    let margin = look.margin as f32 * scale;
    let placed = match look.align {
        TextAlign::Left => block.absolute(margin - pad + dx, top),
        TextAlign::Center => row().absolute(dx, top).w(w).justify_center().child(block),
        TextAlign::Right => row()
            .absolute(dx + pad - margin, top)
            .w(w)
            .justify_end()
            .child(block),
    };
    // Each frame shows the text where export draws it: a new caption, a
    // change of alignment or a drag lands at once instead of springing.
    placed.instant_changes()
}

/// Drag a title to move it, as a share of the `monitor` size. It selects
/// the title, lands back on its style's place when near it, and Esc puts
/// it back where it was.
fn dragged(ui: &mut Ui, id: openreel_engine::ClipId, drag: &rune::Drag, monitor: (f32, f32)) {
    let editor = &mut ui.editor;
    match drag.phase {
        Phase::Down => {
            ui.title_drag = editor.timeline.clip(id).map(|clip| clip.text);
            editor.selected = Some(id);
        }
        Phase::Up => {
            if let Some(before) = ui.title_drag.take().filter(|_| drag.cancelled()) {
                editor.edit_title_text(id, |text| *text = before);
            }
        }
        _ => {
            let Some(before) = ui.title_drag else {
                return;
            };
            let along = |start: f64, travel: f32, size: f32| {
                let moved = start * f64::from(size) + f64::from(travel);
                if moved.abs() < f64::from(HOME_SNAP) {
                    0.0
                } else {
                    moved / f64::from(size.max(1.0))
                }
            };
            let (x, y) = (
                along(before.x, drag.dx, monitor.0),
                along(before.y, drag.dy, monitor.1),
            );
            editor.edit_title_text(id, |text| {
                text.x = x;
                text.y = y;
            });
        }
    }
}

//! The title at the playhead, drawn from the look export draws it with
//! (`openreel_media::clip_title_look`) scaled to the monitor, and a drag
//! that moves it.

use openreel_engine::{Clip, Rgb};
use rune::{Color, Cursor, El, Family, Phase, Span, TextStyle, el, rich, row};

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
pub(super) fn overlay(title: &Clip, playhead: f64, w: f32, h: f32, canvas_h: u32) -> El {
    let alpha = title.opacity.clamp(0.0, 1.0);
    let look = openreel_media::clip_title_look(title);
    let scale = h / canvas_h.max(1) as f32;
    // The face export draws, in the font's own line box as drawtext's
    // `y_align=font` lays it out, so the line sits where export puts it.
    let face = look.face;
    let px = (look.font_size as f32 * scale).max(1.0);
    let font = TextStyle::new(px, face.weight())
        .family(Family::Named(face.font().family()))
        .lh(px * face.line_height());
    let font = if face.italic() { font.italic() } else { font };
    // Export draws one line with single spaces between words.
    let active = look.highlight.and(title.active_word(playhead));
    let (line, lit) = match (look.highlight, active) {
        (Some(highlight), Some(active)) => {
            let mut line = String::new();
            let mut lit = None;
            for (index, word) in title.name.split_whitespace().enumerate() {
                if index > 0 {
                    line.push(' ');
                }
                if index == active {
                    lit = Some((line.len(), line.len() + word.len(), color(highlight, alpha)));
                }
                line.push_str(word);
            }
            (line, lit)
        }
        _ => (title.name.clone(), None),
    };
    let label = |ink: Color, lit: Option<(usize, usize, Color)>| {
        let spans = lit
            .map(|(start, end, lit)| vec![Span::new(start, end).color(lit)])
            .unwrap_or_default();
        rich(line.clone(), spans)
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
    let mut block = el().key("monitor-title").padding(pad);
    if let Some((fill, opacity)) = look.text_box {
        block = block.fill(color(fill, opacity * alpha));
    }
    if let Some((width, ink)) = outline {
        // Copies of the line round a circle under it: a round stroke, as
        // FreeType's stroker draws export's outline.
        let steps = if width > 1.5 { 16 } else { 8 };
        for step in 0..steps {
            let angle = std::f32::consts::TAU * step as f32 / steps as f32;
            block = block.child(
                label(ink, None).absolute(pad + width * angle.cos(), pad + width * angle.sin()),
            );
        }
    }
    let id = title.id;
    let block = block
        .child(label(color(look.color, alpha), lit))
        .cursor(Cursor::Move)
        .help("Drag to move this text; double-click to edit it")
        .on_double_press(move |ui: &mut Ui| {
            ui.editor.selected = Some(id);
            ui.show_inspector = true;
            ui.focus_title = true;
        })
        .on_drag(move |ui: &mut Ui, drag, _| dragged(ui, id, drag, (w, h)));
    let (dx, dy) = (look.offset.0 as f32 * w, look.offset.1 as f32 * h);
    let top = h - look.bottom as f32 * scale - pad + dy;
    match look.left {
        Some(left) => block.absolute(left as f32 * scale - pad + dx, top),
        None => row().absolute(dx, top).w(w).justify_center().child(block),
    }
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

//! A transition on the monitor: the picture it comes from and the one it
//! goes to, each its clip's whole canvas as export draws it, put where
//! [`openreel_engine::TransitionKind::frame`] says, under a black or white
//! veil for a dip.

use openreel_engine::{Clip, Layer, TransitionFrame, TransitionSpan};
use rune::{Color, El, el};

use super::picture::{LaneSlots, placed};
use crate::app::Ui;

/// `from`'s held picture and the incoming one, held in `slots` and keyed
/// `{key}-…`, `span` as far through as the playhead, on a `w` by `h`
/// monitor showing a `canvas` sized frame. A mix or dip passes between
/// whole frames: each side is its picture over `under`, the lanes under
/// it, on black. A wipe or slide moves only the pictures, over the lanes
/// drawn before it.
#[allow(clippy::too_many_arguments)]
pub(super) fn view(
    ui: &Ui,
    slots: &LaneSlots,
    key: &str,
    span: TransitionSpan,
    from: &Clip,
    canvas: (u32, u32),
    (w, h): (f32, f32),
    decoded: (u32, u32),
    under: Vec<El>,
) -> El {
    let editor = &ui.editor;
    let frame = span.kind.frame(span.progress(editor.playhead), canvas);
    let side = |picture: El| {
        if span.kind.blends() {
            el().absolute(0.0, 0.0)
                .size(w, h)
                .fill(Color::BLACK)
                .children(under.clone())
                .child(picture)
        } else {
            picture
        }
    };
    let from = side(placed(
        ui,
        &slots.program,
        key,
        from,
        canvas,
        (w, h),
        decoded,
    ));
    let incoming = format!("{key}-incoming");
    let to = editor
        .timeline
        .clip(span.to)
        .zip(slots.incoming.size.get())
        .map(|(to, size)| {
            side(placed(
                ui,
                &slots.incoming,
                &incoming,
                to,
                canvas,
                (w, h),
                size,
            ))
        });
    layers(key, frame, canvas, (w, h), from, to)
        .key(format!("{key}-transition"))
        .instant_changes()
}

/// One moment of a transition on a `w` by `h` box showing a `canvas` sized
/// frame: `from`, then `to` over it, each a whole canvas (placed at the
/// box's origin, `w` by `h`) moved and cut to its window, then the veil.
/// The parts are keyed `{key}-from`, `{key}-to` and `{key}-veil`.
pub(crate) fn layers(
    key: &str,
    frame: TransitionFrame,
    canvas: (u32, u32),
    (w, h): (f32, f32),
    from: El,
    to: Option<El>,
) -> El {
    let k = w / canvas.0.max(1) as f32;
    let side = |part: &str, layer: Layer, picture: El| {
        let [x, y, lw, lh] = layer.window.map(|v| v as f32 * k);
        let [ox, oy] = layer.offset.map(|v| v as f32 * k);
        el().key(format!("{key}-{part}"))
            .absolute(x, y)
            .size(lw, lh)
            .clip_rounded(0.0)
            .layer_opacity(layer.opacity as f32)
            .child(el().absolute(ox - x, oy - y).size(w, h).child(picture))
    };
    let mut stack = el()
        .absolute(0.0, 0.0)
        .size(w, h)
        .child(side("from", frame.from, from));
    if let Some(to) = to {
        stack = stack.child(side("to", frame.to, to));
    }
    if let Some((white, opacity)) = frame.veil {
        stack = stack.child(
            el().key(format!("{key}-veil"))
                .absolute(0.0, 0.0)
                .size(w, h)
                .fill(if white { Color::WHITE } else { Color::BLACK })
                .layer_opacity(opacity as f32),
        );
    }
    stack
}

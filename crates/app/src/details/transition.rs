//! The Transition card: how a video clip's picture comes in from the clip
//! right before it, picked from tiles that each draw their kind part way
//! through as the monitor would, and how long it takes.

use eddie_core::Editor;
use openreel_engine::{
    Clip, ClipId, DEFAULT_TRANSITION, MAX_TRANSITION, MIN_TRANSITION, Transition, TransitionKind,
};
use rune::{El, LineCap, Shape, el};

use super::fields::Field;
use super::filters::{tile, tile_grid, tile_size};
use super::rows::{section, slider_row};
use crate::app::Ui;
use crate::monitor::transition_layers;
use crate::theme::c;

pub(super) fn card(ui: &Ui, id: ClipId, clip: &Clip, value_w: f32, card_inner: f32) -> El {
    let current = clip.transition;
    let (cols, w, h) = tile_size(card_inner);
    let none = el()
        .absolute(0.0, 0.0)
        .size(w, h)
        .shape(Shape::new().view_box(w, h).line(
            [w * 0.3, h * 0.75],
            [w * 0.7, h * 0.25],
            1.5,
            LineCap::Round,
            c().muted_fg,
        ));
    let mut tiles = vec![
        tile(
            "details-transition-none".to_owned(),
            "None",
            "No transition: a straight cut from the clip before",
            none,
            current.is_none(),
            (w, h),
        )
        .on_press(move |ui: &mut Ui| ui.editor.set_clip_transition(id, None)),
    ];
    for kind in TransitionKind::ALL {
        let picked = current.is_some_and(|transition| transition.kind == kind);
        let key = format!("details-transition-{}", kind.as_str());
        tiles.push(
            tile(
                key,
                kind.label(),
                help(kind),
                picture(kind, (w, h)),
                picked,
                (w, h),
            )
            .on_press(move |ui: &mut Ui| {
                let editor = &mut ui.editor;
                let duration = editor
                    .timeline
                    .clip(id)
                    .and_then(|clip| clip.transition)
                    .map_or(DEFAULT_TRANSITION, |transition| transition.duration);
                editor.set_clip_transition(id, Some(Transition::new(kind, duration)));
            }),
        );
    }
    let mut rows = vec![tile_grid("details-transitions", tiles, cols)];
    if let Some(transition) = current {
        rows.push(slider_row(
            ui,
            id,
            "Duration",
            Field::TransitionLength,
            transition.duration as f32,
            MIN_TRANSITION as f32..=MAX_TRANSITION as f32,
            value_w,
            set_length,
            "How long the transition takes, centred on the cut; each side uses at most half its clip",
        ));
    }
    section("Transition", rows)
}

fn set_length(editor: &mut Editor, id: ClipId, seconds: f32) {
    let kept = editor.timeline.clip(id).and_then(|clip| clip.transition);
    let seconds = (f64::from(seconds) * 100.0).round() / 100.0;
    editor.set_clip_transition(id, kept.map(|t| Transition::new(t.kind, seconds)));
}

/// A tile's picture: the clip before as a blue card with a circle, the
/// next as an amber one with a square, `kind` part way from one to the
/// other as the monitor draws it.
fn picture(kind: TransitionKind, (w, h): (f32, f32)) -> El {
    let c = c();
    let card = |fill, round: f32| {
        let side = h * 0.42;
        el().absolute(0.0, 0.0).size(w, h).fill(fill).child(
            el().absolute((w - side) / 2.0, (h - side) / 2.0)
                .size(side, side)
                .radius(side * round)
                .fill(c.foreground.alpha(0.85)),
        )
    };
    // A dip is shown on its way down, the rest halfway.
    let t = match kind {
        TransitionKind::DipBlack | TransitionKind::DipWhite => 0.3,
        _ => 0.5,
    };
    let canvas = (w.round() as u32, h.round() as u32);
    let frame = kind.frame(t, canvas);
    transition_layers(
        &format!("details-transition-{}-picture", kind.as_str()),
        frame,
        canvas,
        (w, h),
        card(c.clip_video, 0.5),
        Some(card(c.clip_title, 0.15)),
    )
}

/// What each kind does, for its tooltip.
fn help(kind: TransitionKind) -> &'static str {
    match kind {
        TransitionKind::Mix => "Mix: the next clip fades in over this one",
        TransitionKind::DipBlack => "Dip to black: fade to black, then up into the next clip",
        TransitionKind::DipWhite => "Dip to white: fade to white, then into the next clip",
        TransitionKind::WipeLeft => {
            "Wipe left: an edge sweeps right to left, uncovering the next clip"
        }
        TransitionKind::WipeRight => {
            "Wipe right: an edge sweeps left to right, uncovering the next clip"
        }
        TransitionKind::WipeUp => {
            "Wipe up: an edge sweeps from the bottom up, uncovering the next clip"
        }
        TransitionKind::WipeDown => {
            "Wipe down: an edge sweeps from the top down, uncovering the next clip"
        }
        TransitionKind::SlideLeft => "Slide left: the next clip pushes this one out to the left",
        TransitionKind::SlideRight => "Slide right: the next clip pushes this one out to the right",
        TransitionKind::SlideUp => "Slide up: the next clip pushes this one out the top",
        TransitionKind::SlideDown => "Slide down: the next clip pushes this one out the bottom",
    }
}

use eddie_core::editor::lanes::RULER_H;
use openreel_engine::{ClipKind, MarkerColor};
use rune::{Key, KeyEvent, Mods, Runtime};

use crate::app::Ui;
use crate::testing::{FRAME, chord, click, frames, input_arrives, press, shown, window};

/// A point on the ruler over marker `id`'s pole.
fn flag(rt: &Runtime<Ui>, id: u64) -> (f32, f32) {
    let ([x, y, _, _], _) = rt
        .presented(&format!("marker-{id}"))
        .expect("the flag is drawn");
    (x + 1.0, y + RULER_H / 2.0)
}

fn clicks(rt: &mut Runtime<Ui>, (x, y): (f32, f32), count: usize) {
    input_arrives();
    rt.pointer_move(x, y);
    rt.frame(FRAME);
    for _ in 0..count {
        rt.pointer_down(x, y);
        rt.frame(FRAME);
        rt.pointer_up(x, y);
        rt.frame(FRAME);
    }
    frames(rt, 2);
}

/// M drops a flag on the ruler; its tooltip names it, a double-click or
/// a right-click opens it to name, recolour or delete, and a drag moves it.
#[test]
fn markers_are_flags_on_the_ruler_that_open_to_edit() {
    let mut ui = Ui::headless();
    ui.editor
        .timeline
        .add_clip("Intro", ClipKind::Title, 0, 0.0, 12.0, 0.0);
    ui.editor.playhead = 2.0;
    let mut rt = window(ui);
    press(&mut rt, Key::Text("m".into()));
    let marker = |rt: &Runtime<Ui>| rt.state.editor.timeline.marker(1).cloned();
    assert_eq!(marker(&rt).map(|m| m.time), Some(2.0));
    let at = flag(&rt, 1);

    rt.help_delay = 0.0;
    rt.pointer_leave();
    rt.frame(FRAME);
    rt.pointer_move(at.0, at.1);
    frames(&mut rt, 2);
    let tip = rt.help_tooltip().map(|(words, _)| words.to_owned());
    assert_eq!(
        tip.as_deref(),
        Some("Marker 1 · 00:00:02:00. Drag to move it; double-click or right-click to edit it")
    );

    clicks(&mut rt, at, 2);
    assert!(shown(&rt, "marker-editor"), "a double-click opens it");
    assert!(rt.state.typing(), "its note has the keyboard");
    rt.paste("B-roll here");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    assert!(!shown(&rt, "marker-editor"), "Enter closes it");
    assert_eq!(marker(&rt).unwrap().name, "B-roll here");
    assert_eq!(rt.state.editor.playhead, 2.0);

    input_arrives();
    rt.secondary_down(at.0, at.1);
    rt.frame(FRAME);
    rt.secondary_up(at.0, at.1);
    frames(&mut rt, 2);
    assert!(shown(&rt, "marker-editor"), "a right-click opens it");
    click(&mut rt, "marker-color-red");
    assert_eq!(marker(&rt).unwrap().color, MarkerColor::Red);
    press(&mut rt, Key::Escape);
    assert!(!shown(&rt, "marker-editor"), "Esc closes it");

    // Two seconds right.
    let two = 2.0 * rt.state.editor.viewport.px_per_sec;
    input_arrives();
    rt.pointer_down(at.0, at.1);
    rt.frame(FRAME);
    for step in 1..=4 {
        rt.pointer_move(at.0 + two * step as f32 / 4.0, at.1);
        rt.frame(FRAME);
    }
    rt.pointer_up(at.0 + two, at.1);
    frames(&mut rt, 2);
    assert_eq!(marker(&rt).unwrap().time, 4.0);
    assert_eq!(
        rt.state.editor.playhead, 2.0,
        "a flag drags, not the playhead"
    );

    // M on a marker opens it rather than adding another.
    rt.state.editor.playhead = 4.0;
    press(&mut rt, Key::Text("m".into()));
    assert!(shown(&rt, "marker-editor"));
    assert_eq!(rt.state.editor.timeline.markers.len(), 1);
    click(&mut rt, "marker-delete");
    assert!(!shown(&rt, "marker-editor"));
    assert!(rt.state.editor.timeline.markers.is_empty());
}

/// Alt+M adds in the next colour and Shift+M jumps to the next marker,
/// with Alt read from the key's own letter.
#[test]
fn alt_m_adds_a_new_colour_and_shift_m_jumps() {
    let mut ui = Ui::headless();
    ui.editor
        .timeline
        .add_clip("Intro", ClipKind::Title, 0, 0.0, 12.0, 0.0);
    ui.editor.playhead = 3.0;
    let mut rt = window(ui);
    press(&mut rt, Key::Text("m".into()));
    rt.state.editor.playhead = 6.0;
    let alt = Mods {
        alt: true,
        ..Mods::default()
    };
    chord(&mut rt, Key::Text("m".into()), alt);
    let colours: Vec<MarkerColor> = rt
        .state
        .editor
        .timeline
        .markers_in_order()
        .iter()
        .map(|m| m.color)
        .collect();
    assert_eq!(colours, [MarkerColor::Yellow, MarkerColor::Orange]);

    rt.state.editor.playhead = 0.0;
    let shift = Mods {
        shift: true,
        ..Mods::default()
    };
    input_arrives();
    rt.key(KeyEvent {
        key: Key::Text("M".into()),
        mods: shift,
    });
    frames(&mut rt, 2);
    assert_eq!(rt.state.editor.playhead, 3.0);
    chord(&mut rt, Key::Text("M".into()), shift);
    assert_eq!(rt.state.editor.playhead, 6.0);
}

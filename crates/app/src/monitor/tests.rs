use openreel_engine::ClipKind;
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

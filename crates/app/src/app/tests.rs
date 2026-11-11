use eddie_core::shortcuts::{BINDINGS, Chord, KeyName};
use rune::{Key, KeyEvent, Mods};

/// The event the window gets for a chord: Ctrl with a character
/// arrives as a command, a plain character as the text it types.
fn event(chord: Chord) -> KeyEvent {
    let key = match chord.key {
        KeyName::Char(key) if chord.command => Key::Command(key),
        KeyName::Char(key) if chord.shift => Key::Text(key.to_uppercase().collect()),
        KeyName::Char(key) => Key::Text(key.to_string()),
        KeyName::Space => Key::Text(" ".to_owned()),
        KeyName::Left => Key::Left,
        KeyName::Right => Key::Right,
        KeyName::Up => Key::Up,
        KeyName::Down => Key::Down,
        KeyName::Home => Key::Home,
        KeyName::End => Key::End,
        KeyName::Delete => Key::Delete,
        KeyName::Backspace => Key::Backspace,
        KeyName::Escape => Key::Escape,
        KeyName::Function(number) => Key::Function(number),
    };
    KeyEvent {
        key,
        mods: Mods {
            cmd: chord.command,
            ctrl: chord.command,
            shift: chord.shift,
            ..Mods::default()
        },
    }
}

/// The window has asked for its next frame, not for a later timer such
/// as a tooltip's.
fn poll_is_due(rt: &rune::Runtime<super::Ui>) -> bool {
    rt.wake_deadline()
        .is_some_and(|due| due <= rt.clock + crate::testing::FRAME)
}

/// Rune polls before it runs a frame's handlers. Space sets the editor
/// playing after that frame's poll, so without another poll playback
/// never starts and nothing asks for a later frame.
#[test]
fn a_key_is_followed_by_a_poll() {
    use crate::testing;
    let mut rt = testing::window(super::Ui::headless());
    assert_eq!(
        rt.wake_deadline(),
        None,
        "a resting window asks for no frame"
    );
    testing::input_arrives();
    rt.key(KeyEvent {
        key: Key::Text(" ".to_owned()),
        mods: Mods::default(),
    });
    rt.frame(testing::FRAME);
    assert!(rt.state.editor.playing, "Space plays");
    assert!(
        poll_is_due(&rt),
        "nothing would poll the editor after the key"
    );
}

/// The same for the pointer: a press handled after the poll.
#[test]
fn a_press_is_followed_by_a_poll() {
    use crate::testing;
    let mut rt = testing::window(super::Ui::headless());
    let (x, y) = rt.find("transport-PlayPause").expect("the play button");
    testing::input_arrives();
    rt.pointer_move(x, y);
    rt.frame(testing::FRAME);
    testing::input_arrives();
    rt.pointer_down(x, y);
    rt.frame(testing::FRAME);
    rt.frame(testing::FRAME);
    testing::input_arrives();
    rt.pointer_up(x, y);
    rt.frame(testing::FRAME);
    assert!(rt.state.editor.playing, "the button plays");
    assert!(
        poll_is_due(&rt),
        "nothing would poll the editor after the press"
    );
}

/// A binding whose key the window cannot name would never fire.
#[test]
fn every_bound_chord_is_one_the_window_can_read() {
    for binding in BINDINGS {
        for chord in binding.keys {
            assert_eq!(
                super::chord(&event(*chord)),
                Some(*chord),
                "{:?} never reaches {:?}",
                chord,
                binding.action
            );
        }
    }
}

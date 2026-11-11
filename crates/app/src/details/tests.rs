use eddie_core::editor::details::MAX_VOLUME;
use eddie_core::editor::media::{AssetState, BinAsset};
use eddie_core::editor::sweep::SweepData;
use openreel_engine::{Clip, ClipKind, TitleStyle};
use rune::{Env, Key, KeyEvent, Mods, Runtime};

use super::styles::option_key;
use super::*;
use crate::app;

const FRAME: f64 = 1.0 / 60.0;

/// A window whose only clip is selected: a title from 2 s to 5 s, or
/// 4 s of a 10 s file with sound, placed from 1 s of the source.
fn window(kind: ClipKind) -> (Runtime<Ui>, ClipId) {
    let mut ui = Ui::headless();
    ui.editor.assets.push(BinAsset {
        info: openreel_media::MediaInfo {
            path: "/media/a.mp4".to_owned(),
            name: "a.mp4".to_owned(),
            duration: 10.0,
            width: 320,
            height: 180,
            fps: 30.0,
            has_video: true,
            has_audio: true,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks: Vec::new(),
        beats: None,
        thumbs: Vec::new(),
        sweep: SweepData::default(),
    });
    let id = match kind {
        ClipKind::Title => ui.editor.timeline.add_clip("Title", kind, 0, 2.0, 3.0, 0.0),
        _ => ui
            .editor
            .timeline
            .add_media_clip("Clip", kind, 0, 2.0, 4.0, 1.0, "/media/a.mp4"),
    };
    ui.editor.selected = Some(id);
    let mut rt = Runtime::new(ui, app::view, (1400.0, 1200.0), Env::new(true, 1.0));
    rt.poll = Some(app::poll);
    for _ in 0..3 {
        rt.frame(FRAME);
    }
    (rt, id)
}

fn clip(rt: &Runtime<Ui>, id: ClipId) -> &Clip {
    rt.state
        .editor
        .timeline
        .clip(id)
        .expect("the clip is on the timeline")
}

fn click(rt: &mut Runtime<Ui>, key: &str) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("{key} is not shown"));
    rt.pointer_move(x, y);
    rt.frame(FRAME);
    rt.pointer_down(x, y);
    rt.frame(FRAME);
    rt.pointer_up(x, y);
    rt.frame(FRAME);
    rt.frame(FRAME);
}

fn press(rt: &mut Runtime<Ui>, key: Key) {
    rt.key(KeyEvent {
        key,
        mods: Mods::default(),
    });
    rt.frame(FRAME);
    rt.frame(FRAME);
}

fn typed(rt: &Runtime<Ui>, field: Field) -> &str {
    rt.state.details.entry(field).edit.text.as_str()
}

#[test]
fn enter_applies_a_typed_value_and_shows_what_the_clip_took() {
    let (mut rt, id) = window(ClipKind::Video);
    assert_eq!(typed(&rt, Field::Duration), "4.00 s");
    click(&mut rt, "details-duration");
    assert!(rt.state.typing());
    rt.paste("3");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    assert_eq!(clip(&rt, id).duration, 3.0);
    assert_eq!(typed(&rt, Field::Duration), "3.00 s");
    assert!(!rt.state.typing());

    // The source is 10 s and the clip starts 1 s into it.
    click(&mut rt, "details-duration");
    rt.paste("60 s");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    assert_eq!(clip(&rt, id).duration, 9.0);
    assert_eq!(typed(&rt, Field::Duration), "9.00 s");
}

#[test]
fn escape_leaves_the_clip_alone_and_leaving_the_field_applies() {
    let (mut rt, id) = window(ClipKind::Video);
    click(&mut rt, "details-start");
    rt.paste("7");
    rt.frame(FRAME);
    press(&mut rt, Key::Escape);
    assert_eq!(clip(&rt, id).start, 2.0);
    assert_eq!(typed(&rt, Field::Start), "2.00 s");
    assert!(!rt.state.typing());

    click(&mut rt, "details-start");
    rt.paste("0:05");
    rt.frame(FRAME);
    click(&mut rt, "details-duration");
    assert_eq!(clip(&rt, id).start, 5.0);
    assert_eq!(typed(&rt, Field::Start), "5.00 s");

    // Text that is no time changes nothing.
    click(&mut rt, "details-start");
    rt.paste("soon");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    assert_eq!(clip(&rt, id).start, 5.0);
    assert_eq!(typed(&rt, Field::Start), "5.00 s");
}

#[test]
fn an_untouched_field_does_not_round_the_clip() {
    let (mut rt, id) = window(ClipKind::Video);
    rt.state.editor.timeline.clip_mut(id).unwrap().source_in = 1.2345;
    rt.frame(FRAME);
    click(&mut rt, "details-source-in");
    click(&mut rt, "details-duration");
    assert_eq!(clip(&rt, id).source_in, 1.2345);
}

#[test]
fn dragging_a_slider_sets_the_value_and_its_percentage() {
    let (mut rt, id) = window(ClipKind::Video);
    let (x, y) = rt.find("details-opacity-slider").expect("the slider");
    rt.pointer_move(x, y);
    rt.frame(FRAME);
    rt.pointer_down(x, y);
    rt.frame(FRAME);
    rt.pointer_move(x - 20.0, y);
    rt.frame(FRAME);
    assert!(rt.state.pointer_held, "undo waits while the slider is held");
    rt.pointer_move(x - 400.0, y);
    rt.frame(FRAME);
    rt.pointer_up(x - 400.0, y);
    rt.frame(FRAME);
    rt.frame(FRAME);
    assert_eq!(clip(&rt, id).opacity, 0.0);
    assert_eq!(typed(&rt, Field::Opacity), "0%");
    assert!(!rt.state.pointer_held);

    // The field takes more than the slider's top and keeps the limit.
    click(&mut rt, "details-volume");
    rt.paste("350");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    assert_eq!(clip(&rt, id).volume, MAX_VOLUME);
    assert_eq!(typed(&rt, Field::Volume), "200%");
}

#[test]
fn detach_links_a_sound_clip_and_unlink_frees_it() {
    let (mut rt, id) = window(ClipKind::Video);
    assert!(rt.find("details-unlink").is_none());
    click(&mut rt, "details-detach");
    let partner = clip(&rt, id).linked_clip.expect("the sound is linked");
    assert!(!clip(&rt, id).embedded_audio);
    assert!(rt.find("details-detach").is_none());
    assert!(rt.find("details-linked").is_some());
    click(&mut rt, "details-unlink");
    assert_eq!(clip(&rt, id).linked_clip, None);
    assert_eq!(clip(&rt, partner).linked_clip, None);
}

#[test]
fn sound_switches_and_fades_edit_the_clip() {
    let (mut rt, id) = window(ClipKind::Audio);
    assert!(rt.find("details-opacity-slider").is_none());
    let before = clip(&rt, id).duck;
    click(&mut rt, "details-duck");
    assert_eq!(clip(&rt, id).duck, !before);
    click(&mut rt, "details-smooth-fades");
    assert!(clip(&rt, id).smooth_fades);
    click(&mut rt, "details-fade-in");
    rt.paste("0.5");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    assert_eq!(clip(&rt, id).fade_in, 0.5);
    assert_eq!(clip(&rt, id).fade_out, 0.0);
}

#[test]
fn a_title_takes_its_text_as_typed_on_one_line_and_its_style_from_the_list() {
    let (mut rt, id) = window(ClipKind::Title);
    rt.state.focus_title = true;
    rt.frame(FRAME);
    rt.frame(FRAME);
    assert!(rt.state.details.name.focused);
    rt.paste(" card\ntwo");
    rt.frame(FRAME);
    rt.frame(FRAME);
    assert_eq!(clip(&rt, id).name, "Title card two");
    press(&mut rt, Key::Enter);
    assert!(!rt.state.typing());
    assert_eq!(clip(&rt, id).name, "Title card two");

    let style = TitleStyle::ALL[2];
    assert_ne!(clip(&rt, id).title_style, style);
    click(&mut rt, "details-style");
    assert_eq!(rt.state.details.select, Some(Select::Title));
    click(&mut rt, &option_key(style));
    assert_eq!(clip(&rt, id).title_style, style);
    assert_eq!(rt.state.details.select, None);
}

#[test]
fn split_and_delete_act_on_the_selected_clip() {
    let (mut rt, id) = window(ClipKind::Title);
    rt.state.editor.playhead = 3.0;
    rt.frame(FRAME);
    click(&mut rt, "details-split");
    let right = rt
        .state
        .editor
        .selected
        .expect("the right half is selected");
    assert_ne!(right, id);
    assert_eq!(clip(&rt, id).duration, 1.0);
    click(&mut rt, "details-delete");
    assert!(rt.state.editor.timeline.clip(right).is_none());
    assert!(rt.find("details-empty").is_some());
}

#[test]
fn every_control_has_a_tooltip() {
    let cases: [(ClipKind, &[&str]); 3] = [
        (
            ClipKind::Video,
            &[
                "details-name",
                "details-start",
                "details-duration",
                "details-source-in",
                "details-opacity-slider",
                "details-opacity",
                "details-volume-slider",
                "details-volume",
                "details-embedded-audio",
                "details-detach",
                "details-remove-silence",
                "details-split",
                "details-delete",
                "details-resize",
            ],
        ),
        (
            ClipKind::Audio,
            &[
                "details-fade-in",
                "details-fade-out",
                "details-smooth-fades",
                "details-duck",
                "details-clean-voice",
            ],
        ),
        (ClipKind::Title, &["details-title-box", "details-style"]),
    ];
    for (kind, keys) in cases {
        let (mut rt, _) = window(kind);
        rt.help_delay = 0.0;
        for key in keys {
            let (x, y) = rt
                .find(key)
                .unwrap_or_else(|| panic!("{key} is not shown for {kind:?}"));
            // Rest the pointer on the control, coming from where nothing explains itself.
            rt.pointer_move(700.0, 300.0);
            rt.frame(FRAME);
            rt.pointer_move(x, y);
            rt.frame(FRAME);
            rt.frame(FRAME);
            let shown = rt.help_tooltip().map(|(words, _)| words.to_owned());
            assert!(
                shown.as_deref().is_some_and(|words| !words.is_empty()),
                "{key} has no tooltip"
            );
        }
    }
}

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
            photo: false,
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

/// Scroll Details until `key` is inside its visible part.
fn reveal(rt: &mut Runtime<Ui>, key: &str) {
    let (Some(([x, top, w, height], _)), Some((_, y))) =
        (rt.presented("details-scroll"), rt.find(key))
    else {
        return;
    };
    if y < top + 20.0 || y > top + height - 20.0 {
        rt.wheel(x + w / 2.0, top + height / 2.0, top + height / 2.0 - y);
        rt.frame(FRAME);
        rt.frame(FRAME);
    }
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
fn the_text_card_sets_a_titles_size_colours_box_outline_and_place() {
    use openreel_engine::{DEFAULT_OUTLINE, Rgb, TextLook};
    let (mut rt, id) = window(ClipKind::Title);
    let text = |rt: &Runtime<Ui>| clip(rt, id).text;
    let drawn = |rt: &Runtime<Ui>| openreel_media::drawn_text(clip(rt, id));
    let tap = |rt: &mut Runtime<Ui>, key: &str| {
        reveal(rt, key);
        click(rt, key);
    };
    let enter = |rt: &mut Runtime<Ui>, key: &str, typed: &str| {
        tap(rt, key);
        rt.paste(typed);
        rt.frame(FRAME);
        press(rt, Key::Enter);
    };

    tap(&mut rt, "details-text-yellow");
    assert_eq!(text(&rt).color, Some(Rgb(255, 214, 0)));
    enter(&mut rt, "details-text-color", "#36f");
    assert_eq!(text(&rt).color, Some(Rgb(0x33, 0x66, 0xFF)));
    assert_eq!(typed(&rt, Field::TextColor), "#3366FF");

    // A lower third has a box: None takes it away, and a colour brings it
    // back at the style's opacity.
    tap(&mut rt, "details-box-none");
    assert_eq!(drawn(&rt).box_opacity, Some(0.0));
    tap(&mut rt, "details-box-red");
    assert_eq!(text(&rt).box_color, Some(Rgb(255, 59, 48)));
    assert_eq!(drawn(&rt).box_opacity, Some(0.55_f32 as f64));

    // It has no outline: a colour draws one at the usual thickness.
    assert_eq!(drawn(&rt).outline, Some(0.0));
    tap(&mut rt, "details-outline-green");
    assert_eq!(drawn(&rt).outline, Some(DEFAULT_OUTLINE));
    assert_eq!(typed(&rt, Field::Outline), "6%");

    enter(&mut rt, "details-text-size", "150");
    enter(&mut rt, "details-text-x", "25");
    enter(&mut rt, "details-text-y", "-300");
    let t = text(&rt);
    assert_eq!(
        (t.size, t.x, t.y),
        (1.5, 0.25, -1.0),
        "y stops a canvas away"
    );

    tap(&mut rt, "details-text-reset");
    assert_eq!(text(&rt), TextLook::default());
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
                "details-speed-slider",
                "details-speed",
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
        (
            ClipKind::Title,
            &[
                "details-title-box",
                "details-style",
                "details-text-size-slider",
                "details-text-size",
                "details-text-color",
                "details-text-yellow",
                "details-box-color",
                "details-box-none",
                "details-box-blue",
                "details-box-opacity-slider",
                "details-box-opacity",
                "details-outline-color",
                "details-outline-none",
                "details-outline-red",
                "details-outline-slider",
                "details-outline",
                "details-text-x",
                "details-text-y",
                "details-text-reset",
            ],
        ),
    ];
    for (kind, keys) in cases {
        let (mut rt, _) = window(kind);
        rt.help_delay = 0.0;
        for key in keys {
            reveal(&mut rt, key);
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

#[test]
fn the_colour_card_grades_the_clip_from_its_list_sliders_and_fields() {
    use openreel_engine::{ColorAdjust, Filter};
    let (mut rt, id) = window(ClipKind::Video);
    // The card sits below Transform.
    reveal(&mut rt, "details-color-reset");
    let color = |rt: &Runtime<Ui>| clip(rt, id).color;

    // Strength shows once there is a filter to weaken. A tile picks its
    // filter at full strength; the current one keeps its strength.
    assert!(rt.find("details-strength").is_none());
    click(&mut rt, "details-filter-vintage");
    assert_eq!(color(&rt).filter, Filter::Vintage);
    assert!(rt.find("details-strength").is_some());
    rt.state.editor.edit_clip_color(id, |c| c.strength = 0.4);
    click(&mut rt, "details-filter-vintage");
    assert_eq!(color(&rt).strength, 0.4);
    click(&mut rt, "details-filter-warm");
    assert_eq!(
        (color(&rt).filter, color(&rt).strength),
        (Filter::Warm, 1.0)
    );

    // Exposure is typed in stops, with or without its unit; other values
    // in percent, moved into range.
    click(&mut rt, "details-exposure");
    rt.paste("1.5 ev");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    assert_eq!(color(&rt).exposure, 1.5);
    assert_eq!(typed(&rt, Field::Exposure), "1.5 EV");
    click(&mut rt, "details-saturation");
    rt.paste("-250");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    assert_eq!(color(&rt).saturation, -1.0);
    assert_eq!(typed(&rt, Field::Saturation), "-100%");

    // A slider over -100% to 100%: its ends are the range's ends.
    let ([x, y, w, h], _) = rt
        .presented("details-brightness-slider")
        .expect("the brightness slider");
    let y = y + h / 2.0;
    rt.pointer_move(x + w / 2.0, y);
    rt.frame(FRAME);
    rt.pointer_down(x + w / 2.0, y);
    rt.frame(FRAME);
    rt.pointer_move(x + w, y);
    rt.frame(FRAME);
    assert!(rt.state.details.slider_held);
    assert_eq!(color(&rt).brightness, 1.0);
    rt.pointer_move(x, y);
    rt.frame(FRAME);
    rt.pointer_up(x, y);
    rt.frame(FRAME);
    assert_eq!(color(&rt).brightness, -1.0);
    assert!(!rt.state.details.slider_held);

    rt.help_delay = 0.0;
    for key in [
        "details-filter-none",
        "details-filter-noir",
        "details-strength-slider",
        "details-strength",
        "details-brightness-slider",
        "details-contrast",
        "details-exposure",
        "details-temperature-slider",
        "details-tint",
        "details-color-reset",
    ] {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("{key} is not shown"));
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

    click(&mut rt, "details-color-reset");
    assert_eq!(color(&rt), ColorAdjust::default());
    assert!(rt.find("details-strength").is_none());
}

#[test]
fn speed_changes_the_length_and_slows_only_as_far_as_the_lane_has_room() {
    // Four source seconds from 1 s, placed at 2 s.
    let (mut rt, id) = window(ClipKind::Video);
    click(&mut rt, "details-speed");
    rt.paste("2x");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    assert_eq!((clip(&rt, id).speed, clip(&rt, id).duration), (2.0, 2.0));
    assert_eq!(typed(&rt, Field::Speed), "2.00×");
    assert_eq!(typed(&rt, Field::Duration), "2.00 s");

    // With a clip at 8 s, the slider's slow end stops where the four
    // seconds still fit in the six before it: 4 / 6, rounded up to 0.67x.
    rt.state.editor.timeline.add_media_clip(
        "Next",
        ClipKind::Video,
        0,
        8.0,
        1.0,
        0.0,
        "/media/a.mp4",
    );
    let (x, y) = rt.find("details-speed-slider").expect("the slider");
    rt.pointer_move(x, y);
    rt.frame(FRAME);
    rt.pointer_down(x, y);
    rt.frame(FRAME);
    rt.pointer_move(x - 400.0, y);
    rt.frame(FRAME);
    rt.pointer_up(x - 400.0, y);
    rt.frame(FRAME);
    rt.frame(FRAME);
    let slowed = clip(&rt, id);
    assert_eq!(slowed.speed, 0.67);
    assert!(
        slowed.end() <= 8.0,
        "runs into the next clip: {}",
        slowed.end()
    );
    assert_eq!(typed(&rt, Field::Speed), "0.67×");
    assert!(
        rt.state.editor.status.contains("0.67×"),
        "{}",
        rt.state.editor.status
    );

    // Back at normal speed the clip covers the same four source seconds.
    click(&mut rt, "details-speed");
    rt.paste("1");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    let normal = clip(&rt, id);
    assert!((normal.duration - 4.0).abs() < 1e-9, "{}", normal.duration);
    assert!((normal.source_end() - 5.0).abs() < 1e-9);
}

#[test]
fn a_photo_has_a_length_but_no_source_range_speed_or_sound() {
    let (mut rt, id) = window(ClipKind::Video);
    let info = &mut rt.state.editor.assets[0].info;
    (info.photo, info.has_audio, info.duration, info.fps) = (true, false, 0.0, 0.0);
    let photo = rt.state.editor.timeline.clip_mut(id).unwrap();
    (photo.photo, photo.source_in) = (true, 0.0);
    rt.frame(FRAME);
    rt.frame(FRAME);
    for key in ["details-start", "details-duration", "details-opacity"] {
        assert!(rt.find(key).is_some(), "{key} is not shown");
    }
    for key in [
        "details-source-in",
        "details-speed",
        "details-volume",
        "details-detach",
    ] {
        assert!(rt.find(key).is_none(), "{key} is shown for a photo");
    }
    // No source runs out: it lasts as long as it is told to.
    click(&mut rt, "details-duration");
    rt.paste("60 s");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    assert_eq!(clip(&rt, id).duration, 60.0);
}

#[test]
fn the_transition_card_shows_after_a_cut_and_sets_the_kind_and_length() {
    use openreel_engine::TransitionKind;
    let (mut rt, id) = window(ClipKind::Video);
    assert!(
        rt.find("details-transition-none").is_none(),
        "nothing comes before the clip, so there is nothing to pass from"
    );
    // A clip that ends where the selected one starts.
    rt.state.editor.timeline.add_media_clip(
        "Before",
        ClipKind::Video,
        0,
        0.0,
        2.0,
        0.0,
        "/media/a.mp4",
    );
    rt.frame(FRAME);
    rt.frame(FRAME);
    reveal(&mut rt, "details-transition-mix");
    assert!(rt.find("details-transition-length").is_none());
    click(&mut rt, "details-transition-dip-white");
    // The Duration row grows into the card.
    for _ in 0..30 {
        rt.frame(FRAME);
    }
    let transition = |rt: &Runtime<Ui>| clip(rt, id).transition;
    assert_eq!(
        transition(&rt).map(|t| (t.kind, t.duration)),
        Some((TransitionKind::DipWhite, 0.5))
    );

    // The length is typed in seconds and moved into range; another kind
    // keeps it.
    reveal(&mut rt, "details-transition-length");
    click(&mut rt, "details-transition-length");
    rt.paste("9");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    assert_eq!(transition(&rt).unwrap().duration, 5.0);
    assert_eq!(typed(&rt, Field::TransitionLength), "5.00 s");
    reveal(&mut rt, "details-transition-slide-up");
    click(&mut rt, "details-transition-slide-up");
    assert_eq!(
        transition(&rt).map(|t| (t.kind, t.duration)),
        Some((TransitionKind::SlideUp, 5.0))
    );

    rt.help_delay = 0.0;
    let mut keys = vec![
        "details-transition-none".to_owned(),
        "details-transition-length".to_owned(),
    ];
    keys.extend(TransitionKind::ALL.map(|kind| format!("details-transition-{}", kind.as_str())));
    for key in keys {
        reveal(&mut rt, &key);
        let (x, y) = rt
            .find(&key)
            .unwrap_or_else(|| panic!("{key} is not shown"));
        rt.pointer_move(700.0, 300.0);
        rt.frame(FRAME);
        rt.pointer_move(x, y);
        rt.frame(FRAME);
        rt.frame(FRAME);
        let shown = rt.help_tooltip().map(|(words, _)| words.to_owned());
        assert!(
            shown.is_some_and(|words| !words.is_empty()),
            "{key} has no tooltip"
        );
    }

    click(&mut rt, "details-transition-none");
    assert_eq!(transition(&rt), None);
    assert!(rt.find("details-transition-length").is_none());
}

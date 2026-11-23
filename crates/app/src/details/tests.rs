use eddie_core::editor::details::MAX_VOLUME;
use eddie_core::editor::media::{AssetState, BinAsset};
use eddie_core::editor::sweep::SweepData;
use openreel_engine::{Clip, ClipKind, Ease, TitleStyle};
use rune::{Key, KeyEvent, Mods, Runtime};

use super::styles::{blend_key, ease_key, option_key, stabilize_key};
use super::*;
use crate::app;
use crate::testing::shown;

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
            alpha: false,
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
    let mut rt = crate::testing::runtime(ui, (1400.0, 1200.0));
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
    // The buttons under the changed rows fade in.
    for _ in 0..30 {
        rt.frame(FRAME);
    }
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
    click(&mut rt, "details-reduce-noise");
    assert!(clip(&rt, id).reduce_noise);
    click(&mut rt, "details-fade-in");
    rt.paste("0.5");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    assert_eq!(clip(&rt, id).fade_in, 0.5);
    assert_eq!(clip(&rt, id).fade_out, 0.0);
}

#[test]
fn a_title_takes_its_lines_as_typed_and_its_style_from_the_list() {
    let (mut rt, id) = window(ClipKind::Title);
    rt.state.focus_title = true;
    rt.frame(FRAME);
    rt.frame(FRAME);
    assert!(rt.state.details.name.focused);
    // A pasted line break stays one, Shift+Enter starts another, and
    // Enter ends the edit.
    rt.paste(" card\r\ntwo");
    rt.frame(FRAME);
    rt.key(KeyEvent {
        key: Key::Enter,
        mods: Mods {
            shift: true,
            ..Mods::default()
        },
    });
    rt.frame(FRAME);
    rt.paste("three");
    rt.frame(FRAME);
    rt.frame(FRAME);
    assert_eq!(clip(&rt, id).name, "Title card\ntwo\nthree");
    assert!(rt.state.typing(), "Shift+Enter keeps the box open");
    press(&mut rt, Key::Enter);
    assert!(!rt.state.typing());
    assert_eq!(clip(&rt, id).name, "Title card\ntwo\nthree");

    let style = TitleStyle::ALL[2];
    assert_ne!(clip(&rt, id).title_style, style);
    click(&mut rt, "details-style");
    assert_eq!(rt.state.details.select, Some(Select::Title));
    click(&mut rt, &option_key(style));
    assert_eq!(clip(&rt, id).title_style, style);
    assert_eq!(rt.state.details.select, None);
}

#[test]
fn a_pictures_blend_mode_comes_from_its_list() {
    use openreel_engine::BlendMode;
    let (mut rt, id) = window(ClipKind::Video);
    assert_eq!(clip(&rt, id).blend, BlendMode::Normal);
    click(&mut rt, "details-blend");
    assert_eq!(rt.state.details.select, Some(Select::Blend));
    for mode in BlendMode::ALL {
        assert!(rt.find(&blend_key(mode)).is_some(), "{mode:?} is listed");
    }
    click(&mut rt, &blend_key(BlendMode::Screen));
    assert_eq!(clip(&rt, id).blend, BlendMode::Screen);
    assert_eq!(rt.state.details.select, None);
    // A title has none.
    let (rt, _) = window(ClipKind::Title);
    assert!(rt.find("details-blend").is_none());
}

#[test]
fn a_pictures_stabilize_level_comes_from_its_list() {
    use openreel_engine::Stabilize;
    let (mut rt, id) = window(ClipKind::Video);
    click(&mut rt, "details-stabilize");
    assert_eq!(rt.state.details.select, Some(Select::Stabilize));
    for level in std::iter::once(None).chain(Stabilize::ALL.map(Some)) {
        assert!(
            rt.find(&stabilize_key(level)).is_some(),
            "{level:?} is listed"
        );
    }
    click(&mut rt, &stabilize_key(Some(Stabilize::Recommended)));
    assert_eq!(rt.state.details.select, None);
    // The clip's file is not real, so making the steadied one may already
    // have failed by now.
    let editor = &rt.state.editor;
    let asked = editor.remake_pending(id).map(|spec| spec.stabilize);
    assert!(
        asked == Some(Some(Stabilize::Recommended)) || editor.status.contains("Could not make"),
        "{asked:?}: {}",
        editor.status
    );
    // A title and a sound have none.
    for kind in [ClipKind::Title, ClipKind::Audio] {
        let (rt, _) = window(kind);
        assert!(rt.find("details-stabilize").is_none(), "{kind:?}");
    }
}

#[test]
fn the_text_card_sets_a_titles_size_colours_box_outline_and_place() {
    use openreel_engine::{DEFAULT_OUTLINE, DEFAULT_SHADOW_OPACITY, Rgb, TextLook};
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

    // The font list draws each name in its font; Bold and Italic pick
    // those faces, and switch off for a font that has none.
    tap(&mut rt, "details-text-font");
    tap(&mut rt, "details-font-lora");
    assert_eq!(text(&rt).font, Some(openreel_engine::TitleFont::Lora));
    assert!(!shown(&rt, "details-select"), "a pick closes the list");
    tap(&mut rt, "details-text-bold");
    tap(&mut rt, "details-text-italic");
    let lora = drawn(&rt);
    assert!(lora.bold && lora.italic, "{lora:?}");
    tap(&mut rt, "details-text-font");
    tap(&mut rt, "details-font-bebas-neue");
    let bebas = drawn(&rt);
    assert!(!bebas.bold && !bebas.italic, "Bebas Neue has one face");
    tap(&mut rt, "details-text-bold");
    assert!(text(&rt).bold, "a disabled toggle changes nothing");

    // A lower third is lined up on the left until another is picked.
    use openreel_engine::TextAlign;
    assert_eq!(drawn(&rt).align, Some(TextAlign::Left));
    tap(&mut rt, "details-text-align-right");
    assert_eq!(text(&rt).align, Some(TextAlign::Right));

    // No style has a shadow, and how it falls shows only once there is
    // one: a colour turns it on at the usual opacity.
    assert_eq!(drawn(&rt).shadow_opacity, Some(0.0));
    assert!(!shown(&rt, "details-shadow-angle"));
    tap(&mut rt, "details-shadow-purple");
    assert_eq!(text(&rt).shadow_color, Some(Rgb(168, 85, 247)));
    assert_eq!(drawn(&rt).shadow_opacity, Some(DEFAULT_SHADOW_OPACITY));
    assert_eq!(typed(&rt, Field::ShadowOpacity), "70%");
    enter(&mut rt, "details-shadow-angle", "135°");
    enter(&mut rt, "details-shadow-distance", "20");
    enter(&mut rt, "details-shadow-blur", "90");
    let t = text(&rt);
    assert_eq!(
        (t.shadow_angle, t.shadow_distance, t.shadow_blur),
        (135.0, 0.2, 0.25),
        "blur stops at a quarter of the text size"
    );
    // The Angle slider's arrow keys turn it a whole degree.
    reveal(&mut rt, "details-shadow-angle-slider");
    click(&mut rt, "details-shadow-angle-slider");
    let before = text(&rt).shadow_angle;
    press(&mut rt, Key::Right);
    assert_eq!(text(&rt).shadow_angle, (before + 1.0).min(180.0));
    tap(&mut rt, "details-shadow-none");
    assert_eq!(drawn(&rt).shadow_opacity, Some(0.0));
    assert!(!shown(&rt, "details-shadow-angle"), "no shadow, no angle");

    tap(&mut rt, "details-text-reset");
    assert_eq!(text(&rt), TextLook::default());
}

/// Apply to all gives every caption the selected one's style and look,
/// and leaves lower thirds alone; a lower third has no such button.
#[test]
fn apply_to_all_gives_every_caption_this_captions_look() {
    use openreel_engine::{Rgb, TextLook};
    let (mut rt, id) = window(ClipKind::Title);
    assert!(!shown(&rt, "details-text-all-captions"), "a lower third");
    let timeline = &mut rt.state.editor.timeline;
    let other = timeline.add_clip("Second", ClipKind::Title, 0, 6.0, 2.0, 0.0);
    let lower = timeline.add_clip("Name", ClipKind::Title, 1, 2.0, 2.0, 0.0);
    timeline.clip_mut(other).unwrap().title_style = TitleStyle::Caption;
    let look = TextLook {
        size: 1.5,
        color: Some(Rgb(255, 214, 0)),
        y: -0.2,
        ..TextLook::default()
    };
    let this = timeline.clip_mut(id).unwrap();
    (this.title_style, this.text) = (TitleStyle::PopCaption, look);
    rt.frame(FRAME);
    rt.frame(FRAME);
    reveal(&mut rt, "details-text-all-captions");
    click(&mut rt, "details-text-all-captions");
    let got = |rt: &Runtime<Ui>, id| (clip(rt, id).title_style, clip(rt, id).text);
    assert_eq!(got(&rt, other), (TitleStyle::PopCaption, look));
    assert_eq!(
        got(&rt, lower),
        (TitleStyle::LowerThird, TextLook::default()),
        "a lower third keeps its look"
    );
    assert_eq!(
        rt.state.editor.status,
        "1 more caption looks like this one."
    );
}

#[test]
fn the_chroma_key_card_keys_a_picture_from_its_switch_presets_and_fields() {
    use openreel_engine::{ChromaKey, KEY_BLUE, Rgb};
    let (mut rt, id) = window(ClipKind::Video);
    let key = |rt: &Runtime<Ui>| clip(rt, id).chroma;
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
    assert!(
        !shown(&rt, "details-key-strength"),
        "no controls without a key"
    );
    tap(&mut rt, "details-chroma");
    assert_eq!(key(&rt), Some(ChromaKey::default()));
    tap(&mut rt, "details-chroma-blue");
    assert_eq!(key(&rt).unwrap().color, KEY_BLUE);
    assert_eq!(typed(&rt, Field::KeyColor), "#0047BB");
    enter(&mut rt, "details-key-color", "#1a2b3c");
    assert_eq!(key(&rt).unwrap().color, Rgb(0x1A, 0x2B, 0x3C));
    enter(&mut rt, "details-key-strength", "70");
    enter(&mut rt, "details-key-softness", "0");
    enter(&mut rt, "details-key-spill", "150");
    let k = key(&rt).unwrap();
    assert_eq!(
        (k.strength, k.softness, k.spill),
        (0.7, 0.0, 1.0),
        "spill stops at 100%"
    );

    // Pick waits for a click on the monitor; pressing it again stops.
    tap(&mut rt, "details-chroma-pick");
    assert_eq!(rt.state.key_pick, Some(id));
    tap(&mut rt, "details-chroma-pick");
    assert_eq!(rt.state.key_pick, None);

    tap(&mut rt, "details-chroma");
    assert_eq!(key(&rt), None);
    assert!(!shown(&rt, "details-key-strength"));
}

#[test]
fn the_mask_card_picks_a_shape_and_places_sizes_turns_and_inverts_it() {
    use openreel_engine::{Mask, MaskShape};
    let (mut rt, id) = window(ClipKind::Video);
    let mask = |rt: &Runtime<Ui>| clip(rt, id).mask;
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
    assert!(
        !shown(&rt, "details-mask-width"),
        "no controls without a mask"
    );
    tap(&mut rt, "details-mask-heart");
    assert_eq!(mask(&rt), Some(Mask::new(MaskShape::Heart)));
    enter(&mut rt, "details-mask-x", "25");
    enter(&mut rt, "details-mask-y", "-10%");
    enter(&mut rt, "details-mask-width", "80");
    enter(&mut rt, "details-mask-height", "900");
    enter(&mut rt, "details-mask-rotation", "45°");
    enter(&mut rt, "details-mask-feather", "20");
    let m = mask(&rt).unwrap();
    assert_eq!(
        (m.x, m.y, m.width, m.height, m.rotation, m.feather),
        (0.25, -0.1, 0.8, 5.0, 45.0, 0.2),
        "height stops at 500%"
    );
    assert!(!shown(&rt, "details-mask-round"), "only a rectangle rounds");
    // Another shape keeps where the mask is and how big.
    tap(&mut rt, "details-mask-rectangle");
    let m = mask(&rt).unwrap();
    assert_eq!((m.shape, m.x, m.width), (MaskShape::Rectangle, 0.25, 0.8));
    enter(&mut rt, "details-mask-round", "40");
    assert_eq!(mask(&rt).unwrap().round, 0.4);
    // Split runs across the whole picture: no width or height.
    tap(&mut rt, "details-mask-split");
    assert!(!shown(&rt, "details-mask-width") && !shown(&rt, "details-mask-height"));
    tap(&mut rt, "details-mask-invert");
    assert!(mask(&rt).unwrap().invert);
    tap(&mut rt, "details-mask-reset");
    assert_eq!(mask(&rt), Some(Mask::new(MaskShape::Split)));
    tap(&mut rt, "details-mask-none");
    assert_eq!(mask(&rt), None);
    assert!(!shown(&rt, "details-mask-invert"));
}

#[test]
fn the_shape_card_draws_a_shape_again_from_its_tiles_swatches_fields_and_sliders() {
    use openreel_engine::{Graphic, Rgb, Shape, ShapeKind};
    let dir = std::env::temp_dir().join(format!("eddie-shape-card-{}", std::process::id()));
    let mut ui = Ui::headless();
    ui.editor.graphics_dir = Some(dir.clone());
    let rectangle = Graphic::Shape(Shape::new(ShapeKind::Rectangle));
    let id = ui.editor.add_graphic(rectangle, None, 1.0, 3.0).unwrap();
    let mut rt = crate::testing::runtime(ui, (1400.0, 1200.0));
    rt.poll = Some(app::poll);
    for _ in 0..3 {
        rt.frame(FRAME);
    }
    let shape = |rt: &Runtime<Ui>| match &clip(rt, id).graphic {
        Some(Graphic::Shape(shape)) => *shape,
        other => panic!("not a shape: {other:?}"),
    };
    let source = |rt: &Runtime<Ui>| clip(rt, id).source.clone().unwrap();
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

    // Another kind brings its own proportions and keeps the paint, and its
    // drawing is another file.
    let first = source(&rt);
    tap(&mut rt, "details-shape-fill-red");
    tap(&mut rt, "details-shape-heart");
    let heart = shape(&rt);
    assert_eq!(
        (heart.kind, heart.ratio, heart.fill),
        (
            ShapeKind::Heart,
            ShapeKind::Heart.natural_ratio(),
            Some(Rgb(255, 59, 48))
        )
    );
    assert_ne!(source(&rt), first);
    assert!(std::path::Path::new(&source(&rt)).is_file());
    assert_eq!(
        clip(&rt, id).name,
        "Heart",
        "the clip is called what it draws"
    );

    // The outline's thickness shows once there is an outline.
    assert!(!shown(&rt, "details-shape-thickness"));
    enter(&mut rt, "details-shape-outline", "#000000");
    assert_eq!(shape(&rt).outline, Some(Rgb::BLACK));
    enter(&mut rt, "details-shape-thickness", "10");
    assert_eq!(shape(&rt).thickness, 0.1);
    enter(&mut rt, "details-shape-fill", "none");
    assert_eq!(shape(&rt).fill, None);
    assert_eq!(typed(&rt, Field::ShapeFill), "None");
    // Without a fill the outline is all there is: it cannot go too.
    tap(&mut rt, "details-shape-outline-none");
    assert_eq!(shape(&rt).outline, Some(Rgb::BLACK));
    assert!(rt.state.editor.status.contains("fill or an outline"));

    // Only a rectangle and a bubble round their corners.
    assert!(!shown(&rt, "details-shape-round"));
    tap(&mut rt, "details-shape-bubble");
    assert_eq!(shape(&rt).round, 0.5);
    enter(&mut rt, "details-shape-ratio", "2");
    assert_eq!(shape(&rt).ratio, 2.0);
    // The Ratio slider runs on a log scale, its right end ten to one.
    reveal(&mut rt, "details-shape-ratio-slider");
    let ([x, y, w, h], _) = rt.presented("details-shape-ratio-slider").unwrap();
    let y = y + h / 2.0;
    rt.pointer_move(x + w / 2.0, y);
    rt.frame(FRAME);
    rt.pointer_down(x + w / 2.0, y);
    rt.frame(FRAME);
    rt.pointer_move(x + w, y);
    rt.frame(FRAME);
    rt.pointer_up(x + w, y);
    rt.frame(FRAME);
    assert_eq!(shape(&rt).ratio, 10.0);

    rt.help_delay = 0.0;
    for key in [
        "details-shape-rectangle",
        "details-shape-arrow",
        "details-shape-fill",
        "details-shape-fill-none",
        "details-shape-fill-yellow",
        "details-shape-outline",
        "details-shape-outline-blue",
        "details-shape-thickness-slider",
        "details-shape-thickness",
        "details-shape-round-slider",
        "details-shape-round",
        "details-shape-ratio-slider",
        "details-shape-ratio",
        "details-shape-reset",
    ] {
        reveal(&mut rt, key);
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
    tap(&mut rt, "details-shape-reset");
    assert_eq!(shape(&rt), Shape::new(ShapeKind::Bubble));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn split_and_delete_act_on_the_selected_clip() {
    let (mut rt, id) = window(ClipKind::Title);
    rt.state.editor.playhead = 3.0;
    rt.frame(FRAME);
    reveal(&mut rt, "details-split");
    click(&mut rt, "details-split");
    let right = rt
        .state
        .editor
        .selected
        .expect("the right half is selected");
    assert_ne!(right, id);
    assert_eq!(clip(&rt, id).duration, 1.0);
    reveal(&mut rt, "details-delete");
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
                "details-blend",
                "details-stabilize",
                "details-volume-slider",
                "details-volume",
                "details-key-opacity",
                "details-key-volume",
                "details-key-scale",
                "details-key-position",
                "details-key-rotation",
                "details-previous-key",
                "details-next-key",
                "details-ease",
                "details-embedded-audio",
                "details-detach",
                "details-remove-silence",
                "details-chroma",
                "details-key-color",
                "details-chroma-green",
                "details-chroma-blue",
                "details-chroma-pick",
                "details-key-strength-slider",
                "details-key-strength",
                "details-key-softness-slider",
                "details-key-softness",
                "details-key-spill-slider",
                "details-key-spill",
                "details-mask-none",
                "details-mask-split",
                "details-mask-filmstrip",
                "details-mask-circle",
                "details-mask-rectangle",
                "details-mask-heart",
                "details-mask-star",
                "details-mask-x",
                "details-mask-y",
                "details-mask-width-slider",
                "details-mask-width",
                "details-mask-height-slider",
                "details-mask-height",
                "details-mask-rotation-slider",
                "details-mask-rotation",
                "details-mask-feather-slider",
                "details-mask-feather",
                "details-mask-round-slider",
                "details-mask-round",
                "details-mask-invert",
                "details-mask-reset",
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
                "details-reduce-noise",
                "details-clean-voice",
            ],
        ),
        (
            ClipKind::Title,
            &[
                "details-title-box",
                "details-style",
                "details-text-font",
                "details-text-bold",
                "details-text-italic",
                "details-text-align-left",
                "details-text-align-center",
                "details-text-align-right",
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
                "details-shadow-color",
                "details-shadow-none",
                "details-shadow-purple",
                "details-shadow-opacity-slider",
                "details-shadow-opacity",
                "details-shadow-distance-slider",
                "details-shadow-distance",
                "details-shadow-angle-slider",
                "details-shadow-angle",
                "details-shadow-blur-slider",
                "details-shadow-blur",
                "details-text-x",
                "details-text-y",
                "details-text-reset",
                "details-text-all-captions",
            ],
        ),
    ];
    for (kind, keys) in cases {
        let (mut rt, id) = window(kind);
        // A picture with opacity keys shows the key steps and Ease, a
        // chroma key its controls, and a rectangle mask every mask control.
        // A caption shows Apply to all, and a shadow how it falls.
        if kind == ClipKind::Title {
            let clip = rt.state.editor.timeline.clip_mut(id).unwrap();
            clip.title_style = TitleStyle::Caption;
            clip.text.shadow_color = Some(openreel_engine::Rgb::BLACK);
            rt.frame(FRAME);
            rt.frame(FRAME);
        }
        if kind == ClipKind::Video {
            let clip = rt.state.editor.timeline.clip_mut(id).unwrap();
            clip.keyframes
                .set(openreel_engine::Property::Opacity, 0.0, 1.0);
            clip.keyframes
                .set(openreel_engine::Property::Opacity, 2.0, 0.5);
            clip.chroma = Some(Default::default());
            clip.mask = Some(openreel_engine::Mask::new(
                openreel_engine::MaskShape::Rectangle,
            ));
            rt.frame(FRAME);
            rt.frame(FRAME);
        }
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

#[test]
fn key_buttons_and_fields_animate_a_clip_from_the_playhead() {
    let (mut rt, id) = window(ClipKind::Video);
    // The clip runs 2 s to 6 s; a key goes at the playhead.
    rt.state.editor.playhead = 3.0;
    rt.frame(FRAME);
    click(&mut rt, "details-key-opacity");
    assert_eq!(clip(&rt, id).keyframes.opacity.len(), 1);
    assert_eq!(rt.state.editor.status, "Key added.");
    // Once opacity has keys, a value typed at another moment is a key there.
    rt.state.editor.playhead = 5.0;
    rt.frame(FRAME);
    click(&mut rt, "details-opacity");
    rt.paste("30");
    rt.frame(FRAME);
    press(&mut rt, Key::Enter);
    let keys = &clip(&rt, id).keyframes.opacity;
    assert_eq!((keys.len(), keys[1].at, keys[1].value), (2, 3.0, 0.3));
    assert_eq!(clip(&rt, id).opacity, 1.0);
    // Between them the field shows the value under the playhead.
    rt.state.editor.playhead = 4.0;
    rt.frame(FRAME);
    rt.frame(FRAME);
    assert_eq!(typed(&rt, Field::Opacity), "65%");
    // Ease in from the first key: an eighth of the way from 100% to 30%
    // halfway, where a straight line is halfway.
    reveal(&mut rt, "details-ease");
    click(&mut rt, "details-ease");
    assert_eq!(rt.state.details.select, Some(Select::Ease));
    click(&mut rt, &ease_key(Ease::EaseIn));
    assert_eq!(rt.state.details.select, None);
    assert_eq!(clip(&rt, id).keyframes.opacity[0].ease, Ease::EaseIn);
    rt.frame(FRAME);
    assert_eq!(typed(&rt, Field::Opacity), "91%");
    reveal(&mut rt, "details-previous-key");
    click(&mut rt, "details-previous-key");
    assert_eq!(rt.state.editor.playhead, 3.0);
    // On a key the diamond takes it away; one key has nothing to ease.
    reveal(&mut rt, "details-key-opacity");
    click(&mut rt, "details-key-opacity");
    assert_eq!(clip(&rt, id).keyframes.opacity.len(), 1);
    reveal(&mut rt, "details-ease");
    click(&mut rt, "details-ease");
    assert_eq!(rt.state.details.select, None, "an inert list stays shut");
}

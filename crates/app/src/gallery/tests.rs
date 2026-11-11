use eddie_core::editor::media::{AssetState, BinAsset};
use eddie_core::editor::sweep::{ClipReview, SweepCandidate, SweepData};
use rune::{Env, Key, KeyEvent, Mods, Runtime};

use super::grid::{BAR_RESERVE, MAX_CARD_W, grid};
use super::*;
use crate::app;

const FRAME: f64 = 1.0 / 60.0;

/// A range of `a.wav`. Sound only, so its card draws a waveform and
/// asks no decoder for a picture.
fn range(id: &str, category: &str, score: f32, source_in: f64) -> SweepCandidate {
    SweepCandidate {
        id: id.to_owned(),
        asset_path: "/media/a.wav".to_owned(),
        asset_name: "a.wav".to_owned(),
        title: String::new(),
        source_in,
        duration: 2.5,
        score,
        reason: "steady".to_owned(),
        stability: 0.0,
        audio_energy: 0.0,
        audio_peak: 0.0,
        category: category.to_owned(),
        tags: vec!["prime".to_owned()],
        motion_score: 0.0,
        audio_score: 0.0,
        music_bed: false,
        visual: None,
        has_video: false,
        transcript: Vec::new(),
        review: ClipReview::default(),
        dismissed: false,
    }
}

/// The Gallery, open on one curated file with these suggestions; the
/// file's scenes are the same ranges under `scene-` ids.
fn gallery(candidates: Vec<SweepCandidate>, size: (f32, f32)) -> Runtime<Ui> {
    let mut ui = Ui::headless();
    let scene_clips = candidates
        .iter()
        .map(|clip| SweepCandidate {
            id: format!("scene-{}", clip.id),
            ..clip.clone()
        })
        .collect();
    ui.editor.assets.push(BinAsset {
        info: openreel_media::MediaInfo {
            path: "/media/a.wav".to_owned(),
            name: "a.wav".to_owned(),
            duration: 600.0,
            width: 0,
            height: 0,
            fps: 0.0,
            has_video: false,
            has_audio: true,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks: Vec::new(),
        beats: None,
        thumbs: Vec::new(),
        sweep: SweepData {
            status: SweepStatus::Ready,
            candidates,
            scene_clips,
            ..SweepData::default()
        },
    });
    ui.editor.open_curator_gallery();
    let mut rt = Runtime::new(ui, app::view, size, Env::new(true, 1.0));
    rt.poll = Some(app::poll);
    rt.on_key = Some(app::on_key);
    // The grid sizes its cards from the box the first frames give it.
    for _ in 0..4 {
        rt.frame(FRAME);
    }
    rt
}

fn three() -> Vec<SweepCandidate> {
    vec![
        range("talk", "talking", 80.0, 10.0),
        range("act", "action", 60.0, 20.0),
        range("roll", "broll", 40.0, 30.0),
    ]
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

fn shown(rt: &Runtime<Ui>, id: &str) -> bool {
    rt.find(&format!("gallery-card:{id}")).is_some()
}

#[test]
fn a_row_of_cards_ends_where_the_grid_does() {
    // 1,256 px beside the scroll bar: six cards and five gaps, 4 px over.
    assert_eq!(grid(1256.0 + BAR_RESERVE, 2000.0), (6, 197.0, 4));
    // A card at its widest shares nothing.
    assert_eq!(grid(300.0 + BAR_RESERVE, 2000.0), (1, MAX_CARD_W, 0));
}

#[test]
fn a_filter_narrows_the_cards_and_reset_brings_them_back() {
    let mut rt = gallery(three(), (1400.0, 1200.0));
    assert!(shown(&rt, "talk") && shown(&rt, "act") && shown(&rt, "roll"));

    click(&mut rt, "gallery-category");
    assert_eq!(rt.state.gallery.popup, Some(Popup::Category));
    click(&mut rt, "gallery-option-action");
    assert_eq!(rt.state.gallery.popup, None, "choosing closes the list");
    assert!(shown(&rt, "act"));
    assert!(!shown(&rt, "talk") && !shown(&rt, "roll"));

    click(&mut rt, "gallery-reset");
    assert!(shown(&rt, "talk") && shown(&rt, "act") && shown(&rt, "roll"));
}

#[test]
fn search_filters_as_typed_and_reset_empties_the_box() {
    let mut rt = gallery(three(), (1400.0, 1200.0));
    click(&mut rt, "gallery-search");
    assert!(rt.state.typing(), "typing must not fire editor shortcuts");
    rt.paste("b-roll");
    rt.frame(FRAME);
    rt.frame(FRAME);
    assert_eq!(rt.state.editor.curator.filters.search, "b-roll");
    assert!(shown(&rt, "roll"));
    assert!(!shown(&rt, "talk") && !shown(&rt, "act"));

    // "No matching clips." offers the way back.
    rt.paste(" nothing");
    rt.frame(FRAME);
    rt.frame(FRAME);
    assert!(rt.find("gallery-empty").is_some());
    click(&mut rt, "gallery-reset-filters");
    assert_eq!(rt.state.gallery.search.text, "");
    assert!(shown(&rt, "talk") && shown(&rt, "act") && shown(&rt, "roll"));
}

#[test]
fn add_appends_the_exact_range_and_dismiss_keeps_its_scene() {
    let mut rt = gallery(three(), (1400.0, 1200.0));
    click(&mut rt, "gallery-add:act");
    let clips = &rt.state.editor.timeline.clips;
    assert_eq!(clips.len(), 1);
    assert_eq!((clips[0].source_in, clips[0].duration), (20.0, 2.5));
    assert!(shown(&rt, "act"), "adding a range does not use it up");

    click(&mut rt, "gallery-dismiss:act");
    assert!(!shown(&rt, "act"));
    assert!(shown(&rt, "talk") && shown(&rt, "roll"));

    // All scenes keeps the source whole and offers nothing to dismiss.
    click(&mut rt, "gallery-view-scenes");
    assert!(shown(&rt, "scene-act"));
    assert!(rt.find("gallery-add:scene-act").is_some());
    assert!(rt.find("gallery-dismiss:scene-act").is_none());
    click(&mut rt, "gallery-add-all");
    assert_eq!(rt.state.editor.timeline.clips.len(), 4);
}

#[test]
fn a_long_list_builds_only_the_cards_in_view() {
    let ranges = (0..400)
        .map(|index| range(&format!("r{index}"), "action", 50.0, f64::from(index)))
        .collect();
    let rt = gallery(ranges, (1400.0, 1200.0));
    // Equal scores keep source order.
    assert!(shown(&rt, "r0"));
    assert!(!shown(&rt, "r399"));
}

#[test]
fn a_narrow_gallery_sets_its_filters_from_one_menu() {
    let mut rt = gallery(three(), (1100.0, 900.0));
    assert!(rt.find("gallery-category").is_none());
    click(&mut rt, "gallery-filters");
    assert_eq!(rt.state.gallery.popup, Some(Popup::Filters));
    click(&mut rt, "gallery-category");
    click(&mut rt, "gallery-option-talking");
    assert_eq!(
        rt.state.gallery.popup,
        Some(Popup::Filters),
        "the menu comes back for the next filter"
    );
    assert_eq!(
        rt.state.editor.curator.filters.category.as_deref(),
        Some("talking")
    );
    assert!(shown(&rt, "talk") && !shown(&rt, "act"));
}

#[test]
fn escape_closes_an_open_list_without_changing_the_filter() {
    let mut rt = gallery(three(), (1400.0, 1200.0));
    click(&mut rt, "gallery-category");
    assert_eq!(rt.state.gallery.popup, Some(Popup::Category));
    assert!(rt.find("gallery-option-talking").is_some());
    rt.key(KeyEvent {
        key: Key::Escape,
        mods: Mods::default(),
    });
    rt.frame(FRAME);
    rt.frame(FRAME);
    assert_eq!(rt.state.gallery.popup, None);
    assert!(rt.find("gallery-option-talking").is_none());
    assert_eq!(rt.state.editor.curator.filters.category, None);
    assert!(shown(&rt, "talk") && shown(&rt, "act") && shown(&rt, "roll"));
}

#[test]
fn every_gallery_control_explains_itself() {
    let mut rt = gallery(three(), (1400.0, 1200.0));
    rt.help_delay = 0.0;
    for key in [
        "gallery-view-clips",
        "gallery-view-scenes",
        "gallery-source",
        "gallery-search",
        "gallery-sort",
        "gallery-category",
        "gallery-quality",
        "gallery-duration",
        "gallery-reset",
        "gallery-count",
        "gallery-mute",
        "gallery-add-all",
        "gallery-picture:talk",
        "gallery-add:talk",
        "gallery-dismiss:talk",
    ] {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("{key} is not shown"));
        // Rest the pointer on the control, coming from outside the window.
        rt.pointer_leave();
        rt.frame(FRAME);
        assert!(rt.help_tooltip().is_none());
        rt.pointer_move(x, y);
        rt.frame(FRAME);
        rt.frame(FRAME);
        let words = rt.help_tooltip().map(|(words, _)| words.to_owned());
        assert!(
            words.as_deref().is_some_and(|words| !words.is_empty()),
            "{key} has no tooltip"
        );
    }
    // An open list explains its choices too.
    click(&mut rt, "gallery-sort");
    let (x, y) = rt
        .find("gallery-option-source-order")
        .expect("the sort list is open");
    rt.pointer_move(x, y);
    rt.frame(FRAME);
    rt.frame(FRAME);
    assert_eq!(
        rt.help_tooltip()
            .map(|(words, _)| words.to_owned())
            .as_deref(),
        Some("Add shown uses this same order")
    );
}

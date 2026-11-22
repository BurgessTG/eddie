//! Recording a voiceover with the manual microphone, which hears a tone.

use std::time::Duration;

use super::*;
use crate::editor::voiceover::{take_start, COUNTDOWN};

/// A folder of the test's own, removed when it ends.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("eddie-vo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Frames at 30 a second for `seconds` of the editor's clock.
fn run(editor: &mut Editor, now: &mut f64, seconds: f64) {
    for _ in 0..(seconds * 30.0).round() as usize {
        *now += 1.0 / 30.0;
        editor.begin_frame(*now);
        editor.end_frame(false, false);
    }
}

/// Frames until `done`, giving workers (imports) real time.
fn until(editor: &mut Editor, now: &mut f64, what: &str, done: impl Fn(&Editor) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(editor) {
        assert!(Instant::now() < deadline, "{what}: {}", editor.status);
        run(editor, now, 1.0 / 30.0);
        thread::sleep(Duration::from_millis(10));
    }
}

/// An editor with a saved project in `dir` and the sample video on it.
fn project(dir: &Path) -> (Editor, f64) {
    let mut editor = Editor::headless();
    let mut now = 0.0;
    editor.spawn_import(sample_media().to_vec());
    until(&mut editor, &mut now, "the samples", |e| {
        e.import_rx.is_none() && e.assets.len() == 2
    });
    editor.save_project_to(&dir.join("talk")).unwrap();
    (editor, now)
}

#[test]
fn a_take_records_from_the_playhead_and_lands_on_a_free_audio_lane() {
    let dir = Folder::new("take");
    let (mut editor, mut now) = project(&dir.0);
    let video = sample_media()[0].to_string_lossy().into_owned();
    let music = sample_media()[1].to_string_lossy().into_owned();
    editor.add_asset_range_to_timeline(&video, "", None, 0.0, 0.0, 6.0);
    // Music on the first audio lane where the take will be.
    editor.add_asset_range_to_timeline(&music, "", None, 0.0, 0.0, 6.0);
    assert_eq!(editor.timeline.lanes.audio, 1);
    editor.seek(1.0);

    editor.perform(Action::RecordVoiceover);
    assert_eq!(editor.status, "Recording in 3 seconds.");
    assert_eq!(editor.voiceover().and_then(|v| v.countdown()), Some(3));
    run(&mut editor, &mut now, COUNTDOWN - 0.5);
    assert_eq!(editor.voiceover().and_then(|v| v.countdown()), Some(1));
    assert!(!editor.playing, "nothing plays during the countdown");
    run(&mut editor, &mut now, 0.6);
    let (start, take) = editor.voiceover().and_then(|v| v.recording()).unwrap();
    assert_eq!(start, 1.0);
    assert!(take.path().ends_with("talk voiceovers/Voiceover 1.wav"));
    assert!(editor.playing, "the timeline plays under the take");

    run(&mut editor, &mut now, 2.0);
    let (_, take) = editor.voiceover().and_then(|v| v.recording()).unwrap();
    assert!((take.seconds() - 2.0).abs() < 0.1, "{}", take.seconds());
    // Space stops the take and the timeline.
    editor.perform(Action::PlayPause);
    assert!(!editor.playing && !editor.voiceover_busy());
    let pending = &editor.pending_takes()[0];
    assert_eq!((pending.start, pending.source_in), (1.0, 0.0));
    let seconds = pending.seconds;
    assert!(
        pending.peaks.len() >= 90,
        "the waveform drawn until it lands"
    );

    until(&mut editor, &mut now, "the take", |e| {
        e.pending_takes().is_empty()
    });
    let clip = editor.timeline.clips.last().unwrap();
    assert_eq!(clip.name, "Voiceover 1");
    assert_eq!(clip.kind, ClipKind::Audio);
    assert_eq!((clip.start, clip.track), (1.0, 1), "beside the music");
    assert!((clip.duration - seconds).abs() < 0.01, "{}", clip.duration);
    assert!(!clip.duck, "a voiceover is what music ducks under");
    assert_eq!(editor.selected, Some(clip.id));
    let wav = dir.0.join("talk voiceovers").join("Voiceover 1.wav");
    assert_eq!(clip.source.as_deref(), Some(&*wav.to_string_lossy()));
    let asset = editor.asset(&wav.to_string_lossy()).unwrap();
    // The tone's peaks, as an import reads them.
    let loud = asset
        .peaks
        .iter()
        .filter(|p| (**p - 0.25).abs() < 0.02)
        .count();
    assert!(
        loud * 10 >= asset.peaks.len() * 9,
        "{:?}",
        &asset.peaks[..5]
    );

    // One undo takes the take off; the next take gets the next name.
    run(&mut editor, &mut now, 0.1);
    editor.perform(Action::Undo);
    assert!(editor
        .timeline
        .clips
        .iter()
        .all(|c| c.name != "Voiceover 1"));
    editor.perform(Action::RecordVoiceover);
    run(&mut editor, &mut now, COUNTDOWN + 0.5);
    let (_, take) = editor.voiceover().and_then(|v| v.recording()).unwrap();
    assert!(take.path().ends_with("Voiceover 2.wav"));
    editor.perform(Action::RecordVoiceover);
    assert!(!editor.voiceover_busy());
}

#[test]
fn a_take_goes_on_past_the_end_and_a_countdown_can_be_cancelled() {
    let dir = Folder::new("past");
    let (mut editor, mut now) = project(&dir.0);
    editor.perform(Action::RecordVoiceover);
    run(&mut editor, &mut now, 1.0);
    // Esc during the countdown records nothing.
    editor.perform(Action::Deselect);
    assert_eq!(editor.status, "Recording cancelled.");
    run(&mut editor, &mut now, COUNTDOWN);
    assert!(editor.voiceover().is_some_and(|v| v.recording().is_none()));
    assert!(!dir
        .0
        .join("talk voiceovers")
        .join("Voiceover 1.wav")
        .exists());

    // An empty timeline has nothing to play; the take still records.
    editor.perform(Action::RecordVoiceover);
    run(&mut editor, &mut now, COUNTDOWN + 1.5);
    assert!(!editor.playing);
    editor.perform(Action::Deselect);
    until(&mut editor, &mut now, "the take", |e| {
        e.pending_takes().is_empty()
    });
    let clip = editor.timeline.clips.last().expect("the take");
    assert_eq!((clip.start, clip.track), (0.0, 0));
    assert!((clip.duration - 1.5).abs() < 0.1, "{}", clip.duration);
    editor.close_microphone();
    assert!(editor.voiceover().is_none());
}

#[test]
fn a_take_lands_where_the_playhead_was_when_its_first_sample_was_heard() {
    let now = Instant::now();
    let anchor = Some((now, 5.0));
    let before = now - Duration::from_millis(30);
    let after = now + Duration::from_millis(30);
    assert!((take_start(4.0, Some(before), anchor) - 4.97).abs() < 1e-9);
    assert!((take_start(4.0, Some(after), anchor) - 5.03).abs() < 1e-9);
    // Without either reading the take starts where Record started it.
    assert_eq!(take_start(4.0, None, anchor), 4.0);
    assert_eq!(take_start(4.0, Some(before), None), 4.0);
}

#[test]
fn recording_needs_a_project_to_keep_takes_beside() {
    let mut editor = Editor::headless();
    editor.perform(Action::RecordVoiceover);
    assert_eq!(
        editor.status,
        "Save the project first: takes are kept beside it."
    );
    assert!(!editor.voiceover_busy());
}

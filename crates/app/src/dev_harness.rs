//! Dev-only test harness: Muse Code as the stand-in model (slice 5).
//!
//! This module is `#[cfg(test)]` only (see `main.rs`). Nothing here is
//! compiled into a normal or release build, with or without the
//! `dev-harness` feature. The feature only enables the extra tests that
//! shell out to `muse exec`; the offline tests below always run.
//!
//! What this validates: the prompt contract (developer contract +
//! cache-ordered `build_context_messages`), JavaScript validity in the real QuickJS console
//! (`scripting::ScriptEngine`), headless application of the resulting
//! `ScriptCommand`s to a real `Timeline`, and the one self-correction
//! retry on console rejection.
//!
//! Known hazard this harness mirrors on purpose: the QuickJS context
//! persists across evals (REPL variables survive), so chat JS that
//! redeclares a top-level `const`/`let` on the self-correction retry
//! dies with a redeclaration `SyntaxError`. See
//! `persistent_console_redeclaration_hazard`.
//!
//! What this does NOT validate: the API model's behavior. The live
//! driver runs whatever model this machine's Muse Code is
//! configured with. The shipped chat path runs Muse Spark / Muse
//! Wonder over the Meta Model API. Never treat a green harness as proof
//! the API route works.
//!
//! ## Final live-API verification (run against the real key before the demo)
//!
//! 1. Chat edit end-to-end: open the app, add the Meta Model API key in
//!    Chat > Settings (Spark, then repeat on Wonder), load the sample
//!    project, send "Trim the music under the video so it ends when the
//!    video ends", and confirm the timeline actually changes and the
//!    Debug drawer shows sane JavaScript.
//! 2. One transcription: import speech-bearing media with the key set
//!    and confirm the sweep returns transcript-backed candidates
//!    (`muse-voice-transcribe-1.0`), not just the local fallback.
//! 3. One sweep: run "Build the montage from the sweep" in chat (or Add
//!    all in the media bin) and confirm model-ranked candidates land on
//!    the timeline in ranked order.
//!
//! ## Running the harness
//!
//! ```sh
//! mise exec -- cargo test                                # offline scenarios
//! mise exec -- cargo test --features dev-harness         # compiles the live driver too
//! OPENREEL_LIVE_HARNESS=1 mise exec -- cargo test --features dev-harness dev_harness -- --nocapture
//! ```
//!
//! Live tests print `SKIP` and pass unless `OPENREEL_LIVE_HARNESS=1`,
//! so `cargo test` stays green without a working `muse` install.
//! Optional env: `OPENREEL_MUSE_BIN` (default `muse`),
//! `OPENREEL_MUSE_MODEL` (passed as `--model` when set).

use openreel_engine::{ClipKind, Timeline};

use crate::app::chat::{
    build_context_messages, parse_generated_text, render_messages_as_prompt, SYSTEM_PROMPT,
};
use crate::app::session::ChatSession;
use crate::scripting::{ScriptCommand, ScriptEngine};

const TRACK_TITLES: usize = 0;
const TRACK_VIDEO: usize = 1;
const TRACK_AUDIO: usize = 2;

const VIDEO_PATH: &str = "samples/sample_video.mp4";
const VIDEO_NAME: &str = "sample_video.mp4";
const MUSIC_PATH: &str = "samples/sample_music.mp3";
const MUSIC_NAME: &str = "sample_music.mp3";

#[derive(Clone)]
struct FakeAsset {
    name: String,
    path: String,
    duration: f64,
    width: u32,
    height: u32,
    has_video: bool,
    has_audio: bool,
}

#[derive(Clone)]
struct FakeCandidate {
    id: String,
    asset_path: String,
    asset_name: String,
    title: String,
    source_in: f64,
    duration: f64,
    score: f32,
    has_video: bool,
    audio_peak: f32,
    is_reaction: bool,
    reaction_score: f32,
}

#[derive(Clone)]
struct FakeTranscript {
    id: String,
    asset_path: String,
    asset_name: String,
    start: f64,
    end: f64,
    speaker: Option<String>,
    text: String,
}

struct Fixture {
    timeline: Timeline,
    assets: Vec<FakeAsset>,
    sweep: Vec<FakeCandidate>,
    transcripts: Vec<FakeTranscript>,
    playhead: f64,
    selected: Option<u64>,
}

impl Fixture {
    /// Mirrors `EditorApp::script_state_json` field-for-field.
    fn state_json(&self) -> String {
        let clips: Vec<serde_json::Value> = self
            .timeline
            .clips
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "name": c.name,
                    "kind": match c.kind {
                        ClipKind::Video => "video",
                        ClipKind::Audio => "audio",
                        ClipKind::Title => "title",
                    },
                    "track": c.track,
                    "start": c.start,
                    "duration": c.duration,
                    "sourceIn": c.source_in,
                    "source": c.source,
                    "opacity": c.opacity,
                    "volume": c.volume,
                })
            })
            .collect();
        let assets: Vec<serde_json::Value> = self
            .assets
            .iter()
            .map(|a| {
                serde_json::json!({
                    "name": a.name,
                    "path": a.path,
                    "duration": a.duration,
                    "width": a.width,
                    "height": a.height,
                    "hasVideo": a.has_video,
                    "hasAudio": a.has_audio,
                    "ready": true,
                })
            })
            .collect();
        let mut sweep: Vec<FakeCandidate> = self.sweep.clone();
        sweep.sort_by(|a, b| b.score.total_cmp(&a.score));
        let sweep_candidates: Vec<serde_json::Value> = sweep
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "asset": c.asset_path,
                    "assetName": c.asset_name,
                    "title": c.title,
                    "sourceIn": c.source_in,
                    "duration": c.duration,
                    "score": c.score,
                    "reason": "Steady, clean section.",
                    "stability": 0.9,
                    "audioEnergy": 0.5,
                    "audioPeak": c.audio_peak,
                    "isReaction": c.is_reaction,
                    "reactionScore": c.reaction_score,
                    "hasVideo": c.has_video,
                    "transcript": "",
                })
            })
            .collect();
        let transcript_segments: Vec<serde_json::Value> = self
            .transcripts
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "asset": t.asset_path,
                    "assetName": t.asset_name,
                    "start": t.start,
                    "end": t.end,
                    "speaker": t.speaker,
                    "text": t.text,
                })
            })
            .collect();
        serde_json::json!({
            "playhead": self.playhead,
            "playing": false,
            "selected": self.selected,
            "duration": self.timeline.end(),
            "fps": self.timeline.fps,
            "tool": "select",
            "magnet": true,
            "clips": clips,
            "assets": assets,
            "sweepCandidates": sweep_candidates,
            "transcriptSegments": transcript_segments,
        })
        .to_string()
    }
}

fn video_asset(name: &str, path: &str, duration: f64) -> FakeAsset {
    FakeAsset {
        name: name.to_owned(),
        path: path.to_owned(),
        duration,
        width: 1280,
        height: 720,
        has_video: true,
        has_audio: true,
    }
}

fn audio_asset(name: &str, path: &str, duration: f64) -> FakeAsset {
    FakeAsset {
        name: name.to_owned(),
        path: path.to_owned(),
        duration,
        width: 0,
        height: 0,
        has_video: false,
        has_audio: true,
    }
}

/// The arranged sample project (see `arrange_sample_project`): 20 s
/// sample video, 14.4 s sample music at 0.8, one title.
fn sample_project_fixture() -> Fixture {
    let mut timeline = Timeline::empty_project();
    let video = timeline.add_media_clip(
        VIDEO_NAME,
        ClipKind::Video,
        TRACK_VIDEO,
        0.0,
        20.0,
        0.0,
        VIDEO_PATH,
    );
    let music = timeline.add_media_clip(
        MUSIC_NAME,
        ClipKind::Audio,
        TRACK_AUDIO,
        0.0,
        14.4,
        0.0,
        MUSIC_PATH,
    );
    timeline.clip_mut(music).expect("music clip").volume = 0.8;
    timeline.add_clip(
        "Sample project — chop me",
        ClipKind::Title,
        TRACK_TITLES,
        1.0,
        4.0,
        0.0,
    );
    Fixture {
        timeline,
        assets: vec![
            video_asset(VIDEO_NAME, VIDEO_PATH, 20.0),
            audio_asset(MUSIC_NAME, MUSIC_PATH, 14.4),
        ],
        sweep: Vec::new(),
        transcripts: Vec::new(),
        playhead: 0.0,
        selected: Some(video),
    }
}

struct Scenario {
    name: &'static str,
    prompt: &'static str,
    fixture: fn() -> Fixture,
    /// Canned model replies, in call order. The last one repeats.
    canned: &'static [&'static str],
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "twenty_second_cut",
            prompt: "Cut this down to a tight 20-second cut and put the title 'Highlights' at the start.",
            fixture: || {
                let mut timeline = Timeline::empty_project();
                let video = timeline.add_media_clip(
                    "LongTake.mp4",
                    ClipKind::Video,
                    TRACK_VIDEO,
                    0.0,
                    64.0,
                    0.0,
                    "media/LongTake.mp4",
                );
                timeline.add_media_clip(
                    "Music_Bed.wav",
                    ClipKind::Audio,
                    TRACK_AUDIO,
                    0.0,
                    64.0,
                    0.0,
                    "media/Music_Bed.wav",
                );
                Fixture {
                    timeline,
                    assets: vec![
                        video_asset("LongTake.mp4", "media/LongTake.mp4", 64.0),
                        audio_asset("Music_Bed.wav", "media/Music_Bed.wav", 64.0),
                    ],
                    sweep: Vec::new(),
                    transcripts: Vec::new(),
                    playhead: 0.0,
                    selected: Some(video),
                }
            },
            canned: &[r#"
const s = state();
s.clips.filter(c => c.kind === 'video').forEach(c => trimClip(c.id, -1, 20));
s.clips.filter(c => c.kind === 'audio').forEach(c => trimClip(c.id, -1, 20));
addTitle('Highlights', 0, 4);
console.log("Done — trimmed to a 20-second cut with a title.");
"#],
        },
        Scenario {
            name: "trim_music_under_video",
            prompt: "Trim the music under the video so it ends when the video ends.",
            fixture: || {
                let mut f = sample_project_fixture();
                let music = f
                    .timeline
                    .clips
                    .iter()
                    .find(|c| c.kind == ClipKind::Audio)
                    .map(|c| c.id)
                    .expect("audio clip");
                f.timeline.trim_right(music, 32.0);
                f.assets[1] = audio_asset(MUSIC_NAME, MUSIC_PATH, 32.0);
                f
            },
            canned: &[r#"
const s = state();
const vend = Math.max(...s.clips.filter(c => c.kind === 'video').map(c => c.start + c.duration));
s.clips.filter(c => c.kind === 'audio').forEach(c => trimClip(c.id, -1, vend));
console.log("Done — music now ends with the video.");
"#],
        },
        Scenario {
            name: "sweep_assembly",
            prompt: "Build the montage from the sweep.",
            fixture: || {
                let mut timeline = Timeline::empty_project();
                timeline.add_media_clip(
                    "Old.mp4",
                    ClipKind::Video,
                    TRACK_VIDEO,
                    0.0,
                    6.0,
                    0.0,
                    "media/Old.mp4",
                );
                let cand = |n: usize, source_in: f64, duration: f64, score: f32| FakeCandidate {
                    id: format!("media/Rushes.mp4#{n}"),
                    asset_path: "media/Rushes.mp4".to_owned(),
                    asset_name: "Rushes.mp4".to_owned(),
                    title: format!("Rushes.mp4 · pick {n}"),
                    source_in,
                    duration,
                    score,
                    has_video: true,
                    audio_peak: 0.3,
                    is_reaction: false,
                    reaction_score: 0.2,
                };
                Fixture {
                    timeline,
                    assets: vec![
                        video_asset("Old.mp4", "media/Old.mp4", 6.0),
                        video_asset("Rushes.mp4", "media/Rushes.mp4", 120.0),
                    ],
                    sweep: vec![
                        cand(2, 40.0, 5.0, 84.0),
                        cand(1, 12.0, 4.0, 91.0),
                        cand(3, 70.0, 3.0, 77.0),
                    ],
                    transcripts: Vec::new(),
                    playhead: 0.0,
                    selected: None,
                }
            },
            canned: &[r#"
const cands = sweepCandidates().filter(c => c.hasVideo);
clips().forEach(c => deleteClip(c.id));
let cursor = 0;
cands.forEach(c => { addAssetRange(c.asset, cursor, c.sourceIn, c.duration); cursor += c.duration; });
console.log("Done — built the montage from the sweep.");
"#],
        },
        Scenario {
            name: "reaction_shots",
            prompt: "Build it from the reaction shots.",
            fixture: || {
                let timeline = Timeline::empty_project();
                let cand = |n: usize,
                            source_in: f64,
                            duration: f64,
                            score: f32,
                            reaction: bool|
                 -> FakeCandidate {
                    FakeCandidate {
                        id: format!("media/Rushes.mp4#{n}"),
                        asset_path: "media/Rushes.mp4".to_owned(),
                        asset_name: "Rushes.mp4".to_owned(),
                        title: format!("Rushes.mp4 · pick {n}"),
                        source_in,
                        duration,
                        score,
                        has_video: true,
                        audio_peak: if reaction { 0.92 } else { 0.25 },
                        is_reaction: reaction,
                        reaction_score: if reaction { 0.85 } else { 0.18 },
                    }
                };
                Fixture {
                    timeline,
                    assets: vec![video_asset("Rushes.mp4", "media/Rushes.mp4", 120.0)],
                    sweep: vec![
                        cand(1, 12.0, 4.0, 91.0, false),
                        cand(2, 40.0, 5.0, 84.0, true),
                        cand(3, 70.0, 3.0, 77.0, true),
                    ],
                    transcripts: Vec::new(),
                    playhead: 0.0,
                    selected: None,
                }
            },
            canned: &[r#"
var shots = sweepCandidates().filter(function (c) { return c.isReaction; });
clips().forEach(function (c) { deleteClip(c.id); });
var cursor = 0;
shots.forEach(function (c) { addAssetRange(c.asset, cursor, c.sourceIn, c.duration); cursor += c.duration; });
console.log("Done — built the cut from the reaction shots.");
"#],
        },
        Scenario {
            name: "pricing_transcript",
            prompt: "Use the part where he talks about pricing.",
            fixture: || {
                let timeline = Timeline::empty_project();
                Fixture {
                    timeline,
                    assets: vec![video_asset("Talk.mp4", "media/Talk.mp4", 180.0)],
                    sweep: Vec::new(),
                    transcripts: vec![
                        FakeTranscript {
                            id: "tx:media/Talk.mp4#0".to_owned(),
                            asset_path: "media/Talk.mp4".to_owned(),
                            asset_name: "Talk.mp4".to_owned(),
                            start: 5.0,
                            end: 14.0,
                            speaker: Some("A".to_owned()),
                            text: "Welcome, let me introduce the team.".to_owned(),
                        },
                        FakeTranscript {
                            id: "tx:media/Talk.mp4#1".to_owned(),
                            asset_path: "media/Talk.mp4".to_owned(),
                            asset_name: "Talk.mp4".to_owned(),
                            start: 42.0,
                            end: 51.0,
                            speaker: Some("A".to_owned()),
                            text: "Our pricing is simple: one flat monthly fee.".to_owned(),
                        },
                    ],
                    playhead: 0.0,
                    selected: None,
                }
            },
            canned: &[r#"
var seg = transcriptSegments().find(function (t) { return t.text.toLowerCase().indexOf('pricing') !== -1; });
if (!seg) throw new Error('no pricing segment');
addAssetRange(seg.asset, 0, seg.start, seg.end - seg.start);
console.log("Done — used the pricing section.");
"#],
        },
        Scenario {
            name: "impossible_request",
            prompt: "Add the clip 'CEO_Interview_Final_v9.mp4' at 5 seconds and upscale everything to 8K HDR.",
            fixture: sample_project_fixture,
            canned: &[
                r#"
const s = state();
if (!s.assets.some(a => a.name === 'CEO_Interview_Final_v9.mp4')) throw new Error('CEO_Interview_Final_v9.mp4 is not in the media bin');
addAsset('CEO_Interview_Final_v9.mp4', 5);
console.log("Done — added the interview clip.");
"#,
                // `var`, not `const`: the console context persists
                // across attempts, so redeclaring `const s` would
                // fail for the wrong reason (see hazard test below).
                r#"
var s = state();
if (!s.assets.some(a => a.name === 'CEO_Interview_Final_v9.mp4')) throw new Error('CEO_Interview_Final_v9.mp4 is not in the media bin');
addAsset('CEO_Interview_Final_v9.mp4', 5);
console.log("Done — added the interview clip.");
"#,
            ],
        },
        Scenario {
            name: "self_correction_retry",
            prompt: "Lower the music volume to 0.3.",
            fixture: sample_project_fixture,
            canned: &[
                "seek(3);\nsetVolum(musicId(), 0.3);",
                r#"
const m = state().clips.find(c => c.kind === 'audio');
setVolume(m.id, 0.3);
console.log("Done — music volume is now 0.3.");
"#,
            ],
        },
    ]
}

fn scenario(name: &str) -> Scenario {
    scenarios()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("unknown scenario {name}"))
}

/// The exact prompt text both drivers receive: the API path's
/// cache-ordered messages (developer contract first, timeline
/// last), flattened for the single-prompt stand-in driver.
fn full_prompt(request: &str, state_json: &str, correction: Option<(&str, &str)>) -> String {
    let session = ChatSession::new("dev-harness");
    let messages = build_context_messages(&session, &[], request, state_json, correction);
    render_messages_as_prompt(&messages)
}

trait JsSource {
    fn generate(
        &mut self,
        request: &str,
        state_json: &str,
        correction: Option<(&str, &str)>,
    ) -> Result<String, String>;
}

/// Offline stand-in: replays canned replies through the same
/// fence-stripping the API path applies to model output.
struct CannedSource {
    replies: Vec<String>,
    next: usize,
    prompts: Vec<String>,
}

impl CannedSource {
    fn new(replies: &[&str]) -> Self {
        assert!(!replies.is_empty(), "scenario needs a canned reply");
        Self {
            replies: replies.iter().map(|r| r.to_string()).collect(),
            next: 0,
            prompts: Vec::new(),
        }
    }
}

impl JsSource for CannedSource {
    fn generate(
        &mut self,
        request: &str,
        state_json: &str,
        correction: Option<(&str, &str)>,
    ) -> Result<String, String> {
        self.prompts
            .push(full_prompt(request, state_json, correction));
        let index = self.next.min(self.replies.len() - 1);
        self.next += 1;
        // Same parsing as the API path: structured envelope first,
        // tolerant free-text (what canned replies use) as fallback.
        parse_generated_text(&self.replies[index]).map(|edit| edit.code)
    }
}

/// Headless mirror of `EditorApp::apply_script_commands` for every
/// timeline-affecting command. Playback/UI commands only move the
/// harness playhead/selection. Import/export are plain errors here:
/// the harness never touches the filesystem.
struct HeadlessEditor {
    timeline: Timeline,
    assets: Vec<FakeAsset>,
    playhead: f64,
    selected: Option<u64>,
    playing: bool,
}

impl HeadlessEditor {
    fn from_fixture(fixture: &Fixture) -> Self {
        Self {
            timeline: fixture.timeline.clone(),
            assets: fixture.assets.clone(),
            playhead: fixture.playhead,
            selected: fixture.selected,
            playing: false,
        }
    }

    fn asset_by_path(&self, path: &str) -> Option<&FakeAsset> {
        self.assets.iter().find(|a| a.path == path)
    }

    fn asset_by_name_or_path(&self, key: &str) -> Option<&FakeAsset> {
        self.assets.iter().find(|a| a.path == key || a.name == key)
    }

    fn seek(&mut self, time: f64) {
        self.playhead = self
            .timeline
            .snap(time.clamp(0.0, self.timeline.end().max(0.0)));
    }

    fn add_asset_range(&mut self, asset: &FakeAsset, at: f64, source_in: f64, duration: f64) {
        let source_in = source_in.clamp(0.0, asset.duration.max(0.0));
        let duration = duration.min((asset.duration - source_in).max(0.0));
        if duration <= 0.0 {
            return;
        }
        let (kind, track) = if asset.has_video {
            (ClipKind::Video, TRACK_VIDEO)
        } else {
            (ClipKind::Audio, TRACK_AUDIO)
        };
        let id = self.timeline.add_media_clip(
            &asset.name,
            kind,
            track,
            at.max(0.0),
            duration,
            source_in,
            &asset.path,
        );
        self.selected = Some(id);
    }

    fn apply(&mut self, commands: Vec<ScriptCommand>) -> Vec<String> {
        let mut notes = Vec::new();
        for command in commands {
            match command {
                ScriptCommand::Log(msg) => notes.push(msg),
                ScriptCommand::Play => self.playing = true,
                ScriptCommand::Pause => self.playing = false,
                ScriptCommand::TogglePlay => self.playing = !self.playing,
                ScriptCommand::Seek(t) => self.seek(t),
                ScriptCommand::StepFrames(n) => {
                    let t = self.playhead + n as f64 / self.timeline.fps;
                    self.seek(t);
                }
                ScriptCommand::SelectClip(id) => {
                    if self.timeline.clip(id).is_some() {
                        self.selected = Some(id);
                    } else {
                        notes.push(format!("error: no clip {id}"));
                    }
                }
                ScriptCommand::SplitClip { clip, at } => match self.timeline.split_at(clip, at) {
                    Some(right) => self.selected = Some(right),
                    None => notes.push(format!("error: cannot split clip {clip} at {at}")),
                },
                ScriptCommand::DeleteClip(id) => {
                    self.timeline.remove_clip(id);
                    if self.selected == Some(id) {
                        self.selected = None;
                    }
                }
                ScriptCommand::MoveClip { clip, start } => self.timeline.move_clip(clip, start),
                ScriptCommand::TrimClip { clip, left, right } => {
                    if let Some(left) = left {
                        self.timeline.trim_left(clip, left);
                    }
                    if let Some(right) = right {
                        let mut end = right;
                        if let Some(c) = self.timeline.clip(clip).cloned() {
                            if let Some(asset) =
                                c.source.as_ref().and_then(|s| self.asset_by_path(s))
                            {
                                end = end.min(c.start + (asset.duration - c.source_in).max(0.0));
                            }
                        }
                        self.timeline.trim_right(clip, end);
                    }
                }
                ScriptCommand::RenameClip { clip, name } => {
                    if let Some(c) = self.timeline.clip_mut(clip) {
                        c.name = name;
                    }
                }
                ScriptCommand::SetOpacity { clip, value } => {
                    if let Some(c) = self.timeline.clip_mut(clip) {
                        c.opacity = value.clamp(0.0, 1.0);
                    }
                }
                ScriptCommand::SetVolume { clip, value } => {
                    if let Some(c) = self.timeline.clip_mut(clip) {
                        c.volume = value.clamp(0.0, 2.0);
                    }
                }
                ScriptCommand::AddTitle { text, at, duration } => {
                    let id = self.timeline.add_clip(
                        &text,
                        ClipKind::Title,
                        TRACK_TITLES,
                        at,
                        duration,
                        0.0,
                    );
                    self.selected = Some(id);
                }
                ScriptCommand::AddAsset { asset, at } => {
                    match self.asset_by_name_or_path(&asset).cloned() {
                        Some(found) => {
                            let duration = found.duration;
                            self.add_asset_range(&found, at, 0.0, duration);
                        }
                        None => notes.push(format!("error: no asset '{asset}'")),
                    }
                }
                ScriptCommand::AddAssetRange {
                    asset,
                    at,
                    source_in,
                    duration,
                } => match self.asset_by_name_or_path(&asset).cloned() {
                    Some(found) => self.add_asset_range(&found, at, source_in, duration),
                    None => notes.push(format!("error: no asset '{asset}'")),
                },
                ScriptCommand::Import(path) => notes.push(format!(
                    "error: import is not available in the harness ({path})"
                )),
                ScriptCommand::LoadSample => {
                    notes.push("error: loadSample is not available in the harness".to_owned())
                }
                ScriptCommand::Export(path) => notes.push(format!(
                    "error: export is not available in the harness ({path})"
                )),
                ScriptCommand::ClearTimeline => {
                    self.playing = false;
                    self.timeline = Timeline::empty_project();
                    self.selected = None;
                    self.playhead = 0.0;
                }
                ScriptCommand::SetTool(tool) => match tool.as_str() {
                    "select" | "split" => {}
                    other => notes.push(format!("error: unknown tool '{other}'")),
                },
                ScriptCommand::SetMagnet(_) | ScriptCommand::SetZoom(_) => {}
                ScriptCommand::CloseGaps => self.timeline.close_gaps(TRACK_VIDEO),
            }
        }
        notes
    }
}

struct Attempt {
    js: String,
    result: String,
}

struct RunReport {
    scenario: String,
    prompt: String,
    attempts: Vec<Attempt>,
    outcome: String,
}

impl RunReport {
    /// Same information as the chat Debug drawer (the raw generated
    /// JavaScript), plus the console verdict per attempt.
    fn render(&self) -> String {
        let mut out = format!(
            "=== dev-harness scenario: {} ===\nprompt: {}\n",
            self.scenario, self.prompt
        );
        for (i, attempt) in self.attempts.iter().enumerate() {
            out.push_str(&format!(
                "--- attempt {}: generated JavaScript (Debug drawer) ---\n{}\n--- attempt {} result: {} ---\n",
                i + 1,
                attempt.js,
                i + 1,
                attempt.result
            ));
        }
        out.push_str(&format!("--- outcome: {} ---\n", self.outcome));
        out
    }
}

/// Run one scenario end to end. Mirrors the chat loop: evaluate with
/// `eval_checked` (a rejected script makes no edit), retry exactly
/// once with the failed JS and console error, then give up plainly.
/// Application is transactional in the harness: commands run against
/// a clone and any `error:` note discards the whole edit, so a failed
/// scenario can never leave a half-edited timeline behind.
fn run_scenario(
    source: &mut dyn JsSource,
    scenario: &Scenario,
    fixture: &Fixture,
) -> (Result<HeadlessEditor, String>, RunReport) {
    let state_json = fixture.state_json();
    let mut report = RunReport {
        scenario: scenario.name.to_owned(),
        prompt: scenario.prompt.to_owned(),
        attempts: Vec::new(),
        outcome: String::new(),
    };
    let mut engine = match ScriptEngine::new() {
        Ok(engine) => engine,
        Err(error) => {
            report.outcome = format!("FAIL scripting unavailable: {error}");
            return (Err(error), report);
        }
    };
    let mut correction: Option<(String, String)> = None;
    for attempt in 0..2 {
        let correction_ref = correction
            .as_ref()
            .map(|(js, err)| (js.as_str(), err.as_str()));
        let js = match source.generate(scenario.prompt, &state_json, correction_ref) {
            Ok(js) => js,
            Err(error) => {
                report.outcome = format!("FAIL driver error: {error}");
                return (Err(error), report);
            }
        };
        match engine.eval_checked(&js, &state_json) {
            Ok((_, commands)) => {
                let command_count = commands.len();
                let mut editor = HeadlessEditor::from_fixture(fixture);
                let notes = editor.apply(commands);
                let errors: Vec<String> = notes
                    .iter()
                    .filter(|n| n.starts_with("error:"))
                    .cloned()
                    .collect();
                if errors.is_empty() {
                    report.attempts.push(Attempt {
                        js,
                        result: format!("applied {command_count} command(s); notes: {notes:?}"),
                    });
                    report.outcome = format!(
                        "PASS clips={} end={:.3}s",
                        editor.timeline.clips.len(),
                        editor.timeline.end()
                    );
                    return (Ok(editor), report);
                }
                report.attempts.push(Attempt {
                    js,
                    result: format!("rejected at apply: {errors:?}"),
                });
                report.outcome = format!("FAIL apply errors: {errors:?}");
                return (Err(errors.join("; ")), report);
            }
            Err(error) => {
                report.attempts.push(Attempt {
                    js: js.clone(),
                    result: format!("rejected by console: {error}"),
                });
                if attempt == 0 {
                    correction = Some((js, error));
                } else {
                    report.outcome = format!("FAIL console rejected twice: {error}");
                    return (Err(error), report);
                }
            }
        }
    }
    report.outcome = "FAIL exhausted attempts".to_owned();
    (Err("exhausted attempts".to_owned()), report)
}

/// Final assistant text from `muse exec --json` JSONL. The terminal
/// event (`run.terminal.completed`, `payload.text`) wins; streamed
/// `run.output.delta` texts are the fallback. Non-JSON lines and
/// unrelated events are ignored.
fn extract_final_text(jsonl: &str) -> Result<String, String> {
    let mut terminal: Option<String> = None;
    let mut terminal_error: Option<String> = None;
    let mut deltas: Vec<String> = Vec::new();
    for line in jsonl.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let payload = &value["payload"];
        match value["payload_type"].as_str() {
            Some("run.terminal.completed") => {
                if let Some(text) = payload["text"].as_str() {
                    terminal = Some(text.to_owned());
                }
            }
            Some(kind) if kind.starts_with("run.terminal.") => {
                let reason = payload["reason"]
                    .as_str()
                    .or_else(|| payload["text"].as_str())
                    .unwrap_or(kind)
                    .to_owned();
                terminal_error = Some(reason);
            }
            Some("run.output.delta") => {
                if let Some(text) = payload["text"].as_str() {
                    deltas.push(text.to_owned());
                }
            }
            _ => {}
        }
    }
    if let Some(text) = terminal {
        if text.trim().is_empty() {
            return Err("muse exec finished with empty final text".to_owned());
        }
        return Ok(text);
    }
    if let Some(error) = terminal_error {
        return Err(format!("muse exec run failed: {error}"));
    }
    let joined = deltas.join("");
    if joined.trim().is_empty() {
        Err("muse exec produced no final text".to_owned())
    } else {
        Ok(joined)
    }
}

fn extract_js_from_jsonl(jsonl: &str) -> Result<String, String> {
    parse_generated_text(&extract_final_text(jsonl)?).map(|edit| edit.code)
}

/// Stable timeline fingerprint for "unchanged" assertions.
fn fingerprint(timeline: &Timeline) -> Vec<String> {
    let mut rows: Vec<String> = timeline
        .clips
        .iter()
        .map(|c| {
            format!(
                "{}|{}|{:?}|{}|{:.4}|{:.4}|{:.4}|{}|{:.3}|{:.3}",
                c.id,
                c.name,
                c.kind,
                c.track,
                c.start,
                c.duration,
                c.source_in,
                c.source.as_deref().unwrap_or(""),
                c.opacity,
                c.volume
            )
        })
        .collect();
    rows.sort();
    rows
}

fn clips_of_kind(timeline: &Timeline, kind: ClipKind) -> Vec<&openreel_engine::Clip> {
    let mut clips: Vec<&openreel_engine::Clip> =
        timeline.clips.iter().filter(|c| c.kind == kind).collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    clips
}

fn frame_tol(timeline: &Timeline) -> f64 {
    2.0 / timeline.fps + 1e-9
}

/// Scenario expectations. Returns human-readable failures (empty = pass).
/// Shape-based on purpose: clip counts, order, durations, titles.
fn check_scenario(
    scenario: &Scenario,
    before: &Fixture,
    result: &Result<HeadlessEditor, String>,
    attempts: usize,
) -> Vec<String> {
    let mut failures = Vec::new();
    let mut expect = |ok: bool, message: String| {
        if !ok {
            failures.push(message);
        }
    };
    match scenario.name {
        "impossible_request" => {
            expect(
                result.is_err(),
                "impossible request must end in a plain failure".to_owned(),
            );
            expect(
                attempts == 2,
                format!("impossible request should exhaust the one retry (attempts={attempts})"),
            );
            if let Ok(editor) = result {
                expect(
                    fingerprint(&editor.timeline) == fingerprint(&before.timeline),
                    "impossible request must not change the timeline".to_owned(),
                );
            }
        }
        "twenty_second_cut" => {
            let Ok(editor) = result else {
                return vec!["20-second cut run failed".to_owned()];
            };
            let tl = &editor.timeline;
            let video = clips_of_kind(tl, ClipKind::Video);
            let video_end = video.iter().map(|c| c.end()).fold(0.0, f64::max);
            expect(!video.is_empty(), "expected video clips".to_owned());
            expect(
                (video_end - 20.0).abs() <= 1.0,
                format!("video should end at ~20s, ends at {video_end:.3}s"),
            );
            expect(
                clips_of_kind(tl, ClipKind::Title)
                    .iter()
                    .any(|c| c.name.contains("Highlights")),
                "expected a 'Highlights' title clip".to_owned(),
            );
        }
        "trim_music_under_video" => {
            let Ok(editor) = result else {
                return vec!["trim-music run failed".to_owned()];
            };
            let tl = &editor.timeline;
            let tol = frame_tol(tl);
            let video_end = clips_of_kind(tl, ClipKind::Video)
                .iter()
                .map(|c| c.end())
                .fold(0.0, f64::max);
            let audio = clips_of_kind(tl, ClipKind::Audio);
            expect(
                tl.clips.len() == before.timeline.clips.len(),
                format!(
                    "clip count should stay {}, got {}",
                    before.timeline.clips.len(),
                    tl.clips.len()
                ),
            );
            expect(audio.len() == 1, "expected one audio clip".to_owned());
            if let Some(music) = audio.first() {
                expect(
                    (music.end() - video_end).abs() <= tol,
                    format!(
                        "music end {:.3}s should match video end {:.3}s",
                        music.end(),
                        video_end
                    ),
                );
            }
        }
        "sweep_assembly" => {
            let Ok(editor) = result else {
                return vec!["sweep assembly run failed".to_owned()];
            };
            let tl = &editor.timeline;
            let tol = frame_tol(tl);
            let video = clips_of_kind(tl, ClipKind::Video);
            let source_ins: Vec<f64> = video.iter().map(|c| c.source_in).collect();
            let starts: Vec<f64> = video.iter().map(|c| c.start).collect();
            expect(
                video.len() == 3,
                format!("expected 3 sweep clips, got {}", video.len()),
            );
            expect(
                source_ins == vec![12.0, 40.0, 70.0],
                format!("clips must be in ranked order (sourceIns {source_ins:?})"),
            );
            expect(
                starts == vec![0.0, 4.0, 9.0],
                format!("clips must be sequential from 0 (starts {starts:?})"),
            );
            expect(
                (tl.end() - 12.0).abs() <= tol,
                format!("montage should total 12s, got {:.3}s", tl.end()),
            );
            expect(
                tl.clips.iter().all(|c| c.name != "Old.mp4"),
                "previous timeline clips must be replaced".to_owned(),
            );
        }
        "reaction_shots" => {
            let Ok(editor) = result else {
                return vec!["reaction-shots run failed".to_owned()];
            };
            let video = clips_of_kind(&editor.timeline, ClipKind::Video);
            let source_ins: Vec<f64> = video.iter().map(|c| c.source_in).collect();
            expect(
                source_ins == vec![40.0, 70.0],
                format!("only reaction candidates, in ranked order (got {source_ins:?})"),
            );
            expect(
                video.iter().map(|c| c.start).collect::<Vec<_>>() == vec![0.0, 5.0],
                "reaction clips must be sequential from 0".to_owned(),
            );
        }
        "pricing_transcript" => {
            let Ok(editor) = result else {
                return vec!["pricing-transcript run failed".to_owned()];
            };
            let video = clips_of_kind(&editor.timeline, ClipKind::Video);
            expect(
                video.len() == 1,
                format!("expected 1 clip, got {}", video.len()),
            );
            if let Some(clip) = video.first() {
                expect(
                    (clip.source_in - 42.0).abs() < 1e-9
                        && (clip.duration - 9.0).abs() <= frame_tol(&editor.timeline),
                    format!(
                        "clip must cover the pricing turn 42–51 (got in {} dur {})",
                        clip.source_in, clip.duration
                    ),
                );
                expect(
                    clip.source.as_deref() == Some("media/Talk.mp4"),
                    "clip must come from the transcript segment's asset".to_owned(),
                );
            }
        }
        "self_correction_retry" => {
            let Ok(editor) = result else {
                return vec!["retry run failed".to_owned()];
            };
            expect(
                attempts == 2,
                format!("expected exactly one retry (attempts={attempts})"),
            );
            expect(
                (editor.playhead - before.playhead).abs() < 1e-9,
                "rejected first script must make no edit (seek leaked)".to_owned(),
            );
            let audio = clips_of_kind(&editor.timeline, ClipKind::Audio);
            if let Some(music) = audio.first() {
                expect(
                    (music.volume - 0.3).abs() < 1e-6,
                    format!("music volume should be 0.3, got {}", music.volume),
                );
            } else {
                failures.push("expected an audio clip".to_owned());
            }
        }
        other => failures.push(format!("no expectations for scenario {other}")),
    }
    failures
}

#[cfg(feature = "dev-harness")]
mod live_driver {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::{extract_js_from_jsonl, full_prompt, JsSource};

    const TIMEOUT: Duration = Duration::from_secs(240);

    /// Real driver: `muse exec --json` as the stand-in model. Tools
    /// are disabled so the run can only answer with text; the prompt
    /// itself is byte-identical to the API path's contract.
    pub(super) struct MuseExecSource;

    impl JsSource for MuseExecSource {
        fn generate(
            &mut self,
            request: &str,
            state_json: &str,
            correction: Option<(&str, &str)>,
        ) -> Result<String, String> {
            let prompt = full_prompt(request, state_json, correction);
            let stamp = format!(
                "openreel-harness-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            );
            let dir = std::env::temp_dir();
            let prompt_path = dir.join(format!("{stamp}-prompt.txt"));
            let stdout_path = dir.join(format!("{stamp}-stdout.jsonl"));
            let stderr_path = dir.join(format!("{stamp}-stderr.txt"));
            let result = run_muse(&prompt, &prompt_path, &stdout_path, &stderr_path);
            let _ = std::fs::remove_file(&prompt_path);
            let _ = std::fs::remove_file(&stdout_path);
            let _ = std::fs::remove_file(&stderr_path);
            result
        }
    }

    fn run_muse(
        prompt: &str,
        prompt_path: &std::path::Path,
        stdout_path: &std::path::Path,
        stderr_path: &std::path::Path,
    ) -> Result<String, String> {
        std::fs::write(prompt_path, prompt)
            .map_err(|e| format!("could not write harness prompt file: {e}"))?;
        let stdout_file = std::fs::File::create(stdout_path)
            .map_err(|e| format!("could not create harness output file: {e}"))?;
        let stderr_file = std::fs::File::create(stderr_path)
            .map_err(|e| format!("could not create harness output file: {e}"))?;
        let bin = std::env::var("OPENREEL_MUSE_BIN").unwrap_or_else(|_| "muse".to_owned());
        let mut command = Command::new(&bin);
        command
            .arg("exec")
            .arg("--json")
            .arg("--prompt-file")
            .arg(prompt_path)
            .arg("--no-session-log")
            .arg("--disable-shell")
            .arg("--disable-write")
            .arg("--disable-web-tools")
            .arg("--disable-reminders")
            .arg("--disable-request-user-input")
            .arg("--max-model-steps")
            .arg("4")
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));
        if let Ok(model) = std::env::var("OPENREEL_MUSE_MODEL") {
            if !model.trim().is_empty() {
                command.arg("--model").arg(model.trim());
            }
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("could not start `{bin} exec`: {e}"))?;
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(format!(
                            "`{bin} exec` timed out after {}s",
                            TIMEOUT.as_secs()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => return Err(format!("`{bin} exec` wait failed: {error}")),
            }
        };
        let stdout = std::fs::read_to_string(stdout_path).unwrap_or_default();
        let stderr = std::fs::read_to_string(stderr_path).unwrap_or_default();
        if !status.success() {
            let tail: String = stderr.chars().rev().take(800).collect::<String>();
            let tail: String = tail.chars().rev().collect();
            return Err(format!(
                "`{bin} exec` exited with {status}: {}",
                tail.trim()
            ));
        }
        extract_js_from_jsonl(&stdout)
    }
}

/// Run a scenario and assert, with the full report in every failure.
fn assert_scenario(source: &mut dyn JsSource, scenario: &Scenario) {
    let fixture = (scenario.fixture)();
    let (result, report) = run_scenario(source, scenario, &fixture);
    eprintln!("{}", report.render());
    let failures = check_scenario(scenario, &fixture, &result, report.attempts.len());
    assert!(
        failures.is_empty(),
        "scenario {} failed: {:?}\n{}",
        scenario.name,
        failures,
        report.render()
    );
}

fn assert_canned(name: &str) {
    let scenario = scenario(name);
    let mut source = CannedSource::new(scenario.canned);
    assert_scenario(&mut source, &scenario);
}

// -- offline scenarios (always run; canned JS, real console) -------------

#[test]
fn offline_twenty_second_cut() {
    assert_canned("twenty_second_cut");
}

#[test]
fn offline_trim_music_under_video() {
    assert_canned("trim_music_under_video");
}

#[test]
fn offline_sweep_assembly() {
    assert_canned("sweep_assembly");
}

#[test]
fn offline_reaction_shots_use_sweep_ids() {
    assert_canned("reaction_shots");
}

#[test]
fn offline_pricing_resolves_against_transcript_segments() {
    assert_canned("pricing_transcript");
}

#[test]
fn local_intent_builds_timeline_offline_through_script_commands() {
    // The "Reactions first" button with the network unplugged: no
    // JsSource, no model — ordered candidates become a
    // ScriptCommand batch applied like any chat edit.
    use crate::app::intents::{local_commands, ordered_candidates, CandidateView, LocalSort};
    let fixture = scenario("reaction_shots").fixture;
    let fixture = fixture();
    let views: Vec<CandidateView> = fixture
        .sweep
        .iter()
        .map(|c| CandidateView {
            id: c.id.clone(),
            asset_path: c.asset_path.clone(),
            title: c.title.clone(),
            source_in: c.source_in,
            duration: c.duration,
            score: c.score,
            stability: 0.5,
            reaction_score: c.reaction_score,
            is_reaction: c.is_reaction,
            has_video: c.has_video,
        })
        .collect();
    let ordered = ordered_candidates(&views, LocalSort::ReactionsFirst);
    assert_eq!(ordered[0].source_in, 40.0, "reaction lands first");
    let mut editor = HeadlessEditor::from_fixture(&fixture);
    let notes = editor.apply(local_commands(&[], &ordered));
    assert!(notes.iter().all(|n| !n.starts_with("error:")), "{notes:?}");
    let video = clips_of_kind(&editor.timeline, ClipKind::Video);
    let source_ins: Vec<f64> = video.iter().map(|c| c.source_in).collect();
    assert_eq!(source_ins, vec![40.0, 70.0, 12.0]);
    assert_eq!(
        video.iter().map(|c| c.start).collect::<Vec<_>>(),
        vec![0.0, 5.0, 8.0]
    );
}

#[test]
fn offline_impossible_request_fails_without_corruption() {
    assert_canned("impossible_request");
}

#[test]
fn offline_self_correction_retry() {
    let scenario = scenario("self_correction_retry");
    let mut source = CannedSource::new(scenario.canned);
    assert_scenario(&mut source, &scenario);
    // The retry prompt must carry the failed JS and the console error,
    // exactly like the API path's correction round-trip.
    assert_eq!(source.prompts.len(), 2);
    assert!(source.prompts[1].contains("setVolum(musicId(), 0.3)"));
    assert!(source.prompts[1].contains("ReferenceError"));
    assert!(source.prompts[1]
        .contains("Reply with corrected JavaScript only, in the same JSON envelope."));
}

#[test]
fn persistent_console_redeclaration_hazard() {
    // Known contract hazard, faithfully mirrored from the app: the
    // QuickJS context persists across evals, so a retry that redeclares
    // a top-level `const` from the failed attempt dies with a
    // redeclaration SyntaxError before doing anything. Model JS should
    // prefer `var` or block scope until the chat path isolates evals.
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let state = sample_project_fixture().state_json();
    let first = engine.eval_checked("const s = state(); bogusCall();", &state);
    assert!(first.is_err());
    let second = engine.eval_checked("const s = state(); seek(1);", &state);
    let error = second.expect_err("redeclaration must reject the retry");
    assert!(error.contains("redeclaration"), "{error}");
}

#[test]
fn offline_apply_error_is_plain_failure() {
    // A script that evaluates cleanly but names a missing asset must
    // fail plainly and leave no partial edit behind.
    let scenario = scenario("impossible_request");
    let fixture = (scenario.fixture)();
    let before = fingerprint(&fixture.timeline);
    let mut source = CannedSource::new(&[
        "addAsset('CEO_Interview_Final_v9.mp4', 5);\nconsole.log(\"Done — added it.\");",
    ]);
    let (result, report) = run_scenario(&mut source, &scenario, &fixture);
    eprintln!("{}", report.render());
    assert!(result.is_err(), "{}", report.render());
    assert_eq!(fingerprint(&fixture.timeline), before);
    assert_eq!(report.attempts.len(), 1, "{}", report.render());
}

// -- contract: prompt, state shape, JSONL extraction --------------------

#[test]
fn prompt_contract_matches_api_path() {
    let fixture = sample_project_fixture();
    let state = fixture.state_json();
    let prompt = full_prompt("tighten it", &state, None);
    assert!(
        prompt.starts_with(&format!("developer: {SYSTEM_PROMPT}")),
        "{prompt}"
    );
    assert!(
        prompt.contains(r#"Reply with a single JSON object {"code":"#),
        "{prompt}"
    );
    assert!(
        prompt.contains("Build it from the reaction shots."),
        "few-shot present"
    );
    assert!(prompt.contains(&format!("Timeline state JSON:\n{state}")));
    assert!(prompt.contains("Request:\ntighten it"));
    let request_at = prompt.rfind("Request:\ntighten it").unwrap();
    let timeline_at = prompt.rfind("Timeline state JSON:").unwrap();
    assert!(
        request_at < timeline_at,
        "timeline JSON is the volatile tail"
    );

    let retry = full_prompt("tighten it", &state, Some(("bad()", "ReferenceError: bad")));
    assert!(retry.contains("Your previous JavaScript failed in the sandbox"));
    assert!(retry.contains("Previous JavaScript:\nbad()"));
    assert!(retry.contains("Reply with corrected JavaScript only, in the same JSON envelope."));
}

#[test]
fn fixture_state_matches_console_shape() {
    let fixture = scenario("sweep_assembly").fixture;
    let fixture = fixture();
    let value: serde_json::Value =
        serde_json::from_str(&fixture.state_json()).expect("state JSON parses");
    for key in [
        "playhead",
        "playing",
        "selected",
        "duration",
        "fps",
        "tool",
        "magnet",
        "clips",
        "assets",
        "sweepCandidates",
        "transcriptSegments",
    ] {
        assert!(value.get(key).is_some(), "missing state key {key}");
    }
    let scores: Vec<f64> = value["sweepCandidates"]
        .as_array()
        .expect("candidates array")
        .iter()
        .map(|c| c["score"].as_f64().expect("score"))
        .collect();
    assert_eq!(scores, vec![91.0, 84.0, 77.0], "candidates ranked desc");
    let clip = &value["clips"][0];
    for key in [
        "id", "name", "kind", "track", "start", "duration", "sourceIn", "source", "opacity",
        "volume",
    ] {
        assert!(clip.get(key).is_some(), "missing clip key {key}");
    }
    assert_eq!(clip["kind"], "video");
}

#[test]
fn jsonl_extractor_prefers_terminal_text_and_strips_fences() {
    let jsonl = concat!(
        "{\"payload_type\":\"turn.input.user\",\"payload\":{\"prompt\":\"x\"}}\n",
        "not json at all\n",
        "{\"payload_type\":\"run.output.delta\",\"payload\":{\"text\":\"partial \"}}\n",
        "{\"payload_type\":\"run.terminal.completed\",\"payload\":{\"kind\":\"run_terminal\",\"terminal\":\"completed\",\"text\":\"```javascript\\nseek(2);\\nconsole.log(\\\"Done — seeked.\\\");\\n```\"}}\n",
    );
    assert_eq!(
        extract_js_from_jsonl(jsonl).expect("extract JS"),
        "seek(2);\nconsole.log(\"Done — seeked.\");"
    );
}

#[test]
fn jsonl_extractor_falls_back_to_deltas() {
    let jsonl = concat!(
        "{\"payload_type\":\"run.output.delta\",\"payload\":{\"text\":\"seek(\"}}\n",
        "{\"payload_type\":\"run.output.delta\",\"payload\":{\"text\":\"1)\"}}\n",
    );
    assert_eq!(extract_js_from_jsonl(jsonl).expect("extract JS"), "seek(1)");
}

#[test]
fn jsonl_extractor_errors_without_final_text() {
    assert!(extract_js_from_jsonl("{\"payload_type\":\"run.usage\",\"payload\":{}}").is_err());
    assert!(extract_js_from_jsonl("").is_err());
}

// -- live scenarios (feature `dev-harness` + OPENREEL_LIVE_HARNESS=1) ---

#[cfg(feature = "dev-harness")]
fn assert_live(name: &str) {
    if std::env::var("OPENREEL_LIVE_HARNESS").as_deref() != Ok("1") {
        eprintln!("SKIP live scenario {name}: set OPENREEL_LIVE_HARNESS=1 to drive `muse exec`");
        return;
    }
    let scenario = scenario(name);
    let mut source = live_driver::MuseExecSource;
    assert_scenario(&mut source, &scenario);
}

#[cfg(feature = "dev-harness")]
#[test]
fn live_twenty_second_cut() {
    assert_live("twenty_second_cut");
}

#[cfg(feature = "dev-harness")]
#[test]
fn live_trim_music_under_video() {
    assert_live("trim_music_under_video");
}

#[cfg(feature = "dev-harness")]
#[test]
fn live_sweep_assembly() {
    assert_live("sweep_assembly");
}

#[cfg(feature = "dev-harness")]
#[test]
fn live_impossible_request() {
    assert_live("impossible_request");
}

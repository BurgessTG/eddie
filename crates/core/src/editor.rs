//! The editor without a window: the timeline, the media bin, playback,
//! scripting, curation, projects and export. A front end draws it, feeds it
//! input and time, and owns the pixels; the command line drives it bare.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use openreel_engine::{ClipId, ClipKind, Orientation, Timeline, TitleStyle};
use openreel_media::{
    AudioEngine, AudioSegment, FrameStream, Loudness, MediaInfo, RawFrame, StillDecoder,
};

use crate::scripting::{ScriptCommand, ScriptEngine};

pub mod commands;
pub mod details;
pub mod gallery;
mod headless;
pub use headless::cli_main;
pub mod history;
pub mod inspect;
pub mod lanes;
pub mod media;
pub mod pictures;
pub mod project;
#[cfg(unix)]
pub mod server;
#[cfg(not(unix))]
#[path = "editor/server_stub.rs"]
pub mod server;
pub mod settings;
pub mod sweep;
pub mod viewport;
use gallery::CuratorGallery;
use media::{AssetState, BinAsset, ImportMsg};
use settings::Settings;
use sweep::SweepMsg;
use viewport::TimelineViewport;

pub const PREVIEW_W: u32 = 960;
pub const SCRUB_W: u32 = 480;
const STILL_CACHE_CAP: usize = 24;
/// How long a still that failed to decode waits before it is asked for again.
const STILL_RETRY: Duration = Duration::from_secs(2);
const SHUTTLE_MAX: f64 = 8.0;
pub const MEDIA_EXTS: &[&str] = &[
    "mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "flac", "m4a", "aac", "ogg",
];

// Clip identity, source, and exact timeline/source starts. Editing a clip's
// source mapping invalidates playback even when its ID has not changed.
type VideoKey = (ClipId, String, u64, u64);
/// A still picture: clip, source frame index, and decoded width.
pub type StillKey = (ClipId, i64, u32);

/// What a still request decodes: its key, source path and size, and time.
pub struct StillTarget {
    pub key: StillKey,
    source: String,
    size: (u32, u32),
    time: f64,
}

enum ExportMsg {
    Progress(f64),
    Done(Result<PathBuf, String>),
}

/// Asks the front end for another frame. Worker threads call it when their
/// result is ready, and playback calls it to keep the clock moving.
#[derive(Clone)]
pub struct Wake(Arc<dyn Fn() + Send + Sync>);

impl Wake {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(wake))
    }

    /// For an editor nothing draws: the caller polls on its own schedule.
    pub fn none() -> Self {
        Self::new(|| {})
    }

    pub fn wake(&self) {
        (self.0)()
    }
}

/// Where the program monitor's picture goes. The editor picks the frame;
/// the front end owns the pixels it is copied into.
pub trait PreviewSink {
    fn show(&mut self, frame: &RawFrame);
    fn clear(&mut self);
}

/// The monitor of an editor nothing draws.
pub struct NoPreview;

impl PreviewSink for NoPreview {
    fn show(&mut self, _frame: &RawFrame) {}
    fn clear(&mut self) {}
}

/// Everything a key, menu item or toolbar button can ask for. The editor
/// performs the ones that need no window (`Editor::perform`); a front end
/// handles the rest (file dialogs, panels) itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    NewProject,
    Import,
    Export,
    OpenProject,
    SaveProject,
    SaveProjectAs,
    LocateMedia,
    Undo,
    Redo,
    ClearTimeline,
    SplitSelected,
    DeleteSelected,
    AddTitle,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    GoStart,
    GoEnd,
    PlayPause,
    StepBack,
    StepForward,
    StepBackSecond,
    StepForwardSecond,
    PrevEdit,
    NextEdit,
    ToggleMagnet,
    Deselect,
    OpenSettings,
    ShowShortcuts,
    ShuttleReverse,
    ShuttleStop,
    ShuttleForward,
}

pub struct Editor {
    pub timeline: Timeline,
    /// Timeline zoom, scroll and magnet. Scripts read and set zoom and
    /// magnet; the front end reports the lanes' width and held gestures.
    pub viewport: TimelineViewport,
    pub playhead: f64,
    pub playing: bool,
    /// J/L shuttle speed in timeline seconds per second; negative runs
    /// backward. Zero when off. Shuttling is silent and previews stills.
    pub shuttle: f64,
    last_now: Option<f64>,
    pub selected: Option<ClipId>,

    pub assets: Vec<BinAsset>,
    pub selected_asset: Option<String>,
    /// User-selected add policy: ranges from baked-in music beds
    /// start silent when true, keep their score when false.
    pub mute_music_beds: bool,
    /// Preview loudness: the mix last measured (or being measured), the
    /// result's gain, and the measurement in flight.
    pub match_export_loudness: bool,
    loudness_mix: Vec<(u64, AudioSegment)>,
    pub loudness_gain: Option<f32>,
    #[allow(clippy::type_complexity)]
    loudness_rx: Option<Receiver<(Vec<(u64, AudioSegment)>, Result<Loudness, String>)>>,
    /// The last completed measurement of `loudness_mix`.
    loudness: Option<Loudness>,
    loudness_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    loudness_checked: Instant,
    pub curator: CuratorGallery,
    pub caption_style: TitleStyle,
    pub status: String,

    pub import_rx: Option<Receiver<ImportMsg>>,
    pub import_queue: Vec<PathBuf>,
    /// Why each file that could not be imported failed, by path, until it
    /// is imported again.
    pub import_errors: HashMap<String, String>,
    /// Ready files being imported again. One that now fails stays in the
    /// bin as it was: clips may use it, and the file may come back.
    refreshing: HashSet<String>,
    sweep_rx: Option<Receiver<SweepMsg>>,
    pub sweep_queue: Vec<String>,
    /// Shared by every running curation worker; dropped when none run.
    sweep_tx: Option<Sender<SweepMsg>>,
    pub sweep_running: Vec<String>,
    initial_imports: Vec<PathBuf>,

    still_decoder: StillDecoder,
    still_rx: Receiver<(u64, Result<RawFrame, String>)>,
    preview: Box<dyn PreviewSink>,
    /// The sink holds a picture of the active clip.
    preview_shown: bool,
    /// When pictures were shown during the last second of playback.
    shown_at: VecDeque<Instant>,
    /// Playback start, so the rate is judged only over a full second.
    playing_since: Option<Instant>,
    preview_key: Option<VideoKey>,
    still_gen: u64,
    /// The one still request being decoded; `None` key discards its answer.
    /// A prefetch only shows if the playhead has since moved onto it.
    still_inflight: Option<(u64, Option<StillKey>, bool)>,
    /// The still the preview shows, if it shows one.
    still_shown: Option<StillKey>,
    /// Recently decoded stills, least recently used first.
    still_cache: VecDeque<(StillKey, RawFrame)>,
    /// Stills that failed to decode and when, so a file that cannot be read
    /// is not asked for again every frame.
    still_failed: VecDeque<(StillKey, Instant)>,

    stream: Option<FrameStream>,
    stream_key: Option<VideoKey>,
    stream_generation: u64,
    stream_ready: bool,
    stream_failed: bool,
    /// Mixer and master playback clock, opened on first play.
    audio: Option<AudioEngine>,
    /// The mixer renders only when the editor advances it: no device, so
    /// tests and windowless runs are deterministic.
    manual_audio: bool,
    /// A dry run is applying an edit it puts back before the frame ends.
    /// Playback follows the timeline, playhead and transport, and all three
    /// come back, so nothing a playing editor has open is stopped meanwhile.
    pub rehearsing: bool,
    audio_failed: bool,
    /// The playhead the audio clock last produced; any other value means
    /// something moved the playhead and the clock must follow it.
    clock_playhead: Option<f64>,
    last_sync_playhead: f64,
    last_sync_time: f64,
    /// The monitor's scrubber is held; previews like a ruler scrub.
    pub transport_scrubbing: bool,

    export_rx: Option<Receiver<ExportMsg>>,
    pub export_progress: Option<f64>,
    /// The last finished export and the status line announcing it; Open
    /// folder shows while that line is still the status.
    pub exported: Option<(PathBuf, String)>,
    /// Why the last export stopped, until the next one starts.
    pub export_failed: Option<String>,

    script_engine: Option<ScriptEngine>,
    pub console_output: Vec<String>,
    pub console_history: Vec<String>,
    pub console_history_idx: Option<usize>,

    pub settings: Settings,
    /// Saved curations; `None` keeps tests away from the user's config.
    pub curation_dir: Option<PathBuf>,
    /// The user's installed whisper models; `None` keeps tests off them.
    pub model_dir: Option<PathBuf>,
    /// Imports decode filmstrip thumbnails; the command line has no bin.
    pub make_thumbnails: bool,
    pub history: history::History,
    /// Where Save writes; set by Save as or Open.
    pub project_path: Option<PathBuf>,
    /// The timeline as last saved or opened; a difference is unsaved.
    saved_timeline: Timeline,
    /// The music-bed choice in the last project saved or opened.
    saved_mute_music_beds: bool,
    /// Project media not found on open, in the order Locate asks for it.
    pub missing_media: Vec<String>,

    /// The hold on the open project's file (`project::lock_project`), kept
    /// by an editor that stays open. Declared before `server` so it is
    /// dropped first: whoever sees the socket close can take the file.
    pub(crate) project_lock: Option<std::fs::File>,
    /// Answers agents' requests for the open project (`server`).
    server: Option<server::Server>,
    /// A window listens for whichever project it has open.
    listen: bool,
    /// The project another editor was listening for, and when it was asked.
    listen_refused: Option<(PathBuf, Instant)>,
    /// Why this window is not listening, until the status bar has said it.
    listen_notice: Option<String>,
    /// The pointer was down when the last frame ended.
    pointer_held: bool,
    /// Where editors listen; tests use a folder of their own.
    socket_dir: Option<PathBuf>,

    wake: Wake,
    /// The soonest a job in progress wants another frame without a wake.
    repaint_after: Option<Duration>,
}

/// The editor state a script's commands can change (`Editor::edit_snapshot`).
#[derive(Clone)]
pub struct EditSnapshot {
    timeline: Timeline,
    selected: Option<ClipId>,
    playhead: f64,
    playing: bool,
    magnet: bool,
    zoom: f32,
    mute_music_beds: bool,
}

/// Refuse a time before zero instead of moving it there: a script that
/// computed one has a mistake its author should hear about.
fn from_zero(command: &str, what: &str, value: f64) -> Result<(), String> {
    if value < 0.0 {
        return Err(format!(
            "{command} needs a {what} of 0 or later, not {value}"
        ));
    }
    Ok(())
}

/// A title style by its script name; the error names the ones that exist.
fn title_style(name: &str) -> Result<TitleStyle, String> {
    TitleStyle::from_name(name).ok_or_else(|| {
        let known: Vec<&str> = TitleStyle::ALL.iter().map(|style| style.as_str()).collect();
        format!("unknown title style '{name}'; use {}", known.join(", "))
    })
}

/// What captioning one clip did with each transcript line in its range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CaptionPass {
    added: usize,
    /// Already captioned by an earlier pass.
    repeated: usize,
    /// Under half inside the clip: the other side of the cut shows them.
    outside: usize,
}

impl CaptionPass {
    /// One line for the script's notes, so a clip that got no caption is
    /// not mistaken for one that was captioned.
    fn note(self, clip: ClipId) -> String {
        let count = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
        let mut parts = vec![match self.added {
            0 => "no captions added".to_owned(),
            added => format!("added {}", count(added, "caption")),
        }];
        if self.repeated > 0 {
            parts.push(format!(
                "{} already captioned",
                count(self.repeated, "line")
            ));
        }
        if self.outside > 0 {
            parts.push(format!(
                "{} left out, under half inside the clip",
                count(self.outside, "line")
            ));
        }
        if self == Self::default() {
            parts.push("no speech in its source range".to_owned());
        }
        format!("clip {clip}: {}", parts.join("; "))
    }
}

/// What one `Editor::begin_frame` saw finish.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameEvents {
    /// An export finished successfully; a window asks for attention.
    pub exported: bool,
}

impl Editor {
    /// The editor behind a window: the user's settings and saved curations,
    /// filmstrip thumbnails, and a preview at the export's loudness.
    pub fn open(wake: Wake, preview: Box<dyn PreviewSink>, initial_imports: Vec<PathBuf>) -> Self {
        let settings = Settings::load();
        openreel_media::set_hardware_decode(settings.hardware_decode);
        openreel_media::warm_hardware_decode();
        let mut editor = Self::build(wake, preview, settings);
        editor.match_export_loudness = true;
        editor.initial_imports = initial_imports;
        editor.curation_dir = sweep::default_curation_dir();
        editor.model_dir = sweep::transcribe::installed_model_dir();
        editor.manual_audio = false;
        editor.listen = true;
        editor
    }

    /// Without a window or the user's saved settings and curations, for the
    /// command line and tests. Its mixer has no device.
    pub fn headless() -> Self {
        Self::headless_with(Wake::none(), Box::new(NoPreview))
    }

    /// `headless`, drawn by a front end under test.
    pub fn headless_with(wake: Wake, preview: Box<dyn PreviewSink>) -> Self {
        Self::build(wake, preview, Settings::default())
    }

    fn build(wake: Wake, preview: Box<dyn PreviewSink>, settings: Settings) -> Self {
        let still_wake = wake.clone();
        let (still_decoder, still_rx) = StillDecoder::spawn(move || still_wake.wake());
        let (script_engine, console_output) = match ScriptEngine::new() {
            Ok(engine) => (Some(engine), vec!["Eddie JS. help()".to_owned()]),
            Err(e) => (None, vec![format!("Scripting unavailable: {e}")]),
        };
        let timeline = Timeline::empty_project().with_orientation(settings.format);
        Self {
            history: history::History::new(&timeline),
            saved_timeline: timeline.clone(),
            saved_mute_music_beds: true,
            timeline,
            viewport: TimelineViewport::default(),
            playhead: 0.0,
            playing: false,
            shuttle: 0.0,
            last_now: None,
            selected: None,
            assets: Vec::new(),
            selected_asset: None,
            mute_music_beds: true,
            match_export_loudness: false,
            loudness_mix: Vec::new(),
            loudness_gain: None,
            loudness_rx: None,
            loudness_cancel: None,
            loudness: None,
            loudness_checked: Instant::now(),
            curator: CuratorGallery::default(),
            caption_style: TitleStyle::Caption,
            status: "Ready.".to_owned(),
            import_rx: None,
            import_queue: Vec::new(),
            import_errors: HashMap::new(),
            refreshing: HashSet::new(),
            sweep_rx: None,
            sweep_queue: Vec::new(),
            sweep_tx: None,
            sweep_running: Vec::new(),
            initial_imports: Vec::new(),
            still_decoder,
            still_rx,
            preview,
            preview_shown: false,
            shown_at: VecDeque::with_capacity(128),
            playing_since: None,
            preview_key: None,
            still_gen: 0,
            still_inflight: None,
            still_shown: None,
            still_cache: VecDeque::new(),
            still_failed: VecDeque::new(),
            stream: None,
            stream_key: None,
            stream_generation: 0,
            stream_ready: false,
            stream_failed: false,
            audio: None,
            manual_audio: true,
            rehearsing: false,
            audio_failed: false,
            clock_playhead: None,
            last_sync_playhead: 0.0,
            last_sync_time: 0.0,
            transport_scrubbing: false,
            export_rx: None,
            export_progress: None,
            exported: None,
            export_failed: None,
            script_engine,
            console_output,
            console_history: Vec::new(),
            console_history_idx: None,
            settings,
            curation_dir: None,
            model_dir: None,
            make_thumbnails: true,
            project_path: None,
            missing_media: Vec::new(),
            project_lock: None,
            server: None,
            listen: false,
            listen_refused: None,
            listen_notice: None,
            pointer_held: false,
            socket_dir: server::socket_dir(),
            wake,
            repaint_after: None,
        }
    }

    /// Split at the playhead: the selected clip when the playhead is
    /// inside it, otherwise the topmost clip under the playhead.
    /// Clicking the timeline parks the playhead first, so the flow
    /// is click, then split (button, S, or B).
    pub fn split_at_playhead(&mut self) {
        let playhead = self.playhead;
        let target = self
            .selected
            .filter(|id| {
                self.timeline
                    .clip(*id)
                    .map(|c| c.contains(playhead))
                    .unwrap_or(false)
            })
            .or_else(|| {
                self.timeline
                    .clips
                    .iter()
                    .filter(|c| c.contains(playhead))
                    .min_by_key(|c| c.track)
                    .map(|c| c.id)
            });
        let Some(id) = target else {
            self.status = "No clip under the playhead.".to_owned();
            return;
        };
        match self.timeline.split_at(id, playhead) {
            Some(new_id) => {
                self.selected = Some(new_id);
                self.status = "Clip split at playhead.".to_owned();
            }
            None => {
                self.status = "Playhead is too close to a clip edge.".to_owned();
            }
        }
    }

    /// Delete a clip from a user action (menu, key, toolbar,
    /// inspector). The gap stays; the magnet only snaps while
    /// dragging and never moves clips on its own. Scripts close
    /// gaps only via the explicit CloseGaps command.
    pub fn delete_clip(&mut self, id: ClipId) {
        self.timeline.remove_clip(id);
        if self.selected == Some(id) {
            self.selected = None;
        }
    }

    /// Remove dead air from one speech clip using its imported
    /// waveform peaks. Kept phrases become contiguous clips; all
    /// material after the original clip ripples left once. A linked
    /// detach partner is rebuilt in matching linked phrases.
    ///
    /// This is waveform DSP only. It can find pauses accurately,
    /// but it cannot know that a sound is “um” or another filler;
    /// transcript evidence supplies that meaning.
    pub fn tighten_clip_silence(
        &mut self,
        id: ClipId,
        threshold: Option<f32>,
        min_silence: f64,
        padding: f64,
    ) -> Result<usize, String> {
        let target = self
            .timeline
            .clip(id)
            .cloned()
            .ok_or_else(|| format!("no clip {id}"))?;
        let source = target
            .source
            .as_deref()
            .ok_or_else(|| format!("clip {id} has no waveform audio"))?;
        let asset = self
            .asset(source)
            .ok_or_else(|| format!("clip {id} source is not in the media bin"))?;
        if !asset.info.has_audio || asset.peaks.is_empty() || asset.info.duration <= 0.0 {
            return Err(format!("clip {id} has no analyzed waveform"));
        }

        let source_start = target.source_in;
        let source_end = target.source_in + target.duration;
        let bed_overlap = asset.sweep.music_bed_overlap(source_start, source_end);
        if target.duration > 0.0 && bed_overlap >= target.duration * 0.5 {
            return Err(format!(
                "clip {id} is mostly baked-in music; silence tightening is for speech"
            ));
        }

        let ranges = openreel_media::audible_ranges(
            &asset.peaks,
            openreel_media::WAVEFORM_PEAKS_PER_SEC as f64,
            source_start,
            source_end,
            threshold.unwrap_or_else(|| openreel_media::silence_threshold(&asset.peaks)),
            min_silence,
            padding,
        );
        if ranges.is_empty() {
            return Err(format!("clip {id} has no waveform above the threshold"));
        }

        let frame = 1.0 / self.timeline.fps;
        let mut kept: Vec<openreel_media::TimeRange> = Vec::new();
        for range in ranges {
            let mut start = self.timeline.snap(range.start);
            let end = self.timeline.snap(range.end);
            if let Some(previous) = kept.last() {
                start = start.max(previous.end);
            }
            if end - start >= frame {
                kept.push(openreel_media::TimeRange { start, end });
            }
        }
        if kept.is_empty() {
            return Err(format!("clip {id} has no phrase long enough to keep"));
        }
        let kept_total: f64 = kept.iter().map(|range| range.duration()).sum();
        let removed = (target.duration - kept_total).max(0.0);
        if removed <= frame / 2.0 {
            return Ok(1);
        }

        let mut templates = vec![target.clone()];
        if let Some(linked) = target.linked_clip {
            if let Some(companion) = self.timeline.clip(linked).cloned() {
                if companion.source.as_deref() != target.source.as_deref() {
                    return Err(format!(
                        "clip {id} is linked to another source; unlink it before tightening"
                    ));
                }
                templates.push(companion);
            }
        }
        let template_ids: Vec<ClipId> = templates.iter().map(|clip| clip.id).collect();
        let original_end = target.end();
        let follower_ids: Vec<ClipId> = self
            .timeline
            .clips
            .iter()
            .filter(|clip| {
                !template_ids.contains(&clip.id) && clip.start >= original_end - f64::EPSILON
            })
            .map(|clip| clip.id)
            .collect();

        self.stop_sources();
        let mut cursor = target.start;
        let mut first_piece_ids: Vec<ClipId> = Vec::new();
        // As a split does: the fade in stays on the first phrase and the
        // fade out moves to the last.
        let last = kept.len() - 1;
        for (range_index, range) in kept.iter().enumerate() {
            let mut piece_ids: Vec<ClipId> = Vec::new();
            for template in &templates {
                let timeline_offset = template.start - target.start;
                let source_offset = template.source_in - target.source_in;
                let start = self.timeline.snap(cursor + timeline_offset);
                let source_in = (range.start + source_offset).max(0.0);
                let piece_id = if range_index == 0 {
                    if let Some(clip) = self.timeline.clip_mut(template.id) {
                        clip.start = start;
                        clip.duration = range.duration();
                        clip.source_in = source_in;
                        if last > 0 {
                            clip.fade_out = 0.0;
                        }
                    }
                    template.id
                } else {
                    let new_id = match template.source.as_deref() {
                        Some(path) => self.timeline.add_media_clip(
                            &template.name,
                            template.kind,
                            template.track,
                            start,
                            range.duration(),
                            source_in,
                            path,
                        ),
                        None => self.timeline.add_clip(
                            &template.name,
                            template.kind,
                            template.track,
                            start,
                            range.duration(),
                            source_in,
                        ),
                    };
                    if let Some(clip) = self.timeline.clip_mut(new_id) {
                        clip.opacity = template.opacity;
                        clip.volume = template.volume;
                        clip.embedded_audio = template.embedded_audio;
                        clip.duck = template.duck;
                        clip.clean_voice = template.clean_voice;
                        clip.smooth_fades = template.smooth_fades;
                        if range_index == last {
                            clip.fade_out = template.fade_out;
                        }
                    }
                    new_id
                };
                piece_ids.push(piece_id);
            }
            if piece_ids.len() == 2 {
                let (left, right) = (piece_ids[0], piece_ids[1]);
                if let Some(clip) = self.timeline.clip_mut(left) {
                    clip.linked_clip = Some(right);
                }
                if let Some(clip) = self.timeline.clip_mut(right) {
                    clip.linked_clip = Some(left);
                }
            }
            if range_index == 0 {
                first_piece_ids = piece_ids;
            }
            cursor = self.timeline.snap(cursor + range.duration());
        }

        for follower in follower_ids {
            let Some(start) = self.timeline.clip(follower).map(|clip| clip.start) else {
                continue;
            };
            let shifted = self.timeline.snap((start - removed).max(0.0));
            if let Some(clip) = self.timeline.clip_mut(follower) {
                clip.start = shifted;
            }
        }
        if let Some(first) = first_piece_ids.first().copied() {
            self.selected = Some(first);
        }
        Ok(kept.len())
    }

    /// Generate styled caption title clips for the transcript that
    /// overlaps one media clip's source range. Word timings are
    /// honored when the provider supplied them; turn-only chunks
    /// are proportional estimates. Exact duplicates are skipped.
    pub fn add_captions_for_clip(
        &mut self,
        id: ClipId,
        style: openreel_engine::TitleStyle,
    ) -> Result<usize, String> {
        Ok(self.caption_clip(id, style)?.added)
    }

    fn caption_clip(
        &mut self,
        id: ClipId,
        style: openreel_engine::TitleStyle,
    ) -> Result<CaptionPass, String> {
        let target = self
            .timeline
            .clip(id)
            .cloned()
            .ok_or_else(|| format!("no clip {id}"))?;
        if target.kind == ClipKind::Title {
            return Err(format!("clip {id} is already a title"));
        }
        let source = target
            .source
            .as_deref()
            .ok_or_else(|| format!("clip {id} has no transcript source"))?;
        let asset = self
            .asset(source)
            .ok_or_else(|| format!("clip {id} source is not in the media bin"))?;
        if asset.sweep.transcript.is_empty() {
            return Err(format!("clip {id} has no transcript to caption"));
        }
        let transcript = asset.sweep.transcript.clone();
        let source_start = target.source_in;
        let source_end = target.source_in + target.duration;
        let frame = 1.0 / self.timeline.fps;
        let mut pass = CaptionPass::default();
        let mut last_added: Option<ClipId> = None;

        for segment in &transcript {
            for chunk in sweep::caption_chunks(segment) {
                if chunk.end <= source_start || chunk.start >= source_end {
                    continue;
                }
                let clipped_start = chunk.start.max(source_start);
                let clipped_end = chunk.end.min(source_end);
                // A line mostly spoken on the other side of a cut belongs
                // there; showing it again here repeats it for a moment.
                if clipped_end - clipped_start < 0.5 * (chunk.end - chunk.start) {
                    pass.outside += 1;
                    continue;
                }
                let start = self
                    .timeline
                    .snap(target.start + (clipped_start - source_start));
                let mut end = self
                    .timeline
                    .snap(target.start + (clipped_end - source_start));
                if end - start < frame {
                    end = (start + frame).min(target.end());
                }
                if end <= start {
                    pass.outside += 1;
                    continue;
                }
                let duplicate = self.timeline.clips.iter().any(|clip| {
                    clip.kind == ClipKind::Title
                        && clip.name == chunk.text
                        && clip.title_style == style
                        && (clip.start - start).abs() <= frame / 2.0
                });
                if duplicate {
                    pass.repeated += 1;
                    continue;
                }
                let duration = end - start;
                let track = self.first_free_track(start, duration);
                let caption = self.timeline.add_clip(
                    &chunk.text,
                    ClipKind::Title,
                    track,
                    start,
                    duration,
                    0.0,
                );
                if let Some(clip) = self.timeline.clip_mut(caption) {
                    clip.title_style = style;
                    // Offsets from the caption's start, so word `i` lights
                    // when its source time plays.
                    clip.words = chunk
                        .words
                        .iter()
                        .map(|word| {
                            (target.start + word - source_start - start).clamp(0.0, duration)
                        })
                        .collect();
                }
                last_added = Some(caption);
                pass.added += 1;
            }
        }
        // A second identical pass is a successful no-op, as is a clip whose
        // source range holds no speech; the counts say which.
        if let Some(caption) = last_added {
            self.selected = Some(caption);
        }
        Ok(pass)
    }

    pub fn stop_sources(&mut self) {
        if self.rehearsing {
            return;
        }
        if let Some(stream) = &self.stream {
            stream.cancel_preview();
        }
        self.stream_key = None;
        self.stream_ready = false;
        self.stream_failed = false;
        // A still decoding now still answers its own frame; let it land.
        self.still_shown = None;
        self.audio_failed = false;
        self.clock_playhead = None;
        if let Some(audio) = &mut self.audio {
            audio.seek(self.playhead);
            audio.set_running(false);
        }
    }

    pub fn audio_engine(&mut self) -> &mut AudioEngine {
        // An unplugged device (or a restarted sound server) stops calling
        // back, and the transport clock with it. Start again on whatever is
        // the default output now.
        if self.audio.as_ref().is_some_and(AudioEngine::device_lost) {
            self.audio = None;
            self.clock_playhead = None;
            self.status = "Audio device lost; switched output.".to_owned();
        }
        if self.audio.is_none() {
            let engine = if self.manual_audio {
                AudioEngine::manual()
            } else {
                AudioEngine::open()
            };
            eprintln!("audio output: {}", engine.description);
            if engine.description.starts_with("no audio device") {
                self.status = "No audio device; playing silently.".to_owned();
            }
            if self.match_export_loudness {
                engine.set_loudness(self.loudness_gain);
            }
            self.audio = Some(engine);
        }
        self.audio.as_mut().unwrap()
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.shuttle = 0.0;
        if playing {
            self.curator.open = false;
            if self.stream_failed {
                self.stop_sources();
            }
        }
        if playing && self.playhead >= self.timeline.end() {
            self.playhead = 0.0;
        }
        self.playing = playing;
        self.shown_at.clear();
        self.playing_since = playing.then(Instant::now);
        if !playing {
            self.stop_sources();
            if self.status == "Buffering video." || self.status == "Playing." {
                self.status = "Paused.".to_owned();
            }
        }
    }

    pub fn seek(&mut self, time: f64) {
        self.playhead = self
            .timeline
            .snap(time.clamp(0.0, self.timeline.end().max(0.0)));
        // The current picture stays up until the new one arrives; a different
        // clip's picture is cleared in poll_frames.
        self.stop_sources();
    }

    /// Ruler, clip drag or monitor scrubber held: preview from low-res
    /// stills instead of restarting playback on every move.
    pub fn scrubbing(&self) -> bool {
        self.viewport.interacting || self.transport_scrubbing || self.shuttle != 0.0
    }

    /// J and L: the first L plays normally; each repeat in the same
    /// direction doubles the speed up to 8x. Reverse and faster-than-1x
    /// runs are silent and preview low-res stills.
    pub fn shuttle(&mut self, forward: bool) {
        let sign = if forward { 1.0 } else { -1.0 };
        let speed = if self.shuttle * sign > 0.0 {
            (self.shuttle.abs() * 2.0).min(SHUTTLE_MAX)
        } else if forward && self.playing {
            2.0
        } else {
            1.0
        };
        if forward && speed == 1.0 {
            self.curator.open = false;
            self.set_playing(true);
            self.status = "Playing.".to_owned();
            return;
        }
        self.set_playing(false);
        self.curator.open = false;
        self.shuttle = sign * speed;
        self.status = format!("{} {speed}x.", if forward { "Forward" } else { "Reverse" });
    }

    pub fn active_video_clip(&self) -> Option<openreel_engine::Clip> {
        self.timeline.active_video(self.playhead).cloned()
    }

    pub fn active_video_key(&self) -> Option<VideoKey> {
        let clip = self.timeline.active_video(self.playhead)?;
        Some((
            clip.id,
            clip.source.clone()?,
            clip.start.to_bits(),
            clip.source_in.to_bits(),
        ))
    }

    /// The next edge after `after` where a different clip becomes visible,
    /// with its source and source time there, as `sync_playback` computes
    /// them at that edge. Edges into a video gap need no picture.
    pub fn next_video_change(&self, after: f64) -> Option<(f64, Option<(&str, f64)>)> {
        let current = self.timeline.active_video(after).map(|clip| clip.id);
        let mut next: Option<(f64, &openreel_engine::Clip)> = None;
        for clip in &self.timeline.clips {
            if clip.kind != ClipKind::Video {
                continue;
            }
            for edge in [clip.start, clip.end()] {
                if edge <= after || next.is_some_and(|(best, _)| edge >= best) {
                    continue;
                }
                if let Some(visible) = self.timeline.active_video(edge) {
                    if Some(visible.id) != current {
                        next = Some((edge, visible));
                    }
                }
            }
        }
        let (edge, clip) = next?;
        let source = clip
            .source
            .as_deref()
            .map(|source| (source, clip.source_in + (edge - clip.start)));
        Some((edge, source))
    }

    /// How far the audio clock may run before a picture must be ready: the
    /// next visible cut, or the one after it when the preview worker has
    /// already prerolled that cut's first picture.
    pub fn clock_limit(&self) -> f64 {
        let end = self.timeline.end();
        if self.waiting_for_video() {
            return self.playhead;
        }
        let Some((edge, source)) = self.next_video_change(self.playhead) else {
            return end;
        };
        let prerolled = source
            .zip(self.stream.as_ref())
            .is_some_and(|((path, time), stream)| stream.prefetch_ready(path, time, PREVIEW_W));
        if !prerolled {
            return edge.min(end);
        }
        self.next_video_change(edge)
            .map_or(end, |(after, _)| after)
            .min(end)
    }

    pub fn waiting_for_video(&self) -> bool {
        self.playing
            && self
                .active_video_key()
                .is_some_and(|key| self.stream_key.as_ref() != Some(&key) || !self.stream_ready)
    }

    pub fn video_playback_failed(&mut self, error: String) {
        self.playing = false;
        self.stream_failed = true;
        if let Some(audio) = &self.audio {
            audio.set_running(false);
        }
        self.status = format!("Video playback failed: {error}");
    }

    /// The monitor holds the active clip's picture. False while a newer
    /// clip's first picture is still decoding: draw nothing, not the old one.
    pub fn preview_is_current(&self) -> bool {
        self.preview_shown && self.preview_key == self.active_video_key()
    }

    /// Forget the shown picture so the next still is asked for again, such
    /// as at full size once a low-resolution scrub lets go.
    pub fn drop_preview(&mut self) {
        self.preview.clear();
        self.preview_shown = false;
        self.preview_key = None;
    }

    fn show_preview(&mut self, frame: &RawFrame) {
        self.preview.show(frame);
        self.preview_shown = true;
        self.preview_key = self.active_video_key();
        if self.playing {
            let now = Instant::now();
            while self
                .shown_at
                .front()
                .is_some_and(|at| now.duration_since(*at) > Duration::from_secs(1))
            {
                self.shown_at.pop_front();
            }
            self.shown_at.push_back(now);
        }
    }

    /// Pictures shown over the last second of playback and the rate the
    /// active picture should reach (its source rate, at most the project's).
    /// `None` until playback has run a full second over video.
    pub fn playback_rate(&self) -> Option<(usize, f64)> {
        let since = self.playing_since?;
        if !self.playing || since.elapsed() < Duration::from_secs(1) {
            return None;
        }
        let clip = self.timeline.active_video(self.playhead)?;
        let source_fps = self
            .asset(clip.source.as_deref()?)
            .map_or(0.0, |asset| asset.info.fps);
        let target = if source_fps > 0.0 {
            source_fps.min(self.timeline.fps)
        } else {
            self.timeline.fps
        };
        let now = Instant::now();
        let shown = self
            .shown_at
            .iter()
            .filter(|at| now.duration_since(**at) <= Duration::from_secs(1))
            .count();
        Some((shown, target))
    }

    /// Drop cached stills and the answer in flight after the timeline is
    /// replaced, so no picture from before can show.
    pub fn forget_stills(&mut self) {
        if self.rehearsing {
            return;
        }
        self.still_shown = None;
        self.still_cache.clear();
        self.still_failed.clear();
        if let Some((_, key, _)) = &mut self.still_inflight {
            *key = None;
        }
    }

    pub fn poll_frames(&mut self) {
        let active_key = self.active_video_key();
        if self.preview_key != active_key {
            if self.preview_shown {
                self.preview.clear();
                self.preview_shown = false;
            }
            self.preview_key = None;
        }
        let active = self.active_video_clip();
        let source_time = active
            .as_ref()
            .map(|clip| clip.source_in + (self.playhead - clip.start));
        let wants_still = !self.playing || self.scrubbing();
        while let Ok((gen, result)) = self.still_rx.try_recv() {
            let Some((pending, key, prefetch)) = self.still_inflight else {
                continue;
            };
            if gen != pending {
                continue;
            }
            self.still_inflight = None;
            let Some(key) = key else {
                continue;
            };
            let Ok(frame) = result else {
                self.still_failed.retain(|(failed, _)| *failed != key);
                if self.still_failed.len() >= STILL_CACHE_CAP {
                    self.still_failed.pop_front();
                }
                self.still_failed.push_back((key, Instant::now()));
                continue;
            };
            // The newest finished picture of this clip shows even when the
            // pointer has moved on; the next request chases the pointer.
            // A prefetched neighbor shows only once the playhead is on it.
            let shows = if prefetch {
                self.still_target(self.playhead, PREVIEW_W)
                    .is_some_and(|target| target.key == key)
            } else {
                active.as_ref().is_some_and(|clip| clip.id == key.0)
            };
            if wants_still && shows {
                self.show_preview(&frame);
                self.still_shown = Some(key);
            }
            self.still_cache.retain(|(cached, _)| *cached != key);
            if self.still_cache.len() >= STILL_CACHE_CAP {
                self.still_cache.pop_front();
            }
            self.still_cache.push_back((key, frame));
        }

        // A boundary/seek may precede sync_playback in this UI frame. Never
        // upload the previous generation as the active clip's picture.
        if wants_still || self.stream_key != active_key {
            return;
        }
        let result = self
            .stream
            .as_ref()
            .and_then(|stream| stream.preview_frame(self.stream_generation, source_time?));
        match result {
            Some(Ok(frame)) => {
                self.show_preview(&frame);
                self.still_shown = None;
                self.stream_ready = true;
                if self.status == "Buffering video." {
                    self.status = "Playing.".to_owned();
                }
                if let Some(stream) = &self.stream {
                    stream.recycle(frame.rgba);
                }
            }
            Some(Err(error)) => self.video_playback_failed(error),
            None => {}
        }
    }

    /// The still for the video picture at timeline time `at`, decoded at
    /// `width`.
    pub fn still_target(&self, at: f64, width: u32) -> Option<StillTarget> {
        let clip = self.timeline.active_video(at)?;
        let source = clip.source.clone()?;
        let info = &self.asset(&source)?.info;
        let time = clip.source_in + (at - clip.start);
        Some(StillTarget {
            key: (clip.id, (time * self.timeline.fps).round() as i64, width),
            size: (info.width, info.height),
            source,
            time,
        })
    }

    pub fn request_still(&mut self, target: StillTarget, prefetch: bool) {
        self.still_gen += 1;
        self.still_inflight = Some((self.still_gen, Some(target.key), prefetch));
        let width = target.key.2;
        self.still_decoder.request(
            self.still_gen,
            target.source,
            target.size.0,
            target.size.1,
            target.time,
            width,
            width <= SCRUB_W,
        );
    }

    pub fn request_still_if_needed(&mut self) {
        // Two tiers: fast small frames while interacting, full quality when idle.
        let width = if self.scrubbing() { SCRUB_W } else { PREVIEW_W };
        let Some(target) = self.still_target(self.playhead, width) else {
            return;
        };
        let key = target.key;
        let cached =
            |app: &Self, key: StillKey| app.still_cache.iter().position(|(c, _)| *c == key);
        // A relinked or repaired file gets another try, but not a busy loop.
        let failed = |app: &Self, key: StillKey| {
            app.still_failed
                .iter()
                .any(|(failed, at)| *failed == key && at.elapsed() < STILL_RETRY)
        };
        if self.still_shown != Some(key) || !self.preview_shown {
            if let Some(index) = cached(self, key) {
                // Most recently used moves to the back.
                let (key, frame) = self.still_cache.remove(index).unwrap();
                self.show_preview(&frame);
                self.still_shown = Some(key);
                self.still_cache.push_back((key, frame));
            } else {
                // One request at a time: while it decodes, the pointer can
                // move on; its answer shows, then this asks for wherever the
                // pointer is.
                if self.still_inflight.is_none() && !failed(self, key) {
                    self.request_still(target, false);
                }
                return;
            }
        }
        // Paused on a picture: decode the frames an arrow step reaches next,
        // so the step shows from the cache in the same UI frame.
        if width != PREVIEW_W || self.still_inflight.is_some() {
            return;
        }
        let frame = 1.0 / self.timeline.fps;
        for at in [self.playhead + frame, self.playhead - frame] {
            let at = self.timeline.snap(at);
            if at < 0.0 || at > self.timeline.end() {
                continue;
            }
            if let Some(neighbor) = self.still_target(at, PREVIEW_W) {
                if cached(self, neighbor.key).is_none() && !failed(self, neighbor.key) {
                    self.request_still(neighbor, true);
                    return;
                }
            }
        }
    }

    /// Keep the video stream and audio players aligned with the playhead.
    /// Restarts sources on seeks and clip-boundary crossings.
    pub fn sync_playback(&mut self) {
        let now = self.last_now.unwrap_or(0.0);
        if self.last_sync_time > 0.0 && self.shuttle == 0.0 {
            let expected = if self.playing && !self.waiting_for_video() {
                self.last_sync_playhead + (now - self.last_sync_time)
            } else {
                self.last_sync_playhead
            };
            if (self.playhead - expected).abs() > 0.35 {
                self.stop_sources();
            }
        }
        self.last_sync_playhead = self.playhead;
        self.last_sync_time = now;

        // While scrubbing/dragging, never (re)spawn playback processes —
        // still frames carry the picture until the pointer settles.
        if self.scrubbing() {
            if self.stream_key.is_some() || self.audio_running() {
                self.stop_sources();
            }
            self.request_still_if_needed();
            return;
        }

        if !self.playing {
            // A terminal decode failure pauses visibly instead of consuming
            // an unseen clip or automatically retrying it through stills.
            if self.stream_failed && self.stream_key == self.active_video_key() {
                return;
            }
            if self.stream_key.is_some() || self.audio_running() {
                self.stop_sources();
            }
            self.request_still_if_needed();
            return;
        }

        // Video stream follows the active video clip.
        let active_video = self.active_video_clip();
        let active_key = self.active_video_key();
        if self.stream_key != active_key {
            // A following request replaces the generation itself; cancelling
            // first would also discard the prerolled next clip.
            let requests = active_key
                .as_ref()
                .is_some_and(|(_, source, _, _)| self.asset(source).is_some());
            if !requests {
                if let Some(stream) = &self.stream {
                    stream.cancel_preview();
                }
            }
            self.stream_key = active_key;
            self.stream_ready = false;
            self.stream_failed = false;
            self.still_shown = None;
            if let Some(clip) = &active_video {
                if let Some(source) = &clip.source {
                    if self.asset(source).is_some() {
                        let start = clip.source_in + (self.playhead - clip.start);
                        if self.stream.is_none() {
                            let wake = self.wake.clone();
                            match FrameStream::spawn_preview(move || wake.wake()) {
                                Ok(stream) => self.stream = Some(stream),
                                Err(error) => self.video_playback_failed(error),
                            }
                        }
                        if let Some(stream) = &self.stream {
                            // A prerolled first picture is handed over, not
                            // decoded. Holding the clock for its upload would
                            // stall audio already rendered past the cut.
                            let prerolled = stream.prefetch_ready(source, start, PREVIEW_W);
                            match stream.request_preview(
                                source,
                                start,
                                PREVIEW_W,
                                self.timeline.fps,
                            ) {
                                Ok(generation) => {
                                    self.stream_generation = generation;
                                    self.stream_ready = prerolled;
                                }
                                Err(error) => self.video_playback_failed(error),
                            }
                        }
                    } else {
                        self.video_playback_failed("source is not imported".to_owned());
                    }
                }
            }
        }

        if self.stream_failed {
            return;
        }
        // Preroll the next visible clip while this one plays or buffers, so
        // its first picture is ready when the playhead lands on the cut.
        if let (Some(stream), Some((edge, Some((source, time))))) =
            (&self.stream, self.next_video_change(self.playhead))
        {
            // Where the playing clip's decoder will be when the cut comes.
            let active_until = self
                .timeline
                .active_video(self.playhead)
                .map(|clip| clip.source_in + (edge - clip.start));
            let _ = stream.prefetch_preview(source, time, PREVIEW_W, active_until);
        }
        self.sync_audio();
        if self.waiting_for_video() {
            // Preroll can exceed a short excerpt. The clock limit holds the
            // shared transport and its audio until the first picture.
            // A hardware failure during buffering keeps its notice visible.
            if !self.status.starts_with("Hardware decode failed") {
                self.status = "Buffering video.".to_owned();
            }
            // A held clock is not a seek, even if the next UI tick is late.
            self.last_sync_time = 0.0;
        }
    }

    pub fn audio_running(&self) -> bool {
        self.audio
            .as_ref()
            .is_some_and(|audio| !audio.voice_ids().is_empty())
    }

    /// Where each source that a non-ducking clip plays has speech, in
    /// source seconds: its transcript turns, leaving out sparse ambience
    /// turns. An untranscribed source falls back to its waveform outside the
    /// silences tightenSilence uses, since missing transcription is
    /// uncertainty, not silence. Empty when no clip ducks, so playback pays
    /// nothing for it.
    pub fn speech_map(&self) -> HashMap<String, Vec<(f64, f64)>> {
        if !self.timeline.clips.iter().any(|clip| clip.duck) {
            return HashMap::new();
        }
        let mut map = HashMap::new();
        for clip in self.timeline.clips.iter().filter(|clip| !clip.duck) {
            let Some(source) = clip.source.as_ref() else {
                continue;
            };
            if map.contains_key(source) {
                continue;
            }
            let Some(asset) = self.asset(source) else {
                continue;
            };
            if !asset.sweep.transcript.is_empty() {
                let turns = asset
                    .sweep
                    .transcript
                    .iter()
                    .filter(|turn| turn.end > turn.start && !turn.is_ambient())
                    .map(|turn| (turn.start, turn.end))
                    .collect();
                map.insert(source.clone(), turns);
                continue;
            }
            if !asset.info.has_audio || asset.peaks.is_empty() {
                continue;
            }
            // Peaks keep their own rate; sound can end before the container.
            let rate = openreel_media::WAVEFORM_PEAKS_PER_SEC as f64;
            let sound_end = asset.peaks.len() as f64 / rate;
            let mut audible = Vec::new();
            let mut cursor = 0.0;
            let threshold = openreel_media::silence_threshold(&asset.peaks);
            for quiet in openreel_media::silence_ranges(&asset.peaks, rate, threshold, 0.35) {
                if quiet.start > cursor {
                    audible.push((cursor, quiet.start));
                }
                cursor = quiet.end;
            }
            if cursor < sound_end {
                audible.push((cursor, sound_end));
            }
            map.insert(source.clone(), audible);
        }
        map
    }

    /// Give the mixer every audible clip under the playhead or starting
    /// within the look-ahead, so it starts decoding before their edges.
    pub fn sync_audio(&mut self) {
        const LOOK_AHEAD: f64 = 0.75;
        let segments = self.audio_segments(Some((self.playhead, self.playhead + LOOK_AHEAD)));
        let audio = self.audio_engine();
        audio.set_voices(&segments);
        if let Some(error) = audio.take_error() {
            if !self.audio_failed {
                self.audio_failed = true;
                self.status = format!("Audio playback failed: {error}");
            }
        }
    }

    /// Every audible clip as the mixer plays it (ducking included), or only
    /// those overlapping `window`.
    pub fn audio_segments(&self, window: Option<(f64, f64)>) -> Vec<(u64, AudioSegment)> {
        let speech = self.speech_map();
        self.timeline
            .clips
            .iter()
            .filter(|c| {
                window.is_none_or(|(from, to)| c.start < to && c.end() > from)
                    && (c.kind == ClipKind::Audio
                        || (c.kind == ClipKind::Video && c.embedded_audio))
            })
            .filter_map(|c| {
                let source = c.source.as_ref()?;
                self.asset(source)?.info.has_audio.then(|| {
                    (
                        c.id,
                        AudioSegment {
                            path: source.clone(),
                            start: c.start,
                            duration: c.duration,
                            source_in: c.source_in,
                            volume: c.volume,
                            fade_in: c.fade_in,
                            fade_out: c.fade_out,
                            duck: self.timeline.duck_ranges(c, &speech),
                            clean_voice: c.clean_voice,
                            smooth_fades: c.smooth_fades,
                        },
                    )
                })
            })
            .collect()
    }

    /// The measured mix for scripts: loudness before export's gain,
    /// what export will do to it, quiet stretches, and the mix's loudness
    /// while each audible clip plays.
    pub fn audio_report(&self) -> serde_json::Value {
        let round = |value: f64| (value * 100.0).round() / 100.0;
        let status = if self.audio_segments(None).is_empty() {
            "silent"
        } else if !self.mix_measured() {
            "measuring"
        } else if self.loudness.is_none() {
            "failed"
        } else {
            "measured"
        };
        let Some(loudness) = self.loudness.as_ref().filter(|_| status == "measured") else {
            return serde_json::json!({ "status": status });
        };
        let whole = 0.0..loudness.duration();
        let integrated = loudness.integrated(whole.clone());
        let peak = loudness.true_peak(whole);
        let gain =
            integrated.map(|lufs| (openreel_media::LOUDNESS_TARGET - lufs).clamp(-30.0, 30.0));
        let export_peak = peak.zip(gain).map(|(peak, gain)| peak + gain);
        let clips: Vec<serde_json::Value> = self
            .loudness_mix
            .iter()
            .map(|(id, segment)| {
                let range = segment.start..segment.start + segment.duration;
                serde_json::json!({
                    "id": id,
                    "integratedLufs": loudness.integrated(range.clone()).map(round),
                    "truePeakDbtp": loudness.true_peak(range).map(round),
                })
            })
            .collect();
        serde_json::json!({
            "status": status,
            "integratedLufs": integrated.map(round),
            "truePeakDbtp": peak.map(round),
            "maxMomentaryLufs": loudness.max_momentary().map(round),
            "clippedSamples": loudness.clipped,
            "exportTargetLufs": openreel_media::LOUDNESS_TARGET,
            "exportGainDb": gain.map(round),
            "exportPeakDbtp": export_peak.map(round),
            // Export's limiter holds peaks at 0.8 (-1.94 dBFS).
            "limited": export_peak.is_some_and(|peak| peak > 20.0 * 0.8_f64.log10()),
            "quietRanges": loudness
                .quiet_ranges(-60.0, 1.0)
                .into_iter()
                .map(|range| [round(range.start), round(range.end)])
                .collect::<Vec<_>>(),
            "clips": clips,
        })
    }

    /// Keep the preview at the export's loudness. Half a second after the
    /// mix changes (and with no pointer held), the whole mix is rendered and
    /// measured off the UI thread; the preview then plays with the gain
    /// export would apply and the same 0.8 limiter.
    pub fn poll_loudness(&mut self, pointer_down: bool) {
        self.take_loudness();
        if !self.match_export_loudness
            || pointer_down
            || self.loudness_checked.elapsed() < Duration::from_millis(500)
        {
            return;
        }
        self.measure_mix();
    }

    /// Take in a finished measurement.
    fn take_loudness(&mut self) {
        if let Some(rx) = &self.loudness_rx {
            match rx.try_recv() {
                Ok((mix, result)) => {
                    self.loudness_rx = None;
                    if mix == self.loudness_mix {
                        self.loudness = result.ok();
                        let lufs = self
                            .loudness
                            .as_ref()
                            .and_then(|loudness| loudness.integrated(0.0..loudness.duration()));
                        let gain = lufs.map(|lufs| {
                            let db = (openreel_media::LOUDNESS_TARGET - lufs).clamp(-30.0, 30.0);
                            10f64.powf(db / 20.0) as f32
                        });
                        self.loudness_gain = gain;
                        self.match_preview_loudness();
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.loudness_rx = None,
            }
        }
    }

    /// Only an editor with a preview plays the measured gain; a measurement
    /// made for a script changes nothing anyone hears.
    fn match_preview_loudness(&mut self) {
        if self.match_export_loudness {
            let gain = self.loudness_gain;
            self.audio_engine().set_loudness(gain);
        }
    }

    /// Whether the last measurement is of the mix the timeline has now.
    pub fn mix_measured(&self) -> bool {
        self.loudness_rx.is_none() && self.loudness_mix == self.audio_segments(None)
    }

    /// Measure the mix off this thread unless that measurement exists or is
    /// under way. Its worker wakes the editor; call again, or
    /// `poll_loudness`, to take the result in.
    pub fn measure_mix(&mut self) {
        self.take_loudness();
        self.loudness_checked = Instant::now();
        let mix = self.audio_segments(None);
        if mix == self.loudness_mix {
            return;
        }
        if let Some(cancel) = self.loudness_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.loudness_mix = mix.clone();
        if mix.is_empty() {
            self.loudness_rx = None;
            self.loudness_gain = None;
            self.loudness = None;
            self.match_preview_loudness();
            return;
        }
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.loudness_cancel = Some(Arc::clone(&cancel));
        let (tx, rx) = channel();
        self.loudness_rx = Some(rx);
        let end = self.timeline.end();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let result = openreel_media::measure_mix(&mix, end, &cancel);
            let _ = tx.send((mix, result));
            wake.wake();
        });
    }

    /// Returns true when an export finished successfully this frame.
    pub fn poll_export(&mut self) -> bool {
        let mut done = false;
        let mut exported = false;
        loop {
            let msg = match self.export_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(msg)) => msg,
                Some(Err(TryRecvError::Empty)) => break,
                Some(Err(TryRecvError::Disconnected)) => {
                    if !done {
                        // The worker ended without an answer: it panicked.
                        let error = "Export stopped unexpectedly.".to_owned();
                        self.export_failed = Some(error.clone());
                        self.status = error;
                    }
                    done = true;
                    break;
                }
                None => break,
            };
            match msg {
                ExportMsg::Progress(p) => self.export_progress = Some(p),
                ExportMsg::Done(Ok(path)) => {
                    let name = path.file_name().unwrap_or(path.as_os_str());
                    self.status = format!("Exported {}", name.to_string_lossy());
                    self.exported = Some((path, self.status.clone()));
                    exported = true;
                    done = true;
                }
                ExportMsg::Done(Err(e)) => {
                    self.export_failed = Some(e.clone());
                    self.status = e;
                    done = true;
                }
            }
        }
        if done {
            self.export_rx = None;
            self.export_progress = None;
        }
        exported
    }

    pub fn start_export(&mut self, output: PathBuf) {
        if self.export_rx.is_some() {
            self.status = "An export is already running.".to_owned();
            return;
        }
        if self.timeline.clips.is_empty() {
            self.status = "Nothing to export.".to_owned();
            return;
        }
        if let Some(used) = self.export_would_replace(&output) {
            self.status = format!("Export cannot replace {used}, which this project uses.");
            return;
        }
        let timeline = self.timeline.clone();
        let options = openreel_media::ExportOptions {
            normalize_loudness: true,
            speech: self.speech_map(),
        };
        let infos: HashMap<String, MediaInfo> = self
            .assets
            .iter()
            .map(|a| (a.info.path.clone(), a.info.clone()))
            .collect();
        let (tx, rx) = channel::<ExportMsg>();
        let (progress_tx, progress_rx) = channel::<f64>();
        self.export_rx = Some(rx);
        self.export_progress = Some(0.0);
        self.exported = None;
        self.export_failed = None;
        let wake = self.wake.clone();
        thread::spawn(move || {
            let tx_progress = tx.clone();
            let wake_progress = wake.clone();
            let forwarder = thread::spawn(move || {
                while let Ok(p) = progress_rx.recv() {
                    let _ = tx_progress.send(ExportMsg::Progress(p));
                    wake_progress.wake();
                }
            });
            let result =
                openreel_media::export_timeline(&timeline, &infos, &output, &options, progress_tx);
            let _ = forwarder.join();
            let _ = tx.send(ExportMsg::Done(result));
            wake.wake();
        });
    }

    /// The name of the project file or imported media that `output` is.
    /// Exporting over one would destroy what the export is made from.
    pub(crate) fn export_would_replace(&self, output: &Path) -> Option<String> {
        let output = std::fs::canonicalize(output).ok()?;
        self.project_path
            .iter()
            .map(PathBuf::as_path)
            .chain(self.assets.iter().map(|asset| Path::new(&asset.info.path)))
            .find(|used| std::fs::canonicalize(used).is_ok_and(|used| used == output))
            .map(|used| {
                used.file_name()
                    .unwrap_or(used.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            })
    }

    /// Make the timeline as it stands an undo step now, without waiting for
    /// the frame to end. A script's edit is one step, apart from whatever
    /// the user was in the middle of.
    pub fn commit_history(&mut self) {
        let now = self.last_now.unwrap_or(0.0);
        self.history
            .track(&self.timeline, self.selected, now, false, false);
    }

    /// Undo or redo one step. False when there was nothing to step.
    pub fn step_history(&mut self, redo: bool) -> bool {
        let stepped = if redo {
            self.history.redo(&mut self.timeline, &mut self.selected)
        } else {
            self.history.undo(&mut self.timeline, &mut self.selected)
        };
        if !stepped {
            self.status = if redo {
                "Nothing to redo."
            } else {
                "Nothing to undo."
            }
            .to_owned();
            return false;
        }
        self.set_playing(false);
        self.playhead = self.playhead.min(self.timeline.end());
        // A format change is a step too; Settings shows the project's format.
        self.settings.format = self.timeline.orientation;
        self.forget_stills();
        self.status = if redo {
            "Redid the edit."
        } else {
            "Undid the last edit."
        }
        .to_owned();
        true
    }

    /// Do what `action` asks when it needs no window. False leaves it to
    /// the front end: file dialogs, the unsaved-changes prompt and panels.
    pub fn perform(&mut self, action: Action) -> bool {
        match action {
            Action::Import
            | Action::Export
            | Action::NewProject
            | Action::OpenProject
            | Action::SaveProject
            | Action::SaveProjectAs
            | Action::LocateMedia
            | Action::OpenSettings
            | Action::ShowShortcuts => return false,
            Action::Undo => {
                self.step_history(false);
            }
            Action::Redo => {
                self.step_history(true);
            }
            Action::ClearTimeline => {
                self.clear_timeline();
                self.status = "Timeline cleared. Media bin kept.".to_owned();
            }
            Action::SplitSelected => self.split_at_playhead(),
            Action::DeleteSelected => {
                if let Some(id) = self.selected.take() {
                    self.delete_clip(id);
                    self.status = "Clip deleted.".to_owned();
                }
            }
            Action::AddTitle => {
                let track = self.first_free_track(self.playhead, 4.0);
                let id = self.timeline.add_clip(
                    "New title",
                    ClipKind::Title,
                    track,
                    self.playhead,
                    4.0,
                    0.0,
                );
                self.selected = Some(id);
                self.status = "Title added.".to_owned();
            }
            Action::ZoomIn => self.viewport.zoom(1.5, self.playhead, self.timeline.end()),
            Action::ZoomOut => self
                .viewport
                .zoom(1.0 / 1.5, self.playhead, self.timeline.end()),
            Action::ZoomFit => self.viewport.zoom_to_fit(self.timeline.end()),
            Action::GoStart => self.seek_and_reveal(0.0),
            Action::GoEnd => self.seek_and_reveal(self.timeline.end()),
            Action::PlayPause => {
                // Playback shows on Program, not behind the gallery.
                self.curator.open = false;
                // Space stops a shuttle rather than starting playback.
                let next = !self.playing && self.shuttle == 0.0;
                self.set_playing(next);
                if !next {
                    self.playhead = self.timeline.snap(self.playhead);
                }
            }
            Action::ShuttleReverse => self.shuttle(false),
            Action::ShuttleForward => self.shuttle(true),
            Action::ShuttleStop => {
                self.set_playing(false);
                self.playhead = self.timeline.snap(self.playhead);
                self.status = "Paused.".to_owned();
            }
            Action::StepBack => self.seek_and_reveal(self.playhead - 1.0 / self.timeline.fps),
            Action::StepForward => self.seek_and_reveal(self.playhead + 1.0 / self.timeline.fps),
            Action::StepBackSecond => self.seek_and_reveal(self.playhead - 1.0),
            Action::StepForwardSecond => self.seek_and_reveal(self.playhead + 1.0),
            Action::PrevEdit | Action::NextEdit => {
                if let Some(time) = self.edit_point(action == Action::NextEdit) {
                    self.seek_and_reveal(time);
                }
            }
            Action::ToggleMagnet => {
                self.viewport.magnet = !self.viewport.magnet;
                self.status = if self.viewport.magnet {
                    "Magnet on.".to_owned()
                } else {
                    "Magnet off.".to_owned()
                };
            }
            Action::Deselect => self.selected = None,
        }
        true
    }

    /// Empty the timeline and keep the bin and the project's format.
    pub fn clear_timeline(&mut self) {
        self.set_playing(false);
        self.timeline = Timeline::empty_project().with_orientation(self.timeline.orientation);
        self.selected = None;
        self.playhead = 0.0;
        self.forget_stills();
    }

    /// Keyboard and transport seeks keep the playhead on screen.
    pub fn seek_and_reveal(&mut self, time: f64) {
        self.seek(time);
        self.viewport.reveal(self.playhead);
    }

    /// The nearest clip edge (or timeline start/end) after or before the
    /// playhead. Half a frame of slack steps past an edge the grid moved.
    pub fn edit_point(&self, forward: bool) -> Option<f64> {
        let slack = 0.5 / self.timeline.fps;
        let points = self
            .timeline
            .clips
            .iter()
            .flat_map(|clip| [clip.start, clip.end()])
            .chain([0.0, self.timeline.end()]);
        if forward {
            points
                .filter(|time| *time > self.playhead + slack)
                .min_by(f64::total_cmp)
        } else {
            points
                .filter(|time| *time < self.playhead - slack)
                .max_by(f64::total_cmp)
        }
    }

    // -- scripting ---------------------------------------------------------------

    /// The timeline's clips as scripts read them (`state().clips`).
    pub fn script_clips(&self) -> Vec<serde_json::Value> {
        self.timeline
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
                    "fadeIn": c.fade_in,
                    "fadeOut": c.fade_out,
                    "duck": c.duck,
                    "cleanVoice": c.clean_voice,
                    "smoothFades": c.smooth_fades,
                    "embeddedAudio": c.embedded_audio,
                    "linkedClip": c.linked_clip,
                    "titleStyle": c.title_style.as_str(),
                })
            })
            .collect()
    }

    pub fn script_state_json(&self) -> String {
        let clips = self.script_clips();
        let assets: Vec<serde_json::Value> = self
            .assets
            .iter()
            .map(|a| {
                let peaks_per_sec = if a.info.duration > 0.0 && !a.peaks.is_empty() {
                    openreel_media::WAVEFORM_PEAKS_PER_SEC as f64
                } else {
                    0.0
                };
                let silence_ranges: Vec<serde_json::Value> = if peaks_per_sec > 0.0 {
                    let threshold = openreel_media::silence_threshold(&a.peaks);
                    openreel_media::silence_ranges(&a.peaks, peaks_per_sec, threshold, 0.35)
                        .iter()
                        .map(|range| {
                            serde_json::json!({
                                "start": range.start,
                                "end": range.end,
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                serde_json::json!({
                    "name": a.info.name,
                    "path": a.info.path,
                    "duration": a.info.duration,
                    "width": a.info.width,
                    "height": a.info.height,
                    "hasVideo": a.info.has_video,
                    "hasAudio": a.info.has_audio,
                    "orientation": Orientation::of(a.info.width, a.info.height).as_str(),
                    "ready": a.state == AssetState::Ready,
                    "waveformPeaksPerSec": peaks_per_sec,
                    "silenceRanges": silence_ranges,
                    "tempo": a.beats.as_ref().map(|grid| (grid.bpm * 10.0).round() / 10.0),
                    "utterances": sweep::utterances(&a.sweep.transcript)
                        .into_iter()
                        .map(|u| serde_json::json!({"start": u.start, "end": u.end, "text": u.text}))
                        .collect::<Vec<_>>(),
                    "beats": a.beats.as_ref().map(|grid| {
                        grid.beats
                            .iter()
                            .map(|t| (t * 1000.0).round() / 1000.0)
                            .collect::<Vec<_>>()
                    }),
                })
            })
            .collect();
        let sweep_candidate_json = |candidate: &sweep::SweepCandidate| {
            serde_json::json!({
                    "id": candidate.id,
                    "asset": candidate.asset_path,
                    "assetName": candidate.asset_name,
                    "title": candidate.title,
                    "sourceIn": candidate.source_in,
                    "duration": candidate.duration,
                    "score": candidate.score,
                    "reason": candidate.reason,
                    "stability": candidate.stability,
                    "audioEnergy": candidate.audio_energy,
                    "audioPeak": candidate.audio_peak,
                    "musicBed": candidate.music_bed,
                    "isReaction": candidate.is_reaction(),
                    "reactionScore": candidate.reaction_score(),
                    "category": candidate.category,
                    "tags": candidate.tags,
                    "quality": {
                        "stability": candidate.stability,
                        "audio": candidate.audio_score,
                        "motion": candidate.motion_score,
                    },
                    "hasVideo": candidate.has_video,
                    "transcript": candidate.transcript_text(),
                    "talkingScore": candidate.talking_score(),
                    "visual": candidate.visual_json(),
                    "review": candidate.review_json(),
            })
        };
        let mut sweep_candidates: Vec<serde_json::Value> = self
            .assets
            .iter()
            .flat_map(|asset| asset.sweep.candidates.iter())
            .filter(|candidate| !candidate.dismissed)
            .map(&sweep_candidate_json)
            .collect();
        let scene_clips: Vec<serde_json::Value> = self
            .assets
            .iter()
            .flat_map(|asset| asset.sweep.scene_clips.iter())
            .map(&sweep_candidate_json)
            .collect();
        sweep_candidates.sort_by(|a, b| {
            b["score"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&a["score"].as_f64().unwrap_or(0.0))
        });
        // Transcript segments are normally turn-level; provider
        // word timings are preserved when present. IDs match the
        // pinned digest so requests like "the part where he talks
        // about pricing" resolve against real ranges.
        let transcript_segments: Vec<serde_json::Value> = self
            .assets
            .iter()
            .flat_map(|asset| {
                asset
                    .sweep
                    .transcript
                    .iter()
                    .enumerate()
                    .map(|(index, segment)| {
                        serde_json::json!({
                            "id": sweep::transcript_segment_id(&asset.info.path, index),
                            "asset": asset.info.path,
                            "assetName": asset.info.name,
                            "start": segment.start,
                            "end": segment.end,
                            "speaker": segment.speaker,
                            "text": segment.text,
                            "timingPrecision": segment.timing_precision(),
                            "asrSession": segment.session_json(),
                            "words": segment.words.iter().map(|word| {
                                serde_json::json!({
                                    "start": word.start,
                                    "end": word.end,
                                    "text": word.text,
                                    "confidence": word.confidence,
                                    "estimated": word.estimated,
                                })
                            }).collect::<Vec<_>>(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        serde_json::json!({
            "playhead": self.playhead,
            "playing": self.playing,
            "selected": self.selected,
            "duration": self.timeline.end(),
            "fps": self.timeline.fps,
            "magnet": self.viewport.magnet,
            "musicBedMuted": self.mute_music_beds,
            "format": self.timeline.orientation.as_str(),
            "clips": clips,
            "assets": assets,
            "sweepCandidates": sweep_candidates,
            "sceneClips": scene_clips,
            "transcriptSegments": transcript_segments,
            "inspection": inspect::inspect(&self.timeline, &self.assets),
            "audio": self.audio_report(),
        })
        .to_string()
    }

    /// Apply a script's commands as one edit: every command applies, or the
    /// editor is restored (timeline, selection, transport, magnet, zoom and
    /// music-bed policy). Files go through import and export, never a
    /// script. Returns the notes and each handle's new clip ID.
    pub fn apply_edit(
        &mut self,
        commands: Vec<ScriptCommand>,
    ) -> Result<(Vec<String>, HashMap<String, ClipId>), String> {
        for command in &commands {
            if matches!(command, ScriptCommand::Import(_) | ScriptCommand::Export(_)) {
                return Err("use import and export for files, not a script".to_owned());
            }
            command.validate()?;
        }
        let before = self.edit_snapshot();
        let overlaps_before = self.timeline.lane_overlaps();
        let started = Instant::now();
        let mut created = HashMap::new();
        let result = (|| {
            let mut notes = Vec::new();
            for command in commands {
                notes.extend(self.apply_script_command(command, &mut created)?);
                // Finite input can still overflow frame-grid arithmetic, or
                // send a clip days away.
                if !self.playhead.is_finite() || !self.timeline.in_supported_range() {
                    return Err("edit exceeds the supported timeline range".to_owned());
                }
                // Cooperative bound: individual native commands are not
                // preemptible, but a long finite batch cannot run unchecked.
                if started.elapsed() > Duration::from_millis(250) {
                    return Err("edit took too long; try a smaller batch".to_owned());
                }
            }
            // Checked once the whole edit has run, so commands may pass
            // through an overlap on the way to a valid timeline.
            if let Some(&(a, b)) = self
                .timeline
                .lane_overlaps()
                .iter()
                .find(|pair| !overlaps_before.contains(pair))
            {
                let describe = |id: ClipId| match self.timeline.clip(id) {
                    Some(clip) => format!("clip {id} ({})", clip.name),
                    None => format!("clip {id}"),
                };
                let lane = self.timeline.clip(a).map_or(0, |clip| clip.track);
                return Err(format!(
                    "{} and {} would overlap on lane {lane}; trim one, or move it in time or to another lane with setClipLane",
                    describe(a),
                    describe(b)
                ));
            }
            Ok(notes)
        })();
        let changed = self.timeline != before.timeline;
        match result {
            Ok(notes) => {
                if changed {
                    self.stop_sources();
                    self.forget_stills();
                }
                let handles = created
                    .into_iter()
                    .map(|(handle, clip)| (handle.to_string(), clip))
                    .collect();
                Ok((notes, handles))
            }
            Err(error) => {
                self.restore_edit(before);
                Err(error)
            }
        }
    }

    /// Everything a script's commands can change, to put back with
    /// `restore_edit`.
    pub fn edit_snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            timeline: self.timeline.clone(),
            selected: self.selected,
            playhead: self.playhead,
            playing: self.playing,
            magnet: self.viewport.magnet,
            zoom: self.viewport.px_per_sec,
            mute_music_beds: self.mute_music_beds,
        }
    }

    /// The project file would differ from one saved at `before`.
    pub fn differs_from(&self, before: &EditSnapshot) -> bool {
        self.timeline != before.timeline || self.mute_music_beds != before.mute_music_beds
    }

    /// Undo an edit that must leave no trace: a failed one, or a dry run.
    pub fn restore_edit(&mut self, before: EditSnapshot) {
        // An edit refused before it changed anything leaves playback alone.
        let disturbed = self.timeline != before.timeline
            || self.playhead != before.playhead
            || self.playing != before.playing;
        self.timeline = before.timeline;
        self.selected = before.selected;
        self.playhead = before.playhead;
        self.playing = before.playing;
        self.viewport.magnet = before.magnet;
        self.viewport.px_per_sec = before.zoom;
        self.mute_music_beds = before.mute_music_beds;
        if disturbed && !self.rehearsing {
            self.stop_sources();
            self.last_now = None;
            self.forget_stills();
        }
    }

    pub fn apply_script_commands(&mut self, commands: Vec<ScriptCommand>) -> Vec<String> {
        let mut notes = Vec::new();
        let mut created = HashMap::new();
        for command in commands {
            match self.apply_script_command(command, &mut created) {
                Ok(Some(note)) => notes.push(note),
                Ok(None) => {}
                Err(error) => notes.push(format!("error: {error}")),
            }
        }
        notes
    }

    /// One executor for console commands and transactional generated edits.
    /// Failures are typed; a console log is never an application error.
    /// `created` maps this batch's addition handles to the clips they made.
    pub fn apply_script_command(
        &mut self,
        mut command: ScriptCommand,
        created: &mut HashMap<u64, ClipId>,
    ) -> Result<Option<String>, String> {
        command.validate()?;
        command.resolve_handle(created)?;
        if let Some(&mut id) = command.target_mut() {
            if self.timeline.clip(id).is_none() {
                return Err(format!("no clip {id}"));
            }
        }
        match command {
            ScriptCommand::Log(msg) => return Ok(Some(msg)),
            ScriptCommand::Play => self.set_playing(true),
            ScriptCommand::Pause => self.set_playing(false),
            ScriptCommand::TogglePlay => self.set_playing(!self.playing),
            ScriptCommand::Seek(t) => self.seek(t),
            ScriptCommand::StepFrames(n) => {
                self.seek(self.playhead + n as f64 / self.timeline.fps);
            }
            ScriptCommand::SelectClip(id) => self.selected = Some(id),
            ScriptCommand::SplitClip { clip, at, handle } => {
                let right = self
                    .timeline
                    .split_at(clip, at)
                    .ok_or_else(|| format!("cannot split clip {clip} at {at}"))?;
                self.selected = Some(right);
                created.extend(handle.map(|handle| (handle, right)));
            }
            ScriptCommand::DeleteClip(id) => {
                self.timeline.remove_clip(id);
                if self.selected == Some(id) {
                    self.selected = None;
                }
            }
            ScriptCommand::MoveClip { clip, start } => {
                from_zero("moveClip", "start", start)?;
                self.timeline.move_clip(clip, start);
            }
            ScriptCommand::TrimClip { clip, left, right } => {
                if let Some(c) = self.timeline.clip(clip) {
                    let (start, end) = (left.unwrap_or(c.start), right.unwrap_or(c.end()));
                    if end <= start {
                        return Err(format!(
                            "trimClip would end clip {clip} at {end} s, not after its start at \
                             {start} s: left is the new start and right the new end"
                        ));
                    }
                }
                // A request takes the nearest frame; the model then rounds
                // one way only where a bound (the media's end) is off the grid.
                if let Some(left) = left {
                    self.timeline.trim_left(clip, self.timeline.snap(left));
                }
                if let Some(right) = right {
                    let mut end = self.timeline.snap(right);
                    if let Some(c) = self.timeline.clip(clip) {
                        if let Some(asset) = c.source.as_ref().and_then(|s| self.asset(s)) {
                            end = end.min(c.start + (asset.info.duration - c.source_in).max(0.0));
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
                    c.volume = value.clamp(0.0, details::MAX_VOLUME);
                }
            }
            ScriptCommand::SetFade {
                clip,
                fade_in,
                fade_out,
            } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.fade_in = fade_in.max(0.0);
                    c.fade_out = fade_out.max(0.0);
                }
            }
            ScriptCommand::DetachAudio { clip } => {
                let has_audio = self
                    .timeline
                    .clip(clip)
                    .and_then(|c| c.source.as_ref())
                    .and_then(|source| self.asset(source))
                    .map(|asset| asset.info.has_audio)
                    .unwrap_or(false);
                if !has_audio {
                    return Err(format!("clip {clip} has no detachable audio"));
                }
                self.selected = Some(
                    self.timeline
                        .detach_audio(clip)
                        .ok_or_else(|| format!("clip {clip} audio is already detached"))?,
                );
            }
            ScriptCommand::SetClipLane { clip, lane } => {
                if self.timeline.clip(clip).is_none() || lane > 64 {
                    return Err(format!("cannot move clip {clip} to lane {lane}"));
                }
                self.timeline.set_clip_track(clip, lane as usize);
            }
            ScriptCommand::UnlinkClip { clip } => {
                if !self.timeline.unlink_clip(clip) {
                    return Err(format!("clip {clip} is not linked"));
                }
            }
            ScriptCommand::SetEmbeddedAudio { clip, enabled } => {
                let source_has_audio = !enabled
                    || self
                        .timeline
                        .clip(clip)
                        .and_then(|c| c.source.as_ref())
                        .and_then(|source| self.asset(source))
                        .map(|asset| asset.info.has_audio)
                        .unwrap_or(false);
                if !source_has_audio || !self.timeline.set_embedded_audio(clip, enabled) {
                    return Err(format!(
                        "cannot {} embedded audio on clip {clip}",
                        if enabled { "enable" } else { "disable" }
                    ));
                }
            }
            ScriptCommand::TightenSilence {
                clip,
                threshold,
                min_silence,
                padding,
            } => {
                self.tighten_clip_silence(clip, threshold, min_silence, padding)?;
            }
            ScriptCommand::AddTitle {
                text,
                at,
                duration,
                style,
                handle,
            } => {
                from_zero("addTitle", "time", at)?;
                if duration <= 0.0 {
                    return Err(format!("addTitle needs a positive length, not {duration}"));
                }
                let style = title_style(&style)?;
                let track = self.first_free_track(at, duration);
                let id = self
                    .timeline
                    .add_clip(&text, ClipKind::Title, track, at, duration, 0.0);
                if let Some(clip) = self.timeline.clip_mut(id) {
                    clip.title_style = style;
                }
                self.selected = Some(id);
                created.extend(handle.map(|handle| (handle, id)));
            }
            ScriptCommand::SetTitleStyle { clip, style } => {
                let style = title_style(&style)?;
                if let Some(target) = self.timeline.clip_mut(clip) {
                    if target.kind != ClipKind::Title {
                        return Err(format!("clip {clip} is not a title"));
                    }
                    target.title_style = style;
                }
            }
            ScriptCommand::AddCaptionsForClip { clip, style } => {
                let style = title_style(&style)?;
                return Ok(Some(self.caption_clip(clip, style)?.note(clip)));
            }
            ScriptCommand::AddAsset { asset, at, handle } => {
                from_zero("addAsset", "time", at)?;
                let (path, name, duration) = self
                    .asset_named(&asset)
                    .map(|a| (a.info.path.clone(), a.info.name.clone(), a.info.duration))?;
                let id =
                    self.try_add_asset_range_to_timeline(&path, &name, None, at, 0.0, duration)?;
                created.extend(handle.map(|handle| (handle, id)));
            }
            ScriptCommand::AddAssetRange {
                asset,
                at,
                source_in,
                duration,
                handle,
            } => {
                from_zero("addAssetRange", "time", at)?;
                from_zero("addAssetRange", "source start", source_in)?;
                if duration <= 0.0 {
                    return Err(format!(
                        "addAssetRange needs a positive length, not {duration}"
                    ));
                }
                let (path, name) = self
                    .asset_named(&asset)
                    .map(|a| (a.info.path.clone(), a.info.name.clone()))?;
                let id = self
                    .try_add_asset_range_to_timeline(&path, &name, None, at, source_in, duration)?;
                created.extend(handle.map(|handle| (handle, id)));
            }
            ScriptCommand::Import(path) => self.spawn_import(vec![PathBuf::from(path)]),
            ScriptCommand::Export(path) => self.start_export(PathBuf::from(path)),
            ScriptCommand::ClearTimeline => self.clear_timeline(),
            ScriptCommand::SetMagnet(on) => self.viewport.magnet = on,
            ScriptCommand::SetMusicBedMuted(on) => self.mute_music_beds = on,
            ScriptCommand::SetZoom(px) => self.viewport.set_zoom(px, self.timeline.end()),
            ScriptCommand::CloseGaps => {
                self.timeline.close_gaps();
            }
            ScriptCommand::SetDuck { clip, enabled } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.duck = enabled;
                }
            }
            ScriptCommand::SetCleanVoice { clip, enabled } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.clean_voice = enabled;
                }
            }
            ScriptCommand::SetSmoothFades { clip, enabled } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.smooth_fades = enabled;
                }
            }
            ScriptCommand::InsertTime { at, seconds } => {
                from_zero("insertTime", "time", at)?;
                if seconds <= 0.0 {
                    return Err(format!("insertTime needs a positive length, not {seconds}"));
                }
                self.timeline.insert_time(at, seconds);
            }
        }
        Ok(None)
    }

    pub fn run_console(&mut self, code: String) {
        let code = code.trim().to_owned();
        if code.is_empty() {
            return;
        }
        self.console_output.push(format!("> {code}"));
        if self.console_history.last().map(String::as_str) != Some(code.as_str()) {
            self.console_history.push(code.clone());
        }
        self.console_history_idx = None;

        let state = self.script_state_json();
        let Some(engine) = self.script_engine.as_mut() else {
            self.console_output
                .push("error: scripting unavailable".to_owned());
            return;
        };
        let started = Instant::now();
        let (output, commands) = engine.eval(&code, &state);
        let elapsed_ms = started.elapsed().as_secs_f32() * 1000.0;
        for note in self.apply_script_commands(commands) {
            self.console_output.push(note);
        }
        if output != "undefined" {
            for line in output.lines() {
                self.console_output.push(line.to_owned());
            }
        }
        self.status = format!("JS {elapsed_ms:.2} ms");
        if self.console_output.len() > 300 {
            let excess = self.console_output.len() - 300;
            self.console_output.drain(..excess);
        }
    }

    /// Up (`older`) or Down in the console's input: the line it should now
    /// hold. Up stops at the oldest line; Down past the newest gives the
    /// empty line back. `None` when Up has nothing to recall.
    pub fn recall_console(&mut self, older: bool) -> Option<String> {
        let index = if older {
            Some(match self.console_history_idx {
                Some(index) => index.saturating_sub(1),
                None => self.console_history.len().checked_sub(1)?,
            })
        } else {
            self.console_history_idx
                .map(|index| index + 1)
                .filter(|index| *index < self.console_history.len())
        };
        self.console_history_idx = index;
        Some(index.map_or_else(String::new, |index| self.console_history[index].clone()))
    }

    pub fn advance_playback(&mut self, now: f64) {
        let dt = self.last_now.map_or(0.0, |last| now - last);
        self.last_now = Some(now);
        if self.playing {
            // The audio clock is the transport. It may run to the next cut
            // whose picture is not ready, so a late UI tick cannot jump over
            // a short clip and audio is never heard ahead of its picture.
            let limit = self.clock_limit();
            let playhead = self.playhead;
            let moved = self.clock_playhead != Some(playhead);
            let audio = self.audio_engine();
            if moved {
                audio.seek(playhead);
            }
            audio.set_limit(limit);
            audio.set_running(true);
            // Only the manual (test) output renders here; devices pace themselves.
            audio.advance(dt.max(0.0));
            let clock = audio.clock().min(limit);
            // The playhead still lands on each cut for one tick, so the clip
            // is requested at its first picture, which may be prerolled.
            let cut = self
                .next_video_change(playhead)
                .map_or(f64::INFINITY, |(edge, _)| edge);
            self.playhead = playhead.max(clock.min(cut));
            self.clock_playhead = Some(self.playhead);
            if !self.scrubbing() {
                self.viewport.follow(playhead, self.playhead);
            }
            if self.playhead >= self.timeline.end() {
                self.playhead = self.timeline.end();
                self.set_playing(false);
            } else {
                self.wake.wake();
            }
        } else if self.shuttle != 0.0 {
            // A hand scrub takes the playhead back.
            if self.viewport.interacting || self.transport_scrubbing {
                self.shuttle = 0.0;
                return;
            }
            let end = self.timeline.end();
            // A UI hitch must not fling the playhead.
            let next = self.playhead + self.shuttle * dt.clamp(0.0, 0.1);
            self.playhead = next.clamp(0.0, end);
            self.viewport.reveal(self.playhead);
            if next <= 0.0 || next >= end {
                self.shuttle = 0.0;
                self.status = "Paused.".to_owned();
            } else {
                self.wake.wake();
            }
        }
    }

    /// Start a frame at `now` seconds on the front end's clock: move the
    /// transport and take in whatever the workers finished.
    pub fn begin_frame(&mut self, now: f64) -> FrameEvents {
        self.repaint_after = None;
        self.advance_playback(now);
        self.poll_imports();
        self.poll_frames();
        if openreel_media::take_hardware_failure().is_some() {
            self.status = "Hardware decode failed; playing on the CPU.".to_owned();
        }
        let exported = self.poll_export();
        self.poll_sweeps();
        if !self.initial_imports.is_empty() && self.import_rx.is_none() {
            let paths = std::mem::take(&mut self.initial_imports);
            self.open_launch_paths(paths);
        }
        self.serve_requests();
        FrameEvents { exported }
    }

    /// Open what the command line named: the first `.eddie` file as the
    /// project, everything else as media.
    fn open_launch_paths(&mut self, paths: Vec<PathBuf>) {
        let (projects, media): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case(project::EXTENSION))
        });
        if let Some(project) = projects.first() {
            if let Err(error) = self.open_project(project) {
                self.status = error;
            }
        }
        if !media.is_empty() {
            self.spawn_import(media);
        }
    }

    /// Finish the frame after this frame's input and `sync_playback`: a
    /// settled edit becomes an undo step and the preview loudness follows
    /// the mix. `pointer_down` and `typing` hold an edit open while a drag
    /// or a text field is still changing it.
    pub fn end_frame(&mut self, pointer_down: bool, typing: bool) {
        self.pointer_held = pointer_down;
        self.poll_loudness(pointer_down);
        let now = self.last_now.unwrap_or(0.0);
        if self
            .history
            .track(&self.timeline, self.selected, now, pointer_down, typing)
        {
            self.frame_within(Duration::from_millis(250));
        }
        if self.import_rx.is_some() || self.export_rx.is_some() {
            self.frame_within(Duration::from_millis(120));
        }
    }

    /// Ask for a frame no later than `after`, without a wake.
    pub(crate) fn frame_within(&mut self, after: Duration) {
        self.repaint_after = Some(
            self.repaint_after
                .map_or(after, |soonest| soonest.min(after)),
        );
    }

    /// How soon the editor wants another frame even if nothing wakes it.
    pub fn repaint_after(&self) -> Option<Duration> {
        self.repaint_after
    }

    /// The clock of the frame in progress.
    pub fn now(&self) -> f64 {
        self.last_now.unwrap_or(0.0)
    }

    pub fn exporting(&self) -> bool {
        self.export_rx.is_some()
    }

    pub fn importing(&self) -> bool {
        self.import_rx.is_some()
    }

    pub fn curating(&self) -> bool {
        self.sweep_rx.is_some()
    }
}

#[cfg(test)]
mod tests;

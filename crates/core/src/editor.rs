//! The editor without a window: the timeline, the media bin, playback,
//! scripting, curation, projects and export. A front end draws it, feeds it
//! input and time, and owns the pixels; the command line drives it bare.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use openreel_engine::{Clip, ClipId, ClipKind, Timeline, TitleStyle};
use openreel_media::{AudioEngine, AudioSegment, Loudness, RawFrame, StillDecoder};

use crate::scripting::ScriptEngine;
use export::ExportMsg;

mod audio;
mod captions;
pub mod commands;
pub mod details;
mod edits;
mod export;
pub mod gallery;
mod headless;
pub mod history;
mod incoming;
pub mod inspect;
pub mod keys;
pub mod lanes;
mod markers;
pub mod media;
pub mod pictures;
mod playback;
mod program;
pub mod project;
mod script;
#[cfg(unix)]
pub mod server;
#[cfg(not(unix))]
#[path = "editor/server_stub.rs"]
pub mod server;
pub mod settings;
mod silence;
mod stills;
use stills::StillUse;
pub mod sweep;
mod untitled;
pub mod viewport;
pub mod voiceover;

pub use headless::cli_main;
pub use program::ProgramLayer;
pub use script::EditSnapshot;

use gallery::CuratorGallery;
use media::{BinAsset, ImportMsg};
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
    "mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "flac", "m4a", "aac", "ogg", "png",
    "jpg", "jpeg", "webp", "heic", "heif",
];

// Clip identity, source, and exact timeline/source starts. Editing a clip's
// source mapping invalidates playback even when its ID has not changed.
type VideoKey = (ClipId, String, u64, u64, u64);

/// A still picture: clip, source frame index, and decoded width.
pub type StillKey = (ClipId, i64, u32);

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
    /// The picture of video lane `lane` ([`Editor::program_layers`]).
    fn show(&mut self, lane: usize, frame: &RawFrame);
    /// Lane `lane` no longer holds a picture of the clip it shows.
    fn clear(&mut self, lane: usize);
    /// The picture of the clip a transition on lane `lane` goes to.
    fn show_incoming(&mut self, _lane: usize, _frame: &RawFrame) {}
}

/// The monitor of an editor nothing draws.
pub struct NoPreview;

impl PreviewSink for NoPreview {
    fn show(&mut self, _lane: usize, _frame: &RawFrame) {}
    fn clear(&mut self, _lane: usize) {}
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
    RippleDelete,
    Copy,
    Cut,
    Paste,
    Duplicate,
    FreezeFrame,
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
    RecordVoiceover,
    AddMarker,
    AddMarkerNewColor,
    NextMarker,
    PrevMarker,
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
    /// What Copy or Cut took: a clip and its linked partner.
    clipboard: Vec<Clip>,

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
    /// Playback start, so the rate is judged only over a full second.
    playing_since: Option<Instant>,
    still_gen: u64,
    /// The one still request being decoded; `None` key discards its answer.
    /// A prefetch only shows if the playhead has since moved onto it.
    still_inflight: Option<(u64, Option<StillKey>, StillUse)>,
    /// Recently decoded stills, least recently used first.
    still_cache: VecDeque<(StillKey, RawFrame)>,
    /// Stills that failed to decode and when, so a file that cannot be read
    /// is not asked for again every frame.
    still_failed: VecDeque<(StillKey, Instant)>,

    /// Each video lane's picture, by lane (`program`).
    players: Vec<program::Player>,
    /// A picture failed to decode; playback paused on it.
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
    /// Where a window without a project makes an untitled one (`untitled`).
    untitled_dir: Option<PathBuf>,
    /// The untitled project this editor made, while it has it.
    untitled: Option<PathBuf>,

    /// The microphone while the Voiceover panel is open or a take runs,
    /// and takes waiting for their import to be placed.
    voiceover: Option<voiceover::Voiceover>,
    pending_takes: Vec<voiceover::PendingTake>,

    wake: Wake,
    /// The soonest a job in progress wants another frame without a wake.
    repaint_after: Option<Duration>,
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
        editor.untitled_dir = untitled::default_dir();
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
        let timeline = Timeline::empty_project().with_format(settings.format);
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
            clipboard: Vec::new(),
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
            playing_since: None,
            still_gen: 0,
            still_inflight: None,
            still_cache: VecDeque::new(),
            still_failed: VecDeque::new(),
            players: Vec::new(),
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
            untitled_dir: None,
            untitled: None,
            voiceover: None,
            pending_takes: Vec::new(),
            wake,
            repaint_after: None,
        }
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
        self.settings.format = self.timeline.format;
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
            Action::RippleDelete => self.ripple_delete_selected(),
            Action::Copy => self.copy_selected(),
            Action::Cut => self.cut_selected(),
            Action::Paste => self.paste(),
            Action::Duplicate => self.duplicate_selected(),
            Action::FreezeFrame => self.freeze_at_playhead(),
            Action::AddTitle => {
                let track =
                    self.timeline
                        .free_lane(ClipKind::Title, self.playhead, self.playhead + 4.0);
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
            // While a take counts down or records, Space and Esc stop it.
            Action::RecordVoiceover => self.toggle_voiceover(),
            Action::PlayPause | Action::Deselect if self.voiceover_busy() => self.stop_voiceover(),
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
            Action::AddMarker
            | Action::AddMarkerNewColor
            | Action::NextMarker
            | Action::PrevMarker => self.marker_action(action),
            Action::Deselect => self.selected = None,
        }
        true
    }

    /// Empty the timeline and keep the bin, the project's format and its
    /// markers, which say where things should go.
    pub fn clear_timeline(&mut self) {
        self.set_playing(false);
        let mut cleared = Timeline::empty_project().with_format(self.timeline.format);
        cleared.markers = std::mem::take(&mut self.timeline.markers);
        cleared.ensure_fresh_ids();
        self.timeline = cleared;
        self.selected = None;
        self.playhead = 0.0;
        self.forget_stills();
    }

    // -- scripting ---------------------------------------------------------------

    /// Start a frame at `now` seconds on the front end's clock: move the
    /// transport and take in whatever the workers finished.
    pub fn begin_frame(&mut self, now: f64) -> FrameEvents {
        self.repaint_after = None;
        let dt = self.last_now.map_or(0.0, |last| (now - last).max(0.0));
        self.advance_playback(now);
        self.poll_voiceover(now, dt);
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
        let (project, media) = project::split_opened(paths);
        if let Some(project) = project {
            if let Err(error) = self.open_project(&project) {
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

    pub fn importing(&self) -> bool {
        self.import_rx.is_some()
    }

    pub fn curating(&self) -> bool {
        self.sweep_rx.is_some()
    }
}

#[cfg(test)]
mod tests;

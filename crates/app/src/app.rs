use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Instant;

use eframe::egui::{
    self, ColorImage, CornerRadius, RichText, Sense, Shape, Stroke, TextureHandle, TextureOptions,
    Ui, Vec2,
};
use openreel_engine::{format_timecode, ClipId, ClipKind, Orientation, Timeline, TitleStyle};
use openreel_media::{AudioEngine, AudioSegment, FrameStream, MediaInfo, RawFrame, StillDecoder};

use crate::preview;
use crate::scripting::{ScriptCommand, ScriptEngine};
use crate::theme;

pub(crate) mod chat;
mod gallery;
mod history;
pub(crate) mod intents;
mod media;
mod project;
#[cfg(test)]
mod screenshot;
pub(crate) mod session;
mod sweep;
use crate::timeline_ui::{ClipVisual, TimelineView, VisualMap};
use chat::{ChatSettings, ChatWorkerResult, PendingChat, TimelineSnapshot};
use gallery::CuratorGallery;
use media::{AssetState, BinAsset, ImportMsg};
use session::ChatSession;
use sweep::SweepMsg;

const PREVIEW_W: u32 = 960;
const SCRUB_W: u32 = 480;
const STILL_CACHE_CAP: usize = 24;
const MEDIA_EXTS: &[&str] = &[
    "mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "flac", "m4a", "aac", "ogg",
];

// Clip identity, source, and exact timeline/source starts. Editing a clip's
// source mapping invalidates playback even when its ID has not changed.
type VideoKey = (ClipId, String, u64, u64);
/// A still picture: clip, source frame index, and decoded width.
type StillKey = (ClipId, i64, u32);

enum ExportMsg {
    Progress(f64),
    Done(Result<PathBuf, String>),
}

#[derive(Clone, Copy)]
enum UiAction {
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
    GoStart,
    GoEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RightTab {
    Chat,
    Details,
}

pub struct EditorApp {
    timeline: Timeline,
    timeline_view: TimelineView,
    playhead: f64,
    playing: bool,
    last_now: Option<f64>,
    selected: Option<ClipId>,

    assets: Vec<BinAsset>,
    selected_asset: Option<String>,
    /// User-selected add policy: ranges from baked-in music beds
    /// start silent when true, keep their score when false.
    mute_music_beds: bool,
    curator: CuratorGallery,
    caption_style: TitleStyle,
    search: String,
    status: String,

    import_rx: Option<Receiver<ImportMsg>>,
    import_queue: Vec<PathBuf>,
    sweep_rx: Option<Receiver<SweepMsg>>,
    sweep_queue: Vec<String>,
    /// Shared by every running curation worker; dropped when none run.
    sweep_tx: Option<Sender<SweepMsg>>,
    sweep_running: Vec<String>,
    initial_imports: Vec<PathBuf>,

    still_decoder: StillDecoder,
    still_rx: Receiver<(u64, Result<RawFrame, String>)>,
    preview_texture: Option<TextureHandle>,
    preview_key: Option<VideoKey>,
    still_gen: u64,
    /// The one still request being decoded; `None` key discards its answer.
    still_inflight: Option<(u64, Option<StillKey>)>,
    /// The still the preview texture shows, if it shows one.
    still_shown: Option<StillKey>,
    /// Recently decoded stills, least recently used first.
    still_cache: VecDeque<(StillKey, RawFrame)>,

    stream: Option<FrameStream>,
    stream_key: Option<VideoKey>,
    stream_generation: u64,
    stream_ready: bool,
    stream_failed: bool,
    /// Mixer and master playback clock, opened on first play.
    audio: Option<AudioEngine>,
    audio_failed: bool,
    /// The playhead the audio clock last produced; any other value means
    /// something moved the playhead and the clock must follow it.
    clock_playhead: Option<f64>,
    last_sync_playhead: f64,
    last_sync_time: f64,
    /// The monitor's scrubber is held; previews like a ruler scrub.
    transport_scrubbing: bool,

    export_rx: Option<Receiver<ExportMsg>>,
    export_progress: Option<f64>,

    show_media: bool,
    show_inspector: bool,
    show_console: bool,

    script_engine: Option<ScriptEngine>,
    console_input: String,
    console_output: Vec<String>,
    console_history: Vec<String>,
    console_history_idx: Option<usize>,

    chat_settings: ChatSettings,
    connection_check: Option<chat::PendingConnectionCheck>,
    /// Last connection check for the current settings, shown in the dialog.
    connection_result: Option<Result<(), String>>,
    chat_session: ChatSession,
    persist_session: bool,
    /// Saved curations; `None` keeps tests away from the user's config.
    curation_dir: Option<PathBuf>,
    chat_input: String,
    chat_rx: Option<Receiver<(u64, ChatWorkerResult)>>,
    chat_pending: Option<PendingChat>,
    chat_generation: u64,
    chat_undo: Option<TimelineSnapshot>,
    history: history::History,
    /// Where Save writes; set by Save as or Open.
    project_path: Option<PathBuf>,
    /// The timeline as last saved or opened; a difference is unsaved.
    saved_timeline: Timeline,
    /// Project media not found on open, in the order Locate asks for it.
    missing_media: Vec<String>,
    pending_discard: Option<project::Discard>,
    /// Set once the user chose to close without (or after) saving.
    allow_close: bool,
    show_chat: bool,
    right_tab: RightTab,
    prev_selected: Option<ClipId>,
    show_chat_settings: bool,
    show_chat_debug: bool,
    last_generated_js: String,

    frame_ms_ema: f32,
    rss_mb: u64,
    frame_count: u64,
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial_imports: Vec<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let wake_ctx = cc.egui_ctx.clone();
        let (still_decoder, still_rx) = StillDecoder::spawn(move || wake_ctx.request_repaint());
        let (script_engine, console_output) = match ScriptEngine::new() {
            Ok(engine) => (Some(engine), vec!["Eddie JS. help()".to_owned()]),
            Err(e) => (None, vec![format!("Scripting unavailable: {e}")]),
        };
        let chat_settings = ChatSettings::load();
        let timeline = Timeline::empty_project().with_orientation(chat_settings.format);
        let saved_timeline = timeline.clone();
        Self {
            history: history::History::new(&timeline),
            timeline,
            timeline_view: TimelineView::default(),
            playhead: 0.0,
            playing: false,
            last_now: None,
            selected: None,
            assets: Vec::new(),
            selected_asset: None,
            mute_music_beds: true,
            curator: CuratorGallery::default(),
            caption_style: TitleStyle::Caption,
            search: String::new(),
            status: "Ready.".to_owned(),
            import_rx: None,
            import_queue: Vec::new(),
            sweep_rx: None,
            sweep_queue: Vec::new(),
            sweep_tx: None,
            sweep_running: Vec::new(),
            initial_imports,
            still_decoder,
            still_rx,
            preview_texture: None,
            preview_key: None,
            still_gen: 0,
            still_inflight: None,
            still_shown: None,
            still_cache: VecDeque::new(),
            stream: None,
            stream_key: None,
            stream_generation: 0,
            stream_ready: false,
            stream_failed: false,
            audio: None,
            audio_failed: false,
            clock_playhead: None,
            last_sync_playhead: 0.0,
            last_sync_time: 0.0,
            transport_scrubbing: false,
            export_rx: None,
            export_progress: None,
            show_media: true,
            show_inspector: true,
            show_console: false,
            script_engine,
            console_input: String::new(),
            console_output,
            console_history: Vec::new(),
            console_history_idx: None,
            chat_settings,
            connection_check: None,
            connection_result: None,
            chat_session: ChatSession::load(),
            persist_session: true,
            curation_dir: sweep::default_curation_dir(),
            chat_input: String::new(),
            chat_rx: None,
            chat_pending: None,
            chat_generation: 0,
            chat_undo: None,
            project_path: None,
            saved_timeline,
            missing_media: Vec::new(),
            pending_discard: None,
            allow_close: false,
            show_chat: true,
            right_tab: RightTab::Chat,
            prev_selected: None,
            show_chat_settings: false,
            show_chat_debug: false,
            last_generated_js: String::new(),
            frame_ms_ema: 0.0,
            rss_mb: read_rss_mb(),
            frame_count: 0,
        }
    }

    /// Headless constructor for the screenshot test: same defaults
    /// as `new`, but without a window or persisted user config.
    #[cfg(test)]
    fn headless_for_test(ctx: &egui::Context) -> Self {
        theme::apply(ctx);
        let wake_ctx = ctx.clone();
        let (still_decoder, still_rx) = StillDecoder::spawn(move || wake_ctx.request_repaint());
        let (script_engine, console_output) = match ScriptEngine::new() {
            Ok(engine) => (Some(engine), vec!["Eddie JS. help()".to_owned()]),
            Err(e) => (None, vec![format!("Scripting unavailable: {e}")]),
        };
        Self {
            timeline: Timeline::empty_project(),
            timeline_view: TimelineView::default(),
            playhead: 0.0,
            playing: false,
            last_now: None,
            selected: None,
            assets: Vec::new(),
            selected_asset: None,
            mute_music_beds: true,
            curator: CuratorGallery::default(),
            caption_style: TitleStyle::Caption,
            search: String::new(),
            status: "Ready.".to_owned(),
            import_rx: None,
            import_queue: Vec::new(),
            sweep_rx: None,
            sweep_queue: Vec::new(),
            sweep_tx: None,
            sweep_running: Vec::new(),
            initial_imports: Vec::new(),
            still_decoder,
            still_rx,
            preview_texture: None,
            preview_key: None,
            still_gen: 0,
            still_inflight: None,
            still_shown: None,
            still_cache: VecDeque::new(),
            stream: None,
            stream_key: None,
            stream_generation: 0,
            stream_ready: false,
            stream_failed: false,
            audio: None,
            audio_failed: false,
            clock_playhead: None,
            last_sync_playhead: 0.0,
            last_sync_time: 0.0,
            transport_scrubbing: false,
            export_rx: None,
            export_progress: None,
            show_media: true,
            show_inspector: true,
            show_console: false,
            script_engine,
            console_input: String::new(),
            console_output,
            console_history: Vec::new(),
            console_history_idx: None,
            chat_settings: ChatSettings::default(),
            connection_check: None,
            connection_result: None,
            chat_session: ChatSession::new("screenshot"),
            persist_session: false,
            curation_dir: None,
            chat_input: String::new(),
            chat_rx: None,
            chat_pending: None,
            chat_generation: 0,
            chat_undo: None,
            history: history::History::new(&Timeline::empty_project()),
            project_path: None,
            saved_timeline: Timeline::empty_project(),
            missing_media: Vec::new(),
            pending_discard: None,
            allow_close: false,
            show_chat: true,
            right_tab: RightTab::Chat,
            prev_selected: None,
            show_chat_settings: false,
            show_chat_debug: false,
            last_generated_js: String::new(),
            frame_ms_ema: 0.0,
            rss_mb: read_rss_mb(),
            frame_count: 0,
        }
    }

    /// Split at the playhead: the selected clip when the playhead is
    /// inside it, otherwise the topmost clip under the playhead.
    /// Clicking the timeline parks the playhead first, so the flow
    /// is click, then split (button, S, or B).
    fn split_at_playhead(&mut self) {
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
    fn delete_clip(&mut self, id: ClipId) {
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
    fn tighten_clip_silence(
        &mut self,
        id: ClipId,
        threshold: f32,
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
            threshold,
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
    fn add_captions_for_clip(
        &mut self,
        id: ClipId,
        style: openreel_engine::TitleStyle,
    ) -> Result<usize, String> {
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
        let mut added = 0_usize;
        let mut last_added: Option<ClipId> = None;

        for segment in &transcript {
            for chunk in sweep::caption_chunks(segment) {
                if chunk.end <= source_start || chunk.start >= source_end {
                    continue;
                }
                let clipped_start = chunk.start.max(source_start);
                let clipped_end = chunk.end.min(source_end);
                if clipped_end <= clipped_start {
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
                    continue;
                }
                let duplicate = self.timeline.clips.iter().any(|clip| {
                    clip.kind == ClipKind::Title
                        && clip.name == chunk.text
                        && clip.title_style == style
                        && (clip.start - start).abs() <= frame / 2.0
                });
                if duplicate {
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
                }
                last_added = Some(caption);
                added += 1;
            }
        }
        if added == 0 && last_added.is_none() {
            // A second identical pass is a successful no-op; an
            // out-of-range transcript is also reported by the caller
            // through the zero count.
            return Ok(0);
        }
        if let Some(caption) = last_added {
            self.selected = Some(caption);
        }
        Ok(added)
    }

    fn stop_sources(&mut self) {
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

    fn audio_engine(&mut self) -> &mut AudioEngine {
        if self.audio.is_none() {
            // Tests render deterministically from their synthetic UI clock.
            let engine = if cfg!(test) {
                AudioEngine::manual()
            } else {
                AudioEngine::open()
            };
            eprintln!("audio output: {}", engine.description);
            if engine.description.starts_with("no audio device") {
                self.status = "No audio device; playing silently.".to_owned();
            }
            self.audio = Some(engine);
        }
        self.audio.as_mut().unwrap()
    }

    fn set_playing(&mut self, playing: bool) {
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
        if !playing {
            self.stop_sources();
            if self.status == "Buffering video." || self.status == "Playing." {
                self.status = "Paused.".to_owned();
            }
        }
    }

    fn seek(&mut self, time: f64) {
        self.playhead = self
            .timeline
            .snap(time.clamp(0.0, self.timeline.end().max(0.0)));
        // The current picture stays up until the new one arrives; a different
        // clip's picture is cleared in poll_frames.
        self.stop_sources();
    }

    /// Ruler, clip drag or monitor scrubber held: preview from low-res
    /// stills instead of restarting playback on every move.
    fn scrubbing(&self) -> bool {
        self.timeline_view.is_interacting() || self.transport_scrubbing
    }

    fn active_video_clip(&self) -> Option<openreel_engine::Clip> {
        self.timeline.active_video(self.playhead).cloned()
    }

    fn active_video_key(&self) -> Option<VideoKey> {
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
    fn next_video_change(&self, after: f64) -> Option<(f64, Option<(&str, f64)>)> {
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
    fn clock_limit(&self) -> f64 {
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

    fn waiting_for_video(&self) -> bool {
        self.playing
            && self
                .active_video_key()
                .is_some_and(|key| self.stream_key.as_ref() != Some(&key) || !self.stream_ready)
    }

    fn video_playback_failed(&mut self, error: String) {
        self.playing = false;
        self.stream_failed = true;
        if let Some(audio) = &self.audio {
            audio.set_running(false);
        }
        self.status = format!("Video playback failed: {error}");
    }

    fn update_preview_texture(&mut self, ctx: &egui::Context, frame: &RawFrame) {
        let image = ColorImage::from_rgba_unmultiplied(
            [frame.width as usize, frame.height as usize],
            &frame.rgba,
        );
        match self.preview_texture.as_mut() {
            Some(tex) if tex.size() == image.size => tex.set(image, TextureOptions::LINEAR),
            _ => {
                self.preview_texture =
                    Some(ctx.load_texture("preview", image, TextureOptions::LINEAR));
            }
        }
        self.preview_key = self.active_video_key();
    }

    /// Drop cached stills and the answer in flight after the timeline is
    /// replaced, so no picture from before can show.
    fn forget_stills(&mut self) {
        self.still_shown = None;
        self.still_cache.clear();
        if let Some((_, key)) = &mut self.still_inflight {
            *key = None;
        }
    }

    fn poll_frames(&mut self, ctx: &egui::Context) {
        let active_key = self.active_video_key();
        if self.preview_key != active_key {
            self.preview_texture = None;
            self.preview_key = None;
        }
        let active = self.active_video_clip();
        let source_time = active
            .as_ref()
            .map(|clip| clip.source_in + (self.playhead - clip.start));
        let wants_still = !self.playing || self.scrubbing();
        while let Ok((gen, result)) = self.still_rx.try_recv() {
            let Some((pending, key)) = self.still_inflight else {
                continue;
            };
            if gen != pending {
                continue;
            }
            self.still_inflight = None;
            let (Some(key), Ok(frame)) = (key, result) else {
                continue;
            };
            // The newest finished picture of this clip shows even when the
            // pointer has moved on; the next request chases the pointer.
            if wants_still && active.as_ref().is_some_and(|clip| clip.id == key.0) {
                self.update_preview_texture(ctx, &frame);
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
                self.update_preview_texture(ctx, &frame);
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

    fn request_still_if_needed(&mut self, ctx: &egui::Context) {
        let Some(clip) = self.active_video_clip() else {
            return;
        };
        let Some(source) = clip.source.clone() else {
            return;
        };
        let Some(asset) = self.asset(&source) else {
            return;
        };
        let (w, h) = (asset.info.width, asset.info.height);
        let source_time = clip.source_in + (self.playhead - clip.start);
        // Two tiers: fast small frames while interacting, full quality when idle.
        let width = if self.scrubbing() { SCRUB_W } else { PREVIEW_W };
        let key = (
            clip.id,
            (source_time * self.timeline.fps).round() as i64,
            width,
        );
        if self.still_shown == Some(key) && self.preview_texture.is_some() {
            return;
        }
        if let Some(index) = self
            .still_cache
            .iter()
            .position(|(cached, _)| *cached == key)
        {
            // Most recently used moves to the back.
            let (key, frame) = self.still_cache.remove(index).unwrap();
            self.update_preview_texture(ctx, &frame);
            self.still_shown = Some(key);
            self.still_cache.push_back((key, frame));
            return;
        }
        // One request at a time: while it decodes, the pointer can move on;
        // its answer shows, then this asks for wherever the pointer is.
        if self.still_inflight.is_some() {
            return;
        }
        self.still_gen += 1;
        self.still_inflight = Some((self.still_gen, Some(key)));
        self.still_decoder.request(
            self.still_gen,
            source,
            w,
            h,
            source_time,
            width,
            width <= SCRUB_W,
        );
    }

    /// Keep the video stream and audio players aligned with the playhead.
    /// Restarts sources on seeks and clip-boundary crossings.
    fn sync_playback(&mut self, ctx: &egui::Context) {
        let now = self.last_now.unwrap_or(0.0);
        if self.last_sync_time > 0.0 {
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
            self.request_still_if_needed(ctx);
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
            self.request_still_if_needed(ctx);
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
                            let wake_ctx = ctx.clone();
                            match FrameStream::spawn_preview(move || wake_ctx.request_repaint()) {
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
        if let (Some(stream), Some((_, Some((source, time))))) =
            (&self.stream, self.next_video_change(self.playhead))
        {
            let _ = stream.prefetch_preview(source, time, PREVIEW_W);
        }
        self.sync_audio();
        if self.waiting_for_video() {
            // Preroll can exceed a short excerpt. The clock limit holds the
            // shared transport and its audio until the first picture.
            self.status = "Buffering video.".to_owned();
            // A held clock is not a seek, even if the next UI tick is late.
            self.last_sync_time = 0.0;
            return;
        }
    }

    fn audio_running(&self) -> bool {
        self.audio
            .as_ref()
            .is_some_and(|audio| !audio.voice_ids().is_empty())
    }

    /// Give the mixer every audible clip under the playhead or starting
    /// within the look-ahead, so it starts decoding before their edges.
    fn sync_audio(&mut self) {
        const LOOK_AHEAD: f64 = 0.75;
        let horizon = self.playhead + LOOK_AHEAD;
        let segments: Vec<(u64, AudioSegment)> = self
            .timeline
            .clips
            .iter()
            .filter(|c| {
                c.start < horizon
                    && c.end() > self.playhead
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
                        },
                    )
                })
            })
            .collect();
        let audio = self.audio_engine();
        audio.set_voices(&segments);
        if let Some(error) = audio.take_error() {
            if !self.audio_failed {
                self.audio_failed = true;
                self.status = format!("Audio playback failed: {error}");
            }
        }
    }

    fn poll_export(&mut self) {
        let mut done = false;
        loop {
            let msg = match self.export_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(msg)) => msg,
                Some(Err(TryRecvError::Empty)) => break,
                Some(Err(TryRecvError::Disconnected)) => {
                    done = true;
                    break;
                }
                None => break,
            };
            match msg {
                ExportMsg::Progress(p) => self.export_progress = Some(p),
                ExportMsg::Done(Ok(path)) => {
                    self.status = format!("Exported {}", path.display());
                    done = true;
                }
                ExportMsg::Done(Err(e)) => {
                    self.status = e;
                    done = true;
                }
            }
        }
        if done {
            self.export_rx = None;
            self.export_progress = None;
        }
    }

    fn start_export(&mut self, ctx: &egui::Context, output: PathBuf) {
        if self.export_rx.is_some() {
            self.status = "An export is already running.".to_owned();
            return;
        }
        if self.timeline.clips.is_empty() {
            self.status = "Nothing to export.".to_owned();
            return;
        }
        let timeline = self.timeline.clone();
        let infos: HashMap<String, MediaInfo> = self
            .assets
            .iter()
            .map(|a| (a.info.path.clone(), a.info.clone()))
            .collect();
        let (tx, rx) = channel::<ExportMsg>();
        let (progress_tx, progress_rx) = channel::<f64>();
        self.export_rx = Some(rx);
        self.export_progress = Some(0.0);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let tx_progress = tx.clone();
            let ctx_progress = ctx.clone();
            let forwarder = thread::spawn(move || {
                while let Ok(p) = progress_rx.recv() {
                    let _ = tx_progress.send(ExportMsg::Progress(p));
                    ctx_progress.request_repaint();
                }
            });
            let result =
                openreel_media::export_timeline(&timeline, &infos, &output, true, progress_tx);
            let _ = forwarder.join();
            let _ = tx.send(ExportMsg::Done(result));
            ctx.request_repaint();
        });
    }

    fn step_history(&mut self, redo: bool) {
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
            return;
        }
        self.set_playing(false);
        self.playhead = self.playhead.min(self.timeline.end());
        self.forget_stills();
        self.status = if redo {
            "Redid the edit."
        } else {
            "Undid the last edit."
        }
        .to_owned();
    }

    fn handle_action(&mut self, ctx: &egui::Context, action: UiAction) {
        match action {
            UiAction::Import => {
                if let Some(paths) = rfd::FileDialog::new()
                    .add_filter("Media", MEDIA_EXTS)
                    .set_title("Import media")
                    .pick_files()
                {
                    self.spawn_import(ctx, paths);
                }
            }
            UiAction::Export => {
                if let Some(output) = rfd::FileDialog::new()
                    .add_filter("MP4 video", &["mp4"])
                    .set_file_name("export.mp4")
                    .set_title("Export MP4")
                    .save_file()
                {
                    self.start_export(ctx, output);
                }
            }
            UiAction::NewProject => self.request_discard(ctx, project::Discard::New),
            UiAction::OpenProject => self.request_discard(ctx, project::Discard::Open),
            UiAction::SaveProject => {
                self.save_project(false);
            }
            UiAction::SaveProjectAs => {
                self.save_project(true);
            }
            UiAction::LocateMedia => self.pick_missing_media(ctx),
            UiAction::Undo => self.step_history(false),
            UiAction::Redo => self.step_history(true),
            UiAction::ClearTimeline => {
                self.cancel_chat();
                self.set_playing(false);
                self.timeline =
                    Timeline::empty_project().with_orientation(self.timeline.orientation);
                self.selected = None;
                self.playhead = 0.0;
                self.forget_stills();
                self.status = "Timeline cleared. Media bin kept.".to_owned();
            }
            UiAction::SplitSelected => self.split_at_playhead(),
            UiAction::DeleteSelected => {
                if let Some(id) = self.selected.take() {
                    self.delete_clip(id);
                    self.status = "Clip deleted.".to_owned();
                }
            }
            UiAction::AddTitle => {
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
            UiAction::ZoomIn => {
                self.timeline_view.px_per_sec = (self.timeline_view.px_per_sec * 1.5).min(600.0);
            }
            UiAction::ZoomOut => {
                self.timeline_view.px_per_sec = (self.timeline_view.px_per_sec / 1.5).max(4.0);
            }
            UiAction::GoStart => self.seek(0.0),
            UiAction::GoEnd => self.seek(self.timeline.end()),
        }
    }

    // -- scripting ---------------------------------------------------------------

    fn script_state_json(&self) -> String {
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
                    "fadeIn": c.fade_in,
                    "fadeOut": c.fade_out,
                    "embeddedAudio": c.embedded_audio,
                    "linkedClip": c.linked_clip,
                    "titleStyle": c.title_style.as_str(),
                })
            })
            .collect();
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
                    openreel_media::silence_ranges(&a.peaks, peaks_per_sec, 0.03, 0.35)
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
                    "beats": a.beats.as_ref().map(|grid| {
                        grid.beats
                            .iter()
                            .map(|t| (t * 1000.0).round() / 1000.0)
                            .collect::<Vec<_>>()
                    }),
                    "curationUsage": a.sweep.usage.json(),
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
                            "id": session::transcript_segment_id(&asset.info.path, index),
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
            "tool": "select",
            "magnet": self.timeline_view.magnet,
            "musicBedMuted": self.mute_music_beds,
            "format": self.timeline.orientation.as_str(),
            "clips": clips,
            "assets": assets,
            "sweepCandidates": sweep_candidates,
            "sceneClips": scene_clips,
            "transcriptSegments": transcript_segments,
        })
        .to_string()
    }

    fn apply_script_commands(
        &mut self,
        ctx: &egui::Context,
        commands: Vec<ScriptCommand>,
    ) -> Vec<String> {
        let mut notes = Vec::new();
        let mut created = HashMap::new();
        for command in commands {
            // Explicit console clears cancel old model replies even when
            // the replacement timeline happens to reuse identical IDs.
            if matches!(command, ScriptCommand::ClearTimeline) {
                self.cancel_chat();
            }
            match self.apply_script_command(ctx, command, &mut created) {
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
    fn apply_script_command(
        &mut self,
        ctx: &egui::Context,
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
            ScriptCommand::MoveClip { clip, start } => self.timeline.move_clip(clip, start),
            ScriptCommand::TrimClip { clip, left, right } => {
                if let Some(left) = left {
                    self.timeline.trim_left(clip, left);
                }
                if let Some(right) = right {
                    let mut end = right;
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
                    c.volume = value.clamp(0.0, 2.0);
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
                let style = TitleStyle::from_name(&style)
                    .ok_or_else(|| format!("unknown title style '{style}'"))?;
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
                let style = TitleStyle::from_name(&style)
                    .ok_or_else(|| format!("unknown title style '{style}'"))?;
                if let Some(target) = self.timeline.clip_mut(clip) {
                    if target.kind != ClipKind::Title {
                        return Err(format!("clip {clip} is not a title"));
                    }
                    target.title_style = style;
                }
            }
            ScriptCommand::AddCaptionsForClip { clip, style } => {
                let style = TitleStyle::from_name(&style)
                    .ok_or_else(|| format!("unknown title style '{style}'"))?;
                self.add_captions_for_clip(clip, style)?;
            }
            ScriptCommand::AddAsset { asset, at, handle } => {
                let (path, name, duration) = self
                    .assets
                    .iter()
                    .find(|a| a.info.path == asset || a.info.name == asset)
                    .map(|a| (a.info.path.clone(), a.info.name.clone(), a.info.duration))
                    .ok_or_else(|| format!("no asset '{asset}'"))?;
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
                let (path, name) = self
                    .assets
                    .iter()
                    .find(|a| a.info.path == asset || a.info.name == asset)
                    .map(|a| (a.info.path.clone(), a.info.name.clone()))
                    .ok_or_else(|| format!("no asset '{asset}'"))?;
                let id = self
                    .try_add_asset_range_to_timeline(&path, &name, None, at, source_in, duration)?;
                created.extend(handle.map(|handle| (handle, id)));
            }
            ScriptCommand::Import(path) => self.spawn_import(ctx, vec![PathBuf::from(path)]),
            ScriptCommand::Export(path) => self.start_export(ctx, PathBuf::from(path)),
            ScriptCommand::ClearTimeline => {
                self.set_playing(false);
                self.timeline =
                    Timeline::empty_project().with_orientation(self.timeline.orientation);
                self.selected = None;
                self.playhead = 0.0;
                self.forget_stills();
            }
            ScriptCommand::SetTool(tool) => match tool.as_str() {
                // One pointer mode remains; split is an action at the playhead.
                "select" => {}
                other => return Err(format!("unknown tool '{other}'")),
            },
            ScriptCommand::SetMagnet(on) => self.timeline_view.magnet = on,
            ScriptCommand::SetMusicBedMuted(on) => self.mute_music_beds = on,
            ScriptCommand::SetZoom(px) => self.timeline_view.px_per_sec = px.clamp(4.0, 600.0),
            ScriptCommand::CloseGaps => {
                self.timeline.close_gaps();
            }
        }
        Ok(None)
    }

    fn run_console(&mut self, ctx: &egui::Context, code: String) {
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
        for note in self.apply_script_commands(ctx, commands) {
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

    fn console_panel(&mut self, ctx: &egui::Context) {
        if !self.show_console {
            return;
        }
        let mut submitted: Option<String> = None;
        egui::TopBottomPanel::bottom("console")
            .resizable(true)
            .default_height(150.0)
            .height_range(90.0..=340.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    theme::panel_title(ui, "Console");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::ghost_button(ui, "Clear")
                            .on_hover_text("Clear console output")
                            .clicked()
                        {
                            self.console_output.clear();
                        }
                    });
                });
                let output_height = (ui.available_height() - 38.0).max(24.0);
                egui::ScrollArea::vertical()
                    .max_height(output_height)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.console_output {
                            ui.monospace(line);
                        }
                    });
                ui.horizontal(|ui| {
                    ui.monospace(">");
                    let response = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.console_input)
                                .hint_text("JavaScript. help() lists the editor API.")
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace),
                        )
                        .on_hover_text(
                            "Run JavaScript against the editor. Enter runs. Up and Down recall history.",
                        );
                    if response.has_focus() || response.lost_focus() {
                        if ui.input(|i| i.key_pressed(egui::Key::ArrowUp))
                            && !self.console_history.is_empty()
                        {
                            let idx = match self.console_history_idx {
                                Some(0) => 0,
                                Some(i) => i - 1,
                                None => self.console_history.len() - 1,
                            };
                            self.console_history_idx = Some(idx);
                            self.console_input = self.console_history[idx].clone();
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                            match self.console_history_idx {
                                Some(i) if i + 1 < self.console_history.len() => {
                                    self.console_history_idx = Some(i + 1);
                                    self.console_input =
                                        self.console_history[i + 1].clone();
                                }
                                _ => {
                                    self.console_history_idx = None;
                                    self.console_input.clear();
                                }
                            }
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            submitted = Some(std::mem::take(&mut self.console_input));
                            response.request_focus();
                        }
                    }
                });
            });
        if let Some(code) = submitted {
            self.run_console(ctx, code);
        }
    }

    // -- panels ------------------------------------------------------------------

    fn advance_playback(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
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
            if self.playhead >= self.timeline.end() {
                self.playhead = self.timeline.end();
                self.set_playing(false);
            } else {
                ctx.request_repaint();
            }
        }
        let text_focused = ctx.memory(|m| m.focused().is_some());
        if !text_focused && ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            let next = !self.playing;
            self.set_playing(next);
        }
        if !text_focused && ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
            if let Some(id) = self.selected.take() {
                self.delete_clip(id);
                self.status = "Clip deleted.".to_owned();
            }
        }
        if !text_focused {
            let frame = 1.0 / self.timeline.fps;
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                self.seek(self.playhead - frame);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                self.seek(self.playhead + frame);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Home)) {
                self.seek(0.0);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::End)) {
                self.seek(self.timeline.end());
            }
            if ctx.input(|i| {
                !i.modifiers.command && (i.key_pressed(egui::Key::S) || i.key_pressed(egui::Key::B))
            }) {
                self.split_at_playhead();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::M)) {
                self.timeline_view.magnet = !self.timeline_view.magnet;
            }
        }
    }

    fn menu_bar(&mut self, ctx: &egui::Context) -> Option<UiAction> {
        let mut action: Option<UiAction> = None;
        egui::TopBottomPanel::top("menu_bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    brand_mark(ui);
                    ui.label(RichText::new("Eddie").family(theme::semibold()).size(15.0));
                    ui.add_space(10.0);
                    ui.menu_button("File", |ui| {
                        if menu_item(
                            ui,
                            "New project",
                            "Ctrl+N",
                            "Start an empty project with an empty media bin",
                            true,
                        ) {
                            action = Some(UiAction::NewProject);
                        }
                        if menu_item(
                            ui,
                            "Open project…",
                            "Ctrl+O",
                            "Open a saved timeline and its media",
                            true,
                        ) {
                            action = Some(UiAction::OpenProject);
                        }
                        if menu_item(
                            ui,
                            "Save project",
                            "Ctrl+S",
                            "Save the timeline and its media list",
                            true,
                        ) {
                            action = Some(UiAction::SaveProject);
                        }
                        if menu_item(
                            ui,
                            "Save project as…",
                            "Ctrl+Shift+S",
                            "Save the project to a new file",
                            true,
                        ) {
                            action = Some(UiAction::SaveProjectAs);
                        }
                        let missing = self.missing_media.len();
                        if menu_item(
                            ui,
                            &if missing > 0 {
                                format!("Locate missing media ({missing})…")
                            } else {
                                "Locate missing media…".to_owned()
                            },
                            "",
                            "Find a project's moved media files",
                            missing > 0,
                        ) {
                            action = Some(UiAction::LocateMedia);
                        }
                        ui.separator();
                        if menu_item(ui, "Import media…", "", "Import video or audio files", true)
                        {
                            action = Some(UiAction::Import);
                        }
                        if menu_item(ui, "Export MP4…", "", "Export the timeline as an MP4", true)
                        {
                            action = Some(UiAction::Export);
                        }
                        ui.separator();
                        if menu_item_destructive(
                            ui,
                            "Clear timeline",
                            "Remove every clip from the timeline; imported media stays",
                        ) {
                            action = Some(UiAction::ClearTimeline);
                        }
                    });
                    ui.menu_button("Edit", |ui| {
                        if menu_item(ui, "Undo", "Ctrl+Z", "Undo the last timeline edit", true) {
                            action = Some(UiAction::Undo);
                        }
                        if menu_item(
                            ui,
                            "Redo",
                            "Ctrl+Shift+Z",
                            "Redo the edit you undid (also Ctrl+Y)",
                            true,
                        ) {
                            action = Some(UiAction::Redo);
                        }
                        ui.separator();
                        let has_selection = self.selected.is_some();
                        if menu_item(
                            ui,
                            "Split at playhead",
                            "S",
                            "Split the selected clip at the playhead",
                            has_selection,
                        ) {
                            action = Some(UiAction::SplitSelected);
                        }
                        if menu_item(
                            ui,
                            "Delete clip",
                            "Del",
                            "Delete the selected clip",
                            has_selection,
                        ) {
                            action = Some(UiAction::DeleteSelected);
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            "Add title",
                            "",
                            "Add a title clip at the playhead",
                            true,
                        ) {
                            action = Some(UiAction::AddTitle);
                        }
                    });
                    ui.menu_button("View", |ui| {
                        ui.checkbox(&mut self.show_media, "Media bin")
                            .on_hover_text("Show the media bin and curator card");
                        ui.checkbox(&mut self.show_chat, "Chat")
                            .on_hover_text("Show the chat tab in the right dock (Ctrl+K)");
                        ui.checkbox(&mut self.show_inspector, "Details")
                            .on_hover_text("Show the selected clip's details in the right dock");
                        ui.checkbox(&mut self.show_console, "JS console")
                            .on_hover_text("Debug console for the editing script API");
                        ui.separator();
                        if menu_item(ui, "Zoom in", "Ctrl+Scroll", "Zoom the timeline in", true) {
                            action = Some(UiAction::ZoomIn);
                        }
                        if menu_item(ui, "Zoom out", "Ctrl+Scroll", "Zoom the timeline out", true) {
                            action = Some(UiAction::ZoomOut);
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            "Go to start",
                            "Home",
                            "Move the playhead to the start",
                            true,
                        ) {
                            action = Some(UiAction::GoStart);
                        }
                        if menu_item(ui, "Go to end", "End", "Move the playhead to the end", true) {
                            action = Some(UiAction::GoEnd);
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::primary_button(ui, "Export")
                            .on_hover_text("Export the timeline as an MP4")
                            .clicked()
                        {
                            action = Some(UiAction::Export);
                        }
                    });
                });
            });
        action
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status_bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(14, 5)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let busy = self.import_rx.is_some()
                        || self.export_rx.is_some()
                        || self.chat_pending.is_some()
                        || self.sweep_rx.is_some();
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    if busy {
                        egui::Spinner::new()
                            .color(theme::MUTED_FG)
                            .paint_at(ui, rect.expand(1.0));
                    } else {
                        ui.painter()
                            .circle_filled(rect.center(), 3.0, theme::SUBTLE_FG);
                    }
                    // The readout lays out first; the message truncates into
                    // what is left, so a long error never runs under it.
                    let status = |ui: &mut egui::Ui| {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&self.status)
                                        .size(12.0)
                                        .color(theme::MUTED_FG),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&self.status);
                        });
                    };
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(p) = self.export_progress {
                            ui.add(
                                egui::ProgressBar::new(p as f32)
                                    .desired_width(180.0)
                                    .show_percentage(),
                            )
                            .on_hover_text("Export progress");
                            ui.label(RichText::new("Exporting").size(12.0).color(theme::MUTED_FG));
                            ui.add_space(12.0);
                            status(ui);
                            return;
                        }
                        // Right to left: memory, UI frame time, length, rate.
                        let segments = [
                            (format!("{} MB", self.rss_mb), "Memory in use"),
                            (format!("{:.1} ms", self.frame_ms_ema), "UI frame time"),
                            (
                                format_timecode(self.timeline.end(), self.timeline.fps),
                                "Timeline length",
                            ),
                            (
                                format!("{:.0} fps", self.timeline.fps),
                                "Project frame rate",
                            ),
                        ];
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for (index, (text, tip)) in segments.into_iter().enumerate() {
                            if index > 0 {
                                ui.add_space(8.0);
                                ui.label(RichText::new("·").size(11.0).color(theme::SUBTLE_FG));
                                ui.add_space(8.0);
                            }
                            ui.label(
                                RichText::new(text)
                                    .monospace()
                                    .size(11.0)
                                    .color(theme::SUBTLE_FG),
                            )
                            .on_hover_text(tip);
                        }
                        ui.add_space(12.0);
                        status(ui);
                    });
                });
            });
    }

    fn timeline_panel(&mut self, ctx: &egui::Context) {
        // Short windows give the monitor room; this only sets the first
        // height, and a user resize is remembered.
        let screen_h = ctx.screen_rect().height();
        let (id, height) = if self.curator.open {
            ("gallery_timeline", 170.0)
        } else {
            ("timeline", (screen_h * 0.27).clamp(170.0, 240.0))
        };
        egui::TopBottomPanel::bottom(id)
            .resizable(true)
            .default_height(height)
            .height_range(170.0..=460.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    theme::panel_title(ui, "Timeline");
                    ui.add_space(10.0);
                    if tool_icon_button(
                        ui,
                        ToolIcon::Magnet,
                        self.timeline_view.magnet,
                        "Magnet: snap clip edges while dragging (M)",
                    )
                    .clicked()
                    {
                        self.timeline_view.magnet = !self.timeline_view.magnet;
                    }
                    toolbar_divider(ui);
                    if tool_icon_button(
                        ui,
                        ToolIcon::Title,
                        false,
                        "Add a title clip at the playhead",
                    )
                    .clicked()
                    {
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
                    if tool_icon_button(
                        ui,
                        ToolIcon::Split,
                        false,
                        "Split the clip under the playhead (S)",
                    )
                    .clicked()
                    {
                        self.split_at_playhead();
                    }
                    if tool_icon_button(
                        ui,
                        ToolIcon::Delete,
                        false,
                        "Delete the selected clip (Del)",
                    )
                    .clicked()
                    {
                        if let Some(id) = self.selected.take() {
                            self.delete_clip(id);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if tool_icon_button(ui, ToolIcon::ZoomIn, false, "Zoom in").clicked() {
                            self.timeline_view.px_per_sec =
                                (self.timeline_view.px_per_sec * 1.5).min(600.0);
                        }
                        ui.spacing_mut().slider_width = 110.0;
                        ui.add(
                            egui::Slider::new(&mut self.timeline_view.px_per_sec, 4.0..=600.0)
                                .logarithmic(true)
                                .show_value(false),
                        )
                        .on_hover_text("Timeline zoom. Ctrl+scroll over the timeline also zooms.");
                        if tool_icon_button(ui, ToolIcon::ZoomOut, false, "Zoom out").clicked() {
                            self.timeline_view.px_per_sec =
                                (self.timeline_view.px_per_sec / 1.5).max(4.0);
                        }
                    });
                });
                ui.add_space(8.0);
                let visuals: VisualMap<'_> = self
                    .assets
                    .iter()
                    .map(|a| {
                        (
                            a.info.path.as_str(),
                            ClipVisual {
                                duration: a.info.duration,
                                peaks: &a.peaks,
                                thumbs: &a.thumb_textures,
                            },
                        )
                    })
                    .collect();
                self.timeline_view.show(
                    ui,
                    &mut self.timeline,
                    &mut self.playhead,
                    &mut self.selected,
                    &visuals,
                );
            });
    }

    /// Right dock: Chat and Details (inspector) share one panel
    /// with tabs, so the timeline keeps its full height. Selecting
    /// a clip switches to Details; Cmd+K switches back to Chat.
    fn right_dock(&mut self, ctx: &egui::Context) {
        if !self.show_chat && !self.show_inspector {
            return;
        }
        if self.selected != self.prev_selected {
            self.prev_selected = self.selected;
            if self.selected.is_some() && self.show_inspector {
                self.right_tab = RightTab::Details;
            }
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::K)) && self.show_chat {
            self.right_tab = RightTab::Chat;
        }
        // Fall back when the active tab is hidden from the View menu.
        match self.right_tab {
            RightTab::Chat if !self.show_chat => self.right_tab = RightTab::Details,
            RightTab::Details if !self.show_inspector => self.right_tab = RightTab::Chat,
            _ => {}
        }
        egui::SidePanel::right("right_dock")
            .resizable(true)
            .default_width(340.0)
            .width_range(280.0..=460.0)
            .frame(theme::panel_frame())
            .show(ctx, |ui| {
                let mut tabs = Vec::new();
                if self.show_chat {
                    tabs.push((RightTab::Chat, "Chat", "Chat editing. Cmd+K jumps here."));
                }
                if self.show_inspector {
                    tabs.push((
                        RightTab::Details,
                        "Details",
                        "Fine-grained numbers for the selected clip",
                    ));
                }
                ui.horizontal(|ui| {
                    if let Some(tab) = theme::segmented(ui, self.right_tab, &tabs) {
                        self.right_tab = tab;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if tool_icon_button(
                            ui,
                            ToolIcon::Settings,
                            self.show_chat_settings,
                            "Meta Model API settings",
                        )
                        .clicked()
                        {
                            self.show_chat_settings = !self.show_chat_settings;
                        }
                        if self.right_tab == RightTab::Chat {
                            if tool_icon_button(
                                ui,
                                ToolIcon::Code,
                                self.show_chat_debug,
                                "Show the last generated JavaScript",
                            )
                            .clicked()
                            {
                                self.show_chat_debug = !self.show_chat_debug;
                            }
                            if self.chat_undo.is_some()
                                && tool_icon_button(
                                    ui,
                                    ToolIcon::Undo,
                                    false,
                                    "Undo the last AI edit",
                                )
                                .clicked()
                            {
                                self.undo_chat_edit();
                            }
                        }
                    });
                });
                ui.add_space(12.0);
                match self.right_tab {
                    RightTab::Chat => self.chat_content(ui, ctx),
                    RightTab::Details => {
                        egui::ScrollArea::vertical()
                            .id_salt("inspector_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.inspector_content(ui));
                    }
                }
            });
    }

    fn inspector_content(&mut self, ui: &mut Ui) {
        let Some(id) = self.selected else {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No clip selected").color(theme::MUTED_FG));
                ui.label(theme::caption("Click a clip on the timeline to edit it."));
            });
            return;
        };

        let clip_snapshot = self.timeline.clip(id).cloned();
        let linked_snapshot = clip_snapshot
            .as_ref()
            .and_then(|clip| clip.linked_clip)
            .and_then(|linked| self.timeline.clip(linked).cloned());
        let source_has_audio = clip_snapshot
            .as_ref()
            .and_then(|clip| clip.source.as_ref())
            .and_then(|source| self.asset(source))
            .map(|asset| asset.info.has_audio)
            .unwrap_or(false);
        let has_transcript = clip_snapshot
            .as_ref()
            .and_then(|clip| clip.source.as_ref())
            .and_then(|source| self.asset(source))
            .map(|asset| !asset.sweep.transcript.is_empty())
            .unwrap_or(false);
        let mut caption_style = self.caption_style;
        let mut do_split = false;
        let mut do_delete = false;
        let mut do_detach = false;
        let mut do_unlink = false;
        let mut do_tighten = false;
        let mut do_captions = false;
        let mut start_changed = false;
        let mut edited_start = clip_snapshot.as_ref().map(|clip| clip.start).unwrap_or(0.0);
        if let Some(clip) = self.timeline.clip_mut(id) {
            let (kind_label, kind_color) = match clip.kind {
                ClipKind::Video => ("Video", theme::CLIP_VIDEO),
                ClipKind::Audio => ("Audio", theme::CLIP_AUDIO),
                ClipKind::Title => ("Title", theme::CLIP_TITLE),
            };
            // One header row: kind dot, then the editable name. The kind
            // and source file live in the tooltips.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                ui.painter()
                    .circle_filled(dot.center(), 4.0, kind_color.gamma_multiply(1.6));
                let hover = match &clip.source {
                    Some(path) => format!("{kind_label} clip name\nSource: {path}"),
                    None => format!("{kind_label} clip name"),
                };
                ui.add(
                    egui::TextEdit::singleline(&mut clip.name)
                        .desired_width(f32::INFINITY)
                        .margin(egui::Margin::symmetric(8, 6)),
                )
                .on_hover_text(hover);
            });
            ui.add_space(10.0);
            let is_audio = clip.kind == ClipKind::Audio;
            let has_source = clip.source.is_some();
            inspector_section(ui, "Timing", |ui| {
                start_changed =
                    prop_row(ui, "Start", |ui| seconds_field(ui, &mut edited_start, 0.0))
                        .on_hover_text("Where the clip starts on the timeline")
                        .changed();
                prop_row(ui, "Duration", |ui| {
                    seconds_field(ui, &mut clip.duration, 0.1)
                })
                .on_hover_text("How long the clip plays");
                if has_source {
                    prop_row(ui, "Source in", |ui| {
                        seconds_field(ui, &mut clip.source_in, 0.0)
                    })
                    .on_hover_text("Where playback starts inside the source file");
                }
            });
            ui.add_space(8.0);
            let look_title = match clip.kind {
                ClipKind::Video => "Picture & sound",
                ClipKind::Audio => "Sound",
                ClipKind::Title => "Look",
            };
            let kind = clip.kind;
            let video_audio = kind == ClipKind::Video && source_has_audio;
            let linked = clip.linked_clip.is_some();
            let can_detach = video_audio && clip.embedded_audio && !linked;
            let can_tighten = source_has_audio && kind != ClipKind::Title;
            // Sound controls share the card with picture controls, so a video
            // clip's whole inspector fits without scrolling.
            inspector_section(ui, look_title, |ui| {
                if !is_audio {
                    prop_row(ui, "Opacity", |ui| {
                        percent_slider(ui, &mut clip.opacity, 1.0)
                    })
                    .on_hover_text("Fades the picture against black");
                }
                if has_source {
                    prop_row(ui, "Volume", |ui| percent_slider(ui, &mut clip.volume, 1.5))
                        .on_hover_text("Clip gain; 100% is unity");
                }
                // Audio clips only, so a video clip's card keeps its height;
                // scripts can fade any clip's sound with setFade.
                if is_audio && has_source {
                    prop_row(ui, "Fades", |ui| {
                        let half = ((ui.available_width() - 6.0) / 2.0).max(40.0);
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let fade_in = ui
                            .allocate_ui(Vec2::new(half, 24.0), |ui| {
                                seconds_field(ui, &mut clip.fade_in, 0.0)
                            })
                            .inner
                            .on_hover_text("Fade in: seconds to rise from silence");
                        let fade_out = ui
                            .allocate_ui(Vec2::new(half, 24.0), |ui| {
                                seconds_field(ui, &mut clip.fade_out, 0.0)
                            })
                            .inner
                            .on_hover_text("Fade out: seconds to fall to silence");
                        fade_in.union(fade_out)
                    });
                }
                if kind == ClipKind::Title {
                    prop_row(ui, "Style", |ui| {
                        egui::ComboBox::from_id_salt("title_style")
                            .width(ui.available_width())
                            .selected_text(clip.title_style.label())
                            .show_ui(ui, |ui| {
                                for style in TitleStyle::ALL {
                                    ui.selectable_value(
                                        &mut clip.title_style,
                                        style,
                                        style.label(),
                                    );
                                }
                            })
                            .response
                    })
                    .on_hover_text("Choose how this title or caption is drawn");
                }
                // Detached sound plays from the linked clip; the line below
                // says so, so the checkbox only shows while it applies.
                if video_audio && !linked {
                    ui.checkbox(&mut clip.embedded_audio, "Embedded audio")
                        .on_hover_text("Play this video clip's own sound");
                }
                if let Some(partner) = &linked_snapshot {
                    ui.add(
                        egui::Label::new(theme::caption(format!("Linked to {}", partner.name)))
                            .truncate(),
                    )
                    .on_hover_text("Picture and sound move, trim and split together");
                }
                let mut buttons: Vec<(&str, &str, &mut bool)> = Vec::new();
                if can_detach {
                    buttons.push((
                        "Detach audio",
                        "Move this clip's sound to a linked audio clip you can edit separately",
                        &mut do_detach,
                    ));
                }
                if linked_snapshot.is_some() {
                    buttons.push((
                        "Unlink",
                        "Let picture and sound move independently for J/L cuts or replacement audio",
                        &mut do_unlink,
                    ));
                }
                if can_tighten {
                    buttons.push((
                        "Remove silence",
                        "Cut waveform pauses from this speech clip and ripple later clips left",
                        &mut do_tighten,
                    ));
                }
                button_row(ui, buttons, false);
                // Captions come from this clip's sound, so they share its card:
                // one row with the style and the action.
                if kind != ClipKind::Title && has_transcript {
                    prop_row(ui, "Captions", |ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let add = theme::secondary_button(ui, "Add").on_hover_text(
                                "Generate caption clips from this clip's transcript in the chosen style",
                            );
                            if add.clicked() {
                                do_captions = true;
                            }
                            egui::ComboBox::from_id_salt("caption_style")
                                .width(ui.available_width() - 20.0)
                                .truncate()
                                .selected_text(caption_style.label())
                                .show_ui(ui, |ui| {
                                    for style in TitleStyle::ALL {
                                        ui.selectable_value(
                                            &mut caption_style,
                                            style,
                                            style.label(),
                                        );
                                    }
                                })
                                .response
                                .on_hover_text("Choose the style for generated captions");
                            add
                        })
                        .inner
                    });
                }
            });
            ui.add_space(12.0);
            button_row(
                ui,
                vec![
                    (
                        "Split at playhead",
                        "Split this clip at the playhead (S)",
                        &mut do_split,
                    ),
                    ("Delete", "Delete this clip (Del)", &mut do_delete),
                ],
                true,
            );
        }

        self.caption_style = caption_style;

        // Merely viewing Details must not quantize a detected scene boundary.
        if start_changed {
            self.timeline.move_clip(id, edited_start);
        }
        let source_duration = self
            .timeline
            .clip(id)
            .and_then(|c| c.source.as_ref())
            .and_then(|s| self.asset(s))
            .map(|a| a.info.duration);
        if let (Some(max_dur), Some(clip)) = (source_duration, self.timeline.clip_mut(id)) {
            clip.source_in = clip.source_in.clamp(0.0, max_dur);
            clip.duration = clip.duration.min((max_dur - clip.source_in).max(0.1));
        }

        // Fine-grained duration/source edits keep a linked pair in
        // sync by the same deltas start edits get from `move_clip`.
        if let (Some(original), Some(current), Some(linked_original)) = (
            clip_snapshot.as_ref(),
            self.timeline.clip(id).cloned(),
            linked_snapshot.as_ref(),
        ) {
            let duration_delta = current.duration - original.duration;
            let source_delta = current.source_in - original.source_in;
            if duration_delta != 0.0 || source_delta != 0.0 {
                let min_duration = 1.0 / self.timeline.fps;
                if let Some(linked) = self.timeline.clip_mut(linked_original.id) {
                    linked.duration = (linked_original.duration + duration_delta).max(min_duration);
                    linked.source_in = (linked_original.source_in + source_delta).max(0.0);
                }
            }
        }

        if do_detach {
            if source_has_audio {
                match self.timeline.detach_audio(id) {
                    Some(audio) => {
                        self.selected = Some(audio);
                        self.status = "Audio detached.".to_owned();
                    }
                    None => {
                        self.status = "This clip's audio is already detached.".to_owned();
                    }
                }
            } else {
                self.status = "This clip has no detachable audio.".to_owned();
            }
        }
        if do_unlink {
            if self.timeline.unlink_clip(id) {
                self.status = "Picture and audio unlinked.".to_owned();
            } else {
                self.status = "This clip is not linked.".to_owned();
            }
        }
        if do_captions {
            match self.add_captions_for_clip(id, caption_style) {
                Ok(0) => {
                    self.status = "Captions already added or none overlap.".to_owned();
                }
                Ok(count) => {
                    self.status = format!(
                        "Added {count} caption{}.",
                        if count == 1 { "" } else { "s" }
                    );
                }
                Err(error) => self.status = error,
            }
        }
        if do_tighten {
            match self.tighten_clip_silence(id, 0.03, 0.35, 0.08) {
                Ok(1) => self.status = "No removable silence found.".to_owned(),
                Ok(pieces) => {
                    self.status = format!("Removed silence into {pieces} phrases.");
                }
                Err(error) => self.status = error,
            }
        }
        if do_split {
            match self.timeline.split_at(id, self.playhead) {
                Some(new_id) => {
                    self.selected = Some(new_id);
                    self.status = "Clip split at playhead.".to_owned();
                }
                None => {
                    self.status = "Playhead is not inside the selected clip.".to_owned();
                }
            }
        }
        if do_delete {
            self.delete_clip(id);
            self.selected = None;
            self.status = "Clip deleted.".to_owned();
        }
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(16, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let picked = theme::segmented(
                        ui,
                        self.curator.open,
                        &[
                            (
                                false,
                                "Program",
                                "Show the timeline preview and playback controls",
                            ),
                            (true, "Gallery", "Pause playback and browse curated clips"),
                        ],
                    );
                    match picked {
                        Some(false) => self.curator.open = false,
                        Some(true) => self.open_curator_gallery(),
                        None => {}
                    }
                });
                ui.add_space(10.0);
                if self.curator.open {
                    self.gallery_content(ui);
                    return;
                }

                let avail = ui.available_size();
                let (rect, _) = ui.allocate_exact_size(
                    Vec2::new(avail.x, (avail.y - 72.0).clamp(0.0, avail.y)),
                    Sense::hover(),
                );
                let audio_peaks = self
                    .timeline
                    .clips
                    .iter()
                    .find(|c| c.kind == ClipKind::Audio && c.contains(self.playhead))
                    .and_then(|c| c.source.as_ref())
                    .and_then(|s| self.asset(s))
                    .map(|a| (a.peaks.as_slice(), a.info.duration));
                preview::paint(
                    ui,
                    rect,
                    &self.timeline,
                    self.playhead,
                    self.preview_texture
                        .as_ref()
                        .filter(|_| self.preview_key == self.active_video_key()),
                    audio_peaks,
                    openreel_media::export_canvas(
                        self.timeline.orientation,
                        self.timeline
                            .clips
                            .iter()
                            .filter(|c| c.kind == ClipKind::Video)
                            .filter_map(|c| self.asset(c.source.as_deref()?))
                            .map(|a| (a.info.width, a.info.height)),
                    ),
                    !self.assets.is_empty(),
                );

                ui.add_space(4.0);
                let row = ui.available_rect_before_wrap();
                let bar_w = 520.0_f32.min(row.width());
                // Below this the length readout would overlap the controls.
                let show_length = bar_w >= 470.0;
                let scrub = egui::Rect::from_center_size(
                    egui::pos2(row.center().x, row.top() + 7.0),
                    Vec2::new(bar_w - 16.0, 14.0),
                );
                self.transport_scrubber(ui, scrub);
                let bar = egui::Rect::from_center_size(
                    egui::pos2(row.center().x, row.top() + 14.0 + 22.0),
                    Vec2::new(bar_w, 44.0),
                );
                ui.painter().rect(
                    bar,
                    CornerRadius::same(theme::R_LG),
                    theme::SURFACE,
                    Stroke::new(1.0_f32, theme::BORDER),
                    egui::StrokeKind::Inside,
                );
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(bar.shrink2(Vec2::new(14.0, 6.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        ui.label(timecode_job(
                            &format_timecode(self.playhead, self.timeline.fps),
                            theme::FOREGROUND,
                        ))
                        .on_hover_text("Playhead position");
                        let controls_w = 38.0 * 5.0 + 2.0 * 4.0;
                        let readout_w = ui.min_rect().width();
                        let left_pad = if show_length {
                            (ui.available_width() + readout_w - controls_w) / 2.0 - readout_w
                        } else {
                            ui.available_width() - controls_w
                        };
                        ui.add_space(left_pad.max(8.0));
                        if transport_button(ui, TransportIcon::ToStart, "Go to start (Home)")
                            .clicked()
                        {
                            self.seek(0.0);
                        }
                        if transport_button(ui, TransportIcon::StepBack, "Step back one frame (←)")
                            .clicked()
                        {
                            self.seek(self.playhead - 1.0 / self.timeline.fps);
                        }
                        let (icon, tip) = if self.playing {
                            (TransportIcon::Pause, "Pause (Space)")
                        } else {
                            (TransportIcon::Play, "Play (Space)")
                        };
                        if transport_button(ui, icon, tip).clicked() {
                            let next = !self.playing;
                            self.set_playing(next);
                        }
                        if transport_button(
                            ui,
                            TransportIcon::StepForward,
                            "Step forward one frame (→)",
                        )
                        .clicked()
                        {
                            self.seek(self.playhead + 1.0 / self.timeline.fps);
                        }
                        if transport_button(ui, TransportIcon::ToEnd, "Go to end (End)").clicked() {
                            self.seek(self.timeline.end());
                        }
                        if !show_length {
                            return;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(timecode_job(
                                &format_timecode(self.timeline.end(), self.timeline.fps),
                                theme::MUTED_FG,
                            ))
                            .on_hover_text("Timeline length");
                        });
                    },
                );
            });
    }
}

impl EditorApp {
    /// Thin whole-edit position track above the transport. Click or drag
    /// to move the playhead; while held it previews like a ruler scrub.
    fn transport_scrubber(&mut self, ui: &mut Ui, rect: egui::Rect) {
        let end = self.timeline.end();
        let mut response = ui.interact(
            rect,
            ui.id().with("transport_scrub"),
            Sense::click_and_drag(),
        );
        if !response.is_pointer_button_down_on() {
            // No tooltip over the readout while dragging.
            response = response.on_hover_text("Click or drag to move the playhead");
        }
        if end > 0.0 {
            if let Some(pos) = response.interact_pointer_pos() {
                if response.is_pointer_button_down_on() || response.clicked() {
                    let fraction = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                    self.playhead = self.timeline.snap(fraction as f64 * end);
                }
            }
        }
        let held = response.is_pointer_button_down_on();
        if self.transport_scrubbing && !held {
            // Settle at full preview quality where the pointer let go.
            self.preview_texture = None;
            self.preview_key = None;
        }
        self.transport_scrubbing = held;
        let active = held || response.hovered();
        let track = egui::Rect::from_center_size(
            rect.center(),
            Vec2::new(rect.width(), if active { 5.0 } else { 3.0 }),
        );
        let painter = ui.painter();
        painter.rect_filled(track, CornerRadius::same(3), theme::SECONDARY);
        let fraction = if end > 0.0 {
            (self.playhead / end).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let x = track.left() + track.width() * fraction;
        painter.rect_filled(
            egui::Rect::from_min_max(track.min, egui::pos2(x, track.max.y)),
            CornerRadius::same(3),
            if active {
                theme::FOREGROUND
            } else {
                theme::MUTED_FG
            },
        );
        if active {
            painter.circle_filled(egui::pos2(x, track.center().y), 6.0, theme::FOREGROUND);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    /// Post-panel work for one frame: playback sync, then the
    /// deferred UI action. Split out of `eframe::App::update` so
    /// tests can drive the real seam. Gap closing lives at the edit
    /// sites (drag release, delete, script CloseGaps) where the
    /// intent is known; a per-frame close here cannot tell a gap a
    /// left trim opened on purpose from one a delete left behind.
    fn finish_frame(&mut self, ctx: &egui::Context, action: Option<UiAction>) {
        self.sync_playback(ctx);
        if let Some(action) = action {
            self.handle_action(ctx, action);
        }
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let update_start = Instant::now();
        self.advance_playback(ctx);
        self.poll_imports(ctx);
        self.poll_frames(ctx);
        self.poll_export();
        self.poll_chat(ctx);
        self.poll_sweeps(ctx);
        if !self.initial_imports.is_empty() && self.import_rx.is_none() {
            let paths = std::mem::take(&mut self.initial_imports);
            self.spawn_import(ctx, paths);
        }

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.spawn_import(ctx, dropped);
        }

        self.guard_close(ctx);
        let mut action = self.menu_bar(ctx);
        self.status_bar(ctx);
        self.console_panel(ctx);
        self.timeline_panel(ctx);
        self.chat_settings_window(ctx);
        if let Some(a) = self.media_bin(ctx) {
            action = Some(a);
        }
        self.right_dock(ctx);
        self.central(ctx);
        self.unsaved_dialog(ctx);
        if action.is_none() && self.pending_discard.is_none() {
            action = shortcut_action(ctx);
        }
        self.finish_frame(ctx, action);
        let (now, pointer_down) = ctx.input(|i| (i.time, i.pointer.any_down()));
        let typing = ctx.memory(|m| m.focused().is_some());
        if self
            .history
            .track(&self.timeline, self.selected, now, pointer_down, typing)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if self.import_rx.is_some() || self.export_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        let ms = update_start.elapsed().as_secs_f32() * 1000.0;
        self.frame_ms_ema = if self.frame_ms_ema == 0.0 {
            ms
        } else {
            self.frame_ms_ema * 0.9 + ms * 0.1
        };
        self.frame_count += 1;
        if self.frame_count % 60 == 0 {
            self.rss_mb = read_rss_mb();
        }
    }
}

/// Project and history shortcuts. Undo/redo leave a focused text field
/// its own text undo.
fn shortcut_action(ctx: &egui::Context) -> Option<UiAction> {
    use egui::{Key, KeyboardShortcut, Modifiers};
    let typing = ctx.memory(|m| m.focused().is_some());
    ctx.input_mut(|i| {
        let shift = Modifiers::COMMAND | Modifiers::SHIFT;
        if i.consume_shortcut(&KeyboardShortcut::new(shift, Key::S)) {
            Some(UiAction::SaveProjectAs)
        } else if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::S)) {
            Some(UiAction::SaveProject)
        } else if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::O)) {
            Some(UiAction::OpenProject)
        } else if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::N)) {
            Some(UiAction::NewProject)
        } else if typing {
            None
        } else if i.consume_shortcut(&KeyboardShortcut::new(shift, Key::Z))
            || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Y))
        {
            Some(UiAction::Redo)
        } else if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)) {
            Some(UiAction::Undo)
        } else {
            None
        }
    })
}

/// Resident set size from `/proc` (Linux). Returns 0 elsewhere.
fn read_rss_mb() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb / 1024)
        .unwrap_or(0)
}

enum ToolIcon {
    Magnet,
    Title,
    Split,
    Delete,
    ZoomIn,
    ZoomOut,
    Settings,
    Code,
    Undo,
}

/// App mark (timeline-clip E with a playhead), drawn as shapes.
fn brand_mark(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
    theme::paint_mark(ui.painter(), rect);
}

/// Titled card grouping related inspector controls.
fn inspector_section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 8.0;
        theme::panel_title(ui, title);
        add(ui);
    });
}

/// Muted label in a fixed column, control filling the rest of the row.
fn prop_row(
    ui: &mut Ui,
    label: &str,
    add: impl FnOnce(&mut Ui) -> egui::Response,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(78.0, 26.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(78.0);
                ui.label(RichText::new(label).color(theme::MUTED_FG));
            },
        );
        add(ui)
    })
    .inner
}

/// Seconds at a fixed two decimals; the exact value is in its tooltip.
fn seconds_field(ui: &mut Ui, value: &mut f64, min: f64) -> egui::Response {
    let exact = format!("{:.3} s", *value);
    ui.add_sized(
        [ui.available_width(), 24.0],
        egui::DragValue::new(value)
            .speed(0.05)
            .range(min..=86400.0)
            .clamp_existing_to_range(false)
            .fixed_decimals(2)
            .suffix(" s"),
    )
    .on_hover_text(exact)
}

/// 0..=max gain or opacity as a slider plus a fixed-width percentage
/// field, so the row never asks the dock for more width than it has.
fn percent_slider(ui: &mut Ui, value: &mut f32, max: f32) -> egui::Response {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let field = ui.add_sized(
            [56.0, 24.0],
            egui::DragValue::new(value)
                .speed(0.01)
                .range(0.0..=max)
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                .custom_parser(|text| {
                    text.trim()
                        .trim_end_matches('%')
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .map(|v| v / 100.0)
                }),
        );
        ui.spacing_mut().slider_width = (ui.available_width() - 4.0).max(40.0);
        let slider = ui.add(egui::Slider::new(value, 0.0..=max).show_value(false));
        slider.union(field)
    })
    .inner
}

/// Equal-width buttons sharing one row; with `destructive_last` the final
/// button is the destructive variant.
fn button_row(ui: &mut Ui, buttons: Vec<(&str, &str, &mut bool)>, destructive_last: bool) {
    if buttons.is_empty() {
        return;
    }
    let count = buttons.len();
    let gap = 6.0;
    let width = ((ui.available_width() - gap * (count - 1) as f32) / count as f32).floor();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (index, (label, tooltip, flag)) in buttons.into_iter().enumerate() {
            ui.allocate_ui(Vec2::new(width, 30.0), |ui| {
                let response = if destructive_last && index + 1 == count {
                    theme::destructive_button_wide(ui, label)
                } else {
                    theme::secondary_button_wide(ui, label)
                };
                if response.on_hover_text(tooltip).clicked() {
                    *flag = true;
                }
            });
        }
    });
}

/// Monospace timecode with its leading zero groups dimmed, so the moving digits
/// stand out, as in most editors' transport readouts.
fn timecode_job(timecode: &str, bright: egui::Color32) -> egui::text::LayoutJob {
    // Dim whole leading `00:` groups; seconds and frames always stay bright.
    let mut dim = 0;
    while timecode[dim..].starts_with("00:") && dim + 3 + 5 <= timecode.len() {
        dim += 3;
    }
    let mut job = egui::text::LayoutJob::default();
    let font = egui::FontId::monospace(13.0);
    for (text, color) in [
        (&timecode[..dim], theme::SUBTLE_FG),
        (&timecode[dim..], bright),
    ] {
        job.append(
            text,
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            },
        );
    }
    job
}

/// Menu entries share one width so shortcut hints line up.
const MENU_WIDTH: f32 = 210.0;

/// Menu entry with a right-aligned shortcut hint; closes the menu on click.
fn menu_item(ui: &mut Ui, label: &str, shortcut: &str, tooltip: &str, enabled: bool) -> bool {
    let response = ui
        .add_enabled(
            enabled,
            egui::Button::new(label)
                .shortcut_text(RichText::new(shortcut).color(theme::SUBTLE_FG))
                .min_size(Vec2::new(MENU_WIDTH, 26.0)),
        )
        .on_hover_text(tooltip)
        .on_disabled_hover_text(format!("{tooltip}. Select a clip first"));
    if response.clicked() {
        ui.close();
    }
    response.clicked()
}

/// Menu entry for an action that discards work, in the destructive colour.
pub(super) fn menu_item_destructive(ui: &mut Ui, label: &str, tooltip: &str) -> bool {
    let response = ui
        .add(
            egui::Button::new(RichText::new(label).color(theme::DESTRUCTIVE))
                .min_size(Vec2::new(MENU_WIDTH, 26.0)),
        )
        .on_hover_text(tooltip);
    if response.clicked() {
        ui.close();
    }
    response.clicked()
}

/// Thin vertical rule separating toolbar groups.
fn toolbar_divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(13.0, 18.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(1.0_f32, theme::BORDER_STRONG),
    );
}

/// Timeline tool glyphs drawn as shapes, like the transport icons.
/// Borderless at rest; hover and the active toggle carry the fill.
fn tool_icon_button(ui: &mut Ui, icon: ToolIcon, active: bool, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(30.0, 30.0), Sense::click());
    let painter = ui.painter();
    if active {
        painter.rect(
            rect,
            CornerRadius::same(theme::R_MD),
            theme::SECONDARY,
            Stroke::NONE,
            egui::StrokeKind::Outside,
        );
    } else if response.is_pointer_button_down_on() {
        painter.rect(
            rect,
            CornerRadius::same(theme::R_MD),
            egui::Color32::from_rgb(0x40, 0x40, 0x49),
            Stroke::NONE,
            egui::StrokeKind::Outside,
        );
    } else if response.hovered() {
        painter.rect(
            rect,
            CornerRadius::same(theme::R_MD),
            theme::ACCENT,
            Stroke::NONE,
            egui::StrokeKind::Outside,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let c = rect.center();
    let fg = if active || response.hovered() {
        theme::FOREGROUND
    } else {
        theme::MUTED_FG
    };
    let line = Stroke::new(1.6_f32, fg);
    match icon {
        ToolIcon::ZoomIn | ToolIcon::ZoomOut => {
            painter.circle_stroke(c + Vec2::new(-1.5, -1.5), 5.5, line);
            painter.line_segment([c + Vec2::new(2.5, 2.5), c + Vec2::new(7.0, 7.0)], line);
            painter.line_segment(
                [c + Vec2::new(-4.0, -1.5), c + Vec2::new(1.0, -1.5)],
                Stroke::new(1.4_f32, fg),
            );
            if matches!(icon, ToolIcon::ZoomIn) {
                painter.line_segment(
                    [c + Vec2::new(-1.5, -4.0), c + Vec2::new(-1.5, 1.0)],
                    Stroke::new(1.4_f32, fg),
                );
            }
        }
        ToolIcon::Magnet => {
            // Horseshoe with separated pole tips, so it reads as a magnet
            // rather than a letter U.
            let mut body = vec![c + Vec2::new(-5.0, -3.5)];
            body.extend((0..=12).map(|i| {
                let angle = std::f32::consts::PI * (i as f32 / 12.0);
                c + Vec2::new(-5.0 * angle.cos(), 1.0 + 5.0 * angle.sin())
            }));
            body.push(c + Vec2::new(5.0, -3.5));
            painter.add(Shape::line(body, Stroke::new(3.0_f32, fg)));
            for x in [-5.0_f32, 5.0] {
                let tip = egui::Rect::from_center_size(c + Vec2::new(x, -7.0), Vec2::new(3.0, 3.0));
                painter.rect_filled(tip, CornerRadius::same(1), fg);
            }
        }
        ToolIcon::Settings => {
            // Gear: ring plus eight teeth.
            painter.circle_stroke(c, 4.6, line);
            for i in 0..8 {
                let angle = std::f32::consts::TAU * i as f32 / 8.0;
                let dir = Vec2::new(angle.cos(), angle.sin());
                painter.line_segment([c + dir * 5.4, c + dir * 7.6], Stroke::new(2.2_f32, fg));
            }
            painter.circle_filled(c, 1.6, fg);
        }
        ToolIcon::Code => {
            painter.line_segment([c + Vec2::new(-3.0, -5.0), c + Vec2::new(-7.0, 0.0)], line);
            painter.line_segment([c + Vec2::new(-7.0, 0.0), c + Vec2::new(-3.0, 5.0)], line);
            painter.line_segment([c + Vec2::new(3.0, -5.0), c + Vec2::new(7.0, 0.0)], line);
            painter.line_segment([c + Vec2::new(7.0, 0.0), c + Vec2::new(3.0, 5.0)], line);
            painter.line_segment([c + Vec2::new(1.5, -6.0), c + Vec2::new(-1.5, 6.0)], line);
        }
        ToolIcon::Undo => {
            // Counter-clockwise arrow.
            let arc: Vec<egui::Pos2> = (0..=14)
                .map(|i| {
                    let angle = std::f32::consts::PI * (1.1 - 1.3 * i as f32 / 14.0);
                    c + Vec2::new(1.0 + 5.5 * angle.cos(), 1.5 - 5.5 * angle.sin())
                })
                .collect();
            let start = arc[0];
            painter.add(Shape::line(arc, line));
            painter.line_segment([start, start + Vec2::new(0.5, -4.0)], line);
            painter.line_segment([start, start + Vec2::new(4.0, 0.8)], line);
        }
        ToolIcon::Title => {
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                "T",
                egui::FontId::proportional(16.0),
                fg,
            );
        }
        ToolIcon::Split => {
            // One bar cut in two, slightly separated.
            let left = egui::Rect::from_min_size(c + Vec2::new(-9.0, -5.0), Vec2::new(7.0, 10.0));
            let right = egui::Rect::from_min_size(c + Vec2::new(2.0, -5.0), Vec2::new(7.0, 10.0));
            painter.rect(
                left,
                CornerRadius::same(2),
                egui::Color32::TRANSPARENT,
                line,
                egui::StrokeKind::Inside,
            );
            painter.rect(
                right,
                CornerRadius::same(2),
                egui::Color32::TRANSPARENT,
                line,
                egui::StrokeKind::Inside,
            );
            painter.line_segment(
                [c + Vec2::new(0.0, -8.0), c + Vec2::new(0.0, 8.0)],
                Stroke::new(1.4_f32, theme::PLAYHEAD),
            );
        }
        ToolIcon::Delete => {
            // Trash can: lid, body, ribs.
            painter.line_segment([c + Vec2::new(-7.0, -5.0), c + Vec2::new(7.0, -5.0)], line);
            painter.line_segment([c + Vec2::new(-3.0, -8.0), c + Vec2::new(3.0, -8.0)], line);
            let body = egui::Rect::from_min_size(c + Vec2::new(-5.5, -5.0), Vec2::new(11.0, 13.0));
            painter.rect(
                body,
                CornerRadius::same(2),
                egui::Color32::TRANSPARENT,
                line,
                egui::StrokeKind::Inside,
            );
            for dx in [-2.5_f32, 0.0, 2.5] {
                painter.line_segment(
                    [c + Vec2::new(dx, -1.5), c + Vec2::new(dx, 5.0)],
                    Stroke::new(1.2_f32, fg),
                );
            }
        }
    }
    response.on_hover_text(tooltip)
}

enum TransportIcon {
    ToStart,
    StepBack,
    Play,
    Pause,
    StepForward,
    ToEnd,
}

/// Transport glyphs drawn as shapes — no icon-font dependency, always sharp.
fn transport_button(ui: &mut Ui, icon: TransportIcon, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(38.0, 32.0), Sense::click());
    let painter = ui.painter();
    if matches!(icon, TransportIcon::Play | TransportIcon::Pause) {
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    } else if response.is_pointer_button_down_on() {
        painter.rect(
            rect,
            CornerRadius::same(theme::R_MD),
            egui::Color32::from_rgb(0x40, 0x40, 0x49),
            Stroke::NONE,
            egui::StrokeKind::Outside,
        );
    } else if response.hovered() {
        painter.rect(
            rect,
            CornerRadius::same(theme::R_MD),
            theme::ACCENT,
            Stroke::NONE,
            egui::StrokeKind::Outside,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let c = rect.center();
    let primary = matches!(icon, TransportIcon::Play | TransportIcon::Pause);
    let fg = if primary {
        // The play control is the one filled, high-contrast button.
        let fill = if response.is_pointer_button_down_on() {
            egui::Color32::from_rgb(0xD4, 0xD4, 0xD8)
        } else if response.hovered() {
            egui::Color32::from_rgb(0xE4, 0xE4, 0xE7)
        } else {
            theme::PRIMARY
        };
        painter.circle_filled(c, 15.0, fill);
        theme::PRIMARY_FG
    } else {
        theme::FOREGROUND
    };
    let c = if matches!(icon, TransportIcon::Play) {
        c + Vec2::new(1.0, 0.0)
    } else {
        c
    };
    let tri = |flip: f32| {
        vec![
            c + Vec2::new(-5.0 * flip, -7.0),
            c + Vec2::new(7.0 * flip, 0.0),
            c + Vec2::new(-5.0 * flip, 7.0),
        ]
    };
    match icon {
        TransportIcon::StepBack | TransportIcon::StepForward => {
            // One-frame step: a small chevron beside a frame bar.
            let flip = if matches!(icon, TransportIcon::StepForward) {
                1.0
            } else {
                -1.0
            };
            let stroke = Stroke::new(1.8_f32, fg);
            painter.line_segment(
                [
                    c + Vec2::new(-3.0 * flip, -5.0),
                    c + Vec2::new(2.0 * flip, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    c + Vec2::new(2.0 * flip, 0.0),
                    c + Vec2::new(-3.0 * flip, 5.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    c + Vec2::new(5.5 * flip, -5.5),
                    c + Vec2::new(5.5 * flip, 5.5),
                ],
                stroke,
            );
        }
        TransportIcon::Play => {
            painter.add(Shape::convex_polygon(tri(1.0), fg, Stroke::NONE));
        }
        TransportIcon::Pause => {
            for dx in [-5.0, 2.0] {
                painter.rect_filled(
                    egui::Rect::from_min_size(c + Vec2::new(dx, -7.0), Vec2::new(3.5, 14.0)),
                    CornerRadius::same(1),
                    fg,
                );
            }
        }
        TransportIcon::ToStart => {
            painter.rect_filled(
                egui::Rect::from_min_size(c + Vec2::new(-8.0, -7.0), Vec2::new(2.5, 14.0)),
                CornerRadius::ZERO,
                fg,
            );
            painter.add(Shape::convex_polygon(tri(-1.0), fg, Stroke::NONE));
        }
        TransportIcon::ToEnd => {
            painter.add(Shape::convex_polygon(tri(1.0), fg, Stroke::NONE));
            painter.rect_filled(
                egui::Rect::from_min_size(c + Vec2::new(5.5, -7.0), Vec2::new(2.5, 14.0)),
                CornerRadius::ZERO,
                fg,
            );
        }
    }
    response.on_hover_text(tooltip)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod playback_tests {
        use super::*;
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::Duration;

        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

        struct Fixture {
            dir: PathBuf,
            path: String,
        }

        impl Fixture {
            fn new() -> Self {
                let dir = std::env::temp_dir().join(format!(
                    "eddie-preview-{}-{}",
                    std::process::id(),
                    NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir(&dir).unwrap();
                let path = dir.join("red-blue.mp4").to_string_lossy().into_owned();
                let output = std::process::Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-f",
                        "lavfi",
                        "-i",
                        "color=red:s=64x36:r=10:d=1",
                        "-f",
                        "lavfi",
                        "-i",
                        "color=blue:s=64x36:r=10:d=1",
                        "-filter_complex",
                        "[0:v][1:v]concat=n=2:v=1:a=0[v]",
                        "-map",
                        "[v]",
                        "-c:v",
                        "libx264",
                        "-threads",
                        "1",
                        "-g",
                        "100",
                        "-bf",
                        "3",
                    ])
                    .arg(&path)
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{:?}", output.stderr);
                Self { dir, path }
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }

        fn add_asset(app: &mut EditorApp, path: &str) {
            app.assets.push(BinAsset {
                info: openreel_media::probe(path).expect("probe preview fixture"),
                size_bytes: 0,
                state: AssetState::Ready,
                peaks: Vec::new(),
                beats: None,
                thumb_textures: Vec::new(),
                sweep: sweep::SweepData::default(),
            });
        }

        fn await_picture(app: &mut EditorApp, ctx: &egui::Context) -> egui::Color32 {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                app.poll_frames(ctx);
                app.sync_playback(ctx);
                let preview = app.preview_texture.as_ref().map(TextureHandle::id);
                for (id, delta) in ctx.tex_manager().write().take_delta().set {
                    if Some(id) == preview {
                        let egui::ImageData::Color(image) = delta.image;
                        return image.pixels[image.pixels.len() / 2];
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "picture timed out: {}",
                    app.status
                );
                thread::sleep(Duration::from_millis(5));
            }
        }

        fn advance_to(app: &mut EditorApp, ctx: &egui::Context, time: f64) {
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ctx| app.advance_playback(ctx),
            );
        }

        /// A drag that moves to a new frame every UI frame, faster than the
        /// still decoder answers, still shows pictures while it moves.
        #[test]
        fn continuous_scrub_shows_pictures_while_the_pointer_moves() {
            let fixture = Fixture::new();
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            add_asset(&mut app, &fixture.path);
            app.timeline
                .add_media_clip("Scrub", ClipKind::Video, 0, 0.0, 2.0, 0.0, &fixture.path);
            app.transport_scrubbing = true;
            let deadline = Instant::now() + Duration::from_secs(5);
            for step in 1_u32.. {
                app.playhead = f64::from(step % 20) / 10.0;
                app.poll_frames(&ctx);
                app.sync_playback(&ctx);
                if app.preview_texture.is_some() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "no picture during a continuous scrub"
                );
                thread::sleep(Duration::from_micros(200));
            }
        }

        #[test]
        fn preview_boundary_never_uploads_the_previous_clip() {
            let fixture = Fixture::new();
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            add_asset(&mut app, &fixture.path);
            for (start, source_in) in [(0.0, 0.0), (0.2, 1.0)] {
                app.timeline.add_media_clip(
                    "Short excerpt",
                    ClipKind::Video,
                    0,
                    start,
                    0.2,
                    source_in,
                    &fixture.path,
                );
            }
            app.set_playing(true);
            let first = await_picture(&mut app, &ctx);
            assert!(first.r() > 240 && first.b() < 10, "first frame: {first:?}");

            // This is the production order: advance the playhead, poll frames,
            // paint panels, and only then synchronize the changed source.
            app.playhead = 0.2;
            app.poll_frames(&ctx);
            assert!(
                app.preview_texture.is_none(),
                "old clip texture must not be painted under the new clip's label"
            );
            let next = await_picture(&mut app, &ctx);
            assert!(next.b() > 240 && next.r() < 10, "next frame: {next:?}");
            app.set_playing(false);
        }

        #[test]
        fn preview_transport_waits_for_each_short_clips_first_picture() {
            let fixture = Fixture::new();
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            add_asset(&mut app, &fixture.path);
            for (start, source_in) in [(0.0, 0.0), (0.05, 1.0)] {
                app.timeline.add_media_clip(
                    "Short excerpt",
                    ClipKind::Video,
                    0,
                    start,
                    0.05,
                    source_in,
                    &fixture.path,
                );
            }
            app.last_now = Some(0.0);
            app.set_playing(true);
            app.sync_playback(&ctx);
            advance_to(&mut app, &ctx, 1.0);
            assert_eq!(
                app.playhead, 0.0,
                "wall time must not consume a clip before its picture is uploaded"
            );
            assert!(app.playing);
            let first = await_picture(&mut app, &ctx);
            assert!(first.r() > 240 && first.b() < 10);

            // Even a late UI tick must land at the next clip's entry, not
            // jump over an entire excerpt that is shorter than the tick.
            advance_to(&mut app, &ctx, 1.5);
            assert_eq!(app.playhead, 0.05);
            assert!(app.playing);
            app.poll_frames(&ctx);
            assert!(app.preview_texture.is_none());
            app.sync_playback(&ctx);
            advance_to(&mut app, &ctx, 2.0);
            assert_eq!(app.playhead, 0.05);
            let next = await_picture(&mut app, &ctx);
            assert!(next.b() > 240 && next.r() < 10);
            advance_to(&mut app, &ctx, 2.5);
            assert_eq!(app.playhead, 0.1);
            assert!(!app.playing);
        }

        #[test]
        fn preview_prerolls_the_next_cut_before_the_playhead_reaches_it() {
            let fixture = Fixture::new();
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            add_asset(&mut app, &fixture.path);
            // Red is source [0, 1), blue [1, 2), with one keyframe at 0. The
            // backward cuts need the idle lane's preroll; the forward cut
            // continues on the active decoder instead of reseeking.
            let cuts = [(1.0, true), (0.0, false), (1.5, true), (0.5, false)];
            for (source_in, _) in cuts {
                let start = app.timeline.end();
                app.timeline.add_media_clip(
                    "Cut",
                    ClipKind::Video,
                    0,
                    start,
                    0.2,
                    source_in,
                    &fixture.path,
                );
            }
            app.last_now = Some(0.0);
            app.set_playing(true);
            for (index, (source_in, blue)) in cuts.into_iter().enumerate() {
                let picture = await_picture(&mut app, &ctx);
                assert_eq!(
                    picture.b() > 240 && picture.r() < 10,
                    blue,
                    "cut {index}: {picture:?}"
                );
                let Some((_, Some((source, time)))) = app.next_video_change(app.playhead) else {
                    break;
                };
                let source = source.to_owned();
                if time < source_in {
                    let stream = app.stream.as_ref().unwrap();
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !stream.prefetch_ready(&source, time, PREVIEW_W) {
                        assert!(
                            Instant::now() < deadline,
                            "cut {} was not prerolled before the playhead reached it",
                            index + 1
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                }
                // A late tick lands exactly on the cut that was prerolled.
                advance_to(&mut app, &ctx, index as f64 + 1.0);
                assert_eq!(app.playhead, app.timeline.clips[index + 1].start);
            }
            assert_eq!(
                app.stream.as_ref().unwrap().prefetch_handoffs(),
                2,
                "both backward cuts must be served from their prerolled pictures"
            );
            app.set_playing(false);
        }

        #[test]
        fn preview_reentering_a_file_reuses_its_parked_decoder() {
            let fixture = Fixture::new();
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            let mut paths = vec![fixture.path.clone()];
            for name in ["b.mp4", "c.mp4"] {
                let copy = fixture.dir.join(name);
                std::fs::copy(&fixture.path, &copy).unwrap();
                paths.push(copy.to_string_lossy().into_owned());
            }
            for path in &paths {
                add_asset(&mut app, path);
            }
            // A, B, C, then back to A. Both lanes have moved on from A by
            // then, so only a parked decoder avoids opening it again.
            let cuts = [
                (0, 0.0, false),
                (1, 0.0, false),
                (2, 0.0, false),
                (0, 1.2, true),
            ];
            for (file, source_in, _) in cuts {
                let start = app.timeline.end();
                app.timeline.add_media_clip(
                    "Cut",
                    ClipKind::Video,
                    0,
                    start,
                    0.2,
                    source_in,
                    &paths[file],
                );
            }
            // Parking is off on 8+ cores; this tests the mechanism itself.
            let stream = FrameStream::spawn_preview(|| {}).unwrap();
            stream.set_parked_decoders(4);
            app.stream = Some(stream);
            app.last_now = Some(0.0);
            app.set_playing(true);
            for (index, (_, _, blue)) in cuts.into_iter().enumerate() {
                let picture = await_picture(&mut app, &ctx);
                assert_eq!(
                    picture.b() > 240 && picture.r() < 10,
                    blue,
                    "cut {index}: {picture:?}"
                );
                let Some((_, Some((source, time)))) = app.next_video_change(app.playhead) else {
                    break;
                };
                let source = source.to_owned();
                let stream = app.stream.as_ref().unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                while !stream.prefetch_ready(&source, time, PREVIEW_W) {
                    assert!(Instant::now() < deadline, "cut {} not prerolled", index + 1);
                    thread::sleep(Duration::from_millis(5));
                }
                advance_to(&mut app, &ctx, index as f64 + 1.0);
                assert_eq!(app.playhead, app.timeline.clips[index + 1].start);
            }
            assert_eq!(
                app.stream.as_ref().unwrap().preview_opens(),
                3,
                "re-entering A must reuse its parked decoder"
            );
            app.set_playing(false);
        }

        #[test]
        fn preview_prerolled_cut_keeps_the_audio_clock_running() {
            let fixture = Fixture::new();
            let other = fixture.dir.join("b.mp4");
            std::fs::copy(&fixture.path, &other).unwrap();
            let other = other.to_string_lossy().into_owned();
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            add_asset(&mut app, &fixture.path);
            add_asset(&mut app, &other);
            for (start, path) in [(0.0, &fixture.path), (0.2, &other)] {
                app.timeline
                    .add_media_clip("Cut", ClipKind::Video, 0, start, 0.2, 0.0, path);
            }
            app.last_now = Some(0.0);
            app.set_playing(true);
            await_picture(&mut app, &ctx);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !app
                .stream
                .as_ref()
                .unwrap()
                .prefetch_ready(&other, 0.0, PREVIEW_W)
            {
                assert!(Instant::now() < deadline, "second cut was not prerolled");
                thread::sleep(Duration::from_millis(5));
            }
            advance_to(&mut app, &ctx, 0.3);
            assert_eq!(app.playhead, 0.2);
            app.sync_playback(&ctx);
            // The prerolled picture is handed over, not decoded: the audio
            // clock must not stall a device block waiting for its upload.
            assert!(
                !app.waiting_for_video(),
                "prerolled cut treated as buffering"
            );
            // Audio rendered on through the cut; the playhead rejoins it.
            advance_to(&mut app, &ctx, 0.35);
            assert!((app.playhead - 0.35).abs() < 1e-9, "{}", app.playhead);
            app.set_playing(false);
        }

        #[test]
        fn preview_buffers_audio_with_video_but_keeps_audio_only_gaps_moving() {
            let fixture = Fixture::new();
            let path = fixture.dir.join("with-audio.mp4");
            let output = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-i", &fixture.path])
                .args([
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=2",
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-c:v",
                    "copy",
                    "-c:a",
                    "aac",
                    "-threads:a",
                    "1",
                    "-shortest",
                ])
                .arg(&path)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output.stderr);
            let path = path.to_str().unwrap();
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            add_asset(&mut app, path);
            let video =
                app.timeline
                    .add_media_clip("Video", ClipKind::Video, 0, 0.2, 0.2, 0.0, path);
            let bed = app
                .timeline
                .add_media_clip("Bed", ClipKind::Audio, 1, 0.0, 1.0, 0.0, path);
            app.last_now = Some(0.0);
            app.set_playing(true);
            advance_to(&mut app, &ctx, 0.1);
            assert_eq!(app.playhead, 0.1);
            app.sync_playback(&ctx);
            // The look-ahead starts the video's audio before its edge.
            let voices = |app: &EditorApp| app.audio.as_ref().unwrap().voice_ids();
            assert_eq!(voices(&app), vec![video, bed]);

            advance_to(&mut app, &ctx, 0.3);
            assert_eq!(app.playhead, 0.2);
            app.poll_frames(&ctx);
            app.sync_playback(&ctx);
            advance_to(&mut app, &ctx, 0.5);
            assert_eq!(app.playhead, 0.2);
            assert_eq!(
                app.audio.as_ref().unwrap().clock(),
                0.2,
                "audio must not run ahead during preroll"
            );
            let picture = await_picture(&mut app, &ctx);
            assert!(picture.r() > 240 && picture.b() < 10);
            assert_eq!(voices(&app), vec![video, bed]);

            // The picture ends at 0.4; the audio-only gap keeps the clock.
            advance_to(&mut app, &ctx, 0.8);
            assert!((app.playhead - 0.5).abs() < 1e-9, "{}", app.playhead);
            app.poll_frames(&ctx);
            app.sync_playback(&ctx);
            assert_eq!(voices(&app), vec![bed]);
            advance_to(&mut app, &ctx, 0.9);
            assert!((app.playhead - 0.6).abs() < 1e-9, "{}", app.playhead);
            app.set_playing(false);
        }

        #[test]
        fn preview_source_remap_restarts_the_same_clip_and_invalidates_its_picture() {
            let fixture = Fixture::new();
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            add_asset(&mut app, &fixture.path);
            let clip = app.timeline.add_media_clip(
                "Excerpt",
                ClipKind::Video,
                0,
                0.0,
                0.5,
                0.0,
                &fixture.path,
            );
            app.set_playing(true);
            let first = await_picture(&mut app, &ctx);
            assert!(first.r() > 240 && first.b() < 10);
            let old_texture = app.preview_texture.as_ref().unwrap().id();
            app.timeline.clip_mut(clip).unwrap().source_in = 1.0;
            // Details/console can change the source after poll_frames ran but
            // before Program paints in the same egui frame.
            let painted = ctx.run(egui::RawInput::default(), |ctx| app.central(ctx));
            assert!(
                !painted.shapes.iter().any(|shape| {
                    matches!(&shape.shape, Shape::Mesh(mesh) if mesh.texture_id == old_texture)
                }),
                "Program must not paint the old source mapping"
            );
            app.poll_frames(&ctx);
            assert!(app.preview_texture.is_none());
            let next = await_picture(&mut app, &ctx);
            assert!(next.b() > 240 && next.r() < 10);
            app.set_playing(false);
        }

        #[test]
        fn preview_late_still_cannot_overwrite_playback_after_a_seek() {
            let fixture = Fixture::new();
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            add_asset(&mut app, &fixture.path);
            app.timeline.add_media_clip(
                "Picture",
                ClipKind::Video,
                0,
                0.0,
                2.0,
                0.0,
                &fixture.path,
            );
            app.request_still_if_needed(&ctx);
            // Hold a real native still result until after playback has changed
            // the picture. Only delivery timing is controlled, not decoding.
            let delayed = app.still_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(delayed.1.is_ok());
            let (tx, rx) = channel();
            app.still_rx = rx;
            app.seek(1.0);
            app.set_playing(true);
            let picture = await_picture(&mut app, &ctx);
            assert!(picture.b() > 240 && picture.r() < 10);
            let preview = app.preview_texture.as_ref().unwrap().id();
            tx.send(delayed).unwrap();
            app.poll_frames(&ctx);
            assert!(
                !ctx.tex_manager()
                    .write()
                    .take_delta()
                    .set
                    .iter()
                    .any(|(id, _)| *id == preview),
                "the delayed red still must not replace the blue playback picture"
            );
            app.set_playing(false);
        }

        #[cfg(unix)]
        #[test]
        fn preview_slow_native_open_does_not_block_the_ui_and_reports_failure() {
            let fixture = Fixture::new();
            let fifo = fixture.dir.join("slow-input.mp4");
            assert!(std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success());
            let writer_path = fifo.clone();
            let writer = thread::spawn(move || {
                let mut file = std::fs::File::create(writer_path).unwrap();
                thread::sleep(Duration::from_millis(600));
                file.write_all(b"deliberately invalid video after a slow native open")
                    .unwrap();
            });
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            add_asset(&mut app, &fixture.path);
            app.assets[0].info.path = fifo.to_string_lossy().into_owned();
            app.timeline.add_media_clip(
                "Slow input",
                ClipKind::Video,
                0,
                0.0,
                1.0,
                0.0,
                fifo.to_str().unwrap(),
            );
            app.set_playing(true);
            let started = Instant::now();
            app.sync_playback(&ctx);
            let caller_time = started.elapsed();
            writer.join().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !app.status.starts_with("Video playback failed:") && Instant::now() < deadline {
                app.poll_frames(&ctx);
                app.sync_playback(&ctx);
                thread::sleep(Duration::from_millis(5));
            }
            eprintln!(
                "slow native open: caller={caller_time:?}; status={}",
                app.status
            );
            assert!(
                !app.playing,
                "a failed decoder must not consume an unseen clip"
            );
            let failed_generation = app.stream_generation;
            app.sync_playback(&ctx);
            assert_eq!(app.stream_generation, failed_generation);
            assert!(
                app.still_inflight.is_none(),
                "a failure must not retry through the still worker"
            );
            app.set_playing(false);
            assert!(
                app.status.starts_with("Video playback failed:"),
                "{}",
                app.status
            );
            assert!(
                caller_time < Duration::from_millis(100),
                "UI waited {caller_time:?} for the native decoder"
            );
        }

        /// Diagnostic against reporter-supplied files, not a native-window or
        /// export test. Enable explicitly; fixtures are not committed.
        #[test]
        #[ignore = "set OPENREEL_PREVIEW_MEDIA_DIR to the supplied excerpt directory"]
        fn preview_short_clip_diagnostics() {
            run_short_clip_diagnostics(false);
        }

        #[test]
        #[ignore = "set OPENREEL_PREVIEW_MEDIA_DIR to the supplied excerpt directory"]
        fn preview_short_clip_transport_diagnostics() {
            run_short_clip_diagnostics(true);
        }

        fn run_short_clip_diagnostics(transport_clock: bool) {
            let dir = std::env::var("OPENREEL_PREVIEW_MEDIA_DIR").expect("excerpt directory");
            let mut paths: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "mp4"))
                .collect();
            paths.sort();
            assert!(!paths.is_empty());
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            for path in &paths {
                add_asset(&mut app, path.to_str().unwrap());
            }
            for pass in 0..2 {
                for path in &paths {
                    let id = app.timeline.add_media_clip(
                        path.file_name().unwrap().to_str().unwrap(),
                        ClipKind::Video,
                        0,
                        app.timeline.end(),
                        0.3,
                        1.0 + pass as f64,
                        path.to_str().unwrap(),
                    );
                    app.timeline.clip_mut(id).unwrap().embedded_audio = false;
                }
            }
            let mut sync_ms = Vec::new();
            let mut uploads: HashMap<ClipId, usize> = HashMap::new();
            let mut first_uploads: HashMap<ClipId, f64> = HashMap::new();
            let mut transitions = Vec::new();
            let mut previous = None;
            let mut buffered_seconds = 0.0;
            let mut previous_wall = 0.0;
            app.set_playing(true);
            let started = Instant::now();
            loop {
                let wall = started.elapsed().as_secs_f64();
                if transport_clock {
                    assert!(wall < 30.0, "preview did not finish: {}", app.status);
                    if app.waiting_for_video() {
                        buffered_seconds += wall - previous_wall;
                    }
                    let _ = ctx.run(
                        egui::RawInput {
                            time: Some(wall),
                            ..Default::default()
                        },
                        |ctx| app.advance_playback(ctx),
                    );
                    if !app.playing {
                        assert_eq!(app.playhead, app.timeline.end(), "{}", app.status);
                        break;
                    }
                } else {
                    if wall >= app.timeline.end() {
                        break;
                    }
                    app.playhead = wall;
                    app.last_now = Some(app.playhead);
                }
                previous_wall = wall;
                let active = app.active_video_clip().unwrap();
                app.poll_frames(&ctx);
                let syncing = Instant::now();
                app.sync_playback(&ctx);
                let elapsed = syncing.elapsed().as_secs_f64() * 1000.0;
                sync_ms.push(elapsed);
                if previous != Some(active.id) {
                    transitions.push((active.id, app.playhead, elapsed));
                    previous = Some(active.id);
                }
                let preview = app.preview_texture.as_ref().map(TextureHandle::id);
                for (id, _) in ctx.tex_manager().write().take_delta().set {
                    if Some(id) == preview {
                        *uploads.entry(active.id).or_default() += 1;
                        first_uploads.entry(active.id).or_insert(wall);
                    }
                }
                thread::sleep(Duration::from_millis(8));
            }
            app.set_playing(false);
            sync_ms.sort_by(f64::total_cmp);
            eprintln!("preview short-clip diagnostics: elapsed={:?}, sync samples={}, p50_ms={:.3}, p95_ms={:.3}, max_ms={:.3}",
                started.elapsed(), sync_ms.len(), sync_ms[sync_ms.len() / 2],
                sync_ms[sync_ms.len() * 95 / 100], sync_ms.last().unwrap());
            eprintln!("transport_clock={transport_clock}, buffered_seconds={buffered_seconds:.3}");
            eprintln!("transitions (clip, timeline seconds, sync ms): {transitions:?}");
            for clip in &app.timeline.clips {
                let count = uploads.get(&clip.id).copied().unwrap_or(0);
                eprintln!(
                    "{} {} source={}+{} timeline={} uploads_while_active={} first_upload_wall={:?}",
                    clip.id,
                    clip.name,
                    clip.source_in,
                    clip.duration,
                    clip.start,
                    count,
                    first_uploads.get(&clip.id),
                );
                if transport_clock {
                    assert!(count >= 2, "{} did not deliver moving pictures", clip.name);
                }
            }
        }
    }

    fn pointer_button(x: f32, y: f32, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: egui::Pos2::new(x, y),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn run_view_frames(app: &mut EditorApp, ctx: &egui::Context, frames: &[Vec<egui::Event>]) {
        let visuals: VisualMap<'_> = [
            (
                "/media/a.mp4",
                ClipVisual {
                    duration: 30.0,
                    peaks: &[],
                    thumbs: &[],
                },
            ),
            (
                "/media/b.mp4",
                ClipVisual {
                    duration: 30.0,
                    peaks: &[],
                    thumbs: &[],
                },
            ),
        ]
        .into();
        let EditorApp {
            timeline,
            timeline_view,
            playhead,
            selected,
            ..
        } = app;
        for (i, events) in frames.iter().enumerate() {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1200.0, 400.0),
                )),
                time: Some(i as f64 * 0.016),
                events: events.clone(),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        timeline_view.show(ui, timeline, playhead, selected, &visuals);
                    });
            });
        }
    }

    #[test]
    fn console_enter_runs_edits_and_keeps_input_ready() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        app.show_console = true;
        let mut time = 0.0;
        let mut render = |app: &mut EditorApp, events: Vec<egui::Event>| {
            time += 0.02;
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(800.0, 600.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| app.console_panel(ctx),
            )
        };
        render(&mut app, Vec::new());
        let output = render(&mut app, Vec::new());
        let input = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                Shape::Text(text)
                    if text.galley.text() == "JavaScript. help() lists the editor API." =>
                {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            })
            .expect("console input");
        for pressed in [true, false] {
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(input),
                    pointer_button(input.x, input.y, pressed),
                ],
            );
        }
        for (code, expected_count) in [
            ("addTitle('Intro', 3, 2)", 1),
            ("", 1),
            ("addTitle('Outro', 7, 1)", 2),
        ] {
            render(&mut app, vec![egui::Event::Paste(code.to_owned())]);
            render(
                &mut app,
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                }],
            );
            render(
                &mut app,
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                }],
            );
            assert_eq!(app.timeline.clips.len(), expected_count, "submitted {code}");
            assert!(app.console_input.is_empty());
        }
        let intro = &app.timeline.clips[0];
        assert_eq!(
            (&*intro.name, intro.start, intro.duration),
            ("Intro", 3.0, 2.0)
        );
        let outro = &app.timeline.clips[1];
        assert_eq!(
            (&*outro.name, outro.start, outro.duration),
            ("Outro", 7.0, 1.0)
        );
    }

    #[test]
    fn inspecting_media_does_not_change_scene_timing() {
        for (start, duration) in [(0.157, 0.001), (2.135, 3.2866666666666666 - 2.135)] {
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            let video = app.timeline.add_media_clip(
                "Scene",
                ClipKind::Video,
                0,
                start,
                duration,
                2.135,
                "/media/scene.mp4",
            );
            let audio = app.timeline.detach_audio(video).unwrap();
            let expected = app.timeline.clone();
            let mut time = 0.0;
            for selected in [video, audio] {
                app.selected = Some(selected);
                for _ in 0..3 {
                    time += 0.02;
                    let _ = ctx.run(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                Vec2::new(1200.0, 800.0),
                            )),
                            time: Some(time),
                            ..Default::default()
                        },
                        |ctx| app.right_dock(ctx),
                    );
                    assert_eq!(
                        app.timeline, expected,
                        "viewing Details must not edit media"
                    );
                }
            }
        }
    }

    #[test]
    fn inspector_start_moves_both_members_of_a_linked_pair() {
        for edit_audio in [false, true] {
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            let video = app.timeline.add_media_clip(
                "Picture",
                ClipKind::Video,
                0,
                0.0,
                4.0,
                0.0,
                "/media/a.mp4",
            );
            let audio = app.timeline.detach_audio(video).unwrap();
            app.selected = Some(if edit_audio { audio } else { video });
            let mut time = 0.0;
            let mut render = |app: &mut EditorApp, events: Vec<egui::Event>| {
                time += 0.02;
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(400.0, 800.0),
                        )),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| app.inspector_content(ui));
                    },
                )
            };
            render(&mut app, Vec::new());
            let output = render(&mut app, Vec::new());
            let start = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Text(text)
                        if text
                            .galley
                            .text()
                            .trim_end_matches(" s")
                            .parse::<f64>()
                            .ok()
                            == Some(0.0) =>
                    {
                        Some(text.visual_bounding_rect().center())
                    }
                    _ => None,
                })
                .expect("Start numeric field");
            for pressed in [true, false, true, false] {
                render(
                    &mut app,
                    vec![
                        egui::Event::PointerMoved(start),
                        pointer_button(start.x, start.y, pressed),
                    ],
                );
            }
            render(
                &mut app,
                vec![
                    egui::Event::Key {
                        key: egui::Key::A,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers {
                            ctrl: true,
                            command: true,
                            ..Default::default()
                        },
                    },
                    egui::Event::Paste("5".to_owned()),
                ],
            );
            render(
                &mut app,
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                }],
            );
            render(&mut app, Vec::new());
            let selected = app.timeline.clip(app.selected.unwrap()).unwrap();
            assert_eq!(selected.start, 5.0, "the real inspector field was edited");
            assert_eq!(app.timeline.clip(video).unwrap().start, 5.0);
            assert_eq!(app.timeline.clip(audio).unwrap().start, 5.0);
            assert_eq!(app.timeline.clip(video).unwrap().linked_clip, Some(audio));
            assert_eq!(app.timeline.clip(audio).unwrap().linked_clip, Some(video));
        }
    }

    /// Regression: left-trimming clip B (which follows clip A) with the
    /// magnet on must keep B's end fixed after a full app frame, not
    /// just inside the timeline view. `EditorApp::update` used to run
    /// its own per-frame `close_gaps` after the panels, rippling B
    /// back to A's end in the same frame the drag ended and defeating
    /// the view-level TrimLeft carve-out.
    #[test]
    fn left_trim_survives_app_frame_with_magnet_on() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        assert!(app.timeline_view.magnet);
        let a_id = app.timeline.add_media_clip(
            "a.mp4",
            ClipKind::Video,
            1,
            0.0,
            10.0,
            0.0,
            "/media/a.mp4",
        );
        let b_id = app.timeline.add_media_clip(
            "b.mp4",
            ClipKind::Video,
            1,
            12.0,
            10.0,
            0.0,
            "/media/b.mp4",
        );
        // B spans t=12..22, so its left edge is x=720 (144 + 12*48) at
        // track row y=99. Drag the edge 144px right (+3s) and release.
        let frames = vec![
            vec![],
            vec![pointer_button(720.0, 99.0, true)],
            vec![egui::Event::PointerMoved(egui::Pos2::new(864.0, 99.0))],
            vec![pointer_button(864.0, 99.0, false)],
        ];
        run_view_frames(&mut app, &ctx, &frames);
        let b = app.timeline.clip(b_id).unwrap();
        assert!(
            (b.start - 15.0).abs() < 0.02 && (b.end() - 22.0).abs() < 1e-6,
            "drag alone should land B at 15..22, got {}..{}",
            b.start,
            b.end()
        );

        // One full app frame tail, exactly as update() runs it.
        app.finish_frame(&ctx, None);
        let a = app.timeline.clip(a_id).unwrap();
        assert_eq!(a.start, 0.0);
        assert!((a.end() - 10.0).abs() < 1e-6);
        let b = app.timeline.clip(b_id).unwrap();
        assert!(
            (b.start - 15.0).abs() < 0.02,
            "left trim gap must survive the app frame, got start {}",
            b.start
        );
        assert!(
            (b.end() - 22.0).abs() < 1e-6,
            "left trim must keep the end fixed across app frames, got {}",
            b.end()
        );
        assert!((b.source_in - 3.0).abs() < 0.02);
    }

    /// Free-form timeline: deleting a clip leaves its gap, magnet
    /// on included. The magnet only snaps edges while dragging.
    #[test]
    fn delete_with_magnet_keeps_gap() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        assert!(app.timeline_view.magnet);
        app.timeline
            .add_clip("a.mp4", ClipKind::Video, 0, 0.0, 10.0, 0.0);
        let b_id = app
            .timeline
            .add_clip("b.mp4", ClipKind::Video, 0, 10.0, 10.0, 0.0);
        let c_id = app
            .timeline
            .add_clip("c.mp4", ClipKind::Video, 0, 20.0, 10.0, 0.0);
        app.selected = Some(b_id);
        app.handle_action(&ctx, UiAction::DeleteSelected);
        assert!(app.timeline.clip(b_id).is_none());
        let c = app.timeline.clip(c_id).unwrap();
        assert!(
            (c.start - 20.0).abs() < 1e-6,
            "magnet delete should leave the gap, got {}",
            c.start
        );
    }

    /// Split acts on the playhead, not on a click point: with no
    /// selection it cuts the clip under the playhead.
    #[test]
    fn split_at_playhead_cuts_clip_under_playhead_without_selection() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let clip_id = app
            .timeline
            .add_clip("a.mp4", ClipKind::Video, 0, 0.0, 10.0, 2.0);
        app.selected = None;
        app.playhead = 4.0;
        app.handle_action(&ctx, UiAction::SplitSelected);
        assert_eq!(app.timeline.clips.len(), 2);
        let left = app.timeline.clip(clip_id).unwrap();
        assert!((left.end() - 4.0).abs() < 0.02, "left end {}", left.end());
        let right_id = app.selected.expect("right half selected");
        let right = app.timeline.clip(right_id).unwrap();
        assert!((right.start - 4.0).abs() < 0.02);
        assert!((right.source_in - 6.0).abs() < 0.02);
    }

    /// A second import requested while one is running must queue
    /// (with placeholders visible right away), not be dropped.
    #[test]
    fn import_while_busy_queues_instead_of_dropping() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let first = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../samples/sample_video.mp4"
        ));
        let second = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../samples/sample_music.mp3"
        ));
        app.spawn_import(&ctx, vec![first]);
        assert!(app.import_rx.is_some());
        app.spawn_import(&ctx, vec![second.clone()]);
        assert_eq!(app.import_queue, vec![second]);
        assert_eq!(app.assets.len(), 2, "both files show as placeholders");
    }

    /// The monitor scrubber spans the whole edit: a click a quarter of the
    /// way along moves the playhead a quarter of the way through it.
    #[test]
    fn transport_scrubber_click_seeks_proportionally() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        app.timeline
            .add_clip("Intro", ClipKind::Title, 0, 0.0, 8.0, 0.0);
        let mut time = 0.0;
        let mut render = |app: &mut EditorApp, events| {
            time += 0.05;
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(900.0, 600.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| app.central(ctx),
            )
        };
        render(&mut app, Vec::new());
        let output = render(&mut app, Vec::new());
        let text_rect = |label: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Text(text) if text.galley.text() == label => {
                        Some(text.visual_bounding_rect())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("transport readout {label}"))
        };
        let position = text_rect(&format_timecode(0.0, app.timeline.fps));
        let length = text_rect(&format_timecode(8.0, app.timeline.fps));
        // The track runs from 6 px left of the position readout to 6 px
        // right of the length readout, 29 px above their centre line.
        let (left, right) = (position.left() - 6.0, length.right() + 6.0);
        let target = egui::pos2(left + (right - left) * 0.25, position.center().y - 29.0);
        render(&mut app, vec![egui::Event::PointerMoved(target)]);
        for pressed in [true, false] {
            render(&mut app, vec![pointer_button(target.x, target.y, pressed)]);
        }
        assert!(
            (app.playhead - 2.0).abs() < 0.1,
            "a click at 25% of the scrubber seeks to 2 s, got {}",
            app.playhead
        );
        assert!(!app.transport_scrubbing, "release ends the scrub");
    }

    /// A finished import selects itself only when nothing is selected, so
    /// a background batch never takes the curator card from the user.
    #[test]
    fn finished_import_keeps_the_users_selection() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let music = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../samples/sample_music.mp3"
        );
        let import = |app: &mut EditorApp| {
            app.spawn_import(&ctx, vec![PathBuf::from(music)]);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while app.import_rx.is_some() {
                app.poll_imports(&ctx);
                assert!(std::time::Instant::now() < deadline, "import timed out");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };

        import(&mut app);
        assert_eq!(
            app.selected_asset.as_deref(),
            Some(music),
            "an import fills an empty selection"
        );

        app.assets.retain(|asset| asset.info.path != music);
        app.assets.push(media::BinAsset {
            info: MediaInfo {
                path: "/media/picked.mp4".to_owned(),
                name: "picked.mp4".to_owned(),
                duration: 10.0,
                width: 640,
                height: 360,
                fps: 30.0,
                has_video: true,
                has_audio: false,
            },
            size_bytes: 0,
            state: media::AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumb_textures: Vec::new(),
            sweep: sweep::SweepData::default(),
        });
        app.selected_asset = Some("/media/picked.mp4".to_owned());
        import(&mut app);
        assert_eq!(
            app.selected_asset.as_deref(),
            Some("/media/picked.mp4"),
            "a finished import must not replace the user's selection"
        );
    }

    /// Full JS surface end to end, offline: queued imports of real
    /// downloads, local curator sweep, console JS building fast
    /// cuts (the same eval + ScriptCommand path chat output uses),
    /// then export. Ignored by default; needs /tmp/clips and
    /// /tmp/rockets populated.
    #[test]
    #[ignore = "needs downloaded clips in /tmp/clips and /tmp/rockets"]
    fn js_console_fast_cuts_export_e2e() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let batch1 = vec![
            PathBuf::from("/tmp/clips/ocean02.webm"),
            PathBuf::from("/tmp/clips/interview60.mp4"),
        ];
        let batch2 = vec![
            PathBuf::from("/tmp/clips/cairo.webm"),
            PathBuf::from("/tmp/rockets/crew12.webm"),
        ];
        for path in batch1.iter().chain(&batch2) {
            assert!(path.exists(), "missing test asset {}", path.display());
        }
        app.spawn_import(&ctx, batch1);
        app.spawn_import(&ctx, batch2);
        assert_eq!(app.import_queue.len(), 2, "second batch queues");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let mut curation_started = false;
        loop {
            let _ = ctx.run(egui::RawInput::default(), |_| {});
            app.poll_imports(&ctx);
            app.poll_sweeps(&ctx);
            let imports_done = app.import_rx.is_none() && app.import_queue.is_empty();
            if imports_done && app.assets.len() == 4 && !curation_started {
                assert!(
                    app.assets
                        .iter()
                        .all(|a| a.sweep.status == sweep::SweepStatus::NotStarted),
                    "import must not auto-curate"
                );
                app.curate_all_new(&ctx);
                curation_started = true;
            }
            let sweeps_done = curation_started
                && app.sweep_rx.is_none()
                && app.sweep_queue.is_empty()
                && app.assets.iter().all(|a| {
                    matches!(
                        a.sweep.status,
                        sweep::SweepStatus::Ready | sweep::SweepStatus::Failed
                    )
                });
            if imports_done && sweeps_done && app.assets.len() == 4 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "import/sweep timed out: assets={} queue={}",
                app.assets.len(),
                app.import_queue.len()
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(
            app.assets
                .iter()
                .all(|a| a.state == media::AssetState::Ready),
            "all assets ready"
        );

        // Curator output is classified and inside the vocabularies.
        let mut total_candidates = 0;
        for asset in &app.assets {
            let candidates = &asset.sweep.candidates;
            total_candidates += candidates.len();
            let primes = candidates.iter().filter(|c| c.is_prime()).count();
            let quota = (candidates.len() as f32 * 0.2).ceil() as usize;
            assert!(primes <= quota, "prime quota per asset");
            for c in candidates {
                assert!(
                    sweep::CURATOR_CATEGORIES.contains(&c.category.as_str()),
                    "bad category {}",
                    c.category
                );
                assert!(
                    c.tags
                        .iter()
                        .all(|t| sweep::CURATOR_TAGS.contains(&t.as_str())),
                    "bad tags {:?}",
                    c.tags
                );
                if c.is_prime() {
                    assert!(c.score >= 75.0, "prime below score floor");
                }
            }
        }
        assert!(total_candidates > 0, "curator produced candidates");

        // The console path chat uses: eval JS, apply ScriptCommands.
        app.run_console(
            &ctx,
            "var primes = sweepCandidates().filter(c => c.tags.includes('prime'));\n\
             console.log('prime-count:' + primes.length);"
                .to_owned(),
        );
        app.run_console(
            &ctx,
            "addAssetRange('ocean02.webm', 0, 2, 4);\n\
             addAssetRange('cairo.webm', 4, 1, 3);\n\
             addAssetRange('interview60.mp4', 7, 5, 5);\n\
             addAssetRange('crew12.webm', 12, 20, 6);\n\
             addTitle('FAST CUTS', 0, 2);"
                .to_owned(),
        );
        app.run_console(
            &ctx,
            "var cs = clips().filter(c => c.kind === 'video').sort((a, b) => a.start - b.start);\n\
             trimClip(cs[0].id, 0.5, -1);\n\
             trimClip(cs[3].id, -1, 17);\n\
             splitClip(cs[2].id, cs[2].start + 2);\n\
             moveClip(cs[1].id, 40);\n\
             closeGaps();\n\
             seek(3);"
                .to_owned(),
        );
        let errors: Vec<&String> = app
            .console_output
            .iter()
            .filter(|line| line.starts_with("error"))
            .collect();
        assert!(errors.is_empty(), "console errors: {errors:?}");
        assert!(
            app.console_output
                .iter()
                .any(|line| line.contains("prime-count:")),
            "curator categories visible to JS: {:?}",
            app.console_output
        );
        assert!((app.playhead - 3.0).abs() < 0.02, "seek via JS");
        let videos = app
            .timeline
            .clips
            .iter()
            .filter(|c| c.kind == ClipKind::Video)
            .count();
        assert_eq!(videos, 5, "4 adds + 1 split");
        // No same-lane overlaps survive a JS edit session.
        for a in &app.timeline.clips {
            for b in &app.timeline.clips {
                if a.id < b.id && a.track == b.track {
                    assert!(
                        a.end() <= b.start + 1e-6 || b.end() <= a.start + 1e-6,
                        "overlap on lane {}: {} vs {}",
                        a.track,
                        a.name,
                        b.name
                    );
                }
            }
        }
        let expected_end = app.timeline.end();
        assert!(expected_end > 10.0, "timeline built, end {expected_end}");

        let output = "/tmp/js_fastcuts.mp4";
        let _ = std::fs::remove_file(output);
        app.run_console(&ctx, format!("exportTo('{output}')"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            app.poll_export();
            if app.export_rx.is_none() && PathBuf::from(output).exists() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "export timed out, status: {}",
                app.status
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let info = openreel_media::probe(output).expect("probe JS export");
        println!(
            "JS export: {:.2}s {}x{} status={}",
            info.duration, info.width, info.height, app.status
        );
        assert!(info.has_video);
        assert!(
            (info.duration - expected_end).abs() < 0.6,
            "export duration {} vs timeline {expected_end}",
            info.duration
        );
    }

    /// Recap rehearsal, offline: finished pieces go in as assets,
    /// the curator slices and grades them, then console JS written
    /// the way the model is taught to builds a grouped mashup with
    /// title cards and exports it. Ignored; needs prior renders.
    #[test]
    #[ignore = "needs rendered pieces in /tmp plus downloads"]
    fn recap_rehearsal_from_finished_pieces_e2e() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let pieces = vec![
            PathBuf::from("/tmp/spacex_countdown.mp4"),
            PathBuf::from("/tmp/js_fastcuts.mp4"),
            PathBuf::from("/tmp/clips/ocean02.webm"),
        ];
        for path in &pieces {
            assert!(path.exists(), "missing piece {}", path.display());
        }
        app.spawn_import(&ctx, pieces);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let mut curation_started = false;
        loop {
            let _ = ctx.run(egui::RawInput::default(), |_| {});
            app.poll_imports(&ctx);
            app.poll_sweeps(&ctx);
            if app.import_rx.is_none() && app.assets.len() == 3 && !curation_started {
                app.curate_all_new(&ctx);
                curation_started = true;
            }
            let done = curation_started
                && app.import_rx.is_none()
                && app.sweep_rx.is_none()
                && app.sweep_queue.is_empty()
                && app.assets.len() == 3
                && app.assets.iter().all(|a| {
                    matches!(
                        a.sweep.status,
                        sweep::SweepStatus::Ready | sweep::SweepStatus::Failed
                    )
                });
            if done {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "rehearsal import timeout"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let bed_candidates: usize = app
            .assets
            .iter()
            .flat_map(|a| a.sweep.candidates.iter())
            .filter(|c| c.music_bed)
            .count();
        println!("music-bed candidates across pieces: {bed_candidates}");

        app.run_console(
            &ctx,
            "var byAsset = {};\n\
             sweepCandidates().forEach(function (c) { if (c.hasVideo) { (byAsset[c.asset] = byAsset[c.asset] || []).push(c); } });\n\
             clips().forEach(function (c) { deleteClip(c.id); });\n\
             var cursor = 0;\n\
             Object.keys(byAsset).forEach(function (asset) {\n\
               var picks = byAsset[asset].slice(0, 3);\n\
               addTitle(picks[0].assetName, cursor, 1.5);\n\
               picks.forEach(function (c) { var d = Math.min(c.duration, 4); addAssetRange(c.asset, cursor, c.sourceIn, d); cursor += d; });\n\
             });\n\
             console.log('Done — recap rehearsal built.');"
                .to_owned(),
        );
        let errors: Vec<&String> = app
            .console_output
            .iter()
            .filter(|line| line.starts_with("error"))
            .collect();
        assert!(errors.is_empty(), "console errors: {errors:?}");
        let videos = app
            .timeline
            .clips
            .iter()
            .filter(|c| c.kind == ClipKind::Video)
            .count();
        let titles = app
            .timeline
            .clips
            .iter()
            .filter(|c| c.kind == ClipKind::Title)
            .count();
        assert!(videos >= 6, "grouped picks landed: {videos}");
        assert!(titles >= 3, "place cards per asset: {titles}");
        let expected_end = app.timeline.end();

        let output = "/tmp/recap_rehearsal.mp4";
        let _ = std::fs::remove_file(output);
        app.run_console(&ctx, format!("exportTo('{output}')"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            app.poll_export();
            if app.export_rx.is_none() && PathBuf::from(output).exists() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "export timeout: {}",
                app.status
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let info = openreel_media::probe(output).expect("probe rehearsal");
        println!(
            "rehearsal: {:.2}s clips={} status={}",
            info.duration,
            app.timeline.clips.len(),
            app.status
        );
        assert!((info.duration - expected_end).abs() < 0.6);
    }

    #[test]
    #[ignore = "needs OPENREEL_MONTAGE_DIR pointing at the local montage corpus"]
    fn local_montage_corpus_curates_and_builds_recap_rehearsal() {
        let Ok(dir) = std::env::var("OPENREEL_MONTAGE_DIR") else {
            println!("SKIP local montage corpus: OPENREEL_MONTAGE_DIR is not set");
            return;
        };
        let mut pieces: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("read montage dir")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ["mp4", "mov", "mkv", "webm"].contains(&ext.to_lowercase().as_str()))
                    .unwrap_or(false)
            })
            .collect();
        pieces.sort();
        assert!(!pieces.is_empty(), "no montage videos in {dir}");
        println!("montage corpus: {} file(s) in {dir}", pieces.len());

        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let expected_assets = pieces.len();
        app.spawn_import(&ctx, pieces);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        let mut curation_started = false;
        loop {
            let _ = ctx.run(egui::RawInput::default(), |_| {});
            app.poll_imports(&ctx);
            app.poll_sweeps(&ctx);
            if app.import_rx.is_none() && app.assets.len() == expected_assets && !curation_started {
                app.curate_all_new(&ctx);
                curation_started = true;
            }
            let done = curation_started
                && app.import_rx.is_none()
                && app.sweep_rx.is_none()
                && app.sweep_queue.is_empty()
                && app.assets.len() == expected_assets
                && app.assets.iter().all(|asset| {
                    matches!(
                        asset.sweep.status,
                        sweep::SweepStatus::Ready | sweep::SweepStatus::Failed
                    )
                });
            if done {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "montage import/curation timeout: {}",
                app.status
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        for asset in &app.assets {
            let beds = asset
                .sweep
                .candidates
                .iter()
                .filter(|candidate| candidate.music_bed)
                .count();
            let visual_candidates = asset
                .sweep
                .candidates
                .iter()
                .filter(|candidate| candidate.visual.is_some())
                .count();
            let covered: f64 = asset
                .sweep
                .scene_clips
                .iter()
                .map(|scene| scene.duration)
                .sum();
            println!(
                "curated {}: status={:?} picks={} scenes={} sceneCoverage={covered:.2}/{:.2}s musicBedPicks={} visualPicks={} note={}",
                asset.info.name,
                asset.sweep.status,
                asset.sweep.candidates.len(),
                asset.sweep.scene_clips.len(),
                asset.info.duration,
                beds,
                visual_candidates,
                asset.sweep.note,
            );
            assert!(
                !asset.sweep.scene_clips.is_empty(),
                "no scene clips for {}",
                asset.info.name
            );
            assert!(
                asset.sweep.scene_clips[0].source_in.abs() < 0.001,
                "scene breakdown starts at zero"
            );
            for pair in asset.sweep.scene_clips.windows(2) {
                assert!(
                    (pair[0].end() - pair[1].source_in).abs() < 0.001,
                    "scene breakdown is contiguous for {}",
                    asset.info.name
                );
            }
            let last_scene = asset.sweep.scene_clips.last().expect("last scene");
            assert!(
                (last_scene.end() - asset.info.duration).abs() < 0.01,
                "scene breakdown ends at source duration for {}",
                asset.info.name
            );
            assert!(
                (covered - asset.info.duration).abs() < 0.01,
                "scene breakdown covers the whole source for {}",
                asset.info.name
            );
            let scene_ranges: Vec<String> = asset
                .sweep
                .scene_clips
                .iter()
                .map(|scene| format!("{:.2}-{:.2}", scene.source_in, scene.end()))
                .collect();
            println!(
                "scene ranges {}: {}",
                asset.info.name,
                scene_ranges.join(" ")
            );
            assert!(
                !asset.sweep.candidates.is_empty(),
                "no candidates for {}",
                asset.info.name
            );
            assert_eq!(
                visual_candidates,
                asset.sweep.candidates.len(),
                "every video candidate carries local visual evidence"
            );
            for candidate in &asset.sweep.candidates {
                let visual = candidate.visual_json();
                assert_eq!(visual["source"], "local-proxy");
                assert_eq!(visual["samStatus"], "unavailable");
                assert!(visual["facePresent"].is_null());
                assert!(visual["expression"].is_null());
            }
        }

        app.run_console(
            &ctx,
            "var byAsset = {};\n\
             sweepCandidates().forEach(function (c) { if (c.hasVideo) { (byAsset[c.asset] = byAsset[c.asset] || []).push(c); } });\n\
             clips().forEach(function (c) { deleteClip(c.id); });\n\
             var cursor = 0;\n\
             addTitle('2025 RECAP', 0, 2, 'pop-caption');\n\
             Object.keys(byAsset).sort().forEach(function (asset) {\n\
               byAsset[asset].slice(0, 2).forEach(function (c) { var d = Math.min(c.duration, 3); addAssetRange(c.asset, cursor, c.sourceIn, d); cursor += d; });\n\
             });\n\
             console.log('Done — local recap rehearsal built.');"
                .to_owned(),
        );
        let errors: Vec<&String> = app
            .console_output
            .iter()
            .filter(|line| line.starts_with("error"))
            .collect();
        assert!(errors.is_empty(), "console errors: {errors:?}");
        let videos = app
            .timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Video)
            .count();
        assert!(videos >= expected_assets, "grouped picks landed: {videos}");
        assert!(app.timeline.clips.iter().any(|clip| {
            clip.kind == ClipKind::Title
                && clip.title_style == openreel_engine::TitleStyle::PopCaption
        }));
        let expected_end = app.timeline.end();

        let output = std::env::var("OPENREEL_RECAP_OUTPUT")
            .unwrap_or_else(|_| "/tmp/recap_2025_local_rehearsal.mp4".to_owned());
        let _ = std::fs::remove_file(&output);
        app.run_console(&ctx, format!("exportTo('{output}')"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        loop {
            app.poll_export();
            if app.export_rx.is_none() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "recap export timeout: {}",
                app.status
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(app.status.starts_with("Exported "), "{}", app.status);
        let info = openreel_media::probe(&output).expect("probe local recap");
        assert!(
            info.has_video && info.has_audio,
            "recap must contain picture and sound"
        );
        println!(
            "local recap: {:.2}s clips={} status={} output={}",
            info.duration,
            app.timeline.clips.len(),
            app.status,
            output,
        );
        assert!((info.duration - expected_end).abs() < 0.6);
    }

    #[test]
    fn video_of_the_other_orientation_is_never_added_to_the_project() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        // Displayed sizes: a rotated phone clip probes as portrait.
        for (name, width, height) in [("wide.mp4", 1920, 1080), ("phone.mov", 1080, 1920)] {
            let path = format!("/media/{name}");
            let scene = sweep::SweepCandidate {
                id: format!("{path}#scene-0"),
                asset_path: path.clone(),
                asset_name: name.to_owned(),
                title: name.to_owned(),
                source_in: 1.0,
                duration: 2.0,
                score: 80.0,
                reason: "Detected shot.".to_owned(),
                stability: 0.9,
                audio_energy: 0.4,
                audio_peak: 0.7,
                category: "broll".to_owned(),
                tags: Vec::new(),
                motion_score: 0.3,
                audio_score: 0.5,
                music_bed: false,
                visual: None,
                has_video: true,
                transcript: Vec::new(),
                review: sweep::ClipReview::default(),
                dismissed: false,
            };
            app.assets.push(media::BinAsset {
                info: MediaInfo {
                    path,
                    name: name.to_owned(),
                    duration: 10.0,
                    width,
                    height,
                    fps: 30.0,
                    has_video: true,
                    has_audio: true,
                },
                size_bytes: 0,
                state: media::AssetState::Ready,
                peaks: Vec::new(),
                beats: None,
                thumb_textures: Vec::new(),
                sweep: sweep::SweepData {
                    candidates: vec![scene.clone()],
                    scene_clips: vec![scene],
                    ..Default::default()
                },
            });
        }

        // Bulk adds skip the clashing source instead of leaving its slot.
        app.add_all_scene_clips();
        let sources: Vec<_> = app
            .timeline
            .clips
            .iter()
            .map(|c| c.source.clone())
            .collect();
        assert_eq!(sources, vec![Some("/media/wide.mp4".to_owned())]);
        assert_eq!(app.timeline.end(), 2.0);
        assert!(app.status.contains("skipped 1"), "{}", app.status);

        // Every single-add path, scripts included, shares the refusal.
        let refused =
            app.try_add_asset_range_to_timeline("/media/phone.mov", "", None, 5.0, 0.0, 1.0);
        assert_eq!(
            refused,
            Err("phone.mov is portrait; the project is landscape.".to_owned())
        );
        assert_eq!(app.timeline.clips.len(), 1);

        // A portrait project accepts it, and the canvas stays portrait.
        app.set_format(Orientation::Portrait);
        assert!(
            app.status.contains("1 clip doesn't match"),
            "{}",
            app.status
        );
        app.try_add_asset_range_to_timeline("/media/phone.mov", "", None, 5.0, 0.0, 1.0)
            .expect("portrait video fits a portrait project");
        app.handle_action(&ctx, UiAction::ClearTimeline);
        assert_eq!(app.timeline.orientation, Orientation::Portrait);
    }

    #[test]
    fn adding_all_scene_clips_reconstructs_the_source_in_order() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let scene =
            |index: usize, source_in: f64, duration: f64, score: f32| sweep::SweepCandidate {
                id: format!("/media/montage.mp4#scene-{index}"),
                asset_path: "/media/montage.mp4".to_owned(),
                asset_name: "montage.mp4".to_owned(),
                title: format!("montage.mp4 · scene {index}"),
                source_in,
                duration,
                score,
                reason: "Detected shot.".to_owned(),
                stability: 0.9,
                audio_energy: 0.4,
                audio_peak: 0.7,
                category: "broll".to_owned(),
                tags: Vec::new(),
                motion_score: 0.3,
                audio_score: 0.5,
                music_bed: false,
                visual: None,
                has_video: true,
                transcript: Vec::new(),
                review: sweep::ClipReview::default(),
                dismissed: false,
            };
        app.assets.push(media::BinAsset {
            info: MediaInfo {
                path: "/media/montage.mp4".to_owned(),
                name: "montage.mp4".to_owned(),
                duration: 20.0,
                width: 854,
                height: 480,
                fps: 30.0,
                has_video: true,
                has_audio: true,
            },
            size_bytes: 0,
            state: media::AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumb_textures: Vec::new(),
            sweep: sweep::SweepData {
                candidates: vec![scene(99, 8.0, 2.0, 99.0)],
                scene_clips: vec![
                    scene(2, 8.0, 7.0, 10.0),
                    scene(0, 0.0, 3.0, 90.0),
                    scene(3, 15.0, 5.0, 50.0),
                    scene(1, 3.0, 5.0, 70.0),
                ],
                ..Default::default()
            },
        });

        app.add_all_scene_clips();
        let mut clips: Vec<_> = app.timeline.clips.iter().collect();
        clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        assert_eq!(clips.len(), 4, "scene action ignores ranked picks");
        assert_eq!(
            clips.iter().map(|clip| clip.source_in).collect::<Vec<_>>(),
            vec![0.0, 3.0, 8.0, 15.0]
        );
        for pair in clips.windows(2) {
            assert_eq!(pair[0].end(), pair[1].start);
        }
        assert_eq!(app.timeline.end(), 20.0);

        for boundaries in [
            vec![
                0.0,
                1.0 / 3.0,
                49.0 / 60.0,
                1.235,
                1.585,
                2.135,
                3.2866666666666666,
            ],
            vec![0.0, 1.0 / 120.0, 1.0 / 60.0, 0.157, 0.158, 0.5],
        ] {
            app.assets[0].info.duration = *boundaries.last().unwrap();
            app.assets[0].sweep.scene_clips = boundaries
                .windows(2)
                .enumerate()
                .map(|(index, pair)| scene(index, pair[0], pair[1] - pair[0], 70.0))
                .rev()
                .collect();
            let count = boundaries.len() - 1;
            for route in ["all scenes", "individual", "console"] {
                app.timeline = Timeline::empty_project();
                for batch in 0..2 {
                    let start = app.timeline.end();
                    match route {
                        "all scenes" => app.add_all_scene_clips(),
                        "individual" => {
                            for index in 0..count {
                                app.add_sweep_candidate(&format!("/media/montage.mp4#scene-{index}"));
                            }
                        }
                        "console" => app.run_console(
                            &ctx,
                            "(() => { let at = clips().reduce((end, c) => Math.max(end, c.start + c.duration), 0); sceneClips().sort((a, b) => a.sourceIn - b.sourceIn).forEach(c => { addAssetRange(c.asset, at, c.sourceIn, c.duration); at += c.duration; }); })()".to_owned(),
                        ),
                        _ => unreachable!(),
                    }
                    assert_eq!(app.timeline.clips.len(), (batch + 1) * count, "{route}");
                    assert_eq!(app.timeline.track_labels.len(), 1, "{route}: one free lane");
                    let clips = &app.timeline.clips[batch * count..];
                    for (clip, pair) in clips.iter().zip(boundaries.windows(2)) {
                        assert_eq!(clip.track, 0, "{route}");
                        assert!(
                            (clip.start - start - pair[0]).abs() < 1e-12,
                            "{route}: {clip:?}"
                        );
                        assert_eq!(clip.source_in, pair[0], "{route}");
                        assert_eq!(clip.duration, pair[1] - pair[0], "{route}");
                    }
                    for pair in app.timeline.clips.windows(2) {
                        assert_eq!(
                            pair[0].end(),
                            pair[1].start,
                            "{route}: contiguous placement"
                        );
                    }
                    assert!(
                        (app.timeline.end() - start - boundaries.last().unwrap()).abs() < 1e-12
                    );
                }
            }
        }

        let placed = app.timeline.clips.clone();
        let gap = app
            .try_add_asset_range_to_timeline(
                "/media/montage.mp4",
                "after a gap",
                None,
                3.015,
                0.0,
                0.137,
            )
            .unwrap();
        let overlap = app
            .try_add_asset_range_to_timeline("/media/montage.mp4", "overlap", None, 3.04, 0.1, 0.01)
            .unwrap();
        let explicit = app
            .try_add_asset_range_to_timeline(
                "/media/montage.mp4",
                "explicit lane",
                Some(0),
                3.045,
                0.2,
                0.01,
            )
            .unwrap();
        let clamped = app
            .try_add_asset_range_to_timeline(
                "/media/montage.mp4",
                "source end",
                None,
                4.0,
                0.49,
                1.0,
            )
            .unwrap();
        assert_eq!(
            &app.timeline.clips[..placed.len()],
            placed.as_slice(),
            "adds do not ripple existing clips"
        );
        assert_eq!(app.timeline.clip(gap).unwrap().start, 3.015);
        assert_eq!(app.timeline.clip(gap).unwrap().track, 0);
        assert_eq!(app.timeline.clip(overlap).unwrap().track, 1);
        assert_eq!(app.timeline.clip(explicit).unwrap().track, 0);
        assert!((app.timeline.clip(clamped).unwrap().duration - 0.01).abs() < 1e-12);
    }

    #[test]
    fn adding_a_music_bed_range_starts_muted() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let candidate = |music_bed: bool| sweep::SweepCandidate {
            id: "/media/montage.mp4#0".to_owned(),
            asset_path: "/media/montage.mp4".to_owned(),
            asset_name: "montage.mp4".to_owned(),
            title: "montage.mp4 · 0:00".to_owned(),
            source_in: 0.0,
            duration: 20.0,
            score: 80.0,
            reason: "test".to_owned(),
            stability: 0.9,
            audio_energy: 0.6,
            audio_peak: 0.9,
            category: "broll".to_owned(),
            tags: Vec::new(),
            motion_score: 0.3,
            audio_score: 0.8,
            music_bed,
            visual: None,
            has_video: true,
            transcript: Vec::new(),
            review: sweep::ClipReview::default(),
            dismissed: false,
        };
        let asset = |music_bed: bool| media::BinAsset {
            info: MediaInfo {
                path: "/media/montage.mp4".to_owned(),
                name: "montage.mp4".to_owned(),
                duration: 60.0,
                width: 854,
                height: 480,
                fps: 30.0,
                has_video: true,
                has_audio: true,
            },
            size_bytes: 0,
            state: media::AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumb_textures: Vec::new(),
            sweep: sweep::SweepData {
                candidates: vec![candidate(music_bed)],
                ..Default::default()
            },
        };
        app.assets.push(asset(true));
        app.add_asset_range_to_timeline("/media/montage.mp4", "bed", None, 0.0, 2.0, 6.0);
        let muted = app.timeline.clips.last().expect("clip added");
        assert_eq!(muted.volume, 0.0, "music-bed range starts muted");

        app.mute_music_beds = false;
        app.add_asset_range_to_timeline("/media/montage.mp4", "bed again", None, 20.0, 2.0, 6.0);
        let audible = app.timeline.clips.last().expect("clip added");
        assert_eq!(
            audible.volume, 1.0,
            "the same music bed stays audible when the user opts out of muting"
        );

        app.mute_music_beds = true;
        app.assets[0].sweep.scene_clips = vec![candidate(true)];
        let mut speech = candidate(false);
        speech.source_in = 2.0;
        speech.duration = 6.0;
        speech.review.status = sweep::ReviewStatus::Reviewed;
        app.assets[0].sweep.candidates = vec![speech];
        app.add_asset_range_to_timeline(
            "/media/montage.mp4",
            "reviewed speech",
            None,
            30.0,
            2.0,
            6.0,
        );
        assert_eq!(
            app.timeline.clips.last().unwrap().volume,
            1.0,
            "range-specific speech review overrides a whole-scene music hint"
        );
        app.add_asset_range_to_timeline(
            "/media/montage.mp4",
            "unreviewed bed",
            None,
            40.0,
            8.0,
            6.0,
        );
        assert_eq!(
            app.timeline.clips.last().unwrap().volume,
            0.0,
            "unreviewed portions retain the scene evidence"
        );

        // A curated audio-only track is the new soundtrack, not baked-in score.
        let mut song = asset(true);
        song.info.path = "/media/song.mp3".to_owned();
        song.info.name = "song.mp3".to_owned();
        song.info.has_video = false;
        song.sweep.candidates[0].has_video = false;
        song.sweep.candidates[0].asset_path = song.info.path.clone();
        app.assets.push(song);
        app.add_asset_range_to_timeline("/media/song.mp3", "song", None, 0.0, 0.0, 6.0);
        assert_eq!(
            app.timeline.clips.last().unwrap().volume,
            1.0,
            "an added music track is never muted as a music bed"
        );
    }

    #[test]
    fn adding_captions_maps_transcript_chunks_onto_a_timeline_clip() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let words = [
            ("One", 10.0, 10.3),
            ("two", 10.4, 10.7),
            ("three", 10.8, 11.2),
            ("four", 11.3, 11.7),
            ("five", 11.8, 12.1),
            ("six", 12.2, 12.5),
            ("seven", 12.6, 13.0),
            ("eight.", 13.1, 13.6),
        ]
        .into_iter()
        .map(|(text, start, end)| sweep::TranscriptWord {
            start,
            end,
            text: text.to_owned(),
            confidence: None,
            estimated: false,
        })
        .collect();
        app.assets.push(media::BinAsset {
            info: MediaInfo {
                path: "/media/talk.mp4".to_owned(),
                name: "talk.mp4".to_owned(),
                duration: 20.0,
                width: 854,
                height: 480,
                fps: 30.0,
                has_video: true,
                has_audio: true,
            },
            size_bytes: 0,
            state: media::AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumb_textures: Vec::new(),
            sweep: sweep::SweepData {
                transcript: vec![sweep::TranscriptSegment {
                    start: 10.0,
                    end: 14.0,
                    speaker: Some("A".to_owned()),
                    text: "One two three four five six seven eight.".to_owned(),
                    words,
                    session: None,
                }],
                ..Default::default()
            },
        });
        let clip = app.timeline.add_media_clip(
            "talk.mp4",
            ClipKind::Video,
            0,
            5.0,
            4.0,
            10.0,
            "/media/talk.mp4",
        );

        let added = app
            .add_captions_for_clip(clip, openreel_engine::TitleStyle::PopCaption)
            .expect("captions generated from transcript");
        assert!(added > 1);
        let captions: Vec<_> = app
            .timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Title)
            .collect();
        assert_eq!(captions.len(), added);
        assert!((captions[0].start - 5.0).abs() < 0.04);
        assert!(captions
            .iter()
            .all(|clip| clip.title_style == openreel_engine::TitleStyle::PopCaption));
        assert!(captions
            .iter()
            .all(|clip| clip.start >= 5.0 && clip.end() <= 9.04));
        assert_eq!(
            app.add_captions_for_clip(clip, openreel_engine::TitleStyle::PopCaption)
                .expect("second caption pass is idempotent"),
            0
        );
    }

    #[test]
    fn tightening_silence_replaces_dead_air_and_ripples_following_clips() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let mut peaks = vec![0.0; 25];
        peaks.extend([0.8; 25]);
        peaks.extend([0.0; 25]);
        peaks.extend([0.7; 25]);
        app.assets.push(media::BinAsset {
            info: MediaInfo {
                path: "/media/talk.mp4".to_owned(),
                name: "talk.mp4".to_owned(),
                duration: 4.0,
                width: 854,
                height: 480,
                fps: 30.0,
                has_video: true,
                has_audio: true,
            },
            size_bytes: 0,
            state: media::AssetState::Ready,
            peaks,
            beats: None,
            thumb_textures: Vec::new(),
            sweep: sweep::SweepData::default(),
        });
        let clip = app.timeline.add_media_clip(
            "talk.mp4",
            ClipKind::Video,
            0,
            2.0,
            2.0,
            0.0,
            "/media/talk.mp4",
        );
        let follower = app
            .timeline
            .add_clip("Place card", ClipKind::Title, 1, 5.0, 1.0, 0.0);

        let pieces = app
            .tighten_clip_silence(clip, 0.03, 0.35, 0.1)
            .expect("waveform peaks tighten the clip");
        assert_eq!(pieces, 2);
        let mut kept: Vec<_> = app
            .timeline
            .clips
            .iter()
            .filter(|c| c.kind == ClipKind::Video)
            .collect();
        kept.sort_by(|a, b| a.start.total_cmp(&b.start));
        assert_eq!(
            kept[0].id, clip,
            "the first kept phrase retains the clip id"
        );
        assert!((kept[0].start - 2.0).abs() < 0.04);
        assert!((kept[0].source_in - 0.4).abs() < 0.04);
        assert!((kept[0].duration - 0.7).abs() < 0.04);
        assert!((kept[1].start - kept[0].end()).abs() < 0.04);
        assert!((kept[1].source_in - 1.4).abs() < 0.04);
        assert!((kept[1].duration - 0.6).abs() < 0.04);
        assert!(
            (app.timeline.clip(follower).unwrap().start - 4.3).abs() < 0.04,
            "following material ripples left by the removed dead air"
        );
    }

    #[test]
    fn tightening_silence_keeps_detached_audio_in_linked_phrases() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let mut peaks = vec![0.0; 25];
        peaks.extend([0.8; 25]);
        peaks.extend([0.0; 25]);
        peaks.extend([0.7; 25]);
        app.assets.push(media::BinAsset {
            info: MediaInfo {
                path: "/media/talk.mp4".to_owned(),
                name: "talk.mp4".to_owned(),
                duration: 2.0,
                width: 854,
                height: 480,
                fps: 30.0,
                has_video: true,
                has_audio: true,
            },
            size_bytes: 0,
            state: media::AssetState::Ready,
            peaks,
            beats: None,
            thumb_textures: Vec::new(),
            sweep: sweep::SweepData::default(),
        });
        let video = app.timeline.add_media_clip(
            "talk.mp4",
            ClipKind::Video,
            0,
            0.0,
            2.0,
            0.0,
            "/media/talk.mp4",
        );
        let audio = app.timeline.detach_audio(video).expect("detach audio");

        assert_eq!(
            app.tighten_clip_silence(video, 0.03, 0.35, 0.1)
                .expect("tighten linked pair"),
            2
        );
        assert_eq!(
            app.timeline
                .clips
                .iter()
                .filter(|c| c.kind == ClipKind::Video)
                .count(),
            2
        );
        assert_eq!(
            app.timeline
                .clips
                .iter()
                .filter(|c| c.kind == ClipKind::Audio)
                .count(),
            2
        );
        assert_eq!(app.timeline.clip(video).unwrap().linked_clip, Some(audio));
        for picture in app
            .timeline
            .clips
            .iter()
            .filter(|c| c.kind == ClipKind::Video)
        {
            let sound = app
                .timeline
                .clip(picture.linked_clip.expect("each phrase stays linked"))
                .unwrap();
            assert_eq!(sound.start, picture.start);
            assert_eq!(sound.duration, picture.duration);
            assert_eq!(sound.source_in, picture.source_in);
            assert!(!picture.embedded_audio);
        }
    }

    #[test]
    fn curator_add_shown_uses_the_selected_source_and_filter_but_all_scenes_stays_complete() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        for path in ["/media/a.mp4", "/media/b.mp4"] {
            let candidates: Vec<_> = ["action", "reaction", "reaction", "talking"]
                .into_iter()
                .enumerate()
                .map(|(index, category)| sweep::SweepCandidate {
                    id: format!("{path}#clip-{index}"),
                    asset_path: path.to_owned(),
                    asset_name: path.to_owned(),
                    title: category.to_owned(),
                    source_in: index as f64 * 2.0,
                    duration: 2.0,
                    score: 80.0 + index as f32,
                    reason: "Fixture range".to_owned(),
                    stability: 0.9,
                    audio_energy: 0.0,
                    audio_peak: 0.0,
                    category: category.to_owned(),
                    tags: if index == 0 {
                        vec!["prime".to_owned()]
                    } else {
                        Vec::new()
                    },
                    motion_score: 0.3,
                    audio_score: 0.0,
                    music_bed: false,
                    visual: None,
                    has_video: true,
                    transcript: Vec::new(),
                    review: sweep::ClipReview {
                        status: if index == 2 {
                            sweep::ReviewStatus::NeedsReview
                        } else {
                            sweep::ReviewStatus::Reviewed
                        },
                        ..Default::default()
                    },
                    dismissed: index == 1,
                })
                .collect();
            let mut scenes = candidates.clone();
            scenes[1].review.status = sweep::ReviewStatus::Rejected;
            app.assets.push(media::BinAsset {
                info: MediaInfo {
                    path: path.to_owned(),
                    name: path.to_owned(),
                    duration: 8.0,
                    width: 64,
                    height: 36,
                    fps: 30.0,
                    has_video: true,
                    has_audio: false,
                },
                size_bytes: 0,
                state: media::AssetState::Ready,
                peaks: Vec::new(),
                beats: None,
                thumb_textures: Vec::new(),
                sweep: sweep::SweepData {
                    candidates,
                    scene_clips: scenes,
                    ..Default::default()
                },
            });
        }
        app.selected_asset = Some("/media/b.mp4".to_owned());
        for (filter, expected) in [("reaction", 4.0), ("needs-review", 4.0), ("prime", 0.0)] {
            app.timeline = Timeline::empty_project();
            app.curator.filters = gallery::GalleryFilters::default();
            match filter {
                "prime" => app.curator.filters.quality = gallery::GalleryQuality::Prime,
                "needs-review" => {
                    app.curator.filters.quality = gallery::GalleryQuality::NeedsReview
                }
                category => app.curator.filters.category = Some(category.to_owned()),
            }
            app.add_all_sweep_candidates();
            assert_eq!(app.timeline.clips.len(), 1, "only the shown {filter} range");
            let clip = &app.timeline.clips[0];
            assert_eq!(clip.source.as_deref(), Some("/media/b.mp4"));
            assert_eq!((clip.source_in, clip.duration), (expected, 2.0));
        }
        app.timeline = Timeline::empty_project();
        app.add_all_scene_clips();
        assert_eq!(
            app.timeline.clips.len(),
            4,
            "category and dismissals never hide source scenes"
        );
        for (index, clip) in app.timeline.clips.iter().enumerate() {
            assert_eq!(clip.source.as_deref(), Some("/media/b.mp4"));
            assert_eq!(
                (clip.start, clip.source_in, clip.duration),
                (index as f64 * 2.0, index as f64 * 2.0, 2.0)
            );
        }
        let snapshot: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
        assert_eq!(
            snapshot["sceneClips"].as_array().unwrap().len(),
            8,
            "REPL inventory is not the UI filter"
        );
        assert_eq!(
            snapshot["sweepCandidates"].as_array().unwrap().len(),
            6,
            "only explicit dismissals hide suggestions"
        );
    }

    #[test]
    fn gallery_add_shown_combines_filters_and_preserves_display_order() {
        use gallery::{GalleryDuration, GalleryFilters, GalleryQuality, GallerySort};

        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        for (path, name) in [
            ("/media/expo.mp4", "Expo.mp4"),
            ("/media/interview.mov", "Interview.mov"),
        ] {
            let candidates: Vec<_> = [
                (0.25, 1.5, 82.0, "action", "Rocket launch ready"),
                (10.0, 2.0, 96.0, "action", "Rocket launch now"),
                (20.0, 7.5, 87.0, "action", "Launch later"),
                (40.0, 30.0, 88.0, "talking", "Rocket payload"),
                (80.0, 8.0, 77.0, "action", "Slow launch"),
                (100.0, 29.5, 80.0, "action", "Rocket flame"),
            ]
            .into_iter()
            .enumerate()
            .map(
                |(index, (source_in, duration, score, category, text))| sweep::SweepCandidate {
                    id: format!("{path}#pick-{index}"),
                    asset_path: path.to_owned(),
                    asset_name: name.to_owned(),
                    title: format!("Take {index}"),
                    source_in,
                    duration,
                    score,
                    reason: if index == 2 {
                        "Crowd portrait"
                    } else {
                        "Useful range"
                    }
                    .to_owned(),
                    stability: 0.9,
                    audio_energy: 0.1,
                    audio_peak: 0.2,
                    category: category.to_owned(),
                    tags: vec![if matches!(index, 0 | 1 | 3 | 5) {
                        "prime"
                    } else {
                        "steady"
                    }
                    .to_owned()],
                    motion_score: 0.3,
                    audio_score: 0.4,
                    music_bed: false,
                    visual: None,
                    has_video: true,
                    transcript: vec![sweep::TranscriptSegment {
                        start: source_in,
                        end: source_in + duration,
                        speaker: None,
                        text: text.to_owned(),
                        words: Vec::new(),
                        session: None,
                    }],
                    review: sweep::ClipReview {
                        status: match index {
                            2 => sweep::ReviewStatus::NeedsReview,
                            4 => sweep::ReviewStatus::Local,
                            _ => sweep::ReviewStatus::Reviewed,
                        },
                        ..Default::default()
                    },
                    dismissed: index == 5,
                },
            )
            .collect();
            let mut scene = candidates[0].clone();
            scene.id = format!("{path}#scene-0");
            scene.source_in = 0.0;
            scene.duration = 200.0;
            scene.review.status = sweep::ReviewStatus::Rejected;
            app.assets.push(media::BinAsset {
                info: MediaInfo {
                    path: path.to_owned(),
                    name: name.to_owned(),
                    duration: 200.0,
                    width: 64,
                    height: 36,
                    fps: 30.0,
                    has_video: true,
                    has_audio: false,
                },
                size_bytes: 0,
                state: media::AssetState::Ready,
                peaks: Vec::new(),
                beats: None,
                thumb_textures: Vec::new(),
                sweep: sweep::SweepData {
                    status: sweep::SweepStatus::Ready,
                    candidates,
                    scene_clips: vec![scene],
                    ..Default::default()
                },
            });
        }
        let before: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
        app.selected_asset = Some("/media/interview.mov".to_owned());
        let add_shown = |app: &mut EditorApp| -> Vec<(f64, f64)> {
            app.timeline = Timeline::empty_project();
            app.add_all_sweep_candidates();
            assert!(app
                .timeline
                .clips
                .iter()
                .all(|clip| clip.source.as_deref() == Some("/media/interview.mov")));
            app.timeline
                .clips
                .iter()
                .map(|clip| (clip.source_in, clip.duration))
                .collect()
        };
        app.curator.filters = GalleryFilters {
            search: "  ROCKET  launch ".to_owned(),
            category: Some("action".to_owned()),
            quality: GalleryQuality::Prime,
            duration: GalleryDuration::TwoToEight,
            ..Default::default()
        };
        assert_eq!(
            add_shown(&mut app),
            [(10.0, 2.0)],
            "every filter must match"
        );
        app.curator.filters.duration = GalleryDuration::All;
        assert_eq!(add_shown(&mut app), [(10.0, 2.0), (0.25, 1.5)]);

        for (search, expected) in [
            ("interview rocket payload", vec![(40.0, 30.0)]),
            ("CROWD steady", vec![(20.0, 7.5)]),
            ("flame", Vec::new()),
            ("not-in-any-clip", Vec::new()),
        ] {
            app.curator.filters = GalleryFilters {
                search: search.to_owned(),
                ..Default::default()
            };
            assert_eq!(add_shown(&mut app), expected, "search {search}");
        }
        for (quality, expected) in [
            (GalleryQuality::NeedsReview, vec![(20.0, 7.5)]),
            (GalleryQuality::Local, vec![(80.0, 8.0)]),
            (
                GalleryQuality::Reviewed,
                vec![(10.0, 2.0), (40.0, 30.0), (0.25, 1.5)],
            ),
        ] {
            app.curator.filters = GalleryFilters {
                quality,
                ..Default::default()
            };
            assert_eq!(add_shown(&mut app), expected);
        }
        for (duration, expected) in [
            (GalleryDuration::UnderTwo, vec![(0.25, 1.5)]),
            (GalleryDuration::TwoToEight, vec![(10.0, 2.0), (20.0, 7.5)]),
            (GalleryDuration::EightToThirty, vec![(80.0, 8.0)]),
            (GalleryDuration::ThirtyPlus, vec![(40.0, 30.0)]),
        ] {
            app.curator.filters = GalleryFilters {
                duration,
                ..Default::default()
            };
            assert_eq!(add_shown(&mut app), expected, "half-open duration bands");
        }
        for (sort, expected) in [
            (
                GallerySort::Score,
                // The Needs review range sorts after every usable one.
                vec![
                    (10.0, 2.0),
                    (40.0, 30.0),
                    (0.25, 1.5),
                    (80.0, 8.0),
                    (20.0, 7.5),
                ],
            ),
            (
                GallerySort::Source,
                vec![
                    (0.25, 1.5),
                    (10.0, 2.0),
                    (20.0, 7.5),
                    (40.0, 30.0),
                    (80.0, 8.0),
                ],
            ),
            (
                GallerySort::Shortest,
                vec![
                    (0.25, 1.5),
                    (10.0, 2.0),
                    (20.0, 7.5),
                    (80.0, 8.0),
                    (40.0, 30.0),
                ],
            ),
            (
                GallerySort::Longest,
                vec![
                    (40.0, 30.0),
                    (80.0, 8.0),
                    (20.0, 7.5),
                    (10.0, 2.0),
                    (0.25, 1.5),
                ],
            ),
        ] {
            app.curator.filters = GalleryFilters {
                sort,
                ..Default::default()
            };
            assert_eq!(add_shown(&mut app), expected);
            let mut cursor = 0.0;
            for clip in &app.timeline.clips {
                assert_eq!(clip.start, cursor, "display order becomes timeline order");
                cursor += clip.duration;
            }
        }
        app.selected_asset = None;
        app.curator.filters = GalleryFilters {
            search: "rocket launch".to_owned(),
            quality: GalleryQuality::Prime,
            duration: GalleryDuration::TwoToEight,
            ..Default::default()
        };
        app.timeline = Timeline::empty_project();
        app.add_all_sweep_candidates();
        assert_eq!(app.timeline.clips.len(), 2);
        assert_eq!(
            app.timeline.clips[0].source.as_deref(),
            Some("/media/expo.mp4")
        );
        assert_eq!(
            app.timeline.clips[1].source.as_deref(),
            Some("/media/interview.mov")
        );

        app.curator.filters.search = "no matches".to_owned();
        app.timeline = Timeline::empty_project();
        app.add_all_scene_clips();
        assert_eq!(
            app.timeline.clips.len(),
            2,
            "All scenes bypasses every Clip filter"
        );
        assert_eq!(
            (app.timeline.clips[0].start, app.timeline.clips[0].duration),
            (0.0, 200.0)
        );
        assert_eq!(
            (app.timeline.clips[1].start, app.timeline.clips[1].duration),
            (200.0, 200.0)
        );
        let after: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
        assert_eq!(before["sweepCandidates"], after["sweepCandidates"]);
        assert_eq!(before["sceneClips"], after["sceneClips"]);

        app.curator.filters = GalleryFilters::default();
        app.curator.view = gallery::GalleryView::Scenes;
        let scene = app.assets[0].sweep.scene_clips[0].clone();
        app.assets[0].sweep.scene_clips = (0..48)
            .map(|index| {
                let mut scene = scene.clone();
                scene.id = format!("scene-{index}");
                scene.source_in = index as f64 * 4.0;
                scene.duration = 4.0;
                scene
            })
            .collect();
        app.open_curator_gallery();
        let mut time = 0.0;
        for size in [Vec2::new(1440.0, 900.0), Vec2::new(1024.0, 640.0)] {
            app.timeline = Timeline::empty_project();
            let mut render = |app: &mut EditorApp, events| {
                time += 0.1;
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        app.menu_bar(ctx);
                        app.status_bar(ctx);
                        app.timeline_panel(ctx);
                        app.media_bin(ctx);
                        app.right_dock(ctx);
                        app.central(ctx);
                    },
                )
            };
            render(&mut app, Vec::new());
            render(&mut app, Vec::new());
            let scroll_position = egui::pos2(size.x / 2.0, size.y - 300.0);
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(scroll_position),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(0.0, 10000.0),
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
            for _ in 0..10 {
                render(&mut app, Vec::new());
            }
            let output = render(&mut app, Vec::new());
            let add = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Text(text) if text.galley.text() == "Add" => {
                        Some(text.visual_bounding_rect().center())
                    }
                    _ => None,
                })
                .expect("first card's Add button");
            // Move before pressing, as a native pointer does, so the scroll area's
            // drag handling cannot mistake the move from the wheel location for a drag.
            render(&mut app, vec![egui::Event::PointerMoved(add)]);
            for pressed in [true, false] {
                render(&mut app, vec![pointer_button(add.x, add.y, pressed)]);
            }
            assert_eq!(
                app.timeline.clips.len(),
                1,
                "first card actions fit the gallery at {size:?}"
            );
            assert_eq!(app.timeline.clips[0].source_in, 0.0);
            app.timeline = Timeline::empty_project();
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(scroll_position),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(0.0, -10000.0),
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
            for _ in 0..10 {
                render(&mut app, Vec::new());
            }
            let output = render(&mut app, Vec::new());
            let add = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    Shape::Text(text) if text.galley.text() == "Add" => {
                        Some(text.visual_bounding_rect().center())
                    }
                    _ => None,
                })
                .max_by(|a, b| a.y.total_cmp(&b.y))
                .expect("last card's Add button");
            render(&mut app, vec![egui::Event::PointerMoved(add)]);
            for pressed in [true, false] {
                render(&mut app, vec![pointer_button(add.x, add.y, pressed)]);
            }
            assert_eq!(
                app.timeline.clips.len(),
                1,
                "last card stays reachable after scrolling at {size:?}"
            );
            let clip = &app.timeline.clips[0];
            assert_eq!(clip.source.as_deref(), Some("/media/interview.mov"));
            assert_eq!((clip.source_in, clip.duration), (0.0, 200.0));

            if size.x == 1024.0 {
                let mut click_text = |app: &mut EditorApp, label: &str| {
                    render(app, Vec::new());
                    let output = render(app, Vec::new());
                    let position = output
                        .shapes
                        .iter()
                        .rev()
                        .find_map(|shape| match &shape.shape {
                            Shape::Text(text) if text.galley.text() == label => {
                                let rect = text.visual_bounding_rect();
                                shape
                                    .clip_rect
                                    .contains(rect.center())
                                    .then_some(rect.center())
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("visible gallery control {label}"));
                    render(app, vec![egui::Event::PointerMoved(position)]);
                    for pressed in [true, false] {
                        render(app, vec![pointer_button(position.x, position.y, pressed)]);
                    }
                };
                click_text(&mut app, "Clips");
                for (menu, control, value) in [
                    ("Filters", "All types", "Action"),
                    ("Filters (1)", "Any quality", "Prime"),
                    ("Filters (2)", "Any duration", "Under 2 s"),
                ] {
                    click_text(&mut app, menu);
                    click_text(&mut app, control);
                    click_text(&mut app, value);
                }
                assert_eq!(app.curator.filters.category.as_deref(), Some("action"));
                assert!(app.curator.filters.quality == GalleryQuality::Prime);
                assert!(app.curator.filters.duration == GalleryDuration::UnderTwo);
                assert_eq!(app.timeline.clips.len(), 1, "filtering never adds clips");
                click_text(&mut app, "Add shown");
                assert_eq!(app.timeline.clips.len(), 3);
                for clip in &app.timeline.clips[1..] {
                    assert_eq!((clip.source_in, clip.duration), (0.25, 1.5));
                }
                click_text(&mut app, "Filters (3)");
                click_text(&mut app, "Reset");
                assert!(app.curator.filters == GalleryFilters::default());
                assert_eq!(app.timeline.clips.len(), 3, "reset only changes the query");
                click_text(&mut app, "Search clips…");
                for character in "rocket launch".chars() {
                    render(&mut app, vec![egui::Event::Text(character.to_string())]);
                }
                assert_eq!(app.curator.filters.search, "rocket launch");
            }
        }
    }

    #[test]
    fn curation_waits_for_explicit_curate_action() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        app.assets.push(media::BinAsset {
            info: MediaInfo {
                path: "/media/not-curated-yet.mp4".to_owned(),
                name: "not-curated-yet.mp4".to_owned(),
                duration: 10.0,
                width: 854,
                height: 480,
                fps: 30.0,
                has_video: true,
                has_audio: true,
            },
            size_bytes: 0,
            state: media::AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumb_textures: Vec::new(),
            sweep: sweep::SweepData::default(),
        });

        assert_eq!(
            app.assets[0].sweep.status,
            sweep::SweepStatus::NotStarted,
            "a ready import stays untouched until Curate is pressed"
        );
        assert!(app.sweep_rx.is_none());
        assert!(app.sweep_queue.is_empty());
        assert!(!app.curator.open, "import does not open the curator");

        app.playing = true;
        app.curate_asset(&ctx, "/media/not-curated-yet.mp4".to_owned());
        assert_eq!(app.assets[0].sweep.status, sweep::SweepStatus::Running);
        assert!(app.sweep_rx.is_some(), "explicit Curate starts one worker");
        assert!(!app.playing, "gallery pauses playback");
        assert!(app.curator.open, "curation opens the results gallery");
        assert!(app.timeline.clips.is_empty(), "browsing never adds clips");

        let (tx, rx) = channel();
        app.sweep_rx = Some(rx);
        app.curator.open = false;
        tx.send(SweepMsg::Done {
            path: "/media/not-curated-yet.mp4".to_owned(),
            data: sweep::SweepData {
                status: sweep::SweepStatus::Ready,
                ..Default::default()
            },
        })
        .unwrap();
        app.poll_sweeps(&ctx);
        assert_eq!(app.assets[0].sweep.status, sweep::SweepStatus::Ready);
        assert!(!app.curator.open, "completion respects a return to Program");
        assert!(app.timeline.clips.is_empty());
        app.open_curator_gallery();
        app.set_playing(true);
        assert!(!app.curator.open, "playback returns to Program");
    }

    #[test]
    fn saved_curation_returns_with_the_same_file_only() {
        let dir = std::env::temp_dir().join(format!("eddie-saved-curation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("two-shots.mp4");
        let render = |second: &str| {
            let output = std::process::Command::new("ffmpeg")
                .args([
                    "-y",
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=red:s=64x36:r=10:d=2",
                ])
                .args([
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("color={second}:s=64x36:r=10:d=2"),
                ])
                .args([
                    "-filter_complex",
                    "[0:v][1:v]concat=n=2:v=1:a=0[v]",
                    "-map",
                    "[v]",
                ])
                .args(["-c:v", "libx264", "-preset", "ultrafast", "-threads", "1"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output.stderr);
        };
        render("blue");
        let media = path.to_string_lossy().into_owned();
        let session = |curate: bool| {
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            app.curation_dir = Some(dir.join("store"));
            app.spawn_import(&ctx, vec![path.clone()]);
            let deadline = Instant::now() + std::time::Duration::from_secs(120);
            let mut curated = !curate;
            loop {
                app.poll_imports(&ctx);
                app.poll_sweeps(&ctx);
                let imported =
                    app.import_rx.is_none() && app.assets[0].state == media::AssetState::Ready;
                if imported && !curated {
                    app.curate_asset(&ctx, media.clone());
                    curated = true;
                }
                if imported && curated && app.sweep_rx.is_none() {
                    break;
                }
                assert!(Instant::now() < deadline, "import or curation timed out");
                thread::sleep(std::time::Duration::from_millis(20));
            }
            let state: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
            (
                app.assets[0].sweep.status,
                state["sweepCandidates"].clone(),
                state["sceneClips"].clone(),
            )
        };

        let (status, candidates, scenes) = session(true);
        assert_eq!(status, sweep::SweepStatus::Ready);
        assert!(scenes.as_array().unwrap().len() >= 2, "two shots: {scenes}");

        let (status, restored_candidates, restored_scenes) = session(false);
        assert_eq!(
            status,
            sweep::SweepStatus::Ready,
            "a reopened file keeps its curation"
        );
        assert_eq!(restored_candidates, candidates);
        assert_eq!(restored_scenes, scenes);

        std::thread::sleep(std::time::Duration::from_millis(20));
        render("green");
        let (status, candidates, _) = session(false);
        assert_eq!(
            status,
            sweep::SweepStatus::NotStarted,
            "a replaced file is not matched"
        );
        assert_eq!(candidates, serde_json::json!([]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_project_reopens_its_timeline_media_and_fresh_ids() {
        let dir = std::env::temp_dir().join(format!("eddie-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let media = dir.join("shot.mp4");
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=red:s=64x36:r=10:d=3",
            ])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-threads", "1"])
            .arg(&media)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        let settle = |app: &mut EditorApp, ctx: &egui::Context| {
            let deadline = Instant::now() + std::time::Duration::from_secs(60);
            while app.import_rx.is_some() || !app.import_queue.is_empty() {
                app.poll_imports(ctx);
                assert!(Instant::now() < deadline, "import timed out");
                thread::sleep(std::time::Duration::from_millis(20));
            }
        };

        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        app.spawn_import(&ctx, vec![media.clone()]);
        settle(&mut app, &ctx);
        app.run_console(
            &ctx,
            "addAssetRange('shot.mp4', 0.5, 1.25, 1.5); addTitle('Booth', 3, 2);".to_owned(),
        );
        app.mute_music_beds = false;
        assert_eq!(app.timeline.clips.len(), 2, "{}", app.status);
        app.save_project_to(&dir.join("expo")).unwrap();
        assert!(dir.join("expo.eddie").exists(), "the extension is added");

        let mut reopened = EditorApp::headless_for_test(&ctx);
        reopened
            .open_project(&ctx, &dir.join("expo.eddie"))
            .unwrap();
        settle(&mut reopened, &ctx);
        assert_eq!(reopened.timeline, app.timeline);
        assert!(!reopened.mute_music_beds);
        assert_eq!(reopened.assets.len(), 1);
        assert!(reopened.assets[0].state == media::AssetState::Ready);
        reopened.run_console(&ctx, "addTitle('Next', 6, 1);".to_owned());
        let mut ids: Vec<_> = reopened.timeline.clips.iter().map(|clip| clip.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 3, "a new clip never reuses a saved ID");

        assert!(
            !app.has_unsaved_changes(),
            "saving clears the unsaved state"
        );
        assert!(reopened.has_unsaved_changes());
        reopened.handle_action(&ctx, UiAction::OpenProject);
        assert_eq!(reopened.pending_discard, Some(project::Discard::Open));
        assert_eq!(
            reopened.timeline.clips.len(),
            3,
            "nothing is dropped before an answer"
        );
        reopened.pending_discard = None;

        // Media in use stays in the bin; unused media can be removed.
        let source = reopened.assets[0].info.path.clone();
        reopened.remove_asset(&source);
        assert_eq!(reopened.assets.len(), 1, "{}", reopened.status);
        reopened.timeline.clips.retain(|clip| clip.source.is_none());
        reopened.remove_asset(&source);
        assert!(reopened.assets.is_empty(), "{}", reopened.status);

        // New project asks while edits are unsaved, then starts empty.
        reopened.handle_action(&ctx, UiAction::NewProject);
        assert_eq!(reopened.pending_discard, Some(project::Discard::New));
        reopened.pending_discard = None;
        reopened.spawn_import(&ctx, vec![media.clone()]);
        settle(&mut reopened, &ctx);
        reopened.save_project_to(&dir.join("other")).unwrap();
        reopened.handle_action(&ctx, UiAction::NewProject);
        assert!(reopened.timeline.clips.is_empty());
        assert!(reopened.assets.is_empty(), "the bin starts empty");
        assert!(reopened.project_path.is_none(), "Save asks for a new file");
        assert!(!reopened.has_unsaved_changes());

        // The project and its footage move together: nothing goes missing.
        let moved = dir.with_extension("moved");
        let _ = std::fs::remove_dir_all(&moved);
        std::fs::rename(&dir, &moved).unwrap();
        let mut relocated = EditorApp::headless_for_test(&ctx);
        relocated
            .open_project(&ctx, &moved.join("expo.eddie"))
            .unwrap();
        assert_eq!(relocated.status, "Opened expo.eddie.");
        settle(&mut relocated, &ctx);
        let moved_media = moved.join("shot.mp4").to_string_lossy().into_owned();
        assert_eq!(
            relocated.timeline.clips[0].source.as_deref(),
            Some(moved_media.as_str())
        );
        assert!(relocated.assets[0].info.path == moved_media);
        assert!(
            !relocated.has_unsaved_changes(),
            "an automatic relink is not an edit"
        );

        // Footage moved elsewhere is located by hand.
        let elsewhere = moved.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::rename(moved.join("shot.mp4"), elsewhere.join("shot.mp4")).unwrap();
        let mut missing = EditorApp::headless_for_test(&ctx);
        missing
            .open_project(&ctx, &moved.join("expo.eddie"))
            .unwrap();
        assert_eq!(missing.missing_media.len(), 1);
        assert!(
            missing.status.contains("1 media file is missing"),
            "{}",
            missing.status
        );
        assert_eq!(missing.timeline, app.timeline, "clips stay for relinking");
        missing.locate_missing_media(&ctx, &elsewhere.join("shot.mp4"));
        assert_eq!(missing.status, "Relinked 1 media file.");
        settle(&mut missing, &ctx);
        assert!(missing.missing_media.is_empty());
        let located = elsewhere.join("shot.mp4").to_string_lossy().into_owned();
        assert_eq!(
            missing.timeline.clips[0].source.as_deref(),
            Some(located.as_str())
        );
        assert!(missing.assets[0].state == media::AssetState::Ready);

        let before = missing.timeline.clone();
        std::fs::write(moved.join("bad.eddie"), "{}").unwrap();
        assert!(missing
            .open_project(&ctx, &moved.join("bad.eddie"))
            .is_err());
        assert_eq!(missing.timeline, before, "a bad file changes nothing");
        let _ = std::fs::remove_dir_all(&moved);
    }

    #[test]
    fn undo_and_redo_step_through_console_edits() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let track = |app: &mut EditorApp| {
            app.history
                .track(&app.timeline, app.selected, 0.0, false, false);
        };
        app.run_console(&ctx, "addTitle('One', 0, 1);".to_owned());
        track(&mut app);
        app.run_console(
            &ctx,
            "addTitle('Two', 2, 1); addTitle('Three', 4, 1);".to_owned(),
        );
        track(&mut app);
        assert_eq!(app.timeline.clips.len(), 3);

        app.handle_action(&ctx, UiAction::Undo);
        assert_eq!(app.timeline.clips.len(), 1, "one console run is one step");
        assert_eq!(app.status, "Undid the last edit.");
        app.handle_action(&ctx, UiAction::Undo);
        assert!(app.timeline.clips.is_empty());
        app.handle_action(&ctx, UiAction::Undo);
        assert_eq!(app.status, "Nothing to undo.");
        app.handle_action(&ctx, UiAction::Redo);
        app.handle_action(&ctx, UiAction::Redo);
        assert_eq!(app.timeline.clips.len(), 3);
        app.handle_action(&ctx, UiAction::ClearTimeline);
        track(&mut app);
        app.handle_action(&ctx, UiAction::Undo);
        assert_eq!(
            app.timeline.clips.len(),
            3,
            "clearing the timeline is undoable"
        );
    }

    #[test]
    fn curate_all_new_overlaps_files_and_finishes_every_one() {
        let dir =
            std::env::temp_dir().join(format!("eddie-parallel-curation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let colors = ["red", "green", "blue", "yellow"];
        for color in colors {
            let path = dir.join(format!("{color}.mp4"));
            let output = std::process::Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
                .arg(format!("color={color}:s=64x36:r=10:d=2"))
                .args(["-c:v", "libx264", "-preset", "ultrafast", "-threads", "1"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output.stderr);
            app.assets.push(media::BinAsset {
                info: openreel_media::probe(path.to_str().unwrap()).unwrap(),
                size_bytes: 0,
                state: media::AssetState::Ready,
                peaks: Vec::new(),
                beats: None,
                thumb_textures: Vec::new(),
                sweep: sweep::SweepData::default(),
            });
        }

        app.curate_all_new(&ctx);
        let active = app
            .assets
            .iter()
            .filter(|asset| {
                asset.sweep.status == sweep::SweepStatus::Running
                    && !app.sweep_queue.contains(&asset.info.path)
            })
            .count();
        let expected =
            std::thread::available_parallelism().map_or(1, |n| (n.get() / 4).clamp(1, 3));
        assert_eq!(active, expected, "several files curate at once");
        assert_eq!(app.sweep_queue.len(), colors.len() - expected);

        let deadline = Instant::now() + std::time::Duration::from_secs(120);
        while app.sweep_rx.is_some() || !app.sweep_queue.is_empty() {
            app.poll_sweeps(&ctx);
            assert!(Instant::now() < deadline, "curation timed out");
            thread::sleep(std::time::Duration::from_millis(20));
        }
        for asset in &app.assets {
            assert_eq!(
                asset.sweep.status,
                sweep::SweepStatus::Ready,
                "{}",
                asset.info.name
            );
            assert!(!asset.sweep.scene_clips.is_empty(), "{}", asset.info.name);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn right_dock_switches_to_details_on_selection() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let id = app
            .timeline
            .add_clip("a.mp4", ClipKind::Video, 0, 0.0, 5.0, 0.0);
        let run_dock = |app: &mut EditorApp| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1200.0, 800.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    app.right_dock(ctx);
                    egui::CentralPanel::default().show(ctx, |_| {});
                },
            );
        };
        run_dock(&mut app);
        assert_eq!(app.right_tab, RightTab::Chat);
        app.selected = Some(id);
        run_dock(&mut app);
        assert_eq!(app.right_tab, RightTab::Details);
    }

    #[test]
    fn delete_without_magnet_keeps_gap() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        app.timeline_view.magnet = false;
        app.timeline
            .add_clip("a.mp4", ClipKind::Video, 0, 0.0, 10.0, 0.0);
        let b_id = app
            .timeline
            .add_clip("b.mp4", ClipKind::Video, 0, 10.0, 10.0, 0.0);
        let c_id = app
            .timeline
            .add_clip("c.mp4", ClipKind::Video, 0, 20.0, 10.0, 0.0);
        app.selected = Some(b_id);
        app.handle_action(&ctx, UiAction::DeleteSelected);
        let c = app.timeline.clip(c_id).unwrap();
        assert!(
            (c.start - 20.0).abs() < 1e-6,
            "magnet off should leave the gap, got {}",
            c.start
        );
    }
}

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

use eframe::egui::{
    self, ColorImage, CornerRadius, RichText, Sense, Shape, Stroke, TextureHandle, TextureOptions,
    Ui, Vec2,
};
use openreel_engine::{format_timecode, ClipId, ClipKind, Timeline, TitleStyle};
use openreel_media::{AudioEngine, AudioSegment, FrameStream, MediaInfo, RawFrame, StillDecoder};

use crate::preview;
use crate::scripting::{ScriptCommand, ScriptEngine};
use crate::theme;

pub(crate) mod chat;
mod gallery;
pub(crate) mod intents;
mod media;
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

enum ExportMsg {
    Progress(f64),
    Done(Result<PathBuf, String>),
}

#[derive(Clone, Copy)]
enum UiAction {
    Import,
    Export,
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
    initial_imports: Vec<PathBuf>,

    still_decoder: StillDecoder,
    still_rx: Receiver<(u64, Result<RawFrame, String>)>,
    preview_texture: Option<TextureHandle>,
    preview_key: Option<VideoKey>,
    still_gen: u64,
    still_pending: bool,
    still_key: Option<(ClipId, i64, u32)>,
    still_cache: HashMap<(ClipId, i64, u32), RawFrame>,

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
    chat_session: ChatSession,
    persist_session: bool,
    chat_input: String,
    chat_rx: Option<Receiver<(u64, ChatWorkerResult)>>,
    chat_pending: Option<PendingChat>,
    chat_generation: u64,
    chat_undo: Option<TimelineSnapshot>,
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
            initial_imports,
            still_decoder,
            still_rx,
            preview_texture: None,
            preview_key: None,
            still_gen: 0,
            still_pending: false,
            still_key: None,
            still_cache: HashMap::new(),
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
            chat_settings: ChatSettings::load(),
            connection_check: None,
            chat_session: ChatSession::load(),
            persist_session: true,
            chat_input: String::new(),
            chat_rx: None,
            chat_pending: None,
            chat_generation: 0,
            chat_undo: None,
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
            initial_imports: Vec::new(),
            still_decoder,
            still_rx,
            preview_texture: None,
            preview_key: None,
            still_gen: 0,
            still_pending: false,
            still_key: None,
            still_cache: HashMap::new(),
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
            chat_session: ChatSession::new("screenshot"),
            persist_session: false,
            chat_input: String::new(),
            chat_rx: None,
            chat_pending: None,
            chat_generation: 0,
            chat_undo: None,
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
        self.still_gen = self.still_gen.wrapping_add(1);
        self.still_pending = false;
        self.still_key = None;
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
        self.stop_sources();
        self.preview_texture = None;
        self.preview_key = None;
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
        let wants_still = !self.playing || self.timeline_view.is_interacting();
        let mut latest_still: Option<((ClipId, i64, u32), RawFrame)> = None;
        while let Ok((gen, result)) = self.still_rx.try_recv() {
            if gen == self.still_gen {
                self.still_pending = false;
                match result {
                    Ok(frame) => {
                        if let Some(key) = self.still_key {
                            if wants_still
                                && active.as_ref().is_some_and(|clip| clip.id == key.0)
                                && source_time.is_some_and(|time| {
                                    (time * self.timeline.fps).round() as i64 == key.1
                                })
                            {
                                latest_still = Some((key, frame));
                            }
                        }
                    }
                    Err(_) => self.still_key = None,
                }
            }
        }
        if let Some((key, frame)) = latest_still {
            self.update_preview_texture(ctx, &frame);
            if self.still_cache.len() >= STILL_CACHE_CAP {
                self.still_cache.clear();
            }
            self.still_cache.insert(key, frame);
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
        let width = if self.timeline_view.is_interacting() {
            SCRUB_W
        } else {
            PREVIEW_W
        };
        let key = (
            clip.id,
            (source_time * self.timeline.fps).round() as i64,
            width,
        );
        if self.still_key == Some(key) && (self.still_pending || self.preview_texture.is_some()) {
            return;
        }
        if let Some(cached) = self.still_cache.get(&key) {
            self.still_key = Some(key);
            let image = ColorImage::from_rgba_unmultiplied(
                [cached.width as usize, cached.height as usize],
                &cached.rgba,
            );
            match self.preview_texture.as_mut() {
                Some(tex) if tex.size() == image.size => tex.set(image, TextureOptions::LINEAR),
                _ => {
                    self.preview_texture =
                        Some(ctx.load_texture("preview", image, TextureOptions::LINEAR));
                }
            }
            self.preview_key = self.active_video_key();
            return;
        }
        self.still_key = Some(key);
        self.still_pending = true;
        self.still_gen += 1;
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
        if self.timeline_view.is_interacting() {
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
            self.still_gen = self.still_gen.wrapping_add(1);
            self.still_pending = false;
            self.still_key = None;
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
                            match stream.request_preview(
                                source,
                                start,
                                PREVIEW_W,
                                self.timeline.fps,
                            ) {
                                Ok(generation) => self.stream_generation = generation,
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
            let result = openreel_media::export_timeline(&timeline, &infos, &output, progress_tx);
            let _ = forwarder.join();
            let _ = tx.send(ExportMsg::Done(result));
            ctx.request_repaint();
        });
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
            UiAction::ClearTimeline => {
                self.cancel_chat();
                self.set_playing(false);
                self.timeline = Timeline::empty_project();
                self.selected = None;
                self.playhead = 0.0;
                self.still_key = None;
                self.still_cache.clear();
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
                    "ready": a.state == AssetState::Ready,
                    "waveformPeaksPerSec": peaks_per_sec,
                    "silenceRanges": silence_ranges,
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
        for command in commands {
            // Explicit console clears cancel old model replies even when
            // the replacement timeline happens to reuse identical IDs.
            if matches!(command, ScriptCommand::ClearTimeline) {
                self.cancel_chat();
            }
            match self.apply_script_command(ctx, command) {
                Ok(Some(note)) => notes.push(note),
                Ok(None) => {}
                Err(error) => notes.push(format!("error: {error}")),
            }
        }
        notes
    }

    /// One executor for console commands and transactional generated edits.
    /// Failures are typed; a console log is never an application error.
    fn apply_script_command(
        &mut self,
        ctx: &egui::Context,
        command: ScriptCommand,
    ) -> Result<Option<String>, String> {
        command.validate()?;
        let target = match &command {
            ScriptCommand::SelectClip(id) | ScriptCommand::DeleteClip(id) => Some(*id),
            ScriptCommand::SplitClip { clip, .. }
            | ScriptCommand::MoveClip { clip, .. }
            | ScriptCommand::TrimClip { clip, .. }
            | ScriptCommand::RenameClip { clip, .. }
            | ScriptCommand::SetOpacity { clip, .. }
            | ScriptCommand::SetVolume { clip, .. }
            | ScriptCommand::DetachAudio { clip }
            | ScriptCommand::UnlinkClip { clip }
            | ScriptCommand::SetEmbeddedAudio { clip, .. }
            | ScriptCommand::TightenSilence { clip, .. }
            | ScriptCommand::SetTitleStyle { clip, .. }
            | ScriptCommand::AddCaptionsForClip { clip, .. } => Some(*clip),
            _ => None,
        };
        if let Some(id) = target {
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
            ScriptCommand::SplitClip { clip, at } => {
                self.selected = Some(
                    self.timeline
                        .split_at(clip, at)
                        .ok_or_else(|| format!("cannot split clip {clip} at {at}"))?,
                );
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
            ScriptCommand::AddAsset { asset, at } => {
                let (path, name, duration) = self
                    .assets
                    .iter()
                    .find(|a| a.info.path == asset || a.info.name == asset)
                    .map(|a| (a.info.path.clone(), a.info.name.clone(), a.info.duration))
                    .ok_or_else(|| format!("no asset '{asset}'"))?;
                self.try_add_asset_range_to_timeline(&path, &name, None, at, 0.0, duration)?;
            }
            ScriptCommand::AddAssetRange {
                asset,
                at,
                source_in,
                duration,
            } => {
                let (path, name) = self
                    .assets
                    .iter()
                    .find(|a| a.info.path == asset || a.info.name == asset)
                    .map(|a| (a.info.path.clone(), a.info.name.clone()))
                    .ok_or_else(|| format!("no asset '{asset}'"))?;
                self.try_add_asset_range_to_timeline(&path, &name, None, at, source_in, duration)?;
            }
            ScriptCommand::Import(path) => self.spawn_import(ctx, vec![PathBuf::from(path)]),
            ScriptCommand::Export(path) => self.start_export(ctx, PathBuf::from(path)),
            ScriptCommand::ClearTimeline => {
                self.set_playing(false);
                self.timeline = Timeline::empty_project();
                self.selected = None;
                self.playhead = 0.0;
                self.still_key = None;
                self.still_cache.clear();
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
            if ctx.input(|i| i.key_pressed(egui::Key::S) || i.key_pressed(egui::Key::B)) {
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
                        if ui.button("Import media…").clicked() {
                            action = Some(UiAction::Import);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Export MP4…").clicked() {
                            action = Some(UiAction::Export);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Clear timeline").clicked() {
                            action = Some(UiAction::ClearTimeline);
                            ui.close();
                        }
                    });
                    ui.menu_button("Edit", |ui| {
                        let has_selection = self.selected.is_some();
                        if ui
                            .add_enabled(has_selection, egui::Button::new("Split at playhead"))
                            .clicked()
                        {
                            action = Some(UiAction::SplitSelected);
                            ui.close();
                        }
                        if ui
                            .add_enabled(has_selection, egui::Button::new("Delete clip"))
                            .clicked()
                        {
                            action = Some(UiAction::DeleteSelected);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Add title at playhead").clicked() {
                            action = Some(UiAction::AddTitle);
                            ui.close();
                        }
                    });
                    ui.menu_button("View", |ui| {
                        ui.checkbox(&mut self.show_media, "Media bin");
                        ui.checkbox(&mut self.show_inspector, "Inspector");
                        ui.checkbox(&mut self.show_chat, "Chat");
                        ui.checkbox(&mut self.show_console, "JS console (debug)");
                        ui.separator();
                        if ui.button("Zoom timeline in").clicked() {
                            action = Some(UiAction::ZoomIn);
                            ui.close();
                        }
                        if ui.button("Zoom timeline out").clicked() {
                            action = Some(UiAction::ZoomOut);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Go to start").clicked() {
                            action = Some(UiAction::GoStart);
                            ui.close();
                        }
                        if ui.button("Go to end").clicked() {
                            action = Some(UiAction::GoEnd);
                            ui.close();
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::primary_button(ui, "Export")
                            .on_hover_text("Export the timeline as an MP4")
                            .clicked()
                        {
                            action = Some(UiAction::Export);
                        }
                        if theme::secondary_button(ui, "Import media")
                            .on_hover_text("Import video or audio files")
                            .clicked()
                        {
                            action = Some(UiAction::Import);
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
                        || self.chat_pending.is_some();
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                    ui.painter().circle_filled(
                        rect.center(),
                        3.0,
                        if busy { theme::BRAND } else { theme::SUBTLE_FG },
                    );
                    ui.label(
                        RichText::new(&self.status)
                            .size(12.0)
                            .color(theme::MUTED_FG),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(p) = self.export_progress {
                            ui.add(
                                egui::ProgressBar::new(p as f32)
                                    .desired_width(180.0)
                                    .show_percentage(),
                            );
                        } else {
                            ui.label(
                                RichText::new(format!(
                                    "{:.0} fps   {}   UI {:.2} ms   RSS {} MB",
                                    self.timeline.fps,
                                    format_timecode(self.timeline.end(), self.timeline.fps),
                                    self.frame_ms_ema,
                                    self.rss_mb
                                ))
                                .monospace()
                                .size(11.0)
                                .color(theme::SUBTLE_FG),
                            )
                            .on_hover_text(
                                "Project frame rate, timeline length, UI frame time and memory",
                            );
                        }
                    });
                });
            });
    }

    fn timeline_panel(&mut self, ctx: &egui::Context) {
        let (id, height) = if self.curator.open {
            ("gallery_timeline", 170.0)
        } else {
            ("timeline", 240.0)
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
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new(format!("{} clips", self.timeline.clips.len()))
                                .size(11.5)
                                .color(theme::SUBTLE_FG),
                        )
                        .on_hover_text("Clips on the timeline");
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
            .default_width(330.0)
            .width_range(260.0..=480.0)
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
                if let Some(tab) = theme::segmented(ui, self.right_tab, &tabs) {
                    self.right_tab = tab;
                }
                ui.add_space(10.0);
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
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                ui.painter()
                    .circle_filled(dot.center(), 4.0, kind_color.gamma_multiply(1.6));
                ui.label(theme::caption(kind_label));
                if let Some(path) = &clip.source {
                    let file = std::path::Path::new(path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.clone());
                    ui.add(egui::Label::new(theme::caption(format!("· {file}"))).truncate())
                        .on_hover_text(path);
                }
            });
            ui.add_space(4.0);
            ui.add(
                egui::TextEdit::singleline(&mut clip.name)
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(8, 6)),
            )
            .on_hover_text("Clip name");
            ui.add_space(10.0);
            let is_audio = clip.kind == ClipKind::Audio;
            theme::card_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::panel_title(ui, "Properties");
                ui.add_space(4.0);
                egui::Grid::new("clip_props")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Start (s)");
                        start_changed = ui
                            .add(
                                egui::DragValue::new(&mut edited_start)
                                    .speed(0.05)
                                    .range(0.0..=86400.0)
                                    .clamp_existing_to_range(false),
                            )
                            .changed();
                        ui.end_row();
                        ui.label("Duration (s)");
                        ui.add(
                            egui::DragValue::new(&mut clip.duration)
                                .speed(0.05)
                                .range(0.1..=86400.0)
                                .clamp_existing_to_range(false),
                        );
                        ui.end_row();
                        if clip.source.is_some() {
                            ui.label("Source in (s)");
                            ui.add(
                                egui::DragValue::new(&mut clip.source_in)
                                    .speed(0.05)
                                    .range(0.0..=86400.0)
                                    .clamp_existing_to_range(false),
                            );
                            ui.end_row();
                        }
                        if !is_audio {
                            ui.label("Opacity");
                            ui.add(egui::Slider::new(&mut clip.opacity, 0.0..=1.0));
                            ui.end_row();
                        }
                        if clip.source.is_some() {
                            ui.label("Volume");
                            ui.add(egui::Slider::new(&mut clip.volume, 0.0..=1.5));
                            ui.end_row();
                        }
                    });
            });
            ui.add_space(10.0);
            theme::card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::panel_title(ui, "Actions");
            ui.add_space(4.0);
            if let Some(snapshot) = &clip_snapshot {
                if snapshot.kind == ClipKind::Video && source_has_audio {
                    ui.add_enabled(
                        snapshot.linked_clip.is_none(),
                        egui::Checkbox::new(&mut clip.embedded_audio, "Embedded audio"),
                    )
                    .on_hover_text(
                        "Play this video clip's own sound. Disabled while its audio is detached",
                    );
                    ui.add_space(6.0);
                    if snapshot.embedded_audio && snapshot.linked_clip.is_none() {
                        if theme::secondary_button_wide(ui, "Detach audio")
                            .on_hover_text(
                                "Move this clip's sound to a linked audio clip you can edit separately",
                            )
                            .clicked()
                        {
                            do_detach = true;
                        }
                        ui.add_space(6.0);
                    }
                }
                if source_has_audio && snapshot.kind != ClipKind::Title {
                    if theme::secondary_button_wide(ui, "Remove silence")
                        .on_hover_text(
                            "Cut waveform pauses from this speech clip and ripple later clips left",
                        )
                        .clicked()
                    {
                        do_tighten = true;
                    }
                    ui.add_space(6.0);
                }
                if snapshot.kind == ClipKind::Title {
                    let combo = egui::ComboBox::from_label("Style")
                        .selected_text(clip.title_style.label())
                        .show_ui(ui, |ui| {
                            for style in TitleStyle::ALL {
                                ui.selectable_value(&mut clip.title_style, style, style.label());
                            }
                        });
                    combo
                        .response
                        .on_hover_text("Choose how this title or caption is drawn");
                    ui.add_space(6.0);
                } else if has_transcript {
                    let combo = egui::ComboBox::from_label("Caption style")
                        .selected_text(caption_style.label())
                        .show_ui(ui, |ui| {
                            for style in TitleStyle::ALL {
                                ui.selectable_value(&mut caption_style, style, style.label());
                            }
                        });
                    combo
                        .response
                        .on_hover_text("Choose the style for generated captions");
                    ui.add_space(6.0);
                    if theme::secondary_button_wide(ui, "Add captions")
                        .on_hover_text(
                            "Generate caption clips from this clip's transcript in the chosen style",
                        )
                        .clicked()
                    {
                        do_captions = true;
                    }
                    ui.add_space(6.0);
                }
                if let Some(linked) = &linked_snapshot {
                    ui.label(
                        RichText::new(format!("Linked: {}", linked.name))
                            .size(11.0)
                            .color(theme::MUTED_FG),
                    );
                    ui.add_space(4.0);
                    if theme::secondary_button_wide(ui, "Unlink audio")
                        .on_hover_text(
                            "Let picture and sound move independently for J/L cuts or replacement audio",
                        )
                        .clicked()
                    {
                        do_unlink = true;
                    }
                    ui.add_space(6.0);
                }
            }
            if theme::secondary_button_wide(ui, "Split at playhead")
                .on_hover_text("Split this clip at the playhead (S)")
                .clicked()
            {
                do_split = true;
            }
            ui.add_space(6.0);
            if theme::destructive_button_wide(ui, "Delete clip")
                .on_hover_text("Delete this clip (Del)")
                .clicked()
            {
                do_delete = true;
            }
            });
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
                    Vec2::new(avail.x, (avail.y - 58.0).clamp(0.0, avail.y)),
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
                        self.timeline
                            .clips
                            .iter()
                            .filter(|c| c.kind == ClipKind::Video)
                            .filter_map(|c| self.asset(c.source.as_deref()?))
                            .map(|a| (a.info.width, a.info.height)),
                    ),
                );

                ui.add_space(8.0);
                let row = ui.available_rect_before_wrap();
                let bar_w = 520.0_f32.min(row.width());
                let bar = egui::Rect::from_center_size(
                    egui::pos2(row.center().x, row.top() + 22.0),
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
                        ui.label(
                            RichText::new(format_timecode(self.playhead, self.timeline.fps))
                                .monospace()
                                .size(13.0)
                                .color(theme::FOREGROUND),
                        )
                        .on_hover_text("Playhead position");
                        let controls_w = 38.0 * 5.0 + 2.0 * 4.0;
                        let left_pad = (ui.available_width() - controls_w) / 2.0 - 52.0;
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
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format_timecode(
                                    self.timeline.end(),
                                    self.timeline.fps,
                                ))
                                .monospace()
                                .size(13.0)
                                .color(theme::SUBTLE_FG),
                            )
                            .on_hover_text("Timeline length");
                        });
                    },
                );
            });
    }
}

impl EditorApp {
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
        self.finish_frame(ctx, action);
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
}

/// App mark: a rounded tile with a play notch, drawn as shapes.
fn brand_mark(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(6), theme::FOREGROUND);
    let c = rect.center() + Vec2::new(1.0, 0.0);
    painter.add(Shape::convex_polygon(
        vec![
            c + Vec2::new(-4.0, -5.5),
            c + Vec2::new(5.5, 0.0),
            c + Vec2::new(-4.0, 5.5),
        ],
        theme::BACKGROUND,
        Stroke::NONE,
    ));
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
            // Horseshoe: two poles and a U arc.
            let left = egui::Rect::from_min_size(c + Vec2::new(-7.0, -8.0), Vec2::new(4.0, 7.0));
            let right = egui::Rect::from_min_size(c + Vec2::new(3.0, -8.0), Vec2::new(4.0, 7.0));
            painter.rect_filled(left, CornerRadius::same(1), fg);
            painter.rect_filled(right, CornerRadius::same(1), fg);
            let arc: Vec<egui::Pos2> = (0..=12)
                .map(|i| {
                    let angle = std::f32::consts::PI * (i as f32 / 12.0);
                    c + Vec2::new(-5.0 * angle.cos(), -1.0 + 6.0 * angle.sin())
                })
                .collect();
            painter.add(Shape::line(arc, Stroke::new(3.6_f32, fg)));
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
                !app.still_pending,
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
                    Shape::Text(text) if text.galley.text().parse::<f64>().ok() == Some(0.0) => {
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
                vec![
                    (10.0, 2.0),
                    (40.0, 30.0),
                    (20.0, 7.5),
                    (0.25, 1.5),
                    (80.0, 8.0),
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

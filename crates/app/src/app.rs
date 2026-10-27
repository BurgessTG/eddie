use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

use eframe::egui::{
    self, ColorImage, CornerRadius, RichText, Sense, Shape, Stroke, TextureHandle, TextureOptions,
    Ui, Vec2,
};
use openreel_engine::{format_timecode, ClipId, ClipKind, Timeline};
use openreel_media::{AudioPlayer, FrameStream, MediaInfo, RawFrame, StillDecoder};

use crate::preview;
use crate::scripting::{ScriptCommand, ScriptEngine};
use crate::theme;

pub(crate) mod chat;
pub(crate) mod intents;
mod media;
#[cfg(test)]
mod screenshot;
pub(crate) mod session;
mod sweep;
use crate::timeline_ui::{ClipVisual, TimelineTool, TimelineView, VisualMap};
use chat::{ChatSettings, ChatWorkerResult, PendingChat, TimelineSnapshot};
use media::{AssetState, BinAsset, ImportMsg};
use session::ChatSession;
use sweep::SweepMsg;

const TRACK_TITLES: usize = 0;
const TRACK_VIDEO: usize = 1;
const TRACK_AUDIO: usize = 2;
const PREVIEW_W: u32 = 960;
const SCRUB_W: u32 = 480;
const STILL_CACHE_CAP: usize = 24;
const MEDIA_EXTS: &[&str] = &[
    "mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "flac", "m4a", "aac", "ogg",
];

enum ExportMsg {
    Progress(f64),
    Done(Result<PathBuf, String>),
}

#[derive(Clone, Copy)]
enum UiAction {
    Import,
    SampleProject,
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

pub struct EditorApp {
    timeline: Timeline,
    timeline_view: TimelineView,
    playhead: f64,
    playing: bool,
    last_now: Option<f64>,
    selected: Option<ClipId>,

    assets: Vec<BinAsset>,
    selected_asset: Option<String>,
    search: String,
    status: String,

    import_rx: Option<Receiver<ImportMsg>>,
    pending_sample_project: bool,
    sweep_rx: Option<Receiver<SweepMsg>>,
    sweep_queue: Vec<String>,
    initial_imports: Vec<PathBuf>,

    still_decoder: StillDecoder,
    still_rx: Receiver<(u64, Result<RawFrame, String>)>,
    preview_texture: Option<TextureHandle>,
    still_gen: u64,
    still_pending: bool,
    still_key: Option<(ClipId, i64, u32)>,
    still_cache: HashMap<(ClipId, i64, u32), RawFrame>,

    stream: Option<FrameStream>,
    stream_clip: Option<ClipId>,
    audio_players: HashMap<ClipId, AudioPlayer>,
    audio_failed: bool,
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
    chat_session: ChatSession,
    chat_input: String,
    chat_rx: Option<Receiver<(u64, ChatWorkerResult)>>,
    chat_pending: Option<PendingChat>,
    chat_generation: u64,
    chat_undo: Option<TimelineSnapshot>,
    show_chat: bool,
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
            Ok(engine) => (Some(engine), vec!["OpenReel JS. help()".to_owned()]),
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
            search: String::new(),
            status: "Ready.".to_owned(),
            import_rx: None,
            pending_sample_project: false,
            sweep_rx: None,
            sweep_queue: Vec::new(),
            initial_imports,
            still_decoder,
            still_rx,
            preview_texture: None,
            still_gen: 0,
            still_pending: false,
            still_key: None,
            still_cache: HashMap::new(),
            stream: None,
            stream_clip: None,
            audio_players: HashMap::new(),
            audio_failed: false,
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
            chat_session: ChatSession::load(),
            chat_input: String::new(),
            chat_rx: None,
            chat_pending: None,
            chat_generation: 0,
            chat_undo: None,
            show_chat: true,
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
            Ok(engine) => (Some(engine), vec!["OpenReel JS. help()".to_owned()]),
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
            search: String::new(),
            status: "Ready.".to_owned(),
            import_rx: None,
            pending_sample_project: false,
            sweep_rx: None,
            sweep_queue: Vec::new(),
            initial_imports: Vec::new(),
            still_decoder,
            still_rx,
            preview_texture: None,
            still_gen: 0,
            still_pending: false,
            still_key: None,
            still_cache: HashMap::new(),
            stream: None,
            stream_clip: None,
            audio_players: HashMap::new(),
            audio_failed: false,
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
            chat_session: ChatSession::new("screenshot"),
            chat_input: String::new(),
            chat_rx: None,
            chat_pending: None,
            chat_generation: 0,
            chat_undo: None,
            show_chat: true,
            show_chat_settings: false,
            show_chat_debug: false,
            last_generated_js: String::new(),
            frame_ms_ema: 0.0,
            rss_mb: read_rss_mb(),
            frame_count: 0,
        }
    }

    /// Delete a clip from a user action (menu, key, toolbar,
    /// inspector). With the magnet on, the main track ripples
    /// closed. Script deletes do not come through here; scripts
    /// close gaps only via the explicit CloseGaps command.
    fn delete_clip(&mut self, id: ClipId) {
        self.timeline.remove_clip(id);
        if self.selected == Some(id) {
            self.selected = None;
        }
        if self.timeline_view.magnet {
            self.timeline.close_gaps(TRACK_VIDEO);
        }
    }

    fn stop_sources(&mut self) {
        if let Some(stream) = self.stream.take() {
            stream.stop();
        }
        self.stream_clip = None;
        self.audio_players.clear();
        self.audio_failed = false;
    }

    fn set_playing(&mut self, playing: bool) {
        if playing && self.playhead >= self.timeline.end() {
            self.playhead = 0.0;
        }
        self.playing = playing;
        if !playing {
            self.stop_sources();
        }
    }

    fn seek(&mut self, time: f64) {
        self.playhead = self
            .timeline
            .snap(time.clamp(0.0, self.timeline.end().max(0.0)));
        self.stop_sources();
    }

    fn active_video_clip(&self) -> Option<openreel_engine::Clip> {
        self.timeline
            .clips
            .iter()
            .filter(|c| c.kind == ClipKind::Video && c.contains(self.playhead))
            .min_by_key(|c| c.track)
            .cloned()
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
    }

    fn poll_frames(&mut self, ctx: &egui::Context) {
        let mut latest_still: Option<((ClipId, i64, u32), RawFrame)> = None;
        while let Ok((gen, result)) = self.still_rx.try_recv() {
            if gen == self.still_gen {
                self.still_pending = false;
                match result {
                    Ok(frame) => {
                        if let Some(key) = self.still_key {
                            latest_still = Some((key, frame));
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

        // Stream frames: upload only the newest, recycle every spent buffer.
        let mut latest_stream: Option<RawFrame> = None;
        if let Some(stream) = self.stream.as_ref() {
            while let Ok(frame) = stream.frames.try_recv() {
                if let Some(mut spent) = latest_stream.replace(frame) {
                    stream.recycle(std::mem::take(&mut spent.rgba));
                }
            }
        }
        if let Some(mut frame) = latest_stream {
            self.update_preview_texture(ctx, &frame);
            if let Some(stream) = self.stream.as_ref() {
                stream.recycle(std::mem::take(&mut frame.rgba));
            }
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
        if self.still_key == Some(key) || self.still_pending {
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
            let expected = if self.playing {
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
            if self.stream.is_some() || !self.audio_players.is_empty() {
                self.stop_sources();
            }
            self.request_still_if_needed(ctx);
            return;
        }

        if !self.playing {
            if self.stream.is_some() || !self.audio_players.is_empty() {
                self.stop_sources();
            }
            self.request_still_if_needed(ctx);
            return;
        }

        // Video stream follows the active video clip.
        let active_video = self.active_video_clip();
        let active_id = active_video.as_ref().map(|c| c.id);
        if self.stream_clip != active_id {
            if let Some(stream) = self.stream.take() {
                stream.stop();
            }
            self.stream_clip = active_id;
            if let Some(clip) = &active_video {
                if let Some(source) = &clip.source {
                    if let Some(asset) = self.asset(source) {
                        let start = clip.source_in + (self.playhead - clip.start);
                        match FrameStream::spawn(
                            source,
                            asset.info.width,
                            asset.info.height,
                            start,
                            PREVIEW_W,
                            self.timeline.fps,
                        ) {
                            Ok(stream) => self.stream = Some(stream),
                            Err(e) => self.status = format!("Video playback failed: {e}"),
                        }
                    }
                }
            }
        }

        // Audio players follow every audible clip under the playhead.
        let audible: Vec<openreel_engine::Clip> = self
            .timeline
            .clips
            .iter()
            .filter(|c| c.kind != ClipKind::Title && c.contains(self.playhead))
            .filter(|c| {
                c.source
                    .as_ref()
                    .and_then(|s| self.asset(s))
                    .map(|a| a.info.has_audio)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        self.audio_players
            .retain(|id, player| audible.iter().any(|c| c.id == *id) && !player.is_finished());
        for clip in &audible {
            if self.audio_players.contains_key(&clip.id) || self.audio_failed {
                continue;
            }
            let Some(source) = &clip.source else { continue };
            let start = clip.source_in + (self.playhead - clip.start);
            let remaining = (clip.end() - self.playhead).max(0.0);
            match AudioPlayer::play_segment(source, start, remaining, clip.volume) {
                Ok(player) => {
                    self.audio_players.insert(clip.id, player);
                }
                Err(e) => {
                    self.audio_failed = true;
                    self.status = format!("Audio playback failed: {e}");
                }
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
                    self.spawn_import(ctx, paths, false);
                }
            }
            UiAction::SampleProject => {
                self.pending_sample_project = true;
                self.spawn_import(ctx, Vec::new(), true);
                if self.import_rx.is_none() {
                    self.pending_sample_project = false;
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
                self.set_playing(false);
                self.timeline = Timeline::empty_project();
                self.selected = None;
                self.playhead = 0.0;
                self.still_key = None;
                self.still_cache.clear();
                self.status = "Timeline cleared. Media bin kept.".to_owned();
            }
            UiAction::SplitSelected => {
                if let Some(id) = self.selected {
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
            }
            UiAction::DeleteSelected => {
                if let Some(id) = self.selected.take() {
                    self.delete_clip(id);
                    self.status = "Clip deleted.".to_owned();
                }
            }
            UiAction::AddTitle => {
                let id = self.timeline.add_clip(
                    "New title",
                    ClipKind::Title,
                    TRACK_TITLES,
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
                })
            })
            .collect();
        let assets: Vec<serde_json::Value> = self
            .assets
            .iter()
            .map(|a| {
                serde_json::json!({
                    "name": a.info.name,
                    "path": a.info.path,
                    "duration": a.info.duration,
                    "width": a.info.width,
                    "height": a.info.height,
                    "hasVideo": a.info.has_video,
                    "hasAudio": a.info.has_audio,
                    "ready": a.state == AssetState::Ready,
                })
            })
            .collect();
        let mut sweep_candidates: Vec<serde_json::Value> = self
            .assets
            .iter()
            .flat_map(|asset| asset.sweep.candidates.iter())
            .filter(|candidate| !candidate.dismissed)
            .map(|candidate| {
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
                    "isReaction": candidate.is_reaction(),
                    "reactionScore": candidate.reaction_score(),
                    "hasVideo": candidate.has_video,
                    "transcript": candidate.transcript_text(),
                })
            })
            .collect();
        sweep_candidates.sort_by(|a, b| {
            b["score"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&a["score"].as_f64().unwrap_or(0.0))
        });
        // Turn-level transcript segments (Voice Transcribe has no
        // word-level timing), id-matched with the pinned digest so
        // requests like "the part where he talks about pricing"
        // resolve against real ranges.
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
            "tool": match self.timeline_view.tool {
                TimelineTool::Select => "select",
                TimelineTool::Split => "split",
            },
            "magnet": self.timeline_view.magnet,
            "clips": clips,
            "assets": assets,
            "sweepCandidates": sweep_candidates,
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
            match command {
                ScriptCommand::Log(msg) => notes.push(msg),
                ScriptCommand::Play => self.set_playing(true),
                ScriptCommand::Pause => self.set_playing(false),
                ScriptCommand::TogglePlay => {
                    let next = !self.playing;
                    self.set_playing(next);
                }
                ScriptCommand::Seek(t) => self.seek(t),
                ScriptCommand::StepFrames(n) => {
                    self.seek(self.playhead + n as f64 / self.timeline.fps);
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
                            if let Some(asset) = c.source.as_ref().and_then(|s| self.asset(s)) {
                                end =
                                    end.min(c.start + (asset.info.duration - c.source_in).max(0.0));
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
                    let path = self
                        .assets
                        .iter()
                        .find(|a| a.info.path == asset || a.info.name == asset)
                        .map(|a| a.info.path.clone());
                    match path {
                        Some(path) => self.add_asset_to_timeline(&path, at),
                        None => notes.push(format!("error: no asset '{asset}'")),
                    }
                }
                ScriptCommand::AddAssetRange {
                    asset,
                    at,
                    source_in,
                    duration,
                } => {
                    let found = self
                        .assets
                        .iter()
                        .find(|a| a.info.path == asset || a.info.name == asset)
                        .map(|a| (a.info.path.clone(), a.info.name.clone()));
                    match found {
                        Some((path, name)) => {
                            self.add_asset_range_to_timeline(&path, &name, at, source_in, duration)
                        }
                        None => notes.push(format!("error: no asset '{asset}'")),
                    }
                }
                ScriptCommand::Import(path) => {
                    self.spawn_import(ctx, vec![PathBuf::from(path)], false);
                }
                ScriptCommand::LoadSample => {
                    self.pending_sample_project = true;
                    self.spawn_import(ctx, Vec::new(), true);
                    if self.import_rx.is_none() {
                        self.pending_sample_project = false;
                    }
                }
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
                    "select" => self.timeline_view.tool = TimelineTool::Select,
                    "split" => self.timeline_view.tool = TimelineTool::Split,
                    other => notes.push(format!("error: unknown tool '{other}'")),
                },
                ScriptCommand::SetMagnet(on) => self.timeline_view.magnet = on,
                ScriptCommand::SetZoom(px) => {
                    self.timeline_view.px_per_sec = px.clamp(4.0, 600.0);
                }
                ScriptCommand::CloseGaps => self.timeline.close_gaps(TRACK_VIDEO),
            }
        }
        notes
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
                    .fill(theme::CARD)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    theme::panel_title(ui, "CONSOLE");
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
                    if response.has_focus() {
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
            self.playhead += dt;
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
            if ctx.input(|i| i.key_pressed(egui::Key::S)) {
                if let Some(id) = self.selected {
                    if let Some(new_id) = self.timeline.split_at(id, self.playhead) {
                        self.selected = Some(new_id);
                        self.status = "Clip split at playhead.".to_owned();
                    }
                }
            }
            if ctx.input(|i| i.key_pressed(egui::Key::V)) {
                self.timeline_view.tool = TimelineTool::Select;
            }
            if ctx.input(|i| i.key_pressed(egui::Key::B)) {
                self.timeline_view.tool = TimelineTool::Split;
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
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(12, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("OpenReel").strong().size(15.0));
                    ui.separator();
                    ui.menu_button("File", |ui| {
                        if ui.button("Import media…").clicked() {
                            action = Some(UiAction::Import);
                            ui.close();
                        }
                        if ui.button("Load sample project").clicked() {
                            action = Some(UiAction::SampleProject);
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
                        if theme::outline_button(ui, "Import media")
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
                    .fill(theme::CARD)
                    .inner_margin(egui::Margin::symmetric(12, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
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
                            ui.monospace(format!(
                                "{:.0} fps · {} · UI {:.2} ms · RSS {} MB",
                                self.timeline.fps,
                                format_timecode(self.timeline.end(), self.timeline.fps),
                                self.frame_ms_ema,
                                self.rss_mb
                            ));
                        }
                    });
                });
            });
    }

    fn timeline_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("timeline")
            .resizable(true)
            .default_height(240.0)
            .height_range(170.0..=460.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    theme::panel_title(ui, "TIMELINE");
                    let select_active = self.timeline_view.tool == TimelineTool::Select;
                    let select_response = if select_active {
                        theme::outline_button(ui, "Select")
                    } else {
                        theme::ghost_button(ui, "Select")
                    };
                    if select_response
                        .on_hover_text("Select and move clips (V)")
                        .clicked()
                    {
                        self.timeline_view.tool = TimelineTool::Select;
                    }
                    let split_active = self.timeline_view.tool == TimelineTool::Split;
                    let split_response = if split_active {
                        theme::outline_button(ui, "Split tool")
                    } else {
                        theme::ghost_button(ui, "Split tool")
                    };
                    if split_response
                        .on_hover_text("Click a clip to cut it at that point (B)")
                        .clicked()
                    {
                        self.timeline_view.tool = TimelineTool::Split;
                    }
                    let magnet_response = if self.timeline_view.magnet {
                        theme::outline_button(ui, "Magnet")
                    } else {
                        theme::ghost_button(ui, "Magnet")
                    };
                    if magnet_response
                        .on_hover_text(
                            "Magnet: snap to edges and close gaps on the main video track (M)",
                        )
                        .clicked()
                    {
                        self.timeline_view.magnet = !self.timeline_view.magnet;
                    }
                    if theme::ghost_button(ui, "Add title")
                        .on_hover_text("Add a title clip at the playhead")
                        .clicked()
                    {
                        let id = self.timeline.add_clip(
                            "New title",
                            ClipKind::Title,
                            TRACK_TITLES,
                            self.playhead,
                            4.0,
                            0.0,
                        );
                        self.selected = Some(id);
                        self.status = "Title added.".to_owned();
                    }
                    if theme::ghost_button(ui, "Split")
                        .on_hover_text("Split the selected clip at the playhead (S)")
                        .clicked()
                    {
                        if let Some(id) = self.selected {
                            match self.timeline.split_at(id, self.playhead) {
                                Some(new_id) => self.selected = Some(new_id),
                                None => {
                                    self.status =
                                        "Playhead is not inside the selected clip.".to_owned()
                                }
                            }
                        }
                    }
                    if theme::ghost_button(ui, "Delete")
                        .on_hover_text("Delete the selected clip (Del)")
                        .clicked()
                    {
                        if let Some(id) = self.selected.take() {
                            self.delete_clip(id);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Slider::new(&mut self.timeline_view.px_per_sec, 4.0..=600.0)
                                .logarithmic(true)
                                .show_value(false),
                        )
                        .on_hover_text("Timeline zoom. Ctrl+scroll over the timeline also zooms.");
                        ui.label(RichText::new("Zoom").size(12.0).color(theme::MUTED_FG));
                    });
                });
                ui.add_space(6.0);
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

    fn inspector(&mut self, ctx: &egui::Context) {
        if !self.show_inspector {
            return;
        }
        egui::SidePanel::right("inspector")
            .resizable(true)
            .default_width(272.0)
            .width_range(220.0..=420.0)
            .frame(egui::Frame::new().fill(theme::BACKGROUND).inner_margin(12))
            .show(ctx, |ui| {
                theme::panel_title(ui, "INSPECTOR");
                ui.add_space(10.0);
                let Some(id) = self.selected else {
                    ui.label(RichText::new("No clip selected.").color(theme::MUTED_FG));
                    return;
                };

                let mut do_split = false;
                let mut do_delete = false;
                if let Some(clip) = self.timeline.clip_mut(id) {
                    ui.add(egui::TextEdit::singleline(&mut clip.name).desired_width(f32::INFINITY));
                    ui.add_space(6.0);
                    let source_label = match &clip.source {
                        Some(path) => path.clone(),
                        None => "Generated clip".to_owned(),
                    };
                    ui.label(
                        RichText::new(source_label)
                            .size(11.0)
                            .color(theme::MUTED_FG),
                    );
                    ui.add_space(10.0);
                    let is_audio = clip.kind == ClipKind::Audio;
                    egui::Grid::new("clip_props")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Start (s)");
                            ui.add(
                                egui::DragValue::new(&mut clip.start)
                                    .speed(0.05)
                                    .range(0.0..=86400.0),
                            );
                            ui.end_row();
                            ui.label("Duration (s)");
                            ui.add(
                                egui::DragValue::new(&mut clip.duration)
                                    .speed(0.05)
                                    .range(0.1..=86400.0),
                            );
                            ui.end_row();
                            if clip.source.is_some() {
                                ui.label("Source in (s)");
                                ui.add(
                                    egui::DragValue::new(&mut clip.source_in)
                                        .speed(0.05)
                                        .range(0.0..=86400.0),
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
                    ui.add_space(14.0);
                    if theme::outline_button(ui, "Split at playhead")
                        .on_hover_text("Split this clip at the playhead (S)")
                        .clicked()
                    {
                        do_split = true;
                    }
                    ui.add_space(6.0);
                    if theme::destructive_button(ui, "Delete clip")
                        .on_hover_text("Delete this clip (Del)")
                        .clicked()
                    {
                        do_delete = true;
                    }
                }

                // Snap and clamp against the real source duration.
                if let Some(start) = self.timeline.clip(id).map(|c| c.start) {
                    self.timeline.move_clip(id, start);
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
            });
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BACKGROUND).inner_margin(12))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    theme::panel_title(ui, "PROGRAM");
                });
                ui.add_space(4.0);

                let avail = ui.available_size();
                let (rect, _) = ui.allocate_exact_size(
                    Vec2::new(avail.x, (avail.y - 52.0).clamp(0.0, avail.y)),
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
                    self.preview_texture.as_ref(),
                    audio_peaks,
                );

                let row_w = ui.available_width();
                ui.horizontal(|ui| {
                    ui.add_space(((row_w - 360.0) / 2.0).max(0.0));
                    if transport_button(ui, TransportIcon::ToStart, "Go to start (Home)").clicked()
                    {
                        self.seek(0.0);
                    }
                    if theme::ghost_button(ui, "-1f")
                        .on_hover_text("Step back one frame (←)")
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
                    if theme::ghost_button(ui, "+1f")
                        .on_hover_text("Step forward one frame (→)")
                        .clicked()
                    {
                        self.seek(self.playhead + 1.0 / self.timeline.fps);
                    }
                    if transport_button(ui, TransportIcon::ToEnd, "Go to end (End)").clicked() {
                        self.seek(self.timeline.end());
                    }
                    ui.add_space(10.0);
                    ui.monospace(format!(
                        "{} / {}",
                        format_timecode(self.playhead, self.timeline.fps),
                        format_timecode(self.timeline.end(), self.timeline.fps)
                    ));
                });
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
            self.spawn_import(ctx, paths, false);
        }

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.spawn_import(ctx, dropped, false);
        }

        let mut action = self.menu_bar(ctx);
        self.status_bar(ctx);
        self.console_panel(ctx);
        self.timeline_panel(ctx);
        self.chat_panel(ctx);
        self.chat_settings_window(ctx);
        if let Some(a) = self.media_bin(ctx) {
            action = Some(a);
        }
        self.inspector(ctx);
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

enum TransportIcon {
    ToStart,
    Play,
    Pause,
    ToEnd,
}

/// Transport glyphs drawn as shapes — no icon-font dependency, always sharp.
fn transport_button(ui: &mut Ui, icon: TransportIcon, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(38.0, 32.0), Sense::click());
    let painter = ui.painter();
    if response.is_pointer_button_down_on() {
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
    let fg = theme::FOREGROUND;
    let tri = |flip: f32| {
        vec![
            c + Vec2::new(-5.0 * flip, -7.0),
            c + Vec2::new(7.0 * flip, 0.0),
            c + Vec2::new(-5.0 * flip, 7.0),
        ]
    };
    match icon {
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
            TRACK_VIDEO,
            0.0,
            10.0,
            0.0,
            "/media/a.mp4",
        );
        let b_id = app.timeline.add_media_clip(
            "b.mp4",
            ClipKind::Video,
            TRACK_VIDEO,
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

    /// The magnet contract for deletes: with the magnet on, deleting
    /// a main-track clip ripples the following clips closed. This
    /// moved from a per-frame close in update() to the delete sites.
    #[test]
    fn delete_with_magnet_closes_gap() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        assert!(app.timeline_view.magnet);
        app.timeline
            .add_clip("a.mp4", ClipKind::Video, TRACK_VIDEO, 0.0, 10.0, 0.0);
        let b_id = app
            .timeline
            .add_clip("b.mp4", ClipKind::Video, TRACK_VIDEO, 10.0, 10.0, 0.0);
        let c_id = app
            .timeline
            .add_clip("c.mp4", ClipKind::Video, TRACK_VIDEO, 20.0, 10.0, 0.0);
        app.selected = Some(b_id);
        app.handle_action(&ctx, UiAction::DeleteSelected);
        assert!(app.timeline.clip(b_id).is_none());
        let c = app.timeline.clip(c_id).unwrap();
        assert!(
            (c.start - 10.0).abs() < 1e-6,
            "magnet delete should ripple C to 10, got {}",
            c.start
        );
    }

    #[test]
    fn delete_without_magnet_keeps_gap() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        app.timeline_view.magnet = false;
        app.timeline
            .add_clip("a.mp4", ClipKind::Video, TRACK_VIDEO, 0.0, 10.0, 0.0);
        let b_id = app
            .timeline
            .add_clip("b.mp4", ClipKind::Video, TRACK_VIDEO, 10.0, 10.0, 0.0);
        let c_id = app
            .timeline
            .add_clip("c.mp4", ClipKind::Video, TRACK_VIDEO, 20.0, 10.0, 0.0);
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

//! Media bin and import pipeline: placeholders, background probe and
//! thumbnail/waveform processing, and the image grid.

use std::path::PathBuf;
use std::sync::mpsc::{channel, TryRecvError};
use std::thread;

use eframe::egui::{
    self, ColorImage, CornerRadius, RichText, Sense, Shape, Stroke, TextureHandle, TextureOptions,
    Vec2,
};
use openreel_engine::{format_timecode, ClipKind};
use openreel_media::{MediaInfo, RawFrame};

use super::sweep::{minute_second, SweepData, SweepStatus};
use super::{EditorApp, UiAction};
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AssetState {
    Probing,
    Processing,
    Ready,
}

pub(super) struct BinAsset {
    pub(super) info: MediaInfo,
    pub(super) size_bytes: u64,
    pub(super) state: AssetState,
    pub(super) peaks: Vec<f32>,
    pub(super) thumb_textures: Vec<(f64, TextureHandle)>,
    pub(super) sweep: SweepData,
}

pub(super) struct ImportedAsset {
    pub(super) info: MediaInfo,
    pub(super) peaks: Vec<f32>,
    thumbs: Vec<(f64, RawFrame)>,
}

pub(super) enum ImportMsg {
    Probed(Box<MediaInfo>),
    Ready(Box<ImportedAsset>),
    Failed(String, String),
    Finished,
}

impl EditorApp {
    pub(super) fn asset(&self, path: &str) -> Option<&BinAsset> {
        self.assets.iter().find(|a| a.info.path == path)
    }

    // -- import pipeline ------------------------------------------------------

    fn insert_placeholder(&mut self, path: &std::path::Path) {
        let path_str = path.to_string_lossy().into_owned();
        let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if let Some(existing) = self.assets.iter_mut().find(|a| a.info.path == path_str) {
            existing.size_bytes = size_bytes;
            existing.state = AssetState::Processing;
            return;
        }
        self.assets.push(BinAsset {
            info: MediaInfo {
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path_str.clone()),
                path: path_str,
                duration: 0.0,
                width: 0,
                height: 0,
                fps: 0.0,
                has_video: false,
                has_audio: false,
            },
            size_bytes,
            state: AssetState::Probing,
            peaks: Vec::new(),
            thumb_textures: Vec::new(),
            sweep: SweepData::default(),
        });
    }

    /// Import files into the bin. Batches queue: asking for more
    /// files while a batch runs adds placeholders now and processes
    /// them when the current batch finishes, instead of dropping
    /// the request.
    pub(super) fn spawn_import(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        for path in &paths {
            self.insert_placeholder(path);
        }
        if self.import_rx.is_some() {
            self.status = format!("Queued {} file(s) to import.", paths.len());
            self.import_queue.extend(paths);
            return;
        }
        self.start_import_batch(ctx, paths);
    }

    fn start_import_batch(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        let (tx, rx) = channel::<ImportMsg>();
        self.import_rx = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            for path in &paths {
                let path_str = path.to_string_lossy().into_owned();
                let msg = match openreel_media::probe(&path_str) {
                    Ok(info) => {
                        if tx.send(ImportMsg::Probed(Box::new(info.clone()))).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                        let peaks = if info.has_audio {
                            openreel_media::waveform_peaks(&path_str, info.duration, 20)
                                .unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                        let thumbs = if info.has_video {
                            openreel_media::thumbnails(
                                &path_str,
                                info.width,
                                info.height,
                                info.duration,
                            )
                            .unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                        ImportMsg::Ready(Box::new(ImportedAsset {
                            info,
                            peaks,
                            thumbs,
                        }))
                    }
                    Err(e) => ImportMsg::Failed(path_str, e),
                };
                if tx.send(msg).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
            let _ = tx.send(ImportMsg::Finished);
            ctx.request_repaint();
        });
    }

    pub(super) fn poll_imports(&mut self, ctx: &egui::Context) {
        let mut finished = false;
        loop {
            let msg = match self.import_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(msg)) => msg,
                Some(Err(TryRecvError::Empty)) => break,
                Some(Err(TryRecvError::Disconnected)) => {
                    finished = true;
                    break;
                }
                None => break,
            };
            match msg {
                ImportMsg::Probed(info) => {
                    let info = *info;
                    if let Some(existing) =
                        self.assets.iter_mut().find(|a| a.info.path == info.path)
                    {
                        existing.info = info;
                        existing.state = AssetState::Processing;
                    } else {
                        let size_bytes =
                            std::fs::metadata(&info.path).map(|m| m.len()).unwrap_or(0);
                        self.assets.push(BinAsset {
                            info,
                            size_bytes,
                            state: AssetState::Processing,
                            peaks: Vec::new(),
                            thumb_textures: Vec::new(),
                            sweep: SweepData::default(),
                        });
                    }
                }
                ImportMsg::Ready(imported) => {
                    let ImportedAsset {
                        info,
                        peaks,
                        thumbs,
                    } = *imported;
                    let thumb_textures = thumbs
                        .into_iter()
                        .enumerate()
                        .map(|(i, (t, frame))| {
                            let image = ColorImage::from_rgba_unmultiplied(
                                [frame.width as usize, frame.height as usize],
                                &frame.rgba,
                            );
                            let handle = ctx.load_texture(
                                format!("thumb-{}-{i}", info.name),
                                image,
                                TextureOptions::LINEAR,
                            );
                            (t, handle)
                        })
                        .collect();
                    self.status = format!("Imported {}", info.name);
                    let ready_path = info.path.clone();
                    self.selected_asset = Some(ready_path.clone());
                    let size_bytes = std::fs::metadata(&info.path).map(|m| m.len()).unwrap_or(0);
                    if let Some(existing) =
                        self.assets.iter_mut().find(|a| a.info.path == info.path)
                    {
                        *existing = BinAsset {
                            info,
                            size_bytes,
                            state: AssetState::Ready,
                            peaks,
                            thumb_textures,
                            sweep: SweepData::default(),
                        };
                    } else {
                        self.assets.push(BinAsset {
                            info,
                            size_bytes,
                            state: AssetState::Ready,
                            peaks,
                            thumb_textures,
                            sweep: SweepData::default(),
                        });
                    }
                    self.queue_sweep(ctx, ready_path);
                }
                ImportMsg::Failed(path, err) => {
                    self.assets
                        .retain(|a| !(a.info.path == path && a.state != AssetState::Ready));
                    self.status = format!("Could not import {path}: {err}");
                }
                ImportMsg::Finished => finished = true,
            }
        }
        if finished {
            self.assets.retain(|a| a.state != AssetState::Probing);
            self.import_rx = None;
            if !self.import_queue.is_empty() {
                let next = std::mem::take(&mut self.import_queue);
                self.start_import_batch(ctx, next);
            }
        }
    }

    /// First lane with no clip overlapping [at, at+duration), or a
    /// freshly appended lane. Lanes are kind-agnostic.
    pub(super) fn first_free_track(&mut self, at: f64, duration: f64) -> usize {
        let end = at + duration;
        for track in 0..self.timeline.track_labels.len() {
            let blocked = self
                .timeline
                .clips
                .iter()
                .any(|c| c.track == track && c.start < end && at < c.end());
            if !blocked {
                return track;
            }
        }
        self.timeline.append_track()
    }

    pub(super) fn add_asset_to_timeline(&mut self, path: &str, track: Option<usize>, at: f64) {
        let Some(asset) = self.asset(path) else {
            return;
        };
        let (name, duration) = (asset.info.name.clone(), asset.info.duration);
        self.add_asset_range_to_timeline(path, &name, track, at, 0.0, duration);
    }

    pub(super) fn add_asset_range_to_timeline(
        &mut self,
        path: &str,
        title: &str,
        track: Option<usize>,
        at: f64,
        source_in: f64,
        duration: f64,
    ) {
        let Some(asset) = self.asset(path) else {
            self.status = "That media is no longer in the bin.".to_owned();
            return;
        };
        if asset.state != AssetState::Ready {
            self.status = format!("Still importing {}", asset.info.name);
            return;
        }
        let asset_name = asset.info.name.clone();
        let source_duration = asset.info.duration;
        let has_video = asset.info.has_video;
        let source_in = source_in.clamp(0.0, source_duration.max(0.0));
        let duration = duration.min((source_duration - source_in).max(0.0));
        if duration <= 0.0 {
            self.status = "That clip range is outside the media.".to_owned();
            return;
        }
        let name = if title.trim().is_empty() {
            asset_name
        } else {
            title.to_owned()
        };
        let kind = if has_video {
            ClipKind::Video
        } else {
            ClipKind::Audio
        };
        // An explicit drop lane wins (one past the end appends a
        // lane); otherwise the first lane free at that span.
        let track = match track {
            Some(requested) => {
                let track = requested.min(self.timeline.track_labels.len());
                self.timeline.ensure_track(track);
                track
            }
            None => self.first_free_track(at.max(0.0), duration),
        };
        let id = self.timeline.add_media_clip(
            &name,
            kind,
            track,
            at.max(0.0),
            duration,
            source_in,
            path,
        );
        self.selected = Some(id);
        self.status = format!("Added {name} at {}", format_timecode(at, self.timeline.fps));
    }

    // -- playback --------------------------------------------------------------

    pub(super) fn media_bin(&mut self, ctx: &egui::Context) -> Option<UiAction> {
        if !self.show_media {
            return None;
        }
        let mut action: Option<UiAction> = None;
        let mut add_request: Option<(String, Option<usize>, f64)> = None;
        let mut sweep_add: Option<String> = None;
        let mut sweep_dismiss: Option<String> = None;
        let mut sweep_add_all = false;
        egui::SidePanel::left("media_bin")
            .resizable(true)
            .default_width(268.0)
            .width_range(210.0..=400.0)
            .frame(egui::Frame::new().fill(theme::BACKGROUND).inner_margin(12))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    theme::panel_title(ui, "MEDIA");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::ghost_button(ui, "Import")
                            .on_hover_text("Import video or audio files")
                            .clicked()
                        {
                            action = Some(UiAction::Import);
                        }
                    });
                });
                ui.add_space(8.0);

                if self.assets.is_empty() {
                    ui.label(RichText::new("No media yet.").color(theme::MUTED_FG));
                    ui.add_space(10.0);
                    if theme::primary_button(ui, "Import media")
                        .on_hover_text("Import files, or drop them onto this window")
                        .clicked()
                    {
                        action = Some(UiAction::Import);
                    }
                    return;
                }

                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search media…")
                        .desired_width(f32::INFINITY),
                )
                .on_hover_text("Filter media by name");
                ui.add_space(8.0);

                let query = self.search.to_lowercase();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    const TILE_W: f32 = 118.0;
                    const TILE_H: f32 = 66.0;
                    const GAP: f32 = 8.0;
                    let filtered: Vec<usize> = self
                        .assets
                        .iter()
                        .enumerate()
                        .filter(|(_, a)| {
                            query.is_empty() || a.info.name.to_lowercase().contains(&query)
                        })
                        .map(|(i, _)| i)
                        .collect();
                    let columns =
                        (((ui.available_width() + GAP) / (TILE_W + GAP)).floor() as usize).max(1);
                    for row in filtered.chunks(columns) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = GAP;
                            for &asset_idx in row {
                                let asset = &self.assets[asset_idx];
                                let is_selected = self.selected_asset.as_deref()
                                    == Some(asset.info.path.as_str());
                                let (rect, response) = ui.allocate_exact_size(
                                    Vec2::new(TILE_W, TILE_H),
                                    Sense::click_and_drag(),
                                );
                                let stroke = if is_selected {
                                    Stroke::new(2.0_f32, theme::RING)
                                } else if response.hovered() || response.is_pointer_button_down_on()
                                {
                                    Stroke::new(1.0_f32, egui::Color32::from_white_alpha(70))
                                } else {
                                    Stroke::NONE
                                };
                                let painter = ui.painter().clone();
                                painter.rect_filled(
                                    rect,
                                    CornerRadius::same(theme::R_MD),
                                    theme::CARD,
                                );
                                let thumb_rect = rect;
                                let is_ready = asset.state == AssetState::Ready;
                                if let Some((_, thumb)) = asset.thumb_textures.first() {
                                    let tint = if is_ready {
                                        egui::Color32::WHITE
                                    } else {
                                        egui::Color32::from_rgb(105, 105, 105)
                                    };
                                    painter.image(
                                        thumb.id(),
                                        thumb_rect,
                                        cover_uv(thumb.size_vec2(), thumb_rect.size()),
                                        tint,
                                    );
                                } else if !asset.peaks.is_empty() {
                                    let mid = thumb_rect.center().y;
                                    let max_amp = thumb_rect.height() * 0.38;
                                    let mut x = thumb_rect.left() + 5.0;
                                    while x < thumb_rect.right() - 5.0 {
                                        let frac =
                                            (x - thumb_rect.left()) / thumb_rect.width().max(1.0);
                                        let idx = ((frac * asset.peaks.len() as f32) as usize)
                                            .min(asset.peaks.len() - 1);
                                        let amp = (asset.peaks[idx] * max_amp).max(1.0);
                                        painter.line_segment(
                                            [
                                                egui::Pos2::new(x, mid - amp),
                                                egui::Pos2::new(x, mid + amp),
                                            ],
                                            Stroke::new(
                                                2.0_f32,
                                                egui::Color32::from_rgb(0x4C, 0x9A, 0x68),
                                            ),
                                        );
                                        x += 4.0;
                                    }
                                } else if asset.info.has_video || asset.state == AssetState::Probing
                                {
                                    let c = thumb_rect.center();
                                    painter.add(Shape::convex_polygon(
                                        vec![
                                            c + Vec2::new(-7.0, -10.0),
                                            c + Vec2::new(10.0, 0.0),
                                            c + Vec2::new(-7.0, 10.0),
                                        ],
                                        egui::Color32::from_white_alpha(45),
                                        Stroke::NONE,
                                    ));
                                }
                                if !is_ready {
                                    painter.rect_filled(
                                        thumb_rect,
                                        CornerRadius::same(theme::R_MD),
                                        egui::Color32::from_black_alpha(100),
                                    );
                                    let spinner_rect = egui::Rect::from_center_size(
                                        thumb_rect.center(),
                                        Vec2::splat(20.0),
                                    );
                                    ui.put(spinner_rect, egui::Spinner::new().size(18.0));
                                }
                                if stroke != Stroke::NONE {
                                    painter.rect(
                                        rect,
                                        CornerRadius::same(theme::R_MD),
                                        egui::Color32::TRANSPARENT,
                                        stroke,
                                        egui::StrokeKind::Inside,
                                    );
                                }
                                if response.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if response.clicked() {
                                    self.selected_asset = Some(asset.info.path.clone());
                                }
                                if response.double_clicked() {
                                    add_request =
                                        Some((asset.info.path.clone(), None, self.playhead));
                                }
                                if response.dragged() {
                                    // Carry a grid-size preview at the
                                    // cursor so the drop reads clearly,
                                    // plus the lane it would land on.
                                    if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos())
                                    {
                                        let layer = egui::LayerId::new(
                                            egui::Order::Foreground,
                                            egui::Id::new("bin_drag_ghost"),
                                        );
                                        let ghost_painter = ui.ctx().layer_painter(layer);
                                        if let Some((track, _)) =
                                            self.timeline_view.drop_target(pos)
                                        {
                                            let lane_track =
                                                track.min(self.timeline.track_labels.len());
                                            if let Some(lane) =
                                                self.timeline_view.lane_rect(lane_track)
                                            {
                                                ghost_painter.rect(
                                                    lane,
                                                    CornerRadius::same(theme::R_MD),
                                                    egui::Color32::from_white_alpha(18),
                                                    Stroke::new(1.5_f32, theme::RING),
                                                    egui::StrokeKind::Inside,
                                                );
                                            }
                                        }
                                        let ghost = egui::Rect::from_min_size(
                                            pos + Vec2::new(12.0, 12.0),
                                            Vec2::new(TILE_W, TILE_H),
                                        );
                                        ghost_painter.rect_filled(
                                            ghost,
                                            CornerRadius::same(theme::R_MD),
                                            theme::CARD,
                                        );
                                        if let Some((_, thumb)) = asset.thumb_textures.first() {
                                            ghost_painter.image(
                                                thumb.id(),
                                                ghost,
                                                cover_uv(thumb.size_vec2(), ghost.size()),
                                                egui::Color32::from_white_alpha(220),
                                            );
                                        }
                                        ghost_painter.rect(
                                            ghost,
                                            CornerRadius::same(theme::R_MD),
                                            egui::Color32::TRANSPARENT,
                                            Stroke::new(2.0_f32, theme::RING),
                                            egui::StrokeKind::Inside,
                                        );
                                    }
                                }
                                if response.drag_stopped() {
                                    if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos())
                                    {
                                        if let Some((track, time)) =
                                            self.timeline_view.drop_target(pos)
                                        {
                                            add_request =
                                                Some((asset.info.path.clone(), Some(track), time));
                                        }
                                    }
                                }
                                let hover = if asset.state == AssetState::Ready {
                                    format!(
                                "{}\n{}\n{}\nDouble-click or drag onto the timeline to add.",
                                asset.info.name,
                                asset_meta(asset),
                                asset.info.path
                            )
                                } else {
                                    format!(
                                        "{}\n{}\nImporting…",
                                        asset.info.name,
                                        asset_meta(asset)
                                    )
                                };
                                response.on_hover_text(hover);
                            }
                        });
                        ui.add_space(GAP);
                    }

                    let sweep_started = self
                        .assets
                        .iter()
                        .any(|asset| asset.sweep.status != SweepStatus::NotStarted);
                    if sweep_started {
                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(6.0);
                        let candidate_refs = self.visible_sweep_candidates();
                        ui.horizontal(|ui| {
                            theme::panel_title(ui, "SWEEP");
                            if self
                                .assets
                                .iter()
                                .any(|asset| asset.sweep.status == SweepStatus::Running)
                            {
                                ui.add(egui::Spinner::new().size(13.0));
                                ui.label(RichText::new("Sweeping…").color(theme::MUTED_FG));
                            }
                            if !candidate_refs.is_empty() {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if theme::outline_button(ui, "Add all to timeline")
                                            .on_hover_text(
                                                "Append every sweep clip in ranked order",
                                            )
                                            .clicked()
                                        {
                                            sweep_add_all = true;
                                        }
                                    },
                                );
                            }
                        });
                        ui.add_space(6.0);

                        if candidate_refs.is_empty() {
                            if let Some(asset) = self.assets.iter().find(|asset| {
                                asset.sweep.status == SweepStatus::Failed
                                    || asset.sweep.status == SweepStatus::Ready
                            }) {
                                if !asset.sweep.note.is_empty() {
                                    ui.label(
                                        RichText::new(&asset.sweep.note)
                                            .size(12.0)
                                            .color(theme::MUTED_FG),
                                    );
                                }
                            }
                        }

                        for (rank, (asset_idx, candidate_idx)) in
                            candidate_refs.iter().copied().enumerate()
                        {
                            let asset = &self.assets[asset_idx];
                            let candidate = &asset.sweep.candidates[candidate_idx];
                            ui.horizontal_top(|ui| {
                                let (rect, thumb_response) =
                                    ui.allocate_exact_size(Vec2::new(76.0, 43.0), Sense::hover());
                                let painter = ui.painter().clone();
                                painter.rect_filled(
                                    rect,
                                    CornerRadius::same(theme::R_MD),
                                    theme::CARD,
                                );
                                let nearest_thumb = asset.thumb_textures.iter().min_by(|a, b| {
                                    (a.0 - candidate.source_in)
                                        .abs()
                                        .total_cmp(&(b.0 - candidate.source_in).abs())
                                });
                                if let Some((_, thumb)) = nearest_thumb {
                                    painter.image(
                                        thumb.id(),
                                        rect,
                                        cover_uv(thumb.size_vec2(), rect.size()),
                                        egui::Color32::WHITE,
                                    );
                                } else if !asset.peaks.is_empty() && asset.info.duration > 0.0 {
                                    let mid = rect.center().y;
                                    let max_amp = rect.height() * 0.36;
                                    let mut x = rect.left() + 4.0;
                                    while x < rect.right() - 4.0 {
                                        let frac = (x - rect.left()) / rect.width().max(1.0);
                                        let source_time =
                                            candidate.source_in + frac as f64 * candidate.duration;
                                        let idx = ((source_time / asset.info.duration) as f32
                                            * asset.peaks.len() as f32)
                                            as usize;
                                        let idx = idx.min(asset.peaks.len() - 1);
                                        let amp = (asset.peaks[idx] * max_amp).max(1.0);
                                        painter.line_segment(
                                            [
                                                egui::Pos2::new(x, mid - amp),
                                                egui::Pos2::new(x, mid + amp),
                                            ],
                                            Stroke::new(
                                                2.0_f32,
                                                egui::Color32::from_rgb(0x4C, 0x9A, 0x68),
                                            ),
                                        );
                                        x += 4.0;
                                    }
                                }
                                let transcript = candidate.transcript_text();
                                thumb_response.on_hover_text(format!(
                                    "{}\nScore {:.0} · Stability {:.0}% · Energy {:.0}%\n{}",
                                    candidate.title,
                                    candidate.score,
                                    candidate.stability * 100.0,
                                    candidate.audio_energy * 100.0,
                                    if transcript.is_empty() {
                                        "No speech in this clip.".to_owned()
                                    } else {
                                        transcript.clone()
                                    }
                                ));

                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{}. {} · {}–{}",
                                            rank + 1,
                                            candidate.asset_name,
                                            minute_second(candidate.source_in),
                                            minute_second(candidate.end())
                                        ))
                                        .size(12.0)
                                        .strong(),
                                    );
                                    ui.label(
                                        RichText::new(&candidate.reason)
                                            .size(12.0)
                                            .color(theme::MUTED_FG),
                                    );
                                    if !transcript.is_empty() {
                                        let excerpt: String = transcript.chars().take(96).collect();
                                        ui.label(
                                            RichText::new(format!("“{excerpt}”"))
                                                .size(12.0)
                                                .color(theme::MUTED_FG),
                                        );
                                    }
                                    ui.horizontal(|ui| {
                                        if theme::primary_button(ui, "Add")
                                            .on_hover_text("Add this sweep clip to the timeline")
                                            .clicked()
                                        {
                                            sweep_add = Some(candidate.id.clone());
                                        }
                                        if theme::ghost_button(ui, "Dismiss")
                                            .on_hover_text("Hide this suggestion")
                                            .clicked()
                                        {
                                            sweep_dismiss = Some(candidate.id.clone());
                                        }
                                    });
                                });
                            });
                            ui.add_space(8.0);
                        }
                    }
                });
            });
        if let Some((path, track, at)) = add_request {
            self.add_asset_to_timeline(&path, track, at);
        }
        if let Some(candidate_id) = sweep_add {
            self.add_sweep_candidate(&candidate_id);
        }
        if let Some(candidate_id) = sweep_dismiss {
            self.dismiss_sweep_candidate(&candidate_id);
        }
        if sweep_add_all {
            self.add_all_sweep_candidates();
        }
        action
    }
}

/// Center-crop UVs so a thumbnail fills its tile without distortion.
fn cover_uv(source: Vec2, target: Vec2) -> egui::Rect {
    if source.x <= 0.0 || source.y <= 0.0 || target.x <= 0.0 || target.y <= 0.0 {
        return egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0));
    }
    let source_aspect = source.x / source.y;
    let target_aspect = target.x / target.y;
    if source_aspect > target_aspect {
        let fraction = target_aspect / source_aspect;
        let x0 = (1.0 - fraction) / 2.0;
        egui::Rect::from_min_max(
            egui::Pos2::new(x0, 0.0),
            egui::Pos2::new(x0 + fraction, 1.0),
        )
    } else {
        let fraction = source_aspect / target_aspect;
        let y0 = (1.0 - fraction) / 2.0;
        egui::Rect::from_min_max(
            egui::Pos2::new(0.0, y0),
            egui::Pos2::new(1.0, y0 + fraction),
        )
    }
}

fn asset_meta(asset: &BinAsset) -> String {
    let info = &asset.info;
    let size = format_file_size(asset.size_bytes);
    match asset.state {
        AssetState::Probing => format!("{} · {size}", file_extension(&info.name)),
        AssetState::Processing | AssetState::Ready => {
            let dur = format_duration(info.duration);
            if info.has_video {
                format!("{}×{} · {dur} · {size}", info.width, info.height)
            } else {
                format!("Audio · {dur} · {size}")
            }
        }
    }
}

fn file_extension(name: &str) -> String {
    name.rsplit('.')
        .next()
        .filter(|ext| ext.len() <= 5 && !ext.is_empty())
        .map(|ext| ext.to_uppercase())
        .unwrap_or_else(|| "FILE".to_owned())
}

fn format_file_size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{bytes} B")
    }
}

fn format_duration(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

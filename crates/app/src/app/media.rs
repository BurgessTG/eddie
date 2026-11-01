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

use super::sweep::{ReviewStatus, SweepData, SweepStatus};
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
                            openreel_media::waveform_peaks(
                                &path_str,
                                info.duration,
                                openreel_media::WAVEFORM_PEAKS_PER_SEC,
                            )
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
                    // Fill an empty selection only; a background batch
                    // must not take the curator card from the user.
                    let keep_selection = self
                        .selected_asset
                        .as_deref()
                        .is_some_and(|path| self.assets.iter().any(|a| a.info.path == path));
                    if !keep_selection {
                        self.selected_asset = Some(ready_path.clone());
                    }
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
                    // Import only makes the asset ready. Curation is
                    // an explicit Curate action in the media bin.
                    let _ = ready_path;
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
        if let Err(error) =
            self.try_add_asset_range_to_timeline(path, title, track, at, source_in, duration)
        {
            self.status = error;
        }
    }

    /// The command path needs a typed failure, not a status that
    /// its caller can accidentally replace with a success message.
    pub(super) fn try_add_asset_range_to_timeline(
        &mut self,
        path: &str,
        title: &str,
        track: Option<usize>,
        at: f64,
        source_in: f64,
        duration: f64,
    ) -> Result<openreel_engine::ClipId, String> {
        let Some(asset) = self.asset(path) else {
            return Err("That media is no longer in the bin.".to_owned());
        };
        if asset.state != AssetState::Ready {
            return Err(format!("Still importing {}", asset.info.name));
        }
        let asset_name = asset.info.name.clone();
        let source_duration = asset.info.duration;
        let has_video = asset.info.has_video;
        // Baked-in montage music must not ride into a new edit.
        // JS cannot mute a clip it just added (its id exists only
        // after apply), so the add path does it: if most of the
        // added range is curator-flagged music bed, start muted.
        // The volume slider in Details brings it back.
        let range_end = source_in + duration;
        let bed_overlap = asset.sweep.music_bed_overlap(source_in, range_end);
        let starts_muted = self.mute_music_beds && duration > 0.0 && bed_overlap >= duration * 0.5;
        let source_in = source_in.clamp(0.0, source_duration.max(0.0));
        let duration = duration.min((source_duration - source_in).max(0.0));
        if duration <= 0.0 {
            return Err("That clip range is outside the media.".to_owned());
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
        if starts_muted {
            if let Some(clip) = self.timeline.clip_mut(id) {
                clip.volume = 0.0;
            }
        }
        self.selected = Some(id);
        self.status = format!(
            "Added {name} at {}{}",
            format_timecode(at, self.timeline.fps),
            if starts_muted {
                " (muted: music bed)"
            } else {
                ""
            }
        );
        Ok(id)
    }

    // -- playback --------------------------------------------------------------

    pub(super) fn media_bin(&mut self, ctx: &egui::Context) -> Option<UiAction> {
        if !self.show_media {
            return None;
        }
        let mut action: Option<UiAction> = None;
        let mut add_request: Option<(String, Option<usize>, f64)> = None;
        let mut open_gallery = false;
        let mut curate_selected: Option<String> = None;
        let mut curate_all_new = false;
        egui::SidePanel::left("media_bin")
            .resizable(true)
            .default_width(280.0)
            .width_range(220.0..=420.0)
            .frame(theme::panel_frame())
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    theme::panel_title(ui, "Media");
                    if !self.assets.is_empty() {
                        ui.label(
                            RichText::new(self.assets.len().to_string())
                                .size(11.5)
                                .color(theme::SUBTLE_FG),
                        );
                    }
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
                    ui.add_space(8.0);
                    let (drop, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 132.0),
                        Sense::hover(),
                    );
                    ui.painter().rect(
                        drop,
                        CornerRadius::same(theme::R_LG),
                        theme::CARD,
                        Stroke::new(1.0_f32, theme::BORDER),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(
                        drop.center() - Vec2::new(0.0, 10.0),
                        egui::Align2::CENTER_CENTER,
                        "No media yet",
                        egui::FontId::proportional(13.0),
                        theme::FOREGROUND,
                    );
                    ui.painter().text(
                        drop.center() + Vec2::new(0.0, 10.0),
                        egui::Align2::CENTER_CENTER,
                        "Drop files here",
                        egui::FontId::proportional(11.5),
                        theme::MUTED_FG,
                    );
                    ui.add_space(10.0);
                    if theme::primary_button_wide(ui, "Import media")
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
                        .margin(egui::Margin::symmetric(8, 6))
                        .desired_width(f32::INFINITY),
                )
                .on_hover_text("Filter media by name");
                ui.add_space(10.0);

                let query = self.search.to_lowercase();
                let has_ready_assets = self
                    .assets
                    .iter()
                    .any(|asset| asset.state == AssetState::Ready);
                // The curator card is pinned below the grid at its measured
                // height from the last frame, so it never covers thumbnails.
                let card_id = ui.id().with("curator_card_height");
                let card_height: f32 = ui.ctx().data(|d| d.get_temp(card_id)).unwrap_or(132.0);
                let reserve = if has_ready_assets { card_height + 12.0 } else { 0.0 };
                let grid_height = (ui.available_height() - reserve).max(66.0);
                egui::ScrollArea::vertical()
                    .id_salt("media_grid")
                    .max_height(grid_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                    const GAP: f32 = 8.0;
                    const MIN_TILE_W: f32 = 112.0;
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
                        (((ui.available_width() + GAP) / (MIN_TILE_W + GAP)).floor() as usize)
                            .max(1);
                    // Tiles stretch to fill the row so the grid has even edges.
                    let tile_w = ((ui.available_width() - GAP * (columns - 1) as f32)
                        / columns as f32)
                        .floor()
                        .max(40.0);
                    let tile_h = (tile_w * 9.0 / 16.0).round();
                    for row in filtered.chunks(columns) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = GAP;
                            for &asset_idx in row {
                                let asset = &self.assets[asset_idx];
                                let is_selected = self.selected_asset.as_deref()
                                    == Some(asset.info.path.as_str());
                                let (rect, response) = ui.allocate_exact_size(
                                    Vec2::new(tile_w, tile_h),
                                    Sense::click_and_drag(),
                                );
                                let stroke = if is_selected {
                                    Stroke::new(2.0_f32, theme::BRAND)
                                } else if response.hovered() || response.is_pointer_button_down_on()
                                {
                                    Stroke::new(1.0_f32, egui::Color32::from_white_alpha(70))
                                } else {
                                    Stroke::NONE
                                };
                                let painter = ui.painter().clone();
                                let is_ready = asset.state == AssetState::Ready;
                                // Placeholders read as a grey card until
                                // their thumbnails or peaks arrive.
                                painter.rect_filled(
                                    rect,
                                    CornerRadius::same(theme::R_MD),
                                    if is_ready { theme::CARD } else { theme::SECONDARY },
                                );
                                let thumb_rect = rect;
                                let has_preview =
                                    !asset.thumb_textures.is_empty() || !asset.peaks.is_empty();
                                if let Some((_, thumb)) = asset.thumb_textures.first() {
                                    let tint = if is_ready {
                                        egui::Color32::WHITE
                                    } else {
                                        egui::Color32::from_rgb(105, 105, 105)
                                    };
                                    egui::Image::from_texture(thumb)
                                        .uv(cover_uv(thumb.size_vec2(), thumb_rect.size()))
                                        .tint(tint)
                                        .corner_radius(theme::R_MD)
                                        .paint_at(ui, thumb_rect);
                                } else if !asset.peaks.is_empty() {
                                    paint_peaks(&painter, thumb_rect, &asset.peaks);
                                } else if is_ready && asset.info.has_video {
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
                                    if has_preview {
                                        painter.rect_filled(
                                            thumb_rect,
                                            CornerRadius::same(theme::R_MD),
                                            egui::Color32::from_black_alpha(100),
                                        );
                                    }
                                    let spinner_rect = egui::Rect::from_center_size(
                                        thumb_rect.center(),
                                        Vec2::splat(20.0),
                                    );
                                    egui::Spinner::new().paint_at(ui, spinner_rect);
                                } else if asset.sweep.status == SweepStatus::Running {
                                    // Curating: a corner spinner, so the
                                    // thumbnail stays readable.
                                    let c = rect.right_top() + Vec2::new(-13.0, 13.0);
                                    painter.circle_filled(c, 11.0,
                                        egui::Color32::from_black_alpha(170),
                                    );
                                    egui::Spinner::new().paint_at(
                                        ui,
                                        egui::Rect::from_center_size(c, Vec2::splat(18.0)),
                                    );
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
                                            Vec2::new(tile_w, tile_h),
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
                });

                if has_ready_assets {
                    ui.add_space((ui.available_height() - card_height).max(12.0));
                    // On a short window the card scrolls rather than
                    // running off the bottom of the panel.
                    let card_area = ui.available_height();
                    egui::ScrollArea::vertical()
                        .id_salt("curator_card")
                        .max_height(card_area)
                        .show(ui, |ui| {
                    let card = theme::card_frame().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 8.0;
                        ui.horizontal(|ui| {
                            theme::panel_title(ui, "Curator");
                            let running = self
                                .assets
                                .iter()
                                .filter(|asset| asset.sweep.status == SweepStatus::Running)
                                .count();
                            if running > 0 {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.add(egui::Spinner::new().size(13.0))
                                            .on_hover_text(format!("Curating {}", count(running, "video")));
                                    },
                                );
                            }
                        });
                        let selected_asset = self.selected_asset.as_deref().and_then(|path| {
                            self.assets.iter().find(|asset| asset.info.path == path)
                        });
                        let half = ((ui.available_width() - 6.0) / 2.0).floor();
                        let Some(asset) = selected_asset else {
                            ui.label(theme::caption("Select a video, then press Curate."));
                            open_gallery = theme::secondary_button_wide(ui, "Browse gallery")
                                .on_hover_text("Pause playback and search, filter or sort curated clips")
                                .clicked();
                            return;
                        };
                        let needs_review = asset
                            .sweep
                            .candidates
                            .iter()
                            .filter(|clip| clip.review.status == ReviewStatus::NeedsReview)
                            .count();
                        let summary = match asset.sweep.status {
                            SweepStatus::NotStarted if asset.state != AssetState::Ready => {
                                "Importing…".to_owned()
                            }
                            SweepStatus::NotStarted => "Not curated yet".to_owned(),
                            SweepStatus::Running if self.sweep_queue.contains(&asset.info.path) => {
                                "Queued".to_owned()
                            }
                            SweepStatus::Running => asset.sweep.note.clone(),
                            SweepStatus::Failed => "Analysis failed".to_owned(),
                            _ if needs_review > 0 => format!(
                                "{} · {needs_review} need review",
                                count(asset.sweep.candidates.len(), "clip")
                            ),
                            _ => format!(
                                "{} · {} · {}",
                                if asset.sweep.usage.batches > 0 { "Reviewed" } else { "Local" },
                                count(asset.sweep.candidates.len(), "clip"),
                                count(asset.sweep.scene_clips.len(), "scene")
                            ),
                        };
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            // The selected source, so the card reads at a glance.
                            let (thumb_rect, _) =
                                ui.allocate_exact_size(Vec2::new(56.0, 32.0), Sense::hover());
                            let painter = ui.painter();
                            painter.rect_filled(
                                thumb_rect,
                                CornerRadius::same(theme::R_SM),
                                theme::SECONDARY,
                            );
                            if let Some((_, thumb)) = asset.thumb_textures.first() {
                                egui::Image::from_texture(thumb)
                                    .uv(cover_uv(thumb.size_vec2(), thumb_rect.size()))
                                    .corner_radius(theme::R_SM)
                                    .paint_at(ui, thumb_rect);
                            } else if !asset.peaks.is_empty() {
                                paint_peaks(ui.painter(), thumb_rect, &asset.peaks);
                            }
                            ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&asset.info.name).size(12.5).color(theme::FOREGROUND),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&asset.info.path);
                            let note = if asset.sweep.note.is_empty() {
                                summary.clone()
                            } else {
                                asset.sweep.note.clone()
                            };
                            ui.add(egui::Label::new(theme::caption(summary)).truncate())
                                .on_hover_text(note);
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let curate = match asset.sweep.status {
                                SweepStatus::NotStarted => Some((
                                    "Curate",
                                    true,
                                    "Extract every shot and review useful ranges; Meta review sends sampled frames and audio when configured",
                                )),
                                SweepStatus::Running => None,
                                SweepStatus::Ready => Some((
                                    "Re-curate",
                                    false,
                                    "Analyze again, for example after adding an API key",
                                )),
                                SweepStatus::Failed => {
                                    Some(("Retry", false, "Try analyzing this video again"))
                                }
                            };
                            let gallery_w = if curate.is_some() { half } else { half * 2.0 + 6.0 };
                            if let Some((label, primary, tooltip)) = curate {
                                ui.allocate_ui(egui::vec2(half, 30.0), |ui| {
                                    // curate_asset ignores unready assets;
                                    // show that instead of a silent click.
                                    ui.add_enabled_ui(asset.state == AssetState::Ready, |ui| {
                                        let response = if primary {
                                            theme::primary_button_wide(ui, label)
                                        } else {
                                            theme::secondary_button_wide(ui, label)
                                        };
                                        if response
                                            .on_hover_text(tooltip)
                                            .on_disabled_hover_text("Available once import finishes")
                                            .clicked()
                                        {
                                            curate_selected = Some(asset.info.path.clone());
                                        }
                                    });
                                });
                            }
                            ui.allocate_ui(egui::vec2(gallery_w, 30.0), |ui| {
                                open_gallery = theme::secondary_button_wide(ui, "Gallery")
                                    .on_hover_text("Pause playback and search, filter or sort curated clips")
                                    .clicked();
                            });
                        });
                        let uncurated = self
                            .assets
                            .iter()
                            .filter(|asset| {
                                asset.state == AssetState::Ready
                                    && asset.sweep.status == SweepStatus::NotStarted
                            })
                            .count();
                        let offer_all = uncurated > 1
                            || (uncurated == 1 && asset.sweep.status != SweepStatus::NotStarted);
                        if offer_all
                            && theme::ghost_button_wide(ui, &format!("Curate all new ({uncurated})"))
                                .on_hover_text("Curate every imported video that has not been curated")
                                .clicked()
                        {
                            curate_all_new = true;
                        }
                    });
                    let height = card.response.rect.height();
                    ui.ctx().data_mut(|d| d.insert_temp(card_id, height));
                        });
                }
            });
        if let Some((path, track, at)) = add_request {
            self.add_asset_to_timeline(&path, track, at);
        }
        if open_gallery {
            self.open_curator_gallery();
        }
        if let Some(path) = curate_selected {
            self.curate_asset(ctx, path);
        }
        if curate_all_new {
            self.curate_all_new(ctx);
        }
        action
    }
}

/// Whole-file waveform on an audio-tinted tile, in the timeline's audio
/// colours. Linear scale: a whole-file overview of loud music would fill
/// the tile solid under the timeline's square-root scale.
fn paint_peaks(painter: &egui::Painter, rect: egui::Rect, peaks: &[f32]) {
    painter.rect_filled(
        rect,
        CornerRadius::same(theme::R_MD),
        theme::CLIP_AUDIO.gamma_multiply(0.55),
    );
    let inner = rect.shrink2(Vec2::new(10.0, rect.height() * 0.22));
    let color = theme::CLIP_AUDIO.gamma_multiply(2.6);
    let mut x = inner.left();
    while x <= inner.right() {
        // Each bar shows the loudest peak in its slice of the file.
        let slice =
            |x: f32| (((x - inner.left()) / inner.width().max(1.0)) * peaks.len() as f32) as usize;
        let start = slice(x).min(peaks.len() - 1);
        let end = slice(x + 3.0).clamp(start + 1, peaks.len());
        let peak = peaks[start..end].iter().copied().fold(0.0_f32, f32::max);
        let amp = (peak.clamp(0.0, 1.0) * inner.height() / 2.0).max(1.0);
        painter.line_segment(
            [
                egui::Pos2::new(x, inner.center().y - amp),
                egui::Pos2::new(x, inner.center().y + amp),
            ],
            Stroke::new(1.5_f32, color),
        );
        x += 3.0;
    }
}

/// Center-crop UVs so a thumbnail fills its tile without distortion.
pub(super) fn cover_uv(source: Vec2, target: Vec2) -> egui::Rect {
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

fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

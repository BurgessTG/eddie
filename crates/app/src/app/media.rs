//! The media bin: an image grid of imported files, with drag to the
//! timeline and the Curate controls.

use eddie_core::editor::media::{asset_meta, count, AssetState};
use eddie_core::editor::sweep::{ReviewStatus, SweepStatus};
use eddie_core::Action;
use eframe::egui::{self, CornerRadius, RichText, Sense, Shape, Stroke, Vec2};

use super::EditorApp;
use crate::theme;

impl EditorApp {
    // -- playback --------------------------------------------------------------

    pub(super) fn media_bin(&mut self, ctx: &egui::Context) -> Option<Action> {
        if !self.show_media {
            return None;
        }
        let mut action: Option<Action> = None;
        let mut add_request: Option<(String, Option<usize>, f64)> = None;
        let mut remove_request: Option<String> = None;
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
                    if !self.editor.assets.is_empty() {
                        ui.label(
                            RichText::new(self.editor.assets.len().to_string())
                                .size(11.5)
                                .color(theme::SUBTLE_FG),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::ghost_button(ui, "Import")
                            .on_hover_text("Import video or audio files")
                            .clicked()
                        {
                            action = Some(Action::Import);
                        }
                    });
                });
                ui.add_space(8.0);

                if self.editor.assets.is_empty() {
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
                        action = Some(Action::Import);
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
                    .editor
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
                        .editor
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
                                let asset = &self.editor.assets[asset_idx];
                                let is_selected = self.editor.selected_asset.as_deref()
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
                                let clashes = self.editor.clashes_with_format(&asset.info);
                                // Placeholders read as a grey card until
                                // their thumbnails or peaks arrive.
                                painter.rect_filled(
                                    rect,
                                    CornerRadius::same(theme::R_MD),
                                    if is_ready { theme::CARD } else { theme::SECONDARY },
                                );
                                let thumb_rect = rect;
                                let has_preview = !self.thumbs(&asset.info.path).is_empty()
                                    || !asset.peaks.is_empty();
                                if let Some((_, thumb)) = self.thumbs(&asset.info.path).first() {
                                    // Dimmed: importing, or the other format.
                                    let tint = if is_ready && !clashes {
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
                                    self.editor.selected_asset = Some(asset.info.path.clone());
                                }
                                if response.double_clicked() {
                                    add_request =
                                        Some((asset.info.path.clone(), None, self.editor.playhead));
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
                                                track.min(self.editor.timeline.track_labels.len());
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
                                        if let Some((_, thumb)) = self.thumbs(&asset.info.path).first() {
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
                                let hover = if clashes {
                                    format!(
                                        "{}\n{}\n{}\nIts orientation doesn't match the {} project, so it isn't added.",
                                        asset.info.name,
                                        asset_meta(asset),
                                        asset.info.path,
                                        self.editor.timeline.orientation.as_str()
                                    )
                                } else if asset.state == AssetState::Ready {
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
                                response.context_menu(|ui| {
                                    if super::menu_item_destructive(
                                        ui,
                                        "Remove from bin",
                                        "Take this file out of the bin; the file stays on disk",
                                    ) {
                                        remove_request = Some(asset.info.path.clone());
                                    }
                                });
                                response.on_hover_text(format!("{hover}\nRight-click to remove it."));
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
                                .editor
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
                        let selected_asset = self.editor.selected_asset.as_deref().and_then(|path| {
                            self.editor.assets.iter().find(|asset| asset.info.path == path)
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
                            SweepStatus::Running if self.editor.sweep_queue.contains(&asset.info.path) => {
                                "Queued".to_owned()
                            }
                            SweepStatus::Running => asset.sweep.note.clone(),
                            SweepStatus::Failed => "Analysis failed".to_owned(),
                            _ if needs_review > 0 => format!(
                                "{} · {needs_review} need review",
                                count(asset.sweep.candidates.len(), "clip")
                            ),
                            _ => format!(
                                "{} · {}",
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
                            if let Some((_, thumb)) = self.thumbs(&asset.info.path).first() {
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
                                    "Find every shot, transcribe speech and rank usable ranges, on this computer unless Speech uses a cloud provider",
                                )),
                                SweepStatus::Running => None,
                                SweepStatus::Ready => Some((
                                    "Re-curate",
                                    false,
                                    "Analyze again, for example after changing the speech provider",
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
                            .editor
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
            self.editor.add_asset_to_timeline(&path, track, at);
        }
        if let Some(path) = remove_request {
            self.editor.remove_asset(&path);
        }
        if open_gallery {
            self.editor.open_curator_gallery();
        }
        if let Some(path) = curate_selected {
            self.editor.curate_asset(path);
        }
        if curate_all_new {
            self.editor.curate_all_new();
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

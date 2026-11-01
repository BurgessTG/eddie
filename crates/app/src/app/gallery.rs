//! Browsing curated ranges never changes the source inventory or timeline.

use std::cmp::Ordering;

use eframe::egui::{self, CornerRadius, RichText, Sense, Stroke, Vec2};

use super::media::{cover_uv, BinAsset};
use super::sweep::{ReviewStatus, SweepCandidate, SweepStatus, CURATOR_CATEGORIES};
use super::EditorApp;
use crate::theme;

mod thumbnails;
use thumbnails::GalleryThumbnails;

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(super) enum GalleryView {
    #[default]
    Clips,
    Scenes,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(super) enum GalleryQuality {
    #[default]
    All,
    Prime,
    Reviewed,
    NeedsReview,
    Local,
}

impl GalleryQuality {
    fn label(self) -> &'static str {
        match self {
            Self::All => "Any quality",
            Self::Prime => "Prime",
            Self::Reviewed => "Reviewed",
            Self::NeedsReview => "Needs review",
            Self::Local => "Local only",
        }
    }

    fn matches(self, clip: &SweepCandidate) -> bool {
        match self {
            Self::All => true,
            Self::Prime => clip.is_prime(),
            Self::Reviewed => clip.review.status == ReviewStatus::Reviewed,
            Self::NeedsReview => clip.review.status == ReviewStatus::NeedsReview,
            Self::Local => clip.review.status == ReviewStatus::Local,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(super) enum GalleryDuration {
    #[default]
    All,
    UnderTwo,
    TwoToEight,
    EightToThirty,
    ThirtyPlus,
}

impl GalleryDuration {
    fn label(self) -> &'static str {
        match self {
            Self::All => "Any duration",
            Self::UnderTwo => "Under 2 s",
            Self::TwoToEight => "2 to 8 s",
            Self::EightToThirty => "8 to 30 s",
            Self::ThirtyPlus => "30+ s",
        }
    }

    fn matches(self, duration: f64) -> bool {
        match self {
            Self::All => true,
            Self::UnderTwo => duration < 2.0,
            Self::TwoToEight => (2.0..8.0).contains(&duration),
            Self::EightToThirty => (8.0..30.0).contains(&duration),
            Self::ThirtyPlus => duration >= 30.0,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(super) enum GallerySort {
    #[default]
    Score,
    Source,
    Shortest,
    Longest,
}

impl GallerySort {
    fn label(self) -> &'static str {
        match self {
            Self::Score => "Best first",
            Self::Source => "Source order",
            Self::Shortest => "Shortest first",
            Self::Longest => "Longest first",
        }
    }

    fn compare(self, a: &SweepCandidate, b: &SweepCandidate) -> Ordering {
        match self {
            Self::Score => b.score.total_cmp(&a.score),
            Self::Source => Ordering::Equal,
            Self::Shortest => a.duration.total_cmp(&b.duration),
            Self::Longest => b.duration.total_cmp(&a.duration),
        }
    }
}

#[derive(Default, PartialEq, Eq, Hash)]
pub(super) struct GalleryFilters {
    pub(super) search: String,
    pub(super) category: Option<String>,
    pub(super) quality: GalleryQuality,
    pub(super) duration: GalleryDuration,
    pub(super) sort: GallerySort,
}

impl GalleryFilters {
    fn matches(&self, clip: &SweepCandidate, words: &[&str]) -> bool {
        if self
            .category
            .as_ref()
            .is_some_and(|category| *category != clip.category)
            || !self.quality.matches(clip)
            || !self.duration.matches(clip.duration)
        {
            return false;
        }
        words.iter().all(|word| {
            [&clip.asset_name, &clip.title, &clip.category, &clip.reason]
                .into_iter()
                .chain(clip.tags.iter())
                .chain(clip.transcript.iter().map(|turn| &turn.text))
                .any(|text| text.to_lowercase().contains(word))
        })
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        let category = category_label(self.category.as_deref().unwrap_or("all")).to_owned();
        filter_choice(ui, "gallery_category", 110.0, &category, |ui| {
            ui.selectable_value(&mut self.category, None, "All types")
                .on_hover_text("Show every content category");
            for category in CURATOR_CATEGORIES {
                ui.selectable_value(
                    &mut self.category,
                    Some(category.to_owned()),
                    category_label(category),
                )
                .on_hover_text("Match this category as well as the other active filters");
            }
        })
        .on_hover_text("Filter by content type");
        filter_choice(ui, "gallery_quality", 120.0, self.quality.label(), |ui| {
            for quality in [
                GalleryQuality::All,
                GalleryQuality::Prime,
                GalleryQuality::Reviewed,
                GalleryQuality::NeedsReview,
                GalleryQuality::Local,
            ] {
                ui.selectable_value(&mut self.quality, quality, quality.label())
                    .on_hover_text(
                        "Filter quality or review status without replacing the content category",
                    );
            }
        })
        .on_hover_text("Filter quality or review status independently of type");
        filter_choice(ui, "gallery_duration", 115.0, self.duration.label(), |ui| {
            for duration in [
                GalleryDuration::All,
                GalleryDuration::UnderTwo,
                GalleryDuration::TwoToEight,
                GalleryDuration::EightToThirty,
                GalleryDuration::ThirtyPlus,
            ] {
                ui.selectable_value(&mut self.duration, duration, duration.label())
                    .on_hover_text("Filter by usable range length; each upper bound is exclusive");
            }
        })
        .on_hover_text("Find shorter inserts or longer usable takes");
        if theme::ghost_button(ui, "Reset")
            .on_hover_text("Clear clip filters and restore Best first; keep the selected source")
            .clicked()
        {
            *self = Self::default();
        }
    }
}

#[derive(Default)]
pub(super) struct CuratorGallery {
    pub(super) open: bool,
    pub(super) view: GalleryView,
    pub(super) filters: GalleryFilters,
    thumbnails: Option<GalleryThumbnails>,
}

impl EditorApp {
    pub(super) fn open_curator_gallery(&mut self) {
        self.set_playing(false);
        self.curator.open = true;
    }

    pub(super) fn curator_candidate_refs(&self, scenes: bool) -> Vec<(usize, usize)> {
        let mut refs = if scenes {
            self.visible_scene_clips()
        } else {
            self.visible_sweep_candidates()
        };
        let query = self.curator.filters.search.to_lowercase();
        let words: Vec<_> = query.split_whitespace().collect();
        refs.retain(|(asset, clip)| {
            let asset = &self.assets[*asset];
            if self
                .selected_asset
                .as_ref()
                .is_some_and(|path| *path != asset.info.path)
            {
                return false;
            }
            scenes
                || self
                    .curator
                    .filters
                    .matches(&asset.sweep.candidates[*clip], &words)
        });
        refs.sort_by(|(a_asset, a), (b_asset, b)| {
            let (a, b) = if scenes {
                (
                    &self.assets[*a_asset].sweep.scene_clips[*a],
                    &self.assets[*b_asset].sweep.scene_clips[*b],
                )
            } else {
                (
                    &self.assets[*a_asset].sweep.candidates[*a],
                    &self.assets[*b_asset].sweep.candidates[*b],
                )
            };
            let order = if scenes {
                Ordering::Equal
            } else {
                self.curator.filters.sort.compare(a, b)
            };
            order
                .then_with(|| a_asset.cmp(b_asset))
                .then_with(|| a.source_in.total_cmp(&b.source_in))
                .then_with(|| a.id.cmp(&b.id))
        });
        refs
    }

    pub(super) fn gallery_content(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if let Some(view) = theme::segmented(ui, self.curator.view, &[
                (GalleryView::Clips, "Clips", "Search and sort usable ranges and ranges needing review"),
                (GalleryView::Scenes, "All scenes", "Every source shot, including rejected material; ignores Clip filters and stays in source order"),
            ]) {
                self.curator.view = view;
            }
            let source = self.selected_asset.as_deref().and_then(|path| self.asset(path))
                .map(|asset| asset.info.name.as_str()).unwrap_or("All sources");
            egui::ComboBox::from_id_salt("gallery_source")
                .width(180.0)
                .truncate()
                .selected_text(source)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.selected_asset, None, "All sources")
                        .on_hover_text("Browse ranges from every imported source");
                    for asset in &self.assets {
                        ui.selectable_value(&mut self.selected_asset, Some(asset.info.path.clone()), &asset.info.name)
                            .on_hover_text(&asset.info.path);
                    }
                }).response.on_hover_text("Choose one source or search across every source");
        });
        let scenes = self.curator.view == GalleryView::Scenes;
        ui.add_space(8.0);
        ui.add_enabled_ui(!scenes, |ui| {
            ui.horizontal(|ui| {
                let search_width = (ui.available_width() - 145.0).max(80.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.curator.filters.search)
                        .hint_text("Search clips…")
                        .margin(egui::Margin::symmetric(8, 6))
                        .desired_width(search_width),
                )
                .on_hover_text(
                    "Match words in source names, categories, tags, review notes and transcripts",
                );
                egui::ComboBox::from_id_salt("gallery_sort")
                    .width(125.0)
                    .selected_text(if scenes {
                        "Source order"
                    } else {
                        self.curator.filters.sort.label()
                    })
                    .show_ui(ui, |ui| {
                        for sort in [
                            GallerySort::Score,
                            GallerySort::Source,
                            GallerySort::Shortest,
                            GallerySort::Longest,
                        ] {
                            ui.selectable_value(&mut self.curator.filters.sort, sort, sort.label())
                                .on_hover_text("Add shown uses this same order");
                        }
                    })
                    .response
                    .on_hover_text("Sort the visible clips; ties keep source order");
            });
        });

        let compact = ui.available_width() < 720.0;
        let running = self.assets.iter().any(|asset| {
            asset.sweep.status == SweepStatus::Running
                && self
                    .selected_asset
                    .as_ref()
                    .is_none_or(|path| *path == asset.info.path)
        });
        let mut add_all = false;
        let refs = ui
            .horizontal_wrapped(|ui| {
                ui.add_enabled_ui(!scenes, |ui| {
                    if compact {
                        let active = [
                            self.curator.filters.category.is_some(),
                            self.curator.filters.quality != GalleryQuality::All,
                            self.curator.filters.duration != GalleryDuration::All,
                        ]
                        .into_iter()
                        .filter(|active| *active)
                        .count();
                        let label = if active == 0 {
                            "Filters".to_owned()
                        } else {
                            format!("Filters ({active})")
                        };
                        ui.menu_button(label, |ui| {
                            self.curator.filters.controls(ui);
                        })
                        .response
                        .on_hover_text(
                            "Combine content type, quality, review status and duration filters",
                        );
                    } else {
                        self.curator.filters.controls(ui);
                    }
                });
                add_all = theme::secondary_button(
                    ui,
                    if scenes {
                        "Add all scenes"
                    } else {
                        "Add shown"
                    },
                )
                .on_hover_text(if scenes {
                    "Append every source scene in source order"
                } else {
                    "Append exactly the matching clips in the displayed order"
                })
                .clicked();
                ui.checkbox(&mut self.mute_music_beds, "Mute music beds")
                    .on_hover_text(
                    "Add ranges with baked-in music muted; turn off to keep their original score",
                );
                let refs = self.curator_candidate_refs(scenes);
                ui.label(
                    RichText::new(format!(
                        "{} {}{}",
                        refs.len(),
                        if scenes { "scene" } else { "clip" },
                        if refs.len() == 1 { "" } else { "s" }
                    ))
                    .size(12.0)
                    .color(theme::MUTED_FG),
                )
                .on_hover_text("Matching ranges only; filtering does not remove source material");
                if running {
                    ui.add(egui::Spinner::new().size(12.0));
                    ui.label(RichText::new("Curating…").size(12.0).color(theme::MUTED_FG));
                }
                refs
            })
            .inner;
        ui.add_space(6.0);
        if add_all {
            if scenes {
                self.add_all_scene_clips();
            } else {
                self.add_all_sweep_candidates();
            }
        }
        if refs.is_empty() {
            ui.add_space(12.0);
            ui.label(
                RichText::new(if running {
                    "Results appear when curation finishes."
                } else {
                    "No matching clips."
                })
                .color(theme::MUTED_FG),
            );
            if !scenes
                && self.curator.filters != GalleryFilters::default()
                && theme::ghost_button(ui, "Reset filters")
                    .on_hover_text("Show every suggestion for this source again")
                    .clicked()
            {
                self.curator.filters = GalleryFilters::default();
            }
            return;
        }

        const GAP: f32 = 14.0;
        const MIN_CARD_WIDTH: f32 = 176.0;
        let width = (ui.available_width() - ui.spacing().scroll.bar_width - 4.0).max(1.0);
        let columns = ((width + GAP) / (MIN_CARD_WIDTH + GAP)).floor().max(1.0) as usize;
        let card_width = ((width - GAP * (columns - 1) as f32) / columns as f32).min(280.0);
        let card_height = card_width * 9.0 / 16.0 + 84.0;
        let thumbnails = self
            .curator
            .thumbnails
            .get_or_insert_with(|| GalleryThumbnails::new(ui.ctx()));
        let mut add = None;
        let mut dismiss = None;
        egui::ScrollArea::vertical()
            .id_salt((
                "curator_gallery",
                self.curator.view,
                &self.curator.filters,
                &self.selected_asset,
            ))
            .auto_shrink([false, false])
            .show_rows(ui, card_height, refs.len().div_ceil(columns), |ui, rows| {
                ui.spacing_mut().item_spacing.x = GAP;
                for row in rows {
                    ui.horizontal_top(|ui| {
                        for &(asset, clip) in refs.iter().skip(row * columns).take(columns) {
                            let asset = &self.assets[asset];
                            let clip = if scenes {
                                &asset.sweep.scene_clips[clip]
                            } else {
                                &asset.sweep.candidates[clip]
                            };
                            ui.push_id(&clip.id, |ui| {
                                let (add_clip, dismiss_clip) = gallery_card(
                                    ui,
                                    asset,
                                    clip,
                                    scenes,
                                    Vec2::new(card_width, card_height),
                                    thumbnails,
                                );
                                if add_clip {
                                    add = Some(clip.id.clone());
                                }
                                if dismiss_clip {
                                    dismiss = Some(clip.id.clone());
                                }
                            });
                        }
                    });
                }
            });
        if let Some(id) = add {
            self.add_sweep_candidate(&id);
        }
        if let Some(id) = dismiss {
            self.dismiss_sweep_candidate(&id);
        }
    }
}

fn filter_choice(
    ui: &mut egui::Ui,
    id: &'static str,
    width: f32,
    label: &str,
    choices: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    if egui::containers::menu::is_in_menu(ui) {
        // A ComboBox opens a separate root popup, closing its parent menu.
        ui.menu_button(label, choices).response
    } else {
        egui::ComboBox::from_id_salt(id)
            .width(width)
            .selected_text(label)
            .show_ui(ui, choices)
            .response
    }
}

fn category_label(category: &str) -> &str {
    match category {
        "all" => "All types",
        "reaction" => "Reaction",
        "talking" => "Talking",
        "action" => "Action",
        "broll" => "B-roll",
        "establishing" => "Establishing",
        other => other,
    }
}

/// Vertical breathing room between card rows.
const GAP_BELOW: f32 = 10.0;

fn gallery_card(
    ui: &mut egui::Ui,
    asset: &BinAsset,
    clip: &SweepCandidate,
    scenes: bool,
    size: Vec2,
    thumbnails: &mut GalleryThumbnails,
) -> (bool, bool) {
    let mut add = false;
    let mut dismiss = false;
    let card = egui::Rect::from_min_size(ui.cursor().min, size - Vec2::new(0.0, GAP_BELOW));
    let hovered = ui.rect_contains_pointer(card);
    ui.painter().rect(
        card,
        CornerRadius::same(theme::R_LG),
        theme::CARD,
        Stroke::new(
            1.0_f32,
            if hovered {
                theme::BORDER_STRONG
            } else {
                theme::BORDER
            },
        ),
        egui::StrokeKind::Inside,
    );
    let thumb_radius = CornerRadius {
        nw: theme::R_LG,
        ne: theme::R_LG,
        sw: 0,
        se: 0,
    };
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_width(size.x);
        ui.spacing_mut().item_spacing.y = 6.0;
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(size.x, size.x * 9.0 / 16.0), Sense::hover());
        ui.painter().rect_filled(rect, thumb_radius, theme::INPUT);
        let mut details = format!(
            "{}\n{} · {} · Score {:.0}\n{:.3} to {:.3} s · {:.3} s\n{}\n{}\n{}",
            clip.asset_name,
            category_label(&clip.category),
            clip.review.status.as_str(),
            clip.score,
            clip.source_in,
            clip.end(),
            clip.duration,
            clip.reason,
            clip.tags.join(", "),
            clip.transcript_text()
        );
        if clip.has_video {
            match thumbnails.texture(ui.ctx(), &asset.info, clip.source_in + clip.duration / 2.0) {
                Ok(Some(texture)) => {
                    egui::Image::from_texture(&texture)
                        .uv(cover_uv(texture.size_vec2(), rect.size()))
                        .corner_radius(thumb_radius)
                        .paint_at(ui, rect);
                }
                Ok(None) => {
                    egui::Spinner::new().paint_at(
                        ui,
                        egui::Rect::from_center_size(rect.center(), Vec2::splat(18.0)),
                    );
                }
                Err(error) => {
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Preview unavailable",
                        egui::FontId::proportional(12.0),
                        theme::MUTED_FG,
                    );
                    details.push_str(&format!("\n{error}"));
                }
            }
        } else {
            let mut x = rect.left() + 5.0;
            while x < rect.right() - 5.0 {
                let fraction = (x - rect.left()) / rect.width();
                let time = clip.source_in + fraction as f64 * clip.duration;
                let index = (time * openreel_media::WAVEFORM_PEAKS_PER_SEC as f64) as usize;
                let amplitude =
                    asset.peaks.get(index).copied().unwrap_or(0.0) * rect.height() * 0.38;
                ui.painter().line_segment(
                    [
                        egui::pos2(x, rect.center().y - amplitude),
                        egui::pos2(x, rect.center().y + amplitude),
                    ],
                    Stroke::new(2.0_f32, theme::CLIP_AUDIO),
                );
                x += 4.0;
            }
        }
        // Duration sits on the picture, like a player's time chip.
        let duration = format!("{:.2}s", clip.duration);
        let galley =
            ui.painter()
                .layout_no_wrap(duration, egui::FontId::monospace(10.5), theme::FOREGROUND);
        let chip = egui::Rect::from_min_size(
            rect.right_bottom() - galley.size() - Vec2::new(14.0, 10.0),
            galley.size() + Vec2::new(8.0, 4.0),
        );
        ui.painter().rect_filled(
            chip,
            CornerRadius::same(4),
            egui::Color32::from_black_alpha(180),
        );
        ui.painter()
            .galley(chip.min + Vec2::new(4.0, 2.0), galley, theme::FOREGROUND);
        response.on_hover_text(&details);
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 10,
                right: 10,
                top: 0,
                bottom: 8,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let label = if clip.review.status == ReviewStatus::NeedsReview {
                        "Needs review"
                    } else {
                        category_label(&clip.category)
                    };
                    ui.add(egui::Label::new(RichText::new(label).size(12.0).strong()).truncate())
                        .on_hover_text(&details);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{:.0}", clip.score))
                                .size(11.0)
                                .color(theme::SUBTLE_FG),
                        )
                        .on_hover_text(format!(
                            "Score {:.0} · source {:.3} to {:.3} seconds",
                            clip.score,
                            clip.source_in,
                            clip.end()
                        ));
                    });
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    add = theme::secondary_button(ui, "Add")
                        .on_hover_text("Append this exact source range to the timeline")
                        .clicked();
                    if !scenes {
                        dismiss = theme::ghost_button(ui, "Dismiss")
                            .on_hover_text("Hide this suggestion; keep its source in All scenes")
                            .clicked();
                    }
                });
            });
    });
    (add, dismiss)
}

//! The Gallery: curated ranges as a searchable grid of cards. Browsing never
//! changes the source inventory or the timeline.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use eddie_core::editor::gallery::{
    GalleryDuration, GalleryFilters, GalleryQuality, GallerySort, GalleryView, category_label,
    clock,
};
use eddie_core::editor::media::BinAsset;
use eddie_core::editor::sweep::{CURATOR_CATEGORIES, ReviewStatus, SweepCandidate, SweepStatus};
use rune::{
    Align, Color, Cx, El, GrowthSpec, LineCap, ScrollSpec, Shape, Side, SurfaceFit, TextEdit, col,
    el, row, spacer, text,
};
use rune_kit::{ScrollBar, ScrollBarStyle};

use crate::app::Ui;
use crate::gallery_thumbs::GalleryThumbnails;
use crate::media::spinner;
use crate::theme::{self, Variant, c};

const FIELD_H: f32 = 28.0;
const GAP: f32 = 8.0;
/// The view switch and the source list, which search and sort sit beside.
const SWITCH_W: f32 = 158.0;
/// The source list is this wide, and grows to show a longer name whole
/// until `SOURCE_MAX_W`.
const SOURCE_W: f32 = 160.0;
const SOURCE_MAX_W: f32 = 320.0;
/// What a list adds around its choice: padding, the gaps either side of
/// its spacer, and the arrow.
const SELECT_CHROME_W: f32 = 48.0;
const SORT_W: f32 = 120.0;
/// Search needs this much beside the sort list, or the pair takes a row of
/// its own.
const SEARCH_MIN: f32 = 180.0;
/// Narrower than this, the three filter lists fold into one Filters menu
/// and the second row wraps.
const COMPACT_W: f32 = 720.0;

const CARD_GAP: f32 = 14.0;
const MIN_CARD_W: f32 = 176.0;
const MAX_CARD_W: f32 = 280.0;
/// Under a card's picture: its label, source line and buttons.
const CARD_TEXT_H: f32 = 94.0;
/// Breathing room between card rows.
const GAP_BELOW: f32 = 8.0;
/// Room kept clear of cards for the scroll bar.
const BAR_RESERVE: f32 = 12.0;
/// A list longer than this scrolls.
const LIST_ROWS: usize = 10;
const OPTION_PITCH: f32 = 34.0;

/// The list hanging open under one of the Gallery's fields.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Popup {
    Source,
    Sort,
    Category,
    Quality,
    Duration,
    /// The narrow layout's one menu holding the three filter lists.
    Filters,
}

pub struct State {
    /// The search box. The filters own the search text; `sync` copies
    /// typing into them and a reset or script change back.
    pub search: TextEdit,
    synced: String,
    pub popup: Option<Popup>,
    /// The last view was the narrow layout.
    compact: Cell<bool>,
    /// Card pictures; the view asks for those of the cards it builds.
    thumbs: RefCell<GalleryThumbnails>,
}

impl State {
    /// `wake` runs on a worker thread when a card's picture is ready.
    pub fn new(wake: impl Fn() + Send + 'static) -> Self {
        Self {
            search: TextEdit::new(""),
            synced: String::new(),
            popup: None,
            compact: Cell::new(false),
            thumbs: RefCell::new(GalleryThumbnails::new(wake)),
        }
    }

    pub fn typing(&self) -> bool {
        self.search.focused
    }

    /// A card is waiting for its picture, so its spinner must keep turning.
    pub fn busy(&self) -> bool {
        self.thumbs.borrow().pending()
    }
}

/// Once a frame, before the view: finished pictures in, and the search box
/// and the filters brought back in step.
pub fn sync(ui: &mut Ui) {
    let gallery = &mut ui.gallery;
    gallery.thumbs.get_mut().collect();
    let curator = &mut ui.editor.curator;
    if gallery.search.text != gallery.synced {
        curator.filters.search = gallery.search.text.clone();
        gallery.synced = gallery.search.text.clone();
    } else if curator.filters.search != gallery.synced {
        gallery.search.replace_text(curator.filters.search.clone());
        gallery.synced = curator.filters.search.clone();
    }
    let scenes = curator.view == GalleryView::Scenes;
    let stale = match gallery.popup {
        None => false,
        _ if !curator.open => true,
        Some(Popup::Source) => false,
        Some(Popup::Filters) => scenes || !gallery.compact.get(),
        // All scenes ignores the Clip filters and stays in source order.
        Some(Popup::Sort | Popup::Category | Popup::Quality | Popup::Duration) => scenes,
    };
    if stale {
        gallery.popup = None;
    }
    if !curator.open || scenes {
        gallery.search.focused = false;
    }
}

fn compact(width: f32) -> bool {
    width < COMPACT_W
}

/// The Gallery's width and height as last laid out.
fn size(cx: &Cx<Ui>) -> [f32; 2] {
    cx.layout_size("gallery").unwrap_or([960.0, 480.0])
}

pub fn view(ui: &Ui, cx: &Cx<Ui>) -> El {
    let c = c();
    let editor = &ui.editor;
    let [width, height] = size(cx);
    let scenes = editor.curator.view == GalleryView::Scenes;
    let filters = &editor.curator.filters;
    let narrow = compact(width);
    ui.gallery.compact.set(narrow);
    let popup = ui.gallery.popup;

    // Row one picks what to browse: view and source, then search and order
    // beside them when they fit, or on their own row when not.
    let switch = theme::segmented(
        "gallery-view",
        vec![
            (
                "gallery-view-clips".to_owned(),
                "Clips",
                "Search, filter and sort the suggested ranges",
                !scenes,
            ),
            (
                "gallery-view-scenes".to_owned(),
                "All scenes",
                "Every source shot, including rejected material; ignores Clip filters and stays in source order",
                scenes,
            ),
        ],
        |index, segment| {
            let view = if index == 0 {
                GalleryView::Clips
            } else {
                GalleryView::Scenes
            };
            segment.on_press(move |ui: &mut Ui| ui.editor.curator.view = view)
        },
    );
    let source = editor
        .selected_asset
        .as_deref()
        .and_then(|path| editor.asset(path))
        .map_or("All sources", |asset| asset.info.name.as_str());
    let source_w = (rune::text::measure(source, &[], theme::BODY, None).0 + SELECT_CHROME_W)
        .clamp(SOURCE_W, SOURCE_MAX_W);
    let source = select(
        "gallery-source",
        source,
        "Choose one source or search across every source",
        Popup::Source,
        popup == Some(Popup::Source),
        true,
    )
    .w(source_w);
    let search = rune::text_input(&ui.gallery.search, theme::BODY)
        .selection_color(theme::selection())
        .key("gallery-search")
        .single_line()
        .grow(1.0)
        .min_w(80.0)
        .h(FIELD_H)
        .px(8.0)
        .py(6.0)
        .radius(theme::R_MD)
        .fill(c.input)
        .color(c.foreground)
        .placeholder("Search clips…", c.muted_fg)
        .focus_within_ring(1.0, -1.0, c.brand)
        .help("Match words in source names, titles, categories, tags, notes and transcripts")
        .disabled(scenes)
        .opacity(if scenes { 0.5 } else { 1.0 })
        .on_edit(|ui: &mut Ui| &mut ui.gallery.search);
    let sort = select(
        "gallery-sort",
        if scenes {
            "Source order"
        } else {
            filters.sort.label()
        },
        "Sort the visible clips; ties keep source order",
        Popup::Sort,
        popup == Some(Popup::Sort),
        !scenes,
    )
    .min_w(SORT_W);
    let inline = width - SWITCH_W - source_w - 2.0 * GAP >= SEARCH_MIN + GAP + SORT_W;
    let mut top = row()
        .w_full()
        .h(32.0)
        .gap(GAP)
        .center_y()
        .child(switch)
        .child(source);
    let mut content = col()
        .key("gallery")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        // Under the 16 the panel leaves below its tabs.
        .child(el().h(1.0));
    if inline {
        content = content.child(top.child(search).child(sort));
    } else {
        top = top.child(spacer());
        // The same pitch as the two rows of the wide layout.
        content = content.child(top).child(el().h(15.0)).child(
            row()
                .w_full()
                .h(FIELD_H)
                .gap(GAP)
                .center_y()
                .child(search)
                .child(sort),
        );
    }

    // Row two narrows and acts: filters on the left; the count, the music
    // policy and the one primary action on the right.
    let in_source = |asset: &BinAsset| {
        editor
            .selected_asset
            .as_deref()
            .is_none_or(|path| path == asset.info.path)
    };
    let running = editor
        .assets
        .iter()
        .any(|asset| asset.sweep.status == SweepStatus::Running && in_source(asset));
    let refs = editor.curator_candidate_refs(scenes);
    let count = text(format!(
        "{} {}{}",
        refs.len(),
        if scenes { "scene" } else { "clip" },
        if refs.len() == 1 { "" } else { "s" }
    ))
    .key("gallery-count")
    .font(theme::sized(12.0))
    .color(c.muted_fg)
    .single_line()
    .help("Matching ranges only; filtering does not remove source material");
    let add_all = theme::button(
        "gallery-add-all",
        if scenes {
            "Add all scenes"
        } else {
            "Add shown"
        },
        Variant::Primary,
        if scenes {
            "Append every source scene in source order"
        } else {
            "Append exactly the matching clips in the displayed order"
        },
        true,
    )
    .on_press(move |ui: &mut Ui| {
        if scenes {
            ui.editor.add_all_scene_clips();
        } else {
            ui.editor.add_all_sweep_candidates();
        }
    });
    let mute = theme::checkbox(
        "gallery-mute",
        "Mute music beds",
        "Add ranges with baked-in music muted; turn off to keep their original score",
        editor.mute_music_beds,
    )
    .on_press(|ui: &mut Ui| ui.editor.mute_music_beds = !ui.editor.mute_music_beds);
    // A spinner only: a text label here ran into Reset.
    let curating = running.then(|| {
        spinner(16.0, ui.turn())
            .key("gallery-curating")
            .help("Curating; results appear when it finishes")
    });
    let second = if narrow {
        let active = active_filters(filters);
        let label = if active == 0 {
            "Filters".to_owned()
        } else {
            format!("Filters ({active})")
        };
        let menu = theme::button(
            "gallery-filters",
            &label,
            Variant::Secondary,
            "Combine content type, quality and duration filters",
            !scenes,
        )
        .role("combobox")
        .expanded(popup.is_some_and(|popup| popup != Popup::Source && popup != Popup::Sort))
        .on_press(|ui: &mut Ui| toggle(ui, Popup::Filters));
        // Too narrow to split the row; wrap in reading order instead.
        row()
            .w_full()
            .flex_wrap()
            .gaps(GAP, GAP)
            .center_y()
            .child(menu)
            .child(add_all)
            .child(mute)
            .child(count)
            .children(curating)
    } else {
        row()
            .w_full()
            .h(FIELD_H)
            .gap(GAP)
            .center_y()
            .children(filter_controls(filters, popup, !scenes))
            .child(spacer())
            .children(curating)
            .child(count)
            .child(mute)
            .child(add_all)
    };
    content = content
        .child(el().h(if narrow { 14.0 } else { 15.0 }))
        .child(second);

    if refs.is_empty() {
        let ready = editor
            .assets
            .iter()
            .any(|asset| asset.sweep.status == SweepStatus::Ready && in_source(asset));
        content = content.child(el().h(24.0)).child(
            text(if running {
                "Results appear when curation finishes."
            } else if !ready {
                "Curate a video to see its clips here."
            } else {
                "No matching clips."
            })
            .key("gallery-empty")
            .font(theme::BODY)
            .color(c.muted_fg),
        );
        if !scenes && *filters != GalleryFilters::default() {
            content = content.child(el().h(6.0)).child(
                row().child(
                    theme::button(
                        "gallery-reset-filters",
                        "Reset filters",
                        Variant::Ghost,
                        "Show every suggestion for this source again",
                        true,
                    )
                    .on_press(reset),
                ),
            );
        }
        return content;
    }

    // The grid's own box when it has been laid out; before that, what the
    // rows above leave of the Gallery.
    let [grid_w, grid_h] = cx
        .layout_size("gallery-grid")
        .unwrap_or([width, (height - 94.0).max(1.0)]);
    let (columns, card_w, wider) = grid(grid_w, grid_h);
    let pitch = (card_w * 9.0 / 16.0).round() + CARD_TEXT_H + GAP_BELOW;
    let refs = Rc::new(refs);
    let keys = Rc::clone(&refs);
    let rows = col()
        .key("gallery-rows")
        .w_full()
        .growth(GrowthSpec::default())
        .instant_changes()
        .virtual_rows(
            refs.len().div_ceil(columns),
            pitch,
            move |ui: &Ui, index| match range(ui, &keys, index * columns, scenes) {
                Some((_, clip)) => Cow::Borrowed(clip.id.as_str()),
                None => Cow::Owned(format!("gone-{index}")),
            },
            move |ui: &Ui, index| {
                let cards = (index * columns..refs.len().min((index + 1) * columns))
                    .filter_map(|at| Some((at % columns, range(ui, &refs, at, scenes)?)))
                    .map(|(column, (asset, clip))| {
                        let w = card_w + if column < wider { 1.0 } else { 0.0 };
                        card(ui, asset, clip, scenes, w, pitch - GAP_BELOW)
                    });
                row().h(pitch).gap(CARD_GAP).children(cards)
            },
        );
    // A new search, filter, order or source starts at the top again.
    let mut hasher = DefaultHasher::new();
    (editor.curator.view, filters, &editor.selected_asset).hash(&mut hasher);
    content.child(el().h(12.0)).child(
        col()
            .key("gallery-grid")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(
                col()
                    .key(format!("gallery-scroll-{:x}", hasher.finish()))
                    .w_full()
                    .grow(1.0)
                    .min_h(0.0)
                    .scroll(ScrollSpec {
                        stick_bottom: false,
                    })
                    .child(rows)
                    .scroll_bar_styled(
                        ScrollBarStyle::native(c.border_strong, c.subtle_fg).inside(),
                    ),
            ),
    )
}

/// Columns, a card's whole-pixel width, and how many leading columns are
/// one pixel wider so the row ends where the box does. A whole card,
/// buttons included, must fit the box's height, so a wide but short Gallery
/// shrinks its cards instead of cutting them.
fn grid(avail_w: f32, avail_h: f32) -> (usize, f32, usize) {
    let width = (avail_w - BAR_RESERVE).max(1.0);
    let columns = (((width + CARD_GAP) / (MIN_CARD_W + CARD_GAP)).floor() as usize).max(1);
    let fit_height = ((avail_h - 108.0) * 16.0 / 9.0).max(120.0);
    let free = width - CARD_GAP * (columns - 1) as f32;
    let share = free / columns as f32;
    let card_w = share.min(MAX_CARD_W).min(fit_height).floor();
    // Only cards that fill the row have pixels left over to share.
    let wider = if card_w == share.floor() {
        (free - card_w * columns as f32).floor() as usize
    } else {
        0
    };
    (columns, card_w, wider)
}

/// The `at`th shown range and its source, while both still exist.
fn range<'a>(
    ui: &'a Ui,
    refs: &[(usize, usize)],
    at: usize,
    scenes: bool,
) -> Option<(&'a BinAsset, &'a SweepCandidate)> {
    let &(asset, clip) = refs.get(at)?;
    let asset = ui.editor.assets.get(asset)?;
    let clips = if scenes {
        &asset.sweep.scene_clips
    } else {
        &asset.sweep.candidates
    };
    Some((asset, clips.get(clip)?))
}

fn active_filters(filters: &GalleryFilters) -> usize {
    [
        filters.category.is_some(),
        filters.quality != GalleryQuality::All,
        filters.duration != GalleryDuration::All,
    ]
    .into_iter()
    .filter(|active| *active)
    .count()
}

fn reset(ui: &mut Ui) {
    ui.editor.curator.filters = GalleryFilters::default();
}

fn toggle(ui: &mut Ui, which: Popup) {
    ui.gallery.popup = if ui.gallery.popup == Some(which) {
        None
    } else {
        Some(which)
    };
}

/// The Clips filters: type, quality and length lists, then Reset.
fn filter_controls(filters: &GalleryFilters, popup: Option<Popup>, enabled: bool) -> Vec<El> {
    vec![
        select(
            "gallery-category",
            category_label(filters.category.as_deref().unwrap_or("all")),
            "Filter by content type",
            Popup::Category,
            popup == Some(Popup::Category),
            enabled,
        )
        .min_w(110.0),
        select(
            "gallery-quality",
            filters.quality.label(),
            "Filter by quality, independently of type",
            Popup::Quality,
            popup == Some(Popup::Quality),
            enabled,
        )
        .min_w(120.0),
        select(
            "gallery-duration",
            filters.duration.label(),
            "Find shorter inserts or longer usable takes",
            Popup::Duration,
            popup == Some(Popup::Duration),
            enabled,
        )
        .min_w(115.0),
        theme::button(
            "gallery-reset",
            "Reset",
            Variant::Ghost,
            "Clear clip filters and restore Best first; keep the selected source",
            enabled,
        )
        .on_press(reset),
    ]
}

/// A closed list: its choice and a down arrow. The caller sets its width.
fn select(
    key: &'static str,
    choice: &str,
    help: &str,
    which: Popup,
    open: bool,
    enabled: bool,
) -> El {
    // The field fill as it lands on the stage.
    let field = theme::select(key, choice, help, theme::hex("#131315"), open, enabled);
    if enabled {
        field.on_press(move |ui: &mut Ui| toggle(ui, which))
    } else {
        field
    }
}

/// One choice in an open list; picking it leaves `after` open.
fn option(
    key: String,
    label: &str,
    help: &str,
    current: bool,
    after: Option<Popup>,
    pick: impl Fn(&mut Ui) + 'static,
) -> El {
    theme::option(key, label, help, current).on_press(move |ui: &mut Ui| {
        pick(ui);
        ui.gallery.popup = after;
    })
}

fn slug(label: &str) -> String {
    label.to_lowercase().replace(' ', "-")
}

/// The open list, hung under its field. A press outside closes it.
pub fn overlays(ui: &Ui, cx: &Cx<Ui>, overlays: &mut Vec<El>) {
    let editor = &ui.editor;
    let Some(popup) = ui.gallery.popup.filter(|_| editor.curator.open) else {
        return;
    };
    let c = c();
    let filters = &editor.curator.filters;
    let narrow = compact(size(cx)[0]);
    // In the narrow layout a filter list replaces the Filters menu and
    // gives way to it again, so several filters can be set in one visit.
    let after = narrow.then_some(Popup::Filters);
    let (anchor, options): (&str, Vec<El>) = match popup {
        Popup::Source => {
            let current = editor.selected_asset.as_deref();
            let all = option(
                "gallery-option-all-sources".to_owned(),
                "All sources",
                "Browse ranges from every imported source",
                current.is_none(),
                None,
                |ui: &mut Ui| ui.editor.selected_asset = None,
            );
            let assets = editor.assets.iter().enumerate().map(|(index, asset)| {
                let path = asset.info.path.clone();
                option(
                    format!("gallery-option-source-{index}"),
                    &asset.info.name,
                    &asset.info.path,
                    current == Some(path.as_str()),
                    None,
                    move |ui: &mut Ui| ui.editor.selected_asset = Some(path.clone()),
                )
            });
            (
                "gallery-source",
                std::iter::once(all).chain(assets).collect(),
            )
        }
        Popup::Sort => (
            "gallery-sort",
            [
                GallerySort::Score,
                GallerySort::Source,
                GallerySort::Shortest,
                GallerySort::Longest,
            ]
            .into_iter()
            .map(|sort| {
                option(
                    format!("gallery-option-{}", slug(sort.label())),
                    sort.label(),
                    "Add shown uses this same order",
                    filters.sort == sort,
                    None,
                    move |ui: &mut Ui| ui.editor.curator.filters.sort = sort,
                )
            })
            .collect(),
        ),
        Popup::Category => {
            let all = option(
                "gallery-option-all-types".to_owned(),
                "All types",
                "Show every content category",
                filters.category.is_none(),
                after,
                |ui: &mut Ui| ui.editor.curator.filters.category = None,
            );
            let categories = CURATOR_CATEGORIES.iter().map(|&category| {
                option(
                    format!("gallery-option-{category}"),
                    category_label(category),
                    "Match this category as well as the other active filters",
                    filters.category.as_deref() == Some(category),
                    after,
                    move |ui: &mut Ui| {
                        ui.editor.curator.filters.category = Some(category.to_owned());
                    },
                )
            });
            (
                if narrow {
                    "gallery-filters"
                } else {
                    "gallery-category"
                },
                std::iter::once(all).chain(categories).collect(),
            )
        }
        Popup::Quality => (
            if narrow {
                "gallery-filters"
            } else {
                "gallery-quality"
            },
            editor
                .gallery_quality_choices()
                .iter()
                .map(|&quality| {
                    option(
                        format!("gallery-option-{}", slug(quality.label())),
                        quality.label(),
                        match quality {
                            GalleryQuality::All => "Show ranges of every quality",
                            GalleryQuality::Prime => {
                                "The best fifth of each file, scored 75 or more"
                            }
                            GalleryQuality::Reviewed => "Ranges an earlier model review accepted",
                            GalleryQuality::NeedsReview => "Ranges an earlier model review doubted",
                            GalleryQuality::Local => "Ranges no model reviewed",
                        },
                        filters.quality == quality,
                        after,
                        move |ui: &mut Ui| ui.editor.curator.filters.quality = quality,
                    )
                })
                .collect(),
        ),
        Popup::Duration => (
            if narrow {
                "gallery-filters"
            } else {
                "gallery-duration"
            },
            [
                GalleryDuration::All,
                GalleryDuration::UnderTwo,
                GalleryDuration::TwoToEight,
                GalleryDuration::EightToThirty,
                GalleryDuration::ThirtyPlus,
            ]
            .into_iter()
            .map(|duration| {
                option(
                    format!("gallery-option-{}", slug(duration.label())),
                    duration.label(),
                    "Filter by usable range length; each upper bound is exclusive",
                    filters.duration == duration,
                    after,
                    move |ui: &mut Ui| ui.editor.curator.filters.duration = duration,
                )
            })
            .collect(),
        ),
        Popup::Filters => (
            "gallery-filters",
            filter_controls(filters, None, true)
                .into_iter()
                .map(|control| control.w(180.0))
                .collect(),
        ),
    };
    let close = |ui: &mut Ui| ui.gallery.popup = None;
    overlays.push(rune_kit::scrim("gallery-popup-scrim", close));
    // As wide as its field, wider only when a choice's name needs it.
    let width = cx.layout_size(anchor).map_or(0.0, |size| size[0]);
    let list = col().gap(6.0);
    let list = if options.len() > LIST_ROWS {
        col()
            .key("gallery-popup-scroll")
            .h(LIST_ROWS as f32 * OPTION_PITCH - 6.0)
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .child(list.children(options))
            .scroll_bar_styled(ScrollBarStyle::native(c.border_strong, c.subtle_fg).inside())
    } else {
        list.children(options)
    };
    overlays.push(
        theme::popup("gallery-popup")
            .min_w(width)
            .child(list)
            .anchored(anchor, Side::Bottom, Align::Start, 0.0)
            .modal(true, close),
    );
}

/// A layer over a card's `w`×`h` picture, rounded where the card is.
fn over_picture(w: f32, h: f32) -> El {
    el().absolute(0.0, 0.0)
        .size(w, h)
        .corners([theme::R_LG, theme::R_LG, 0.0, 0.0])
        .inert()
}

/// The range's own stretch of its file's waveform, for sound without a
/// picture.
fn range_peaks(w: f32, h: f32, asset: &BinAsset, clip: &SweepCandidate) -> Shape {
    let mut shape = Shape::new().view_box(w, h);
    // CLIP_AUDIO × 2.6.
    let color = theme::hex("#72DA9C");
    let mid = h / 2.0;
    let mut x = 5.0;
    while x < w - 5.0 {
        let time = clip.source_in + f64::from(x / w) * clip.duration;
        let index = (time * openreel_media::WAVEFORM_PEAKS_PER_SEC as f64) as usize;
        let amp = asset.peaks.get(index).copied().unwrap_or(0.0) * h * 0.38;
        shape = shape.line([x, mid - amp], [x, mid + amp], 1.5, LineCap::Butt, color);
        x += 3.0;
    }
    shape
}

/// One range: its middle picture or waveform with the length on it, what
/// it is and where it comes from, then Add and Dismiss.
fn card(ui: &Ui, asset: &BinAsset, clip: &SweepCandidate, scenes: bool, w: f32, h: f32) -> El {
    let c = c();
    let id = &clip.id;
    let picture_h = h - CARD_TEXT_H;
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
    let mut picture = el()
        .key(format!("gallery-picture:{id}"))
        .size(w, picture_h)
        .corners([theme::R_LG, theme::R_LG, 0.0, 0.0])
        .fill(c.input);
    if clip.has_video {
        let wanted = ui
            .gallery
            .thumbs
            .borrow_mut()
            .picture(&asset.info, clip.source_in + clip.duration / 2.0);
        picture = match wanted {
            Ok(Some(pixels)) => {
                picture.child(over_picture(w, picture_h).pixels_fit(&pixels, SurfaceFit::Fill))
            }
            Ok(None) => picture.child(
                spinner(18.0, ui.turn())
                    .absolute((w - 18.0) / 2.0, (picture_h - 18.0) / 2.0)
                    .inert(),
            ),
            Err(error) => {
                details.push_str(&format!("\n{error}"));
                picture.child(
                    row()
                        .absolute(0.0, 0.0)
                        .size(w, picture_h)
                        .center()
                        .inert()
                        .child(
                            text("Preview unavailable")
                                .font(theme::sized(12.0))
                                .color(c.muted_fg)
                                .single_line(),
                        ),
                )
            }
        };
    } else {
        picture = picture.child(
            over_picture(w, picture_h)
                // CLIP_AUDIO at 55% over the field fill.
                .fill(theme::hex("#253B30"))
                .shape(range_peaks(w, picture_h, asset, clip)),
        );
    }
    // The length sits on the picture, like a player's time chip.
    picture = picture.child(
        row()
            .pin(None, None, Some(6.0), Some(6.0))
            .px(4.0)
            .pt(2.0)
            .pb(1.0)
            .radius(4.0)
            .fill(Color::BLACK.alpha(180.0 / 255.0))
            .inert()
            .child(
                text(format!("{:.2}s", clip.duration))
                    .font(theme::mono(10.5))
                    .color(c.foreground)
                    .single_line(),
            ),
    );

    let label = if clip.review.status == ReviewStatus::NeedsReview {
        "Needs review"
    } else {
        category_label(&clip.category)
    };
    let heading = row()
        .w_full()
        .h(28.0)
        .px(10.0)
        .gap(GAP)
        .center_y()
        .child(
            text(label.to_owned())
                .font(theme::sized(12.0))
                .color(c.foreground)
                .single_line()
                .truncate()
                .help(details.clone()),
        )
        .child(spacer())
        // The digits sit on the label's baseline.
        .child(
            el().pt(2.0).child(
                text(format!("{:.0}", clip.score))
                    .font(theme::mono(11.0))
                    .color(c.muted_fg)
                    .single_line()
                    .help(format!(
                        "Score {:.0} · source {:.3} to {:.3} seconds",
                        clip.score,
                        clip.source_in,
                        clip.end()
                    )),
            ),
        );
    // Where the range comes from, so mixed-source grids read without
    // hovering. A long file name gives way; the times, which tell one
    // file's cards apart, stay whole. A line a fraction of a pixel too long
    // takes one pixel of the padding instead of losing three letters.
    let origin = row()
        .w_full()
        .h(15.0)
        .pl(10.0)
        .pr(9.0)
        .center_y()
        .child(
            theme::caption(clip.asset_name.clone())
                .single_line()
                .truncate_middle()
                .help(details.clone()),
        )
        .child(
            theme::caption(format!(
                " · {}–{}",
                clock(clip.source_in),
                clock(clip.end())
            ))
            .single_line()
            .help(details.clone()),
        );
    let clashes = ui.editor.clashes_with_format(&asset.info);
    let add_id = id.clone();
    let mut buttons = row().w_full().h(28.0).px(10.0).gap(4.0).center_y().child(
        theme::button(
            &format!("gallery-add:{id}"),
            "Add",
            Variant::Secondary,
            if clashes {
                "This video's orientation doesn't match the project format"
            } else {
                "Append this exact source range to the timeline"
            },
            !clashes,
        )
        .on_press(move |ui: &mut Ui| ui.editor.add_sweep_candidate(&add_id)),
    );
    if !scenes {
        let dismiss_id = id.clone();
        buttons = buttons.child(
            theme::button(
                &format!("gallery-dismiss:{id}"),
                "Dismiss",
                Variant::Ghost,
                "Hide this suggestion; keep its source in All scenes",
                true,
            )
            .on_press(move |ui: &mut Ui| ui.editor.dismiss_sweep_candidate(&dismiss_id)),
        );
    }

    let outline = |color: Color| {
        el().absolute(0.0, 0.0)
            .size(w, h)
            .radius(theme::R_LG)
            .border(1.0, color)
            .inert()
    };
    el().key(format!("gallery-card:{id}"))
        .size(w, h)
        .radius(theme::R_LG)
        .fill(c.card)
        .hover_group()
        // The picture runs to the card's edge, over its outline.
        .child(outline(c.border))
        .child(outline(c.border_strong).group_opacity(0.0, 1.0))
        .child(
            col()
                .absolute(0.0, 0.0)
                .size(w, h)
                .child(picture.help(details))
                .child(el().h(6.0))
                .child(heading)
                .child(el().h(6.0))
                .child(origin)
                .child(el().h(5.0))
                .child(buttons),
        )
}

#[cfg(test)]
mod tests {
    use eddie_core::editor::media::AssetState;
    use eddie_core::editor::sweep::{ClipReview, SweepData};
    use rune::{Env, Key, KeyEvent, Mods, Runtime};

    use super::*;
    use crate::app;

    const FRAME: f64 = 1.0 / 60.0;

    /// A range of `a.wav`. Sound only, so its card draws a waveform and
    /// asks no decoder for a picture.
    fn range(id: &str, category: &str, score: f32, source_in: f64) -> SweepCandidate {
        SweepCandidate {
            id: id.to_owned(),
            asset_path: "/media/a.wav".to_owned(),
            asset_name: "a.wav".to_owned(),
            title: String::new(),
            source_in,
            duration: 2.5,
            score,
            reason: "steady".to_owned(),
            stability: 0.0,
            audio_energy: 0.0,
            audio_peak: 0.0,
            category: category.to_owned(),
            tags: vec!["prime".to_owned()],
            motion_score: 0.0,
            audio_score: 0.0,
            music_bed: false,
            visual: None,
            has_video: false,
            transcript: Vec::new(),
            review: ClipReview::default(),
            dismissed: false,
        }
    }

    /// The Gallery, open on one curated file with these suggestions; the
    /// file's scenes are the same ranges under `scene-` ids.
    fn gallery(candidates: Vec<SweepCandidate>, size: (f32, f32)) -> Runtime<Ui> {
        let mut ui = Ui::headless();
        let scene_clips = candidates
            .iter()
            .map(|clip| SweepCandidate {
                id: format!("scene-{}", clip.id),
                ..clip.clone()
            })
            .collect();
        ui.editor.assets.push(BinAsset {
            info: openreel_media::MediaInfo {
                path: "/media/a.wav".to_owned(),
                name: "a.wav".to_owned(),
                duration: 600.0,
                width: 0,
                height: 0,
                fps: 0.0,
                has_video: false,
                has_audio: true,
            },
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumbs: Vec::new(),
            sweep: SweepData {
                status: SweepStatus::Ready,
                candidates,
                scene_clips,
                ..SweepData::default()
            },
        });
        ui.editor.open_curator_gallery();
        let mut rt = Runtime::new(ui, app::view, size, Env::new(true, 1.0));
        rt.poll = Some(app::poll);
        rt.on_key = Some(app::on_key);
        // The grid sizes its cards from the box the first frames give it.
        for _ in 0..4 {
            rt.frame(FRAME);
        }
        rt
    }

    fn three() -> Vec<SweepCandidate> {
        vec![
            range("talk", "talking", 80.0, 10.0),
            range("act", "action", 60.0, 20.0),
            range("roll", "broll", 40.0, 30.0),
        ]
    }

    fn click(rt: &mut Runtime<Ui>, key: &str) {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("{key} is not shown"));
        rt.pointer_move(x, y);
        rt.frame(FRAME);
        rt.pointer_down(x, y);
        rt.frame(FRAME);
        rt.pointer_up(x, y);
        rt.frame(FRAME);
        rt.frame(FRAME);
    }

    fn shown(rt: &Runtime<Ui>, id: &str) -> bool {
        rt.find(&format!("gallery-card:{id}")).is_some()
    }

    #[test]
    fn a_row_of_cards_ends_where_the_grid_does() {
        // 1,256 px beside the scroll bar: six cards and five gaps, 4 px over.
        assert_eq!(grid(1256.0 + BAR_RESERVE, 2000.0), (6, 197.0, 4));
        // A card at its widest shares nothing.
        assert_eq!(grid(300.0 + BAR_RESERVE, 2000.0), (1, MAX_CARD_W, 0));
    }

    #[test]
    fn a_filter_narrows_the_cards_and_reset_brings_them_back() {
        let mut rt = gallery(three(), (1400.0, 1200.0));
        assert!(shown(&rt, "talk") && shown(&rt, "act") && shown(&rt, "roll"));

        click(&mut rt, "gallery-category");
        assert_eq!(rt.state.gallery.popup, Some(Popup::Category));
        click(&mut rt, "gallery-option-action");
        assert_eq!(rt.state.gallery.popup, None, "choosing closes the list");
        assert!(shown(&rt, "act"));
        assert!(!shown(&rt, "talk") && !shown(&rt, "roll"));

        click(&mut rt, "gallery-reset");
        assert!(shown(&rt, "talk") && shown(&rt, "act") && shown(&rt, "roll"));
    }

    #[test]
    fn search_filters_as_typed_and_reset_empties_the_box() {
        let mut rt = gallery(three(), (1400.0, 1200.0));
        click(&mut rt, "gallery-search");
        assert!(rt.state.typing(), "typing must not fire editor shortcuts");
        rt.paste("b-roll");
        rt.frame(FRAME);
        rt.frame(FRAME);
        assert_eq!(rt.state.editor.curator.filters.search, "b-roll");
        assert!(shown(&rt, "roll"));
        assert!(!shown(&rt, "talk") && !shown(&rt, "act"));

        // "No matching clips." offers the way back.
        rt.paste(" nothing");
        rt.frame(FRAME);
        rt.frame(FRAME);
        assert!(rt.find("gallery-empty").is_some());
        click(&mut rt, "gallery-reset-filters");
        assert_eq!(rt.state.gallery.search.text, "");
        assert!(shown(&rt, "talk") && shown(&rt, "act") && shown(&rt, "roll"));
    }

    #[test]
    fn add_appends_the_exact_range_and_dismiss_keeps_its_scene() {
        let mut rt = gallery(three(), (1400.0, 1200.0));
        click(&mut rt, "gallery-add:act");
        let clips = &rt.state.editor.timeline.clips;
        assert_eq!(clips.len(), 1);
        assert_eq!((clips[0].source_in, clips[0].duration), (20.0, 2.5));
        assert!(shown(&rt, "act"), "adding a range does not use it up");

        click(&mut rt, "gallery-dismiss:act");
        assert!(!shown(&rt, "act"));
        assert!(shown(&rt, "talk") && shown(&rt, "roll"));

        // All scenes keeps the source whole and offers nothing to dismiss.
        click(&mut rt, "gallery-view-scenes");
        assert!(shown(&rt, "scene-act"));
        assert!(rt.find("gallery-add:scene-act").is_some());
        assert!(rt.find("gallery-dismiss:scene-act").is_none());
        click(&mut rt, "gallery-add-all");
        assert_eq!(rt.state.editor.timeline.clips.len(), 4);
    }

    #[test]
    fn a_long_list_builds_only_the_cards_in_view() {
        let ranges = (0..400)
            .map(|index| range(&format!("r{index}"), "action", 50.0, f64::from(index)))
            .collect();
        let rt = gallery(ranges, (1400.0, 1200.0));
        // Equal scores keep source order.
        assert!(shown(&rt, "r0"));
        assert!(!shown(&rt, "r399"));
    }

    #[test]
    fn a_narrow_gallery_sets_its_filters_from_one_menu() {
        let mut rt = gallery(three(), (1100.0, 900.0));
        assert!(rt.find("gallery-category").is_none());
        click(&mut rt, "gallery-filters");
        assert_eq!(rt.state.gallery.popup, Some(Popup::Filters));
        click(&mut rt, "gallery-category");
        click(&mut rt, "gallery-option-talking");
        assert_eq!(
            rt.state.gallery.popup,
            Some(Popup::Filters),
            "the menu comes back for the next filter"
        );
        assert_eq!(
            rt.state.editor.curator.filters.category.as_deref(),
            Some("talking")
        );
        assert!(shown(&rt, "talk") && !shown(&rt, "act"));
    }

    #[test]
    fn escape_closes_an_open_list_without_changing_the_filter() {
        let mut rt = gallery(three(), (1400.0, 1200.0));
        click(&mut rt, "gallery-category");
        assert_eq!(rt.state.gallery.popup, Some(Popup::Category));
        assert!(rt.find("gallery-option-talking").is_some());
        rt.key(KeyEvent {
            key: Key::Escape,
            mods: Mods::default(),
        });
        rt.frame(FRAME);
        rt.frame(FRAME);
        assert_eq!(rt.state.gallery.popup, None);
        assert!(rt.find("gallery-option-talking").is_none());
        assert_eq!(rt.state.editor.curator.filters.category, None);
        assert!(shown(&rt, "talk") && shown(&rt, "act") && shown(&rt, "roll"));
    }

    #[test]
    fn every_gallery_control_explains_itself() {
        let mut rt = gallery(three(), (1400.0, 1200.0));
        rt.help_delay = 0.0;
        for key in [
            "gallery-view-clips",
            "gallery-view-scenes",
            "gallery-source",
            "gallery-search",
            "gallery-sort",
            "gallery-category",
            "gallery-quality",
            "gallery-duration",
            "gallery-reset",
            "gallery-count",
            "gallery-mute",
            "gallery-add-all",
            "gallery-picture:talk",
            "gallery-add:talk",
            "gallery-dismiss:talk",
        ] {
            let (x, y) = rt.find(key).unwrap_or_else(|| panic!("{key} is not shown"));
            // Rest the pointer on the control, coming from outside the window.
            rt.pointer_leave();
            rt.frame(FRAME);
            assert!(rt.help_tooltip().is_none());
            rt.pointer_move(x, y);
            rt.frame(FRAME);
            rt.frame(FRAME);
            let words = rt.help_tooltip().map(|(words, _)| words.to_owned());
            assert!(
                words.as_deref().is_some_and(|words| !words.is_empty()),
                "{key} has no tooltip"
            );
        }
        // An open list explains its choices too.
        click(&mut rt, "gallery-sort");
        let (x, y) = rt
            .find("gallery-option-source-order")
            .expect("the sort list is open");
        rt.pointer_move(x, y);
        rt.frame(FRAME);
        rt.frame(FRAME);
        assert_eq!(
            rt.help_tooltip()
                .map(|(words, _)| words.to_owned())
                .as_deref(),
            Some("Add shown uses this same order")
        );
    }
}

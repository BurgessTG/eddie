//! The list hanging open under one of the Gallery's fields.

use eddie_core::Editor;
use eddie_core::editor::gallery::{
    GalleryDuration, GalleryFilters, GalleryQuality, GallerySort, category_label,
};
use eddie_core::editor::sweep::CURATOR_CATEGORIES;
use rune::{Align, Cx, El, ScrollSpec, Side, col};
use rune_kit::{ScrollBar, ScrollBarStyle};

use super::toolbar::filter_controls;
use super::{Popup, compact, size};
use crate::app::Ui;
use crate::theme::{self, c};

/// A list longer than this scrolls.
const LIST_ROWS: usize = 10;
const OPTION_PITCH: f32 = 34.0;

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
    let filter = |wide: &'static str| if narrow { "gallery-filters" } else { wide };
    let (anchor, options) = match popup {
        Popup::Source => ("gallery-source", sources(editor)),
        Popup::Sort => ("gallery-sort", sorts(filters)),
        Popup::Category => (filter("gallery-category"), categories(filters, after)),
        Popup::Quality => (filter("gallery-quality"), qualities(editor, after)),
        Popup::Duration => (filter("gallery-duration"), durations(filters, after)),
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

fn sources(editor: &Editor) -> Vec<El> {
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
    std::iter::once(all).chain(assets).collect()
}

fn sorts(filters: &GalleryFilters) -> Vec<El> {
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
    .collect()
}

/// The three filter lists leave `after` open once a choice is made.
fn categories(filters: &GalleryFilters, after: Option<Popup>) -> Vec<El> {
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
    std::iter::once(all).chain(categories).collect()
}

fn qualities(editor: &Editor, after: Option<Popup>) -> Vec<El> {
    editor
        .gallery_quality_choices()
        .iter()
        .map(|&quality| {
            option(
                format!("gallery-option-{}", slug(quality.label())),
                quality.label(),
                match quality {
                    GalleryQuality::All => "Show ranges of every quality",
                    GalleryQuality::Prime => "The best fifth of each file, scored 75 or more",
                    GalleryQuality::Reviewed => "Ranges an earlier model review accepted",
                    GalleryQuality::NeedsReview => "Ranges an earlier model review doubted",
                    GalleryQuality::Local => "Ranges no model reviewed",
                },
                editor.curator.filters.quality == quality,
                after,
                move |ui: &mut Ui| ui.editor.curator.filters.quality = quality,
            )
        })
        .collect()
}

fn durations(filters: &GalleryFilters, after: Option<Popup>) -> Vec<El> {
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
    .collect()
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

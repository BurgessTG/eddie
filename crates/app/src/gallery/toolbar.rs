//! The two rows above the Gallery's cards: what to browse, then how to
//! narrow it and what to do with what is shown.

use eddie_core::Editor;
use eddie_core::editor::gallery::{
    GalleryDuration, GalleryFilters, GalleryQuality, GalleryView, category_label,
};
use rune::{El, el, row, spacer, text};

use super::{FIELD_H, GAP, Popup, reset, toggle};
use crate::app::Ui;
use crate::media::spinner;
use crate::theme::{self, Variant, c};

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

/// Row one picks what to browse: view and source, then search and order
/// beside them when they fit, or on their own row when not.
pub(super) fn browse(ui: &Ui, width: f32) -> Vec<El> {
    let editor = &ui.editor;
    let scenes = editor.curator.view == GalleryView::Scenes;
    let popup = ui.gallery.popup;
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
    let search = search(ui, scenes);
    let sort = select(
        "gallery-sort",
        if scenes {
            "Source order"
        } else {
            editor.curator.filters.sort.label()
        },
        "Sort the visible clips; ties keep source order",
        Popup::Sort,
        popup == Some(Popup::Sort),
        !scenes,
    )
    .min_w(SORT_W);
    let inline = width - SWITCH_W - source_w - 2.0 * GAP >= SEARCH_MIN + GAP + SORT_W;
    let top = row()
        .w_full()
        .h(32.0)
        .gap(GAP)
        .center_y()
        .child(view_switch(scenes))
        .child(source);
    if inline {
        return vec![top.child(search).child(sort)];
    }
    vec![
        top.child(spacer()),
        // The same pitch as the two rows of the wide layout.
        el().h(15.0),
        row()
            .w_full()
            .h(FIELD_H)
            .gap(GAP)
            .center_y()
            .child(search)
            .child(sort),
    ]
}

/// Clips, or every scene of the source.
fn view_switch(scenes: bool) -> El {
    theme::segmented(
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
    )
}

fn search(ui: &Ui, scenes: bool) -> El {
    let c = c();
    rune::text_input(&ui.gallery.search, theme::BODY)
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
        .on_edit(|ui: &mut Ui| &mut ui.gallery.search)
}

/// Row two narrows and acts: filters on the left; the count, the music
/// policy and the one primary action on the right. `shown` ranges match,
/// and `running` says the browsed source is still being curated.
pub(super) fn actions(ui: &Ui, narrow: bool, shown: usize, running: bool) -> El {
    let editor = &ui.editor;
    let scenes = editor.curator.view == GalleryView::Scenes;
    let filters = &editor.curator.filters;
    let popup = ui.gallery.popup;
    // A spinner only: a text label here ran into Reset.
    let curating = running.then(|| {
        spinner(16.0, ui.turn())
            .key("gallery-curating")
            .help("Curating; results appear when it finishes")
    });
    if narrow {
        // Too narrow to split the row; wrap in reading order instead.
        row()
            .w_full()
            .flex_wrap()
            .gaps(GAP, GAP)
            .center_y()
            .child(filters_menu(filters, popup, scenes))
            .child(add_all(scenes))
            .child(mute(editor))
            .child(count(shown, scenes))
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
            .child(count(shown, scenes))
            .child(mute(editor))
            .child(add_all(scenes))
    }
}

fn count(shown: usize, scenes: bool) -> El {
    text(format!(
        "{} {}{}",
        shown,
        if scenes { "scene" } else { "clip" },
        if shown == 1 { "" } else { "s" }
    ))
    .key("gallery-count")
    .font(theme::sized(12.0))
    .color(c().muted_fg)
    .single_line()
    .help("Matching ranges only; filtering does not remove source material")
}

fn add_all(scenes: bool) -> El {
    theme::button(
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
    })
}

fn mute(editor: &Editor) -> El {
    theme::checkbox(
        "gallery-mute",
        "Mute music beds",
        "Add ranges with baked-in music muted; turn off to keep their original score",
        editor.mute_music_beds,
    )
    .on_press(|ui: &mut Ui| ui.editor.mute_music_beds = !ui.editor.mute_music_beds)
}

/// The narrow layout's one menu for the three filter lists.
fn filters_menu(filters: &GalleryFilters, popup: Option<Popup>, scenes: bool) -> El {
    let active = active_filters(filters);
    let label = if active == 0 {
        "Filters".to_owned()
    } else {
        format!("Filters ({active})")
    };
    theme::button(
        "gallery-filters",
        &label,
        Variant::Secondary,
        "Combine content type, quality and duration filters",
        !scenes,
    )
    .role("combobox")
    .expanded(popup.is_some_and(|popup| popup != Popup::Source && popup != Popup::Sort))
    .on_press(|ui: &mut Ui| toggle(ui, Popup::Filters))
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

/// The Clips filters: type, quality and length lists, then Reset.
pub(super) fn filter_controls(
    filters: &GalleryFilters,
    popup: Option<Popup>,
    enabled: bool,
) -> Vec<El> {
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

//! The Gallery: curated ranges as a searchable grid of cards. Browsing never
//! changes the source inventory or the timeline.
//!
//! `toolbar` is the two rows of controls above the cards, `grid` the
//! scrolling rows, `card` one range, `popups` the list hanging open under a
//! field and `thumbs` the cards' pictures.

mod card;
mod grid;
mod popups;
mod thumbs;
mod toolbar;

use std::cell::{Cell, RefCell};

use eddie_core::Editor;
use eddie_core::editor::gallery::{GalleryFilters, GalleryView};
use eddie_core::editor::sweep::SweepStatus;
use rune::{Cx, El, TextEdit, col, el, row, text};

pub use popups::overlays;
use thumbs::GalleryThumbnails;

use crate::app::Ui;
use crate::theme::{self, Variant, c};

const FIELD_H: f32 = 28.0;
const GAP: f32 = 8.0;
/// Narrower than this, the three filter lists fold into one Filters menu
/// and the second row wraps.
const COMPACT_W: f32 = 720.0;

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
    let editor = &ui.editor;
    let [width, height] = size(cx);
    let scenes = editor.curator.view == GalleryView::Scenes;
    let narrow = compact(width);
    ui.gallery.compact.set(narrow);
    let running = any_source(editor, SweepStatus::Running);
    let refs = editor.curator_candidate_refs(scenes);
    let content = col()
        .key("gallery")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        // Under the 16 the panel leaves below its tabs.
        .child(el().h(1.0))
        .children(toolbar::browse(ui, width))
        .child(el().h(if narrow { 14.0 } else { 15.0 }))
        .child(toolbar::actions(ui, narrow, refs.len(), running));
    if refs.is_empty() {
        return content.child(el().h(24.0)).children(empty(editor, running));
    }
    content
        .child(el().h(12.0))
        .child(grid::view(ui, cx, refs, [width, height]))
}

/// Whether a file of the browsed source (any file, with All sources) is in
/// `status`.
fn any_source(editor: &Editor, status: SweepStatus) -> bool {
    editor.assets.iter().any(|asset| {
        asset.sweep.status == status
            && editor
                .selected_asset
                .as_deref()
                .is_none_or(|path| path == asset.info.path)
    })
}

/// What a Gallery without cards says, and the way back when filters
/// emptied it.
fn empty(editor: &Editor, running: bool) -> Vec<El> {
    let scenes = editor.curator.view == GalleryView::Scenes;
    let ready = any_source(editor, SweepStatus::Ready);
    let mut lines = vec![
        text(if running {
            "Results appear when curation finishes."
        } else if !ready {
            "Curate a video to see its clips here."
        } else {
            "No matching clips."
        })
        .key("gallery-empty")
        .font(theme::BODY)
        .color(c().muted_fg),
    ];
    if !scenes && editor.curator.filters != GalleryFilters::default() {
        lines.push(el().h(6.0));
        lines.push(
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
    lines
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

#[cfg(test)]
mod tests;

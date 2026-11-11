//! The Gallery's cards as a scrolling grid that builds only the rows in
//! view.

use std::borrow::Cow;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use eddie_core::editor::gallery::GalleryView;
use eddie_core::editor::media::BinAsset;
use eddie_core::editor::sweep::SweepCandidate;
use rune::{Cx, El, GrowthSpec, ScrollSpec, col, row};
use rune_kit::{ScrollBar, ScrollBarStyle};

use super::card::{CARD_TEXT_H, card};
use crate::app::Ui;
use crate::theme::c;

const CARD_GAP: f32 = 14.0;
const MIN_CARD_W: f32 = 176.0;
pub(super) const MAX_CARD_W: f32 = 280.0;
/// Breathing room between card rows.
const GAP_BELOW: f32 = 8.0;
/// Room kept clear of cards for the scroll bar.
pub(super) const BAR_RESERVE: f32 = 12.0;

/// The shown ranges (`refs`: asset and range indexes) as rows of cards.
/// `[width, height]` is the whole Gallery's box.
pub(super) fn view(
    ui: &Ui,
    cx: &Cx<Ui>,
    refs: Vec<(usize, usize)>,
    [width, height]: [f32; 2],
) -> El {
    let editor = &ui.editor;
    let scenes = editor.curator.view == GalleryView::Scenes;
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
    (
        editor.curator.view,
        &editor.curator.filters,
        &editor.selected_asset,
    )
        .hash(&mut hasher);
    let c = c();
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
                .scroll_bar_styled(ScrollBarStyle::native(c.border_strong, c.subtle_fg).inside()),
        )
}

/// Columns, a card's whole-pixel width, and how many leading columns are
/// one pixel wider so the row ends where the box does. A whole card,
/// buttons included, must fit the box's height, so a wide but short Gallery
/// shrinks its cards instead of cutting them.
pub(super) fn grid(avail_w: f32, avail_h: f32) -> (usize, f32, usize) {
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

//! The Curator card under the bin grid: what curation found in the
//! selected file, and the buttons that start it or open the gallery.

use eddie_core::editor::media::{AssetState, BinAsset, count};
use eddie_core::editor::sweep::{ReviewStatus, SweepStatus};
use rune::{El, col, el, row, spacer, text};

use super::tile::preview;
use crate::app::Ui;
use crate::icons;
use crate::theme::{self, Variant, c};

/// What the Curator card says about the selected asset.
fn summary(ui: &Ui, asset: &BinAsset) -> String {
    let sweep = &asset.sweep;
    let needs_review = sweep
        .candidates
        .iter()
        .filter(|clip| clip.review.status == ReviewStatus::NeedsReview)
        .count();
    match sweep.status {
        SweepStatus::NotStarted if asset.state != AssetState::Ready => "Importing…".to_owned(),
        SweepStatus::NotStarted => "Not curated yet".to_owned(),
        SweepStatus::Running if ui.editor.sweep_queue.contains(&asset.info.path) => {
            "Queued".to_owned()
        }
        SweepStatus::Running => sweep.note.clone(),
        SweepStatus::Failed => "Analysis failed".to_owned(),
        _ if needs_review > 0 => format!(
            "{} · {needs_review} need review",
            count(sweep.candidates.len(), "clip")
        ),
        _ => format!(
            "{} · {}",
            count(sweep.candidates.len(), "clip"),
            count(sweep.scene_clips.len(), "scene")
        ),
    }
}

const GALLERY_HELP: &str = "Pause playback and search, filter or sort curated clips";

/// The Curate controls for the selected asset, pinned under the grid.
pub(super) fn card(ui: &Ui, width: f32) -> El {
    let editor = &ui.editor;
    let card = theme::card()
        .key("curator-card")
        .w(width)
        .gap(8.0)
        .child(title(ui));
    let selected = editor
        .selected_asset
        .as_deref()
        .and_then(|path| editor.asset(path));
    let Some(asset) = selected else {
        return card
            .child(theme::caption("Select a video, then press Curate."))
            .child(
                theme::button(
                    "curator-browse",
                    "Browse gallery",
                    Variant::Secondary,
                    GALLERY_HELP,
                    true,
                )
                .w_full()
                .on_press(|ui: &mut Ui| ui.editor.open_curator_gallery()),
            );
    };
    let card = card.child(source(ui, asset)).child(buttons(asset, width));
    let uncurated = editor
        .assets
        .iter()
        .filter(|asset| {
            asset.state == AssetState::Ready && asset.sweep.status == SweepStatus::NotStarted
        })
        .count();
    if uncurated > 1 || (uncurated == 1 && asset.sweep.status != SweepStatus::NotStarted) {
        return card.child(
            theme::button(
                "curator-all-new",
                &format!("Curate all new ({uncurated})"),
                Variant::Ghost,
                "Curate every imported video that has not been curated",
                true,
            )
            .w_full()
            .on_press(|ui: &mut Ui| ui.editor.curate_all_new()),
        );
    }
    card
}

/// "Curator", with a spinner while any file is being curated.
fn title(ui: &Ui) -> El {
    let running = ui
        .editor
        .assets
        .iter()
        .filter(|asset| asset.sweep.status == SweepStatus::Running)
        .count();
    let title = row()
        .h(28.0)
        .center_y()
        .child(theme::panel_title("Curator"))
        .child(spacer());
    if running == 0 {
        return title;
    }
    title.child(
        row()
            .key("curator-running")
            .help(format!("Curating {}", count(running, "video")))
            .child(
                el().size(13.0, 13.0)
                    .shape(icons::spinner(13.0, ui.turn(), c().foreground)),
            ),
    )
}

/// The selected source, so the card reads at a glance: its picture, its
/// name and what curation found.
fn source(ui: &Ui, asset: &BinAsset) -> El {
    let c = c();
    let path = &asset.info.path;
    let summary = summary(ui, asset);
    let note = if asset.sweep.note.is_empty() {
        summary.clone()
    } else {
        asset.sweep.note.clone()
    };
    let thumb = preview(
        el().size(56.0, 32.0).radius(theme::R_SM).fill(c.secondary),
        ui.thumbs(path).first(),
        asset,
        56.0,
        32.0,
        theme::R_SM,
    );
    row().h(33.0).gap(10.0).child(thumb).child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(2.0)
            .child(
                text(asset.info.name.clone())
                    .font(theme::sized(12.5))
                    .color(c.foreground)
                    .single_line()
                    .truncate()
                    .help(path.clone()),
            )
            .child(theme::caption(summary).single_line().truncate().help(note)),
    )
}

/// Curate, Re-curate or Retry for `asset` (nothing while it is being
/// curated), then Gallery.
fn buttons(asset: &BinAsset, width: f32) -> El {
    let inner = width - 26.0;
    let half = ((inner - 6.0) / 2.0).floor();
    let curate = match asset.sweep.status {
        SweepStatus::NotStarted => Some((
            "Curate",
            Variant::Primary,
            "Find every shot, transcribe speech and rank usable ranges, on this computer unless Speech uses a cloud provider",
        )),
        SweepStatus::Running => None,
        SweepStatus::Ready => Some((
            "Re-curate",
            Variant::Secondary,
            "Analyze again, for example after changing the speech provider",
        )),
        SweepStatus::Failed => Some((
            "Retry",
            Variant::Secondary,
            "Try analyzing this video again",
        )),
    };
    let mut buttons = row().h(30.0).gap(6.0).center_y();
    if let Some((label, variant, tooltip)) = curate {
        // Curating ignores assets still importing; say so instead of
        // taking a silent click.
        let ready = asset.state == AssetState::Ready;
        let button = theme::button(
            "curator-curate",
            label,
            variant,
            if ready {
                tooltip
            } else {
                "Available once import finishes"
            },
            ready,
        )
        .w(half);
        buttons = buttons.child(if ready {
            let path = asset.info.path.clone();
            button.on_press(move |ui: &mut Ui| ui.editor.curate_asset(path.clone()))
        } else {
            button
        });
    }
    buttons.child(
        theme::button(
            "curator-gallery",
            "Gallery",
            Variant::Secondary,
            GALLERY_HELP,
            true,
        )
        .grow(1.0)
        .on_press(|ui: &mut Ui| ui.editor.open_curator_gallery()),
    )
}

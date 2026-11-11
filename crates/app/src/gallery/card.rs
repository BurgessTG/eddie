//! One Gallery card: a range's picture or waveform, what it is and where it
//! comes from, then Add and Dismiss.

use eddie_core::editor::gallery::{category_label, clock};
use eddie_core::editor::media::BinAsset;
use eddie_core::editor::sweep::{ReviewStatus, SweepCandidate};
use rune::{Color, El, LineCap, Shape, SurfaceFit, col, el, row, spacer, text};

use super::GAP;
use crate::app::Ui;
use crate::media::spinner;
use crate::theme::{self, Variant, c};

/// Under a card's picture: its label, source line and buttons.
pub(super) const CARD_TEXT_H: f32 = 94.0;

/// One range: its middle picture or waveform with the length on it, what
/// it is and where it comes from, then Add and Dismiss.
pub(super) fn card(
    ui: &Ui,
    asset: &BinAsset,
    clip: &SweepCandidate,
    scenes: bool,
    w: f32,
    h: f32,
) -> El {
    let c = c();
    let id = &clip.id;
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
    let picture = picture(ui, asset, clip, w, h - CARD_TEXT_H, &mut details);
    let heading = heading(clip, &details);
    let origin = origin(clip, &details);
    let buttons = buttons(ui, asset, clip, scenes);

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

/// The range's middle picture, or its waveform when it is sound only, with
/// the length on it. A picture that cannot be made adds its reason to
/// `details`.
fn picture(
    ui: &Ui,
    asset: &BinAsset,
    clip: &SweepCandidate,
    w: f32,
    picture_h: f32,
    details: &mut String,
) -> El {
    let c = c();
    let id = &clip.id;
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
    picture.child(
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
    )
}

/// What the range is, and its score.
fn heading(clip: &SweepCandidate, details: &str) -> El {
    let c = c();
    let label = if clip.review.status == ReviewStatus::NeedsReview {
        "Needs review"
    } else {
        category_label(&clip.category)
    };
    row()
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
                .help(details.to_owned()),
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
        )
}

/// Where the range comes from, so mixed-source grids read without
/// hovering. A long file name gives way; the times, which tell one file's
/// cards apart, stay whole. A line a fraction of a pixel too long takes one
/// pixel of the padding instead of losing three letters.
fn origin(clip: &SweepCandidate, details: &str) -> El {
    row()
        .w_full()
        .h(15.0)
        .pl(10.0)
        .pr(9.0)
        .center_y()
        .child(
            theme::caption(clip.asset_name.clone())
                .single_line()
                .truncate_middle()
                .help(details.to_owned()),
        )
        .child(
            theme::caption(format!(
                " · {}–{}",
                clock(clip.source_in),
                clock(clip.end())
            ))
            .single_line()
            .help(details.to_owned()),
        )
}

/// Add, and Dismiss for a suggestion.
fn buttons(ui: &Ui, asset: &BinAsset, clip: &SweepCandidate, scenes: bool) -> El {
    let id = &clip.id;
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
    buttons
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

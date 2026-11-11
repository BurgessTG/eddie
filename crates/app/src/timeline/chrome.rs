//! What frames the clips: each lane's header card, the ruler and the
//! playhead.

use eddie_core::editor::lanes::{self, HEADER_W, RULER_H, TRACK_H};
use eddie_core::editor::viewport::TimelineViewport;
use openreel_engine::{ClipKind, Timeline};
use rune::{Color, El, LineCap, Shape, el, row, text};

use crate::icons;
use crate::theme::{self, c};

fn kind_index(kind: ClipKind) -> usize {
    match kind {
        ClipKind::Video => 0,
        ClipKind::Audio => 1,
        ClipKind::Title => 2,
    }
}

/// The kind colour × 1.8, for a lane header's accent bar.
fn kind_accent(kind: ClipKind) -> Color {
    theme::hex(match kind {
        ClipKind::Video => "#5385C1",
        ClipKind::Audio => "#4F976C",
        ClipKind::Title => "#C1952B",
    })
}

/// A lane header's glyph in a 16×16 box: film frame, level bars, a T, or
/// an empty dot.
fn kind_glyph(kind: Option<ClipKind>, fg: Color) -> El {
    let shape = Shape::new().view_box(16.0, 16.0);
    let shape =
        match kind {
            Some(ClipKind::Video) => icons::outline(shape, 1.5, 3.0, 13.0, 10.0, 2.0, 1.4, fg)
                .fill(vec![[6.5, 5.5], [10.5, 8.0], [6.5, 10.5]], fg),
            Some(ClipKind::Audio) => [4.0_f32, 9.0, 12.0, 7.0, 3.0].into_iter().enumerate().fold(
                shape,
                |shape, (i, h)| {
                    let x = 2.0 + i as f32 * 3.0;
                    shape.line(
                        [x, 8.0 - h / 2.0],
                        [x, 8.0 + h / 2.0],
                        1.4,
                        LineCap::Butt,
                        fg,
                    )
                },
            ),
            Some(ClipKind::Title) => {
                return row().size(16.0, 16.0).center().child(
                    text("T".to_owned())
                        .font(theme::sized(13.0))
                        .color(fg)
                        .single_line(),
                );
            }
            None => icons::ring(shape, [8.0, 8.0], 3.5, 1.4, fg),
        };
    el().size(16.0, 16.0).shape(shape)
}

/// A lane's header card. Lanes take any kind, so it shows the kind that
/// covers the most time there and stays neutral when the lane is empty.
pub(super) fn header(timeline: &Timeline, track: usize, label: &str, top: f32) -> El {
    let c = c();
    let mut cover = [0.0_f64; 3];
    for clip in timeline.clips.iter().filter(|clip| clip.track == track) {
        cover[kind_index(clip.kind)] += clip.duration;
    }
    let kind = cover.iter().any(|time| *time > 0.0).then(|| {
        [ClipKind::Video, ClipKind::Audio, ClipKind::Title]
            .into_iter()
            .max_by(|a, b| cover[kind_index(*a)].total_cmp(&cover[kind_index(*b)]))
            .unwrap_or(ClipKind::Video)
    });
    let (accent, glyph_fg, label_fg) = match kind {
        Some(kind) => (kind_accent(kind), c.muted_fg, c.foreground),
        None => (c.border_strong, c.subtle_fg, c.subtle_fg),
    };
    let width = HEADER_W - 8.0;
    el().key(format!("lane-header-{track}"))
        .absolute(0.0, top)
        .size(width, TRACK_H)
        .radius(theme::R_SM)
        .fill(c.card)
        .inert()
        .child(
            el().absolute(0.0, 10.0)
                .size(3.0, TRACK_H - 20.0)
                .radius(2.0)
                .fill(accent),
        )
        .child(kind_glyph(kind, glyph_fg).absolute(14.0, (TRACK_H - 16.0) / 2.0))
        .child(
            row().absolute(38.0, 0.0).h(TRACK_H).center_y().child(
                text(label.to_owned())
                    .font(theme::sized(12.0))
                    .color(label_fg)
                    .single_line()
                    .truncate()
                    .max_w(width - 38.0 - 6.0),
            ),
        )
}

/// The ruler's ticks and labels, `width` wide from the lanes' left edge.
pub(super) fn ruler(port: &TimelineViewport, width: f32, mut content: El) -> El {
    let c = c();
    let step = lanes::ruler_step(port.px_per_sec);
    let first = (lanes::time_at(port, 0.0, 0.0) / step).floor() * step;
    let minor = step / 4.0;
    // One point taller than the ruler, for the baseline under it.
    let mut ticks = Shape::new().view_box(width, RULER_H + 1.0);
    let mut labels = Vec::new();
    for i in 0_u32.. {
        let t = first + f64::from(i) * minor;
        // On the pixel grid, so each tick is one sharp column.
        let x = lanes::x_at(port, 0.0, t).floor() + 0.5;
        if x > width {
            break;
        }
        let major = i % 4 == 0;
        ticks = ticks.line(
            [x, RULER_H - if major { 9.0 } else { 4.0 }],
            [x, RULER_H],
            1.0,
            LineCap::Butt,
            if major { c.border_strong } else { c.border },
        );
        if major && t >= 0.0 {
            labels.push(
                text(lanes::ruler_label(t, step))
                    .font(theme::sized(10.5))
                    .color(c.subtle_fg)
                    .single_line()
                    .absolute(x + 5.5, 3.0),
            );
        }
    }
    ticks = ticks.line(
        [0.0, RULER_H + 0.5],
        [width, RULER_H + 0.5],
        1.0,
        LineCap::Butt,
        c.border,
    );
    content = content.child(
        el().absolute(0.0, 0.0)
            .size(width, RULER_H + 1.0)
            .shape(ticks),
    );
    labels
        .into_iter()
        .fold(content, |content, label| content.child(label))
}

/// The playhead: a line down the view with a pentagon head on the ruler.
pub(super) fn playhead(x: f32, height: f32) -> El {
    let color = c().destructive;
    let top = RULER_H - 15.0;
    let shape = Shape::new()
        .view_box(11.0, height)
        .line([5.5, 0.0], [5.5, height], 1.5, LineCap::Butt, color)
        .fill(
            vec![
                [0.0, top],
                [11.0, top],
                [11.0, top + 8.0],
                [5.5, top + 14.0],
                [0.0, top + 8.0],
            ],
            color,
        );
    el().key("playhead")
        .absolute(x - 5.5, 0.0)
        .size(11.0, height)
        .shape(shape)
}

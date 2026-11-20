//! Transitions in export: the frames each one draws, and the filters that
//! draw them from the two clips' pictures with FFmpeg's `xfade`.
//!
//! Each side is its clip's whole canvas, its picture placed on black as the
//! clip alone would show it, so the result is what
//! [`TransitionKind::frame`] describes for the monitor. `xfade` takes the
//! transition's own start and length in microseconds against a fine tick
//! clock, so its progress at each frame is the monitor's at that time, not
//! rounded to frames. A dip is two `fade`s, the clip into black or white
//! up to the cut and out of it after; `xfade`'s own `fadeblack` reaches
//! black in the first fifth and dips below video black.

use std::collections::HashMap;

use openreel_engine::{ClipId, Timeline, TransitionKind};

use super::video::{VideoClock, VIDEO_COLOR_RANGE, VIDEO_COLOR_SPACE};

/// One transition in output frames, and the layers that draw its clips.
pub(super) struct TransitionPlan {
    from: ClipId,
    to: ClipId,
    kind: TransitionKind,
    /// Timeline seconds: where it starts, the cut, where it ends.
    start: f64,
    cut: f64,
    end: f64,
    /// The first output frame it covers, the first after the cut, and the
    /// first after it.
    pub(super) first: i64,
    cut_frame: i64,
    pub(super) end_frame: i64,
    /// The frames it is the picture on; a lower lane can cover the rest.
    shown: Vec<(i64, i64)>,
    /// The layers that draw `from` and `to`; a side without one is black.
    pub(super) layers: [Option<usize>; 2],
}

impl TransitionPlan {
    pub(super) fn clips(&self) -> [ClipId; 2] {
        [self.from, self.to]
    }
}

/// Output frame ranges `[first, end)` per clip.
type FrameRanges = HashMap<ClipId, Vec<(i64, i64)>>;

/// Plan every transition that shows, taking the frames it draws out of
/// `visible`. Returns the plans and, per clip, the frames its picture must
/// be decoded over for them: the whole transition, past its own end or
/// before its start.
pub(super) fn plan(
    timeline: &Timeline,
    clock: VideoClock,
    visible: &mut FrameRanges,
) -> (Vec<TransitionPlan>, FrameRanges) {
    let mut plans = Vec::new();
    let mut extents = FrameRanges::new();
    for span in timeline.transitions() {
        let (first, end_frame) = (clock.boundary(span.start), clock.boundary(span.end));
        if first >= end_frame {
            continue;
        }
        let mut shown = Vec::new();
        for clip in [span.from, span.to] {
            if let Some(ranges) = visible.get_mut(&clip) {
                shown.extend(take(ranges, (first, end_frame)));
            }
        }
        if shown.is_empty() {
            continue;
        }
        shown.sort_unstable();
        for clip in [span.from, span.to] {
            extents.entry(clip).or_default().push((first, end_frame));
        }
        plans.push(TransitionPlan {
            from: span.from,
            to: span.to,
            kind: span.kind,
            start: span.start,
            cut: span.cut,
            end: span.end,
            first,
            cut_frame: clock.boundary(span.cut),
            end_frame,
            shown,
            layers: [None, None],
        });
    }
    (plans, extents)
}

/// Remove the frames of `ranges` inside `window` and return them.
fn take(ranges: &mut Vec<(i64, i64)>, (start, end): (i64, i64)) -> Vec<(i64, i64)> {
    let mut taken = Vec::new();
    let mut kept = Vec::with_capacity(ranges.len() + 1);
    for &(a, b) in ranges.iter() {
        let (inside_a, inside_b) = (a.max(start), b.min(end));
        if inside_a >= inside_b {
            kept.push((a, b));
            continue;
        }
        taken.push((inside_a, inside_b));
        if a < start {
            kept.push((a, start));
        }
        if b > end {
            kept.push((end, b));
        }
    }
    *ranges = kept;
    taken
}

/// `xfade`'s name for a transition that is one `xfade`.
fn xfade(kind: TransitionKind) -> Option<&'static str> {
    Some(match kind {
        TransitionKind::Mix => "fade",
        TransitionKind::WipeLeft => "wipeleft",
        TransitionKind::WipeRight => "wiperight",
        TransitionKind::WipeUp => "wipeup",
        TransitionKind::WipeDown => "wipedown",
        TransitionKind::SlideLeft => "slideleft",
        TransitionKind::SlideRight => "slideright",
        TransitionKind::SlideUp => "slideup",
        TransitionKind::SlideDown => "slidedown",
        TransitionKind::DipBlack | TransitionKind::DipWhite => return None,
    })
}

/// The filters that draw transition `index` of a batch whose first frame is
/// `batch_start` over the picture labelled `current`, and the label of the
/// result. `sides` says where each clip's picture goes on the canvas; its
/// frames come in as `[tw{index}_{side}]`, numbered on the batch's frames.
pub(super) fn filters(
    plan: &TransitionPlan,
    index: usize,
    batch_start: i64,
    (width, height): (u32, u32),
    clock: VideoClock,
    sides: [Option<(i64, i64)>; 2],
    current: &str,
) -> (Vec<String>, String) {
    let rate = clock.rate;
    let frame_base = format!("{}/{}", rate.denominator(), rate.numerator());
    let ticks = clock.tick_rate;
    let [first, cut, end] = [plan.first, plan.cut_frame, plan.end_frame].map(|f| f - batch_start);
    // xfade wants 4:4:4 and one time base on both inputs; a fine one, so
    // its progress is the transition's own and not rounded to frames.
    let canvas = |colour: &str, frames: i64| {
        format!(
            "color=c={colour}:s={width}x{height}:r={rate},\
             setparams=colorspace={VIDEO_COLOR_SPACE}:range={VIDEO_COLOR_RANGE},\
             trim=end_frame={frames},format=yuv444p"
        )
    };
    let mut filters = Vec::new();
    let mut used = [false; 2];
    // Side `side`'s whole canvas over batch frames `from` to `to`.
    let mut side = |filters: &mut Vec<String>, side: usize, from: i64, to: i64| {
        used[side] = true;
        let label = format!("tc{index}_{side}");
        let black = canvas("black", to - from);
        filters.push(match sides[side] {
            Some((x, y)) => format!(
                "[tw{index}_{side}]trim=start_pts={from}:end_pts={to},setpts=PTS-STARTPTS[tp{label}];\
                 {black}[tb{label}];\
                 [tb{label}][tp{label}]overlay={x}:{y}:eof_action=repeat:format=yuv444,\
                 settb=1/{ticks}[{label}]"
            ),
            None => format!("{black},settb=1/{ticks}[{label}]"),
        });
        label
    };
    // `[a]` into `[b]` from timeline time `start` over `length` seconds;
    // both start at output frame `frame`, at or just after `start`.
    let blend = |a: &str, b: &str, name: &str, start: f64, length: f64, frame: i64, out: &str| {
        format!(
            "[{a}][{b}]xfade=transition={name}:duration={:.6}:offset={:.6}[{out}]",
            length.max(1e-6),
            start - clock.time(frame),
        )
    };
    let result = format!("tx{index}");
    match xfade(plan.kind) {
        Some(name) => {
            let a = side(&mut filters, 0, first, end);
            let b = side(&mut filters, 1, first, end);
            filters.push(blend(
                &a,
                &b,
                name,
                plan.start,
                plan.end - plan.start,
                plan.first,
                &result,
            ));
        }
        None => {
            // Into the colour up to the cut, out of it after.
            let colour = if plan.kind == TransitionKind::DipWhite {
                "white"
            } else {
                "black"
            };
            let mut halves = String::new();
            let mut count = 0;
            if cut > first {
                let a = side(&mut filters, 0, first, cut);
                let held = format!("tk{index}_0");
                filters.push(format!(
                    "{},settb=1/{ticks}[{held}]",
                    canvas(colour, cut - first)
                ));
                let out = format!("th{index}_0");
                filters.push(blend(
                    &a,
                    &held,
                    "fade",
                    plan.start,
                    plan.cut - plan.start,
                    plan.first,
                    &out,
                ));
                halves.push_str(&format!("[{out}]"));
                count += 1;
            }
            if end > cut {
                let b = side(&mut filters, 1, cut, end);
                let held = format!("tk{index}_1");
                filters.push(format!(
                    "{},settb=1/{ticks}[{held}]",
                    canvas(colour, end - cut)
                ));
                let out = format!("th{index}_1");
                filters.push(blend(
                    &held,
                    &b,
                    "fade",
                    plan.cut,
                    plan.end - plan.cut,
                    plan.cut_frame,
                    &out,
                ));
                halves.push_str(&format!("[{out}]"));
                count += 1;
            }
            filters.push(if count == 1 {
                format!("{halves}null[{result}]")
            } else {
                format!("{halves}concat=n=2:v=1:a=0[{result}]")
            });
        }
    }
    // Every picture split off for it is taken, even one a dip never shows.
    for (side, place) in sides.iter().enumerate() {
        if place.is_some() && !used[side] {
            filters.push(format!("[tw{index}_{side}]nullsink"));
        }
    }
    let shown: Vec<_> = plan
        .shown
        .iter()
        .map(|&(a, b)| (a - batch_start, b - batch_start))
        .collect();
    // Padded from the batch's first frame, so the picture under it is not
    // held back until the transition's first frame is ready.
    let pad = if first > 0 {
        format!("tpad=start={first}:start_mode=add:color=black,")
    } else {
        String::new()
    };
    let next = format!("vt{index}");
    filters.push(format!(
        "[{result}]settb={frame_base},{pad}setpts=N[ts{index}];\
         [{current}][ts{index}]overlay=0:0:eof_action=repeat:ts_sync_mode=default:\
         enable='{}'[{next}]",
        super::video::enabled_frames(&shown),
    ));
    (filters, next)
}

//! Video lanes in layers. Every video lane draws over the lanes under it
//! (higher numbers, lower on the timeline), lane 0 on top, so a picture
//! moved, scaled down or faded over another shows the other around or
//! through it. A lane hides the lanes under it only where its picture is
//! opaque and fills the canvas ([`hides_below`]).

use crate::{place, Background, Clip, ClipKind, Timeline, TransitionSpan};

/// One video lane's picture at a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LanePicture<'a> {
    pub lane: usize,
    /// The clip whose picture the lane shows; during a transition, the clip
    /// it comes from, past the cut.
    pub clip: &'a Clip,
    /// The transition the lane is part way through, drawing `clip` and the
    /// clip it goes to.
    pub transition: Option<TransitionSpan>,
}

/// Whether `clip`'s picture hides everything under it: fully opaque, from
/// a file with no `alpha`, and filling a `canvas` sized frame (its file
/// `source` sized) or drawn over its own blurred copy, which does. A
/// picture whose keys move or fade it may uncover the lanes under it, so it
/// never hides them.
pub fn hides_below(clip: &Clip, canvas: (f64, f64), source: (f64, f64), alpha: bool) -> bool {
    clip.opacity >= 1.0
        && !clip.keyframes.moves_picture()
        && !alpha
        && (clip.background == Background::Blur
            || place(canvas, source, &clip.transform).covers(canvas))
}

impl Timeline {
    /// Lane `lane`'s picture at `time` among `spans` (this timeline's
    /// [`Timeline::transitions`]), whether or not a lane over it hides it.
    pub fn lane_picture(
        &self,
        spans: &[TransitionSpan],
        lane: usize,
        time: f64,
    ) -> Option<LanePicture<'_>> {
        let on_lane = |clip: &Clip| clip.kind == ClipKind::Video && clip.track == lane;
        if let Some(span) = spans
            .iter()
            .find(|span| span.contains(time) && self.clip(span.from).is_some_and(on_lane))
        {
            return Some(LanePicture {
                lane,
                clip: self.clip(span.from)?,
                transition: Some(*span),
            });
        }
        let clip = self
            .clips
            .iter()
            .find(|clip| on_lane(clip) && clip.contains(time))?;
        Some(LanePicture {
            lane,
            clip,
            transition: None,
        })
    }

    /// The pictures showing at `time`, bottom first: each video lane's,
    /// from the last lane up to lane 0, starting at the top-most one that
    /// `hides` everything under it. A lane part way through a transition
    /// hides only when both its clips do.
    pub fn video_stack<'a>(
        &'a self,
        spans: &[TransitionSpan],
        time: f64,
        hides: &dyn Fn(&Clip) -> bool,
    ) -> Vec<LanePicture<'a>> {
        let mut stack = Vec::new();
        for lane in 0..self.lanes.video.max(self.video_lanes_used()) {
            let Some(picture) = self.lane_picture(spans, lane, time) else {
                continue;
            };
            let covers = hides(picture.clip)
                && picture
                    .transition
                    .is_none_or(|span| self.clip(span.to).is_some_and(hides));
            stack.push(picture);
            if covers {
                break;
            }
        }
        stack.reverse();
        stack
    }

    /// Video lanes with a clip on them: a timeline built in a test may not
    /// count its lanes.
    fn video_lanes_used(&self) -> usize {
        self.clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Video)
            .map(|clip| clip.track + 1)
            .max()
            .unwrap_or(0)
    }
}

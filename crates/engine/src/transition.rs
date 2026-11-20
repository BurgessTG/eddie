//! Transitions: how the picture passes from one clip to the next at a cut.
//!
//! A transition belongs to the clip it leads into and sits centred on the
//! cut, so adding one moves nothing. Over its span the clip before plays on
//! past its out point and the clip after starts before its in point; where
//! a file has no picture there, its edge picture holds.

use crate::{Clip, ClipId, ClipKind, Timeline};

/// How one picture becomes the next. Export renders each with FFmpeg's
/// `xfade` (a dip as two `fade`s through black or white), and the program
/// monitor draws what [`TransitionKind::frame`] says, which follows
/// `xfade`'s own arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransitionKind {
    /// The next picture fades in over this one.
    Mix,
    /// This picture fades to black, then the next fades in from it.
    DipBlack,
    /// The same through white.
    DipWhite,
    /// A line sweeps from right to left, showing the next picture behind it.
    WipeLeft,
    WipeRight,
    /// A line sweeps from the bottom up.
    WipeUp,
    WipeDown,
    /// The next picture comes in from the right and pushes this one out to
    /// the left.
    SlideLeft,
    SlideRight,
    /// The next picture comes in from below and pushes this one up.
    SlideUp,
    SlideDown,
}

impl TransitionKind {
    pub const ALL: [Self; 11] = [
        Self::Mix,
        Self::DipBlack,
        Self::DipWhite,
        Self::WipeLeft,
        Self::WipeRight,
        Self::WipeUp,
        Self::WipeDown,
        Self::SlideLeft,
        Self::SlideRight,
        Self::SlideUp,
        Self::SlideDown,
    ];

    /// The name scripts and project files use, such as `wipe-left`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mix => "mix",
            Self::DipBlack => "dip-black",
            Self::DipWhite => "dip-white",
            Self::WipeLeft => "wipe-left",
            Self::WipeRight => "wipe-right",
            Self::WipeUp => "wipe-up",
            Self::WipeDown => "wipe-down",
            Self::SlideLeft => "slide-left",
            Self::SlideRight => "slide-right",
            Self::SlideUp => "slide-up",
            Self::SlideDown => "slide-down",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mix => "Mix",
            Self::DipBlack => "Dip to black",
            Self::DipWhite => "Dip to white",
            Self::WipeLeft => "Wipe left",
            Self::WipeRight => "Wipe right",
            Self::WipeUp => "Wipe up",
            Self::WipeDown => "Wipe down",
            Self::SlideLeft => "Slide left",
            Self::SlideRight => "Slide right",
            Self::SlideUp => "Slide up",
            Self::SlideDown => "Slide down",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_lowercase();
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == name || kind.label().to_lowercase() == name)
    }

    /// The picture `t` of the way through (0 to 1) on a `width` by
    /// `height` canvas. `xfade` works from its progress `p = 1 - t` and
    /// whole pixels: a wipe's edge is at `int(width * p)`, and a slide
    /// moves both pictures by `int(width * p)`.
    pub fn frame(self, t: f64, (width, height): (u32, u32)) -> TransitionFrame {
        let t = t.clamp(0.0, 1.0);
        let p = 1.0 - t;
        let (w, h) = (f64::from(width), f64::from(height));
        let whole = [0.0, 0.0, w, h];
        let shown = |opacity| Layer {
            offset: [0.0, 0.0],
            window: whole,
            opacity,
        };
        let at = |offset: [f64; 2]| Layer {
            offset,
            window: whole,
            opacity: 1.0,
        };
        let cut_to = |window: [f64; 4]| Layer {
            offset: [0.0, 0.0],
            window,
            opacity: 1.0,
        };
        // A pixel shows the next picture past the edge: `x > int(w * p)`.
        let edge = |size: f64, share: f64| ((size * share) as i64 + 1).min(size as i64) as f64;
        let (from, to, veil) = match self {
            Self::Mix => (shown(1.0), shown(t), None),
            Self::DipBlack | Self::DipWhite => {
                let white = self == Self::DipWhite;
                if t < 0.5 {
                    (shown(1.0), shown(0.0), Some((white, 2.0 * t)))
                } else {
                    (shown(0.0), shown(1.0), Some((white, 2.0 - 2.0 * t)))
                }
            }
            Self::WipeLeft => {
                let x = edge(w, p);
                (shown(1.0), cut_to([x, 0.0, w - x, h]), None)
            }
            Self::WipeRight => (shown(1.0), cut_to([0.0, 0.0, edge(w, t), h]), None),
            Self::WipeUp => {
                let y = edge(h, p);
                (shown(1.0), cut_to([0.0, y, w, h - y]), None)
            }
            Self::WipeDown => (shown(1.0), cut_to([0.0, 0.0, w, edge(h, t)]), None),
            Self::SlideLeft => {
                let z = (w * p).trunc();
                (at([z - w, 0.0]), at([z, 0.0]), None)
            }
            Self::SlideRight => {
                let z = (w * p).trunc();
                (at([w - z, 0.0]), at([-z, 0.0]), None)
            }
            Self::SlideUp => {
                let z = (h * p).trunc();
                (at([0.0, z - h]), at([0.0, z]), None)
            }
            Self::SlideDown => {
                let z = (h * p).trunc();
                (at([0.0, h - z]), at([0.0, -z]), None)
            }
        };
        TransitionFrame { from, to, veil }
    }
}

/// One moment of a transition: the clip it comes from drawn first, the
/// clip it goes to over it, then a black or white veil over both. Each
/// picture is its clip's whole canvas, as the clip alone would show it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionFrame {
    pub from: Layer,
    pub to: Layer,
    /// White (`true`) or black, and its opacity.
    pub veil: Option<(bool, f64)>,
}

/// Where one picture of a transition shows, in canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layer {
    /// How far the picture is moved from its place.
    pub offset: [f64; 2],
    /// The part of the canvas it shows in, `[x, y, width, height]`, after
    /// the move; outside it the picture below shows.
    pub window: [f64; 4],
    pub opacity: f64,
}

/// The shortest and longest a transition takes, and what one takes when
/// added without a length (CapCut's default).
pub const MIN_TRANSITION: f64 = 0.1;
pub const MAX_TRANSITION: f64 = 5.0;
pub const DEFAULT_TRANSITION: f64 = 0.5;

/// A clip's way in from the clip before it.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Transition {
    pub kind: TransitionKind,
    /// Seconds, centred on the cut. Each side takes at most half of its
    /// clip, so a short clip shortens it ([`TransitionSpan`]).
    pub duration: f64,
}

impl Transition {
    pub fn new(kind: TransitionKind, duration: f64) -> Self {
        Self {
            kind,
            duration: duration.clamp(MIN_TRANSITION, MAX_TRANSITION),
        }
    }

    pub fn is_valid(&self) -> bool {
        (MIN_TRANSITION..=MAX_TRANSITION).contains(&self.duration)
    }
}

/// A transition where it plays: from `start` to `end` around the cut at
/// `cut`, passing from clip `from` to clip `to`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionSpan {
    pub from: ClipId,
    pub to: ClipId,
    pub kind: TransitionKind,
    pub start: f64,
    pub cut: f64,
    pub end: f64,
}

impl TransitionSpan {
    /// How far through it `time` is: 0 at its start, 1 at its end.
    pub fn progress(&self, time: f64) -> f64 {
        ((time - self.start) / (self.end - self.start)).clamp(0.0, 1.0)
    }

    pub fn contains(&self, time: f64) -> bool {
        self.start <= time && time < self.end
    }
}

impl Timeline {
    /// The video clip that ends where clip `id` starts, on its lane (within
    /// half a frame): where a transition into `id` comes from.
    pub fn clip_before(&self, id: ClipId) -> Option<&Clip> {
        let clip = self.clip(id).filter(|clip| clip.kind == ClipKind::Video)?;
        let slack = 0.5 / self.fps;
        let gap = |other: &Clip| (other.end() - clip.start).abs();
        self.clips
            .iter()
            .filter(|other| other.id != id && other.same_lane(clip) && gap(other) <= slack)
            .min_by(|a, b| gap(a).total_cmp(&gap(b)))
    }

    /// Every transition that has a clip to come from, in timeline order.
    /// Each side takes at most half its clip, so the two transitions of a
    /// clip never meet.
    pub fn transitions(&self) -> Vec<TransitionSpan> {
        let mut spans: Vec<TransitionSpan> = self
            .clips
            .iter()
            .filter_map(|to| {
                let transition = to.transition?;
                let from = self.clip_before(to.id)?;
                let half = (transition.duration / 2.0)
                    .min(from.duration / 2.0)
                    .min(to.duration / 2.0);
                (half > 0.0).then_some(TransitionSpan {
                    from: from.id,
                    to: to.id,
                    kind: transition.kind,
                    start: to.start - half,
                    cut: to.start,
                    end: to.start + half,
                })
            })
            .collect();
        spans.sort_by(|a, b| a.start.total_cmp(&b.start));
        spans
    }

    /// The transition showing at `time`: one whose clips are the picture
    /// there ([`Timeline::active_video`]), so a lower lane covering it
    /// hides it.
    pub fn transition_at(&self, time: f64) -> Option<TransitionSpan> {
        self.transition_among(&self.transitions(), time)
    }

    /// [`Timeline::transition_at`] among `spans`, this timeline's
    /// [`Timeline::transitions`], for callers that look up many times.
    pub fn transition_among(&self, spans: &[TransitionSpan], time: f64) -> Option<TransitionSpan> {
        let showing = spans.iter().filter(|span| span.contains(time));
        let mut showing = showing.peekable();
        showing.peek()?;
        let active = self.active_video(time)?.id;
        showing
            .find(|span| span.from == active || span.to == active)
            .copied()
    }

    /// The clip whose picture the program shows at `time`: the active
    /// video, except during a transition, where the clip it comes from plays
    /// on past the cut to the transition's end and the clip it goes to is
    /// drawn with it.
    pub fn program_video(&self, time: f64) -> Option<&Clip> {
        self.program_video_among(&self.transitions(), time)
    }

    /// [`Timeline::program_video`] among `spans`.
    pub fn program_video_among(&self, spans: &[TransitionSpan], time: f64) -> Option<&Clip> {
        match self.transition_among(spans, time) {
            Some(span) => self.clip(span.from),
            None => self.active_video(time),
        }
    }
}

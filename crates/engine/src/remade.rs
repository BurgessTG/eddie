//! Clips that play a file Eddie wrote from a range of another: played
//! backwards, stabilized, or both. The clip's `source` is the written file,
//! and [`Remade`] says what it holds, so every renderer reads it like any
//! file and a project can write it again from the original.

use serde::{Deserialize, Serialize};

use crate::Clip;

/// What a remade clip's file holds: `[start, end)` of `original`, whole
/// frames, stabilized as `stabilize` says, and last frame first when
/// `reverse` is set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Remade {
    /// The file shot, which the written one was made from.
    pub original: String,
    pub start: f64,
    pub end: f64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reverse: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stabilize: Option<Stabilize>,
}

/// How hard stabilization smooths the camera's path, and how far it may
/// move the picture, which it enlarges to hide the edges that uncovers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stabilize {
    /// Steadies only the quickest shake and crops the least.
    MinimalCrop,
    Recommended,
    /// Smooths the camera's path the most and crops the most.
    MostStable,
}

impl Stabilize {
    pub const ALL: [Stabilize; 3] = [
        Stabilize::MinimalCrop,
        Stabilize::Recommended,
        Stabilize::MostStable,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Stabilize::MinimalCrop => "minimal-crop",
            Stabilize::Recommended => "recommended",
            Stabilize::MostStable => "most-stable",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Stabilize::MinimalCrop => "Minimal crop",
            Stabilize::Recommended => "Recommended",
            Stabilize::MostStable => "Most stable",
        }
    }

    /// What the level does, for its tooltip.
    pub fn help(self) -> &'static str {
        match self {
            Stabilize::MinimalCrop => "Steady only the quickest shake and crop the least",
            Stabilize::Recommended => "Steady the shake and keep slow camera moves",
            Stabilize::MostStable => "Hold the camera as still as it can; crops the most",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.name() == name)
    }

    /// How many seconds of the camera's path are averaged into each
    /// frame's: the longer, the steadier the picture. Recommended is
    /// vid.stab's own default at 30 fps.
    pub fn smoothing(self) -> f64 {
        match self {
            Stabilize::MinimalCrop => 0.5,
            Stabilize::Recommended => 1.0,
            Stabilize::MostStable => 2.0,
        }
    }

    /// The furthest the picture is moved, as a share of its shorter side,
    /// and the most it is turned, in radians. The picture is enlarged just
    /// enough to keep its edges covered, so these bound the crop.
    pub fn limits(self) -> (f64, f64) {
        match self {
            Stabilize::MinimalCrop => (0.03, 0.01),
            Stabilize::Recommended => (0.06, 0.03),
            Stabilize::MostStable => (0.12, 0.06),
        }
    }
}

impl Remade {
    /// The original's time shown at time `t` of the written file.
    pub fn to_original(&self, t: f64) -> f64 {
        if self.reverse {
            self.end - t
        } else {
            self.start + t
        }
    }

    /// `[from, to)` of the original as a range of the written file: the
    /// same stretch of picture, read from the other end when reversed.
    pub fn to_file_range(&self, from: f64, to: f64) -> (f64, f64) {
        if self.reverse {
            (self.end - to, self.end - from)
        } else {
            (from - self.start, to - self.start)
        }
    }

    /// The original's range a clip playing the written file shows.
    pub fn original_range(&self, clip: &Clip) -> (f64, f64) {
        let (from, to) = (
            self.to_original(clip.source_in),
            self.to_original(clip.source_end()),
        );
        (from.min(to), from.max(to))
    }

    /// Whether `[from, to)` of the original lies inside the written file.
    pub fn holds(&self, from: f64, to: f64) -> bool {
        from >= self.start - 1e-6 && to <= self.end + 1e-6
    }

    pub fn is_valid(&self) -> bool {
        self.start.is_finite()
            && self.end.is_finite()
            && 0.0 <= self.start
            && self.start < self.end
            && (self.reverse || self.stabilize.is_some())
    }
}

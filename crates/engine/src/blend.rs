//! Blend modes: how a picture mixes with what the lanes under it show.
//! Each is the W3C compositing formula, channel by channel, on encoded
//! colour values from 0 to 1, as photo and video editors apply them. The
//! picture's own transparency and opacity then mix the result with what is
//! under it: `under + alpha * (blend(under, picture) - under)`.
//!
//! [`BlendMode::table`] is every 8-bit answer, which the preview looks up;
//! export computes the same numbers with FFmpeg's `lut2`.

use std::sync::OnceLock;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum BlendMode {
    #[default]
    Normal,
    Darken,
    Multiply,
    ColorBurn,
    LinearBurn,
    Lighten,
    Screen,
    ColorDodge,
    Add,
    Overlay,
    SoftLight,
    HardLight,
    Difference,
    Exclusion,
}

impl BlendMode {
    /// In the order a menu lists them: darkening, lightening, contrast,
    /// then comparison.
    pub const ALL: [Self; 14] = [
        Self::Normal,
        Self::Darken,
        Self::Multiply,
        Self::ColorBurn,
        Self::LinearBurn,
        Self::Lighten,
        Self::Screen,
        Self::ColorDodge,
        Self::Add,
        Self::Overlay,
        Self::SoftLight,
        Self::HardLight,
        Self::Difference,
        Self::Exclusion,
    ];

    /// The name scripts and project files use, such as `soft-light`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Darken => "darken",
            Self::Multiply => "multiply",
            Self::ColorBurn => "color-burn",
            Self::LinearBurn => "linear-burn",
            Self::Lighten => "lighten",
            Self::Screen => "screen",
            Self::ColorDodge => "color-dodge",
            Self::Add => "add",
            Self::Overlay => "overlay",
            Self::SoftLight => "soft-light",
            Self::HardLight => "hard-light",
            Self::Difference => "difference",
            Self::Exclusion => "exclusion",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Darken => "Darken",
            Self::Multiply => "Multiply",
            Self::ColorBurn => "Colour burn",
            Self::LinearBurn => "Linear burn",
            Self::Lighten => "Lighten",
            Self::Screen => "Screen",
            Self::ColorDodge => "Colour dodge",
            Self::Add => "Add",
            Self::Overlay => "Overlay",
            Self::SoftLight => "Soft light",
            Self::HardLight => "Hard light",
            Self::Difference => "Difference",
            Self::Exclusion => "Exclusion",
        }
    }

    /// What it does, for a tooltip.
    pub fn help(self) -> &'static str {
        match self {
            Self::Normal => "Draw the picture over the lanes under it",
            Self::Darken => "Keep the darker of the picture and what is under it",
            Self::Multiply => "Darken what is under it by the picture; white disappears",
            Self::ColorBurn => "Darken what is under it with more contrast; white disappears",
            Self::LinearBurn => {
                "Darken what is under it by the picture's darkness; white disappears"
            }
            Self::Lighten => "Keep the lighter of the picture and what is under it",
            Self::Screen => "Lighten what is under it by the picture; black disappears",
            Self::ColorDodge => "Brighten what is under it strongly; black disappears",
            Self::Add => "Add the picture's light to what is under it; black disappears",
            Self::Overlay => "Deepen the darks and lift the lights under it; mid-grey disappears",
            Self::SoftLight => "A gentler overlay; mid-grey disappears",
            Self::HardLight => "Darken or lighten by the picture's own darks and lights",
            Self::Difference => "Subtract the darker from the lighter; black disappears",
            Self::Exclusion => "Like difference with less contrast; black disappears",
        }
    }

    /// The mode a script or file names, by name (`color-dodge`) or label
    /// (`Colour dodge`).
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_lowercase();
        Self::ALL.into_iter().find(|mode| {
            mode.as_str() == name
                || mode.label().to_lowercase() == name
                || mode.as_str().replace('-', " ") == name
        })
    }

    pub fn is_normal(&self) -> bool {
        *self == Self::Normal
    }

    /// One channel: `under` is what the lanes under show, `over` the
    /// picture, both from 0 to 1. Export's `lut2` expressions repeat these
    /// steps in the same order, so the two round alike.
    pub fn apply(self, under: f64, over: f64) -> f64 {
        let (b, s) = (under, over);
        match self {
            Self::Normal => s,
            Self::Darken => b.min(s),
            Self::Multiply => b * s,
            Self::ColorBurn => {
                if b >= 1.0 {
                    1.0
                } else if s <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - b) / s).min(1.0)
                }
            }
            Self::LinearBurn => (b + s - 1.0).max(0.0),
            Self::Lighten => b.max(s),
            Self::Screen => b + s - b * s,
            Self::ColorDodge => {
                if b <= 0.0 {
                    0.0
                } else if s >= 1.0 {
                    1.0
                } else {
                    (b / (1.0 - s)).min(1.0)
                }
            }
            Self::Add => (b + s).min(1.0),
            // Hard light with the two swapped: the lanes under decide.
            Self::Overlay => Self::HardLight.apply(s, b),
            Self::HardLight => {
                if s <= 0.5 {
                    b * (2.0 * s)
                } else {
                    let t = 2.0 * s - 1.0;
                    b + t - b * t
                }
            }
            Self::SoftLight => {
                if s <= 0.5 {
                    b - (1.0 - 2.0 * s) * b * (1.0 - b)
                } else {
                    let d = if b <= 0.25 {
                        ((16.0 * b - 12.0) * b + 4.0) * b
                    } else {
                        b.sqrt()
                    };
                    b + (2.0 * s - 1.0) * (d - b)
                }
            }
            Self::Difference => (b - s).abs(),
            Self::Exclusion => b + s - 2.0 * b * s,
        }
    }

    /// Every 8-bit answer, `[under * 256 + over]`: the blend rounded to
    /// the nearest level, as `round(255 * clip(blend, 0, 1))`.
    pub fn table(self) -> &'static [u8] {
        static TABLES: [OnceLock<Vec<u8>>; BlendMode::ALL.len()] =
            [const { OnceLock::new() }; BlendMode::ALL.len()];
        let index = Self::ALL.iter().position(|&mode| mode == self).unwrap_or(0);
        TABLES[index].get_or_init(|| {
            (0..256 * 256)
                .map(|i| {
                    let (b, s) = (f64::from(i / 256) / 255.0, f64::from(i % 256) / 255.0);
                    (255.0 * self.apply(b, s).clamp(0.0, 1.0)).round() as u8
                })
                .collect()
        })
    }
}

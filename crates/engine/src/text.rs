//! A title's own text look over its style: size, colour, place, box and
//! outline. `openreel_media::clip_title_look` lays it over the style's
//! look, and the program monitor, export and contact sheets all draw from
//! that, so a styled title previews as it exports.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An sRGB colour, written `#RRGGBB` in projects and scripts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const WHITE: Self = Self(255, 255, 255);
    pub const BLACK: Self = Self(0, 0, 0);

    /// `#RRGGBB`, upper case.
    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }

    /// `#RRGGBB` or `#RGB`, with or without the `#`, in either case.
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.trim();
        let digits = digits.strip_prefix('#').unwrap_or(digits);
        if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |at: usize, len: usize| u8::from_str_radix(&digits[at..at + len], 16).ok();
        match digits.len() {
            6 => Some(Self(channel(0, 2)?, channel(2, 2)?, channel(4, 2)?)),
            3 => {
                let short = |at| channel(at, 1).map(|v| v * 17);
                Some(Self(short(0)?, short(1)?, short(2)?))
            }
            _ => None,
        }
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "a colour is '#RRGGBB', such as '#FFD600', not '{text}'"
            ))
        })
    }
}

/// What a title changes from its style. The default changes nothing; a
/// `None` keeps the style's value.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TextLook {
    /// Font size against the style's, [`MIN_TEXT_SIZE`] to
    /// [`MAX_TEXT_SIZE`]. The text grows upward from its lower edge.
    pub size: f64,
    pub color: Option<Rgb>,
    /// The box behind the text, 0 (none) to 1 (solid). A box colour on a
    /// style without a box turns one on at [`DEFAULT_BOX_OPACITY`].
    pub box_opacity: Option<f64>,
    pub box_color: Option<Rgb>,
    /// Outline thickness as a share of the font size, 0 (none) to
    /// [`MAX_OUTLINE`]. An outline colour alone turns one on at
    /// [`DEFAULT_OUTLINE`].
    pub outline: Option<f64>,
    pub outline_color: Option<Rgb>,
    /// Moved from where the style puts the text, in canvas widths and
    /// heights; right and down are positive.
    pub x: f64,
    pub y: f64,
}

pub const MIN_TEXT_SIZE: f64 = 0.25;
pub const MAX_TEXT_SIZE: f64 = 4.0;
pub const MAX_OUTLINE: f64 = 0.2;
/// How far text may move from its style's place, in canvases each way.
pub const MAX_TEXT_OFFSET: f64 = 1.0;
pub const DEFAULT_BOX_OPACITY: f64 = 0.6;
pub const DEFAULT_OUTLINE: f64 = 0.06;

impl Default for TextLook {
    fn default() -> Self {
        Self {
            size: 1.0,
            color: None,
            box_opacity: None,
            box_color: None,
            outline: None,
            outline_color: None,
            x: 0.0,
            y: 0.0,
        }
    }
}

impl TextLook {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// Whether every number is finite and in its range.
    pub fn is_valid(&self) -> bool {
        (MIN_TEXT_SIZE..=MAX_TEXT_SIZE).contains(&self.size)
            && self.box_opacity.is_none_or(|v| (0.0..=1.0).contains(&v))
            && self
                .outline
                .is_none_or(|v| (0.0..=MAX_OUTLINE).contains(&v))
            && [self.x, self.y]
                .iter()
                .all(|v| (-MAX_TEXT_OFFSET..=MAX_TEXT_OFFSET).contains(v))
    }

    /// Every number moved into its range; one that is not a number goes
    /// back to the style's.
    pub fn clamped(mut self) -> Self {
        let fix = |v: f64, lo: f64, hi: f64, default: f64| {
            if v.is_nan() {
                default
            } else {
                v.clamp(lo, hi)
            }
        };
        self.size = fix(self.size, MIN_TEXT_SIZE, MAX_TEXT_SIZE, 1.0);
        self.box_opacity = self
            .box_opacity
            .filter(|v| !v.is_nan())
            .map(|v| v.clamp(0.0, 1.0));
        self.outline = self
            .outline
            .filter(|v| !v.is_nan())
            .map(|v| v.clamp(0.0, MAX_OUTLINE));
        self.x = fix(self.x, -MAX_TEXT_OFFSET, MAX_TEXT_OFFSET, 0.0);
        self.y = fix(self.y, -MAX_TEXT_OFFSET, MAX_TEXT_OFFSET, 0.0);
        self
    }
}

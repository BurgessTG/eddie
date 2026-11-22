//! Shapes and stickers: pictures Eddie draws itself rather than reads
//! from a file. A graphic clip is a video clip holding one picture, like a
//! photo: `openreel_media::graphic` draws it to a PNG, the clip's `source`
//! names that file, and the photo path plays, exports and composites it,
//! so a graphic takes a transform, keys, a blend, a mask and transitions
//! like any picture.

use serde::{Deserialize, Serialize};

use crate::Rgb;

/// What a graphic clip draws.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Graphic {
    Shape(Shape),
    /// One of the built-in stickers, by name (`openreel_media::STICKERS`).
    Sticker {
        name: String,
    },
}

/// The shapes a shape clip can be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShapeKind {
    /// Its corners rounded by `round`.
    #[default]
    Rectangle,
    /// An ellipse filling its box.
    Circle,
    /// Point up.
    Triangle,
    /// Five points, one up.
    Star,
    Heart,
    /// Pointing right; turn the clip for other directions.
    Arrow,
    /// A speech bubble, its tail at the lower left; `round` rounds it.
    Bubble,
}

impl ShapeKind {
    pub const ALL: [Self; 7] = [
        Self::Rectangle,
        Self::Circle,
        Self::Triangle,
        Self::Star,
        Self::Heart,
        Self::Arrow,
        Self::Bubble,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rectangle => "rectangle",
            Self::Circle => "circle",
            Self::Triangle => "triangle",
            Self::Star => "star",
            Self::Heart => "heart",
            Self::Arrow => "arrow",
            Self::Bubble => "bubble",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Rectangle => "Rectangle",
            Self::Circle => "Circle",
            Self::Triangle => "Triangle",
            Self::Star => "Star",
            Self::Heart => "Heart",
            Self::Arrow => "Arrow",
            Self::Bubble => "Speech bubble",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_lowercase();
        Self::ALL.into_iter().find(|kind| kind.as_str() == name)
    }

    /// Whether `round` changes it.
    pub fn has_round(self) -> bool {
        matches!(self, Self::Rectangle | Self::Bubble)
    }

    /// Width over height when it is first drawn: the shape's own
    /// proportions (an equilateral triangle, a regular star), or what it
    /// is usually drawn as.
    pub fn natural_ratio(self) -> f64 {
        match self {
            Self::Rectangle => 1.6,
            Self::Circle => 1.0,
            Self::Triangle => 2.0 / 3f64.sqrt(),
            Self::Star => 1.051,
            Self::Heart => 1.1,
            Self::Arrow => 2.0,
            Self::Bubble => 1.3,
        }
    }
}

/// The narrowest and widest a shape may be drawn, width over height.
pub const MIN_SHAPE_RATIO: f64 = 0.1;
pub const MAX_SHAPE_RATIO: f64 = 10.0;
/// The thinnest and thickest outline, in shorter sides of the shape.
pub const MIN_SHAPE_OUTLINE: f64 = 0.005;
pub const MAX_SHAPE_OUTLINE: f64 = 0.25;

/// A shape and how it is painted.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Shape {
    pub kind: ShapeKind,
    /// The inside's colour; `None` leaves it clear, for an outline alone.
    pub fill: Option<Rgb>,
    /// The edge's colour; `None` draws no outline.
    pub outline: Option<Rgb>,
    /// The outline's thickness, [`MIN_SHAPE_OUTLINE`] to
    /// [`MAX_SHAPE_OUTLINE`] of the shape's shorter side. The outline lies
    /// inside the shape's box.
    pub thickness: f64,
    /// 0 to 1, Rectangle and Bubble only: corner radius, as a share of
    /// half the shorter side.
    pub round: f64,
    /// Width over height, [`MIN_SHAPE_RATIO`] to [`MAX_SHAPE_RATIO`].
    pub ratio: f64,
}

impl Default for Shape {
    fn default() -> Self {
        Self::new(ShapeKind::default())
    }
}

impl Shape {
    /// `kind` in white at its natural proportions, without an outline (a
    /// bubble with half-round corners).
    pub fn new(kind: ShapeKind) -> Self {
        Self {
            kind,
            fill: Some(Rgb::WHITE),
            outline: None,
            thickness: 0.05,
            round: if kind == ShapeKind::Bubble { 0.5 } else { 0.0 },
            ratio: kind.natural_ratio(),
        }
    }

    /// The same paint as `kind`: a new kind brings its own proportions and
    /// corners.
    pub fn with_kind(self, kind: ShapeKind) -> Self {
        if kind == self.kind {
            return self;
        }
        let fresh = Self::new(kind);
        Self {
            kind,
            ratio: fresh.ratio,
            round: fresh.round,
            ..self
        }
    }

    /// Whether every number is one the renderer can use and something
    /// shows: a fill, an outline or both.
    pub fn is_valid(&self) -> bool {
        (self.fill.is_some() || self.outline.is_some())
            && (MIN_SHAPE_OUTLINE..=MAX_SHAPE_OUTLINE).contains(&self.thickness)
            && (0.0..=1.0).contains(&self.round)
            && (MIN_SHAPE_RATIO..=MAX_SHAPE_RATIO).contains(&self.ratio)
    }

    /// Each number moved into its range (a non-finite one to its
    /// default). A shape with neither fill nor outline stays that way;
    /// [`Shape::is_valid`] refuses it.
    pub fn clamped(self) -> Self {
        let base = Self::new(self.kind);
        let fit = |v: f64, default: f64, lo: f64, hi: f64| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            }
        };
        Self {
            thickness: fit(
                self.thickness,
                base.thickness,
                MIN_SHAPE_OUTLINE,
                MAX_SHAPE_OUTLINE,
            ),
            round: fit(self.round, base.round, 0.0, 1.0),
            ratio: fit(self.ratio, base.ratio, MIN_SHAPE_RATIO, MAX_SHAPE_RATIO),
            ..self
        }
    }
}

impl Graphic {
    /// The shape, when it is one.
    pub fn shape(&self) -> Option<&Shape> {
        match self {
            Self::Shape(shape) => Some(shape),
            Self::Sticker { .. } => None,
        }
    }
}

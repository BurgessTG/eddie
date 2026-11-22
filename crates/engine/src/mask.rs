//! Masks: a shape on a video clip's picture that keeps what is inside it
//! (or, inverted, what is outside) and clears the rest, so the lanes under
//! it show through. The shape sits on the picture as it shows, cropped and
//! flipped but before it turns, so it moves, scales and turns with the
//! clip.
//!
//! [`Matte`] is the one coverage every renderer reads: export writes it as
//! a grey picture that FFmpeg multiplies into the picture's alpha, and the
//! monitor and the CPU compositor multiply the same coverage in, so a
//! masked picture previews as it exports.

/// The shapes, as CapCut names them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaskShape {
    /// A straight line through the picture; the side above it stays.
    Split,
    /// A band across the picture, `height` tall.
    Filmstrip,
    /// An ellipse `width` by `height`.
    #[default]
    Circle,
    /// A rectangle `width` by `height`, its corners rounded by `round`.
    Rectangle,
    Heart,
    /// A five-pointed star.
    Star,
}

impl MaskShape {
    pub const ALL: [Self; 6] = [
        Self::Split,
        Self::Filmstrip,
        Self::Circle,
        Self::Rectangle,
        Self::Heart,
        Self::Star,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Split => "split",
            Self::Filmstrip => "filmstrip",
            Self::Circle => "circle",
            Self::Rectangle => "rectangle",
            Self::Heart => "heart",
            Self::Star => "star",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Split => "Split",
            Self::Filmstrip => "Filmstrip",
            Self::Circle => "Circle",
            Self::Rectangle => "Rectangle",
            Self::Heart => "Heart",
            Self::Star => "Star",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_lowercase();
        Self::ALL.into_iter().find(|shape| shape.as_str() == name)
    }

    /// Whether `width` sizes it (Split and Filmstrip run across the whole
    /// picture).
    pub fn has_width(self) -> bool {
        !matches!(self, Self::Split | Self::Filmstrip)
    }

    /// Whether `height` sizes it (Split has none).
    pub fn has_height(self) -> bool {
        self != Self::Split
    }
}

/// The smallest and largest a shape may be, in shorter sides of the
/// picture: large enough to cover a 21:9 picture's width twice.
pub const MASK_MIN_SIZE: f64 = 0.01;
pub const MASK_MAX_SIZE: f64 = 5.0;
/// How far the shape's centre may move from the picture's, in picture
/// widths and heights.
pub const MASK_MAX_OFFSET: f64 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Mask {
    pub shape: MaskShape,
    /// The shape's centre from the picture's centre, in picture widths and
    /// heights (right and down are positive).
    pub x: f64,
    pub y: f64,
    /// The shape's size, in shorter sides of the picture, so equal sizes
    /// make a circle on any picture. Filmstrip reads only `height`, its
    /// band; Split neither.
    pub width: f64,
    pub height: f64,
    /// Clockwise degrees, with the picture.
    pub rotation: f64,
    /// 0 to 1: how far the soft edge runs, 1 being half the picture's
    /// shorter side. 0 still smooths the edge over one pixel.
    pub feather: f64,
    /// 0 to 1, Rectangle only: corner radius, as a share of half its
    /// shorter side.
    pub round: f64,
    /// Keep what is outside the shape instead.
    pub invert: bool,
}

impl Default for Mask {
    fn default() -> Self {
        Self::new(MaskShape::Circle)
    }
}

impl Mask {
    /// `shape` in the middle of the picture, 60% of its shorter side.
    pub fn new(shape: MaskShape) -> Self {
        Self {
            shape,
            x: 0.0,
            y: 0.0,
            width: 0.6,
            height: 0.6,
            rotation: 0.0,
            feather: 0.0,
            round: 0.0,
            invert: false,
        }
    }

    /// Whether every number is one the renderers can use.
    pub fn is_valid(&self) -> bool {
        [
            self.x,
            self.y,
            self.width,
            self.height,
            self.rotation,
            self.feather,
            self.round,
        ]
        .iter()
        .all(|v| v.is_finite())
            && self.x.abs() <= MASK_MAX_OFFSET
            && self.y.abs() <= MASK_MAX_OFFSET
            && (MASK_MIN_SIZE..=MASK_MAX_SIZE).contains(&self.width)
            && (MASK_MIN_SIZE..=MASK_MAX_SIZE).contains(&self.height)
            && self.rotation.abs() <= 360.0
            && (0.0..=1.0).contains(&self.feather)
            && (0.0..=1.0).contains(&self.round)
    }

    /// Each number moved into its range (a non-finite one to its default).
    pub fn clamped(self) -> Self {
        let base = Self::new(self.shape);
        let fit = |v: f64, default: f64, lo: f64, hi: f64| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            }
        };
        Self {
            x: fit(self.x, 0.0, -MASK_MAX_OFFSET, MASK_MAX_OFFSET),
            y: fit(self.y, 0.0, -MASK_MAX_OFFSET, MASK_MAX_OFFSET),
            width: fit(self.width, base.width, MASK_MIN_SIZE, MASK_MAX_SIZE),
            height: fit(self.height, base.height, MASK_MIN_SIZE, MASK_MAX_SIZE),
            rotation: fit(self.rotation, 0.0, -360.0, 360.0),
            feather: fit(self.feather, 0.0, 0.0, 1.0),
            round: fit(self.round, 0.0, 0.0, 1.0),
            ..self
        }
    }

    /// The mask laid on a picture `width` by `height` pixels.
    pub fn on(&self, width: f64, height: f64) -> Matte {
        let (width, height) = (width.max(1.0), height.max(1.0));
        let side = width.min(height);
        let (sin, cos) = self.rotation.to_radians().sin_cos();
        Matte {
            shape: self.shape,
            center: ((0.5 + self.x) * width, (0.5 + self.y) * height),
            sin,
            cos,
            half: (self.width * side / 2.0, self.height * side / 2.0),
            soft: (self.feather * side / 2.0).max(1.0),
            round: self.round,
            invert: self.invert,
        }
    }
}

/// A [`Mask`] laid on a picture of a known size in pixels: how much of
/// the picture shows at each point.
#[derive(Clone, Copy, Debug)]
pub struct Matte {
    shape: MaskShape,
    /// The shape's centre, in pixels from the picture's top-left corner.
    center: (f64, f64),
    sin: f64,
    cos: f64,
    /// Half the shape's width and height, in pixels.
    half: (f64, f64),
    /// The soft edge's width in pixels, at least one.
    soft: f64,
    round: f64,
    invert: bool,
}

/// Fewer pixels than this are laid on one thread.
const THREADED: usize = 120_000;

/// The heart's box in [`heart`]'s own units: half its width, half its
/// height.
const HEART_HALF: (f64, f64) = (0.25 + std::f64::consts::SQRT_2 / 4.0, 0.551_776_695_3);
/// The star's box in [`star`]'s own units, and its centre's height there.
const STAR_HALF: (f64, f64) = (0.951_056_516_3, 0.904_508_497_2);
const STAR_MIDDLE: f64 = 0.095_491_502_8;
/// The star's inner corners, as a share of its points' radius.
const STAR_INNER: f64 = 0.4;

impl Matte {
    /// How much of the picture shows at `(x, y)` pixels from its top-left
    /// corner: 0 cleared to 1 kept.
    pub fn cover(&self, x: f64, y: f64) -> f32 {
        let (dx, dy) = (x - self.center.0, y - self.center.1);
        // Into the shape's own frame: it turns clockwise with y down.
        let lx = dx * self.cos + dy * self.sin;
        let ly = dy * self.cos - dx * self.sin;
        let (a, b) = self.half;
        let distance = match self.shape {
            MaskShape::Split => ly,
            MaskShape::Filmstrip => ly.abs() - b,
            MaskShape::Circle => ellipse(lx, ly, a, b),
            MaskShape::Rectangle => {
                let r = self.round * a.min(b);
                let qx = lx.abs() - a + r;
                let qy = ly.abs() - b + r;
                length(qx.max(0.0), qy.max(0.0)) + qx.max(qy).min(0.0) - r
            }
            MaskShape::Heart => {
                let (sx, sy) = (a / HEART_HALF.0, b / HEART_HALF.1);
                heart(lx / sx, HEART_HALF.1 - ly / sy) * sx.min(sy)
            }
            MaskShape::Star => {
                let (sx, sy) = (a / STAR_HALF.0, b / STAR_HALF.1);
                star(lx / sx, STAR_MIDDLE - ly / sy) * sx.min(sy)
            }
        };
        let t = (0.5 - distance / self.soft).clamp(0.0, 1.0);
        let kept = t * t * (3.0 - 2.0 * t);
        (if self.invert { 1.0 - kept } else { kept }) as f32
    }

    /// The coverage, 0 to 255, of every pixel of a `width` by `height`
    /// frame, row by row, where the picture's top-left corner is at
    /// `origin` pixels of it: export's matte at the picture's own size, or
    /// the monitor's over a decoded frame the picture is a window of.
    /// A large frame's rows are shared among up to eight threads.
    pub fn levels(&self, width: usize, height: usize, origin: (f64, f64)) -> Vec<u8> {
        let mut out = vec![0; width * height];
        if out.is_empty() {
            return out;
        }
        let fill = |first: usize, rows: &mut [u8]| {
            for (offset, line) in rows.chunks_exact_mut(width).enumerate() {
                let y = (first + offset) as f64 + 0.5 - origin.1;
                for (col, level) in line.iter_mut().enumerate() {
                    let x = col as f64 + 0.5 - origin.0;
                    *level = (self.cover(x, y) * 255.0 + 0.5) as u8;
                }
            }
        };
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
        if out.len() < THREADED || threads == 1 {
            fill(0, &mut out);
            return out;
        }
        let per = height.div_ceil(threads);
        std::thread::scope(|scope| {
            for (index, rows) in out.chunks_mut(per * width).enumerate() {
                let fill = &fill;
                scope.spawn(move || fill(index * per, rows));
            }
        });
        out
    }
}

/// An RGBA picture's alpha times `levels` (one per pixel) over 255,
/// rounded as export's `lut2` rounds it.
pub fn mask_alpha(rgba: &mut [u8], levels: &[u8]) {
    for (pixel, &level) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(levels) {
        pixel[3] = multiply(pixel[3], level);
    }
}

/// `a * b / 255` rounded to the nearest level: never a half, since 255 is
/// odd.
#[inline]
pub fn multiply(a: u8, b: u8) -> u8 {
    ((u32::from(a) * u32::from(b) + 127) / 255) as u8
}

/// The length of `(x, y)`. `f64::hypot` is a library call on baseline
/// x86-64 and cost most of a matte.
#[inline]
fn length(x: f64, y: f64) -> f64 {
    (x * x + y * y).sqrt()
}

/// Signed distance from an ellipse with half-axes `a` and `b` (negative
/// inside), close to exact near its edge.
fn ellipse(x: f64, y: f64, a: f64, b: f64) -> f64 {
    let k0 = length(x / a, y / b);
    let k1 = length(x / (a * a), y / (b * b));
    if k1 > 0.0 {
        k0 * (k0 - 1.0) / k1
    } else {
        -a.min(b)
    }
}

/// Signed distance from a heart whose tip is at the origin and whose top
/// is up (y up), [`HEART_HALF`] across; Inigo Quilez's exact form.
fn heart(x: f64, y: f64) -> f64 {
    let x = x.abs();
    if x + y > 1.0 {
        return length(x - 0.25, y - 0.75) - std::f64::consts::SQRT_2 / 4.0;
    }
    let m = 0.5 * (x + y).max(0.0);
    let near = (x * x + (y - 1.0) * (y - 1.0)).min((x - m) * (x - m) + (y - m) * (y - m));
    near.sqrt() * (x - y).signum()
}

/// Signed distance from a five-pointed star whose points reach 1 from the
/// origin, one straight up (y up); Inigo Quilez's exact form.
fn star(x: f64, y: f64) -> f64 {
    const K1: (f64, f64) = (0.809_016_994_375, -0.587_785_252_292);
    const K2: (f64, f64) = (-K1.0, K1.1);
    let (mut x, mut y) = (x.abs(), y);
    for k in [K1, K2] {
        let along = 2.0 * (k.0 * x + k.1 * y).max(0.0);
        x -= along * k.0;
        y -= along * k.1;
    }
    let x = x.abs();
    let y = y - 1.0;
    let ba = (STAR_INNER * -K1.1, STAR_INNER * K1.0 - 1.0);
    let h = ((x * ba.0 + y * ba.1) / (ba.0 * ba.0 + ba.1 * ba.1)).clamp(0.0, 1.0);
    length(x - ba.0 * h, y - ba.1 * h) * (y * ba.0 - x * ba.1).signum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coverage at a point of a 200 by 100 picture.
    fn at(mask: Mask, x: f64, y: f64) -> f32 {
        mask.on(200.0, 100.0).cover(x, y)
    }

    #[test]
    fn each_shape_keeps_its_inside_and_clears_its_outside() {
        // 200x100: the shorter side is 100, so 0.6 is 60 pixels.
        let circle = Mask::new(MaskShape::Circle);
        assert_eq!(at(circle, 100.0, 50.0), 1.0);
        assert_eq!(at(circle, 100.0 + 29.0, 50.0), 1.0);
        assert_eq!(at(circle, 100.0 + 31.0, 50.0), 0.0);
        // Equal sizes are round on a wide picture: 31 pixels up is out too.
        assert_eq!(at(circle, 100.0, 50.0 - 31.0), 0.0);
        assert_eq!(at(circle, 100.0, 50.0 - 29.0), 1.0);
        // On the edge, half shows.
        assert!((at(circle, 130.0, 50.0) - 0.5).abs() < 0.01);

        let rect = Mask {
            width: 1.0,
            height: 0.4,
            ..Mask::new(MaskShape::Rectangle)
        };
        assert_eq!(at(rect, 100.0 + 49.0, 50.0 + 19.0), 1.0);
        assert_eq!(at(rect, 100.0 + 51.0, 50.0), 0.0);
        assert_eq!(at(rect, 100.0, 50.0 + 21.0), 0.0);
        // Rounded corners clear the corner itself but not the sides.
        let rounded = Mask { round: 1.0, ..rect };
        assert_eq!(at(rounded, 100.0 + 49.0, 50.0 + 19.0), 0.0);
        assert_eq!(at(rounded, 100.0 + 49.0, 50.0), 1.0);

        // Split keeps the top half; Filmstrip a band `height` tall.
        let split = Mask::new(MaskShape::Split);
        assert_eq!(at(split, 10.0, 40.0), 1.0);
        assert_eq!(at(split, 190.0, 60.0), 0.0);
        let band = Mask {
            height: 0.2,
            ..Mask::new(MaskShape::Filmstrip)
        };
        assert_eq!(at(band, 5.0, 50.0 + 9.0), 1.0);
        assert_eq!(at(band, 5.0, 50.0 + 11.0), 0.0);
        assert_eq!(at(band, 5.0, 50.0 - 11.0), 0.0);

        // A heart is a heart the right way up: its tip at the bottom, the
        // notch between its lobes at the top, its lobes at the sides.
        let heart = Mask::new(MaskShape::Heart);
        assert_eq!(at(heart, 100.0, 50.0 + 24.0), 1.0, "tip");
        assert_eq!(at(heart, 100.0 + 20.0, 50.0 + 25.0), 0.0, "beside the tip");
        assert_eq!(at(heart, 100.0, 50.0 - 28.0), 0.0, "notch");
        assert_eq!(at(heart, 100.0 + 15.0, 50.0 - 25.0), 1.0, "lobe");
        // A star: one point straight up, a gap straight down between two.
        let star = Mask::new(MaskShape::Star);
        assert_eq!(at(star, 100.0, 50.0 - 24.0), 1.0, "top point");
        assert_eq!(
            at(star, 100.0 + 8.0, 50.0 - 22.0),
            0.0,
            "beside the top point"
        );
        assert_eq!(at(star, 100.0, 50.0 + 20.0), 0.0, "between the feet");
        assert_eq!(at(star, 100.0, 50.0 + 5.0), 1.0, "middle");
    }

    #[test]
    fn a_mask_moves_turns_feathers_and_inverts() {
        let moved = Mask {
            x: 0.25,
            y: -0.25,
            width: 0.2,
            height: 0.2,
            ..Mask::new(MaskShape::Rectangle)
        };
        // Its centre is a quarter of the picture right and up: (150, 25).
        assert_eq!(at(moved, 150.0, 25.0), 1.0);
        assert_eq!(at(moved, 100.0, 50.0), 0.0);
        // Turned a quarter clockwise, a wide rectangle stands tall.
        let wide = Mask {
            width: 0.8,
            height: 0.2,
            ..Mask::new(MaskShape::Rectangle)
        };
        assert_eq!(at(wide, 100.0 + 35.0, 50.0), 1.0);
        let turned = Mask {
            rotation: 90.0,
            ..wide
        };
        assert_eq!(at(turned, 100.0 + 35.0, 50.0), 0.0);
        assert_eq!(at(turned, 100.0, 50.0 + 35.0), 1.0);
        // Split turned clockwise a little keeps more of the right side's
        // top: the line goes down to the right.
        let split = Mask {
            rotation: 20.0,
            ..Mask::new(MaskShape::Split)
        };
        assert_eq!(at(split, 190.0, 70.0), 1.0);
        assert_eq!(at(split, 10.0, 30.0), 0.0);

        // A feather of 0.4 runs 20 pixels, centred on the edge.
        let soft = Mask {
            feather: 0.4,
            ..Mask::new(MaskShape::Circle)
        };
        assert_eq!(at(soft, 100.0 + 19.0, 50.0), 1.0);
        assert_eq!(at(soft, 100.0 + 41.0, 50.0), 0.0);
        let inner = at(soft, 100.0 + 25.0, 50.0);
        let outer = at(soft, 100.0 + 35.0, 50.0);
        assert!(inner > 0.6 && inner < 1.0, "{inner}");
        assert!(outer > 0.0 && outer < 0.4, "{outer}");
        // Inverted, the outside stays.
        let out = Mask {
            invert: true,
            ..Mask::new(MaskShape::Circle)
        };
        assert_eq!(at(out, 100.0, 50.0), 0.0);
        assert_eq!(at(out, 5.0, 5.0), 1.0);
    }

    #[test]
    fn levels_sample_pixel_centres_and_alpha_multiplies_as_ffmpeg_does() {
        // A 4x2 window whose picture starts one pixel in: the split's top
        // half is the frame's top row.
        let levels = Mask::new(MaskShape::Split)
            .on(2.0, 2.0)
            .levels(4, 2, (1.0, 0.0));
        assert_eq!(levels, vec![255, 255, 255, 255, 0, 0, 0, 0]);
        // A frame large enough to be laid on several threads gives each
        // pixel its own coverage, rows in order.
        let star = Mask {
            rotation: 30.0,
            feather: 0.1,
            ..Mask::new(MaskShape::Star)
        }
        .on(600.0, 300.0);
        let (w, h) = (701, 301);
        let levels = star.levels(w, h, (50.0, 1.0));
        for (index, &level) in levels.iter().enumerate() {
            let (x, y) = ((index % w) as f64 - 49.5, (index / w) as f64 - 0.5);
            assert_eq!(level, (star.cover(x, y) * 255.0 + 0.5) as u8, "{x} {y}");
        }
        // lut2's round(x*y/255), for every pair.
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                let exact = (f64::from(a) * f64::from(b) / 255.0).round();
                assert_eq!(f64::from(multiply(a, b)), exact, "{a} {b}");
            }
        }
        let mut rgba = vec![9, 9, 9, 200, 9, 9, 9, 255];
        mask_alpha(&mut rgba, &[128, 0]);
        assert_eq!(rgba, vec![9, 9, 9, multiply(200, 128), 9, 9, 9, 0]);
    }

    #[test]
    fn masks_outside_their_ranges_are_refused_and_clamped_in() {
        assert!(Mask::default().is_valid());
        let wild = Mask {
            x: 3.0,
            width: 0.0,
            feather: f64::NAN,
            round: 2.0,
            ..Mask::new(MaskShape::Rectangle)
        };
        assert!(!wild.is_valid());
        let fixed = wild.clamped();
        assert!(fixed.is_valid());
        assert_eq!(
            (fixed.x, fixed.width, fixed.feather, fixed.round),
            (1.0, MASK_MIN_SIZE, 0.0, 1.0)
        );
    }
}

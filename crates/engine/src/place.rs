//! Where a clip's picture lands on the canvas. Preview and export both
//! read [`place`], so a moved, scaled, turned or cropped picture previews
//! where it exports.

/// A clip's picture on the canvas. The default fits the whole picture
/// inside the canvas, centred, as export always has: bars fill the rest.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Transform {
    /// Size against the fitted picture: 1 fits, 2 is twice that.
    pub scale: f64,
    /// Centre offset from the canvas centre, in canvas widths and heights
    /// (right and down are positive).
    pub x: f64,
    pub y: f64,
    /// Clockwise degrees about the picture's centre.
    pub rotation: f64,
    pub flip_h: bool,
    pub flip_v: bool,
    /// Cut from each edge of the source picture, as a share of its width
    /// or height. What is left fits the canvas.
    pub crop_left: f64,
    pub crop_top: f64,
    pub crop_right: f64,
    pub crop_bottom: f64,
}

/// Smallest picture a transform may leave: a crop keeps 5% of each side.
pub const MIN_CROP_KEEP: f64 = 0.05;
pub const MIN_SCALE: f64 = 0.05;
pub const MAX_SCALE: f64 = 20.0;

impl Default for Transform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            crop_left: 0.0,
            crop_top: 0.0,
            crop_right: 0.0,
            crop_bottom: 0.0,
        }
    }
}

impl Transform {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// Whether every number is one the renderers can use: finite, a scale
    /// in range, and a crop that keeps some picture.
    pub fn is_valid(&self) -> bool {
        let crops = [
            self.crop_left,
            self.crop_top,
            self.crop_right,
            self.crop_bottom,
        ];
        [self.scale, self.x, self.y, self.rotation]
            .iter()
            .chain(&crops)
            .all(|v| v.is_finite())
            && (MIN_SCALE..=MAX_SCALE).contains(&self.scale)
            && self.x.abs() <= 10.0
            && self.y.abs() <= 10.0
            && crops.iter().all(|c| (0.0..1.0).contains(c))
            && self.crop_left + self.crop_right <= 1.0 - MIN_CROP_KEEP
            && self.crop_top + self.crop_bottom <= 1.0 - MIN_CROP_KEEP
    }

    pub fn has_crop(&self) -> bool {
        self.crop_left > 0.0
            || self.crop_top > 0.0
            || self.crop_right > 0.0
            || self.crop_bottom > 0.0
    }
}

/// What shows where a picture does not cover the canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Background {
    #[default]
    Black,
    /// The same picture, filling the canvas and blurred.
    Blur,
}

impl Background {
    pub const ALL: [Self; 2] = [Self::Black, Self::Blur];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Black => "black",
            Self::Blur => "blur",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Black => "Black",
            Self::Blur => "Blurred picture",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|b| b.as_str() == name.trim().to_lowercase())
    }
}

/// A picture on the canvas, in canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// The part of the source shown, in source pixels: x, y, width, height.
    pub crop: [f64; 4],
    /// The picture's size on the canvas before it turns.
    pub size: [f64; 2],
    /// Its centre on the canvas.
    pub center: [f64; 2],
    /// Clockwise degrees.
    pub rotation: f64,
    pub flip_h: bool,
    pub flip_v: bool,
}

impl Placement {
    /// The top-left corner of the unturned picture.
    pub fn origin(&self) -> [f64; 2] {
        [
            self.center[0] - self.size[0] / 2.0,
            self.center[1] - self.size[1] / 2.0,
        ]
    }

    /// The upright box the turned picture fills: its width and height.
    pub fn turned_size(&self) -> [f64; 2] {
        let (sin, cos) = self.rotation.to_radians().sin_cos();
        let [w, h] = self.size;
        [w * cos.abs() + h * sin.abs(), w * sin.abs() + h * cos.abs()]
    }

    /// Whether the picture covers every canvas pixel, so no background
    /// shows. Only an unturned picture is checked exactly; a turned one is
    /// treated as leaving corners.
    pub fn covers(&self, canvas: (f64, f64)) -> bool {
        let quarter = (self.rotation / 90.0).round() * 90.0;
        if (self.rotation - quarter).abs() > 1e-9 {
            return false;
        }
        let [w, h] = self.turned_size();
        let (x, y) = (self.center[0] - w / 2.0, self.center[1] - h / 2.0);
        x <= 0.5 && y <= 0.5 && x + w >= canvas.0 - 0.5 && y + h >= canvas.1 - 0.5
    }

    /// Whether canvas point `point` is on the picture, turned as it is.
    pub fn contains(&self, point: (f64, f64)) -> bool {
        let (sin, cos) = (-self.rotation.to_radians()).sin_cos();
        let (dx, dy) = (point.0 - self.center[0], point.1 - self.center[1]);
        let (x, y) = (dx * cos - dy * sin, dx * sin + dy * cos);
        x.abs() <= self.size[0] / 2.0 && y.abs() <= self.size[1] / 2.0
    }

    /// The source pixel (unflipped, in the file's displayed size) under
    /// canvas point `point`, or `None` off the picture.
    pub fn source_point(&self, point: (f64, f64)) -> Option<(f64, f64)> {
        if !self.contains(point) {
            return None;
        }
        let (sin, cos) = (-self.rotation.to_radians()).sin_cos();
        let (dx, dy) = (point.0 - self.center[0], point.1 - self.center[1]);
        let (x, y) = (dx * cos - dy * sin, dx * sin + dy * cos);
        let mut u = (x / self.size[0] + 0.5).clamp(0.0, 1.0);
        let mut v = (y / self.size[1] + 0.5).clamp(0.0, 1.0);
        if self.flip_h {
            u = 1.0 - u;
        }
        if self.flip_v {
            v = 1.0 - v;
        }
        let [cx, cy, cw, ch] = self.crop;
        Some((cx + u * cw, cy + v * ch))
    }

    /// Whether any of the picture lands on the canvas.
    pub fn on_canvas(&self, canvas: (f64, f64)) -> bool {
        let [w, h] = self.turned_size();
        let (x, y) = (self.center[0] - w / 2.0, self.center[1] - h / 2.0);
        x < canvas.0 && y < canvas.1 && x + w > 0.0 && y + h > 0.0
    }
}

/// Where a `source`-sized picture (displayed size, rotation applied) lands
/// on a `canvas`-sized frame under `t`.
pub fn place(canvas: (f64, f64), source: (f64, f64), t: &Transform) -> Placement {
    let (sw, sh) = (source.0.max(1.0), source.1.max(1.0));
    let crop_w = sw * (1.0 - t.crop_left - t.crop_right).max(MIN_CROP_KEEP);
    let crop_h = sh * (1.0 - t.crop_top - t.crop_bottom).max(MIN_CROP_KEEP);
    let fit = (canvas.0 / crop_w).min(canvas.1 / crop_h);
    let scale = fit * t.scale;
    Placement {
        crop: [sw * t.crop_left, sh * t.crop_top, crop_w, crop_h],
        size: [crop_w * scale, crop_h * scale],
        center: [
            canvas.0 / 2.0 + t.x * canvas.0,
            canvas.1 / 2.0 + t.y * canvas.1,
        ],
        rotation: t.rotation,
        flip_h: t.flip_h,
        flip_v: t.flip_v,
    }
}

/// The scale that makes a picture fill the canvas edge to edge (cropping
/// the overflow), for a "Fill" button. A picture turned a quarter covers
/// with its sides swapped; other turns are taken as upright.
pub fn fill_scale(canvas: (f64, f64), source: (f64, f64), t: &Transform) -> f64 {
    let crop_w = source.0.max(1.0) * (1.0 - t.crop_left - t.crop_right).max(MIN_CROP_KEEP);
    let crop_h = source.1.max(1.0) * (1.0 - t.crop_top - t.crop_bottom).max(MIN_CROP_KEEP);
    let fit = (canvas.0 / crop_w).min(canvas.1 / crop_h);
    let sideways = (t.rotation / 90.0).round().rem_euclid(2.0) == 1.0;
    let (across, down) = if sideways {
        (crop_h, crop_w)
    } else {
        (crop_w, crop_h)
    };
    (canvas.0 / across).max(canvas.1 / down) / fit
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: (f64, f64) = (1920.0, 1080.0);

    #[test]
    fn the_default_fits_the_picture_inside_the_canvas() {
        // 16:9 fills 16:9 exactly.
        let p = place(HD, (3840.0, 2160.0), &Transform::default());
        assert_eq!(p.size, [1920.0, 1080.0]);
        assert_eq!(p.center, [960.0, 540.0]);
        assert!(p.covers(HD));
        // Portrait footage in a landscape canvas is pillarboxed.
        let p = place(HD, (1080.0, 1920.0), &Transform::default());
        assert!((p.size[0] - 607.5).abs() < 1e-9 && p.size[1] == 1080.0);
        assert_eq!(p.origin(), [656.25, 0.0]);
        assert!(!p.covers(HD));
    }

    /// A canvas point finds the source pixel under it through crop, flip
    /// and turn, which the monitor's colour picker reads.
    #[test]
    fn a_canvas_point_finds_its_source_pixel() {
        let canvas = (400.0, 200.0);
        let source = (200.0, 100.0);
        // The right half of the source, fitted 200 px square in the middle.
        let half = Transform {
            crop_left: 0.5,
            ..Transform::default()
        };
        let at = |t: Transform, point| place(canvas, source, &t).source_point(point);
        let near = |got: Option<(f64, f64)>, want: (f64, f64)| {
            got.is_some_and(|(x, y)| (x - want.0).abs() < 1e-9 && (y - want.1).abs() < 1e-9)
        };
        assert!(near(at(half, (101.0, 100.0)), (100.5, 50.0)));
        assert_eq!(at(half, (99.0, 100.0)), None, "off the picture");
        // Mirrored, the left edge shows the source's right edge.
        let mirrored = Transform {
            flip_h: true,
            ..half
        };
        assert!(near(at(mirrored, (101.0, 100.0)), (199.5, 50.0)));
        // Turned a quarter clockwise, its top edge faces right.
        let turned = Transform {
            rotation: 90.0,
            ..half
        };
        assert!(near(at(turned, (299.0, 100.0)), (150.0, 0.5)));
    }

    #[test]
    fn crop_fits_what_is_left_and_fill_covers_the_canvas() {
        // Cutting the left and right quarters off 16:9 leaves 8:9, which
        // fits by height.
        let t = Transform {
            crop_left: 0.25,
            crop_right: 0.25,
            ..Transform::default()
        };
        let p = place(HD, (1920.0, 1080.0), &t);
        assert_eq!(p.crop, [480.0, 0.0, 960.0, 1080.0]);
        assert_eq!(p.size, [960.0, 1080.0]);
        // Fill scales that up until it covers 16:9 by width: 1920 / 960.
        let k = fill_scale(HD, (1920.0, 1080.0), &t);
        assert!((k - 2.0).abs() < 1e-12);
        let p = place(HD, (1920.0, 1080.0), &Transform { scale: k, ..t });
        assert!(p.covers(HD));
        // Turned a quarter, 16:9 covers 16:9 only when its 1080 side spans
        // 1920: 1920 / 1080 times the fitted size.
        let turned = Transform {
            rotation: 90.0,
            ..Transform::default()
        };
        let k = fill_scale(HD, (1920.0, 1080.0), &turned);
        assert!((k - 1920.0 / 1080.0).abs() < 1e-12, "{k}");
        assert!(place(HD, (1920.0, 1080.0), &Transform { scale: k, ..turned }).covers(HD));
    }

    #[test]
    fn position_is_a_share_of_the_canvas_and_rotation_widens_the_box() {
        let t = Transform {
            scale: 0.5,
            x: 0.25,
            y: -0.25,
            rotation: 90.0,
            ..Transform::default()
        };
        let p = place(HD, (1920.0, 1080.0), &t);
        assert_eq!(p.center, [1440.0, 270.0]);
        assert_eq!(p.size, [960.0, 540.0]);
        let [w, h] = p.turned_size();
        assert!((w - 540.0).abs() < 1e-9 && (h - 960.0).abs() < 1e-9);
        // Stood on end, it reaches 480 up and down from its centre but
        // only 270 to the sides.
        assert!(p.contains((1440.0, 270.0 + 470.0)));
        assert!(!p.contains((1440.0 + 280.0, 270.0)));
        assert!(
            !place(HD, (1920.0, 1080.0), &Transform { rotation: 0.0, ..t })
                .contains((1440.0, 270.0 + 470.0))
        );
        let gone = Transform {
            x: 2.0,
            ..Transform::default()
        };
        assert!(!place(HD, (1920.0, 1080.0), &gone).on_canvas(HD));
    }

    #[test]
    fn transforms_outside_what_renderers_take_are_invalid() {
        assert!(Transform::default().is_valid());
        for bad in [
            Transform {
                scale: 0.0,
                ..Transform::default()
            },
            Transform {
                scale: f64::NAN,
                ..Transform::default()
            },
            Transform {
                crop_left: 0.5,
                crop_right: 0.5,
                ..Transform::default()
            },
            Transform {
                crop_top: -0.1,
                ..Transform::default()
            },
            Transform {
                x: f64::INFINITY,
                ..Transform::default()
            },
        ] {
            assert!(!bad.is_valid(), "{bad:?}");
        }
    }
}

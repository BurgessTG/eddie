//! A clip's colour: six adjustments and a preset filter. Everything but
//! brightness is one affine map of gamma-encoded RGB ([`ColorAdjust::matrix`]);
//! brightness is a power curve after it ([`ColorAdjust::gamma`]). The
//! preview applies both with [`ColorAdjust::apply_rgba`]; export builds the
//! same maths from FFmpeg filters, so a graded picture previews as it
//! exports.

/// A preset look, mixed in by [`ColorAdjust::strength`] before the
/// adjustments apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Filter {
    #[default]
    None,
    Vivid,
    Dramatic,
    Warm,
    Cool,
    Fade,
    Vintage,
    Sepia,
    Mono,
    Noir,
}

impl Filter {
    pub const ALL: [Self; 10] = [
        Self::None,
        Self::Vivid,
        Self::Dramatic,
        Self::Warm,
        Self::Cool,
        Self::Fade,
        Self::Vintage,
        Self::Sepia,
        Self::Mono,
        Self::Noir,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Vivid => "vivid",
            Self::Dramatic => "dramatic",
            Self::Warm => "warm",
            Self::Cool => "cool",
            Self::Fade => "fade",
            Self::Vintage => "vintage",
            Self::Sepia => "sepia",
            Self::Mono => "mono",
            Self::Noir => "noir",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Vivid => "Vivid",
            Self::Dramatic => "Dramatic",
            Self::Warm => "Warm",
            Self::Cool => "Cool",
            Self::Fade => "Fade",
            Self::Vintage => "Vintage",
            Self::Sepia => "Sepia",
            Self::Mono => "Mono",
            Self::Noir => "Noir",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_lowercase();
        Self::ALL.into_iter().find(|f| f.as_str() == name)
    }

    /// The look at full strength.
    fn matrix(self) -> Affine {
        let sepia = || {
            // Luma toned brown: white lands on (1, 0.87, 0.66), black on
            // (0.10, 0.07, 0.04).
            let tone = [(0.90, 0.10), (0.80, 0.07), (0.62, 0.04)];
            tone.map(|(k, o)| [k * LUMA[0], k * LUMA[1], k * LUMA[2], o])
        };
        let fade = || then(then(saturation(0.8), contrast(0.75)), offset(0.03));
        match self {
            Self::None => IDENTITY,
            Self::Vivid => then(saturation(1.35), contrast(1.12)),
            Self::Dramatic => then(saturation(0.75), contrast(1.35)),
            Self::Warm => then(gains(1.12, 1.0, 0.86), saturation(1.05)),
            Self::Cool => gains(0.88, 1.0, 1.12),
            Self::Fade => fade(),
            Self::Vintage => then(then(mix(sepia(), 0.35), fade()), gains(1.04, 1.0, 0.94)),
            Self::Sepia => sepia(),
            Self::Mono => saturation(0.0),
            Self::Noir => then(then(saturation(0.0), contrast(1.45)), offset(-0.03)),
        }
    }
}

/// A clip's colour. Zero adjustments and no filter leave the picture
/// untouched.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ColorAdjust {
    /// Midtones lighter or darker, -1 to 1: a power curve that keeps black
    /// and white where they are.
    pub brightness: f64,
    /// -1 to 1: halves or doubles the distance from mid-grey.
    pub contrast: f64,
    /// -1 to 1: -1 is greyscale, 1 doubles colour.
    pub saturation: f64,
    /// Stops of light, -2 to 2.
    pub exposure: f64,
    /// -1 (cooler, bluer) to 1 (warmer, more orange).
    pub temperature: f64,
    /// -1 (greener) to 1 (more magenta).
    pub tint: f64,
    pub filter: Filter,
    /// How much of the filter to apply, 0 to 1.
    pub strength: f64,
}

impl Default for ColorAdjust {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 0.0,
            saturation: 0.0,
            exposure: 0.0,
            temperature: 0.0,
            tint: 0.0,
            filter: Filter::None,
            strength: 1.0,
        }
    }
}

/// Stops of exposure each way.
pub const MAX_EXPOSURE: f64 = 2.0;

/// BT.709 luma weights, as export's matrix uses.
const LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// Gamma of the encoded picture: a stop of light multiplies encoded values
/// by 2^(1/2.2).
const ENCODING_GAMMA: f64 = 2.2;

/// Share each channel gains or loses at full temperature or tint.
const WHITE_BALANCE: f64 = 0.2;

impl ColorAdjust {
    /// Whether this changes no pixel.
    /// The value of the adjustment `property` names, for a colour property.
    pub fn value(mut self, property: crate::Property) -> Option<f64> {
        self.field_mut(property).map(|value| *value)
    }

    /// The adjustment `property` names, for a colour property.
    pub fn field_mut(&mut self, property: crate::Property) -> Option<&mut f64> {
        use crate::Property as P;
        Some(match property {
            P::Brightness => &mut self.brightness,
            P::Contrast => &mut self.contrast,
            P::Saturation => &mut self.saturation,
            P::Exposure => &mut self.exposure,
            P::Temperature => &mut self.temperature,
            P::Tint => &mut self.tint,
            P::FilterStrength => &mut self.strength,
            _ => return None,
        })
    }

    pub fn is_identity(&self) -> bool {
        self.adjustments().iter().all(|&v| v == 0.0)
            && (self.filter == Filter::None || self.strength == 0.0)
    }

    fn adjustments(&self) -> [f64; 6] {
        [
            self.brightness,
            self.contrast,
            self.saturation,
            self.exposure,
            self.temperature,
            self.tint,
        ]
    }

    /// Whether every number is finite and in its range.
    pub fn is_valid(&self) -> bool {
        let [brightness, contrast, saturation, exposure, temperature, tint] = self.adjustments();
        [brightness, contrast, saturation, temperature, tint]
            .iter()
            .all(|v| (-1.0..=1.0).contains(v))
            && (-MAX_EXPOSURE..=MAX_EXPOSURE).contains(&exposure)
            && (0.0..=1.0).contains(&self.strength)
    }

    /// Every value moved into its range; a value that is not a number
    /// becomes the default.
    pub fn clamped(mut self) -> Self {
        let fix = |v: f64, max: f64| if v.is_nan() { 0.0 } else { v.clamp(-max, max) };
        self.brightness = fix(self.brightness, 1.0);
        self.contrast = fix(self.contrast, 1.0);
        self.saturation = fix(self.saturation, 1.0);
        self.exposure = fix(self.exposure, MAX_EXPOSURE);
        self.temperature = fix(self.temperature, 1.0);
        self.tint = fix(self.tint, 1.0);
        self.strength = if self.strength.is_nan() {
            1.0
        } else {
            self.strength.clamp(0.0, 1.0)
        };
        self
    }

    /// The affine part, on RGB from 0 to 1: row `i` gives channel `i` as
    /// `r * m[i][0] + g * m[i][1] + b * m[i][2] + m[i][3]`, clamped to 0..1
    /// afterwards. In order: the filter, exposure, temperature and tint,
    /// saturation, then contrast about mid-grey.
    pub fn matrix(&self) -> [[f64; 4]; 3] {
        let exposure = 2f64.powf(self.exposure / ENCODING_GAMMA);
        let warm = self.temperature * WHITE_BALANCE;
        let steps = [
            mix(self.filter.matrix(), self.strength),
            gains(
                exposure * (1.0 + warm),
                exposure * (1.0 - self.tint * WHITE_BALANCE),
                exposure * (1.0 - warm),
            ),
            saturation(1.0 + self.saturation),
            contrast(2f64.powf(self.contrast)),
        ];
        steps.into_iter().fold(IDENTITY, then)
    }

    /// The brightness curve's power, applied to each 8-bit channel after
    /// the matrix: `round(255 * (v / 255)^gamma)`. 1 leaves it alone.
    pub fn gamma(&self) -> f64 {
        2f64.powf(-self.brightness)
    }

    /// The brightness curve for every 8-bit value, written as export's
    /// `lutrgb` expression computes it, `round(255*exp(gamma*log(v/255)))`,
    /// so the two agree on every value.
    pub fn curve(&self) -> [u8; 256] {
        let gamma = self.gamma();
        std::array::from_fn(|v| (255.0 * (gamma * (v as f64 / 255.0).ln()).exp()).round() as u8)
    }

    /// Grade RGBA pixels in place (alpha untouched): the matrix in 14-bit
    /// fixed point, rounded and clamped to 8 bits, then the curve.
    pub fn apply_rgba(&self, rgba: &mut [u8]) {
        if self.is_identity() {
            return;
        }
        const SHIFT: u32 = 14;
        let one = f64::from(1u32 << SHIFT);
        let m = self.matrix().map(|row| {
            [
                (row[0] * one).round() as i32,
                (row[1] * one).round() as i32,
                (row[2] * one).round() as i32,
                (row[3] * 255.0 * one + one / 2.0).round() as i32,
            ]
        });
        let curve = (self.brightness != 0.0).then(|| self.curve());
        for px in rgba.as_chunks_mut::<4>().0 {
            let [r, g, b, _] = px.map(i32::from);
            for (c, k) in m.iter().enumerate() {
                let v = (k[0] * r + k[1] * g + k[2] * b + k[3]) >> SHIFT;
                px[c] = v.clamp(0, 255) as u8;
            }
            if let Some(curve) = &curve {
                for c in 0..3 {
                    px[c] = curve[px[c] as usize];
                }
            }
        }
    }
}

/// An affine map of RGB: three rows of r, g, b gains and an offset.
type Affine = [[f64; 4]; 3];

const IDENTITY: Affine = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
];

/// `first`, then `next`.
fn then(first: Affine, next: Affine) -> Affine {
    std::array::from_fn(|i| {
        let mut row: [f64; 4] =
            std::array::from_fn(|j| (0..3).map(|k| next[i][k] * first[k][j]).sum());
        row[3] += next[i][3];
        row
    })
}

fn gains(r: f64, g: f64, b: f64) -> Affine {
    [[r, 0.0, 0.0, 0.0], [0.0, g, 0.0, 0.0], [0.0, 0.0, b, 0.0]]
}

/// Each channel's distance from luma times `factor`.
fn saturation(factor: f64) -> Affine {
    std::array::from_fn(|i| {
        let mut row = [0.0; 4];
        for (j, w) in LUMA.iter().enumerate() {
            row[j] = (1.0 - factor) * w + if i == j { factor } else { 0.0 };
        }
        row
    })
}

/// Distance from mid-grey times `factor`.
fn contrast(factor: f64) -> Affine {
    let o = 0.5 * (1.0 - factor);
    [
        [factor, 0.0, 0.0, o],
        [0.0, factor, 0.0, o],
        [0.0, 0.0, factor, o],
    ]
}

fn offset(o: f64) -> Affine {
    [[1.0, 0.0, 0.0, o], [0.0, 1.0, 0.0, o], [0.0, 0.0, 1.0, o]]
}

/// `strength` of the way from no change to `look`.
fn mix(look: Affine, strength: f64) -> Affine {
    std::array::from_fn(|i| {
        std::array::from_fn(|j| IDENTITY[i][j] + strength * (look[i][j] - IDENTITY[i][j]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graded(color: &ColorAdjust, rgb: [u8; 3]) -> [u8; 3] {
        let mut px = [rgb[0], rgb[1], rgb[2], 255];
        color.apply_rgba(&mut px);
        assert_eq!(px[3], 255, "alpha stays");
        [px[0], px[1], px[2]]
    }

    fn near(got: [u8; 3], want: [f64; 3]) {
        for (g, w) in got.iter().zip(want) {
            let w = w.clamp(0.0, 1.0) * 255.0;
            assert!((f64::from(*g) - w).abs() <= 0.6, "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn each_adjustment_does_what_its_name_says() {
        let grey = [128u8; 3];
        let g = 128.0 / 255.0;
        let saturated = [200u8, 60, 30];
        let (r, gr, b) = (200.0 / 255.0, 60.0 / 255.0, 30.0 / 255.0);
        let set = |f: fn(&mut ColorAdjust)| {
            let mut c = ColorAdjust::default();
            f(&mut c);
            c
        };
        // Contrast 1 doubles the distance from mid-grey.
        let c = set(|c| c.contrast = 1.0);
        near(
            graded(&c, [64, 128, 192]),
            [
                64.0 / 255.0 * 2.0 - 0.5,
                g * 2.0 - 0.5,
                192.0 / 255.0 * 2.0 - 0.5,
            ],
        );
        // Contrast -1 halves it.
        let c = set(|c| c.contrast = -1.0);
        near(graded(&c, [0, 255, 0]), [0.25, 0.75, 0.25]);
        // Saturation -1 is BT.709 luma in every channel; grey stays grey.
        let y = 0.2126 * r + 0.7152 * gr + 0.0722 * b;
        let c = set(|c| c.saturation = -1.0);
        near(graded(&c, saturated), [y, y, y]);
        near(graded(&c, grey), [g, g, g]);
        // Saturation 0.5 moves each channel half as far again from luma.
        let c = set(|c| c.saturation = 0.5);
        near(
            graded(&c, saturated),
            [
                y + 1.5 * (r - y),
                y + 1.5 * (gr - y),
                (y + 1.5 * (b - y)).max(0.0),
            ],
        );
        // One stop of exposure is 2^(1/2.2) on encoded values.
        let c = set(|c| c.exposure = 1.0);
        let k = 2f64.powf(1.0 / 2.2);
        near(
            graded(&c, [100, 50, 10]),
            [100.0 / 255.0 * k, 50.0 / 255.0 * k, 10.0 / 255.0 * k],
        );
        // Warm raises red and lowers blue by a fifth; magenta lowers green.
        let c = set(|c| c.temperature = 1.0);
        near(graded(&c, grey), [g * 1.2, g, g * 0.8]);
        let c = set(|c| c.tint = -0.5);
        near(graded(&c, grey), [g, g * 1.1, g]);
        // Brightness bends midtones and keeps black and white.
        let c = set(|c| c.brightness = 1.0);
        near(graded(&c, grey), [g.sqrt(), g.sqrt(), g.sqrt()]);
        assert_eq!(graded(&c, [0, 255, 0]), [0, 255, 0]);
        let c = set(|c| c.brightness = -1.0);
        near(graded(&c, grey), [g * g, g * g, g * g]);
    }

    #[test]
    fn adjustments_follow_the_filter_in_a_fixed_order() {
        // Mono at half strength leaves half the colour.
        let c = ColorAdjust {
            filter: Filter::Mono,
            strength: 0.5,
            ..ColorAdjust::default()
        };
        let (r, g, b) = (200.0 / 255.0, 60.0 / 255.0, 30.0 / 255.0);
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        near(
            graded(&c, [200, 60, 30]),
            [(r + y) / 2.0, (g + y) / 2.0, (b + y) / 2.0],
        );
        // Contrast comes after temperature: warm grey 0.6 is 0.72 red,
        // then doubled from mid-grey to 0.94.
        let c = ColorAdjust {
            temperature: 1.0,
            contrast: 1.0,
            ..ColorAdjust::default()
        };
        let grey = 153.0 / 255.0;
        near(
            graded(&c, [153, 153, 153]),
            [
                (grey * 1.2 - 0.5) * 2.0 + 0.5,
                (grey - 0.5) * 2.0 + 0.5,
                (grey * 0.8 - 0.5) * 2.0 + 0.5,
            ],
        );
    }

    #[test]
    fn strong_grades_clamp_once_at_the_end() {
        // Exposure +2 stops then contrast -1: 200 overshoots to 1.88 x 0.78
        // before contrast halves it back. Clamping between them would land
        // on 0.75.
        let c = ColorAdjust {
            exposure: 2.0,
            contrast: -1.0,
            ..ColorAdjust::default()
        };
        let k = 4f64.powf(1.0 / 2.2);
        let v = 200.0 / 255.0 * k;
        near(graded(&c, [200, 200, 200]), [(v - 0.5) / 2.0 + 0.5; 3]);
    }

    #[test]
    fn the_default_is_identity_and_ranges_are_checked() {
        let none = ColorAdjust::default();
        assert!(none.is_identity() && none.is_valid());
        let mut px = [12, 34, 56, 78];
        none.apply_rgba(&mut px);
        assert_eq!(px, [12, 34, 56, 78]);
        // A filter at no strength changes nothing.
        assert!(ColorAdjust {
            filter: Filter::Noir,
            strength: 0.0,
            ..none
        }
        .is_identity());
        for bad in [
            ColorAdjust {
                contrast: 1.5,
                ..none
            },
            ColorAdjust {
                exposure: f64::NAN,
                ..none
            },
            ColorAdjust {
                strength: -0.1,
                ..none
            },
            ColorAdjust {
                tint: f64::INFINITY,
                ..none
            },
        ] {
            assert!(!bad.is_valid(), "{bad:?}");
            assert!(bad.clamped().is_valid(), "{bad:?}");
        }
        assert_eq!(Filter::from_name(" Vintage "), Some(Filter::Vintage));
        assert_eq!(Filter::from_name("lut"), None);
    }

    #[test]
    fn every_filter_changes_the_picture_and_keeps_grey_neutral_unless_it_tones() {
        for filter in Filter::ALL {
            let c = ColorAdjust {
                filter,
                ..ColorAdjust::default()
            };
            let out = graded(&c, [200, 60, 30]);
            assert_eq!(filter == Filter::None, out == [200, 60, 30], "{filter:?}");
            let grey = graded(&c, [128, 128, 128]);
            let toned = matches!(
                filter,
                Filter::Warm | Filter::Cool | Filter::Sepia | Filter::Vintage
            );
            assert_eq!(toned, grey[0] != grey[2], "{filter:?} {grey:?}");
        }
    }
}

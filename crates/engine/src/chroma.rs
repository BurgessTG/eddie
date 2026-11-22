//! Chroma key: the parts of a picture near one colour (a green or blue
//! screen) turn clear, so the lanes under it show through, and the
//! colour's cast on what stays (spill) is taken off.
//!
//! Clearness follows FFmpeg's `chromakey`: each pixel's distance from the
//! key colour in BT.709 chroma (brightness ignored, so shadows on the
//! screen key with it), averaged over the pixel and its eight neighbours.
//! Spill follows `despill`: the key's own channel is pulled down toward
//! the mean of the other two. Export runs those two filters and the
//! preview [`ChromaKey::apply_rgba`], which repeats their arithmetic, so a
//! keyed picture previews as it exports.

use std::cell::RefCell;
use std::rc::Rc;

use crate::Rgb;

/// The usual chroma-key green and blue.
pub const KEY_GREEN: Rgb = Rgb(0, 177, 64);
pub const KEY_BLUE: Rgb = Rgb(0, 71, 187);

/// How far the edge runs at [`ChromaKey::softness`] 1, as a share of the
/// way from the key colour to grey.
const MAX_SOFTNESS: f64 = 0.5;
/// `despill`'s mix: the other two channels count equally.
const SPILL_MIX: f32 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChromaKey {
    /// The screen's colour.
    pub color: Rgb,
    /// 0 to 1: how far from the colour still turns clear, as a share of
    /// the way from it to grey. Measured so, a dull key (a screen picked
    /// in shadow) clears as much of the screen as a vivid one, and greys,
    /// skin and hair stay until near 1.
    pub strength: f64,
    /// 0 to 1: how gradual the edge between clear and kept is.
    pub softness: f64,
    /// 0 to 1: how much of the colour's cast on what stays is taken off.
    pub spill: f64,
}

impl Default for ChromaKey {
    fn default() -> Self {
        Self {
            color: KEY_GREEN,
            strength: 0.6,
            softness: 0.3,
            spill: 0.5,
        }
    }
}

/// Which channel `despill` pulls down for a key colour, and the two it
/// pulls toward: the key's strongest channel (green on a tie, as screens
/// are). Red is keyed as green with the two swapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spill {
    Red,
    Green,
    Blue,
}

impl Spill {
    /// The pulled channel, the one mixed by [`SPILL_MIX`], and the other.
    fn channels(self) -> (usize, usize, usize) {
        match self {
            // despill's green type on the picture with red and green
            // swapped: the "green" it pulls is red, its "red" is green.
            Self::Red => (0, 1, 2),
            Self::Green => (1, 0, 2),
            Self::Blue => (2, 0, 1),
        }
    }
}

/// BT.709 limited-range conversion in 16-bit fixed point: each row's
/// coefficients for R, G and B, and its offset in levels.
struct Matrix([[i32; 3]; 3]);

impl Matrix {
    const OFFSETS: [i32; 3] = [16, 128, 128];

    fn new() -> Self {
        let (kr, kb) = (0.2126, 0.0722);
        let kg = 1.0 - kr - kb;
        let row = |k: [f64; 3], range: f64| k.map(|k| (k * range / 255.0 * 65536.0).round() as i32);
        Self([
            row([kr, kg, kb], 219.0),
            row(
                [-kr / (2.0 * (1.0 - kb)), -kg / (2.0 * (1.0 - kb)), 0.5],
                224.0,
            ),
            row(
                [0.5, -kg / (2.0 * (1.0 - kr)), -kb / (2.0 * (1.0 - kr))],
                224.0,
            ),
        ])
    }

    /// Row `row` of `[r, g, b]`, rounded to the nearest level.
    #[inline]
    fn level(&self, row: usize, [r, g, b]: [i32; 3]) -> u8 {
        let k = self.0[row];
        let sum = k[0] * r + k[1] * g + k[2] * b + (Self::OFFSETS[row] << 16) + (1 << 15);
        (sum >> 16).clamp(0, 255) as u8
    }
}

/// BT.709 limited-range Y, Cb and Cr of an 8-bit RGB colour, the
/// representation export keys in, rounded to the nearest level.
pub fn yuv(Rgb(r, g, b): Rgb) -> (u8, u8, u8) {
    let m = Matrix::new();
    let rgb = [i32::from(r), i32::from(g), i32::from(b)];
    (m.level(0, rgb), m.level(1, rgb), m.level(2, rgb))
}

/// A key's Cb and Cr, and every Cb and Cr's distance from them.
type Distances = ((u8, u8), Rc<[f64]>);

/// Each Cb and Cr's distance from the key's, `[cb * 256 + cr]`, as
/// `chromakey` measures it. Kept per thread for the last key, since the
/// monitor keys every frame with the same one.
fn distances(key: (u8, u8)) -> Rc<[f64]> {
    thread_local! {
        static LAST: RefCell<Option<Distances>> = const { RefCell::new(None) };
    }
    LAST.with_borrow_mut(|last| {
        if let Some((_, table)) = last.as_ref().filter(|(held, _)| *held == key) {
            return Rc::clone(table);
        }
        let (ku, kv) = (i32::from(key.0), i32::from(key.1));
        let table: Rc<[f64]> = (0..256 * 256)
            .map(|i: i32| {
                let (du, dv) = ((i >> 8) - ku, (i & 255) - kv);
                (f64::from(du * du + dv * dv) / (255.0 * 255.0 * 2.0)).sqrt()
            })
            .collect();
        *last = Some((key, Rc::clone(&table)));
        table
    })
}

impl ChromaKey {
    pub fn is_valid(&self) -> bool {
        [self.strength, self.softness, self.spill]
            .iter()
            .all(|v| (0.0..=1.0).contains(v))
    }

    /// Every amount moved into 0 to 1; not a number becomes 0.
    pub fn clamped(mut self) -> Self {
        for v in [&mut self.strength, &mut self.softness, &mut self.spill] {
            *v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        }
        self
    }

    /// The key colour's chroma distance from grey, as `chromakey`
    /// measures distances: 0.22 for [`KEY_GREEN`], 0 for a grey.
    fn grey_distance(&self) -> f64 {
        let (_, u, v) = yuv(self.color);
        let (du, dv) = (f64::from(u) - 128.0, f64::from(v) - 128.0);
        ((du * du + dv * dv) / (255.0 * 255.0 * 2.0)).sqrt()
    }

    /// `chromakey`'s similarity: the averaged chroma distance (0 to 1)
    /// under which a pixel is clear. FFmpeg's least is 0.00001.
    pub fn similarity(&self) -> f32 {
        ((self.strength * self.grey_distance()) as f32).max(0.00001)
    }

    /// `chromakey`'s blend: how much further the distance runs from clear
    /// to kept. Under 0.0001 FFmpeg cuts hard.
    pub fn blend(&self) -> f32 {
        (self.softness * MAX_SOFTNESS * self.grey_distance()) as f32
    }

    /// The channel spill is taken off.
    pub fn spill_kind(&self) -> Spill {
        let Rgb(r, g, b) = self.color;
        if g >= r && g >= b {
            Spill::Green
        } else if b >= r {
            Spill::Blue
        } else {
            Spill::Red
        }
    }

    /// Key `width`-wide RGBA pixels in place: alpha from the colour's
    /// chroma distance averaged over each pixel and its eight neighbours
    /// (replacing the alpha they had, as `chromakey` does), then spill, as
    /// `despill` computes it in 32-bit floats. `chromakey` reads a frame's
    /// edge pixels oddly; this repeats the edge. One pass: three rows of
    /// sums across are kept, and a row is despilled once its own sums are.
    pub fn apply_rgba(&self, rgba: &mut [u8], width: usize) {
        let width = width.max(1);
        let height = rgba.len() / 4 / width;
        if height == 0 {
            return;
        }
        let m = Matrix::new();
        let (_, ku, kv) = yuv(self.color);
        let table = distances((ku, kv));
        let stride = width * 4;
        let mut distance = vec![0.0; width];
        // Row `y`'s distances, each summed with its left and right ones.
        let mut across = |rgba: &[u8], y: usize, out: &mut Vec<f64>| {
            let line = &rgba[y * stride..(y + 1) * stride];
            for (d, p) in distance.iter_mut().zip(line.as_chunks::<4>().0) {
                let rgb = [i32::from(p[0]), i32::from(p[1]), i32::from(p[2])];
                *d = table[usize::from(m.level(1, rgb)) << 8 | usize::from(m.level(2, rgb))];
            }
            // The edges repeat their own pixel.
            let last = width - 1;
            out.clear();
            out.push(2.0 * distance[0] + distance[1.min(last)]);
            out.extend(distance.windows(3).map(|w| w[0] + w[1] + w[2]));
            if width > 1 {
                out.push(distance[last - 1] + 2.0 * distance[last]);
            }
        };
        // chromakey's alpha, ((sum / 9 - similarity) / blend) * 255, as one
        // multiply of the sum: it can round a hair differently, and so be a
        // level apart where the truncation falls on a boundary.
        let similarity = f64::from(self.similarity());
        let blend = f64::from(self.blend());
        let soft = blend > 0.0001;
        let (cut, gain) = (9.0 * similarity, 255.0 / (9.0 * blend));
        let spill = (self.spill > 0.0).then(|| Despill::new(self.spill_kind(), self.spill as f32));
        let (mut above, mut row, mut below) = (Vec::new(), Vec::new(), Vec::new());
        across(rgba, 0, &mut row);
        above.clone_from(&row);
        across(rgba, 1.min(height - 1), &mut below);
        for y in 0..height {
            let line = &mut rgba[y * stride..(y + 1) * stride];
            let sums = above.iter().zip(&row).zip(&below);
            for (px, ((a, r), b)) in line.as_chunks_mut::<4>().0.iter_mut().zip(sums) {
                let over = a + r + b - cut;
                px[3] = if soft {
                    (over * gain).clamp(0.0, 255.0) as u8
                } else if over > 0.0 {
                    255
                } else {
                    0
                };
                if let Some(spill) = &spill {
                    spill.apply(px);
                }
            }
            std::mem::swap(&mut above, &mut row);
            std::mem::swap(&mut row, &mut below);
            if y + 1 < height {
                across(rgba, (y + 2).min(height - 1), &mut below);
            }
        }
    }
}

/// `despill` with its defaults but the pulled channel's scale at `-amount`:
/// that channel loses `amount` of how far it stands over the mean of the
/// other two. Its steps and float rounding, then truncation to 8 bits; the
/// other channels come out as they went in.
struct Despill {
    pulled: usize,
    mixed: usize,
    other: usize,
    amount: f32,
    /// Each level as despill reads it, `v / 255`.
    unit: [f32; 256],
}

impl Despill {
    fn new(kind: Spill, amount: f32) -> Self {
        let (pulled, mixed, other) = kind.channels();
        Self {
            pulled,
            mixed,
            other,
            amount,
            unit: std::array::from_fn(|v| v as f32 / 255.0),
        }
    }

    #[inline]
    fn apply(&self, px: &mut [u8; 4]) {
        let factor = (1.0 - SPILL_MIX) * (1.0 - 0.0);
        let level = |channel: usize| self.unit[usize::from(px[channel])];
        let c = level(self.pulled);
        let spillmap = (c - (level(self.mixed) * SPILL_MIX + level(self.other) * factor)).max(0.0);
        // Brightness is 0, and so is every other channel's scale. With no
        // spill this writes (v / 255) * 255, truncated, which can be a level
        // lower, as despill does.
        px[self.pulled] = ((c + spillmap * -self.amount).max(0.0) * 255.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one-pass key against `chromakey`'s definition written plainly:
    /// every pixel's alpha from the mean distance of the nine around it,
    /// edges repeated, on a small picture where neighbours differ.
    #[test]
    fn the_key_averages_each_pixel_with_its_eight_neighbours() {
        let (w, h) = (7usize, 5usize);
        let mut seed = 7u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as u8
        };
        let picture: Vec<u8> = (0..w * h)
            .flat_map(|_| [next() / 3, 130 + next() / 4, 30 + next() / 4, 255])
            .collect();
        let key = ChromaKey {
            strength: 0.2,
            softness: 1.0,
            spill: 0.0,
            ..ChromaKey::default()
        };
        let mut keyed = picture.clone();
        key.apply_rgba(&mut keyed, w);

        let (_, ku, kv) = yuv(key.color);
        let distance = |x: usize, y: usize| {
            let p = &picture[(y * w + x) * 4..];
            let (_, u, v) = yuv(Rgb(p[0], p[1], p[2]));
            let (du, dv) = (f64::from(u) - f64::from(ku), f64::from(v) - f64::from(kv));
            ((du * du + dv * dv) / (255.0 * 255.0 * 2.0)).sqrt()
        };
        let mut partial = 0;
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0.0;
                for dy in [-1, 0, 1] {
                    for dx in [-1, 0, 1] {
                        let at = |v: usize, d: isize, len: usize| {
                            v.saturating_add_signed(d).min(len - 1)
                        };
                        sum += distance(at(x, dx, w), at(y, dy, h));
                    }
                }
                let diff = sum / 9.0;
                let similarity = f64::from(key.similarity());
                let want = ((diff - similarity) / f64::from(key.blend())).clamp(0.0, 1.0) * 255.0;
                let got = keyed[(y * w + x) * 4 + 3];
                assert!(
                    (f64::from(got) - want).abs() <= 1.0,
                    "({x}, {y}): alpha {got}, want {want:.2}"
                );
                partial += usize::from((1..255).contains(&got));
            }
        }
        assert!(partial > w * h / 3, "only {partial} partly clear pixels");
        // The key leaves colour alone when spill is off.
        let same = |rgba: &[u8]| -> Vec<u8> {
            rgba.as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect()
        };
        assert_eq!(same(&keyed), same(&picture));
    }

    /// A key picked in the screen's shadow is a duller green than the lit
    /// screen, closer to grey. At the default strength it still clears the
    /// lit screen and keeps a grey-green collar, dark hair and white.
    #[test]
    fn a_key_picked_in_shadow_keeps_greys_and_hair() {
        let key = ChromaKey {
            color: Rgb(0, 114, 36),
            ..ChromaKey::default()
        };
        let alpha = |colour: [u8; 3]| {
            let mut px: Vec<u8> = (0..9)
                .flat_map(|_| [colour[0], colour[1], colour[2], 255])
                .collect();
            key.apply_rgba(&mut px, 3);
            px[4 * 4 + 3]
        };
        assert_eq!(alpha([0, 177, 64]), 0, "the lit screen");
        for kept in [
            [160, 180, 144],
            [40, 30, 25],
            [235, 235, 235],
            [220, 170, 140],
        ] {
            assert_eq!(alpha(kept), 255, "{kept:?} cleared");
        }
    }

    /// The usual green and white land where BT.709 limited range puts them.
    #[test]
    fn colours_convert_as_bt709_limited_range() {
        assert_eq!(yuv(Rgb::WHITE), (235, 128, 128));
        assert_eq!(yuv(Rgb::BLACK), (16, 128, 128));
        // Pure red: Y 16 + 219 * 0.2126, Cr at the top of its range.
        assert_eq!(yuv(Rgb(255, 0, 0)), (63, 102, 240));
    }
}

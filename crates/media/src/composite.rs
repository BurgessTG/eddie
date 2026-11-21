//! Pictures laid over one another on the CPU, as export lays them: each
//! cropped, scaled, turned and flipped where [`openreel_engine::place`]
//! puts it, faded by its own alpha and its opacity, over a blurred copy of
//! itself when it has one, and mixed with what is under it by its blend
//! mode. Contact sheets draw their cells with it, and the monitor the
//! lanes under a blended picture.

use std::ops::Range;

use openreel_engine::{BlendMode, Placement};

use crate::RawFrame;

/// The blurred background's spread, as a share of the frame's width: the
/// monitor's sigma.
const BLUR_SHARE: f64 = 0.021;
/// Fewer pixels than this are drawn on one thread.
const THREADED: usize = 120_000;

/// A decoded picture: `width` by `height` RGBA, straight alpha.
#[derive(Clone, Copy)]
pub struct Image<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
}

impl<'a> From<&'a RawFrame> for Image<'a> {
    fn from(frame: &'a RawFrame) -> Self {
        Self {
            width: frame.width,
            height: frame.height,
            rgba: &frame.rgba,
        }
    }
}

/// One picture to draw on a [`Canvas`].
#[derive(Clone, Copy)]
pub struct Layer<'a> {
    /// The whole of a file whose displayed size is `source`, upright and
    /// unflipped.
    pub image: Image<'a>,
    pub source: (f64, f64),
    /// Where it lands, in canvas pixels.
    pub placement: Placement,
    pub opacity: f32,
    /// Over a blurred copy of itself that fills the frame.
    pub blur: bool,
    pub blend: BlendMode,
    /// Moved this far across and down, background and all, in canvas
    /// pixels: a sliding transition's side.
    pub shift: [f64; 2],
    /// Drawn only inside `[x, y, width, height]` of the canvas.
    pub window: Option<[f64; 4]>,
}

/// An opaque RGBA picture of a `canvas` sized frame, `width` by `height`
/// pixels.
#[derive(Clone)]
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    canvas: (f64, f64),
    pub rgba: Vec<u8>,
}

impl Canvas {
    /// Black.
    pub fn new(width: usize, height: usize, canvas: (f64, f64)) -> Self {
        let mut rgba = vec![0; width * height * 4];
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        Self {
            width,
            height,
            canvas,
            rgba,
        }
    }

    /// Canvas pixels per pixel, across and down.
    fn scale(&self) -> (f64, f64) {
        (
            self.canvas.0 / self.width as f64,
            self.canvas.1 / self.height as f64,
        )
    }

    /// Draw `layer` over what is here.
    pub fn draw(&mut self, layer: &Layer) {
        let gain = layer.opacity.clamp(0.0, 1.0);
        if gain <= 0.0 {
            return;
        }
        let [dx, dy] = layer.shift;
        let moved = Layer {
            placement: Placement {
                center: [
                    layer.placement.center[0] + dx,
                    layer.placement.center[1] + dy,
                ],
                ..layer.placement
            },
            ..*layer
        };
        if layer.blur && !layer.placement.covers(self.canvas) {
            // The picture over its blurred copy is one opaque picture that
            // fills the frame, as export lays it out.
            let mut own = self.background(layer);
            own.sample(
                &Layer {
                    blend: BlendMode::Normal,
                    window: None,
                    ..moved
                },
                1.0,
            );
            self.lay(&own, layer.window, gain, layer.blend);
        } else {
            self.sample(&moved, gain);
        }
    }

    /// `other` over this by `alpha`.
    pub fn mix(&mut self, other: &Canvas, alpha: f32) {
        self.lay(other, None, alpha.clamp(0.0, 1.0), BlendMode::Normal);
    }

    /// Faded toward `rgb` by `amount`.
    pub fn veil(&mut self, rgb: [u8; 3], amount: f32) {
        let amount = fraction(amount);
        for pixel in self.rgba.as_chunks_mut::<4>().0 {
            for channel in 0..3 {
                pixel[channel] = mix(pixel[channel], rgb[channel], amount);
            }
        }
    }

    /// `layer`'s picture filling the frame, blurred.
    fn background(&self, layer: &Layer) -> Canvas {
        let (sw, sh) = layer.source;
        let k = (self.canvas.0 / sw).max(self.canvas.1 / sh);
        let fill = Layer {
            placement: Placement {
                crop: [0.0, 0.0, sw, sh],
                size: [sw * k, sh * k],
                center: [
                    self.canvas.0 / 2.0 + layer.shift[0],
                    self.canvas.1 / 2.0 + layer.shift[1],
                ],
                rotation: 0.0,
                ..layer.placement
            },
            blend: BlendMode::Normal,
            window: None,
            ..*layer
        };
        let mut background = Canvas::new(self.width, self.height, self.canvas);
        background.sample(&fill, 1.0);
        let radius = (1.22 * BLUR_SHARE * self.width as f64).round() as usize;
        box_blur(&mut background.rgba, self.width, self.height, radius);
        background
    }

    /// The pixel rows and columns inside `window` (canvas pixels).
    fn inside(&self, window: Option<[f64; 4]>) -> (Range<usize>, Range<usize>) {
        let (kx, ky) = self.scale();
        let [x, y, w, h] = window.unwrap_or([0.0, 0.0, self.canvas.0, self.canvas.1]);
        // Pixels whose centres fall inside.
        let span = |from: f64, to: f64, k: f64, most: usize| {
            let first = (from / k - 0.5).ceil().max(0.0) as usize;
            let last = ((to / k - 0.5).ceil().max(0.0) as usize).min(most);
            first.min(last)..last
        };
        (
            span(y, y + h, ky, self.height),
            span(x, x + w, kx, self.width),
        )
    }

    /// `over` (the same size) over this inside `window`, mixed by `blend`
    /// and laid on by `alpha`.
    fn lay(&mut self, over: &Canvas, window: Option<[f64; 4]>, alpha: f32, blend: BlendMode) {
        let table = (!blend.is_normal()).then(|| blend.table());
        let alpha = fraction(alpha);
        let (rows, columns) = self.inside(window);
        let width = self.width;
        self.rows(rows, |row, line| {
            for col in columns.clone() {
                let src = &over.rgba[(row * width + col) * 4..][..3];
                let pixel = &mut line[col * 4..][..3];
                for channel in 0..3 {
                    let under = pixel[channel];
                    let blended = table.map_or(src[channel], |table| {
                        table[usize::from(under) * 256 + usize::from(src[channel])]
                    });
                    pixel[channel] = mix(under, blended, alpha);
                }
            }
        });
    }

    /// Draw `layer`'s picture where it covers the frame, its alpha times
    /// `gain`, with bilinear sampling.
    fn sample(&mut self, layer: &Layer, gain: f32) {
        let p = layer.placement;
        let image = layer.image;
        if image.width == 0 || image.height == 0 {
            return;
        }
        let table = (!layer.blend.is_normal()).then(|| layer.blend.table());
        let gain = (gain.clamp(0.0, 1.0) * 256.0 + 0.5) as u32;
        let (sin, cos) = (-p.rotation.to_radians()).sin_cos();
        let (kx, ky) = self.scale();
        let (fx, fy) = (
            f64::from(image.width) / layer.source.0,
            f64::from(image.height) / layer.source.1,
        );
        let [crop_x, crop_y, crop_w, crop_h] = p.crop;
        // Only the box the turned picture fills, inside the window.
        let [tw, th] = p.turned_size();
        let bounds = [
            p.center[0] - tw / 2.0 - 1.0,
            p.center[1] - th / 2.0 - 1.0,
            tw + 2.0,
            th + 2.0,
        ];
        let (box_rows, box_cols) = self.inside(Some(bounds));
        let (rows, columns) = self.inside(layer.window);
        let rows = rows.start.max(box_rows.start)..rows.end.min(box_rows.end);
        let columns = columns.start.max(box_cols.start)..columns.end.min(box_cols.end);
        if rows.is_empty() || columns.is_empty() {
            return;
        }
        // A canvas pixel's centre as a share of the unturned picture, 0 to
        // 1 across and down, offset from the picture's centre `(dx, dy)`.
        let across = move |dx: f64, dy: f64| (dx * cos - dy * sin) / p.size[0] + 0.5;
        let down = move |dx: f64, dy: f64| (dx * sin + dy * cos) / p.size[1] + 0.5;
        // The crop is of the unflipped source; the flip mirrors it.
        let (w, h) = (image.width as usize, image.height as usize);
        let tap_x = move |u: f64| {
            let u = if p.flip_h { 1.0 - u } else { u };
            Tap::at((crop_x + u * crop_w) * fx, w)
        };
        let tap_y = move |v: f64| {
            let v = if p.flip_v { 1.0 - v } else { v };
            Tap::at((crop_y + v * crop_h) * fy, h)
        };
        let inside = |share: f64| (0.0..1.0).contains(&share);
        let dx = move |col: usize| (col as f64 + 0.5) * kx - p.center[0];
        let dy = move |row: usize| (row as f64 + 0.5) * ky - p.center[1];
        if p.rotation.rem_euclid(360.0) == 0.0 {
            // Upright: a column reads the same source pixels on every row.
            let taps: Vec<Option<Tap>> = columns
                .clone()
                .map(|col| {
                    let u = across(dx(col), 0.0);
                    inside(u).then(|| tap_x(u))
                })
                .collect();
            self.rows(rows, |row, line| {
                let v = down(0.0, dy(row));
                if !inside(v) {
                    return;
                }
                let lines = Lines::of(image, tap_y(v));
                for (col, x) in columns.clone().zip(&taps) {
                    if let Some(x) = x {
                        put(&mut line[col * 4..][..3], lines.texel(*x), gain, table);
                    }
                }
            });
        } else {
            self.rows(rows, |row, line| {
                let dy = dy(row);
                for col in columns.clone() {
                    let dx = dx(col);
                    let (u, v) = (across(dx, dy), down(dx, dy));
                    if inside(u) && inside(v) {
                        let texel = Lines::of(image, tap_y(v)).texel(tap_x(u));
                        put(&mut line[col * 4..][..3], texel, gain, table);
                    }
                }
            });
        }
    }

    /// Call `draw(row, line)` for each row of `rows`, `line` that row's
    /// pixels, on several threads when there are many.
    fn rows(&mut self, rows: Range<usize>, draw: impl Fn(usize, &mut [u8]) + Sync) {
        let stride = self.width * 4;
        let lines = &mut self.rgba[rows.start * stride..rows.end * stride];
        let threads = if rows.len() * self.width < THREADED {
            1
        } else {
            std::thread::available_parallelism().map_or(1, |n| n.get().min(8))
        };
        let per = rows.len().div_ceil(threads).max(1);
        let band = |first: usize, chunk: &mut [u8]| {
            for (offset, line) in chunk.chunks_exact_mut(stride).enumerate() {
                draw(first + offset, line);
            }
        };
        if threads == 1 {
            band(rows.start, lines);
            return;
        }
        std::thread::scope(|scope| {
            for (index, chunk) in lines.chunks_mut(per * stride).enumerate() {
                let band = &band;
                scope.spawn(move || band(rows.start + index * per, chunk));
            }
        });
    }
}

/// Where a canvas pixel reads a picture along one axis: the two source
/// pixels around it, and the second one's weight out of 256.
#[derive(Clone, Copy)]
struct Tap {
    first: usize,
    second: usize,
    weight: u32,
}

impl Tap {
    /// Position `at` on a line of `len` pixels whose centres are at halves.
    fn at(at: f64, len: usize) -> Self {
        let x = (at - 0.5).clamp(0.0, (len - 1) as f64);
        // `x` is not negative, so the cast floors it.
        let first = x as usize;
        let second = (first + 1).min(len - 1);
        match ((x - first as f64) * 256.0 + 0.5) as u32 {
            // On a pixel, as a picture drawn at its own size lands.
            256.. => Self {
                first: second,
                second,
                weight: 0,
            },
            weight => Self {
                first,
                second,
                weight,
            },
        }
    }
}

/// The two rows of a picture a [`Tap`] down it reads.
struct Lines<'a> {
    top: &'a [u8],
    bottom: &'a [u8],
    weight: u32,
}

impl<'a> Lines<'a> {
    fn of(image: Image<'a>, y: Tap) -> Self {
        let stride = image.width as usize * 4;
        Self {
            top: &image.rgba[y.first * stride..][..stride],
            bottom: &image.rgba[y.second * stride..][..stride],
            weight: y.weight,
        }
    }

    /// The RGBA between these rows and the pixels `x` names, each
    /// channel's level times 65536. Integer arithmetic: float rounding and
    /// flooring are library calls on baseline x86-64, and cost most of a
    /// pixel.
    fn texel(&self, x: Tap) -> [u32; 4] {
        let (a, b) = (
            &self.top[x.first * 4..][..4],
            &self.top[x.second * 4..][..4],
        );
        let (c, d) = (
            &self.bottom[x.first * 4..][..4],
            &self.bottom[x.second * 4..][..4],
        );
        if x.weight == 0 && self.weight == 0 {
            return [0, 1, 2, 3].map(|channel| u32::from(a[channel]) << 16);
        }
        let (left, right) = (256 - x.weight, x.weight);
        let mut out = [0; 4];
        for (channel, value) in out.iter_mut().enumerate() {
            let top = u32::from(a[channel]) * left + u32::from(b[channel]) * right;
            let bottom = u32::from(c[channel]) * left + u32::from(d[channel]) * right;
            *value = top * (256 - self.weight) + bottom * self.weight;
        }
        out
    }
}

/// Lay a [`Lines::texel`] on `pixel` (RGB), its alpha times `gain` (out of 256),
/// mixed by `table` when it has a blend mode.
fn put(pixel: &mut [u8], texel: [u32; 4], gain: u32, table: Option<&[u8]>) {
    let alpha = (u64::from(texel[3]) * u64::from(gain) / (255 * 256)) as u32;
    if alpha == 0 {
        return;
    }
    for channel in 0..3 {
        let over = ((texel[channel] + (1 << 15)) >> 16) as u8;
        let under = pixel[channel];
        let over = table.map_or(over, |table| {
            table[usize::from(under) * 256 + usize::from(over)]
        });
        pixel[channel] = mix(under, over, alpha);
    }
}

/// `amount` from 0 to 1 as a share out of 65536.
fn fraction(amount: f32) -> u32 {
    (amount.clamp(0.0, 1.0) * 65536.0 + 0.5) as u32
}

/// `under` moved toward `over` by `alpha` out of 65536, rounded.
fn mix(under: u8, over: u8, alpha: u32) -> u8 {
    if alpha >= 1 << 16 {
        return over;
    }
    let step = (i32::from(over) - i32::from(under)) * alpha as i32;
    (i32::from(under) + ((step + (1 << 15)) >> 16)) as u8
}

/// Blur a `w` by `h` RGBA picture with two box passes each way, about a
/// Gaussian of sigma `radius / 1.22`. Edges average what is inside.
fn box_blur(pixels: &mut [u8], w: usize, h: usize, radius: usize) {
    if radius == 0 {
        return;
    }
    for _ in 0..2 {
        blur_lines(pixels, (w, 4), (h, w * 4), radius);
        blur_lines(pixels, (h, w * 4), (w, 4), radius);
    }
}

/// Box-blur `lines` lines of `len` pixels each, `step` bytes apart along a
/// line and `stride` bytes between lines.
fn blur_lines(
    pixels: &mut [u8],
    (len, step): (usize, usize),
    (lines, stride): (usize, usize),
    radius: usize,
) {
    let mut line = vec![[0.0_f32; 3]; len];
    for index in 0..lines {
        let base = index * stride;
        for (i, value) in line.iter_mut().enumerate() {
            let o = base + i * step;
            *value = [
                f32::from(pixels[o]),
                f32::from(pixels[o + 1]),
                f32::from(pixels[o + 2]),
            ];
        }
        // A running sum over the window, so a wide blur costs no more.
        let mut sum = [0.0_f32; 3];
        for value in &line[..=radius.min(len - 1)] {
            for channel in 0..3 {
                sum[channel] += value[channel];
            }
        }
        for i in 0..len {
            let (first, last) = (i.saturating_sub(radius), (i + radius).min(len - 1));
            let n = (last - first + 1) as f32;
            let o = base + i * step;
            for channel in 0..3 {
                pixels[o + channel] = (sum[channel] / n + 0.5) as u8;
            }
            if i + radius + 1 < len {
                for channel in 0..3 {
                    sum[channel] += line[i + radius + 1][channel];
                }
            }
            if i >= radius {
                for channel in 0..3 {
                    sum[channel] -= line[i - radius][channel];
                }
            }
        }
    }
}

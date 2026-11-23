//! vid.stab's second pass done here instead of in `vidstabtransform`,
//! which moves each frame on one core: at 4K that took 70 ms a frame,
//! three quarters of a Stabilize. The camera path is smoothed as vid.stab
//! 1.1 smooths it (`cameraPathGaussian`, then `vsPreprocessTransforms`
//! with a static zoom), and each frame is moved by a port of its
//! fixed-point bilinear `transformPlanar`, in bands of rows on every core.
//! Borders the moved picture leaves keep what the frame before showed
//! there, as the filter's default `crop=keep` does.

use std::thread;

use crate::MediaResult;

/// How a frame moved from the one before, or, once smoothed, how far to
/// move it back: pixels across and down, radians clockwise, and percent
/// enlarged.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Motion {
    pub x: f64,
    pub y: f64,
    pub alpha: f64,
    pub zoom: f64,
}

impl Motion {
    fn add(self, other: Self) -> Self {
        Self {
            x: self.x + other.x,
            y: self.y + other.y,
            alpha: self.alpha + other.alpha,
            zoom: self.zoom + other.zoom,
        }
    }

    fn sub(self, other: Self) -> Self {
        Self {
            x: self.x - other.x,
            y: self.y - other.y,
            alpha: self.alpha - other.alpha,
            zoom: self.zoom - other.zoom,
        }
    }

    fn times(self, factor: f64) -> Self {
        Self {
            x: self.x * factor,
            y: self.y * factor,
            alpha: self.alpha * factor,
            zoom: self.zoom * factor,
        }
    }
}

/// The frame-to-frame motions `vidstabtransform=debug=1` writes to
/// `global_motions.trf`: `0 x y alpha zoom extra` a frame, with `#` lines
/// between. The filter drops the zoom (FFmpeg turns `smoothZoom` off).
pub(super) fn read_motions(text: &str) -> MediaResult<Vec<Motion>> {
    let broken = || "the camera's motion could not be read".to_owned();
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let fields = line
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| broken())?;
            let [_, x, y, alpha, _, _] = fields[..] else {
                return Err(broken());
            };
            Ok(Motion {
                x,
                y,
                alpha,
                zoom: 0.0,
            })
        })
        .collect()
}

/// What smoothing the camera path takes, as `vidstabtransform`'s options
/// say: `smoothing` frames each side, `max_shift` pixels and `max_angle`
/// radians at most, for a picture `width` by `height`.
pub(super) struct PathOptions {
    pub smoothing: usize,
    pub max_shift: i32,
    pub max_angle: f64,
    pub width: u32,
    pub height: u32,
}

/// How far to move each frame: the motions added up into the camera's
/// path, less that path smoothed by a Gaussian, held within the limits,
/// then enlarged by one zoom that hides the edges of 98% of the frames.
pub(super) fn camera_path(motions: &[Motion], options: &PathOptions) -> Vec<Motion> {
    let mut path = motions.to_vec();
    if path.is_empty() {
        return path;
    }
    // Relative to absolute.
    for i in 1..path.len() {
        path[i] = path[i].add(path[i - 1]);
    }
    if options.smoothing > 0 {
        let mu = options.smoothing;
        let size = mu * 2 + 1;
        let sigma2 = (mu as f64 / 2.0) * (mu as f64 / 2.0);
        let mut kernel = vec![0.0; size];
        for i in 0..=mu {
            let d = (i as f64 - mu as f64) * (i as f64 - mu as f64);
            kernel[i] = (-d / sigma2).exp();
            kernel[size - i - 1] = kernel[i];
        }
        let absolute = path.clone();
        for (i, moved) in path.iter_mut().enumerate() {
            let mut weights = 0.0;
            let mut average = Motion::default();
            for (k, &weight) in kernel.iter().enumerate() {
                let Some(at) = (i + k).checked_sub(mu) else {
                    continue;
                };
                if let Some(&point) = absolute.get(at) {
                    weights += weight;
                    average = average.add(point.times(weight));
                }
            }
            if weights > 0.0 {
                *moved = moved.sub(average.times(1.0 / weights));
            }
        }
    }
    let max_shift = options
        .max_shift
        .min(options.width as i32 / 2)
        .min(options.height as i32 / 2);
    if max_shift != -1 {
        let limit = f64::from(max_shift);
        for moved in &mut path {
            moved.x = moved.x.max(-limit).min(limit);
            moved.y = moved.y.max(-limit).min(limit);
        }
    }
    if options.max_angle != -1.0 {
        let limit = options.max_angle;
        for moved in &mut path {
            moved.alpha = moved.alpha.max(-limit).min(limit);
        }
    }
    if path.len() > 1 {
        // The shifts that leave 1% of the frames out at each end.
        let cut = path.len() / 100;
        let mut xs: Vec<f64> = path.iter().map(|moved| moved.x).collect();
        let mut ys: Vec<f64> = path.iter().map(|moved| moved.y).collect();
        xs.sort_by(f64::total_cmp);
        ys.sort_by(f64::total_cmp);
        let last = path.len() - cut - 1;
        let zx = 2.0 * xs[last].max(xs[cut].abs()) / f64::from(options.width);
        let zy = 2.0 * ys[last].max(ys[cut].abs()) / f64::from(options.height);
        let zoom = (100.0 * zx.max(zy)).clamp(-60.0, 60.0);
        if zoom != 0.0 {
            for moved in &mut path {
                moved.zoom += zoom;
            }
        }
    }
    path
}

/// Move the yuv420p picture `src`, `width` by `height`, by `motion` into
/// `out`, which holds the picture before it: where the moved picture does
/// not reach, the one before shows, blending into the moved picture's
/// edge over 10 pixels. `threads` bands of rows are moved at once.
pub(super) fn warp(
    src: &[u8],
    out: &mut [u8],
    (width, height): (usize, usize),
    motion: Motion,
    threads: usize,
) {
    if motion == Motion::default() {
        out.copy_from_slice(src);
        return;
    }
    let luma = width * height;
    let (chroma_w, chroma_h) = (width.div_ceil(2), height.div_ceil(2));
    let chroma = chroma_w * chroma_h;
    let (src_y, src_uv) = src.split_at(luma);
    let (src_u, src_v) = src_uv.split_at(chroma);
    let (out_y, out_uv) = out.split_at_mut(luma);
    let (out_u, out_v) = out_uv.split_at_mut(chroma);
    let planes = [
        Plane::new(src_y, width, height, 0, motion),
        Plane::new(src_u, chroma_w, chroma_h, 1, motion),
        Plane::new(src_v, chroma_w, chroma_h, 1, motion),
    ];
    thread::scope(|scope| {
        for (plane, out) in planes.iter().zip([out_y, out_u, out_v]) {
            let rows = plane.height.div_ceil(threads.max(1)).max(1);
            for (band, out) in out.chunks_mut(rows * plane.width).enumerate() {
                scope.spawn(move || plane.rows(band * rows, out));
            }
        }
    });
}

/// `(int32_t)(v * 0xFFFF)`, vid.stab's fixed point (16 bits, scaled by
/// 65535, not 65536).
fn fixed(value: f64) -> i32 {
    (value * 65535.0) as i32
}

/// One plane of a picture and the motion that moves it, in fixed point.
struct Plane<'a> {
    src: &'a [u8],
    width: usize,
    height: usize,
    zcos: i32,
    zsin: i32,
    c_tx: i32,
    c_ty: i32,
}

impl<'a> Plane<'a> {
    /// A plane `width` by `height`, `sub` times halved from the picture.
    fn new(src: &'a [u8], width: usize, height: usize, sub: u32, motion: Motion) -> Self {
        // `float z = 1.0 - t.zoom/100.0`: single precision, as vid.stab.
        let z = f64::from((1.0 - motion.zoom / 100.0) as f32);
        Self {
            src,
            width,
            height,
            zcos: fixed(z * (-motion.alpha).cos()),
            zsin: fixed(z * (-motion.alpha).sin()),
            c_tx: (((width / 2) as i32) << 16) - (fixed(motion.x) >> sub),
            c_ty: (((height / 2) as i32) << 16) - (fixed(motion.y) >> sub),
        }
    }

    /// Rows from `first` on into `out`, which holds what they showed.
    fn rows(&self, first: usize, out: &mut [u8]) {
        let (c_d_x, c_d_y) = ((self.width / 2) as i32, (self.height / 2) as i32);
        for (row, line) in out.chunks_mut(self.width).enumerate() {
            let y_d = (first + row) as i32 - c_d_y;
            let (row_x, row_y) = (
                self.zsin.wrapping_mul(y_d).wrapping_add(self.c_tx),
                self.zcos.wrapping_mul(y_d).wrapping_add(self.c_ty),
            );
            for (x, dest) in line.iter_mut().enumerate() {
                let x_d = x as i32 - c_d_x;
                let x_s = self.zcos.wrapping_mul(x_d).wrapping_add(row_x);
                let y_s = self
                    .zsin
                    .wrapping_neg()
                    .wrapping_mul(x_d)
                    .wrapping_add(row_y);
                *dest = self.bilinear(x_s, y_s, *dest);
            }
        }
    }

    fn pixel(&self, x: i32, y: i32) -> i32 {
        i32::from(self.src[x as usize + y as usize * self.width])
    }

    /// `interpolateBiLin`, `interpolateBiLinBorder` where it reaches the
    /// edge, with `kept` as the value outside.
    fn bilinear(&self, x: i32, y: i32, kept: u8) -> u8 {
        let (width, height) = (self.width as i32, self.height as i32);
        let (ix_f, iy_f) = (x >> 16, y >> 16);
        if ix_f < 0 || ix_f > width - 2 || iy_f < 0 || iy_f > height - 2 {
            // Past the edge: the edge pixel, giving way to what was kept
            // over 10 pixels outwards.
            const BLUR: i32 = 10;
            let (ix_c, iy_c) = (ix_f + 1, iy_f + 1);
            let far = (-BLUR - ix_f)
                .max(-BLUR - iy_f)
                .max(ix_c - BLUR - width)
                .max(iy_c - BLUR - height);
            let c = far.clamp(0, BLUR);
            let edge = self.pixel(ix_f.min(width - 1).max(0), iy_f.min(height - 1).max(0));
            let value = (i32::from(kept) * c + edge * (BLUR - c)) / BLUR;
            return value.clamp(0, 255) as u8;
        }
        let (ix_c, iy_c) = (ix_f + 1, iy_f + 1);
        let v1 = self.pixel(ix_c, iy_c);
        let v2 = self.pixel(ix_c, iy_f);
        let v3 = self.pixel(ix_f, iy_c);
        let v4 = self.pixel(ix_f, iy_f);
        let (x_f, x_c, y_f, y_c) = (ix_f << 16, ix_c << 16, iy_f << 16, iy_c << 16);
        let s = ((v1 * (x - x_f) + v3 * (x_c - x)) >> 8) * ((y - y_f) >> 8)
            + ((v2 * (x - x_f) + v4 * (x_c - x)) >> 8) * ((y_c - y) >> 8);
        // vid.stab's own rounding: truncated, then one added.
        let value = (s >> 16) as i16;
        if value < 0 {
            0
        } else if value < 255 {
            value as u8 + 1
        } else {
            255
        }
    }
}

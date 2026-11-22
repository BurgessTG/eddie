//! The program monitor's picture: the decoded frame, where
//! `openreel_engine::place` puts it on the canvas (as export does), and a
//! drag that moves it. Rune draws images upright only and has no colour
//! filter or mask for them, so flips, keys, colour and masks happen to the
//! pixels here.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use eddie_core::PreviewSink;
use openreel_engine::{
    Background, ChromaKey, Clip, ClipId, ColorAdjust, Mask, Rgb, mask_alpha, place,
};
use openreel_media::RawFrame;
use rune::{Cursor, El, Phase, Pixels, SurfaceFit, el};

use crate::app::Ui;

/// Export blurs the background with two box passes at an eighth of the
/// canvas (`export/video.rs`): about this share of the canvas width as a
/// Gaussian's sigma.
const BLUR_SHARE: f32 = 0.021;

/// A drag that ends this close to the canvas centre, in points, lands on it.
const CENTRE_SNAP: f32 = 6.0;

/// How the clip at the playhead wants its frame drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Look {
    /// Horizontal and vertical flips.
    pub flips: (bool, bool),
    pub chroma: Option<ChromaKey>,
    pub color: ColorAdjust,
    pub mask: Option<Mask>,
    /// The crop's left, top, right and bottom shares, which place the mask.
    pub crop: [f64; 4],
    /// Over its blurred copy, which the mask does not clear.
    pub blur: bool,
}

/// The look of the video clip at the playhead, for [`Picture::want`].
pub fn look(clip: Option<&Clip>) -> Look {
    clip.map_or_else(Look::default, |clip| {
        let t = clip.transform;
        Look {
            flips: (t.flip_h, t.flip_v),
            chroma: clip.chroma,
            color: clip.color,
            mask: clip.mask,
            crop: [t.crop_left, t.crop_top, t.crop_right, t.crop_bottom],
            blur: clip.background == Background::Blur,
        }
    })
}

/// What a matte was laid for: the mask, crop and flips on a frame's size.
type MatteFor = (Mask, [f64; 4], (bool, bool), u32, u32);

/// The last decoded frame, upright and ungraded, and the look it is shown
/// with.
#[derive(Default)]
pub struct Picture {
    want: Look,
    shown: Look,
    width: u32,
    /// Shared with the blend worker while it lays the frame.
    rgba: Arc<Vec<u8>>,
    /// Counts decoded frames, so a reader can tell a new one arrived.
    frame: u64,
    /// The mask's coverage of each pixel of the frame, kept while the mask,
    /// crop, flips and frame size stay.
    matte: Vec<u8>,
    matte_for: Option<MatteFor>,
}

impl Picture {
    /// Show frames with the clip at the playhead's look, redrawing the held
    /// frame when it changed (a flip toggled or a slider moved while
    /// paused).
    pub fn want(&mut self, look: Look, pixels: &Pixels, backdrop: &Pixels) {
        self.want = look;
        if self.shown != look && !self.rgba.is_empty() {
            self.show(pixels, backdrop);
        }
    }

    /// The held frame as decoded, unflipped and ungraded: its width,
    /// pixels and number.
    pub fn upright(&self) -> (u32, &[u8], u64) {
        (self.width, &self.rgba, self.frame)
    }

    /// The held frame as [`Self::upright`] gives it, shared rather than
    /// copied.
    pub fn shared(&self) -> (u32, Arc<Vec<u8>>) {
        (self.width, Arc::clone(&self.rgba))
    }

    /// Draw the held frame with the wanted look into `pixels`, and, when a
    /// mask clears it over its blurred copy, unmasked into `backdrop`.
    fn show(&mut self, pixels: &Pixels, backdrop: &Pixels) {
        self.shown = self.want;
        let height = (self.rgba.len() / (self.width as usize * 4).max(1)) as u32;
        let mut shown = flipped(&self.rgba, self.width, self.want.flips);
        if let Some(key) = &self.want.chroma {
            key.apply_rgba(&mut shown, self.width as usize);
        }
        self.want.color.apply_rgba(&mut shown);
        if let Some(mask) = self.want.mask {
            if self.want.blur {
                backdrop.set(self.width, height, shown.clone());
            }
            mask_alpha(&mut shown, self.matte(mask, height));
        }
        pixels.set(self.width, height, shown);
    }

    /// The wanted mask's [`matte`] over a held frame `height` tall, kept
    /// until the mask, crop, flips or size change.
    fn matte(&mut self, mask: Mask, height: u32) -> &[u8] {
        let Look { crop, flips, .. } = self.want;
        let made_for = (mask, crop, flips, self.width, height);
        if self.matte_for != Some(made_for) {
            self.matte = matte(mask, crop, flips, (self.width, height));
            self.matte_for = Some(made_for);
        }
        &self.matte
    }
}

/// `mask`'s coverage of each pixel of a `width` by `height` decoded frame
/// already flipped by `flips`: laid on the part `crop` (left, top, right
/// and bottom shares of the unflipped source) shows, which the flip moves
/// to the other side.
pub(super) fn matte(
    mask: Mask,
    crop: [f64; 4],
    flips: (bool, bool),
    (width, height): (u32, u32),
) -> Vec<u8> {
    let (w, h) = (f64::from(width), f64::from(height));
    let [left, top, right, bottom] = crop;
    let x = if flips.0 { right } else { left };
    let y = if flips.1 { bottom } else { top };
    mask.on((1.0 - left - right) * w, (1.0 - top - bottom) * h)
        .levels(width as usize, height as usize, (x * w, y * h))
}

/// `rgba` mirrored left to right and/or top to bottom.
pub(super) fn flipped(rgba: &[u8], width: u32, (across, down): (bool, bool)) -> Vec<u8> {
    if !across && !down {
        return rgba.to_vec();
    }
    let stride = width as usize * 4;
    let mut out = Vec::with_capacity(rgba.len());
    let mut row = |line: &[u8]| {
        if across {
            for pixel in line.as_chunks::<4>().0.iter().rev() {
                out.extend_from_slice(pixel);
            }
        } else {
            out.extend_from_slice(line);
        }
    };
    if down {
        rgba.chunks_exact(stride).rev().for_each(&mut row);
    } else {
        rgba.chunks_exact(stride).for_each(&mut row);
    }
    out
}

/// One picture the monitor holds: the pixels Rune draws, the decoded
/// size while it holds one, and the frame behind them.
#[derive(Clone, Default)]
pub struct Slot {
    pub pixels: Pixels,
    /// The picture without its mask, for its blurred copy.
    pub backdrop: Pixels,
    pub size: Rc<Cell<Option<(u32, u32)>>>,
    pub picture: Rc<RefCell<Picture>>,
}

impl Slot {
    /// Show the held frame as the clip `look` describes wants it.
    pub fn want(&self, look: Look) {
        self.picture
            .borrow_mut()
            .want(look, &self.pixels, &self.backdrop);
    }

    fn show(&self, frame: &RawFrame) {
        let mut picture = self.picture.borrow_mut();
        picture.width = frame.width;
        match Arc::get_mut(&mut picture.rgba) {
            Some(rgba) => {
                rgba.clear();
                rgba.extend_from_slice(&frame.rgba);
            }
            // The blend worker still lays the last one.
            None => picture.rgba = Arc::new(frame.rgba.clone()),
        }
        picture.frame += 1;
        picture.show(&self.pixels, &self.backdrop);
        self.size.set(Some((frame.width, frame.height)));
    }
}

/// The pictures the monitor holds for one video lane: its own, and during
/// a transition on it the picture it goes to.
#[derive(Clone, Default)]
pub struct LaneSlots {
    pub program: Slot,
    pub incoming: Slot,
}

/// Every lane's pictures, made as lanes first show.
#[derive(Clone, Default)]
pub struct Pictures(Rc<RefCell<Vec<LaneSlots>>>);

impl Pictures {
    /// Lane `lane`'s pictures; cheap, they share the held frames.
    pub fn lane(&self, lane: usize) -> LaneSlots {
        let mut lanes = self.0.borrow_mut();
        if lanes.len() <= lane {
            lanes.resize_with(lane + 1, LaneSlots::default);
        }
        lanes[lane].clone()
    }
}

/// Where the editor sends each decoded frame: each lane's picture, and
/// during a transition the picture it goes to. Rune uploads each and wakes
/// the window.
pub struct Sink {
    pub pictures: Pictures,
}

impl PreviewSink for Sink {
    fn show(&mut self, lane: usize, frame: &RawFrame) {
        self.pictures.lane(lane).program.show(frame);
    }

    fn clear(&mut self, lane: usize) {
        self.pictures.lane(lane).program.size.set(None);
    }

    fn show_incoming(&mut self, lane: usize, frame: &RawFrame) {
        self.pictures.lane(lane).incoming.show(frame);
    }
}

/// `clip`'s picture, held in `slot`, on a `w` by `h` monitor showing a
/// `canvas` sized frame: its background, then the picture, cropped, turned
/// and moved as `place` says. `decoded` is the held frame's size, used when
/// the clip's media is not in the bin. Its parts are keyed `{key}-…`.
pub fn placed(
    ui: &Ui,
    slot: &Slot,
    key: &str,
    clip: &Clip,
    canvas: (u32, u32),
    (w, h): (f32, f32),
    decoded: (u32, u32),
) -> El {
    let (cw, ch) = (f64::from(canvas.0), f64::from(canvas.1));
    let source = clip
        .source
        .as_deref()
        .and_then(|path| ui.editor.asset(path))
        .map_or(decoded, |asset| (asset.info.width, asset.info.height));
    let (sw, sh) = (f64::from(source.0.max(1)), f64::from(source.1.max(1)));
    // Keyed values as they are under the playhead.
    let time = ui.editor.playhead;
    let t = &clip.transform_at(time);
    let p = place((cw, ch), (sw, sh), t);
    let k = f64::from(w) / cw;
    let mut layer = el()
        .absolute(0.0, 0.0)
        .size(w, h)
        .opacity(clip.opacity_at(time).clamp(0.0, 1.0))
        .instant_changes();
    if clip.background == Background::Blur && !p.covers((cw, ch)) {
        // Rune blurs in transparency past an element's edges, which darkens
        // them; export repeats the edge. Three sigmas past the frame on each
        // side, the dark rim falls outside it.
        let sigma = BLUR_SHARE * w;
        let rim = 3.0 * sigma;
        // A mask clears the picture, not its blurred copy.
        let pixels = if clip.mask.is_some() {
            &slot.backdrop
        } else {
            &slot.pixels
        };
        layer = layer.child(
            el().key(format!("{key}-background"))
                .absolute(-rim, -rim)
                .size(w + 2.0 * rim, h + 2.0 * rim)
                .pixels_fit(pixels, SurfaceFit::Fill)
                .blur(sigma),
        );
    }
    // The pixels are already flipped, so the crop window mirrors with them.
    let [crop_x, crop_y, crop_w, crop_h] = p.crop;
    let crop_x = if t.flip_h {
        sw - crop_x - crop_w
    } else {
        crop_x
    };
    let crop_y = if t.flip_v {
        sh - crop_y - crop_h
    } else {
        crop_y
    };
    let per = p.size[0] / crop_w * k;
    let whole = el()
        .key(format!("{key}-picture"))
        .absolute((-crop_x * per) as f32, (-crop_y * per) as f32)
        .size((sw * per) as f32, (sh * per) as f32)
        .pixels_fit(&slot.pixels, SurfaceFit::Fill);
    let [ox, oy] = p.origin();
    let (bw, bh) = ((p.size[0] * k) as f32, (p.size[1] * k) as f32);
    let mut shown = el().size(bw, bh).child(whole);
    if t.has_crop() {
        shown = shown.clip_rounded(0.0);
    }
    // Turned about its centre, outside the crop's clip.
    let turned = el()
        .key(format!("{key}-placed"))
        .absolute((ox * k) as f32, (oy * k) as f32)
        .size(bw, bh)
        .rotate(t.rotation.to_radians() as f32)
        .child(shown);
    layer.child(turned)
}

/// Half the side of the square of decoded pixels a pick averages, so
/// noise in the screen does not decide the colour.
const PICK_REACH: usize = 2;

/// Key clip `id` on the colour under canvas point `point` in the frame its
/// lane holds, as decoded (before its flips, grade or key).
fn pick_key(ui: &mut Ui, id: ClipId, point: (f64, f64)) {
    let Some(lane) = ui.editor.timeline.clip(id).map(|clip| clip.track) else {
        ui.key_pick = None;
        return;
    };
    let Some((u, v)) = ui.editor.picture_point(id, point) else {
        ui.editor.status = "Click on the clip's own picture to pick its key colour.".to_owned();
        return;
    };
    let slot = ui.pictures.lane(lane).program;
    let colour = {
        let picture = slot.picture.borrow();
        let (width, rgba, _) = picture.upright();
        let width = width as usize;
        let height = rgba.len() / (width * 4).max(1);
        if height == 0 {
            ui.editor.status = "The clip's picture has not loaded yet.".to_owned();
            return;
        }
        let x = ((u * width as f64) as usize).min(width - 1);
        let y = ((v * height as f64) as usize).min(height - 1);
        let (mut sum, mut count) = ([0u32; 3], 0u32);
        for row in y.saturating_sub(PICK_REACH)..=(y + PICK_REACH).min(height - 1) {
            for column in x.saturating_sub(PICK_REACH)..=(x + PICK_REACH).min(width - 1) {
                let at = (row * width + column) * 4;
                for (total, value) in sum.iter_mut().zip(&rgba[at..at + 3]) {
                    *total += u32::from(*value);
                }
                count += 1;
            }
        }
        let [r, g, b] = sum.map(|total| ((total + count / 2) / count) as u8);
        Rgb(r, g, b)
    };
    ui.key_pick = None;
    ui.editor.edit_clip_chroma(id, |key| {
        key.get_or_insert_default().color = colour;
    });
    ui.editor.status = format!("Keying out {}.", colour.hex());
}

/// Drag the top picture under the pointer to move its clip, which it
/// selects. It lands on the canvas centre when near it, and Esc puts it
/// back. While Details picks a key colour (`picking`), a click picks it
/// instead.
pub fn draggable(frame: El, picking: bool) -> El {
    let cursor = if picking {
        Cursor::Crosshair
    } else {
        Cursor::Move
    };
    frame.cursor(cursor).on_drag(|ui: &mut Ui, drag, _| {
        let (cw, ch) = ui.editor.canvas();
        // The pointer on the canvas.
        let point = (
            f64::from(drag.x / drag.w.max(1.0)) * f64::from(cw),
            f64::from(drag.y / drag.h.max(1.0)) * f64::from(ch),
        );
        if let (Phase::Down, Some(id)) = (drag.phase, ui.key_pick) {
            pick_key(ui, id, point);
            return;
        }
        let editor = &mut ui.editor;
        match drag.phase {
            Phase::Down => {
                // The top picture under the pointer.
                let clip = editor
                    .picture_at(point)
                    .and_then(|id| editor.timeline.clip(id));
                let playhead = editor.playhead;
                ui.monitor_drag = clip.map(|clip| (clip.clone(), clip.transform_at(playhead)));
                if let Some((clip, _)) = &ui.monitor_drag {
                    editor.selected = Some(clip.id);
                }
            }
            Phase::Up => {
                // A key the drag added or moved goes too.
                if let Some((before, _)) = ui.monitor_drag.take().filter(|_| drag.cancelled())
                    && let Some(clip) = editor.timeline.clip_mut(before.id)
                {
                    clip.transform = before.transform;
                    clip.keyframes = before.keyframes;
                }
            }
            _ => {
                let Some((clip, before)) = &ui.monitor_drag else {
                    return;
                };
                let (id, before) = (clip.id, *before);
                let along = |start: f64, travel: f32, size: f32| {
                    let moved = start * f64::from(size) + f64::from(travel);
                    if moved.abs() < f64::from(CENTRE_SNAP) {
                        0.0
                    } else {
                        moved / f64::from(size.max(1.0))
                    }
                };
                let (x, y) = (
                    along(before.x, drag.dx, drag.w),
                    along(before.y, drag.dy, drag.h),
                );
                editor.edit_clip_transform(id, |t| {
                    t.x = x;
                    t.y = y;
                });
            }
        }
    })
}

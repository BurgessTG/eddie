//! The program monitor's picture: the decoded frame, where
//! `openreel_engine::place` puts it on the canvas (as export does), and a
//! drag that moves it. Rune draws images upright only and has no colour
//! filter for them, so flips and colour happen to the pixels here.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use eddie_core::PreviewSink;
use openreel_engine::{Background, Clip, ColorAdjust, Transform, place};
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
    pub color: ColorAdjust,
}

/// The look of the video clip at the playhead, for [`Picture::want`].
pub fn look(clip: Option<&Clip>) -> Look {
    clip.map_or_else(Look::default, |clip| {
        let Transform { flip_h, flip_v, .. } = clip.transform;
        Look {
            flips: (flip_h, flip_v),
            color: clip.color,
        }
    })
}

/// The last decoded frame, upright and ungraded, and the look it is shown
/// with.
#[derive(Default)]
pub struct Picture {
    want: Look,
    shown: Look,
    width: u32,
    rgba: Vec<u8>,
    /// Counts decoded frames, so a reader can tell a new one arrived.
    frame: u64,
}

impl Picture {
    /// Show frames with the clip at the playhead's look, redrawing the held
    /// frame when it changed (a flip toggled or a slider moved while
    /// paused).
    pub fn want(&mut self, look: Look, pixels: &Pixels) {
        self.want = look;
        if self.shown != look && !self.rgba.is_empty() {
            self.show(pixels);
        }
    }

    /// The held frame as decoded, unflipped and ungraded: its width,
    /// pixels and number.
    pub fn upright(&self) -> (u32, &[u8], u64) {
        (self.width, &self.rgba, self.frame)
    }

    fn show(&mut self, pixels: &Pixels) {
        self.shown = self.want;
        let height = (self.rgba.len() / (self.width as usize * 4).max(1)) as u32;
        let mut shown = flipped(&self.rgba, self.width, self.want.flips);
        self.want.color.apply_rgba(&mut shown);
        pixels.set(self.width, height, shown);
    }
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
    pub size: Rc<Cell<Option<(u32, u32)>>>,
    pub picture: Rc<RefCell<Picture>>,
}

impl Slot {
    /// Show the held frame as the clip `look` describes wants it.
    pub fn want(&self, look: Look) {
        self.picture.borrow_mut().want(look, &self.pixels);
    }

    fn show(&self, frame: &RawFrame) {
        let mut picture = self.picture.borrow_mut();
        picture.width = frame.width;
        picture.rgba.clear();
        picture.rgba.extend_from_slice(&frame.rgba);
        picture.frame += 1;
        picture.show(&self.pixels);
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
    let t = &clip.transform;
    let p = place((cw, ch), (sw, sh), t);
    let k = f64::from(w) / cw;
    let mut layer = el()
        .absolute(0.0, 0.0)
        .size(w, h)
        .opacity(clip.opacity.clamp(0.0, 1.0))
        .instant_changes();
    if clip.background == Background::Blur && !p.covers((cw, ch)) {
        // Rune blurs in transparency past an element's edges, which darkens
        // them; export repeats the edge. Three sigmas past the frame on each
        // side, the dark rim falls outside it.
        let sigma = BLUR_SHARE * w;
        let rim = 3.0 * sigma;
        layer = layer.child(
            el().key(format!("{key}-background"))
                .absolute(-rim, -rim)
                .size(w + 2.0 * rim, h + 2.0 * rim)
                .pixels_fit(&slot.pixels, SurfaceFit::Fill)
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

/// Drag the top picture under the pointer to move its clip, which it
/// selects. It lands on the canvas centre when near it, and Esc puts it
/// back.
pub fn draggable(frame: El) -> El {
    frame.cursor(Cursor::Move).on_drag(|ui: &mut Ui, drag, _| {
        let editor = &mut ui.editor;
        match drag.phase {
            Phase::Down => {
                // The top picture under the pointer, on the canvas.
                let (cw, ch) = editor.canvas();
                let point = (
                    f64::from(drag.x / drag.w.max(1.0)) * f64::from(cw),
                    f64::from(drag.y / drag.h.max(1.0)) * f64::from(ch),
                );
                let clip = editor
                    .picture_at(point)
                    .and_then(|id| editor.timeline.clip(id));
                ui.monitor_drag = clip.map(|clip| (clip.id, clip.transform));
                if let Some((id, _)) = ui.monitor_drag {
                    editor.selected = Some(id);
                }
            }
            Phase::Up => {
                if let Some((id, before)) = ui.monitor_drag.take().filter(|_| drag.cancelled()) {
                    editor.edit_clip_transform(id, |t| *t = before);
                }
            }
            _ => {
                let Some((id, before)) = ui.monitor_drag else {
                    return;
                };
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

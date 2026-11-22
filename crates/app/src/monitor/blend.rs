//! Blended pictures on the monitor. Rune lays images over one another but
//! cannot mix them, so while a lane at the playhead has a blend mode, the
//! lanes from the bottom up to the top-most blended one are laid on the
//! CPU with [`openreel_media::Canvas`], as export lays them, and shown as
//! one picture. The lanes above it draw as usual. A lane part way through
//! a transition draws its pictures normally, as export does. While the
//! lanes and clips stay the same, a worker thread lays each new moment.

use std::cell::{Cell, OnceCell, RefCell};
use std::sync::{Arc, mpsc};

use eddie_core::{Editor, ProgramLayer};
use openreel_engine::{Background, BlendMode, Clip, ClipId, TransitionSpan, place};
use openreel_media::{Canvas, Image, Layer};
use rune::{El, Pixels, SurfaceFit, el};

use super::picture::Slot;
use crate::app::Ui;

/// The longer side of the laid picture, in pixels: the preview's decode
/// width, so a 16:9 canvas is laid at the pictures' own size.
const LONG_SIDE: f64 = 960.0;

/// The lanes and clips a laid picture holds, bottom first.
type Covers = Vec<(usize, ClipId)>;

/// The laid picture and what it holds.
#[derive(Default)]
pub struct Composite {
    pixels: Pixels,
    covers: RefCell<Covers>,
    /// The moment last laid or sent to be laid, so an unchanged one is not
    /// laid again.
    asked: RefCell<Option<Made>>,
    /// Numbers each laying, and the one shown: a worker's answer older
    /// than the shown picture is dropped.
    serial: Cell<u64>,
    shown: Cell<u64>,
    worker: OnceCell<Worker>,
}

/// The thread that lays pictures while the lanes and clips stay the same,
/// such as during playback, so the UI thread does not wait for it.
struct Worker {
    jobs: mpsc::Sender<Job>,
    laid: mpsc::Receiver<Laid>,
}

struct Laid {
    serial: u64,
    covers: Covers,
    canvas: Canvas,
}

/// Everything one laying needs, owned, so the worker can lay it.
struct Job {
    serial: u64,
    made: Made,
    covers: Covers,
    /// Per lane of `made`: its picture and the one a transition goes to.
    pictures: Vec<[Held; 2]>,
}

/// A held frame and the displayed size of the file it comes from.
struct Held {
    width: u32,
    rgba: Arc<Vec<u8>>,
    source: (f64, f64),
}

/// Everything the laid picture depends on.
#[derive(Clone, PartialEq)]
struct Made {
    time: f64,
    canvas: (u32, u32),
    /// Per lane: its layer, the clips it draws, and the frame numbers of
    /// the pictures it holds.
    lanes: Vec<(ProgramLayer, Clip, Option<Clip>, u64, u64)>,
}

impl Composite {
    /// How many of `layers` (bottom first) the laid picture holds: all of
    /// them up to the top-most blended one, or none when there is none.
    fn holds(&self, layers: &[ProgramLayer]) -> usize {
        let covers = self.covers.borrow();
        let same = covers.len() <= layers.len()
            && covers
                .iter()
                .zip(layers)
                .all(|(&(lane, clip), layer)| layer.lane == lane && layer.clip == clip);
        if same { covers.len() } else { 0 }
    }

    fn clear(&self) {
        self.covers.borrow_mut().clear();
        *self.asked.borrow_mut() = None;
        self.shown.set(self.serial.get());
    }

    fn next(&self) -> u64 {
        self.serial.set(self.serial.get() + 1);
        self.serial.get()
    }

    /// Show `laid` unless a later picture already shows.
    fn show(&self, laid: Laid) {
        if laid.serial <= self.shown.get() {
            return;
        }
        self.shown.set(laid.serial);
        let canvas = laid.canvas;
        self.pixels
            .set(canvas.width as u32, canvas.height as u32, canvas.rgba);
        *self.covers.borrow_mut() = laid.covers;
    }

    /// The worker, started the first time it is needed.
    fn worker(&self, ui: &Ui) -> &Worker {
        self.worker.get_or_init(|| {
            let (jobs, inbox) = mpsc::channel::<Job>();
            let (out, laid) = mpsc::channel();
            let wake = ui.waker();
            std::thread::Builder::new()
                .name("blend".to_owned())
                .spawn(move || {
                    while let Ok(mut job) = inbox.recv() {
                        // Only the newest moment is worth laying.
                        while let Ok(newer) = inbox.try_recv() {
                            job = newer;
                        }
                        let canvas = lay(&job);
                        let laid = Laid {
                            serial: job.serial,
                            covers: job.covers,
                            canvas,
                        };
                        if out.send(laid).is_err() {
                            break;
                        }
                        wake();
                    }
                })
                .ok();
            Worker { jobs, laid }
        })
    }
}

/// The lane at or under which every picture is laid on the CPU: the
/// top-most that shows with a blend mode outside a transition.
fn top_blended(ui: &Ui, layers: &[ProgramLayer]) -> Option<usize> {
    let timeline = &ui.editor.timeline;
    layers.iter().rposition(|layer| {
        layer.shown
            && layer.transition.is_none()
            && timeline
                .clip(layer.clip)
                .is_some_and(|clip| !clip.blend.is_normal())
    })
}

/// Lay the lanes under the top-most blended picture at the playhead, when
/// what they show changed since the last time. The same lanes and clips at
/// a new moment are laid on the worker, and the last picture shows until
/// it answers; other lanes or clips are laid at once, so no frame shows
/// them unmixed.
pub fn compose(ui: &Ui) {
    let composite = &ui.composite;
    if let Some(worker) = composite.worker.get()
        && let Some(laid) = worker.laid.try_iter().last()
    {
        composite.show(laid);
    }
    let Some(job) = job(ui) else {
        composite.clear();
        return;
    };
    if composite.asked.borrow().as_ref() == Some(&job.made) {
        return;
    }
    *composite.asked.borrow_mut() = Some(job.made.clone());
    let job = if *composite.covers.borrow() == job.covers {
        match composite.worker(ui).jobs.send(job) {
            Ok(()) => return,
            // No worker thread: lay it here.
            Err(mpsc::SendError(job)) => job,
        }
    } else {
        job
    };
    composite.show(Laid {
        canvas: lay(&job),
        serial: job.serial,
        covers: job.covers,
    });
}

/// The lanes laid at this moment, laid afresh.
#[cfg(test)]
pub(super) fn laid(ui: &Ui) -> Option<Canvas> {
    job(ui).map(|job| lay(&job))
}

/// What the lanes up to the top-most blended picture show at the playhead,
/// with their pictures; none without a blended one.
fn job(ui: &Ui) -> Option<Job> {
    let editor = &ui.editor;
    let layers = editor.program_layers();
    let top = top_blended(ui, &layers)?;
    let timeline = &editor.timeline;
    let mut lanes = Vec::new();
    let mut pictures = Vec::new();
    for layer in &layers[..=top] {
        let Some(clip) = timeline.clip(layer.clip) else {
            continue;
        };
        let incoming = layer.transition.and_then(|span| timeline.clip(span.to));
        let slots = ui.pictures.lane(layer.lane);
        let (program, program_frame) = held(editor, &slots.program, Some(clip));
        let (to, incoming_frame) = held(editor, &slots.incoming, incoming);
        lanes.push((
            *layer,
            clip.clone(),
            incoming.cloned(),
            program_frame,
            incoming_frame,
        ));
        pictures.push([program, to]);
    }
    let covers = layers[..=top]
        .iter()
        .map(|layer| (layer.lane, layer.clip))
        .collect();
    Some(Job {
        serial: ui.composite.next(),
        made: Made {
            time: editor.playhead,
            canvas: editor.canvas(),
            lanes,
        },
        covers,
        pictures,
    })
}

/// The frame `slot` holds, shared, with the displayed size of `clip`'s
/// file (the frame's own size when it is not in the bin), and its number.
fn held(editor: &Editor, slot: &Slot, clip: Option<&Clip>) -> (Held, u64) {
    let picture = slot.picture.borrow();
    let (width, rgba) = picture.shared();
    let height = (rgba.len() / (width as usize * 4).max(1)) as u32;
    let source = clip
        .and_then(|clip| clip.source.as_deref())
        .and_then(|path| editor.asset(path))
        .map_or((width, height), |asset| {
            (asset.info.width, asset.info.height)
        });
    let source = (f64::from(source.0.max(1)), f64::from(source.1.max(1)));
    let frame = picture.upright().2;
    (
        Held {
            width,
            rgba,
            source,
        },
        frame,
    )
}

/// The pictures `job` names, laid bottom first on a canvas whose longer
/// side is at most [`LONG_SIDE`].
fn lay(job: &Job) -> Canvas {
    let made = &job.made;
    let (cw, ch) = (f64::from(made.canvas.0), f64::from(made.canvas.1));
    let k = (LONG_SIDE / cw.max(ch)).min(1.0);
    let (w, h) = (
        ((cw * k).round() as usize).max(2),
        ((ch * k).round() as usize).max(2),
    );
    let mut canvas = Canvas::new(w, h, (cw, ch));
    for ((layer, clip, incoming, ..), [program, to_held]) in made.lanes.iter().zip(&job.pictures) {
        if !layer.shown {
            continue;
        }
        let span = layer.transition.filter(|_| layer.incoming_shown);
        match (span, incoming) {
            (Some(span), Some(to)) => {
                transition(&mut canvas, made, span, (clip, program), (to, to_held))
            }
            _ => draw(&mut canvas, made, clip, program, Side::default()),
        }
    }
    canvas
}

/// Where one picture of a transition goes: moved, cut to a window, faded,
/// and drawn without its blend mode.
#[derive(Clone, Copy)]
struct Side {
    shift: [f64; 2],
    window: Option<[f64; 4]>,
    opacity: f32,
    transition: bool,
}

impl Default for Side {
    fn default() -> Self {
        Self {
            shift: [0.0, 0.0],
            window: None,
            opacity: 1.0,
            transition: false,
        }
    }
}

/// `clip`'s picture, `held`, laid over `canvas` as it shows at `made`'s
/// time.
fn draw(canvas: &mut Canvas, made: &Made, clip: &Clip, held: &Held, side: Side) {
    let (width, rgba) = (held.width, held.rgba.as_slice());
    if width == 0 || rgba.is_empty() {
        return;
    }
    let height = (rgba.len() / (width as usize * 4)) as u32;
    let graded;
    let rgba = if clip.color.is_identity() && clip.chroma.is_none() {
        rgba
    } else {
        let mut pixels = rgba.to_vec();
        if let Some(key) = &clip.chroma {
            key.apply_rgba(&mut pixels, width as usize);
        }
        clip.color.apply_rgba(&mut pixels);
        graded = pixels;
        &graded
    };
    let canvas_f = (f64::from(made.canvas.0), f64::from(made.canvas.1));
    let time = made.time;
    canvas.draw(&Layer {
        image: Image {
            width,
            height,
            rgba,
        },
        source: held.source,
        placement: place(canvas_f, held.source, &clip.transform_at(time)),
        opacity: clip.opacity_at(time) * side.opacity,
        blur: clip.background == Background::Blur,
        blend: if side.transition {
            BlendMode::Normal
        } else {
            clip.blend
        },
        shift: side.shift,
        window: side.window,
    });
}

/// A transition at `made`'s time, as [`super::transition::layers`] draws
/// it: a mix or dip passes between whole frames, each picture over the
/// lanes under it; a wipe or slide moves only the pictures.
fn transition(
    canvas: &mut Canvas,
    made: &Made,
    span: TransitionSpan,
    (from, from_held): (&Clip, &Held),
    (to, to_held): (&Clip, &Held),
) {
    let frame = span.kind.frame(span.progress(made.time), made.canvas);
    let side = |layer: openreel_engine::Layer, whole: bool| Side {
        shift: if whole { [0.0, 0.0] } else { layer.offset },
        window: (!whole).then_some(layer.window),
        opacity: if whole { 1.0 } else { layer.opacity as f32 },
        transition: true,
    };
    if span.kind.blends() {
        let mut a = canvas.clone();
        draw(&mut a, made, from, from_held, side(frame.from, true));
        let mut b = canvas.clone();
        draw(&mut b, made, to, to_held, side(frame.to, true));
        let (cw, ch) = made.canvas;
        let mut out = Canvas::new(canvas.width, canvas.height, (f64::from(cw), f64::from(ch)));
        out.mix(&a, frame.from.opacity as f32);
        out.mix(&b, frame.to.opacity as f32);
        *canvas = out;
    } else {
        draw(canvas, made, from, from_held, side(frame.from, false));
        draw(canvas, made, to, to_held, side(frame.to, false));
    }
    if let Some((white, amount)) = frame.veil {
        let level = if white { 255 } else { 0 };
        canvas.veil([level; 3], amount as f32);
    }
}

/// The laid picture on a `w` by `h` monitor, when it holds `layers`' first
/// ones: it and how many it holds.
pub(super) fn view(ui: &Ui, layers: &[ProgramLayer], (w, h): (f32, f32)) -> Option<(El, usize)> {
    let holds = ui.composite.holds(layers);
    (holds > 0).then(|| {
        (
            el().key("monitor-blended")
                .absolute(0.0, 0.0)
                .size(w, h)
                .pixels_fit(&ui.composite.pixels, SurfaceFit::Fill),
            holds,
        )
    })
}

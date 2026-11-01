//! Realtime video playback: an in-process paced decoder stream and the
//! two-lane preview worker. Audio lives in `audio.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::{MediaResult, RawFrame, VideoDecoder};

/// Streaming decoder on its own thread. Paced by presentation timestamps,
/// frame-rate capped, and buffer-pooled. `spawn` is a standalone paced stream;
/// `spawn_preview` follows an external clock with replaceable requests.
/// Dropping requests cancellation without waiting for native work to finish.
pub struct FrameStream {
    /// Standalone `spawn` output. Preview mode uses `preview_frame` instead.
    pub frames: Receiver<RawFrame>,
    recycle_tx: SyncSender<Vec<u8>>,
    stop_flag: Arc<AtomicBool>,
    handles: Vec<thread::JoinHandle<()>>,
    preview: Option<Arc<PreviewMailbox>>,
}

impl FrameStream {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        path: &str,
        _src_w: u32,
        _src_h: u32,
        start: f64,
        target_w: u32,
        fps: f64,
    ) -> MediaResult<Self> {
        let (tx, rx) = sync_channel::<RawFrame>(3);
        // Buffer pool: the consumer returns spent frame buffers so steady
        // state playback performs zero frame-sized allocations.
        let (recycle_tx, recycle_rx) = sync_channel::<Vec<u8>>(4);
        let (init_tx, init_rx) = sync_channel::<MediaResult<()>>(1);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop_flag);
        let frame_interval = if fps > 0.0 { 1.0 / fps } else { 0.0 };
        let start = start.max(0.0);
        let path = path.to_owned();

        let handle = thread::spawn(move || {
            // The scaler context is not `Send`, so the decoder is created
            // and used entirely on this thread.
            let mut decoder = match VideoDecoder::open(&path, target_w, true)
                .and_then(|mut d| d.seek(start).map(|()| d))
            {
                Ok(decoder) => {
                    let _ = init_tx.send(Ok(()));
                    decoder
                }
                Err(e) => {
                    let _ = init_tx.send(Err(e));
                    return;
                }
            };
            let (out_w, out_h) = decoder.output_size();
            let frame_bytes = (out_w * out_h * 4) as usize;
            let started = Instant::now();
            let mut next_due_pts = f64::NEG_INFINITY;
            let mut spare: Option<Vec<u8>> = None;
            'stream: loop {
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                let buffer = spare
                    .take()
                    .or_else(|| {
                        recycle_rx
                            .try_recv()
                            .ok()
                            .filter(|v| v.len() == frame_bytes)
                    })
                    .unwrap_or_else(|| vec![0_u8; frame_bytes]);
                let Some((pts, mut frame)) = decoder.next_frame(buffer) else {
                    break;
                };
                if pts + 1e-6 < next_due_pts {
                    // Above the requested frame-rate cap: keep the buffer.
                    spare = Some(std::mem::take(&mut frame.rgba));
                    continue;
                }
                next_due_pts = pts.max(start) + frame_interval;

                // The first frame covers start, which can lie inside a long
                // VFR interval. Present it now, then wait only the remaining
                // interval, not the whole duration from that frame's PTS.
                let due = Duration::from_secs_f64((pts - start).max(0.0));
                loop {
                    let elapsed = started.elapsed();
                    if elapsed + Duration::from_millis(2) >= due {
                        break;
                    }
                    if thread_stop.load(Ordering::Relaxed) {
                        break 'stream;
                    }
                    thread::sleep((due - elapsed).min(Duration::from_millis(5)));
                }

                loop {
                    match tx.try_send(frame) {
                        Ok(()) => break,
                        Err(std::sync::mpsc::TrySendError::Full(returned)) => {
                            if thread_stop.load(Ordering::Relaxed) {
                                break 'stream;
                            }
                            frame = returned;
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                            break 'stream;
                        }
                    }
                }
            }
        });

        match init_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                frames: rx,
                recycle_tx,
                stop_flag,
                handles: vec![handle],
                preview: None,
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                Err("decoder thread exited during startup".to_owned())
            }
        }
    }

    /// Start the reusable preview workers, without opening media on the caller.
    /// There is one pending request and one latest result, not a decoder/thread
    /// per clip. Two lanes each own at most one decoder: one serves the active
    /// request while the other prerolls `prefetch_preview`'s upcoming clip.
    /// Native open/seek/decode cannot be preempted; superseded work is
    /// discarded at the next native-operation boundary.
    pub fn spawn_preview(wake: impl Fn() + Send + Sync + 'static) -> MediaResult<Self> {
        let (_, frames) = sync_channel(0);
        let (recycle_tx, recycle_rx) = sync_channel(PREVIEW_POOL);
        let pool = Arc::new(BufferPool {
            put: recycle_tx.clone(),
            get: Mutex::new(recycle_rx),
        });
        let preview = Arc::new(PreviewMailbox::default());
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let mut handles = Vec::with_capacity(PREVIEW_LANES);
        for lane in 0..PREVIEW_LANES {
            let (mailbox, pool, wake) =
                (Arc::clone(&preview), Arc::clone(&pool), Arc::clone(&wake));
            let spawned = thread::Builder::new()
                .name(format!("eddie-preview-{lane}"))
                .spawn(move || preview_lane(&mailbox, lane, &pool, &*wake));
            match spawned {
                Ok(handle) => handles.push(handle),
                Err(e) => {
                    preview.state.lock().unwrap().closed = true;
                    preview.changed.notify_all();
                    return Err(format!("start preview worker: {e}"));
                }
            }
        }
        Ok(Self {
            frames,
            recycle_tx,
            stop_flag: Arc::new(AtomicBool::new(false)),
            handles,
            preview: Some(preview),
        })
    }

    /// Replace the desired clip/seek. The returned generation identifies its
    /// frames and errors; replacing a queued request never opens its decoder.
    /// A request at or after the prefetched picture's time on the same file
    /// and width is served by the prefetch lane instead of a cold preroll.
    pub fn request_preview(
        &self,
        path: &str,
        source_time: f64,
        target_w: u32,
        fps: f64,
    ) -> MediaResult<u64> {
        if !source_time.is_finite() || !fps.is_finite() || target_w == 0 {
            return Err("invalid preview time, frame rate or size".to_owned());
        }
        let preview = self.preview.as_ref().ok_or("not a preview worker")?;
        let source_time = source_time.max(0.0);
        let mut state = preview.state.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        state.time = source_time;
        state.result = None;
        let other = 1 - state.active;
        if state.lanes[other].prefetch.as_ref().is_some_and(|target| {
            target.path == path && target.target_w == target_w && target.time <= source_time
        }) {
            // Hand over without bumping the serial: an unfinished preroll for
            // this target continues and then serves the request.
            state.lanes[other].prefetch = None;
            state.lanes[other].ready = false;
            let previous = state.active;
            state.lanes[previous].request = None;
            state.active = other;
            state.handoffs += 1;
        }
        let generation = state.generation;
        let active = state.active;
        state.lanes[active].request = Some(PreviewRequest {
            generation,
            path: path.to_owned(),
            target_w,
            frame_interval: if fps > 0.0 { 1.0 / fps } else { 0.0 },
        });
        state.active_source = Some((path.to_owned(), target_w, source_time));
        preview.changed.notify_all();
        Ok(generation)
    }

    /// Preroll the picture at `source_time` on the idle lane, so a later
    /// `request_preview` at that time can show it without opening, seeking or
    /// decoding first. Repeating the current target is free. A target that
    /// continues the active request's file forward is left to the active
    /// lane, which decodes forward instead of reseeking.
    pub fn prefetch_preview(&self, path: &str, source_time: f64, target_w: u32) -> MediaResult<()> {
        if !source_time.is_finite() || target_w == 0 {
            return Err("invalid preview time or size".to_owned());
        }
        let preview = self.preview.as_ref().ok_or("not a preview worker")?;
        let time = source_time.max(0.0);
        let mut state = preview.state.lock().unwrap();
        let continues = state
            .active_source
            .as_ref()
            .is_some_and(|(active, width, start)| {
                active == path && *width == target_w && *start <= time
            });
        let target = (!continues).then(|| PrefetchTarget {
            path: path.to_owned(),
            time,
            target_w,
        });
        let other = 1 - state.active;
        let lane = &mut state.lanes[other];
        if lane.prefetch != target {
            lane.prefetch = target;
            lane.serial = lane.serial.wrapping_add(1);
            lane.ready = false;
            preview.changed.notify_all();
        }
        Ok(())
    }

    /// Whether the idle lane holds the first picture of exactly this target.
    pub fn prefetch_ready(&self, path: &str, source_time: f64, target_w: u32) -> bool {
        let Some(preview) = &self.preview else {
            return false;
        };
        let state = preview.state.lock().unwrap();
        let lane = &state.lanes[1 - state.active];
        lane.ready
            && lane.prefetch.as_ref().is_some_and(|target| {
                target.path == path
                    && target.target_w == target_w
                    && target.time == source_time.max(0.0)
            })
    }

    /// Requests served by the prefetch lane since the worker started.
    pub fn prefetch_handoffs(&self) -> u64 {
        self.preview
            .as_ref()
            .map_or(0, |preview| preview.state.lock().unwrap().handoffs)
    }

    /// Advance the preview's clock and take its newest available picture or
    /// error. The clock is the caller's source time, not time since decoder
    /// startup. A late consumer skips intermediate pictures instead of replaying
    /// a queue. Call `request_preview` for a backward seek or a different clip.
    pub fn preview_frame(
        &self,
        generation: u64,
        source_time: f64,
    ) -> Option<MediaResult<RawFrame>> {
        let preview = self.preview.as_ref()?;
        let mut state = preview.state.lock().unwrap();
        if generation != state.generation || state.closed {
            return None;
        }
        if !source_time.is_finite() {
            return Some(Err("invalid preview time".to_owned()));
        }
        state.time = source_time.max(0.0);
        preview.changed.notify_all();
        // Never show a future picture. Late pictures are still useful while
        // decode catches up; rejecting every expired source interval can starve
        // a 30 Hz UI consuming a 60 Hz source indefinitely.
        if matches!(state.result.as_ref(), Some(Ok((pts, _))) if *pts > state.time) {
            return None;
        }
        state
            .result
            .take()
            .map(|result| result.map(|(_, frame)| frame))
    }

    /// Cancel a preview generation and any prefetch immediately, retaining
    /// the workers. Unlike `stop`, this does not join a thread on the caller.
    pub fn cancel_preview(&self) {
        if let Some(preview) = &self.preview {
            let mut state = preview.state.lock().unwrap();
            state.generation = state.generation.wrapping_add(1);
            state.result = None;
            state.active_source = None;
            for lane in &mut state.lanes {
                lane.request = None;
                if lane.prefetch.take().is_some() {
                    lane.serial = lane.serial.wrapping_add(1);
                }
                lane.ready = false;
            }
            preview.changed.notify_all();
        }
    }

    /// Return a spent frame buffer for reuse (no-op if the pool is full).
    pub fn recycle(&self, buffer: Vec<u8>) {
        let _ = self.recycle_tx.try_send(buffer);
    }

    fn request_stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(preview) = &self.preview {
            // Set the condition under the waiter's lock to avoid a lost wake.
            preview.state.lock().unwrap().closed = true;
            preview.changed.notify_all();
        }
    }

    /// Stop and join the workers. Do not use this blocking teardown on the UI
    /// thread; preview clip switches and seeks use `cancel_preview` instead.
    pub fn stop(mut self) {
        self.request_stop();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for FrameStream {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Active lane plus one prefetch lane: enough to preroll the next cut while
/// the current clip plays, without a decoder per timeline clip.
const PREVIEW_LANES: usize = 2;
/// Recycled preview buffers; more are only allocated when this runs dry.
const PREVIEW_POOL: usize = 8;
/// Without a container index, decode forward rather than seek only this far.
const FORWARD_WITHOUT_INDEX: f64 = 0.5;

/// A decoded picture and its source-relative presentation time.
type Picture = (f64, RawFrame);

struct PreviewRequest {
    generation: u64,
    path: String,
    target_w: u32,
    frame_interval: f64,
}

#[derive(Clone, PartialEq)]
struct PrefetchTarget {
    path: String,
    time: f64,
    target_w: u32,
}

#[derive(Default)]
struct LaneSlot {
    request: Option<PreviewRequest>,
    prefetch: Option<PrefetchTarget>,
    /// Bumped whenever `prefetch` changes target, cancelling stale preroll.
    serial: u64,
    /// The lane holds the first picture of `prefetch`.
    ready: bool,
}

#[derive(Default)]
struct PreviewState {
    generation: u64,
    time: f64,
    active: usize,
    lanes: [LaneSlot; PREVIEW_LANES],
    /// File, width and start of the latest request.
    active_source: Option<(String, u32, f64)>,
    handoffs: u64,
    result: Option<MediaResult<Picture>>,
    closed: bool,
}

#[derive(Default)]
struct PreviewMailbox {
    state: Mutex<PreviewState>,
    changed: Condvar,
}

impl PreviewMailbox {
    fn time(&self, generation: u64) -> Option<f64> {
        let state = self.state.lock().unwrap();
        (!state.closed && state.generation == generation).then_some(state.time)
    }

    fn wait_until(&self, generation: u64, due: f64) -> Option<f64> {
        let mut state = self.state.lock().unwrap();
        while !state.closed && state.generation == generation && state.time < due {
            state = self.changed.wait(state).unwrap();
        }
        (!state.closed && state.generation == generation).then_some(state.time)
    }

    /// Publish `picture` for `generation`, or hand it back if superseded.
    fn publish(
        &self,
        generation: u64,
        picture: Picture,
        pool: &BufferPool,
        wake: &dyn Fn(),
    ) -> Result<(), Picture> {
        let mut state = self.state.lock().unwrap();
        if state.closed || state.generation != generation {
            return Err(picture);
        }
        if let Some(Ok((_, replaced))) = state.result.replace(Ok(picture)) {
            pool.put(replaced.rgba);
        }
        drop(state);
        wake();
        Ok(())
    }
}

/// Frame buffers shared by the UI consumer and both lanes.
struct BufferPool {
    put: SyncSender<Vec<u8>>,
    get: Mutex<Receiver<Vec<u8>>>,
}

impl BufferPool {
    fn get(&self) -> Vec<u8> {
        self.get.lock().unwrap().try_recv().unwrap_or_default()
    }

    fn put(&self, buffer: Vec<u8>) {
        let _ = self.put.try_send(buffer);
    }
}

/// One lane's decoder and the undelivered pictures it already decoded.
#[derive(Default)]
struct Lane {
    decoder: Option<((String, u32), VideoDecoder)>,
    /// Earliest undelivered picture; the decoder is positioned after it (and
    /// after `pending`, the picture that follows it, when present).
    held: Option<Picture>,
    pending: Option<Picture>,
    /// `held` is known to cover source time up to this value.
    covers: f64,
}

impl Lane {
    fn release(&mut self, pool: &BufferPool) {
        for (_, frame) in self.held.take().into_iter().chain(self.pending.take()) {
            pool.put(frame.rgba);
        }
    }

    fn reset(&mut self, pool: &BufferPool) {
        self.release(pool);
        self.decoder = None;
    }

    /// Leave `held` as the picture covering `time` on `path`. Continues
    /// forward through already-decoded pictures when no keyframe seek would
    /// start later; otherwise seeks. Reuses the open decoder for its file.
    fn prepare(
        &mut self,
        path: &str,
        target_w: u32,
        time: f64,
        pool: &BufferPool,
    ) -> MediaResult<()> {
        if self
            .decoder
            .as_ref()
            .is_none_or(|((open, width), _)| open != path || *width != target_w)
        {
            // Release the previous native context before opening another.
            self.reset(pool);
            let decoder = VideoDecoder::open(path, target_w, true)?;
            self.decoder = Some(((path.to_owned(), target_w), decoder));
        }
        let decoder = &mut self.decoder.as_mut().unwrap().1;
        if self.held.is_none() {
            self.held = self.pending.take();
            self.covers = f64::NEG_INFINITY;
        }
        if let (Some(_), Some((next, _))) = (&self.held, &self.pending) {
            if *next <= time {
                let (_, skipped) = self.held.take().unwrap();
                pool.put(skipped.rgba);
                self.held = self.pending.take();
                self.covers = f64::NEG_INFINITY;
            }
        }
        let forward = self.held.as_ref().is_some_and(|(pts, _)| {
            *pts <= time
                && match decoder.keyframe_before(time) {
                    Some(keyframe) => keyframe <= *pts + 1e-6,
                    None => time - *pts <= FORWARD_WITHOUT_INDEX,
                }
        });
        if !forward {
            for (_, frame) in self.held.take().into_iter().chain(self.pending.take()) {
                pool.put(frame.rgba);
            }
            decoder.seek(time)?;
            let first = decoder
                .try_next_frame(pool.get())?
                .ok_or("no video frame at the requested time")?;
            self.held = Some(first);
            self.covers = time;
            return Ok(());
        }
        let known = self
            .pending
            .as_ref()
            .map_or(time <= self.covers, |(next, _)| *next > time);
        if !known {
            decoder.skip_to(time);
            match decoder.try_next_frame(pool.get())? {
                Some(next) if next.0 <= time => {
                    let (_, skipped) = self.held.replace(next).unwrap();
                    pool.put(skipped.rgba);
                }
                // Starts after `time`, or EOF: the held picture covers it.
                next => self.pending = next,
            }
        }
        self.covers = self.covers.max(time);
        Ok(())
    }
}

enum LaneWork {
    Show(PreviewRequest),
    Prefetch(PrefetchTarget, u64),
}

fn preview_lane(mailbox: &PreviewMailbox, index: usize, pool: &BufferPool, wake: &dyn Fn()) {
    let mut lane = Lane::default();
    let mut done_serial = 0;
    loop {
        let work = {
            let mut state = mailbox.state.lock().unwrap();
            loop {
                if state.closed {
                    return;
                }
                let slot = &mut state.lanes[index];
                if let Some(request) = slot.request.take() {
                    break LaneWork::Show(request);
                }
                if slot.serial != done_serial {
                    done_serial = slot.serial;
                    if let Some(target) = slot.prefetch.clone() {
                        break LaneWork::Prefetch(target, done_serial);
                    }
                    // A cancelled prefetch frees the idle lane's native
                    // decoder, so pausing holds no more than one.
                    if state.active != index {
                        drop(state);
                        lane.reset(pool);
                        state = mailbox.state.lock().unwrap();
                    }
                    continue;
                }
                state = mailbox.changed.wait(state).unwrap();
            }
        };
        match work {
            LaneWork::Prefetch(target, serial) => {
                let result = lane.prepare(&target.path, target.target_w, target.time, pool);
                if result.is_err() {
                    // The real request reopens and reports the error.
                    lane.reset(pool);
                } else {
                    let mut state = mailbox.state.lock().unwrap();
                    let slot = &mut state.lanes[index];
                    slot.ready = slot.serial == serial;
                }
            }
            LaneWork::Show(request) => {
                let result = (|| {
                    let Some(start) = mailbox.time(request.generation) else {
                        return Ok(());
                    };
                    lane.prepare(&request.path, request.target_w, start, pool)?;
                    let first = lane.held.take().unwrap();
                    let Lane {
                        decoder,
                        held,
                        pending,
                        ..
                    } = &mut lane;
                    let decoder = &mut decoder.as_mut().unwrap().1;
                    preview_decode(
                        &mut |buffer| decoder.try_next_frame(buffer),
                        mailbox,
                        &request,
                        start,
                        first,
                        held,
                        pending,
                        pool,
                        wake,
                    )
                })();
                if let Some(held) = &lane.held {
                    lane.covers = held.0;
                }
                if let Err(error) = result {
                    lane.reset(pool);
                    let _ = mailbox.publish_error(request.generation, error, wake);
                }
            }
        }
    }
}

impl PreviewMailbox {
    fn publish_error(&self, generation: u64, error: String, wake: &dyn Fn()) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.closed || state.generation != generation {
            return false;
        }
        state.result = Some(Err(error));
        drop(state);
        wake();
        true
    }
}

/// Show `first` (the picture covering `start`), then follow the caller's
/// clock. On cancellation, undelivered pictures are left in `held`/`pending`
/// with the decoder positioned after them, so the lane can continue forward.
#[allow(clippy::too_many_arguments)]
fn preview_decode(
    next_frame: &mut impl FnMut(Vec<u8>) -> MediaResult<Option<Picture>>,
    mailbox: &PreviewMailbox,
    request: &PreviewRequest,
    start: f64,
    first: Picture,
    held: &mut Option<Picture>,
    pending: &mut Option<Picture>,
    pool: &BufferPool,
    wake: &dyn Fn(),
) -> MediaResult<()> {
    let generation = request.generation;
    let mut pull = |pending: &mut Option<Picture>| match pending.take() {
        Some(picture) => Ok(Some(picture)),
        None => next_frame(pool.get()),
    };
    // Native seeking clamps before the first source picture and at EOF, and
    // the first picture covers `start`, so it is shown without decoding on.
    let (first_pts, first_frame) = first;
    if let Err((_, frame)) =
        mailbox.publish(generation, (first_pts.min(start), first_frame), pool, wake)
    {
        *held = Some((first_pts, frame));
        return Ok(());
    }
    loop {
        // A picture is published. Wait until the next one is due.
        let Some((next_pts, next_frame)) = pull(pending)? else {
            // The final delayed frame remains available at EOF. A new request
            // can reuse this decoder, including seeking backwards after EOF.
            return Ok(());
        };
        // Cap on stable clock ticks, not "one whole frame after now".
        // A seek just before a boundary must not delay the boundary picture
        // by an extra frame, particularly inside a long VFR interval.
        let Some(now) = mailbox.time(generation) else {
            *held = Some((next_pts, next_frame));
            return Ok(());
        };
        let next_tick = if request.frame_interval > 0.0 {
            ((now / request.frame_interval + 1e-9).floor() + 1.0) * request.frame_interval
        } else {
            now
        };
        let Some(target_time) = mailbox.wait_until(generation, next_pts.max(next_tick)) else {
            *held = Some((next_pts, next_frame));
            return Ok(());
        };
        // Finish each captured target before following a newer clock update.
        // Chasing state.time after every decode can outrun the worker forever,
        // discarding even the first usable picture of a short clip.
        let (mut pts, mut frame) = (next_pts, next_frame);
        loop {
            if mailbox.time(generation).is_none() {
                *held = Some((pts, frame));
                return Ok(());
            }
            let next = pull(pending)?;
            match next {
                Some(later) if later.0 <= target_time => {
                    // Coalesce intermediate pictures within this catch-up
                    // batch, not against a target that moves during decoding.
                    pool.put(std::mem::replace(&mut frame, later.1).rgba);
                    pts = later.0;
                }
                later => {
                    let at_eof = later.is_none();
                    if let Err(picture) = mailbox.publish(generation, (pts, frame), pool, wake) {
                        *held = Some(picture);
                        *pending = later;
                        return Ok(());
                    }
                    if at_eof {
                        return Ok(());
                    }
                    *pending = later;
                    break;
                }
            }
        }
    }
}

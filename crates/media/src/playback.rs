//! Realtime video playback: an in-process paced decoder stream and the
//! two-lane preview worker. Audio lives in `audio.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::{MediaResult, RawFrame, VideoDecoder};
use lane::{parked_decoders, preview_lane, PREVIEW_LANES};
use mailbox::{BufferPool, PrefetchTarget, PreviewMailbox, PreviewRequest, PREVIEW_POOL};

mod lane;
mod mailbox;

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
            // The clock starts at the first picture, so a slow first decode
            // (a hardware context, a 4K keyframe) does not rush later frames.
            let mut started: Option<Instant> = None;
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
                let started = *started.get_or_insert_with(Instant::now);
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
    /// A lane that switches files parks its decoder, position and undelivered
    /// pictures (up to `parked_decoders()`, least recently used first out), so
    /// re-entering a file continues forward instead of decoding from its
    /// keyframe again. Cancelling releases every parked decoder.
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
        preview.state.lock().unwrap().parked_cap = parked_decoders();
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
        state.keep_parked = true;
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
    /// lane, which decodes forward instead of reseeking. `active_until` is
    /// the source time the active request will have reached by then (the
    /// playing clip's out point): a target before it is a cut backward, which
    /// the active lane could only reach by seeking at the cut.
    pub fn prefetch_preview(
        &self,
        path: &str,
        source_time: f64,
        target_w: u32,
        active_until: Option<f64>,
    ) -> MediaResult<()> {
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
                active == path
                    && *width == target_w
                    && active_until.unwrap_or(*start) <= time + 1e-6
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

    /// Override how many decoders are parked (tests on many-core machines).
    pub fn set_parked_decoders(&self, cap: usize) {
        if let Some(preview) = &self.preview {
            preview.state.lock().unwrap().parked_cap = cap;
        }
    }

    /// Native decoders opened since the worker started.
    pub fn preview_opens(&self) -> u64 {
        self.preview
            .as_ref()
            .map_or(0, |preview| preview.state.lock().unwrap().opens)
    }

    /// Native decoders the preview holds open now, parked ones included.
    pub fn preview_decoders(&self) -> u64 {
        self.preview.as_ref().map_or(0, |preview| {
            let state = preview.state.lock().unwrap();
            state.opens - state.closes
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
            // Lane threads drop parked decoders; native teardown is slow.
            state.keep_parked = false;
            let parked = std::mem::take(&mut state.parked);
            state.retired.extend(parked);
            let active = state.active;
            for (index, lane) in state.lanes.iter_mut().enumerate() {
                lane.request = None;
                // The idle lane wakes to drop its decoder even when it had
                // no target: after a hand-over it still holds the last file.
                if lane.prefetch.take().is_some() || index != active {
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

//! What the preview worker shares with its caller: the newest request,
//! the prerolled cut, the finished picture, and the pooled buffers.

use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Condvar, Mutex};

use super::lane::{Lane, PREVIEW_LANES};
use crate::{MediaResult, RawFrame};

/// Recycled preview buffers; more are only allocated when this runs dry.
pub(super) const PREVIEW_POOL: usize = 8;

/// A decoded picture and its source-relative presentation time.
pub(super) type Picture = (f64, RawFrame);

pub(super) struct PreviewRequest {
    pub(super) generation: u64,
    pub(super) path: String,
    pub(super) target_w: u32,
    pub(super) frame_interval: f64,
}

#[derive(Clone, PartialEq)]
pub(super) struct PrefetchTarget {
    pub(super) path: String,
    pub(super) time: f64,
    pub(super) target_w: u32,
}

#[derive(Default)]
pub(super) struct LaneSlot {
    pub(super) request: Option<PreviewRequest>,
    pub(super) prefetch: Option<PrefetchTarget>,
    /// Bumped whenever `prefetch` changes target, cancelling stale preroll.
    pub(super) serial: u64,
    /// The lane holds the first picture of `prefetch`.
    pub(super) ready: bool,
}

#[derive(Default)]
pub(super) struct PreviewState {
    pub(super) generation: u64,
    pub(super) time: f64,
    pub(super) active: usize,
    pub(super) lanes: [LaneSlot; PREVIEW_LANES],
    /// File, width and start of the latest request.
    pub(super) active_source: Option<(String, u32, f64)>,
    pub(super) handoffs: u64,
    pub(super) opens: u64,
    pub(super) closes: u64,
    pub(super) result: Option<MediaResult<Picture>>,
    pub(super) closed: bool,
    /// Lanes left by a lane that switched files, oldest first.
    pub(super) parked: Vec<Lane>,
    /// Lanes waiting for a lane thread to drop them.
    pub(super) retired: Vec<Lane>,
    /// False after a cancel: a lane leaving a file retires its decoder.
    pub(super) keep_parked: bool,
    /// How many left lanes to keep; zero drops a decoder when its lane leaves.
    pub(super) parked_cap: usize,
}

#[derive(Default)]
pub(super) struct PreviewMailbox {
    pub(super) state: Mutex<PreviewState>,
    pub(super) changed: Condvar,
}

impl PreviewMailbox {
    /// Park `lane`, then take the parked lane for `wanted`, if any. Evicted
    /// and retired decoders are dropped here, off the UI thread.
    pub(super) fn exchange(
        &self,
        lane: Lane,
        wanted: Option<(&str, u32)>,
        pool: &BufferPool,
    ) -> Lane {
        let mut state = self.state.lock().unwrap();
        if lane.decoder.is_some() {
            if state.keep_parked {
                state.parked.push(lane);
            } else {
                state.retired.push(lane);
            }
        }
        let found = wanted.and_then(|(path, width)| {
            let index = state.parked.iter().position(|parked| {
                parked
                    .decoder
                    .as_ref()
                    .is_some_and(|((open, w), _)| open == path && *w == width)
            })?;
            Some(state.parked.remove(index))
        });
        while state.parked.len() > state.parked_cap {
            let oldest = state.parked.remove(0);
            state.retired.push(oldest);
        }
        let retired = std::mem::take(&mut state.retired);
        drop(state);
        self.close(retired, pool);
        found.unwrap_or_default()
    }

    /// Drop `lanes` and their decoders. Native teardown is slow, so this
    /// runs on lane threads only, never under the state lock.
    pub(super) fn close(&self, lanes: impl IntoIterator<Item = Lane>, pool: &BufferPool) {
        let mut closed = 0;
        for mut lane in lanes {
            closed += u64::from(lane.decoder.is_some());
            lane.reset(pool);
        }
        if closed > 0 {
            self.state.lock().unwrap().closes += closed;
        }
    }

    pub(super) fn time(&self, generation: u64) -> Option<f64> {
        let state = self.state.lock().unwrap();
        (!state.closed && state.generation == generation).then_some(state.time)
    }

    pub(super) fn wait_until(&self, generation: u64, due: f64) -> Option<f64> {
        let mut state = self.state.lock().unwrap();
        while !state.closed && state.generation == generation && state.time < due {
            state = self.changed.wait(state).unwrap();
        }
        (!state.closed && state.generation == generation).then_some(state.time)
    }

    /// Publish `picture` for `generation`, or hand it back if superseded.
    pub(super) fn publish(
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

impl PreviewMailbox {
    pub(super) fn publish_error(&self, generation: u64, error: String, wake: &dyn Fn()) -> bool {
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

/// Frame buffers shared by the UI consumer and both lanes.
pub(super) struct BufferPool {
    pub(super) put: SyncSender<Vec<u8>>,
    pub(super) get: Mutex<Receiver<Vec<u8>>>,
}

impl BufferPool {
    pub(super) fn get(&self) -> Vec<u8> {
        self.get.lock().unwrap().try_recv().unwrap_or_default()
    }

    pub(super) fn put(&self, buffer: Vec<u8>) {
        let _ = self.put.try_send(buffer);
    }
}

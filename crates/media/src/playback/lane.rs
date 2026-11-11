//! A preview lane: one decoder thread that shows a request's first
//! picture, then follows the caller's clock.

use std::thread;

use super::mailbox::{BufferPool, Picture, PrefetchTarget, PreviewMailbox, PreviewRequest};
use crate::decode::{hardware_broken, hardware_failed, FORWARD_WITHOUT_INDEX};
use crate::{MediaResult, VideoDecoder};

/// Active lane plus one prefetch lane: enough to preroll the next cut while
/// the current clip plays, without a decoder per timeline clip.
pub(super) const PREVIEW_LANES: usize = 2;

/// Decoders kept open for files the lanes left, beyond the lanes' own two.
/// Cycling through N files needs N - 2. Only slow machines park: decoders
/// use a frame thread per core, so memory grows with cores while the decode
/// time parking saves shrinks. Ten 0.3 s cuts over five 1080p-1440p files:
/// 2 cores save 1.45 s for 90 MB, 4 cores 0.7 s for 140 MB, 8 cores 0.04 s
/// for 200 MB, 16 cores nothing for 300 MB.
pub(super) fn parked_decoders() -> usize {
    let cores = thread::available_parallelism().map_or(1, |cores| cores.get());
    if cores < 8 {
        4
    } else {
        0
    }
}

/// One lane's decoder and the undelivered pictures it already decoded.
#[derive(Default)]
pub(super) struct Lane {
    pub(super) decoder: Option<((String, u32), VideoDecoder)>,
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

    fn on_hardware(&self) -> bool {
        self.decoder
            .as_ref()
            .is_some_and(|(_, decoder)| decoder.hardware)
    }

    pub(super) fn reset(&mut self, pool: &BufferPool) {
        self.release(pool);
        self.decoder = None;
    }

    /// Leave `held` as the picture covering `time` on `path`. Continues
    /// forward through already-decoded pictures when no keyframe seek would
    /// start later; otherwise seeks. Reuses this lane's or a parked decoder
    /// for the file, parking the one it leaves.
    fn prepare(
        &mut self,
        mailbox: &PreviewMailbox,
        path: &str,
        target_w: u32,
        time: f64,
        pool: &BufferPool,
    ) -> MediaResult<()> {
        let stale = |lane: &Lane| {
            lane.decoder
                .as_ref()
                .is_none_or(|((open, width), decoder)| {
                    open != path || *width != target_w || (decoder.hardware && hardware_broken())
                })
        };
        if stale(self) {
            let left = std::mem::take(self);
            *self = mailbox.exchange(left, Some((path, target_w)), pool);
            if stale(self) {
                // A hardware decoder after a hardware failure reopens in software.
                mailbox.close([std::mem::take(self)], pool);
                let decoder = VideoDecoder::open_playback(path, target_w)?;
                self.decoder = Some(((path.to_owned(), target_w), decoder));
                mailbox.state.lock().unwrap().opens += 1;
            }
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

pub(super) fn preview_lane(
    mailbox: &PreviewMailbox,
    index: usize,
    pool: &BufferPool,
    wake: &dyn Fn(),
) {
    let mut lane = Lane::default();
    let mut done_serial = 0;
    loop {
        let work = {
            let mut state = mailbox.state.lock().unwrap();
            loop {
                if state.closed {
                    return;
                }
                if !state.retired.is_empty() {
                    let retired = std::mem::take(&mut state.retired);
                    drop(state);
                    mailbox.close(retired, pool);
                    state = mailbox.state.lock().unwrap();
                    continue;
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
                    // The idle lane parks its decoder for a later cut. After a
                    // cancel it is dropped, so pausing holds no more than one.
                    if state.active != index {
                        drop(state);
                        lane = mailbox.exchange(std::mem::take(&mut lane), None, pool);
                        state = mailbox.state.lock().unwrap();
                    }
                    continue;
                }
                state = mailbox.changed.wait(state).unwrap();
            }
        };
        match work {
            LaneWork::Prefetch(target, serial) => {
                let result =
                    lane.prepare(mailbox, &target.path, target.target_w, target.time, pool);
                if let Err(error) = result {
                    if lane.on_hardware() {
                        hardware_failed(&error);
                    }
                    // The real request reopens and reports the error.
                    mailbox.close([std::mem::take(&mut lane)], pool);
                } else {
                    let mut state = mailbox.state.lock().unwrap();
                    let slot = &mut state.lanes[index];
                    slot.ready = slot.serial == serial;
                }
            }
            LaneWork::Show(request) => {
                // A hardware failure retries once in software, from the clock.
                for retry in [false, true] {
                    let result = show(&mut lane, mailbox, &request, pool, wake);
                    if let Some(held) = &lane.held {
                        lane.covers = held.0;
                    }
                    let Err(error) = result else {
                        break;
                    };
                    let hardware = lane.on_hardware();
                    mailbox.close([std::mem::take(&mut lane)], pool);
                    if hardware && !retry {
                        hardware_failed(&error);
                        continue;
                    }
                    let _ = mailbox.publish_error(request.generation, error, wake);
                    break;
                }
            }
        }
    }
}

fn show(
    lane: &mut Lane,
    mailbox: &PreviewMailbox,
    request: &PreviewRequest,
    pool: &BufferPool,
    wake: &dyn Fn(),
) -> MediaResult<()> {
    let Some(start) = mailbox.time(request.generation) else {
        return Ok(());
    };
    lane.prepare(mailbox, &request.path, request.target_w, start, pool)?;
    let first = lane.held.take().unwrap();
    let Lane {
        decoder,
        held,
        pending,
        ..
    } = lane;
    let decoder = &mut decoder.as_mut().unwrap().1;
    preview_decode(
        decoder, mailbox, request, start, first, held, pending, pool, wake,
    )
}

/// Show `first` (the picture covering `start`), then follow the caller's
/// clock. Only published pictures are scaled: pictures passed over while
/// catching up stay decoded (on the GPU with hardware decode), so they cost
/// no download or swscale. On cancellation, the next undelivered picture is
/// left in `held` (and `pending`) with the decoder positioned after it, so
/// the lane can continue forward.
#[allow(clippy::too_many_arguments)]
fn preview_decode(
    decoder: &mut VideoDecoder,
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
    // `pending`, when present, is the decoder's current picture, already
    // scaled. Otherwise the current picture was published and the next one
    // is unscaled; scale it so a later request can continue from it.
    let park = |decoder: &mut VideoDecoder,
                held: &mut Option<Picture>,
                pending: &mut Option<Picture>|
     -> MediaResult<()> {
        if let Some(picture) = pending.take() {
            *held = Some(picture);
        } else if let Some(next) = decoder.peek_next()? {
            decoder.advance_to(next)?;
            *held = Some((next, decoder.scale_current(pool.get())?));
        }
        Ok(())
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
        let next_pts = match pending.as_ref() {
            Some((pts, _)) => *pts,
            // The final delayed frame remains available at EOF. A new request
            // can reuse this decoder, including seeking backwards after EOF.
            None => match decoder.peek_next()? {
                Some(pts) => pts,
                None => return Ok(()),
            },
        };
        // Cap on stable clock ticks, not "one whole frame after now".
        // A seek just before a boundary must not delay the boundary picture
        // by an extra frame, particularly inside a long VFR interval.
        let Some(now) = mailbox.time(generation) else {
            return park(decoder, held, pending);
        };
        let next_tick = if request.frame_interval > 0.0 {
            ((now / request.frame_interval + 1e-9).floor() + 1.0) * request.frame_interval
        } else {
            now
        };
        // Finish each captured target before following a newer clock update.
        // Chasing state.time after every decode can outrun the worker forever,
        // discarding even the first usable picture of a short clip.
        let Some(target_time) = mailbox.wait_until(generation, next_pts.max(next_tick)) else {
            return park(decoder, held, pending);
        };
        if mailbox.time(generation).is_none() {
            return park(decoder, held, pending);
        }
        // Coalesce intermediate pictures within this catch-up batch, not
        // against a target that moves during decoding.
        let pts = decoder
            .advance_to(target_time)?
            .ok_or("no video frame to show")?;
        let picture = match pending.take() {
            Some(picture) if picture.0 == pts => picture,
            passed => {
                if let Some((_, frame)) = passed {
                    pool.put(frame.rgba);
                }
                (pts, decoder.scale_current(pool.get())?)
            }
        };
        if let Err(picture) = mailbox.publish(generation, picture, pool, wake) {
            *held = Some(picture);
            return Ok(());
        }
    }
}

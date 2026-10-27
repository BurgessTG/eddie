//! Realtime playback helpers: an in-process paced video decoder stream
//! and fire-and-kill audio segment players (ffplay, pending the same
//! in-process treatment for audio).

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::{MediaResult, RawFrame, VideoDecoder};

/// Streaming decoder on its own thread. Paced by presentation timestamps,
/// frame-rate capped, and buffer-pooled. Dropping stops the thread.
pub struct FrameStream {
    pub frames: Receiver<RawFrame>,
    recycle_tx: SyncSender<Vec<u8>>,
    stop_flag: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
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
            let mut first_pts: Option<f64> = None;
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
                let first = *first_pts.get_or_insert(pts);
                if pts + 1e-6 < next_due_pts {
                    // Above the requested frame-rate cap: keep the buffer.
                    spare = Some(std::mem::take(&mut frame.rgba));
                    continue;
                }
                next_due_pts = pts + frame_interval;

                // Pace to wall clock, waking periodically to notice stop.
                let due = Duration::from_secs_f64((pts - first).max(0.0));
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
                handle: Some(handle),
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

    /// Return a spent frame buffer for reuse (no-op if the pool is full).
    pub fn recycle(&self, buffer: Vec<u8>) {
        let _ = self.recycle_tx.try_send(buffer);
    }

    pub fn stop(mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for FrameStream {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }
}

/// One audio segment played through ffplay. Dropping kills playback.
pub struct AudioPlayer {
    child: Child,
}

impl AudioPlayer {
    pub fn play_segment(path: &str, start: f64, duration: f64, volume: f32) -> MediaResult<Self> {
        let child = Command::new("ffplay")
            .args([
                "-nodisp",
                "-autoexit",
                "-v",
                "error",
                "-ss",
                &format!("{start:.3}"),
                "-t",
                &format!("{duration:.3}"),
                "-af",
                &format!("volume={volume:.3}"),
                path,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to launch ffplay: {e}"))?;
        Ok(Self { child })
    }

    pub fn is_finished(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

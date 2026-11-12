//! Timeline export renders bounded video batches in parallel, each into its
//! own H.264 segment, joins them by stream copy and muxes one AAC encode.
//! Titles and the audio mix use the complete timeline clock.

use std::collections::HashMap;
use std::fs::{self};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::thread;

use openreel_engine::{Clip, ClipKind, Orientation, Timeline};

use crate::{scaled_height, MediaInfo, MediaResult};
use audio::encode_audio;
use scratch::ScratchDir;
use titles::Titles;
use video::{
    batch_command, colorspace_filter, encode_segment, plan_video, visible_frames, VideoClock,
};

mod audio;
mod scratch;
mod titles;
mod video;

pub use audio::LOUDNESS_TARGET;
pub(crate) use titles::{filter_value, font_file};
pub use titles::{title_look, TitleLook};

fn f(value: f64) -> String {
    value.to_string()
}

fn collect_stderr(stderr: Option<ChildStderr>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut err) = stderr {
            let _ = err.read_to_string(&mut text);
        }
        text
    })
}

fn export_error(stderr: &str) -> String {
    if let Some(reason) = crate::cli::cannot_draw_text(stderr) {
        return reason;
    }
    let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
    format!(
        "export failed: {}",
        tail.into_iter().rev().collect::<Vec<_>>().join(" ")
    )
}

/// Canvas for the project format: the largest source of that orientation
/// (width capped at 1920), or 1280×720 / 720×1280 without one. Sources of
/// the other orientation never set the size. The program monitor uses the
/// same canvas, so titles and pillarboxing preview as exported.
pub fn export_canvas(
    orientation: Orientation,
    sources: impl Iterator<Item = (u32, u32)>,
) -> (u32, u32) {
    sources
        .filter(|&(w, h)| Orientation::of(w, h) == orientation)
        .fold(None, |best: Option<(u32, u32)>, s| {
            let area = |(w, h): (u32, u32)| u64::from(w) * u64::from(h);
            match best {
                Some(b) if area(b) >= area(s) => Some(b),
                _ => Some(s),
            }
        })
        .map(|(width, height)| {
            let w = width.min(1920) / 2 * 2;
            (w, scaled_height(width, height, w))
        })
        .unwrap_or(match orientation {
            Orientation::Landscape => (1280, 720),
            Orientation::Portrait => (720, 1280),
        })
}

/// How export treats the audio mix.
#[derive(Clone, Debug, Default)]
pub struct ExportOptions {
    /// Measure the mix first and apply one fixed gain to
    /// [`LOUDNESS_TARGET`], with a limiter catching the peaks that gain
    /// pushes over the ceiling. Off exports the plain mix.
    pub normalize_loudness: bool,
    /// Audible source-time ranges per source path, for clips that duck
    /// under speech (`openreel_engine::Timeline::duck_ranges`).
    pub speech: HashMap<String, Vec<(f64, f64)>>,
}

/// Export `timeline` to `output` (H.264/AAC MP4). Sends 0..=1 progress on
/// `progress` as ffmpeg reports it. Blocks — call from a worker thread.
pub fn export_timeline(
    timeline: &Timeline,
    infos: &HashMap<String, MediaInfo>,
    output: &Path,
    options: &ExportOptions,
    progress: Sender<f64>,
) -> MediaResult<PathBuf> {
    let total = timeline.end();
    if total <= 0.0 {
        return Err("timeline is empty".to_owned());
    }
    let output = &*argument_path(output);
    let fps = timeline.fps;
    if !fps.is_finite() || fps <= 0.0 || !total.is_finite() {
        return Err("timeline has an invalid frame rate or duration".to_owned());
    }
    let rate = ffmpeg_next::Rational::from(fps);
    if rate.numerator() <= 0 || rate.denominator() <= 0 {
        return Err("timeline frame rate cannot be represented by FFmpeg".to_owned());
    }
    let clock = VideoClock::new(rate);
    let frame_count = clock.boundary(total);
    let visible_title = visible_frames(timeline, clock, Timeline::active_title);

    // Keep all source-backed videos here: hidden pictures still supply audio.
    let video_clips: Vec<&Clip> = timeline
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Video && c.source.is_some())
        .collect();
    let title_clips: Vec<&Clip> = timeline
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Title)
        .collect();

    let (canvas_w, canvas_h) = export_canvas(
        timeline.orientation,
        video_clips
            .iter()
            .filter_map(|c| infos.get(c.source.as_ref()?))
            .map(|i| (i.width, i.height)),
    );

    let font = font_file();
    let titles = Titles {
        font_data: font.as_deref().and_then(|path| fs::read(path).ok()),
        font,
        clock,
        clips: title_clips
            .iter()
            .filter(|clip| clip.opacity > 0.0)
            .filter_map(|&clip| Some((clip, visible_title.get(&clip.id)?.clone())))
            .collect(),
    };
    // Audio is independent of picture visibility and is encoded once, without
    // seams.
    let mix = audio::mix(timeline, infos, options)?;

    let mut conversions: HashMap<&str, Option<String>> = HashMap::new();
    for source in video_clips.iter().filter_map(|clip| clip.source.as_deref()) {
        conversions
            .entry(source)
            .or_insert_with(|| colorspace_filter(source));
    }
    let (layers, batches) = plan_video(timeline, &conversions, clock, frame_count);
    // Each batch decodes up to four inputs on slice threads and encodes on
    // its own x264, with threads from the CPU budget (cgroup quota included)
    // rather than x264's per-core default. On four CPUs one batch already
    // saturates them: two or four in flight took 54 s and 60 s instead of
    // 47 s, with 1.5 GB and 3.0 GB peak RSS instead of 0.9 GB.
    let cores = thread::available_parallelism().map_or(4, |n| n.get());
    let workers = (cores / 6).clamp(1, 3).min(batches.len());
    let encoder_threads = (cores / workers).max(1);
    let scratch = ScratchDir::beside(output)?;
    let segment = |index: usize| scratch.0.join(format!("{index:05}.mp4"));
    let audio_path = scratch.0.join("audio.m4a");
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let done = AtomicI64::new(0);
    let (audio, video) = thread::scope(|scope| {
        let audio = scope.spawn(|| encode_audio(&mix.inputs, &mix.graph, &audio_path));
        let workers: Vec<_> = (0..workers)
            .map(|_| {
                let progress = progress.clone();
                scope.spawn(|| -> MediaResult<()> {
                    let progress = progress;
                    while !failed.load(Ordering::Relaxed) {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(batch) = batches.get(index) else {
                            break;
                        };
                        let count = batch.end - batch.start;
                        let result = batch_command(
                            timeline,
                            infos,
                            (canvas_w, canvas_h),
                            clock,
                            &layers,
                            batch,
                        )
                        .and_then(|decoder| {
                            encode_segment(
                                decoder,
                                &titles.graph(batch.start, batch.end),
                                (canvas_w, canvas_h),
                                rate,
                                count,
                                encoder_threads,
                                &segment(index),
                            )
                        });
                        if let Err(error) = result {
                            failed.store(true, Ordering::Relaxed);
                            return Err(error);
                        }
                        let frames = done.fetch_add(count, Ordering::Relaxed) + count;
                        let _ = progress.send((frames as f64 / frame_count as f64).min(0.99));
                    }
                    Ok(())
                })
            })
            .collect();
        let video = workers
            .into_iter()
            .try_for_each(|worker| worker.join().expect("export worker panicked"));
        (audio.join().expect("audio export panicked"), video)
    });
    video?;
    audio?;

    join(&scratch.0, batches.len(), &audio_path, output)?;
    let _ = progress.send(1.0);
    Ok(output.to_path_buf())
}

/// Write `output` from the `segments` video files in `scratch` and the one
/// audio encode. Every segment opens with a keyframe on one shared encoder
/// setup, so they join by stream copy, and the audio is muxed once.
fn join(scratch: &Path, segments: usize, audio: &Path, output: &Path) -> MediaResult<()> {
    let list = scratch.join("segments.ffconcat");
    let mut entries = "ffconcat version 1.0\n".to_owned();
    for index in 0..segments {
        entries.push_str(&format!("file '{index:05}.mp4'\n"));
    }
    fs::write(&list, entries).map_err(|e| format!("write export segment list: {e}"))?;
    let out = crate::cli::ffmpeg()
        .args(["-y", "-v", "error", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list)
        .arg("-i")
        .arg(audio)
        .args([
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c",
            "copy",
            "-movflags",
            "+faststart",
        ])
        .arg(output)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(export_error(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

/// `path` as FFmpeg can take it as an argument. A relative path such as
/// `-cut.mp4` would be read as an option, and so would the scratch folder
/// beside `-clips/out.mp4`.
fn argument_path(path: &Path) -> std::borrow::Cow<'_, Path> {
    if path.is_relative() && path.to_string_lossy().starts_with('-') {
        Path::new(".").join(path).into()
    } else {
        path.into()
    }
}

#[cfg(test)]
mod canvas_tests {
    use super::export_canvas;
    use openreel_engine::Orientation::{Landscape, Portrait};

    const LANDSCAPE_4K: (u32, u32) = (3840, 2160);
    const PORTRAIT_4K: (u32, u32) = (2160, 3840);

    #[test]
    fn the_project_format_sets_the_canvas_not_the_clip_count() {
        // A portrait majority does not turn a landscape project portrait.
        let canvas = export_canvas(
            Landscape,
            [PORTRAIT_4K, PORTRAIT_4K, (1280, 720)].into_iter(),
        );
        assert_eq!(canvas, (1280, 720));
        // The largest source of the format sets the size, in any order.
        let canvas = export_canvas(
            Landscape,
            [(1280, 720), PORTRAIT_4K, LANDSCAPE_4K].into_iter(),
        );
        assert_eq!(canvas, (1920, 1080));
        let canvas = export_canvas(Portrait, [LANDSCAPE_4K, (1080, 1920)].into_iter());
        assert_eq!(canvas, (1080, 1920));
    }

    #[test]
    fn a_format_without_matching_sources_uses_its_default_size() {
        assert_eq!(
            export_canvas(Landscape, [PORTRAIT_4K].into_iter()),
            (1280, 720)
        );
        assert_eq!(
            export_canvas(Portrait, [LANDSCAPE_4K].into_iter()),
            (720, 1280)
        );
        assert_eq!(export_canvas(Landscape, std::iter::empty()), (1280, 720));
    }
}

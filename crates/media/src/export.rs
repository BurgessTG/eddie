//! Timeline export renders bounded video batches in parallel, each into its
//! own H.264 segment, joins them by stream copy and muxes one AAC encode.
//! Titles and the audio mix use the complete timeline clock. An audio export
//! encodes the mix alone; a GIF renders the picture the same way, without
//! sound, and quantizes it in two passes (`gif.rs`).

use std::collections::HashMap;
use std::fs::{self};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::thread;

use openreel_engine::{Clip, ClipKind, Timeline};

use crate::{MediaInfo, MediaResult};
use audio::{encode_audio, encode_audio_file};
use encoder::Encoder;
use frames::{title_frames, VideoClock};
pub(crate) use scratch::ScratchDir;
pub(crate) use source::standard_picture;
use titles::Titles;
use video::{batch_command, encode_segment, plan_video};

mod animate;
mod audio;
mod blend;
mod encoder;
mod frames;
mod gif;
mod picture;
mod scratch;
mod settings;
mod source;
mod titles;
mod transition;
mod video;

pub use audio::LOUDNESS_TARGET;
pub use settings::{
    AudioFormat, ExportKind, ExportSettings, GifSize, Quality, Resolution, FRAME_RATES, GIF_FPS,
};
pub use titles::{clip_title_look, drawn_text, title_look, TitleLook};
pub(crate) use titles::{filter_value, LINE_BOX};

fn f(value: f64) -> String {
    value.to_string()
}

pub(crate) fn collect_stderr(stderr: Option<ChildStderr>) -> thread::JoinHandle<String> {
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

/// Canvas for the project format: its shape, with a short side as long as
/// the largest video source's (at most 1080), or 720 without one. Shapes
/// and stickers do not count.
/// `Format::Original` takes the shape of the picture that starts first
/// (lane 0 first on a tie), or 16:9. `size` gives a source's displayed
/// size. Every clip fits this canvas by its transform; the program monitor
/// uses the same canvas, so pictures and titles preview as exported.
pub fn export_canvas(timeline: &Timeline, size: impl Fn(&str) -> Option<(u32, u32)>) -> (u32, u32) {
    output_canvas(timeline, size, None)
}

/// The picture an export renders: [`export_canvas`]'s shape, with
/// `short_side` as its short side (`None` for Auto's, [`ExportSettings::short_side`]).
pub fn output_canvas(
    timeline: &Timeline,
    size: impl Fn(&str) -> Option<(u32, u32)>,
    short_side: Option<u32>,
) -> (u32, u32) {
    let mut pictures: Vec<(&Clip, (u32, u32))> = timeline
        .clips
        .iter()
        // A shape or sticker sits on the canvas; it never sets its shape
        // or size.
        .filter(|clip| clip.kind == ClipKind::Video && clip.graphic.is_none())
        .filter_map(|clip| Some((clip, size(clip.source.as_deref()?)?)))
        .collect();
    pictures.sort_by(|(a, _), (b, _)| a.start.total_cmp(&b.start).then(a.track.cmp(&b.track)));
    let short = pictures
        .iter()
        .map(|(_, (w, h))| *w.min(h))
        .max()
        .map_or(720, |short| short.min(1080));
    let first = pictures.first().map_or((1280, 720), |(_, size)| *size);
    timeline.format.canvas(short_side.unwrap_or(short), first)
}

/// How export treats the audio mix and the picture's encoder.
#[derive(Clone, Debug, Default)]
pub struct ExportOptions {
    /// Measure the mix first and apply one fixed gain to
    /// [`LOUDNESS_TARGET`], with a limiter catching the peaks that gain
    /// pushes over the ceiling. Off exports the plain mix.
    pub normalize_loudness: bool,
    /// Audible source-time ranges per source path, for clips that duck
    /// under speech (`openreel_engine::Timeline::duck_ranges`).
    pub speech: HashMap<String, Vec<(f64, f64)>>,
    /// Measured noise per source path, for clips that reduce it. A source
    /// missing here is measured before the mix.
    pub noise: HashMap<String, crate::NoiseProfile>,
    pub settings: ExportSettings,
    /// Encode a video's picture with NVENC when the GPU takes it, else
    /// with x264. A GIF always uses x264.
    pub hardware_encode: bool,
}

/// A finished export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
    pub path: PathBuf,
    /// What encoded a video's picture, `x264` or `nvenc`; `None` for a
    /// sound file or a GIF.
    pub encoder: Option<&'static str>,
}

/// Export `timeline` to `output` as `options.settings` say: an H.264/AAC
/// video, the mix alone, or a GIF. Sends 0..=1 progress on `progress` as
/// ffmpeg reports it. Blocks — call from a worker thread.
pub fn export_timeline(
    timeline: &Timeline,
    infos: &HashMap<String, MediaInfo>,
    output: &Path,
    options: &ExportOptions,
    progress: Sender<f64>,
) -> MediaResult<Exported> {
    let total = timeline.end();
    if total <= 0.0 {
        return Err("timeline is empty".to_owned());
    }
    let output = &*argument_path(output);
    let settings = options.settings;
    if !total.is_finite() {
        return Err("timeline has an invalid frame rate or duration".to_owned());
    }
    if settings.kind == ExportKind::Audio {
        let mix = audio::mix(timeline, infos, options)?;
        encode_audio_file(&mix, total, settings.audio, output)?;
        let _ = progress.send(1.0);
        return Ok(Exported {
            path: output.to_path_buf(),
            encoder: None,
        });
    }
    let gif = settings.kind == ExportKind::Gif;
    let fps = settings.frame_rate(timeline.fps);
    if !fps.is_finite() || fps <= 0.0 {
        return Err("timeline has an invalid frame rate or duration".to_owned());
    }
    let rate = ffmpeg_next::Rational::from(fps);
    if rate.numerator() <= 0 || rate.denominator() <= 0 {
        return Err("timeline frame rate cannot be represented by FFmpeg".to_owned());
    }
    let clock = VideoClock::new(rate);
    let frame_count = clock.boundary(total);
    let visible_title = title_frames(timeline, clock);

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

    let size = |path: &str| infos.get(path).map(|i| (i.width, i.height));
    let (canvas_w, canvas_h) = output_canvas(timeline, size, settings.short_side());
    // Titles are sized on the canvas the monitor shows, and keep their
    // place on the frame at any other size.
    let layout = export_canvas(timeline, size);
    let title_scale = f64::from(canvas_w.min(canvas_h)) / f64::from(layout.0.min(layout.1));

    let scratch = ScratchDir::beside(output)?;
    let clips: Vec<_> = title_clips
        .iter()
        .filter(|clip| clip.opacity > 0.0)
        .filter_map(|&clip| Some((clip, visible_title.get(&clip.id)?.clone())))
        .collect();
    let faces = clips.iter().map(|(clip, _)| clip_title_look(clip).face);
    let titles = Titles {
        fonts: crate::fonts::write_faces(faces, &scratch.0)?,
        clock,
        scale: title_scale,
        clips,
    };
    // Audio is independent of picture visibility and is encoded once, without
    // seams. A GIF has none.
    let mix = (!gif)
        .then(|| audio::mix(timeline, infos, options))
        .transpose()?;

    let mut sources = HashMap::new();
    for source in video_clips.iter().filter_map(|clip| clip.source.as_deref()) {
        sources
            .entry(source)
            .or_insert_with(|| source::read(source));
    }
    let (layers, transitions, batches) = plan_video(
        timeline,
        &sources,
        infos,
        (canvas_w, canvas_h),
        clock,
        frame_count,
    );
    // Each batch decodes up to four inputs on slice threads and encodes on
    // its own x264, with threads from the CPU budget (cgroup quota included)
    // rather than x264's per-core default. On four CPUs one batch already
    // saturates them: two or four in flight took 54 s and 60 s instead of
    // 47 s, with 1.5 GB and 3.0 GB peak RSS instead of 0.9 GB.
    let cores = thread::available_parallelism().map_or(4, |n| n.get());
    let workers = |per: usize| (cores / per).clamp(1, 3).min(batches.len());
    // A GIF's picture is kept whole until it is quantized.
    let software = Encoder::X264 {
        crf: if gif { 0 } else { settings.quality.crf() },
        threads: (cores / workers(6)).max(1),
    };
    let hardware = Encoder::Nvenc {
        cq: settings.quality.cq(),
    };
    let mut encoder = if options.hardware_encode
        && !gif
        && encoder::opens(hardware, (canvas_w, canvas_h), rate)
    {
        hardware
    } else {
        software
    };
    let segment = |index: usize| scratch.0.join(format!("{index:05}.mp4"));
    let audio_path = scratch.0.join("audio.m4a");
    // Quantizing a GIF takes the last part of its progress.
    let rendered_share = if gif { 0.6 } else { 0.99 };
    let render = |encoder: Encoder| -> MediaResult<()> {
        // With the GPU encoding, decoding is the CPU's work, and more
        // batches in flight help: on 185 s of 1080p over 16 threads, 2 took
        // 53.0 s, 3 43.1 s at 1.65 GB, 4 39.7 s at 2.1 GB. Three also stays
        // inside the three NVENC sessions older drivers allow.
        let workers = if encoder.is_hardware() {
            workers(4)
        } else {
            workers(6)
        };
        let next = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        let done = AtomicI64::new(0);
        thread::scope(|scope| {
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
                                infos,
                                (canvas_w, canvas_h),
                                clock,
                                &layers,
                                &transitions,
                                batch,
                            )
                            .and_then(|decoder| {
                                encode_segment(
                                    decoder,
                                    &titles.graph(batch.start, batch.end),
                                    (canvas_w, canvas_h),
                                    rate,
                                    encoder,
                                    count,
                                    &segment(index),
                                )
                            });
                            if let Err(error) = result {
                                failed.store(true, Ordering::Relaxed);
                                return Err(error);
                            }
                            let frames = done.fetch_add(count, Ordering::Relaxed) + count;
                            let share = frames as f64 / frame_count as f64 * rendered_share;
                            let _ = progress.send(share.min(rendered_share));
                        }
                        Ok(())
                    })
                })
                .collect();
            workers
                .into_iter()
                .try_for_each(|worker| worker.join().expect("export worker panicked"))
        })
    };
    let (audio, video) = thread::scope(|scope| {
        let audio = scope.spawn(|| match &mix {
            Some(mix) => encode_audio(&mix.inputs, &mix.graph, &audio_path),
            None => Ok(()),
        });
        let mut video = render(encoder);
        // The GPU can still refuse partway, such as when another program
        // takes the last encoder session: the whole picture is made again
        // on the CPU, since segments from two encoders cannot be joined.
        if video.is_err() && encoder.is_hardware() {
            encoder = software;
            video = render(encoder);
        }
        (audio.join().expect("audio export panicked"), video)
    });
    video?;
    audio?;

    let list = segment_list(&scratch.0, batches.len())?;
    if gif {
        gif::write(
            &scratch.0,
            &list,
            batches.len(),
            (frame_count, rate),
            (&progress, rendered_share),
            output,
        )?;
    } else {
        join(&list, &audio_path, output)?;
    }
    let _ = progress.send(1.0);
    Ok(Exported {
        path: output.to_path_buf(),
        encoder: (!gif).then(|| encoder.name()),
    })
}

/// The list FFmpeg's concat demuxer reads the `segments` video files in
/// `scratch` from, in order.
fn segment_list(scratch: &Path, segments: usize) -> MediaResult<PathBuf> {
    let list = scratch.join("segments.ffconcat");
    let mut entries = "ffconcat version 1.0\n".to_owned();
    for index in 0..segments {
        entries.push_str(&format!("file '{index:05}.mp4'\n"));
    }
    fs::write(&list, entries).map_err(|e| format!("write export segment list: {e}"))?;
    Ok(list)
}

/// Write `output` from the segments in `list` and the one audio encode.
/// Every segment opens with a keyframe on one shared encoder setup, so they
/// join by stream copy, and the audio is muxed once.
fn join(list: &Path, audio: &Path, output: &Path) -> MediaResult<()> {
    let out = crate::cli::ffmpeg()
        .args(["-y", "-v", "error", "-f", "concat", "-safe", "0", "-i"])
        .arg(list)
        .arg("-i")
        .arg(audio)
        .args(["-map", "0:v", "-map", "1:a"])
        .args(["-c", "copy", "-movflags", "+faststart"])
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
    use super::{export_canvas, output_canvas, Resolution};
    use openreel_engine::{ClipKind, Format, Timeline};

    /// A timeline whose pictures start one after another in this order.
    fn canvas(format: Format, sources: &[(u32, u32)]) -> (u32, u32) {
        let mut timeline = Timeline::new(30.0).with_format(format);
        for (index, _) in sources.iter().enumerate() {
            let path = index.to_string();
            timeline.add_media_clip("v", ClipKind::Video, 0, index as f64, 1.0, 0.0, &path);
        }
        export_canvas(&timeline, |path| {
            sources.get(path.parse::<usize>().ok()?).copied()
        })
    }

    #[test]
    fn the_format_sets_the_shape_and_the_sources_its_size() {
        // The largest source's short side, whatever its shape.
        let sources = [(1080, 1920), (1280, 720)];
        assert_eq!(canvas(Format::Landscape, &sources), (1920, 1080));
        assert_eq!(canvas(Format::Portrait, &sources), (1080, 1920));
        assert_eq!(canvas(Format::Square, &sources), (1080, 1080));
        assert_eq!(canvas(Format::Vertical, &sources), (1080, 1350));
        assert_eq!(canvas(Format::Classic, &[(1280, 720)]), (960, 720));
        // 4K is held to a 1080 short side; 21:9 rounds up to an even width.
        assert_eq!(canvas(Format::Cinema, &[(3840, 2160)]), (2520, 1080));
        assert_eq!(canvas(Format::Landscape, &[]), (1280, 720));
        assert_eq!(canvas(Format::Portrait, &[]), (720, 1280));
    }

    #[test]
    fn original_takes_the_first_pictures_shape() {
        // The phone clip starts first, so the canvas is tall; the 4K clip
        // after it sets the size.
        assert_eq!(
            canvas(Format::Original, &[(1080, 1920), (3840, 2160)]),
            (1080, 1920)
        );
        assert_eq!(
            canvas(Format::Original, &[(3840, 2160), (1080, 1920)]),
            (1920, 1080)
        );
        // Odd shapes keep theirs, as export always did.
        assert_eq!(canvas(Format::Original, &[(80, 48)]), (80, 48));
        assert_eq!(canvas(Format::Original, &[(1440, 1080)]), (1440, 1080));
        assert_eq!(canvas(Format::Original, &[]), (1280, 720));
    }

    #[test]
    fn a_resolution_sets_the_short_side_and_keeps_the_shape() {
        let at = |format: Format, sources: &[(u32, u32)], resolution: Resolution| {
            let mut timeline = Timeline::new(30.0).with_format(format);
            for index in 0..sources.len() {
                let path = index.to_string();
                timeline.add_media_clip("v", ClipKind::Video, 0, index as f64, 1.0, 0.0, &path);
            }
            output_canvas(
                &timeline,
                |path| sources.get(path.parse::<usize>().ok()?).copied(),
                resolution.short_side(),
            )
        };
        // Above the sources' own size, and past the 1080 that Auto stops at.
        assert_eq!(
            at(Format::Landscape, &[(1280, 720)], Resolution::P2160),
            (3840, 2160)
        );
        assert_eq!(
            at(Format::Portrait, &[(1920, 1080)], Resolution::P1440),
            (1440, 2560)
        );
        assert_eq!(
            at(Format::Original, &[(1440, 1080)], Resolution::P480),
            (640, 480)
        );
        assert_eq!(at(Format::Cinema, &[], Resolution::P720), (1680, 720));
        assert_eq!(
            at(Format::Square, &[(3840, 2160)], Resolution::Auto),
            (1080, 1080)
        );
    }
}

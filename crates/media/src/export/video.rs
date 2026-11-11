//! The picture: which frames each clip shows, the batches they render
//! in, and the commands that decode, convert and encode one batch.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use openreel_engine::{Clip, ClipId, Timeline};

use super::scratch::FilterGraphFile;
use super::{collect_stderr, export_error};
use crate::{MediaInfo, MediaResult};

// Raw batches cannot carry source color metadata. Convert their matrix/range
// before mixing and declare the same representation at the encoder handoff.
const VIDEO_COLOR_SPACE: &str = "bt709";

const VIDEO_COLOR_RANGE: &str = "tv";

#[derive(Clone, Copy)]
pub(super) struct VideoClock {
    rate: ffmpeg_next::Rational,
    tick_rate: i64,
    frame_ticks: i64,
}

impl VideoClock {
    pub(super) fn new(rate: ffmpeg_next::Rational) -> Self {
        let tick_rate = ((i32::MAX / rate.numerator()) * rate.numerator()) as i64;
        Self {
            rate,
            tick_rate,
            frame_ticks: tick_rate / rate.numerator() as i64 * rate.denominator() as i64,
        }
    }

    /// Use the same integer clock as source offsets before rounding up to an
    /// output frame. Subtracting late source timestamps can leave more noise
    /// than a duration-relative epsilon permits, adding an unintended frame.
    /// Only sub-tick noise disappears; real off-grid boundaries still round up.
    pub(super) fn boundary(self, time: f64) -> i64 {
        let ticks = (time.max(0.0) * self.tick_rate as f64).round() as i64;
        ticks / self.frame_ticks + i64::from(ticks % self.frame_ticks != 0)
    }
}

/// Sample only where visibility can change, not once per output frame. The
/// engine owns precedence (including transparent and source-less winners).
pub(super) fn visible_frames(
    timeline: &Timeline,
    clock: VideoClock,
    active: fn(&Timeline, f64) -> Option<&Clip>,
) -> HashMap<ClipId, Vec<(i64, i64)>> {
    let mut boundaries = vec![0.0];
    for clip in &timeline.clips {
        boundaries.extend([clip.start, clip.end()]);
    }
    boundaries.sort_by(f64::total_cmp);
    boundaries.dedup();
    let mut spans: HashMap<ClipId, Vec<(i64, i64)>> = HashMap::new();
    for pair in boundaries.windows(2) {
        let start = clock.boundary(pair[0]);
        let end = clock.boundary(pair[1]);
        if start >= end {
            continue;
        }
        let time = pair[0].max(start as f64 / timeline.fps);
        if let Some(clip) = active(timeline, time) {
            let ranges = spans.entry(clip.id).or_default();
            if let Some(last) = ranges.last_mut().filter(|last| last.1 == start) {
                last.1 = end;
            } else {
                ranges.push((start, end));
            }
        }
    }
    spans
}

pub(super) fn enabled_frames(ranges: &[(i64, i64)]) -> String {
    match ranges {
        [] => "0".to_owned(),
        [(start, end)] => format!("gte(n,{start})*lt(n,{end})"),
        _ => {
            // A flat sum of hundreds of ranges exceeds AVExpr's parse depth.
            let (left, right) = ranges.split_at(ranges.len() / 2);
            format!("({}+{})", enabled_frames(left), enabled_frames(right))
        }
    }
}

pub(super) struct VideoLayer<'a> {
    source: &'a str,
    /// A cheaper exact matrix/range conversion, applied after resizing.
    convert: Option<&'a str>,
    opacity: f32,
    source_offset: i64,
    ranges: Vec<(i64, i64)>,
}

/// One bounded batch of output frames: `[start, end)` drawn from at most
/// [`MAX_INPUTS`] layers. Batches depend only on the timeline, never on the
/// worker count, so the encoded segments are the same however many run.
pub(super) struct Batch {
    pub(super) start: i64,
    pub(super) end: i64,
    inputs: Vec<usize>,
}

// Sharing a decode across different source clocks retains every frame
// between their read positions. Independent clocks need independent
// inputs, and batching bounds the number of live decoders as edits grow.
const MAX_INPUTS: usize = 4;

pub(super) fn plan_video<'a>(
    timeline: &'a Timeline,
    conversions: &'a HashMap<&str, Option<String>>,
    clock: VideoClock,
    frame_count: i64,
) -> (Vec<VideoLayer<'a>>, Vec<Batch>) {
    let VideoClock {
        tick_rate,
        frame_ticks,
        ..
    } = clock;
    let visible = visible_frames(timeline, clock, Timeline::active_video);
    let mut layers: Vec<VideoLayer<'_>> = Vec::new();
    for clip in &timeline.clips {
        let (Some(source), Some(ranges)) = (clip.source.as_deref(), visible.get(&clip.id)) else {
            continue;
        };
        if clip.opacity <= 0.0 {
            continue;
        }
        let convert = conversions
            .get(source)
            .and_then(|convert| convert.as_deref());
        let start = clock.boundary(clip.start);
        let source_in = clip.source_in + (start as f64 / timeline.fps - clip.start);
        let source_offset = (source_in * tick_rate as f64).round() as i64 - start * frame_ticks;
        if let Some(layer) = layers.iter_mut().find(|layer| {
            layer.source == source
                && layer.source_offset == source_offset
                && layer.opacity == clip.opacity
        }) {
            layer.ranges.extend_from_slice(ranges);
        } else {
            layers.push(VideoLayer {
                source,
                convert,
                opacity: clip.opacity,
                source_offset,
                ranges: ranges.clone(),
            });
        }
    }
    let mut spans: Vec<_> = layers
        .iter()
        .enumerate()
        .flat_map(|(index, layer)| layer.ranges.iter().map(move |&(a, b)| (a, b, index)))
        .collect();
    spans.sort_unstable();

    let batch_frames = clock.boundary(8.0).max(1);
    let mut batches = Vec::new();
    let mut start = 0;
    while start < frame_count {
        let mut end = (start + batch_frames).min(frame_count);
        let mut inputs = Vec::new();
        for &(a, b, layer) in &spans {
            if b <= start || a >= end || inputs.contains(&layer) {
                continue;
            }
            if inputs.len() == MAX_INPUTS {
                end = a;
                break;
            }
            inputs.push(layer);
        }
        batches.push(Batch { start, end, inputs });
        start = end;
    }
    (layers, batches)
}

/// The colorspace filter that brings a source to the export's BT.709 limited
/// range after resizing, or `None` when it already is BT.709 limited range
/// or the filter cannot read its format (swscale then converts while
/// resizing). Untagged matrices are BT.601, as the FFmpeg CLI and the
/// preview assume. Primaries and transfer stay as they are, so only the
/// matrix and range change, as before.
pub(super) fn colorspace_filter(path: &str) -> Option<String> {
    use ffmpeg_next::ffi::{AVColorRange, AVColorSpace, AVPixelFormat};
    crate::decode::ensure_init();
    let input = ffmpeg_next::format::input(path).ok()?;
    let stream = input.streams().best(ffmpeg_next::media::Type::Video)?;
    // SAFETY: the parameters belong to the open input and outlive this read.
    let (space, range, format) = unsafe {
        let parameters = stream.parameters().as_ptr();
        (
            crate::decode::raw_enum(std::ptr::addr_of!((*parameters).color_space)),
            crate::decode::raw_enum(std::ptr::addr_of!((*parameters).color_range)),
            (*parameters).format,
        )
    };
    // Compare raw values: a pixel format newer than these bindings, or a
    // matrix the file made up, is not a valid Rust enum value.
    let is = |f: AVPixelFormat| f as i32 == format;
    use AVPixelFormat::*;
    // The colorspace filter keeps a yuvj input's pixel format while tagging
    // it limited range, and later conversions read yuvj as full range.
    // Name the plain format of the same layout as its output.
    let (output, full_range_format) = [
        (AV_PIX_FMT_YUVJ420P, "yuv420p", true),
        (AV_PIX_FMT_YUVJ422P, "yuv422p", true),
        (AV_PIX_FMT_YUVJ444P, "yuv444p", true),
        (AV_PIX_FMT_YUV420P, "yuv420p", false),
        (AV_PIX_FMT_YUV422P, "yuv422p", false),
        (AV_PIX_FMT_YUV444P, "yuv444p", false),
        (AV_PIX_FMT_YUV420P10LE, "yuv420p10", false),
        (AV_PIX_FMT_YUV422P10LE, "yuv422p10", false),
        (AV_PIX_FMT_YUV444P10LE, "yuv444p10", false),
        (AV_PIX_FMT_YUV420P12LE, "yuv420p12", false),
        (AV_PIX_FMT_YUV422P12LE, "yuv422p12", false),
        (AV_PIX_FMT_YUV444P12LE, "yuv444p12", false),
    ]
    .into_iter()
    .find(|&(f, ..)| is(f))
    .map(|(_, output, full)| (output, full))?;
    let matrix = [
        (AVColorSpace::AVCOL_SPC_BT709, "bt709"),
        (AVColorSpace::AVCOL_SPC_UNSPECIFIED, "smpte170m"),
        (AVColorSpace::AVCOL_SPC_RESERVED, "smpte170m"),
        (AVColorSpace::AVCOL_SPC_SMPTE170M, "smpte170m"),
        (AVColorSpace::AVCOL_SPC_BT470BG, "bt470bg"),
        (AVColorSpace::AVCOL_SPC_SMPTE240M, "smpte240m"),
        (AVColorSpace::AVCOL_SPC_FCC, "fcc"),
        (AVColorSpace::AVCOL_SPC_BT2020_NCL, "bt2020ncl"),
    ]
    .into_iter()
    .find(|&(named, _)| named as u32 == space)
    .map(|(_, matrix)| matrix)?;
    let range = if range == AVColorRange::AVCOL_RANGE_JPEG as u32 {
        "pc"
    } else if range == AVColorRange::AVCOL_RANGE_MPEG as u32 || !full_range_format {
        "tv"
    } else {
        "pc"
    };
    ((matrix, range) != (VIDEO_COLOR_SPACE, VIDEO_COLOR_RANGE)).then(|| {
        format!(
            "colorspace=ispace={matrix}:irange={range}:itrc=bt709:iprimaries=bt709:\
             space={VIDEO_COLOR_SPACE}:range={VIDEO_COLOR_RANGE}:trc=bt709:primaries=bt709:\
             format={output}"
        )
    })
}

/// The FFmpeg command that renders one batch as raw yuv420p on `pipe:1`.
pub(super) fn batch_command(
    timeline: &Timeline,
    infos: &HashMap<String, MediaInfo>,
    canvas: (u32, u32),
    clock: VideoClock,
    layers: &[VideoLayer<'_>],
    batch: &Batch,
) -> MediaResult<(Command, FilterGraphFile)> {
    let (width, height) = canvas;
    let VideoClock {
        rate,
        tick_rate,
        frame_ticks,
    } = clock;
    let Batch { start, end, .. } = *batch;
    let count = end - start;
    let mut filters = vec![format!(
        "color=c=black:s={width}x{height}:r={rate},\
         setparams=colorspace={VIDEO_COLOR_SPACE}:range={VIDEO_COLOR_RANGE},\
         trim=end_frame={count}[base]"
    )];
    let mut current = "base".to_owned();
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-v",
        "error",
        "-filter_complex_threads",
        "4",
        "-filter_buffered_frames",
        "128",
    ]);
    for (index, &layer) in batch.inputs.iter().enumerate() {
        let layer = &layers[layer];
        let ranges: Vec<_> = layer
            .ranges
            .iter()
            .filter_map(|&(a, b)| {
                let (a, b) = (a.max(start), b.min(end));
                (a < b).then_some((a - start, b - start))
            })
            .collect();
        let first = ranges.iter().map(|range| range.0).min().unwrap();
        let source_time =
            (start + first) as f64 / timeline.fps + layer.source_offset as f64 / tick_rate as f64;
        let last_seek = infos
            .get(layer.source)
            .map(|info| (info.duration - 1.0).max(0.0))
            .unwrap_or(source_time);
        let seek = source_time.min(last_seek).max(0.0).floor() as i64;
        // Slice threads split each picture: frame threading on four 4K
        // inputs pushed export to about 2.8 GB RSS.
        cmd.args([
            "-thread_type:v",
            "slice",
            "-threads:v",
            "0",
            "-an",
            "-ss",
            &seek.to_string(),
            "-noaccurate_seek",
            "-i",
            layer.source,
        ]);
        let offset = layer.source_offset + (start + first) * frame_ticks - seek * tick_rate;
        // Stop decoding at this input's last visible frame; overlay
        // stops drawing it there anyway (enable), so the 4K decode of
        // the rest of the batch was wasted.
        let last = ranges.iter().map(|range| range.1).max().unwrap();
        let layer_frames = last - first;
        // swscale's accurate, full-chroma matrix change made a 4K BT.601
        // resize cost about 4x a BT.709 one. Resize at the source matrix,
        // then convert at canvas size with the colorspace filter: 6.8 vs
        // 14.1 CPU-s per 240 4K frames, and no further from true RGB.
        let (resize, convert) = match layer.convert {
            Some(convert) => ("bicubic+accurate_rnd".to_owned(), format!(",{convert}")),
            None => (
                format!(
                    "bicubic+accurate_rnd+full_chroma_int:\
                     out_color_matrix={VIDEO_COLOR_SPACE}:out_range={VIDEO_COLOR_RANGE}"
                ),
                String::new(),
            ),
        };
        let alpha = if layer.opacity < 0.999 {
            format!(",format=rgba,colorchannelmixer=aa={:.3}", layer.opacity)
        } else {
            String::new()
        };
        // Input seeking retains keyframe preroll with timestamps relative
        // to the requested seek. Preserve that phase for VFR frame holds.
        // Start at this input's first visible frame: padding back to zero
        // would queue all its leading frames while another input seeks.
        // Shift after fps in integer ticks; its start_time uses microseconds
        // and would change sampling phase at fractional-frame boundaries.
        // Pad before fps so EOF holds the final source frame even if it
        // falls between output samples.
        // Accurate rounding avoids darkening during matrix conversion.
        // Full chroma interpolation avoids a green shift when the matrix
        // changes (BT.601 phone footage to BT.709) on textured pictures.
        filters.push(format!(
            "[{index}:v]settb=1/{tick_rate},setpts=PTS-({offset}),\
             tpad=stop_mode=clone:stop=-1,fps={rate}:round=up:start_time=0,\
             trim=end_frame={layer_frames},setpts=PTS+{first},\
             scale={width}:{height}:force_original_aspect_ratio=decrease:flags={resize},\
             pad={width}:{height}:(ow-iw)/2:(oh-ih)/2{convert},setsar=1{alpha}[seg{index}]"
        ));
        filters.push(format!(
            "[{current}][seg{index}]overlay=0:0:eof_action=repeat:ts_sync_mode=default:\
             enable='{}'[v{index}]",
            enabled_frames(&ranges),
        ));
        current = format!("v{index}");
    }
    filters.push(format!("[{current}]format=yuv420p[vout]"));
    let graph = FilterGraphFile::create(&filters.join(";"))?;
    cmd.arg("-/filter_complex").arg(&graph.0).args([
        "-map",
        "[vout]",
        "-an",
        "-frames:v",
        &count.to_string(),
        "-threads:v",
        "1",
        "-fps_mode",
        "passthrough",
        "-f",
        "rawvideo",
        "pipe:1",
    ]);
    Ok((cmd, graph))
}

/// Pipe one batch's raw frames through its own H.264 encoder into `path`.
/// Every segment starts on a keyframe, so segments join by stream copy.
#[allow(clippy::too_many_arguments)]
pub(super) fn encode_segment(
    decoder: (Command, FilterGraphFile),
    titles: &str,
    canvas: (u32, u32),
    rate: ffmpeg_next::Rational,
    count: i64,
    threads: usize,
    path: &Path,
) -> MediaResult<()> {
    let (width, height) = canvas;
    let (mut decoder, _decoder_graph) = decoder;
    let graph = FilterGraphFile::create(titles)?;
    let mut encoder = Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "rawvideo",
            "-pixel_format",
            "yuv420p",
            "-video_size",
            &format!("{width}x{height}"),
            "-framerate",
            &rate.to_string(),
            "-colorspace",
            VIDEO_COLOR_SPACE,
            "-color_range",
            VIDEO_COLOR_RANGE,
            "-i",
            "pipe:0",
        ])
        .arg("-/filter_complex")
        .arg(&graph.0)
        .args([
            "-map",
            "[vout]",
            "-c:v",
            "libx264",
            "-threads:v",
            &threads.to_string(),
            "-preset",
            "veryfast",
            "-crf",
            "20",
            "-f",
            "mp4",
        ])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let encoder_stderr = collect_stderr(encoder.stderr.take());
    let mut stdin = encoder.stdin.take().expect("piped encoder input");
    let rendered = decoder
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch video export: {e}"))
        .and_then(|mut child| {
            let stderr = collect_stderr(child.stderr.take());
            let copied = io::copy(
                &mut child.stdout.take().expect("piped video output"),
                &mut stdin,
            );
            if copied.is_err() {
                let _ = child.kill();
            }
            let status = child.wait();
            let stderr = stderr.join().unwrap_or_default();
            let status = status.map_err(|e| format!("video export wait: {e}"))?;
            if !status.success() {
                return Err(export_error(&stderr));
            }
            let bytes = copied.map_err(|e| format!("write export video: {e}"))?;
            let expected = count as u64 * width as u64 * height as u64 * 3 / 2;
            if bytes != expected {
                return Err(format!(
                    "export video was incomplete: {bytes} of {expected} bytes"
                ));
            }
            Ok(())
        });
    drop(stdin);
    if rendered.is_err() {
        let _ = encoder.kill();
    }
    let status = encoder.wait();
    let stderr = encoder_stderr.join().unwrap_or_default();
    let status = status.map_err(|e| format!("ffmpeg wait: {e}"))?;
    if !status.success() && !stderr.is_empty() {
        return Err(export_error(&stderr));
    }
    rendered?;
    if !status.success() {
        return Err(format!("export failed: {status}"));
    }
    Ok(())
}

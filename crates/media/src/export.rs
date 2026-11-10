//! Timeline export renders bounded video batches in parallel, each into its
//! own H.264 segment, joins them by stream copy and muxes one AAC encode.
//! Titles and the audio mix use the complete timeline clock.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::thread;

use openreel_engine::{Clip, ClipId, ClipKind, Orientation, Timeline, TitleStyle};

use crate::{scaled_height, MediaInfo, MediaResult};

// Raw batches cannot carry source color metadata. Convert their matrix/range
// before mixing and declare the same representation at the encoder handoff.
const VIDEO_COLOR_SPACE: &str = "bt709";
const VIDEO_COLOR_RANGE: &str = "tv";

fn f(value: f64) -> String {
    value.to_string()
}

#[derive(Clone, Copy)]
struct VideoClock {
    rate: ffmpeg_next::Rational,
    tick_rate: i64,
    frame_ticks: i64,
}

impl VideoClock {
    fn new(rate: ffmpeg_next::Rational) -> Self {
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
    fn boundary(self, time: f64) -> i64 {
        let ticks = (time.max(0.0) * self.tick_rate as f64).round() as i64;
        ticks / self.frame_ticks + i64::from(ticks % self.frame_ticks != 0)
    }
}

/// Sample only where visibility can change, not once per output frame. The
/// engine owns precedence (including transparent and source-less winners).
fn visible_frames(
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

fn enabled_frames(ranges: &[(i64, i64)]) -> String {
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

/// FFmpeg 9's -/filter_complex reads an argument file. A 500-shot edit can
/// exceed the OS limit for a single argv string even with just one input.
/// Create exclusively, close before spawning (also on Windows), and unlink
/// on every return path, including write, spawn, and encoder errors.
struct FilterGraphFile(PathBuf);

impl FilterGraphFile {
    fn create(graph: &str) -> MediaResult<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "openreel-export-{}-{}.ffgraph",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(mut file) => {
                    let graph_file = Self(path);
                    let written = file.write_all(graph.as_bytes());
                    drop(file);
                    written.map_err(|e| format!("write export filter graph: {e}"))?;
                    return Ok(graph_file);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("create export filter graph: {error}")),
            }
        }
    }
}

impl Drop for FilterGraphFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// `value` as one filter option, quoted for both parsers it passes: the
/// option list (`:`, `'` and `\` are special) and then the filtergraph
/// (`'`). With `expansion=none` drawtext draws the text exactly as written.
pub(crate) fn filter_value(value: &str) -> String {
    let option = value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace(':', "\\:");
    format!("'{}'", option.replace('\'', "'\\''"))
}

/// The font titles are drawn with, as a path.
pub(crate) fn font_file() -> Option<String> {
    if let Ok(out) = Command::new("fc-match")
        .args(["-f", "%{file}", "sans"])
        .output()
    {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if Path::new(&path).exists() {
            return Some(path);
        }
    }
    [
        "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "C:/Windows/Fonts/arial.ttf",
    ]
    .iter()
    .find(|p| Path::new(p).exists())
    .map(|p| (*p).to_owned())
}

struct VideoLayer<'a> {
    source: &'a str,
    /// A cheaper exact matrix/range conversion, applied after resizing.
    convert: Option<&'a str>,
    opacity: f32,
    source_offset: i64,
    ranges: Vec<(i64, i64)>,
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
    let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
    format!(
        "export failed: {}",
        tail.into_iter().rev().collect::<Vec<_>>().join(" ")
    )
}

/// One bounded batch of output frames: `[start, end)` drawn from at most
/// [`MAX_INPUTS`] layers. Batches depend only on the timeline, never on the
/// worker count, so the encoded segments are the same however many run.
struct Batch {
    start: i64,
    end: i64,
    inputs: Vec<usize>,
}

// Sharing a decode across different source clocks retains every frame
// between their read positions. Independent clocks need independent
// inputs, and batching bounds the number of live decoders as edits grow.
const MAX_INPUTS: usize = 4;

fn plan_video<'a>(
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
fn colorspace_filter(path: &str) -> Option<String> {
    use ffmpeg_next::ffi::{AVColorRange, AVColorSpace, AVPixelFormat};
    crate::decode::ensure_init();
    let input = ffmpeg_next::format::input(path).ok()?;
    let stream = input.streams().best(ffmpeg_next::media::Type::Video)?;
    // SAFETY: the parameters belong to the open input and outlive this read.
    let (space, range, format) = unsafe {
        let parameters = &*stream.parameters().as_ptr();
        (
            parameters.color_space,
            parameters.color_range,
            parameters.format,
        )
    };
    // Compare raw values: a pixel format newer than these bindings is not
    // a valid Rust enum value.
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
    let matrix = match space {
        AVColorSpace::AVCOL_SPC_BT709 => "bt709",
        AVColorSpace::AVCOL_SPC_UNSPECIFIED
        | AVColorSpace::AVCOL_SPC_RESERVED
        | AVColorSpace::AVCOL_SPC_SMPTE170M => "smpte170m",
        AVColorSpace::AVCOL_SPC_BT470BG => "bt470bg",
        AVColorSpace::AVCOL_SPC_SMPTE240M => "smpte240m",
        AVColorSpace::AVCOL_SPC_FCC => "fcc",
        AVColorSpace::AVCOL_SPC_BT2020_NCL => "bt2020ncl",
        _ => return None,
    };
    let range = match range {
        AVColorRange::AVCOL_RANGE_JPEG => "pc",
        AVColorRange::AVCOL_RANGE_MPEG => "tv",
        _ if full_range_format => "pc",
        _ => "tv",
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
fn batch_command(
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

/// Titles keep the whole-timeline clock: each segment draws the visible
/// frames that fall inside it, shifted to its own frame numbers.
struct Titles<'a> {
    font: Option<String>,
    /// The font file's bytes, to measure highlight captions.
    font_data: Option<Vec<u8>>,
    clock: VideoClock,
    clips: Vec<(&'a Clip, Vec<(i64, i64)>)>,
}

/// Width of `text` in pixels at `size` (drawtext's em size), from advances
/// and kerning: centres a highlight caption, whose layers share one left edge.
fn text_width(font: &[u8], text: &str, size: f32) -> Option<f32> {
    use ab_glyph::{Font, FontRef};
    let font = FontRef::try_from_slice_and_index(font, 0).ok()?;
    let scale = size / font.units_per_em()?;
    let mut previous = None;
    let mut width = 0.0;
    for c in text.chars() {
        let glyph = font.glyph_id(c);
        if let Some(previous) = previous {
            width += font.kern_unscaled(previous, glyph);
        }
        width += font.h_advance_unscaled(glyph);
        previous = Some(glyph);
    }
    Some(width * scale)
}

impl Titles<'_> {
    fn graph(&self, start: i64, end: i64) -> String {
        let mut filters = vec!["[0:v]null[base]".to_owned()];
        let mut current = "base".to_owned();
        if let Some(font) = &self.font {
            let font = filter_value(font);
            for (i, (clip, ranges)) in self.clips.iter().enumerate() {
                let ranges: Vec<_> = ranges
                    .iter()
                    .filter_map(|&(a, b)| {
                        let (a, b) = (a.max(start), b.min(end));
                        (a < b).then_some((a - start, b - start))
                    })
                    .collect();
                if ranges.is_empty() {
                    continue;
                }
                let look = title_look(clip.title_style);
                let (r, g, b) = look.color;
                let font_size = look.font_size;
                let font_color = format!("0x{r:02X}{g:02X}{b:02X}");
                let box_options = match look.box_alpha {
                    Some(alpha) => {
                        format!("box=1:boxcolor=black@{alpha:.2}:boxborderw={}", look.border)
                    }
                    None => format!("box=0:borderw={}:bordercolor=black@0.8", look.border),
                };
                let x = look
                    .left
                    .map_or_else(|| "(w-text_w)/2".to_owned(), |left| left.to_string());
                let y = format!("h-{}", look.bottom);
                let alpha = clip.opacity.clamp(0.0, 1.0);
                let words: Vec<&str> = clip.name.split_whitespace().collect();
                let highlight = look
                    .highlight
                    .zip(clip.word_times())
                    .filter(|_| !words.is_empty());
                // Every highlight layer starts at the same left edge, so
                // drawtext lays out each prefix exactly as in the full line.
                let x = match (&highlight, look.left, &self.font_data) {
                    (Some(_), None, Some(data)) => {
                        text_width(data, &words.join(" "), font_size as f32)
                            .map_or(x, |width| format!("(w-{width:.1})/2"))
                    }
                    _ => x,
                };
                filters.push(format!(
                    "[{current}]drawtext=fontfile={font}:expansion=none:text={}:\
                     fontcolor={font_color}@{alpha:.2}:\
                     fontsize={font_size}:{box_options}:x={x}:y={y}:\
                     enable='{}'[t{i}]",
                    filter_value(&words.join(" ")),
                    enabled_frames(&ranges),
                ));
                current = format!("t{i}");
                let Some(((hr, hg, hb), times)) = highlight else {
                    continue;
                };
                // Word n lights while it is spoken: the words through it in
                // the highlight colour, then the words before it back in the
                // text colour on top. No box: the base layer drew it.
                let outline = match look.box_alpha {
                    Some(_) => "box=0".to_owned(),
                    None => format!("box=0:borderw={}:bordercolor=black@0.8", look.border),
                };
                for (n, &from) in times.iter().enumerate() {
                    let to = times.get(n + 1).copied().unwrap_or(clip.end());
                    let (a, b) = (self.clock.boundary(from), self.clock.boundary(to));
                    let active: Vec<_> = ranges
                        .iter()
                        .filter_map(|&(ra, rb)| {
                            let (ra, rb) = (ra.max(a - start), rb.min(b - start));
                            (ra < rb).then_some((ra, rb))
                        })
                        .collect();
                    if active.is_empty() {
                        continue;
                    }
                    let enable = enabled_frames(&active);
                    let mut layers =
                        vec![(words[..=n].join(" "), format!("0x{hr:02X}{hg:02X}{hb:02X}"))];
                    if n > 0 {
                        layers.push((words[..n].join(" "), font_color.clone()));
                    }
                    for (layer, (text, color)) in layers.into_iter().enumerate() {
                        let text = filter_value(&text);
                        filters.push(format!(
                            "[{current}]drawtext=fontfile={font}:expansion=none:text={text}:\
                             fontcolor={color}@{alpha:.2}:fontsize={font_size}:{outline}:\
                             x={x}:y={y}:enable='{enable}'[t{i}w{n}l{layer}]"
                        ));
                        current = format!("t{i}w{n}l{layer}");
                    }
                }
            }
        }
        filters.push(format!("[{current}]format=yuv420p[vout]"));
        filters.join(";")
    }
}

/// Pipe one batch's raw frames through its own H.264 encoder into `path`.
/// Every segment starts on a keyframe, so segments join by stream copy.
#[allow(clippy::too_many_arguments)]
fn encode_segment(
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

/// Private scratch space beside the output, so segments land on the same
/// disk as the export rather than in a RAM-backed temp directory. Removed
/// on every return path.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn beside(output: &Path) -> MediaResult<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let parent = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = output
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        loop {
            let path = parent.join(format!(
                ".{name}.export-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("export failed: create scratch folder: {error}")),
            }
        }
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The export canvas: the orientation most video clips use (a tie follows
/// the first clip; square counts as landscape), then the largest source of
/// that orientation (displayed size, first on equal area), capped at 1920
/// wide; 1280x720 without video. The program monitor uses the same canvas
/// so titles and pillarboxing preview as exported.
/// How export draws one title style, in output-canvas pixels. The program
/// monitor reads the same table and scales it to its frame, so titles
/// preview at the size and place they export.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TitleLook {
    pub font_size: u32,
    pub color: (u8, u8, u8),
    /// Black box opacity behind the text; `None` draws a black outline.
    pub box_alpha: Option<f32>,
    /// Box padding, or outline width when there is no box.
    pub border: u32,
    /// Text left edge; `None` centers the text.
    pub left: Option<u32>,
    /// Distance from the canvas bottom to the text top.
    pub bottom: u32,
    /// Colour of the word being spoken, for titles with word timing.
    pub highlight: Option<(u8, u8, u8)>,
}

pub fn title_look(style: TitleStyle) -> TitleLook {
    let white = (255, 255, 255);
    match style {
        TitleStyle::LowerThird => TitleLook {
            font_size: 44,
            color: white,
            box_alpha: Some(0.55),
            border: 14,
            left: Some(42),
            bottom: 110,
            highlight: None,
        },
        TitleStyle::Caption => TitleLook {
            font_size: 38,
            color: white,
            box_alpha: Some(0.65),
            border: 12,
            left: None,
            bottom: 90,
            highlight: None,
        },
        TitleStyle::PopCaption => TitleLook {
            font_size: 48,
            color: (255, 255, 0),
            box_alpha: Some(0.75),
            border: 14,
            left: None,
            bottom: 100,
            highlight: None,
        },
        TitleStyle::MinimalCaption => TitleLook {
            font_size: 34,
            color: white,
            box_alpha: None,
            border: 2,
            left: None,
            bottom: 70,
            highlight: None,
        },
        TitleStyle::HighlightCaption => TitleLook {
            font_size: 44,
            color: white,
            box_alpha: Some(0.65),
            border: 12,
            left: None,
            bottom: 96,
            highlight: Some((255, 214, 0)),
        },
    }
}

/// Canvas for the project format: the largest source of that orientation
/// (width capped at 1920), or 1280×720 / 720×1280 without one. Sources of
/// the other orientation never set the size.
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

/// Export `timeline` to `output` (H.264/AAC MP4). Sends 0..=1 progress on
/// `progress` as ffmpeg reports it. Blocks — call from a worker thread.
/// Integrated loudness a normalized export aims for, in LUFS.
pub const LOUDNESS_TARGET: f64 = -16.0;
/// Sample-peak ceiling after normalization (about -1.9 dBFS), leaving
/// headroom for AAC's inter-sample peaks.
const LOUDNESS_PEAK_LIMIT: f64 = 0.8;

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

/// Export the timeline as an H.264/AAC MP4.
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
    // Audio input zero is a placeholder so the source inputs keep the same
    // indices in the loudness measurement and the encode. Audio remains
    // independent of picture visibility and is encoded once, without seams.
    let mut inputs: Vec<String> = Vec::new();

    // Audio is independent of visibility. Coalesce only genuinely contiguous
    // same-source, same-gain runs (the All scenes workflow), rather than
    // creating hundreds of delayed amix branches for continuous sound.
    // Overlapping clips are never coalesced: every audible contribution still
    // reaches amix.
    let mut audio_clips: Vec<_> = timeline
        .clips
        .iter()
        .filter(|clip| {
            clip.source.is_some()
                && (clip.kind == ClipKind::Audio
                    || (clip.kind == ClipKind::Video
                        && clip.embedded_audio
                        && clip
                            .source
                            .as_ref()
                            .and_then(|src| infos.get(src))
                            .is_some_and(|info| info.has_audio)))
        })
        .collect();
    audio_clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut audio_runs: Vec<Clip> = Vec::new();
    let same_time =
        |a: f64, b: f64| (a - b).abs() <= 8.0 * f64::EPSILON * a.abs().max(b.abs()).max(1.0);
    for clip in audio_clips {
        if let Some(last) = audio_runs.last_mut().filter(|last| {
            last.source == clip.source
                && last.volume == clip.volume
                && !last.duck
                && !clip.duck
                && last.clean_voice == clip.clean_voice
                && last.smooth_fades == clip.smooth_fades
                && last.fade_lengths() == (0.0, 0.0)
                && clip.fade_lengths() == (0.0, 0.0)
                && same_time(last.end(), clip.start)
                && same_time(last.source_in + last.duration, clip.source_in)
        }) {
            last.duration = clip.end() - last.start;
        } else {
            audio_runs.push(clip.clone());
        }
    }
    let mut audio_labels: Vec<String> = Vec::new();
    let mut audio_filters: Vec<String> = Vec::new();
    let add_audio = |clip: &Clip,
                     inputs: &mut Vec<String>,
                     filters: &mut Vec<String>,
                     labels: &mut Vec<String>| {
        let Some(src) = clip.source.as_ref() else {
            return;
        };
        let idx = if let Some(i) = inputs.iter().position(|p| p == src) {
            i + 1
        } else {
            inputs.push(src.clone());
            inputs.len()
        };
        let label = format!("a{}", labels.len());
        let delay_ms = (clip.start * 1000.0).round() as i64;
        // afade's default curve is linear and its qsin is
        // `openreel_engine::fade_shape`'s quarter sine, matching the preview.
        let (fade_in, fade_out) = clip.fade_lengths();
        let curve = if clip.smooth_fades { ":curve=qsin" } else { "" };
        let mut fades = String::new();
        if fade_in > 0.0 {
            fades.push_str(&format!(",afade=t=in:st=0:d={}{curve}", f(fade_in)));
        }
        if fade_out > 0.0 {
            fades.push_str(&format!(
                ",afade=t=out:st={}:d={}{curve}",
                f(clip.duration - fade_out),
                f(fade_out)
            ));
        }
        let cleanup = if clip.clean_voice {
            crate::voice::voice_filter()
        } else {
            String::new()
        };
        // The preview mixer's `duck_gain` on clip-local time: raised-cosine
        // ramps in decibels. Merged ranges never share a ramp, so their
        // amounts add. Short frames keep each step under 6 ms so the ramps
        // do not zipper.
        let ducks = timeline.duck_ranges(clip, &options.speech);
        if !ducks.is_empty() {
            use openreel_engine::{DUCK_ATTACK, DUCK_HOLD, DUCK_LEVEL, DUCK_RELEASE};
            let amount: Vec<String> = ducks
                .iter()
                .map(|&(start, end)| {
                    let (down, hold) = (start - DUCK_ATTACK, end + DUCK_HOLD);
                    format!(
                        "if(gte(t,{})*lt(t,{}),if(lt(t,{}),(1-cos((t-{})/{}*PI))/2,\
                         if(lt(t,{}),1,(1+cos((t-{})/{}*PI))/2)),0)",
                        f(down),
                        f(hold + DUCK_RELEASE),
                        f(start),
                        f(down),
                        f(DUCK_ATTACK),
                        f(hold),
                        f(hold),
                        f(DUCK_RELEASE),
                    )
                })
                .collect();
            fades.push_str(&format!(
                ",asetnsamples=n=256:p=0,volume=volume='pow({},min(1,{}))':eval=frame",
                f(f64::from(DUCK_LEVEL)),
                amount.join("+")
            ));
        }
        filters.push(format!(
            "[{idx}:a]atrim=start={}:end={},asetpts=PTS-STARTPTS,aresample=48000{cleanup},\
             volume={:.3}{fades},adelay={delay_ms}|{delay_ms}[{label}]",
            f(clip.source_in),
            f(clip.source_in + clip.duration),
            clip.volume,
        ));
        labels.push(label);
    };
    for clip in &audio_runs {
        add_audio(clip, &mut inputs, &mut audio_filters, &mut audio_labels);
    }
    let audio_mix = match audio_labels.len() {
        0 => "anullsrc=r=48000:cl=stereo".to_owned(),
        1 => format!("[{}]anull", audio_labels[0]),
        n => format!(
            "{}amix=inputs={n}:duration=longest:normalize=0",
            audio_labels
                .iter()
                .map(|l| format!("[{l}]"))
                .collect::<String>()
        ),
    };
    // Video is already bounded by frame_count. Output -t rounds on its frame
    // clock and can drop the final active frame; limit audio independently.
    let mut audio_out = format!("{audio_mix},atrim=end={}", f(total));
    if options.normalize_loudness && !audio_labels.is_empty() {
        if let Some(loudness) = measure_loudness(&inputs, &audio_filters, &audio_out)? {
            let gain = (LOUDNESS_TARGET - loudness).clamp(-30.0, 30.0);
            audio_out.push_str(&format!(
                ",volume={gain:.2}dB,alimiter=limit={LOUDNESS_PEAK_LIMIT}:level=disabled:latency=1"
            ));
        }
    }
    let audio_graph = format!("{};{audio_out}[aout]", audio_filters.join(";"));
    let audio_graph = audio_graph.trim_start_matches(';');

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
        let audio = scope.spawn(|| encode_audio(&inputs, audio_graph, &audio_path));
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

    // Every segment opens with a keyframe on one shared encoder setup, so
    // they join by stream copy, and the single audio encode is muxed once.
    let list = scratch.0.join("segments.ffconcat");
    let mut entries = "ffconcat version 1.0\n".to_owned();
    for index in 0..batches.len() {
        entries.push_str(&format!("file '{index:05}.mp4'\n"));
    }
    fs::write(&list, entries).map_err(|e| format!("write export segment list: {e}"))?;
    let out = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list)
        .arg("-i")
        .arg(&audio_path)
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
    let _ = progress.send(1.0);
    Ok(output.to_path_buf())
}

/// Encode the timeline's audio mix once, on the whole-timeline clock.
fn encode_audio(inputs: &[String], graph: &str, path: &Path) -> MediaResult<()> {
    let graph_file = FilterGraphFile::create(graph)?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-v", "error", "-f", "lavfi", "-i", "anullsrc=d=0.01"]);
    for input in inputs {
        cmd.args(["-vn", "-i", input]);
    }
    let out = cmd
        .arg("-/filter_complex")
        .arg(&graph_file.0)
        .args(["-map", "[aout]", "-c:a", "aac", "-b:a", "160k", "-f", "mp4"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(export_error(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

/// Integrated loudness (LUFS) of the export's audio mix, decoding only the
/// audio inputs. `None` when the mix is silent.
fn measure_loudness(
    inputs: &[String],
    audio_filters: &[String],
    mix: &str,
) -> MediaResult<Option<f64>> {
    let graph = format!(
        "{};{mix},loudnorm=print_format=json[aout]",
        audio_filters.join(";")
    );
    let graph_file = FilterGraphFile::create(&graph)?;
    let mut cmd = Command::new("ffmpeg");
    // Input 0 stands in for the video pipe so audio input indices match.
    cmd.args([
        "-hide_banner",
        "-nostats",
        "-f",
        "lavfi",
        "-i",
        "anullsrc=d=0.01",
    ]);
    for input in inputs {
        cmd.args(["-vn", "-i", input]);
    }
    let out = cmd
        .arg("-/filter_complex")
        .arg(&graph_file.0)
        .args(["-map", "[aout]", "-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(export_error(&stderr));
    }
    // The JSON report is followed by FFmpeg's own summary lines.
    let report = stderr
        .rfind('{')
        .zip(stderr.rfind('}'))
        .and_then(|(start, end)| {
            serde_json::from_str::<serde_json::Value>(stderr.get(start..=end)?).ok()
        })
        .ok_or_else(|| "loudness measurement returned no report".to_owned())?;
    Ok(report["input_i"]
        .as_str()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > -70.0))
}

#[cfg(test)]
mod title_text_tests {
    use super::{filter_value, font_file};
    use std::process::Command;

    /// One 640x80 white frame with `text` options drawn on it by drawtext.
    fn drawn(font: &str, text: &str) -> Vec<u8> {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i"])
            .arg("color=c=white:s=640x80:r=1:d=1")
            .arg("-vf")
            .arg(format!(
                "drawtext=fontfile={}:expansion=none:{text}:fontsize=24:x=10:y=20",
                filter_value(font)
            ))
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .output()
            .expect("run ffmpeg");
        assert!(
            out.status.success(),
            "{text}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    #[test]
    fn titles_draw_every_character_as_typed() {
        let Some(font) = font_file() else {
            return;
        };
        // What the two parsers and drawtext's own expansion treat specially.
        let typed = r"7:05 100% it's a\b, [x]; = %{n} \'";
        let file = std::env::temp_dir().join(format!("eddie-title-{}.txt", std::process::id()));
        std::fs::write(&file, typed).unwrap();
        // drawtext reads a text file without any parsing: the oracle.
        let expected = drawn(
            &font,
            &format!("textfile={}", filter_value(&file.to_string_lossy())),
        );
        let _ = std::fs::remove_file(&file);
        let dark = expected.chunks_exact(3).filter(|rgb| rgb[0] < 128).count();
        assert!(dark > 500, "the text is drawn, dark pixels={dark}");
        assert!(
            drawn(&font, &format!("text={}", filter_value(typed))) == expected,
            "the quoted text draws the same picture as the file"
        );
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

//! Timeline export streams bounded video batches into one H.264/AAC encoder.
//! Titles and the audio mix use the complete timeline clock.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Escape user text for drawtext's `text=` value.
fn escape_drawtext(text: &str) -> String {
    text.replace('\\', "/")
        .replace('\'', "’")
        .replace(':', " -")
        .replace('%', "pct")
        .replace('\n', " ")
}

fn font_file() -> Option<String> {
    if let Ok(out) = Command::new("fc-match")
        .args(["-f", "%{file}", "sans"])
        .output()
    {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if Path::new(&path).exists() {
            return Some(path.replace(':', "\\:"));
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
    .map(|p| p.replace(':', "\\:"))
}

struct VideoLayer<'a> {
    source: &'a str,
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

fn stream_video(
    timeline: &Timeline,
    infos: &HashMap<String, MediaInfo>,
    canvas: (u32, u32),
    clock: VideoClock,
    frame_count: i64,
    output: &mut impl Write,
) -> MediaResult<()> {
    // Sharing a decode across different source clocks retains every frame
    // between their read positions. Independent clocks need independent
    // inputs, and batching bounds the number of live decoders as edits grow.
    const MAX_INPUTS: usize = 4;
    let (width, height) = canvas;
    let VideoClock {
        rate,
        tick_rate,
        frame_ticks,
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
        for (index, &layer) in inputs.iter().enumerate() {
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
            let source_time = (start + first) as f64 / timeline.fps
                + layer.source_offset as f64 / tick_rate as f64;
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
                 scale={width}:{height}:force_original_aspect_ratio=decrease:\
                 flags=bicubic+accurate_rnd+full_chroma_int:\
                 out_color_matrix={VIDEO_COLOR_SPACE}:out_range={VIDEO_COLOR_RANGE},\
                 pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1{alpha}[seg{index}]"
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
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to launch video export: {e}"))?;
        let stderr = collect_stderr(child.stderr.take());
        let copied = io::copy(
            &mut child.stdout.take().expect("piped video output"),
            output,
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
        start = end;
    }
    Ok(())
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
        },
        TitleStyle::Caption => TitleLook {
            font_size: 38,
            color: white,
            box_alpha: Some(0.65),
            border: 12,
            left: None,
            bottom: 90,
        },
        TitleStyle::PopCaption => TitleLook {
            font_size: 48,
            color: (255, 255, 0),
            box_alpha: Some(0.75),
            border: 14,
            left: None,
            bottom: 100,
        },
        TitleStyle::MinimalCaption => TitleLook {
            font_size: 34,
            color: white,
            box_alpha: None,
            border: 2,
            left: None,
            bottom: 70,
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
pub fn export_timeline(
    timeline: &Timeline,
    infos: &HashMap<String, MediaInfo>,
    output: &Path,
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

    // Input zero is the bounded video stream. Audio remains independent of
    // picture visibility and is encoded once, without seams between batches.
    let mut inputs: Vec<String> = Vec::new();
    let mut filters: Vec<String> = vec!["[0:v]null[base]".to_owned()];
    let mut current = "base".to_owned();

    if let Some(font) = font_file() {
        for (i, clip) in title_clips.iter().enumerate() {
            let Some(ranges) = visible_title.get(&clip.id) else {
                continue;
            };
            if clip.opacity <= 0.0 {
                continue;
            }
            let enable = enabled_frames(ranges);
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
            filters.push(format!(
                "[{current}]drawtext=fontfile={font}:text='{}':fontcolor={font_color}@{:.2}:\
                 fontsize={font_size}:{box_options}:x={x}:y={y}:\
                 enable='{enable}'[t{i}]",
                escape_drawtext(&clip.name),
                clip.opacity.clamp(0.0, 1.0),
            ));
            current = format!("t{i}");
        }
    }
    filters.push(format!("[{current}]format=yuv420p[vout]"));

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
        // afade's default curve is linear, matching the preview mixer.
        let (fade_in, fade_out) = clip.fade_lengths();
        let mut fades = String::new();
        if fade_in > 0.0 {
            fades.push_str(&format!(",afade=t=in:st=0:d={}", f(fade_in)));
        }
        if fade_out > 0.0 {
            fades.push_str(&format!(
                ",afade=t=out:st={}:d={}",
                f(clip.duration - fade_out),
                f(fade_out)
            ));
        }
        filters.push(format!(
            "[{idx}:a]atrim=start={}:end={},asetpts=PTS-STARTPTS,aresample=48000,\
             volume={:.3}{fades},adelay={delay_ms}|{delay_ms}[{label}]",
            f(clip.source_in),
            f(clip.source_in + clip.duration),
            clip.volume,
        ));
        labels.push(label);
    };
    for clip in &audio_runs {
        add_audio(clip, &mut inputs, &mut filters, &mut audio_labels);
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
    filters.push(format!("{audio_mix},atrim=end={}[aout]", f(total)));

    let graph_file = FilterGraphFile::create(&filters.join(";"))?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-y",
        "-v",
        "error",
        "-filter_complex_threads",
        "2",
        "-progress",
        "pipe:1",
        "-f",
        "rawvideo",
        "-pixel_format",
        "yuv420p",
        "-video_size",
        &format!("{canvas_w}x{canvas_h}"),
        "-framerate",
        &rate.to_string(),
        "-colorspace",
        VIDEO_COLOR_SPACE,
        "-color_range",
        VIDEO_COLOR_RANGE,
        "-i",
        "pipe:0",
    ]);
    for input in &inputs {
        cmd.args(["-vn", "-i", input]);
    }
    cmd.arg("-/filter_complex")
        .arg(&graph_file.0)
        .args([
            "-map",
            "[vout]",
            "-map",
            "[aout]",
            "-c:v",
            "libx264",
            "-threads:v",
            "0",
            "-preset",
            "veryfast",
            "-crf",
            "20",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
            "-movflags",
            "+faststart",
        ])
        .arg(output)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let stderr_thread = collect_stderr(child.stderr.take());
    let stdout = child.stdout.take();
    let updates = progress.clone();
    let progress_thread = thread::spawn(move || {
        if let Some(stdout) = stdout {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(value) = line.strip_prefix("out_time_ms=") {
                    if let Ok(us) = value.trim().parse::<f64>() {
                        let _ = updates.send((us / 1_000_000.0 / total).clamp(0.0, 0.999));
                    }
                }
            }
        }
    });
    let mut stdin = child.stdin.take().expect("piped encoder input");
    let rendered = stream_video(
        timeline,
        infos,
        (canvas_w, canvas_h),
        clock,
        frame_count,
        &mut stdin,
    );
    drop(stdin);
    if rendered.is_err() {
        let _ = child.kill();
    }
    let status = child.wait();
    let stderr = stderr_thread.join().unwrap_or_default();
    let _ = progress_thread.join();
    let status = status.map_err(|e| format!("ffmpeg wait: {e}"))?;
    if !status.success() && !stderr.is_empty() {
        return Err(export_error(&stderr));
    }
    rendered?;
    if !status.success() {
        return Err(format!("export failed: {status}"));
    }
    let _ = progress.send(1.0);
    Ok(output.to_path_buf())
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

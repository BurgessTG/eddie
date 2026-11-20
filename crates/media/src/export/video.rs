//! The picture: which frames each clip shows, the batches they render
//! in, and the commands that decode, convert and encode one batch.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use openreel_engine::{Background, Clip, ClipId, ColorAdjust, Timeline, Transform};

use super::picture::picture;
use super::scratch::FilterGraphFile;
use super::source::Source;
use super::transition::{self, TransitionPlan};
use super::{collect_stderr, export_error};
use crate::{MediaInfo, MediaResult};

// Raw batches cannot carry source color metadata. Convert their matrix/range
// before mixing and declare the same representation at the encoder handoff.
pub(super) const VIDEO_COLOR_SPACE: &str = "bt709";

pub(super) const VIDEO_COLOR_RANGE: &str = "tv";

#[derive(Clone, Copy)]
pub(super) struct VideoClock {
    pub(super) rate: ffmpeg_next::Rational,
    pub(super) tick_rate: i64,
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

    /// When output frame `frame` starts. The output rate need not be the
    /// timeline's.
    pub(super) fn time(self, frame: i64) -> f64 {
        frame as f64 / f64::from(self.rate)
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
        let time = pair[0].max(clock.time(start));
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
    /// How the picture is read: its stream, and a cheaper exact
    /// matrix/range conversion applied after resizing.
    read: &'a Source,
    opacity: f32,
    pub(super) transform: Transform,
    pub(super) background: Background,
    pub(super) color: ColorAdjust,
    /// Source ticks at timeline tick zero: the source clock is
    /// `speed * timeline + source_offset`.
    source_offset: i64,
    speed: f64,
    /// The file is a photo, read from the top.
    photo: bool,
    /// One picture held: a photo, or a freeze frame of the source picture
    /// at `source_offset`.
    still: bool,
    /// The frames it is the picture on.
    ranges: Vec<(i64, i64)>,
    /// Frames a transition draws it on, past its own: decoded, but drawn
    /// only through the transition.
    extents: Vec<(i64, i64)>,
}

/// One bounded batch of output frames: `[start, end)` drawn from at most
/// [`MAX_INPUTS`] layers. Batches depend only on the timeline, never on the
/// worker count, so the encoded segments are the same however many run.
pub(super) struct Batch {
    pub(super) start: i64,
    pub(super) end: i64,
    inputs: Vec<usize>,
    /// The transitions it draws, each whole.
    transitions: Vec<usize>,
}

// Sharing a decode across different source clocks retains every frame
// between their read positions. Independent clocks need independent
// inputs, and batching bounds the number of live decoders as edits grow.
const MAX_INPUTS: usize = 4;

pub(super) fn plan_video<'a>(
    timeline: &'a Timeline,
    sources: &'a HashMap<&str, Source>,
    clock: VideoClock,
    frame_count: i64,
) -> (Vec<VideoLayer<'a>>, Vec<TransitionPlan>, Vec<Batch>) {
    let VideoClock {
        tick_rate,
        frame_ticks,
        ..
    } = clock;
    let mut visible = visible_frames(timeline, clock, Timeline::active_video);
    let (mut transitions, extents) = transition::plan(timeline, clock, &mut visible);
    let mut layers: Vec<VideoLayer<'_>> = Vec::new();
    let mut layer_of: HashMap<ClipId, usize> = HashMap::new();
    let none = Vec::new();
    for clip in &timeline.clips {
        let Some(source) = clip.source.as_deref() else {
            continue;
        };
        let ranges = visible.get(&clip.id).unwrap_or(&none);
        let extent = extents.get(&clip.id).unwrap_or(&none);
        if (ranges.is_empty() && extent.is_empty()) || clip.opacity <= 0.0 {
            continue;
        }
        let Some(read) = sources.get(source) else {
            continue;
        };
        let source_offset = if clip.photo {
            0
        } else if clip.freeze {
            (clip.source_in * tick_rate as f64).round() as i64
        } else if clip.speed == 1.0 {
            // In whole ticks from the clip's first frame, so a source frame
            // on a timeline frame stays on it.
            let start = clock.boundary(clip.start);
            let source_in = clip.source_at(clock.time(start));
            (source_in * tick_rate as f64).round() as i64 - start * frame_ticks
        } else {
            ((clip.source_in - clip.start * clip.speed) * tick_rate as f64).round() as i64
        };
        let same = layers.iter().position(|layer| {
            layer.source == source
                && layer.speed == clip.speed
                && layer.source_offset == source_offset
                && layer.still == clip.still()
                && layer.opacity == clip.opacity
                && layer.transform == clip.transform
                && layer.background == clip.background
                && layer.color == clip.color
        });
        let index = match same {
            Some(index) => {
                layers[index].ranges.extend_from_slice(ranges);
                layers[index].extents.extend_from_slice(extent);
                index
            }
            None => {
                layers.push(VideoLayer {
                    source,
                    read,
                    opacity: clip.opacity,
                    transform: clip.transform,
                    background: clip.background,
                    color: clip.color,
                    source_offset,
                    speed: clip.speed,
                    photo: clip.photo,
                    still: clip.still(),
                    ranges: ranges.clone(),
                    extents: extent.clone(),
                });
                layers.len() - 1
            }
        };
        layer_of.insert(clip.id, index);
    }
    for plan in &mut transitions {
        plan.layers = plan.clips().map(|clip| layer_of.get(&clip).copied());
    }
    let mut spans: Vec<_> = layers
        .iter()
        .enumerate()
        .flat_map(|(index, layer)| {
            let frames = layer.ranges.iter().chain(&layer.extents);
            frames.map(move |&(a, b)| (a, b, index))
        })
        .collect();
    spans.sort_unstable();

    let batch_frames = clock.boundary(8.0).max(1);
    let mut batches = Vec::new();
    let mut start = 0;
    while start < frame_count {
        let mut end = (start + batch_frames).min(frame_count);
        let mut seen = Vec::new();
        for &(a, b, layer) in &spans {
            if b <= start || a >= end || seen.contains(&layer) {
                continue;
            }
            if seen.len() == MAX_INPUTS {
                end = a;
                break;
            }
            seen.push(layer);
        }
        // A transition needs both its pictures in one batch: end before it,
        // or after it when it starts the batch (a fifth input then, rarely).
        if let Some(plan) = transitions
            .iter()
            .find(|plan| plan.first < end && end < plan.end_frame)
        {
            end = if plan.first > start {
                plan.first
            } else {
                plan.end_frame
            };
        }
        let mut inputs = Vec::new();
        for &(a, b, layer) in &spans {
            if b > start && a < end && !inputs.contains(&layer) {
                inputs.push(layer);
            }
        }
        let drawn = (0..transitions.len())
            .filter(|&index| {
                let plan = &transitions[index];
                start <= plan.first && plan.end_frame <= end
            })
            .collect();
        batches.push(Batch {
            start,
            end,
            inputs,
            transitions: drawn,
        });
        start = end;
    }
    (layers, transitions, batches)
}

/// The FFmpeg command that renders one batch as raw yuv420p on `pipe:1`.
pub(super) fn batch_command(
    infos: &HashMap<String, MediaInfo>,
    canvas: (u32, u32),
    clock: VideoClock,
    layers: &[VideoLayer<'_>],
    transitions: &[TransitionPlan],
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
    let mut cmd = crate::cli::ffmpeg();
    cmd.args([
        "-v",
        "error",
        "-filter_complex_threads",
        "4",
        "-filter_buffered_frames",
        "128",
    ]);
    let in_batch = |frames: &[(i64, i64)]| -> Vec<(i64, i64)> {
        frames
            .iter()
            .filter_map(|&(a, b)| {
                let (a, b) = (a.max(start), b.min(end));
                (a < b).then_some((a - start, b - start))
            })
            .collect()
    };
    // Where each transition's two pictures sit on the canvas.
    let mut sides = vec![[None; 2]; batch.transitions.len()];
    for (index, &layer_index) in batch.inputs.iter().enumerate() {
        let layer = &layers[layer_index];
        let ranges = in_batch(&layer.ranges);
        // Decoded wherever it shows or a transition draws it.
        let needed: Vec<_> = ranges
            .iter()
            .copied()
            .chain(in_batch(&layer.extents))
            .collect();
        let first = needed.iter().map(|range| range.0).min().unwrap();
        let source_time = if layer.still {
            0.0
        } else {
            clock.time(start + first) * layer.speed
        } + layer.source_offset as f64 / tick_rate as f64;
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
            layer.read.threads(),
            "-an",
        ]);
        // A photo is read from the top: `image2` returns no picture at all
        // after a seek, even to 0.
        if !layer.photo {
            cmd.args(["-ss", &seek.to_string(), "-noaccurate_seek"]);
        }
        cmd.args(["-i", layer.source]);
        // Input timestamps (source ticks after the seek) onto output ticks
        // from this input's first frame.
        let timestamps = if layer.speed == 1.0 {
            let offset = layer.source_offset + (start + first) * frame_ticks - seek * tick_rate;
            format!("PTS-({offset})")
        } else {
            // setpts truncates, and fps puts a frame just under an output
            // frame's time on that frame: rounding the offset up keeps a
            // source frame that lands exactly on one from slipping to the
            // next.
            let offset = layer.source_offset as f64 - (seek * tick_rate) as f64
                + layer.speed * ((start + first) * frame_ticks) as f64;
            format!("(PTS-({}))/{}", offset.ceil() as i64 + 1, layer.speed)
        };
        // Stop decoding at this input's last visible frame; overlay
        // stops drawing it there anyway (enable), so the 4K decode of
        // the rest of the batch was wasted.
        let last = needed.iter().map(|range| range.1).max().unwrap();
        let layer_frames = last - first;
        // swscale's accurate, full-chroma matrix change made a 4K BT.601
        // resize cost about 4x a BT.709 one. Resize at the source matrix,
        // then convert at canvas size with the colorspace filter: 6.8 vs
        // 14.1 CPU-s per 240 4K frames, and no further from true RGB.
        let (resize, convert) = match layer.read.convert.as_deref() {
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
        let source_size = infos
            .get(layer.source)
            .map_or((width, height), |info| (info.width, info.height));
        let (picture, x, y) = picture(
            index,
            layer,
            canvas,
            source_size,
            (&resize, &convert, &alpha),
        );
        // Input seeking retains keyframe preroll with timestamps relative
        // to the requested seek. Preserve that phase for VFR frame holds.
        // Start at this input's first visible frame: padding back to zero
        // would queue all its leading frames while another input seeks.
        // Shift after fps in integer ticks; its start_time uses microseconds
        // and would change sampling phase at fractional-frame boundaries.
        // Pad before fps so EOF holds the final source frame even if it
        // falls between output samples.
        let hold = format!(
            "tpad=stop_mode=clone:stop=-1,fps={rate}:round=up:start_time=0,\
             trim=end_frame={layer_frames},setpts=PTS+{first}"
        );
        if layer.still {
            // Lay the picture out once and hold the result, rather than
            // scale the same picture again for every output frame.
            let picture = picture
                .strip_suffix(&format!("[seg{index}]"))
                .expect("a picture chain ends at its segment");
            // A freeze frame keeps the source picture showing at its time,
            // the first frame a clip starting there would show.
            let first_picture = if layer.photo {
                String::new()
            } else {
                format!(
                    "setpts=PTS-({}),fps={rate}:round=up:start_time=0,trim=end_frame=1,",
                    layer.source_offset - seek * tick_rate
                )
            };
            filters.push(format!(
                "{}settb=1/{tick_rate},{first_picture}setpts=PTS-STARTPTS{picture}[photo{index}]",
                layer.read.head(index)
            ));
            filters.push(format!("[photo{index}]{hold}[seg{index}]"));
        } else {
            filters.push(format!(
                "{}settb=1/{tick_rate},setpts={timestamps},{hold}{picture}",
                layer.read.head(index)
            ));
        }
        // The transitions in this batch that draw this picture, by side.
        let uses: Vec<(usize, usize)> = batch
            .transitions
            .iter()
            .enumerate()
            .flat_map(|(t, &plan)| {
                let layers = transitions[plan].layers;
                (0..2).filter_map(move |side| {
                    (layers[side] == Some(layer_index)).then_some((t, side))
                })
            })
            .collect();
        let shows = !ranges.is_empty();
        let own = if uses.is_empty() {
            format!("seg{index}")
        } else {
            let mut outputs: String = uses
                .iter()
                .map(|(t, side)| format!("[tw{t}_{side}]"))
                .collect();
            if shows {
                outputs.push_str(&format!("[own{index}]"));
            }
            filters.push(format!(
                "[seg{index}]split={}{outputs}",
                uses.len() + usize::from(shows)
            ));
            for &(t, side) in &uses {
                sides[t][side] = Some((x, y));
            }
            format!("own{index}")
        };
        if shows {
            filters.push(format!(
                "[{current}][{own}]overlay={x}:{y}:eof_action=repeat:ts_sync_mode=default:\
                 enable='{}'[v{index}]",
                enabled_frames(&ranges),
            ));
            current = format!("v{index}");
        }
    }
    for (index, &plan) in batch.transitions.iter().enumerate() {
        let (drawn, next) = transition::filters(
            &transitions[plan],
            index,
            start,
            canvas,
            clock,
            sides[index],
            &current,
        );
        filters.extend(drawn);
        current = next;
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
    crf: u8,
    count: i64,
    threads: usize,
    path: &Path,
) -> MediaResult<()> {
    let (width, height) = canvas;
    let (mut decoder, _decoder_graph) = decoder;
    let graph = FilterGraphFile::create(titles)?;
    let mut encoder = crate::cli::ffmpeg()
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
            &crf.to_string(),
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

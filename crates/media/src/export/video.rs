//! The picture: which frames each clip shows, the batches they render
//! in, and the commands that decode, convert and encode one batch.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use openreel_engine::{
    hides_below, Background, Clip, ClipId, ColorAdjust, Keyframes, Timeline, Transform,
};

use super::frames::{enabled_frames, visible_frames, VideoClock};
use super::picture::{picture, Motion, Spot};
use super::scratch::FilterGraphFile;
use super::source::Source;
use super::transition::{self, TransitionPlan};
use super::{collect_stderr, export_error};
use crate::{MediaInfo, MediaResult};

// Raw batches cannot carry source color metadata. Convert their matrix/range
// before mixing and declare the same representation at the encoder handoff.
pub(super) const VIDEO_COLOR_SPACE: &str = "bt709";

pub(super) const VIDEO_COLOR_RANGE: &str = "tv";

pub(super) struct VideoLayer<'a> {
    source: &'a str,
    /// The video lane its clips are on: lanes draw bottom first.
    lane: usize,
    /// Drawn over another lane's picture somewhere, so nothing but its
    /// picture (and a blurred background) may cover the canvas.
    pub(super) over: bool,
    /// How the picture is read: its stream, and a cheaper exact
    /// matrix/range conversion applied after resizing.
    read: &'a Source,
    pub(super) opacity: f32,
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
    /// Its clip's start and keys, when keys move or fade its picture. Such
    /// a layer is one clip's.
    keys: Option<(f64, &'a Keyframes)>,
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
    infos: &HashMap<String, MediaInfo>,
    canvas: (u32, u32),
    clock: VideoClock,
    frame_count: i64,
) -> (Vec<VideoLayer<'a>>, Vec<TransitionPlan>, Vec<Batch>) {
    let VideoClock {
        tick_rate,
        frame_ticks,
        ..
    } = clock;
    let canvas_f = (f64::from(canvas.0), f64::from(canvas.1));
    let hides = |clip: &Clip| {
        clip.source
            .as_ref()
            .and_then(|source| infos.get(source))
            .is_some_and(|info| {
                let size = (f64::from(info.width), f64::from(info.height));
                hides_below(clip, canvas_f, size, info.alpha)
            })
    };
    let (mut visible, over) = visible_frames(timeline, clock, &hides);
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
        let faded = match clip.keyframes.opacity.as_slice() {
            [] => clip.opacity <= 0.0,
            keys => keys.iter().all(|key| key.value <= 0.0),
        };
        if (ranges.is_empty() && extent.is_empty()) || faded {
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
        let over = over.contains(&clip.id);
        let keys = clip
            .keyframes
            .moves_picture()
            .then_some((clip.start, &clip.keyframes));
        let same = layers.iter().position(|layer| {
            keys.is_none()
                && layer.keys.is_none()
                && layer.source == source
                && layer.lane == clip.track
                && layer.over == over
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
                    lane: clip.track,
                    over,
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
                    keys,
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
            // Pictures already showing at the batch's first frame all draw
            // it, however many lanes stack there.
            if seen.len() >= MAX_INPUTS && a > start {
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
        // Read and drawn bottom lane first, each over those under it.
        inputs.sort_by_key(|&layer| std::cmp::Reverse(layers[layer].lane));
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
    let mut sides: Vec<[Option<Spot>; 2]> = vec![[None, None]; batch.transitions.len()];
    // Lanes draw bottom first (the inputs are in that order), each lane's
    // transitions over its pictures.
    let mut lanes: Vec<usize> = batch
        .inputs
        .iter()
        .map(|&layer| layers[layer].lane)
        .chain(batch.transitions.iter().map(|&plan| transitions[plan].lane))
        .collect();
    lanes.sort_unstable_by(|a, b| b.cmp(a));
    lanes.dedup();
    for lane in lanes {
        for (index, &layer_index) in batch.inputs.iter().enumerate() {
            let layer = &layers[layer_index];
            if layer.lane != lane {
                continue;
            }
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
            let source_size = infos
                .get(layer.source)
                .map_or((width, height), |info| (info.width, info.height));
            let motion = layer.keys.map(|(clip_start, keys)| Motion {
                keys,
                offset: clip_start - clock.time(start),
            });
            let picture = picture(
                index,
                layer,
                canvas,
                source_size,
                (&resize, &convert),
                motion.as_ref(),
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
                // scale the same picture again for every output frame. A
                // freeze frame keeps the source picture showing at its time,
                // the first frame a clip starting there would show.
                let first_picture = if layer.photo {
                    String::new()
                } else {
                    format!(
                        "setpts=PTS-({}),fps={rate}:round=up:start_time=0,trim=end_frame=1,",
                        layer.source_offset - seek * tick_rate
                    )
                };
                let lead = format!(
                    "{}settb=1/{tick_rate},{first_picture}setpts=PTS-STARTPTS",
                    layer.read.head(index)
                );
                filters.extend(picture.filters(index, &lead, Some(&hold)));
            } else {
                let lead = format!(
                    "{}settb=1/{tick_rate},setpts={timestamps},{hold}",
                    layer.read.head(index)
                );
                filters.extend(picture.filters(index, &lead, None));
            }
            let spot = picture.spot();
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
                    sides[t][side] = Some(spot.clone());
                }
                format!("own{index}")
            };
            if shows {
                filters.push(format!(
                    "[{current}][{own}]overlay={}:eof_action=repeat:ts_sync_mode=default:\
                 enable='{}'[v{index}]",
                    spot.overlay(0.0),
                    enabled_frames(&ranges),
                ));
                current = format!("v{index}");
            }
        }
        for (index, &plan) in batch.transitions.iter().enumerate() {
            if transitions[plan].lane != lane {
                continue;
            }
            let (drawn, next) = transition::filters(
                &transitions[plan],
                index,
                start,
                canvas,
                clock,
                &sides[index],
                &current,
            );
            filters.extend(drawn);
            current = next;
        }
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

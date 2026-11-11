//! Pictures for agents that cannot watch video (`eddie sheet`, `strip` and
//! `frames`): a contact sheet of the program, sixteen pictures through one
//! source, and timestamped frames of a source range. Each command takes
//! what it needs from the editor first and returns the decoding as a
//! [`Render`], so a window stays responsive while the pictures are made.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use openreel_engine::time_spec::TimeIndex;
use openreel_engine::{ClipId, ClipKind, Timeline};
use openreel_media::{MediaInfo, SheetCell};
use serde_json::{json, Value};

use super::inspect::{program_segments, Segment};
use super::media::{AssetState, BinAsset};
use super::Editor;

/// The slow half of a picture command. It decodes and writes files without
/// the editor, so it can run on another thread.
pub type Render = Box<dyn FnOnce() -> Result<Value, String> + Send>;

/// At most this many contact sheet cells; more shots are sampled evenly.
const SHEET_CELLS: usize = 16;
/// Pictures in a source strip.
pub const STRIP_CELLS: usize = 16;
/// Longer uncurated sources are not scanned for shots.
const STRIP_SCAN_LIMIT: f64 = 600.0;
/// `frames` without `--every` spreads this many pictures over the range.
const FRAMES_DEFAULT: usize = 16;
/// `EvidenceSampler::frames` returns at most this many pictures and decodes
/// at most this many seconds in one pass.
const FRAMES_MAX: usize = 40;
const FRAMES_PASS_SECONDS: f64 = 30.0;
const FRAMES_WIDTH: u32 = 384;

/// A contact sheet of the program: labelled cells for the visible shots in
/// `range` (a timeline range spec; the whole timeline without one), titles
/// drawn as export draws them. `shots` counts pictures, not title changes;
/// `sampled` means some shot has no cell.
pub fn sheet(app: &Editor, output: PathBuf, range: Option<&str>) -> Result<Render, String> {
    picture_output(app, &output)?;
    let span = match range {
        Some(spec) => app.timeline.time_index().range(spec)?,
        None => (0.0, app.timeline.end()),
    };
    let (shots, total) = sheet_shots(&app.timeline, span);
    if shots.is_empty() {
        return Err("the timeline is empty".to_owned());
    }
    let canvas = app.canvas();
    Ok(Box::new(move || {
        let cells: Vec<SheetCell> = shots.iter().map(SheetShot::cell).collect();
        let jpeg = openreel_media::contact_sheet(&cells, canvas)?;
        write(&output, &jpeg)?;
        Ok(json!({
            "ok": true,
            "output": output,
            "start": span.0,
            "end": span.1,
            "shots": total,
            "sampled": total > shots.len(),
            "cells": shots.iter().map(SheetShot::json).collect::<Vec<_>>(),
        }))
    }))
}

/// Sixteen pictures through one source, so an agent can find where a shot
/// is instead of guessing a source time. Pictures inside ranges the
/// timeline already shows are marked `*`.
pub fn strip(app: &Editor, asset: &str, output: PathBuf) -> Result<Render, String> {
    picture_output(app, &output)?;
    let asset = video_asset(app, asset)?;
    let source = StripSource::new(asset, &app.timeline, app.curation_dir.as_deref());
    Ok(Box::new(move || {
        let shots = source.shots();
        let times = strip_times(source.info.duration, shots.as_deref());
        let cells: Vec<SheetCell> = times
            .iter()
            .map(|&(time, _)| SheetCell {
                picture: Some((source.info.path.clone(), time)),
                opacity: 1.0,
                title: None,
                label: format!(
                    "{time:.2}s{}",
                    if source.is_placed(time) { " *" } else { "" }
                ),
            })
            .collect();
        let canvas = (source.info.width.max(2), source.info.height.max(2));
        let jpeg = openreel_media::source_strip(&cells, canvas)?;
        write(&output, &jpeg)?;
        let mut pictured: Vec<(f64, f64)> = times.iter().map(|&(_, shot)| shot).collect();
        pictured.dedup();
        Ok(json!({
            "ok": true,
            "output": output,
            "asset": source.info.name,
            "duration": source.info.duration,
            // Null when the shots could not be measured: the pictures are
            // then the middles of equal slices of the file.
            "shots": shots.as_ref().map(Vec::len),
            "shotsWithoutPicture": shots
                .as_ref()
                .map(|shots| shots.len().saturating_sub(pictured.len())),
            "placed": source.placed,
            "pictures": times
                .iter()
                .map(|&(time, shot)| json!({
                    "time": time,
                    "shot": shots.as_ref().map(|_| shot),
                    "placed": source.is_placed(time),
                }))
                .collect::<Vec<_>>(),
        }))
    }))
}

/// Timestamped pictures of a source range, one JPEG each in `directory`.
/// Times are source times in the usual grammar; `clip:7` is the source
/// range clip 7 shows. A single time gives one picture. Without `every`,
/// sixteen pictures spread over the range.
pub fn frames(
    app: &Editor,
    asset: &str,
    range: &str,
    directory: PathBuf,
    every: Option<f64>,
    width: Option<u32>,
) -> Result<Render, String> {
    let asset = video_asset(app, asset)?;
    let info = asset.info.clone();
    let index = TimeIndex {
        fps: if info.fps > 0.0 {
            info.fps
        } else {
            app.timeline.fps
        },
        end: info.duration,
        clips: app
            .timeline
            .clips
            .iter()
            .filter(|clip| clip.source.as_deref() == Some(info.path.as_str()))
            .map(|clip| (clip.id, clip.source_in, clip.source_in + clip.duration))
            .collect(),
    };
    let (start, end, times) = frame_times(&index, range, every)?;
    let width = width.unwrap_or(FRAMES_WIDTH);
    // What `EvidenceSampler` encodes; it would clamp silently.
    if !(64..=1280).contains(&width) {
        return Err("--width takes 64 to 1280 pixels".to_owned());
    }
    Ok(Box::new(move || {
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("couldn't create {}: {error}", directory.display()))?;
        let mut sampler = openreel_media::EvidenceSampler::open(&info.path, width)?;
        let one_pass = times[times.len() - 1] - times[0] <= FRAMES_PASS_SECONDS;
        let pictures = if one_pass {
            sampler.frames(&times)?
        } else {
            // Too far apart to decode through: seek to each.
            let mut pictures: Vec<openreel_media::EvidenceFrame> = Vec::new();
            for &time in &times {
                let picture = sampler.frame_at(time)?;
                if pictures.last().is_none_or(|last| last.time < picture.time) {
                    pictures.push(picture);
                }
            }
            pictures
        };
        let stem = Path::new(&info.name)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "frame".to_owned());
        let mut written = Vec::new();
        for picture in &pictures {
            let file = directory.join(format!("{stem}-{:09.3}s.jpg", picture.time));
            write(&file, &picture.jpeg)?;
            written.push(json!({ "time": picture.time, "file": file }));
        }
        Ok(json!({
            "ok": true,
            "asset": info.name,
            "start": start,
            "end": end,
            "frames": written,
        }))
    }))
}

/// The source range a spec names and the times `frames` samples in it:
/// `every` seconds apart through the half-open range, or `FRAMES_DEFAULT`
/// spread over it. A single time is its own range.
fn frame_times(
    index: &TimeIndex,
    range: &str,
    every: Option<f64>,
) -> Result<(f64, f64, Vec<f64>), String> {
    let outside = || format!("\"{range}\" is outside the source (0s..{:.3}s)", index.end);
    let is_range = range.contains("..") || (range.contains("clip:") && !range.contains('@'));
    if !is_range {
        // The end itself names the last picture.
        let time = index.time(range)?;
        if !(0.0..=index.end).contains(&time) {
            return Err(outside());
        }
        return Ok((time, time, vec![time]));
    }
    let (start, end) = index.range(range)?;
    let (start, end) = (start.max(0.0), end.min(index.end));
    if end <= start {
        return Err(outside());
    }
    let step = match every {
        Some(every) if every.is_finite() && every > 0.0 => every,
        Some(_) => return Err("--every needs a positive number of seconds".to_owned()),
        // Not closer than the source's own pictures.
        None => ((end - start) / FRAMES_DEFAULT as f64).max(1.0 / index.fps.max(1.0)),
    };
    let count = ((end - start) / step - 1e-9).ceil().max(1.0);
    if count > FRAMES_MAX as f64 {
        // `--every 1e-300` would otherwise print a 300-digit count.
        let asked = if count < 1e6 {
            format!("{count:.0}")
        } else {
            "Over a million".to_owned()
        };
        return Err(format!(
            "{asked} pictures asked for; one call gives at most {FRAMES_MAX}. Raise --every or shorten the range."
        ));
    }
    let times = (0..count as usize)
        .map(|k| start + k as f64 * step)
        .collect();
    Ok((start, end, times))
}

fn video_asset<'a>(app: &'a Editor, name: &str) -> Result<&'a BinAsset, String> {
    let asset = app.asset_named(name)?;
    if asset.state != AssetState::Ready {
        return Err(format!("{} is not imported yet", asset.info.name));
    }
    if !asset.info.has_video || asset.info.duration <= 0.0 {
        return Err(format!("{} has no picture", asset.info.name));
    }
    Ok(asset)
}

/// A sheet or strip is a JPEG and never replaces what it pictures: `eddie
/// sheet p.eddie a.mp4`, with the arguments of `strip`, once turned a.mp4
/// into a picture.
fn picture_output(app: &Editor, output: &Path) -> Result<(), String> {
    let jpeg = output
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
        });
    if !jpeg {
        return Err(format!(
            "{} must end in .jpg; the picture is a JPEG",
            output.display()
        ));
    }
    match app.export_would_replace(output) {
        Some(used) => Err(format!(
            "The picture cannot replace {used}, which this project uses."
        )),
        None => Ok(()),
    }
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes)
        .map_err(|error| format!("couldn't write {}: {error}", path.display()))
}

/// One contact sheet cell: the program at `time`.
#[derive(Clone, Debug, PartialEq)]
struct SheetShot {
    time: f64,
    /// The visible clip, its lane, source and the source time shown.
    video: Option<(ClipId, usize, Option<String>, f64, f32)>,
    /// The active title: its clip, text, style and opacity.
    title: Option<(ClipId, String, openreel_engine::TitleStyle, f32)>,
}

impl SheetShot {
    fn label(&self) -> String {
        let mut label = format!("{:.2}s", self.time);
        match &self.video {
            Some((clip, lane, ..)) => label.push_str(&format!(" clip {clip} L{lane}")),
            None => label.push_str(" no picture"),
        }
        if let Some((title, ..)) = &self.title {
            label.push_str(&format!(" title {title}"));
        }
        label
    }

    fn cell(&self) -> SheetCell {
        SheetCell {
            picture: self
                .video
                .as_ref()
                .and_then(|(_, _, source, time, _)| Some((source.clone()?, *time))),
            opacity: self.video.as_ref().map_or(1.0, |video| video.4),
            title: self
                .title
                .as_ref()
                .map(|(_, text, style, opacity)| (text.clone(), *style, *opacity)),
            label: self.label(),
        }
    }

    fn json(&self) -> Value {
        json!({
            "time": self.time,
            "clip": self.video.as_ref().map(|video| video.0),
            "lane": self.video.as_ref().map(|video| video.1),
            "sourceTime": self.video.as_ref().map(|video| video.3),
            "title": self.title.as_ref().map(|title| title.0),
        })
    }
}

/// The sheet's cells inside `span`, and how many shots (stretches of one
/// visible picture) the span holds. A title changing over a shot is not a
/// new shot, but it is something to see: while they fit, every stretch of
/// one picture under one title gets a cell at its middle. Past
/// `SHEET_CELLS` stretches, every shot keeps at least one cell and the
/// spare ones go where a cell covers the most time. Past `SHEET_CELLS`
/// shots, the span is sampled evenly.
fn sheet_shots(timeline: &Timeline, span: (f64, f64)) -> (Vec<SheetShot>, usize) {
    let segments: Vec<Segment> = program_segments(timeline)
        .into_iter()
        .filter(|segment| segment.start < span.1 && span.0 < segment.end)
        .map(|segment| Segment {
            start: segment.start.max(span.0),
            end: segment.end.min(span.1),
            ..segment
        })
        .collect();
    let mut runs: Vec<(f64, f64)> = Vec::new();
    let mut shown = None;
    for segment in &segments {
        match runs.last_mut() {
            Some(run) if shown == Some(segment.video) => run.1 = segment.end,
            _ => runs.push((segment.start, segment.end)),
        }
        shown = Some(segment.video);
    }
    let total = runs.len();
    let at = |time: f64| {
        segments
            .iter()
            .find(|segment| segment.start <= time && time < segment.end)
            .map(|segment| Segment {
                start: time,
                end: time,
                ..*segment
            })
    };
    let picked: Vec<Segment> = if segments.len() <= SHEET_CELLS {
        segments.clone()
    } else if total <= SHEET_CELLS {
        spread(&runs, SHEET_CELLS)
            .into_iter()
            .filter_map(|(time, _)| at(time))
            .collect()
    } else {
        (0..SHEET_CELLS)
            .filter_map(|index| {
                at(span.0 + (index as f64 + 0.5) * (span.1 - span.0) / SHEET_CELLS as f64)
            })
            .collect()
    };
    let shots = picked
        .iter()
        .map(|segment| {
            let time = (segment.start + segment.end) / 2.0;
            SheetShot {
                time,
                video: segment.video.and_then(|id| timeline.clip(id)).map(|clip| {
                    (
                        clip.id,
                        clip.track,
                        clip.source.clone(),
                        clip.source_in + (time - clip.start),
                        clip.opacity,
                    )
                }),
                title: segment
                    .title
                    .and_then(|id| timeline.clip(id))
                    .map(|clip| (clip.id, clip.name.clone(), clip.title_style, clip.opacity)),
            }
        })
        .collect();
    (shots, total)
}

/// One source sampled whole.
struct StripSource {
    info: MediaInfo,
    /// Source ranges the timeline's video clips show.
    placed: Vec<(f64, f64)>,
    /// The curated scene inventory; `None` until shots are detected.
    shots: Option<Vec<(f64, f64)>>,
    /// The curation store, where detected shots are saved per file.
    store: Option<PathBuf>,
}

impl StripSource {
    fn new(asset: &BinAsset, timeline: &Timeline, store: Option<&Path>) -> Self {
        let scenes = &asset.sweep.scene_clips;
        Self {
            info: asset.info.clone(),
            placed: timeline
                .clips
                .iter()
                .filter(|clip| {
                    clip.kind == ClipKind::Video
                        && clip.source.as_deref() == Some(asset.info.path.as_str())
                })
                .map(|clip| (clip.source_in, clip.source_in + clip.duration))
                .collect(),
            shots: (!scenes.is_empty()).then(|| {
                scenes
                    .iter()
                    .map(|scene| (scene.source_in, scene.source_in + scene.duration))
                    .collect()
            }),
            store: store.map(Path::to_path_buf),
        }
    }

    fn is_placed(&self, time: f64) -> bool {
        self.placed
            .iter()
            .any(|(start, end)| *start <= time && time < *end)
    }

    /// The source's shots: curated scenes, else cuts detected in its
    /// picture (saved per file; this decodes the whole file at 64 px).
    /// `None` when they cannot be measured.
    fn shots(&self) -> Option<Vec<(f64, f64)>> {
        type Cache = HashMap<(String, u64), Vec<(f64, f64)>>;
        static DETECTED: Mutex<Option<Cache>> = Mutex::new(None);
        if let Some(shots) = &self.shots {
            return Some(shots.clone());
        }
        let key = (self.info.path.clone(), self.info.duration.to_bits());
        if let Some(shots) = DETECTED.lock().unwrap().get_or_insert_default().get(&key) {
            return Some(shots.clone());
        }
        let saved = self
            .store
            .as_deref()
            .and_then(|dir| super::sweep::load_shots(dir, &self.info.path));
        if let Some(shots) = saved {
            DETECTED
                .lock()
                .unwrap()
                .get_or_insert_default()
                .insert(key, shots.clone());
            return Some(shots);
        }
        if self.info.duration > STRIP_SCAN_LIMIT {
            return None;
        }
        let motion = openreel_media::motion_samples(&self.info.path, &self.info).ok()?;
        let shots: Vec<(f64, f64)> =
            openreel_media::build_scene_segments(self.info.duration, &motion, &[], 50)
                .iter()
                .map(|segment| (segment.start, segment.end))
                .collect();
        let shots = split_at_black(&shots, &motion);
        if shots.is_empty() {
            return None;
        }
        DETECTED
            .lock()
            .unwrap()
            .get_or_insert_default()
            .insert(key, shots.clone());
        if let Some(dir) = &self.store {
            // Failure only costs another scan after a restart.
            let _ = super::sweep::save_shots(dir, &self.info.path, &shots);
        }
        Some(shots)
    }
}

/// Mean luminance (0..1) under which a strip picture counts as black.
const BLACK_LUMINANCE: f32 = 0.06;
/// Black this long inside a detected shot becomes its own shot.
const BLACK_RUN: f64 = 0.25;

/// Split detected shots where they enter or leave a black run. Scene
/// detection deliberately keeps flashes and dark frames inside a shot, so
/// a card that hard-cuts to black read as one shot and a range spanning
/// "verified" card time ended on black. Only the strip's shot list uses
/// this; curation's scene inventory is unchanged.
fn split_at_black(
    shots: &[(f64, f64)],
    motion: &[openreel_media::MotionSample],
) -> Vec<(f64, f64)> {
    // (start, end) of each black run long enough to count.
    let mut runs: Vec<(f64, f64)> = Vec::new();
    let mut start: Option<f64> = None;
    for (index, sample) in motion.iter().enumerate() {
        let black = sample.visual.luminance < BLACK_LUMINANCE;
        match (black, start) {
            (true, None) => start = Some(sample.time),
            (false, Some(from)) => {
                if sample.time - from >= BLACK_RUN {
                    runs.push((from, sample.time));
                }
                start = None;
            }
            _ => {}
        }
        if index + 1 == motion.len() {
            if let Some(from) = start {
                let end = shots.last().map_or(sample.time, |shot| shot.1);
                if end - from >= BLACK_RUN {
                    runs.push((from, end));
                }
            }
        }
    }
    let mut split = Vec::new();
    for &(shot_start, shot_end) in shots {
        let mut cuts: Vec<f64> = runs
            .iter()
            .flat_map(|&(from, to)| [from, to])
            .filter(|&edge| shot_start < edge && edge < shot_end)
            .collect();
        cuts.sort_by(f64::total_cmp);
        let mut from = shot_start;
        for cut in cuts {
            split.push((from, cut));
            from = cut;
        }
        split.push((from, shot_end));
    }
    split
}

/// At most this many shots get a picture, so the longest shots keep spare
/// pictures; a merged shot (a fade between two views) then shows both.
const STRIP_SHOTS: usize = 12;

/// Sample times through a source and the shots they fall in, in source
/// order. Up to `STRIP_SHOTS` of the longest shots get one picture each; the
/// rest go to the pictured shots with the most time per picture, spread
/// evenly inside each. Without shots, the middles of equal slices of the
/// file. Picking by length, not index, keeps a long card or take from going
/// unseen in footage with many quick cuts.
fn strip_times(duration: f64, shots: Option<&[(f64, f64)]>) -> Vec<(f64, (f64, f64))> {
    let whole = [(0.0, duration)];
    let shots = match shots {
        Some(shots) if !shots.is_empty() => shots,
        _ => &whole[..],
    };
    let mut order: Vec<usize> = (0..shots.len()).collect();
    order.sort_by(|a, b| {
        let length = |i: usize| shots[i].1 - shots[i].0;
        length(*b).total_cmp(&length(*a))
    });
    order.truncate(STRIP_SHOTS);
    order.sort_unstable();
    let picked: Vec<(f64, f64)> = order.into_iter().map(|i| shots[i]).collect();
    spread(&picked, STRIP_CELLS)
}

/// `cells` sample times over `ranges` (at most `cells` of them), each with
/// its range, in order: one per range, the spare ones to whichever range
/// has the most time per sample, spread evenly inside each.
fn spread(ranges: &[(f64, f64)], cells: usize) -> Vec<(f64, (f64, f64))> {
    let mut counts = vec![1_usize; ranges.len()];
    for _ in ranges.len()..cells {
        let longest = (0..ranges.len())
            .max_by(|a, b| {
                let per = |i: usize| (ranges[i].1 - ranges[i].0) / counts[i] as f64;
                per(*a).total_cmp(&per(*b))
            })
            .unwrap();
        counts[longest] += 1;
    }
    ranges
        .iter()
        .zip(counts)
        .flat_map(|(&(start, end), count)| {
            (0..count).map(move |k| {
                (
                    start + (k as f64 + 0.5) * (end - start) / count as f64,
                    (start, end),
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;

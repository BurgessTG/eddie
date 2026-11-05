//! Timeline checks the chat agent reads (`inspect()`) and receives after
//! each edit, with a contact sheet of the program, so it can see what its
//! edit produced. Measured facts about the timeline, not taste.

use std::collections::HashMap;
use std::sync::Mutex;

use openreel_engine::{ClipId, ClipKind, Orientation, Timeline};
use openreel_media::SheetCell;
use serde::Serialize;

use super::media::BinAsset;

/// At most this many contact sheet cells; more shots are sampled evenly.
const SHEET_CELLS: usize = 16;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct Finding {
    pub(super) kind: &'static str,
    /// "error" breaks the program; "warning" is probably unintended.
    pub(super) severity: &'static str,
    pub(super) message: String,
    pub(super) clips: Vec<ClipId>,
    pub(super) start: f64,
    pub(super) end: f64,
}

/// A stretch of program time with one visible picture and one title.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Segment {
    pub(super) start: f64,
    pub(super) end: f64,
    pub(super) video: Option<ClipId>,
    pub(super) title: Option<ClipId>,
}

/// The program as preview and export show it, from 0 to the timeline end,
/// split wherever the visible picture or title changes.
pub(super) fn program_segments(timeline: &Timeline) -> Vec<Segment> {
    let mut edges: Vec<f64> = vec![0.0, timeline.end()];
    for clip in &timeline.clips {
        if matches!(clip.kind, ClipKind::Video | ClipKind::Title) {
            edges.extend([clip.start, clip.end()]);
        }
    }
    edges.retain(|edge| (0.0..=timeline.end()).contains(edge));
    edges.sort_by(f64::total_cmp);
    edges.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let mut segments: Vec<Segment> = Vec::new();
    for pair in edges.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let middle = (start + end) / 2.0;
        let video = timeline.active_video(middle).map(|clip| clip.id);
        let title = timeline.active_title(middle).map(|clip| clip.id);
        match segments.last_mut() {
            Some(last) if last.video == video && last.title == title => last.end = end,
            _ => segments.push(Segment {
                start,
                end,
                video,
                title,
            }),
        }
    }
    segments
}

pub(super) fn inspect(timeline: &Timeline, assets: &[BinAsset]) -> Vec<Finding> {
    let frame = 1.0 / timeline.fps.max(1.0);
    let asset = |source: &str| assets.iter().find(|asset| asset.info.path == source);
    let mut findings = Vec::new();
    let mut finding = |kind, severity, message: String, clips: Vec<ClipId>, start, end| {
        findings.push(Finding {
            kind,
            severity,
            message,
            clips,
            start,
            end,
        });
    };

    for clip in &timeline.clips {
        let Some(source) = clip.source.as_deref() else {
            continue;
        };
        let (id, start, end) = (clip.id, clip.start, clip.end());
        let Some(asset) = asset(source) else {
            finding(
                "missing-media",
                "error",
                format!("Clip {id} uses media that is not in the bin."),
                vec![id],
                start,
                end,
            );
            continue;
        };
        let info = &asset.info;
        if clip.kind == ClipKind::Video
            && info.has_video
            && Orientation::of(info.width, info.height) != timeline.orientation
        {
            finding(
                "orientation",
                "error",
                format!(
                    "Clip {id} is {} video in a {} project.",
                    Orientation::of(info.width, info.height).as_str(),
                    timeline.orientation.as_str()
                ),
                vec![id],
                start,
                end,
            );
        }
        if info.duration > 0.0
            && (clip.source_in < -1e-6 || clip.source_in + clip.duration > info.duration + frame)
        {
            finding(
                "source-range",
                "error",
                format!(
                    "Clip {id} plays source {:.2}-{:.2}s; the file is {:.2}s long.",
                    clip.source_in,
                    clip.source_in + clip.duration,
                    info.duration
                ),
                vec![id],
                start,
                end,
            );
        }
        if clip.fade_in + clip.fade_out > clip.duration + 1e-6 {
            finding(
                "fade-too-long",
                "warning",
                format!(
                    "Clip {id}'s fades ({:.2}s + {:.2}s) are longer than the clip ({:.2}s) and get scaled down.",
                    clip.fade_in, clip.fade_out, clip.duration
                ),
                vec![id],
                start,
                end,
            );
        }
        if clip.kind == ClipKind::Video {
            let text = asset
                .sweep
                .candidates
                .iter()
                .chain(&asset.sweep.scene_clips)
                .filter(|range| range.tags.iter().any(|tag| tag == "on-screen-text"))
                .map(|range| {
                    let from = range.source_in.max(clip.source_in);
                    let to = range.end().min(clip.source_in + clip.duration);
                    (from, to)
                })
                .filter(|(from, to)| to - from > frame / 2.0)
                .fold(None, |all: Option<(f64, f64)>, (from, to)| {
                    Some(all.map_or((from, to), |(a, b)| (a.min(from), b.max(to))))
                });
            if let Some((from, to)) = text {
                finding(
                    "on-screen-text",
                    "warning",
                    format!(
                        "Clip {id} shows burned-in text from an earlier edit (source {from:.2}-{to:.2}s)."
                    ),
                    vec![id],
                    start + (from - clip.source_in),
                    start + (to - clip.source_in),
                );
            }
        }
    }

    let segments = program_segments(timeline);
    let last_picture = segments
        .iter()
        .filter(|segment| segment.video.is_some())
        .map(|segment| segment.end)
        .fold(0.0, f64::max);
    // Runs of one visible picture regardless of titles changing over it.
    let mut runs: Vec<(f64, f64, Option<ClipId>)> = Vec::new();
    for segment in &segments {
        match runs.last_mut() {
            Some(run) if run.2 == segment.video => run.1 = segment.end,
            _ => runs.push((segment.start, segment.end, segment.video)),
        }
    }
    for (start, end, video) in &runs {
        let (start, end) = (*start, *end);
        match video {
            None if end <= last_picture + 1e-9 && end - start >= frame / 2.0 => {
                let titled = segments.iter().all(|segment| {
                    segment.end <= start || segment.start >= end || segment.title.is_some()
                });
                if !titled {
                    finding(
                        "picture-gap",
                        "warning",
                        format!("No picture from {start:.2}s to {end:.2}s; it shows black."),
                        Vec::new(),
                        start,
                        end,
                    );
                }
            }
            Some(id) if end - start < 2.0 * frame - 1e-6 && end < timeline.end() - 1e-9 => {
                finding(
                    "flash-frame",
                    "warning",
                    format!(
                        "Clip {id} is visible for only {:.0} frame(s) at {start:.2}s.",
                        ((end - start) / frame).round().max(1.0)
                    ),
                    vec![*id],
                    start,
                    end,
                );
            }
            _ => {}
        }
    }
    // A cut inside one shot that skips or repeats a frame or two pops; it
    // is usually left from a split, trim or swap (fframes' continuity scan
    // finds these in pixels; the timeline already knows them).
    for pair in runs.windows(2) {
        let ((_, cut, Some(left)), (start, end, Some(right))) = (pair[0], pair[1]) else {
            continue;
        };
        let (Some(a), Some(b)) = (timeline.clip(left), timeline.clip(right)) else {
            continue;
        };
        if a.source.is_none() || a.source != b.source {
            continue;
        }
        let source_frame = a
            .source
            .as_deref()
            .and_then(asset)
            .map_or(frame, |asset| 1.0 / asset.info.fps.max(1.0));
        let jump = (b.source_in + (cut - b.start)) - (a.source_in + (cut - a.start));
        let frames = (jump / source_frame).round();
        if frames != 0.0 && frames.abs() <= 3.0 {
            let (verb, count) = if frames > 0.0 {
                ("skips", frames)
            } else {
                ("repeats", -frames)
            };
            finding(
                "near-match-cut",
                "warning",
                format!(
                    "The cut from clip {left} to clip {right} at {cut:.2}s {verb} {count:.0} \
                     source frame(s) of the same shot, so it pops."
                ),
                vec![left, right],
                start,
                end.min(start + frame),
            );
        }
    }
    for clip in &timeline.clips {
        if clip.kind == ClipKind::Video && !runs.iter().any(|run| run.2 == Some(clip.id)) {
            finding(
                "hidden-clip",
                "warning",
                format!(
                    "Clip {} on lane {} is always covered by a lower lane and never shows.",
                    clip.id, clip.track
                ),
                vec![clip.id],
                clip.start,
                clip.end(),
            );
        }
    }
    findings
}

/// Pictures sampled per source in a check's source strip.
pub(super) const STRIP_CELLS: usize = 16;
/// At most this many sources get a strip, in program order.
const STRIP_SOURCES: usize = 3;
/// Longer uncurated sources are not scanned for shots at check time.
const STRIP_SCAN_LIMIT: f64 = 600.0;

/// Sources sampled whole, so a check can see where a described shot
/// actually is instead of guessing a source time.
pub(super) struct SourceStrip {
    pub(super) sources: Vec<StripSource>,
    /// One line per source: its tag (`S1`), path, length and what the edit
    /// placed from it.
    pub(super) legend: Vec<String>,
}

pub(super) struct StripSource {
    tag: String,
    info: openreel_media::MediaInfo,
    /// Source ranges the edit shows.
    placed: Vec<(f64, f64)>,
    /// The curated scene inventory; `None` until shots are detected.
    shots: Option<Vec<(f64, f64)>>,
}

impl StripSource {
    /// The source's shots: curated scenes, else cuts detected in its
    /// picture (cached per file; this decodes the whole file at 64 px, so
    /// call it off the UI thread). `None` when they cannot be measured.
    pub(super) fn shots(&self) -> Option<Vec<(f64, f64)>> {
        type Cache = HashMap<(String, u64), Vec<(f64, f64)>>;
        static DETECTED: Mutex<Option<Cache>> = Mutex::new(None);
        if let Some(shots) = &self.shots {
            return Some(shots.clone());
        }
        let key = (self.info.path.clone(), self.info.duration.to_bits());
        if let Some(shots) = DETECTED.lock().unwrap().get_or_insert_default().get(&key) {
            return Some(shots.clone());
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
        Some(shots)
    }

    /// `STRIP_CELLS` pictures labelled `S1 12.30s`, with `*` on those inside
    /// a placed range, and the line listing the shots they fall in.
    pub(super) fn cells(&self, shots: Option<&[(f64, f64)]>) -> (Vec<SheetCell>, String) {
        let times = strip_times(self.info.duration, shots);
        let cells = times
            .iter()
            .map(|&(time, _)| {
                let inside = self
                    .placed
                    .iter()
                    .any(|(start, end)| *start <= time && time < *end);
                SheetCell {
                    picture: Some((self.info.path.clone(), time)),
                    opacity: 1.0,
                    title: None,
                    label: format!("{} {time:.2}s{}", self.tag, if inside { " *" } else { "" }),
                }
            })
            .collect();
        // Only pictured shots are listed: a long list of unseen cuts made
        // the model cross-check every boundary at great reasoning cost.
        let line = match shots {
            Some(shots) => {
                let mut pictured: Vec<(f64, f64)> = times.iter().map(|&(_, shot)| shot).collect();
                pictured.dedup();
                let mut line = format!(
                    "{} shots with pictures: {}",
                    self.tag,
                    pictured
                        .iter()
                        .map(|(start, end)| format!("{start:.2}-{end:.2}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let unseen = shots.len().saturating_sub(pictured.len());
                if unseen > 0 {
                    line.push_str(&format!(
                        "; {unseen} shorter shots fill the rest and have no picture"
                    ));
                }
                line
            }
            None => format!("{} shots: unknown", self.tag),
        };
        (cells, line)
    }
}

/// Mean luminance (0..1) under which a strip picture counts as black.
const BLACK_LUMINANCE: f32 = 0.06;
/// Black this long inside a detected shot becomes its own shot.
const BLACK_RUN: f64 = 0.25;

/// Split detected shots where they enter or leave a black run. Scene
/// detection deliberately keeps flashes and dark frames inside a shot, so
/// a card that hard-cuts to black read as one shot and a fix spanning
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
    let mut counts = vec![1_usize; picked.len()];
    for _ in picked.len()..STRIP_CELLS {
        let longest = (0..picked.len())
            .max_by(|a, b| {
                let per = |i: usize| (picked[i].1 - picked[i].0) / counts[i] as f64;
                per(*a).total_cmp(&per(*b))
            })
            .unwrap();
        counts[longest] += 1;
    }
    picked
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

/// A strip source for each source whose visible picture the edit placed
/// or changed, at most `STRIP_SOURCES`, in program order.
pub(super) fn source_strip(
    before: &Timeline,
    after: &Timeline,
    assets: &[BinAsset],
) -> SourceStrip {
    let key = |clip: &openreel_engine::Clip| {
        (
            clip.source.clone(),
            clip.source_in.to_bits(),
            clip.duration.to_bits(),
        )
    };
    let old: Vec<_> = before
        .clips
        .iter()
        .filter(|clip| clip.kind == ClipKind::Video)
        .map(key)
        .collect();
    let mut sources: Vec<(String, Vec<&openreel_engine::Clip>)> = Vec::new();
    for segment in program_segments(after) {
        let Some(clip) = segment.video.and_then(|id| after.clip(id)) else {
            continue;
        };
        let Some(source) = clip.source.clone() else {
            continue;
        };
        if old.contains(&key(clip)) {
            continue;
        }
        if let Some((_, clips)) = sources.iter_mut().find(|(path, _)| *path == source) {
            if !clips.iter().any(|c| c.id == clip.id) {
                clips.push(clip);
            }
        } else if sources.len() < STRIP_SOURCES {
            sources.push((source, vec![clip]));
        }
    }
    let mut strip = SourceStrip {
        sources: Vec::new(),
        legend: Vec::new(),
    };
    for (path, clips) in &sources {
        let Some(asset) = assets.iter().find(|asset| {
            asset.info.path == *path && asset.info.has_video && asset.info.duration > 0.0
        }) else {
            continue;
        };
        let tag = format!("S{}", strip.sources.len() + 1);
        let placed: Vec<String> = clips
            .iter()
            .map(|clip| {
                format!(
                    "clip {} shows {:.2}-{:.2}s",
                    clip.id,
                    clip.source_in,
                    clip.source_in + clip.duration
                )
            })
            .collect();
        strip.legend.push(format!(
            "{tag} = {path} ({:.2}s long; {})",
            asset.info.duration,
            placed.join(", ")
        ));
        let scenes = &asset.sweep.scene_clips;
        strip.sources.push(StripSource {
            tag,
            info: asset.info.clone(),
            placed: clips
                .iter()
                .map(|clip| (clip.source_in, clip.source_in + clip.duration))
                .collect(),
            shots: (!scenes.is_empty()).then(|| {
                scenes
                    .iter()
                    .map(|scene| (scene.source_in, scene.source_in + scene.duration))
                    .collect()
            }),
        });
    }
    strip
}

/// One cell per visible shot (sampled evenly beyond `SHEET_CELLS`), each
/// at the middle of its stretch, labelled with its time and clip.
pub(super) fn sheet_cells(timeline: &Timeline) -> Vec<SheetCell> {
    let segments = program_segments(timeline);
    let picked: Vec<Segment> = if segments.len() <= SHEET_CELLS {
        segments
    } else {
        let end = timeline.end();
        (0..SHEET_CELLS)
            .filter_map(|index| {
                let at = (index as f64 + 0.5) * end / SHEET_CELLS as f64;
                segments
                    .iter()
                    .find(|segment| segment.start <= at && at < segment.end)
                    .map(|segment| Segment {
                        start: at,
                        end: at,
                        ..*segment
                    })
            })
            .collect()
    };
    picked
        .iter()
        .map(|segment| {
            let at = (segment.start + segment.end) / 2.0;
            let video = segment.video.and_then(|id| timeline.clip(id));
            let title = segment.title.and_then(|id| timeline.clip(id));
            let mut label = format!("{at:.2}s");
            match video {
                Some(clip) => label.push_str(&format!(" clip {} L{}", clip.id, clip.track)),
                None => label.push_str(" no picture"),
            }
            if let Some(title) = title {
                label.push_str(&format!(" title {}", title.id));
            }
            SheetCell {
                picture: video.and_then(|clip| {
                    Some((clip.source.clone()?, clip.source_in + (at - clip.start)))
                }),
                opacity: video.map_or(1.0, |clip| clip.opacity),
                title: title.map(|clip| (clip.name.clone(), clip.title_style, clip.opacity)),
                label,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::media::{AssetState, BinAsset};
    use crate::app::sweep::SweepData;

    fn asset(path: &str, width: u32, height: u32) -> BinAsset {
        BinAsset {
            info: openreel_media::MediaInfo {
                path: path.to_owned(),
                name: path.to_owned(),
                duration: 10.0,
                width,
                height,
                fps: 30.0,
                has_video: true,
                has_audio: true,
            },
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumb_textures: Vec::new(),
            sweep: SweepData::default(),
        }
    }

    fn kinds(findings: &[Finding]) -> Vec<(&'static str, Vec<ClipId>)> {
        findings
            .iter()
            .map(|finding| (finding.kind, finding.clips.clone()))
            .collect()
    }

    /// Each check fires on the clip that breaks it and stays quiet on a
    /// clean neighbour, including a titled gap and a short final clip.
    #[test]
    fn inspect_reports_broken_and_suspicious_clips() {
        let mut timeline = Timeline::empty_project();
        let fps = timeline.fps;
        let a = timeline.add_media_clip("A", ClipKind::Video, 0, 0.0, 2.0, 0.0, "a.mp4");
        // A one-frame flash between two shots.
        let flash = timeline.add_media_clip("F", ClipKind::Video, 0, 2.0, 1.0 / fps, 3.0, "a.mp4");
        let b =
            timeline.add_media_clip("B", ClipKind::Video, 0, 2.0 + 1.0 / fps, 2.0, 5.0, "a.mp4");
        // Untitled black gap, then a titled one (a title card is deliberate).
        let end_b = 4.0 + 1.0 / fps;
        let c = timeline.add_media_clip("C", ClipKind::Video, 0, end_b + 1.0, 1.0, 9.5, "a.mp4");
        let card = timeline.add_clip("Day two", ClipKind::Title, 3, end_b + 2.0, 1.0, 0.0);
        let d = timeline.add_media_clip("D", ClipKind::Video, 0, end_b + 3.0, 1.0, 0.0, "tall.mp4");
        // Under clip A on a higher lane: never visible.
        let hidden = timeline.add_media_clip("H", ClipKind::Video, 1, 0.5, 1.0, 0.0, "a.mp4");
        let missing = timeline.add_media_clip("M", ClipKind::Audio, 2, 0.0, 1.0, 0.0, "gone.wav");
        // The last frame of the timeline may be short.
        let tail = timeline.add_media_clip(
            "T",
            ClipKind::Video,
            0,
            end_b + 4.0,
            0.5 / fps,
            1.0,
            "a.mp4",
        );
        timeline.clip_mut(b).unwrap().fade_in = 1.5;
        timeline.clip_mut(b).unwrap().fade_out = 1.0;
        let mut text = asset("a.mp4", 1920, 1080);
        text.sweep
            .candidates
            .push(crate::app::sweep::SweepCandidate {
                id: "a.mp4#5.5".to_owned(),
                asset_path: "a.mp4".to_owned(),
                asset_name: "a.mp4".to_owned(),
                title: "a.mp4".to_owned(),
                source_in: 5.5,
                duration: 0.5,
                score: 90.0,
                reason: "test".to_owned(),
                stability: 0.9,
                audio_energy: 0.1,
                audio_peak: 0.1,
                category: "broll".to_owned(),
                tags: vec!["on-screen-text".to_owned()],
                motion_score: 0.3,
                audio_score: 0.1,
                music_bed: false,
                visual: None,
                has_video: true,
                transcript: Vec::new(),
                review: crate::app::sweep::ClipReview::default(),
                dismissed: false,
            });
        let assets = [text, asset("tall.mp4", 1080, 1920)];
        let found = kinds(&inspect(&timeline, &assets));
        let expected = vec![
            ("fade-too-long", vec![b]),
            ("on-screen-text", vec![b]),
            ("source-range", vec![c]),
            ("orientation", vec![d]),
            ("missing-media", vec![missing]),
            ("flash-frame", vec![flash]),
            ("picture-gap", vec![]),
            ("hidden-clip", vec![hidden]),
        ];
        for item in &expected {
            assert!(found.contains(item), "missing {item:?} in {found:?}");
        }
        assert_eq!(
            found.len(),
            expected.len(),
            "unexpected findings: {found:?}"
        );
        let gap = inspect(&timeline, &assets)
            .into_iter()
            .find(|finding| finding.kind == "picture-gap")
            .unwrap();
        assert!((gap.start - end_b).abs() < 1e-6 && (gap.end - (end_b + 1.0)).abs() < 1e-6);
        let _ = (a, card, tail);
    }

    /// Same-shot cuts a few frames off pop; exact continuations, real jump
    /// cuts and cuts between sources do not.
    #[test]
    fn near_match_cuts_report_skipped_and_repeated_frames() {
        let mut timeline = Timeline::empty_project();
        let frame = 1.0 / 30.0;
        // a.mp4 runs at 30 fps: 2 frames skipped, then a clean continuation,
        // then 1 frame repeated, then a 10-frame jump cut, then another file.
        let a = timeline.add_media_clip("A", ClipKind::Video, 0, 0.0, 1.0, 0.0, "a.mp4");
        let b = timeline.add_media_clip(
            "B",
            ClipKind::Video,
            0,
            1.0,
            1.0,
            1.0 + 2.0 * frame,
            "a.mp4",
        );
        let c = timeline.add_media_clip(
            "C",
            ClipKind::Video,
            0,
            2.0,
            1.0,
            2.0 + 2.0 * frame,
            "a.mp4",
        );
        let d = timeline.add_media_clip("D", ClipKind::Video, 0, 3.0, 1.0, 3.0 + frame, "a.mp4");
        let e = timeline.add_media_clip(
            "E",
            ClipKind::Video,
            0,
            4.0,
            1.0,
            4.0 + 11.0 * frame,
            "a.mp4",
        );
        timeline.add_media_clip(
            "F",
            ClipKind::Video,
            0,
            5.0,
            1.0,
            5.0 + 11.0 * frame,
            "b.mp4",
        );
        let assets = [asset("a.mp4", 1920, 1080), asset("b.mp4", 1920, 1080)];
        let found = inspect(&timeline, &assets);
        let pops: Vec<_> = found
            .iter()
            .filter(|finding| finding.kind == "near-match-cut")
            .collect();
        assert_eq!(
            pops.iter().map(|f| f.clips.clone()).collect::<Vec<_>>(),
            vec![vec![a, b], vec![c, d]],
            "{found:?}"
        );
        assert!(pops[0].message.contains("skips 2"), "{}", pops[0].message);
        assert!(pops[1].message.contains("repeats 1"), "{}", pops[1].message);
        let _ = e;
    }

    /// The strip covers sources whose visible picture the edit changed,
    /// sampled through the whole file on its own clock, and marks the
    /// samples inside the placed source range (not its program range).
    #[test]
    fn source_strip_samples_whole_sources_the_edit_placed() {
        let mut before = Timeline::empty_project();
        before.add_media_clip("A", ClipKind::Video, 0, 0.0, 2.0, 0.0, "a.mp4");
        let mut after = before.clone();
        let b = after.add_media_clip("B", ClipKind::Video, 0, 2.0, 2.0, 3.0, "b.mp4");
        // Hidden under B on a higher lane, and sound only: neither is shown.
        after.add_media_clip("H", ClipKind::Video, 1, 2.0, 2.0, 0.0, "c.mp4");
        after.add_media_clip("M", ClipKind::Audio, 2, 0.0, 4.0, 0.0, "d.mp4");
        let assets = ["a.mp4", "b.mp4", "c.mp4", "d.mp4"].map(|path| asset(path, 1920, 1080));
        let strip = source_strip(&before, &after, &assets);
        assert_eq!(
            strip.legend,
            vec![format!(
                "S1 = b.mp4 (10.00s long; clip {b} shows 3.00-5.00s)"
            )]
        );
        // Uncurated and undecodable: no shots, equal slices of the file.
        let (cells, shots) = strip.sources[0].cells(None);
        assert_eq!(shots, "S1 shots: unknown");
        let labels: Vec<&str> = cells.iter().map(|cell| cell.label.as_str()).collect();
        assert_eq!(labels.len(), STRIP_CELLS);
        assert_eq!(labels[0], "S1 0.31s");
        assert_eq!(labels[STRIP_CELLS - 1], "S1 9.69s");
        let marked: Vec<&str> = labels
            .iter()
            .copied()
            .filter(|l| l.ends_with('*'))
            .collect();
        assert_eq!(marked, vec!["S1 3.44s *", "S1 4.06s *", "S1 4.69s *"]);
        assert!(cells.iter().all(|cell| cell
            .picture
            .as_ref()
            .is_some_and(|(path, _)| path == "b.mp4")));
        let (_, shots) = strip.sources[0].cells(Some(&[(0.0, 2.5), (2.5, 10.0)]));
        assert_eq!(shots, "S1 shots with pictures: 0.00-2.50, 2.50-10.00");
        let many: Vec<(f64, f64)> = (0..20)
            .map(|i| (i as f64 * 0.5, i as f64 * 0.5 + 0.5))
            .collect();
        let (_, shots) = strip.sources[0].cells(Some(&many));
        assert!(
            shots.ends_with("; 8 shorter shots fill the rest and have no picture"),
            "{shots}"
        );
        // An edit that changes no visible picture gets no strip.
        assert!(source_strip(&after, &after, &assets).sources.is_empty());
    }

    /// Few shots: every shot gets a picture, however short, and the rest go
    /// where a picture covers the most time, none across a cut. Many shots:
    /// the longest are pictured wherever they fall, not every n-th one.
    #[test]
    fn strip_times_cover_the_longest_shots_and_split_them() {
        let times = strip_times(10.0, Some(&[(0.0, 0.5), (0.5, 9.0), (9.0, 10.0)]));
        let count = |times: &[(f64, (f64, f64))], start: f64, end: f64| {
            times
                .iter()
                .filter(|(t, _)| (start..end).contains(t))
                .count()
        };
        assert_eq!(times.len(), STRIP_CELLS);
        assert_eq!(
            (
                count(&times, 0.0, 0.5),
                count(&times, 0.5, 9.0),
                count(&times, 9.0, 10.0)
            ),
            (1, 13, 2)
        );
        assert_eq!(times[0], (0.25, (0.0, 0.5)));
        assert_eq!(times[14], (9.25, (9.0, 10.0)));
        assert!(times.iter().all(|(t, (start, end))| start <= t && t < end));
        // 47 one-second shots and a 5.3 s card at 31.18, as on honey's
        // 0vSt expo cut: index picking skipped the card entirely.
        let mut many: Vec<(f64, f64)> = (0..31).map(|i| (i as f64, i as f64 + 1.0)).collect();
        many.push((31.0, 31.18));
        many.push((31.18, 36.49));
        many.extend((0..14).map(|i| (36.49 + i as f64 * 0.5, 36.99 + i as f64 * 0.5)));
        let times = strip_times(43.49, Some(&many));
        assert_eq!(times.len(), STRIP_CELLS);
        assert!(times.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(
            count(&times, 31.18, 36.49) >= 3,
            "the longest shot keeps spare pictures: {times:?}"
        );
        assert_eq!(
            count(&times, 36.49, 43.49),
            0,
            "half-second shots stay unseen"
        );
    }

    /// honey's 0vSt card (32.15-34.58) hard-cuts to black inside one
    /// detected shot; the black becomes its own shot, a dark blink does not.
    #[test]
    fn strip_shots_split_where_a_shot_cuts_to_black() {
        let sample = |time: f64, luminance: f32| openreel_media::MotionSample {
            time,
            difference: 0.0,
            scene_score: 0.0,
            visual: openreel_media::FrameVisualEvidence {
                luminance,
                ..Default::default()
            },
        };
        let mut motion: Vec<_> = (0..300)
            .map(|frame| {
                let time = 31.18 + frame as f64 / 60.0;
                let blink = (32.0..32.05).contains(&time);
                sample(time, if time >= 34.58 || blink { 0.01 } else { 0.2 })
            })
            .collect();
        motion.retain(|s| s.time < 36.49);
        let shots = split_at_black(&[(0.0, 31.18), (31.18, 36.49)], &motion);
        assert_eq!(shots.len(), 3, "{shots:?}");
        assert_eq!(shots[0], (0.0, 31.18));
        assert_eq!(shots[1].0, 31.18);
        assert!((shots[1].1 - 34.58).abs() < 1.0 / 60.0, "{shots:?}");
        assert_eq!((shots[2].0, shots[2].1), (shots[1].1, 36.49));
    }

    #[test]
    fn sheet_cells_follow_visible_shots_and_titles() {
        let mut timeline = Timeline::empty_project();
        let a = timeline.add_media_clip("A", ClipKind::Video, 0, 0.0, 2.0, 1.0, "a.mp4");
        let b = timeline.add_media_clip("B", ClipKind::Video, 0, 3.0, 2.0, 4.0, "b.mp4");
        let title = timeline.add_clip("Hello", ClipKind::Title, 1, 1.0, 1.0, 0.0);
        let cells = sheet_cells(&timeline);
        let summary: Vec<_> = cells
            .iter()
            .map(|cell| {
                (
                    cell.picture.clone(),
                    cell.title.as_ref().map(|t| t.0.clone()),
                    cell.label.clone(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    Some(("a.mp4".to_owned(), 1.5)),
                    None,
                    format!("0.50s clip {a} L0")
                ),
                (
                    Some(("a.mp4".to_owned(), 2.5)),
                    Some("Hello".to_owned()),
                    format!("1.50s clip {a} L0 title {title}")
                ),
                (None, None, "2.50s no picture".to_owned()),
                (
                    Some(("b.mp4".to_owned(), 5.0)),
                    None,
                    format!("4.00s clip {b} L0")
                ),
            ]
        );
    }
}

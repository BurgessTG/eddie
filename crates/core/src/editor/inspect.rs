//! Timeline checks scripts and agents read (`inspect()`, `eddie inspect`):
//! measured facts about the timeline, not taste.

use openreel_engine::{Clip, ClipId, ClipKind, Orientation, Timeline};
use serde::Serialize;

use super::media::BinAsset;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding {
    pub kind: &'static str,
    /// "error" breaks the program; "warning" is probably unintended.
    pub severity: &'static str,
    pub message: String,
    pub clips: Vec<ClipId>,
    pub start: f64,
    pub end: f64,
}

/// A stretch of program time with one visible picture and one title.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub video: Option<ClipId>,
    pub title: Option<ClipId>,
}

/// The program as preview and export show it, from 0 to the timeline end,
/// split wherever the visible picture or title changes.
pub fn program_segments(timeline: &Timeline) -> Vec<Segment> {
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

/// A stretch of program time showing one picture, or black, whatever
/// titles change over it: its start, its end and the clip.
type Run = (f64, f64, Option<ClipId>);

pub fn inspect(timeline: &Timeline, assets: &[BinAsset]) -> Vec<Finding> {
    let mut inspection = Inspection {
        timeline,
        assets,
        frame: 1.0 / timeline.fps.max(1.0),
        findings: Vec::new(),
    };
    for clip in &timeline.clips {
        inspection.media(clip);
    }
    let segments = program_segments(timeline);
    let mut runs: Vec<Run> = Vec::new();
    for segment in &segments {
        match runs.last_mut() {
            Some(run) if run.2 == segment.video => run.1 = segment.end,
            _ => runs.push((segment.start, segment.end, segment.video)),
        }
    }
    inspection.gaps_and_flashes(&segments, &runs);
    inspection.near_match_cuts(&runs);
    inspection.hidden_clips(&runs);
    inspection.findings
}

/// One pass over a timeline: what it reads and what it has found.
struct Inspection<'a> {
    timeline: &'a Timeline,
    assets: &'a [BinAsset],
    /// One project frame, in seconds.
    frame: f64,
    findings: Vec<Finding>,
}

impl<'a> Inspection<'a> {
    fn asset(&self, source: &str) -> Option<&'a BinAsset> {
        self.assets.iter().find(|asset| asset.info.path == source)
    }

    fn report(
        &mut self,
        kind: &'static str,
        severity: &'static str,
        message: String,
        clips: Vec<ClipId>,
        start: f64,
        end: f64,
    ) {
        self.findings.push(Finding {
            kind,
            severity,
            message,
            clips,
            start,
            end,
        });
    }

    /// A clip against the file it plays: in the bin, the project's
    /// orientation, inside the file, fades that fit, no burned-in text.
    fn media(&mut self, clip: &Clip) {
        let (timeline, frame) = (self.timeline, self.frame);
        let Some(source) = clip.source.as_deref() else {
            return;
        };
        let (id, start, end) = (clip.id, clip.start, clip.end());
        let Some(asset) = self.asset(source) else {
            self.report(
                "missing-media",
                "error",
                format!("Clip {id} uses media that is not in the bin."),
                vec![id],
                start,
                end,
            );
            return;
        };
        let info = &asset.info;
        if clip.kind == ClipKind::Video
            && info.has_video
            && Orientation::of(info.width, info.height) != timeline.orientation
        {
            self.report(
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
            self.report(
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
            self.report(
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
            self.burned_in_text(clip, asset);
        }
    }

    /// Ranges the curator tagged `on-screen-text` that the clip shows.
    fn burned_in_text(&mut self, clip: &Clip, asset: &BinAsset) {
        let (id, start, frame) = (clip.id, clip.start, self.frame);
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
            self.report(
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

    /// Black where a picture should be, and pictures too short to see.
    fn gaps_and_flashes(&mut self, segments: &[Segment], runs: &[Run]) {
        let (timeline, frame) = (self.timeline, self.frame);
        let last_picture = segments
            .iter()
            .filter(|segment| segment.video.is_some())
            .map(|segment| segment.end)
            .fold(0.0, f64::max);
        for (start, end, video) in runs {
            let (start, end) = (*start, *end);
            match video {
                None if end <= last_picture + 1e-9 && end - start >= frame / 2.0 => {
                    let titled = segments.iter().all(|segment| {
                        segment.end <= start || segment.start >= end || segment.title.is_some()
                    });
                    if !titled {
                        self.report(
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
                    self.report(
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
    }

    /// A cut inside one shot that skips or repeats a frame or two pops; it
    /// is usually left from a split, trim or swap (fframes' continuity scan
    /// finds these in pixels; the timeline already knows them).
    fn near_match_cuts(&mut self, runs: &[Run]) {
        let (timeline, frame) = (self.timeline, self.frame);
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
                .and_then(|source| self.asset(source))
                .map_or(frame, |asset| 1.0 / asset.info.fps.max(1.0));
            let jump = (b.source_in + (cut - b.start)) - (a.source_in + (cut - a.start));
            let frames = (jump / source_frame).round();
            if frames != 0.0 && frames.abs() <= 3.0 {
                let (verb, count) = if frames > 0.0 {
                    ("skips", frames)
                } else {
                    ("repeats", -frames)
                };
                self.report(
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
    }

    /// Video a lower lane covers for its whole length.
    fn hidden_clips(&mut self, runs: &[Run]) {
        let timeline = self.timeline;
        for clip in &timeline.clips {
            if clip.kind == ClipKind::Video && !runs.iter().any(|run| run.2 == Some(clip.id)) {
                // A voice kept under a cutaway is covered on purpose; say what
                // still plays so the reader can tell that from a lost picture.
                let heard = clip.embedded_audio
                    && clip.volume > 0.0
                    && clip
                        .source
                        .as_deref()
                        .and_then(|source| self.asset(source))
                        .is_none_or(|asset| asset.info.has_audio);
                self.report(
                    "hidden-clip",
                    "warning",
                    format!(
                        "Clip {} on lane {} is always covered by a lower lane: {}.",
                        clip.id,
                        clip.track,
                        if heard {
                            "its picture never shows, only its sound plays"
                        } else {
                            "it never shows"
                        }
                    ),
                    vec![clip.id],
                    clip.start,
                    clip.end(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;

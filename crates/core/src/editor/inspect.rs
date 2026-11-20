//! Timeline checks scripts and agents read (`inspect()`, `eddie inspect`):
//! measured facts about the timeline, not taste.

use std::collections::HashSet;

use openreel_engine::{hides_below, place, Clip, ClipId, ClipKind, Timeline};
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

/// A stretch of program time with one top picture and one title.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub video: Option<ClipId>,
    pub title: Option<ClipId>,
}

/// The program as preview and export show it, from 0 to the timeline end,
/// split wherever the top picture or title changes. Lanes under the top
/// picture may show around or through it.
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
    let canvas = openreel_media::export_canvas(timeline, |path| {
        let asset = assets.iter().find(|asset| asset.info.path == path)?;
        Some((asset.info.width, asset.info.height))
    });
    let mut inspection = Inspection {
        timeline,
        assets,
        frame: 1.0 / timeline.fps.max(1.0),
        canvas: (f64::from(canvas.0), f64::from(canvas.1)),
        findings: Vec::new(),
    };
    for clip in &timeline.clips {
        inspection.media(clip);
        inspection.title(clip);
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
    inspection.hidden_clips();
    inspection.transitions();
    inspection.findings
}

/// One pass over a timeline: what it reads and what it has found.
struct Inspection<'a> {
    timeline: &'a Timeline,
    assets: &'a [BinAsset],
    /// One project frame, in seconds.
    frame: f64,
    /// The canvas preview and export draw on, in pixels.
    canvas: (f64, f64),
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

    /// Transitions that do not play as set: with no clip right before, cut
    /// short by a short clip, or past where a side's file has pictures, so
    /// its edge picture holds.
    fn transitions(&mut self) {
        let timeline = self.timeline;
        let spans = timeline.transitions();
        for clip in &timeline.clips {
            let Some(transition) = clip.transition else {
                continue;
            };
            let id = clip.id;
            let Some(span) = spans.iter().find(|span| span.to == id) else {
                self.report(
                    "transition",
                    "warning",
                    format!("Clip {id}'s transition has no clip right before it on its lane, so it does not play."),
                    vec![id],
                    clip.start,
                    clip.start,
                );
                continue;
            };
            let (start, end) = (span.start, span.end);
            if end - start < transition.duration - self.frame / 2.0 {
                self.report(
                    "transition",
                    "warning",
                    format!(
                        "Clip {id}'s {:.2} s transition plays {:.2} s: each side takes at most half of its clip.",
                        transition.duration,
                        end - start
                    ),
                    vec![span.from, id],
                    start,
                    end,
                );
            }
            let assets = self.assets;
            let file = |clip: &Clip| {
                let source = clip.source.as_deref().filter(|_| !clip.still())?;
                let asset = assets.iter().find(|asset| asset.info.path == source)?;
                Some(asset.info.duration).filter(|duration| *duration > 0.0)
            };
            if let Some(from) = timeline.clip(span.from) {
                let past = file(from).map_or(0.0, |duration| {
                    (from.source_at(end) - duration) / from.speed
                });
                if past > self.frame {
                    self.report(
                        "transition",
                        "warning",
                        format!(
                            "Clip {}'s file ends {past:.2} s before the transition into clip {id} does; its last picture holds.",
                            from.id
                        ),
                        vec![from.id, id],
                        end - past,
                        end,
                    );
                }
            }
            let before = file(clip).map_or(0.0, |_| -clip.source_at(start) / clip.speed);
            if before > self.frame {
                self.report(
                    "transition",
                    "warning",
                    format!(
                        "Clip {id}'s file starts {before:.2} s after the transition into it does; its first picture holds."
                    ),
                    vec![span.from, id],
                    start,
                    start + before,
                );
            }
        }
    }

    /// A title moved off the canvas: the middle of its line (its left end,
    /// when its style starts text at the left) outside the picture.
    fn title(&mut self, clip: &Clip) {
        if clip.kind != ClipKind::Title || (clip.text.x == 0.0 && clip.text.y == 0.0) {
            return;
        }
        let look = openreel_media::clip_title_look(clip);
        let (w, h) = self.canvas;
        let x = look.left.map_or(w / 2.0, f64::from) + look.offset.0 * w;
        let y = h - f64::from(look.bottom) + f64::from(look.font_size) / 2.0 + look.offset.1 * h;
        if (0.0..=w).contains(&x) && (0.0..=h).contains(&y) {
            return;
        }
        self.report(
            "off-canvas",
            "warning",
            format!("Title {}'s text is moved off the canvas.", clip.id),
            vec![clip.id],
            clip.start,
            clip.end(),
        );
    }

    /// A clip against the file it plays: in the bin, its picture on the
    /// canvas, inside the file, fades that fit, no burned-in text.
    fn media(&mut self, clip: &Clip) {
        let frame = self.frame;
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
        let source = (f64::from(info.width), f64::from(info.height));
        if clip.kind == ClipKind::Video
            && info.has_video
            && !place(self.canvas, source, &clip.transform).on_canvas(self.canvas)
        {
            self.report(
                "off-canvas",
                "warning",
                format!("Clip {id}'s picture is moved off the canvas, so it shows black."),
                vec![id],
                start,
                end,
            );
        }
        if info.duration > 0.0
            && (clip.source_in < -1e-6 || clip.source_end() > info.duration + frame)
        {
            self.report(
                "source-range",
                "error",
                format!(
                    "Clip {id} plays source {:.2}-{:.2}s; the file is {:.2}s long.",
                    clip.source_in,
                    clip.source_end(),
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
        let (id, frame) = (clip.id, self.frame);
        let text = asset
            .sweep
            .candidates
            .iter()
            .chain(&asset.sweep.scene_clips)
            .filter(|range| range.tags.iter().any(|tag| tag == "on-screen-text"))
            .map(|range| {
                let from = range.source_in.max(clip.source_in);
                let to = range.end().min(clip.source_end());
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
                clip.time_at(from),
                clip.time_at(to),
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
            let jump = b.source_at(cut) - a.source_at(cut);
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

    /// Video that the opaque, frame-filling pictures of the lanes above
    /// cover for its whole length ([`Timeline::video_stack`]).
    fn hidden_clips(&mut self) {
        let timeline = self.timeline;
        let spans = timeline.transitions();
        let hides = |clip: &Clip| {
            clip.source
                .as_deref()
                .and_then(|source| self.asset(source))
                .is_some_and(|asset| {
                    let info = &asset.info;
                    let size = (f64::from(info.width), f64::from(info.height));
                    hides_below(clip, self.canvas, size, info.alpha)
                })
        };
        let mut edges: Vec<f64> = timeline
            .clips
            .iter()
            .flat_map(|clip| [clip.start, clip.end()])
            .chain(spans.iter().flat_map(|span| [span.start, span.end]))
            .collect();
        edges.sort_by(f64::total_cmp);
        edges.dedup();
        let mut shown = HashSet::new();
        for pair in edges.windows(2) {
            let middle = (pair[0] + pair[1]) / 2.0;
            for picture in timeline.video_stack(&spans, middle, &hides) {
                shown.insert(picture.clip.id);
                shown.extend(picture.transition.map(|span| span.to));
            }
        }
        for clip in &timeline.clips {
            if clip.kind == ClipKind::Video && !shown.contains(&clip.id) {
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
                        "Clip {} on lane {} is always covered by a lane above it: {}.",
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

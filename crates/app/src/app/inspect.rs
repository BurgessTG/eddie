//! Timeline checks the chat agent reads (`inspect()`) and receives after
//! each edit, with a contact sheet of the program, so it can see what its
//! edit produced. Measured facts about the timeline, not taste.

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

use super::*;
use crate::editor::sweep::SweepData;

fn asset(path: &str) -> BinAsset {
    BinAsset {
        info: MediaInfo {
            path: path.to_owned(),
            name: path.to_owned(),
            duration: 10.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_video: true,
            has_audio: true,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks: Vec::new(),
        beats: None,
        thumbs: Vec::new(),
        sweep: SweepData::default(),
    }
}

/// A strip marks the source ranges the timeline's pictures show, on the
/// source's own clock, and ignores sound-only use and other sources.
#[test]
fn a_strip_marks_the_source_ranges_the_timeline_shows() {
    let mut timeline = Timeline::empty_project();
    timeline.add_media_clip("A", ClipKind::Video, 0, 0.0, 2.0, 0.0, "a.mp4");
    timeline.add_media_clip("B", ClipKind::Video, 0, 2.0, 2.0, 3.0, "b.mp4");
    timeline.add_media_clip("M", ClipKind::Audio, 2, 0.0, 4.0, 6.0, "b.mp4");
    let source = StripSource::new(&asset("b.mp4"), &timeline, None);
    assert_eq!(source.placed, vec![(3.0, 5.0)]);
    // Uncurated and undecodable: no shots, equal slices of the file.
    assert_eq!(source.shots(), None);
    let times = strip_times(source.info.duration, None);
    assert_eq!(times.len(), STRIP_CELLS);
    assert!((times[0].0 - 0.3125).abs() < 1e-9);
    assert!((times[STRIP_CELLS - 1].0 - 9.6875).abs() < 1e-9);
    let marked: Vec<String> = times
        .iter()
        .filter(|(time, _)| source.is_placed(*time))
        .map(|(time, _)| format!("{time:.2}"))
        .collect();
    assert_eq!(marked, vec!["3.44", "4.06", "4.69"]);
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
    // 47 one-second shots and a 5.3 s card at 31.18, as in a real
    // expo cut: index picking skipped the card entirely.
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

/// A real logo card (32.15-34.58) hard-cuts to black inside one
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
    let summary = |span: (f64, f64)| -> (Vec<_>, usize) {
        let (shots, total) = sheet_shots(&timeline, span);
        let cells = shots
            .iter()
            .map(|shot| {
                let cell = shot.cell();
                (cell.picture, cell.title.map(|title| title.0), cell.label)
            })
            .collect();
        (cells, total)
    };
    assert_eq!(
        summary((0.0, timeline.end())),
        (
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
            ],
            3
        )
    );
    // A range keeps the shots it overlaps, cut to the range.
    assert_eq!(
        summary((2.5, 4.0)),
        (
            vec![
                (None, None, "2.75s no picture".to_owned()),
                (
                    Some(("b.mp4".to_owned(), 4.5)),
                    None,
                    format!("3.50s clip {b} L0")
                ),
            ],
            2
        )
    );
}

/// Captions change far more often than pictures. They are not shots,
/// and they must not crowd a short shot off the sheet.
#[test]
fn captions_neither_count_as_shots_nor_crowd_one_off_the_sheet() {
    let mut timeline = Timeline::empty_project();
    let a = timeline.add_media_clip("A", ClipKind::Video, 0, 0.0, 7.6, 0.0, "a.mp4");
    // Even sampling of 15 s into 16 cells looks at 7.03 s and 7.97 s.
    let b = timeline.add_media_clip("B", ClipKind::Video, 0, 7.6, 0.3, 0.0, "b.mp4");
    let c = timeline.add_media_clip("C", ClipKind::Video, 0, 7.9, 7.1, 0.0, "c.mp4");
    for line in 0..30 {
        let at = line as f64 * 0.5;
        timeline.add_clip(&format!("Line {line}"), ClipKind::Title, 1, at, 0.5, 0.0);
    }
    let (cells, shots) = sheet_shots(&timeline, (0.0, 15.0));
    assert_eq!((cells.len(), shots), (SHEET_CELLS, 3));
    let pictured: Vec<ClipId> = [a, b, c]
        .into_iter()
        .filter(|id| {
            cells
                .iter()
                .any(|cell| cell.video.as_ref().is_some_and(|video| video.0 == *id))
        })
        .collect();
    assert_eq!(pictured, [a, b, c]);
    assert!(cells.windows(2).all(|pair| pair[0].time < pair[1].time));
    // The spare cells show captions spread through the span, not the
    // first thirteen.
    assert!(cells.last().unwrap().time > 13.0);
}

/// More shots than cells: the sheet samples the span evenly and says how
/// many shots it holds, so a caller can ask for a narrower range.
#[test]
fn a_crowded_sheet_samples_evenly_and_counts_every_shot() {
    let mut timeline = Timeline::empty_project();
    for index in 0..40 {
        timeline.add_media_clip(
            "S",
            ClipKind::Video,
            0,
            index as f64,
            1.0,
            0.0,
            &format!("{index}.mp4"),
        );
    }
    let (shots, total) = sheet_shots(&timeline, (0.0, 40.0));
    assert_eq!((shots.len(), total), (SHEET_CELLS, 40));
    assert_eq!(shots[0].time, 1.25);
    assert_eq!(shots[15].time, 38.75);
    let (shots, total) = sheet_shots(&timeline, (10.0, 20.0));
    assert_eq!((shots.len(), total), (10, 10));
}

#[test]
fn frame_times_follow_the_range_the_step_and_the_limits() {
    let index = TimeIndex {
        fps: 25.0,
        end: 60.0,
        clips: vec![(7, 12.0, 14.0)],
    };
    let times = |range: &str, every: Option<f64>| frame_times(&index, range, every);
    assert_eq!(times("12.5", None), Ok((12.5, 12.5, vec![12.5])));
    assert_eq!(times("clip:7@end", None), Ok((14.0, 14.0, vec![14.0])));
    // Half-open: the end itself is not sampled.
    assert_eq!(
        times("12..13", Some(0.25)),
        Ok((12.0, 13.0, vec![12.0, 12.25, 12.5, 12.75]))
    );
    let (start, end, spread) = times("clip:7", None).unwrap();
    assert_eq!((start, end, spread.len()), (12.0, 14.0, FRAMES_DEFAULT));
    assert_eq!((spread[0], spread[1]), (12.0, 12.125));
    // Never closer than the source's own pictures.
    assert_eq!(times("0..0.1", None).unwrap().2.len(), 3);
    // A range past the end stops at the end; a time past it is a mistake.
    assert_eq!(
        times("59..70", Some(0.5)),
        Ok((59.0, 60.0, vec![59.0, 59.5]))
    );
    assert_eq!(times("end", None), Ok((60.0, 60.0, vec![60.0])));
    assert!(times("61", None)
        .unwrap_err()
        .contains("outside the source"));
    assert!(times("61..70", None)
        .unwrap_err()
        .contains("outside the source"));
    assert!(times("0..60", Some(0.1))
        .unwrap_err()
        .contains("at most 40"));
    assert!(times("12..13", Some(0.0)).is_err());
    assert!(times("clip:9", None).unwrap_err().contains("no clip 9"));
}

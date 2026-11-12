use super::scene::{
    block_histogram_difference, frame_difference, histogram_difference, rgb_histogram,
    rgb_histograms, scene_score,
};
use super::*;

/// Sound that starts after the picture must keep its place: speech
/// models stamp words from the first sample they are given.
#[test]
fn excerpts_keep_late_starting_sound_at_its_own_time() {
    let dir = std::env::temp_dir().join(format!("openreel_late_audio_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("late.mp4");
    let made = crate::cli::ffmpeg()
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("color=c=blue:s=64x36:r=30:d=3")
        .args(["-itsoffset", "1.5", "-f", "lavfi", "-i", "sine=f=440:d=1.5"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(made.success());
    let file = path.to_str().unwrap();
    let first_sound = |pcm: &[f32]| {
        pcm.iter()
            .position(|s| s.abs() > 0.01)
            .map(|i| i as f64 / 16_000.0)
    };

    let whole = pcm_mono_16k(file, 0.0, 3.0).unwrap();
    assert!((whole.len() as i64 - 48_000).abs() < 800, "{}", whole.len());
    let heard = first_sound(&whole).unwrap();
    assert!((heard - 1.5).abs() < 0.05, "sound at {heard}");

    // From inside the silent lead, the remaining silence is kept.
    let part = pcm_mono_16k(file, 1.0, 1.5).unwrap();
    let heard = first_sound(&part).unwrap();
    assert!((heard - 0.5).abs() < 0.05, "sound at {heard}");

    // An upload chunk has the same shape: 3 s of 24 kHz 16-bit mono.
    let wav = dir.join("chunk.wav");
    write_wav_24k(file, 0.0, 3.0, &wav).unwrap();
    let bytes = std::fs::metadata(&wav).unwrap().len() as i64;
    assert!((bytes - 144_000).abs() < 3_000, "{bytes}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn frame_difference_detects_changed_pixels() {
    let previous = vec![0_u8; 64 * 4];
    let mut current = previous.clone();
    current[0] = 255;
    current[1] = 255;
    current[2] = 255;
    assert!(frame_difference(&previous, &current) > 0.0);
    assert_eq!(frame_difference(&previous, &previous), 0.0);
}

#[test]
fn segmentation_prefers_scene_spike_and_caps_length() {
    let motion: Vec<MotionSample> = (0..=60)
        .map(|i| MotionSample {
            time: i as f64 * 0.5,
            difference: if i == 8 { 0.5 } else { 0.01 },
            scene_score: if i == 8 { 0.5 } else { 0.01 },
            visual: FrameVisualEvidence::default(),
        })
        .collect();
    let peaks = vec![0.5; 600];
    let segments = build_segments(30.0, &motion, &peaks, 20);
    assert_eq!(segments[0].end, 4.0);
    assert!(segments.iter().all(|s| s.duration() <= 8.01));
    assert!(segments[0].stability > 0.5);
    assert!(segments[0].audio_peak > 0.4);
}

#[test]
fn audio_coverage_separates_beds_from_pauses() {
    // Constant loud peaks: every bucket audible (music bed shape).
    let bed = build_segments(4.0, &[], &[0.6; 80], 20);
    assert!(bed[0].audio_coverage > 0.95, "{}", bed[0].audio_coverage);
    // Sparse peaks: mostly silence between hits (speech shape).
    let mut sparse = vec![0.0_f32; 80];
    sparse[10] = 0.9;
    sparse[40] = 0.8;
    let speech = build_segments(4.0, &[], &sparse, 20);
    assert!(
        speech[0].audio_coverage < 0.1,
        "{}",
        speech[0].audio_coverage
    );
}

#[test]
fn segmentation_cuts_close_visual_scene_changes() {
    // 10 fps samples for 6 seconds. Pixel movement is noisy in
    // places, but scene_score spikes only at true switches.
    let motion: Vec<MotionSample> = (0..=60)
        .map(|i| {
            let cut = matches!(i, 10 | 32 | 37);
            MotionSample {
                time: i as f64 * 0.1,
                difference: if matches!(i, 18 | 44) { 0.14 } else { 0.02 },
                scene_score: if cut { 0.8 } else { 0.02 },
                visual: FrameVisualEvidence::default(),
            }
        })
        .collect();
    let segments = build_segments(6.0, &motion, &[0.0; 120], 20);
    let starts: Vec<f64> = segments.iter().map(|segment| segment.start).collect();
    assert_eq!(starts, vec![0.0, 1.0, 3.2, 3.7]);
}

#[test]
fn scene_cuts_survive_repeated_frame_cadence() {
    for repeats in [1, 2, 3] {
        let motion: Vec<MotionSample> = (0..90 * repeats)
            .map(|frame| {
                let original = frame / repeats;
                let repeated = frame % repeats != 0;
                MotionSample {
                    time: frame as f64 / (30 * repeats) as f64,
                    difference: if repeated { 0.0 } else { 0.15 },
                    scene_score: if repeated {
                        0.0
                    } else if original == 50 {
                        0.38
                    } else {
                        0.19 + (original % 3) as f32 * 0.01
                    },
                    visual: FrameVisualEvidence::default(),
                }
            })
            .collect();
        let scenes = build_scene_segments(3.0, &motion, &[], 50);
        let starts: Vec<_> = scenes.iter().map(|scene| scene.start).collect();
        assert_eq!(
            starts,
            [0.0, 50.0 / 30.0],
            "{repeats} copies per source frame"
        );
    }
}

/// Scores measured on real footage, one per frame after `lead` calm
/// frames, as the scene starts they produce.
fn scene_starts(fps: f64, lead: usize, scores: &[f32]) -> Vec<f64> {
    let motion: Vec<MotionSample> = std::iter::repeat_n(0.02, lead)
        .chain(scores.iter().copied())
        .chain(std::iter::repeat_n(0.015, 12))
        .enumerate()
        .map(|(frame, scene_score)| MotionSample {
            time: frame as f64 / fps,
            difference: scene_score,
            scene_score,
            visual: FrameVisualEvidence::default(),
        })
        .collect();
    let duration = motion.len() as f64 / fps;
    build_scene_segments(duration, &motion, &[], 50)
        .iter()
        .map(|scene| (scene.start * fps).round() / fps)
        .collect()
}

/// A 0.67 s hyperlapse shot: every frame is nearly a new picture. The
/// calm shot before it must not set its baseline, or frames inside it
/// pass as cuts and its real end, judged against both shots at once,
/// does not.
#[test]
fn a_fast_motion_shot_ends_at_its_cut_not_inside_itself() {
    let starts = scene_starts(
        30.0,
        40,
        &[
            0.513, 0.193, 0.199, 0.234, 0.231, 0.191, 0.225, 0.245, 0.235, 0.088, 0.168, 0.217,
            0.255, 0.239, 0.278, 0.246, 0.259, 0.300, 0.186, 0.214, 0.542,
        ],
    );
    assert_eq!(starts, [0.0, 40.0 / 30.0, 60.0 / 30.0]);
}

/// A 0.23 s shot whose few frames vary widely still ends where a score
/// stands half again above all of them.
#[test]
fn a_short_uneven_shot_keeps_its_cut() {
    let starts = scene_starts(
        60.0,
        80,
        &[
            0.484, 0.007, 0.088, 0.002, 0.140, 0.004, 0.210, 0.004, 0.141, 0.004, 0.075, 0.001,
            0.211, 0.009, 0.382,
        ],
    );
    assert_eq!(starts, [0.0, 80.0 / 60.0, 94.0 / 60.0]);
}

#[test]
fn scene_segments_cover_every_shot_without_candidate_cap_splits() {
    // 20 fps for 12 seconds, including a two-frame insert and
    // a final 10-second shot. Scene breakdown must cover the
    // whole source contiguously and must not split the long
    // shot at the 8-second curator-candidate cap.
    let motion: Vec<MotionSample> = (0..=240)
        .map(|i| MotionSample {
            time: i as f64 * 0.05,
            difference: 0.02,
            scene_score: if matches!(i, 20 | 22 | 40) { 0.8 } else { 0.02 },
            visual: FrameVisualEvidence::default(),
        })
        .collect();
    let segments = build_scene_segments(12.0, &motion, &[0.0; 240], 20);
    let starts: Vec<f64> = segments.iter().map(|segment| segment.start).collect();
    assert_eq!(starts, vec![0.0, 1.0, 1.1, 2.0]);
    assert_eq!(segments.last().expect("last scene").end, 12.0);
    assert!((segments.last().expect("last scene").duration() - 10.0).abs() < 1e-9);
    for pair in segments.windows(2) {
        assert_eq!(
            pair[0].end, pair[1].start,
            "scene coverage must be contiguous"
        );
    }
}

#[test]
fn scene_segments_preserve_one_frame_inserts_at_high_rates_and_vfr() {
    for fps in [30, 60, 120] {
        let motion: Vec<MotionSample> = (0..fps * 2)
            .map(|frame| MotionSample {
                time: frame as f64 / fps as f64,
                difference: 0.0,
                scene_score: if frame == fps / 2 {
                    0.8
                } else if frame == fps / 2 + 1 {
                    0.5
                } else {
                    0.0
                },
                visual: FrameVisualEvidence::default(),
            })
            .collect();
        let scenes = build_scene_segments(2.0, &motion, &[], 50);
        let starts: Vec<_> = scenes.iter().map(|scene| scene.start).collect();
        assert_eq!(starts, [0.0, 0.5, (fps / 2 + 1) as f64 / fps as f64]);
        assert_eq!(scenes.last().unwrap().end, 2.0);
    }
    let motion: Vec<MotionSample> = [0.0, 0.1, 0.45, 0.455, 0.9, 1.9]
        .into_iter()
        .map(|time| MotionSample {
            time,
            difference: 0.0,
            scene_score: if time == 0.45 {
                0.8
            } else if time == 0.455 {
                0.5
            } else {
                0.0
            },
            visual: FrameVisualEvidence::default(),
        })
        .collect();
    let scenes = build_scene_segments(2.0, &motion, &[], 50);
    let starts: Vec<_> = scenes.iter().map(|scene| scene.start).collect();
    assert_eq!(starts, [0.0, 0.45, 0.455]);
}

#[test]
fn block_histogram_catches_same_palette_layout_cut() {
    let width = 64_usize;
    let height = 36_usize;
    let mut previous = vec![0_u8; width * height * 4];
    let mut current = vec![0_u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let index = (y * width + x) * 4;
            if x < width / 2 {
                previous[index] = 255;
            } else {
                previous[index + 2] = 255;
            }
            if y < height / 2 {
                current[index] = 255;
            } else {
                current[index + 2] = 255;
            }
            previous[index + 3] = 255;
            current[index + 3] = 255;
        }
    }
    let (previous_global, previous_blocks) = rgb_histograms(&previous, width as u32, height as u32);
    let (current_global, current_blocks) = rgb_histograms(&current, width as u32, height as u32);
    let global_diff = histogram_difference(&previous_global, &current_global);
    let block_diff = block_histogram_difference(&previous_blocks, &current_blocks);
    assert!(
        global_diff < 0.01,
        "global palette is unchanged: {global_diff}"
    );
    assert!(block_diff > 0.4, "layout changed materially: {block_diff}");
    assert!(
        scene_score(
            frame_difference(&previous, &current),
            global_diff,
            block_diff
        ) > 0.35
    );
}

#[test]
fn scene_score_uses_histogram_not_movement_alone() {
    // Same black/white palette with halves swapped: lots of
    // pixel movement, identical color histogram. It is motion,
    // not clear evidence of a new scene by itself.
    let mut previous = vec![0_u8; 128 * 4];
    let mut current = vec![255_u8; 128 * 4];
    for pixel in 0..64 {
        previous[pixel * 4] = 255;
        previous[pixel * 4 + 1] = 255;
        previous[pixel * 4 + 2] = 255;
        current[pixel * 4] = 0;
        current[pixel * 4 + 1] = 0;
        current[pixel * 4 + 2] = 0;
    }
    let previous_hist = rgb_histogram(&previous);
    let hist_diff = histogram_difference(&previous_hist, &rgb_histogram(&current));
    assert!(hist_diff < 0.01, "hist diff {hist_diff}");
    assert!(frame_difference(&previous, &current) > 0.4);
}

#[test]
fn per_frame_detection_finds_known_hard_cuts() {
    let dir = std::env::temp_dir().join(format!("openreel_scene_cuts_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("known_cuts.mp4");
    let output = crate::cli::ffmpeg()
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=160x90:r=10:d=1",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x90:r=10:d=2",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=160x90:r=10:d=0.5",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x90:r=10:d=2.5",
            "-filter_complex",
            "[0:v][1:v][2:v][3:v]concat=n=4:v=1:a=0[v]",
            "-map",
            "[v]",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&path)
        .output()
        .expect("generate known-cut video");
    assert!(
        output.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let info = crate::probe(path.to_str().unwrap()).expect("probe known cuts");
    let samples = motion_samples(path.to_str().unwrap(), &info).expect("motion samples");
    assert!(
        (55..=65).contains(&samples.len()),
        "expected per-frame samples, got {}",
        samples.len()
    );
    let cuts = detect_scene_cuts(&samples);
    for expected in [1.0, 3.0, 3.5] {
        assert!(
            cuts.iter().any(|cut| (cut - expected).abs() <= 0.15),
            "missing cut near {expected}: {cuts:?}"
        );
    }
    assert_eq!(cuts.len(), 3, "unexpected extra cuts: {cuts:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn per_frame_detection_rejects_smooth_lighting_and_camera_pan() {
    let dir =
        std::env::temp_dir().join(format!("openreel_continuous_shots_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    for (name, filter) in [
        ("lighting", "geq=lum='30+2*N':cb=128:cr=128"),
        (
            "pan",
            "geq=lum='30+190*mod(floor((X+2*N)/12)+floor(Y/12),2)':cb=128:cr=128",
        ),
    ] {
        let path = dir.join(format!("{name}.mp4"));
        let output = crate::cli::ffmpeg()
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "nullsrc=s=160x90:r=30:d=3",
                "-vf",
                filter,
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-threads:v",
                "1",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .output()
            .expect("generate continuous shot");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let info = crate::probe(path.to_str().unwrap()).expect("probe continuous shot");
        let samples = motion_samples(path.to_str().unwrap(), &info).expect("motion samples");
        let scenes = build_scene_segments(info.duration, &samples, &[], 50);
        assert_eq!(scenes.len(), 1, "{name} is one continuous shot: {scenes:?}");
        assert_eq!(scenes[0].start, 0.0);
        assert_eq!(scenes[0].end, info.duration);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn frame_visual_evidence_tracks_centered_foreground() {
    let width = 8_usize;
    let height = 8_usize;
    let previous = vec![0_u8; width * height * 4];
    let mut current = previous.clone();
    for y in 2..6 {
        for x in 2..6 {
            let index = (y * width + x) * 4;
            current[index] = 255;
            current[index + 1] = 255;
            current[index + 2] = 255;
            current[index + 3] = 255;
        }
    }

    let evidence = frame_visual_evidence(Some(&previous), &current, width as u32, height as u32);
    assert!(evidence.foreground_coverage > 0.15, "{evidence:?}");
    let center_x = evidence.foreground_center_x.expect("foreground center x");
    let center_y = evidence.foreground_center_y.expect("foreground center y");
    assert!((0.35..=0.65).contains(&center_x), "center x {center_x}");
    assert!((0.35..=0.65).contains(&center_y), "center y {center_y}");
    assert!(evidence.center_saliency > 0.5, "{evidence:?}");
    assert!(evidence.edge_density > 0.0, "{evidence:?}");
}

#[test]
fn segment_visual_evidence_aggregates_subject_proxy() {
    let visual = FrameVisualEvidence {
        foreground_coverage: 0.25,
        foreground_center_x: Some(0.5),
        foreground_center_y: Some(0.45),
        center_saliency: 0.8,
        edge_density: 0.3,
        luminance: 0.6,
        contrast: 0.2,
        skin_tone_ratio: 0.1,
    };
    let motion: Vec<MotionSample> = (0..10)
        .map(|index| MotionSample {
            time: index as f64 * 0.1,
            difference: 0.02,
            scene_score: 0.0,
            visual,
        })
        .collect();
    let segments = build_segments(1.0, &motion, &[0.0; 20], 20);
    let evidence = segments[0]
        .visual
        .as_ref()
        .expect("video segment carries visual evidence");
    assert!((evidence.subject_coverage - 0.25).abs() < 0.001);
    assert!((evidence.subject_center_x.expect("center x") - 0.5).abs() < 0.001);
}

fn subject_sample(time: f64, coverage: f32, x: f32) -> MotionSample {
    MotionSample {
        time,
        difference: 0.02,
        scene_score: 0.0,
        visual: FrameVisualEvidence {
            foreground_coverage: coverage,
            foreground_center_x: Some(x),
            foreground_center_y: Some(0.5),
            ..FrameVisualEvidence::default()
        },
    }
}

/// The centroid of a difference mask jumps around a still subject from
/// frame to frame. Summing those jumps read 1.0 on nearly all real
/// footage; travel between half-second averages does not.
#[test]
fn subject_motion_is_travel_not_frame_to_frame_jitter() {
    // 40 pictures a second: 20 in each half-second window.
    let frames = |x: &dyn Fn(usize) -> f32| -> Vec<MotionSample> {
        (0..160)
            .map(|index| subject_sample(index as f64 / 40.0, 0.1, x(index)))
            .collect()
    };
    let motion_of = |samples: &[MotionSample]| {
        analyze_segment(0.0, 4.0, samples, &[], 50)
            .visual
            .expect("visual evidence")
            .subject_motion
    };
    // Jitter of 0.4 of the frame width every frame, going nowhere.
    let jitter = frames(&|index| if index % 2 == 0 { 0.3 } else { 0.7 });
    assert!(motion_of(&jitter) < 0.01, "{}", motion_of(&jitter));
    // The same jitter on a subject crossing 0.6 of the frame in 4 s:
    // each of the 8 window means is 0.075 past the last, 0.525 in 4 s.
    let crossing =
        frames(&|index| 0.2 + 0.6 * index as f32 / 160.0 + if index % 2 == 0 { -0.2 } else { 0.2 });
    assert!(
        (motion_of(&crossing) - 0.131).abs() < 0.005,
        "{}",
        motion_of(&crossing)
    );
    // Frames with almost no foreground say nothing about a subject.
    let faint: Vec<MotionSample> = (0..160)
        .map(|index| subject_sample(index as f64 / 40.0, 0.01, (index / 20) as f32 / 8.0))
        .collect();
    assert_eq!(motion_of(&faint), 0.0);
}

/// The first picture after a cut was compared with the previous shot,
/// so almost all of it differs. That is the cut, not a subject.
#[test]
fn the_cut_frame_is_not_the_new_shots_subject() {
    let mut samples: Vec<MotionSample> = (0..40)
        .map(|index| subject_sample(index as f64 * 0.1, 0.01, 0.2))
        .collect();
    samples[20] = subject_sample(2.0, 0.9, 0.5);
    let after_cut = analyze_segment(2.0, 4.0, &samples, &[], 50)
        .visual
        .expect("visual evidence");
    assert!((after_cut.subject_coverage - 0.01).abs() < 1e-6);
    assert!((after_cut.subject_center_x.unwrap() - 0.2).abs() < 1e-6);
    // A file's first picture has nothing before it and is kept.
    let first = analyze_segment(0.0, 0.05, &samples, &[], 50)
        .visual
        .expect("visual evidence");
    assert!((first.subject_coverage - 0.01).abs() < 1e-6);
    // A one-frame insert has no picture of its own to compare with.
    let insert = analyze_segment(2.0, 2.05, &samples, &[], 50)
        .visual
        .expect("a one-frame shot still has a picture");
    assert_eq!(insert.subject_coverage, 0.0);
    assert_eq!(insert.subject_center_x, None);
    assert_eq!(insert.subject_motion, 0.0);
}

/// A stride of 16 pixels on a 64-pixel row read columns 0, 16, 32 and
/// 48 of every row and nothing else.
#[test]
fn frame_difference_samples_every_column() {
    let (width, height) = (64_usize, 36_usize);
    let previous = vec![0_u8; width * height * 4];
    for column in [5_usize, 21, 47, 63] {
        let mut current = previous.clone();
        for row in 0..height {
            let index = (row * width + column) * 4;
            current[index..index + 3].fill(255);
        }
        let difference = frame_difference(&previous, &current);
        // One column in 64 changed fully: about 1/64 = 0.0156.
        assert!(
            (0.005..0.03).contains(&difference),
            "column {column}: {difference}"
        );
    }
}

#[test]
fn segmentation_handles_audio_only() {
    let segments = build_segments(9.0, &[], &[0.25; 180], 20);
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[1].end, 9.0);
}

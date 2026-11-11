use openreel_media::{MotionSample, SegmentVisualEvidence, SweepSegment};

use super::classify::{
    apply_prime, candidate_from_segment, classify_local, is_music_bed, local_candidates,
    motion_score, picture_end_note, segment_norms, Speech,
};
use super::transcript::parse_transcript_response;
use super::*;

fn centered_visual() -> SegmentVisualEvidence {
    SegmentVisualEvidence {
        subject_coverage: 0.25,
        subject_center_x: Some(0.5),
        subject_center_y: Some(0.45),
        subject_motion: 0.12,
        center_saliency: 0.8,
        edge_density: 0.3,
        luminance: 0.6,
        contrast: 0.2,
        skin_tone_ratio: 0.1,
    }
}

fn test_info() -> MediaInfo {
    MediaInfo {
        path: "/tmp/a.mp4".to_owned(),
        name: "a.mp4".to_owned(),
        duration: 12.0,
        width: 640,
        height: 360,
        fps: 30.0,
        has_video: true,
        has_audio: true,
    }
}

fn curate_generated_shots(count: usize, alternating: bool) -> (usize, usize, f64) {
    let path = std::env::temp_dir().join(format!(
        "eddie-curation-inventory-{}-{count}-{alternating}.mp4",
        std::process::id()
    ));
    let filter = if alternating {
        "geq=r='255*mod(N,2)':g='255*(1-mod(N,2))':b=0"
    } else {
        "null"
    };
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=red:s=64x36:r=4:d={}", count as f64 / 4.0),
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
        .expect("generate curation source");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let info = openreel_media::probe(path.to_str().unwrap()).expect("probe source");
    let result = run_sweep(
        &info,
        &[],
        &transcribe::TranscribeSettings::default(),
        None,
        &mut |_| {},
    );
    let _ = std::fs::remove_file(&path);
    let result = result.expect("local curation");
    for pair in result.scene_clips.windows(2) {
        assert_eq!(pair[0].end(), pair[1].source_in);
    }
    (
        result.candidates.len(),
        result.scene_clips.len(),
        result.scene_clips.last().expect("scene inventory").end(),
    )
}

#[test]
fn local_curation_keeps_every_shot_beyond_eight_and_five_hundred() {
    let (candidates, scenes, end) = curate_generated_shots(503, true);
    assert_eq!(scenes, 503, "scene inventory must not truncate at 500");
    assert_eq!(end, 125.75, "inventory must reach the source end");
    assert_eq!(
        candidates, 503,
        "local fallback must not keep only eight shots"
    );
}

#[test]
fn local_curation_preserves_a_long_take_without_arbitrary_eight_second_cuts() {
    let (candidates, scenes, end) = curate_generated_shots(260, false);
    assert_eq!(scenes, 1);
    assert_eq!(end, 65.0);
    assert_eq!(
        candidates, 1,
        "only semantic evidence should split this continuous take"
    );
}

#[test]
fn parses_diarized_turns_with_offset() {
    let value = serde_json::json!({
        "transcript": "Hello. Hi.",
        "audioDurationMs": 4000,
        "turns": [
            {"startMs": 500, "endMs": 1800, "transcript": "Hello.", "speaker": "A"},
            {"startMs": 2000, "endMs": 3000, "transcript": "Hi.", "speaker": "B"}
        ]
    });
    let turns = parse_transcript_response(&value, 10.0, 60.0);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].start, 10.5);
    assert_eq!(turns[1].speaker.as_deref(), Some("B"));
}

#[test]
fn shortened_audio_is_not_stretched_across_the_video_duration() {
    let info = MediaInfo {
        has_video: false,
        ..test_info()
    };
    let result = run_sweep(
        &info,
        &vec![0.5; 250],
        &transcribe::TranscribeSettings::default(),
        None,
        &mut |_| {},
    )
    .expect("local audio evidence");
    assert_eq!(result.scene_clips.len(), 1);
    let tail = openreel_media::analyze_segment(
        6.0,
        12.0,
        &[],
        &vec![0.5; 250],
        openreel_media::WAVEFORM_PEAKS_PER_SEC,
    );
    assert_eq!(tail.audio_energy, 0.0);
    assert_eq!(tail.audio_coverage, 0.0);
}

#[test]
fn parses_optional_word_timings_without_requiring_them() {
    let value = serde_json::json!({
        "turns": [{
            "startMs": 1000,
            "endMs": 4000,
            "transcript": "Cut the filler now.",
            "speaker": "A",
            "words": [
                {"word": "Cut", "startMs": 1100, "endMs": 1400, "confidence": 0.98},
                {"text": "the", "start": 1.45, "end": 1.65},
                {"word": "filler", "timestamp": [2.0, 2.6]},
                {"word": "now.", "startMs": 3000, "endMs": 3400}
            ]
        }, {
            "startMs": 5000,
            "endMs": 7000,
            "transcript": "Turn only timing.",
            "speaker": "B"
        }]
    });
    let turns = parse_transcript_response(&value, 10.0, 60.0);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].timing_precision(), "word");
    assert_eq!(turns[0].words.len(), 4);
    assert_eq!(turns[0].words[0].text, "Cut");
    assert!((turns[0].words[0].start - 11.1).abs() < 1e-9);
    assert!((turns[0].words[2].end - 12.6).abs() < 1e-9);
    assert_eq!(turns[1].timing_precision(), "turn");
    assert!(turns[1].words.is_empty());
}

#[test]
fn caption_chunks_use_word_times_when_present_and_turn_estimates_otherwise() {
    let word_segment = TranscriptSegment {
        start: 10.0,
        end: 14.0,
        speaker: None,
        text: "One two three four five six seven eight.".to_owned(),
        words: [
            ("One", 10.0, 10.3),
            ("two", 10.4, 10.7),
            ("three", 10.8, 11.2),
            ("four", 11.3, 11.7),
            ("five", 11.8, 12.1),
            ("six", 12.2, 12.5),
            ("seven", 12.6, 13.0),
            ("eight.", 13.1, 13.6),
        ]
        .into_iter()
        .map(|(text, start, end)| TranscriptWord {
            start,
            end,
            text: text.to_owned(),
            confidence: None,
            estimated: false,
        })
        .collect(),
        session: None,
    };
    let chunks = caption_chunks(&word_segment);
    assert!(
        chunks.len() > 1,
        "long speech is split into readable captions"
    );
    assert_eq!(chunks[0].start, 10.0);
    assert!(chunks
        .iter()
        .all(|chunk| { chunk.words.len() == chunk.text.split_whitespace().count() }));
    assert_eq!(chunks[0].words[..2], [10.0, 10.4]);
    assert!(chunks.iter().all(|chunk| chunk.text.len() <= 42));
    assert!(chunks.windows(2).all(|pair| pair[0].end <= pair[1].start));

    let turn_segment = TranscriptSegment {
        start: 20.0,
        end: 24.0,
        speaker: None,
        text: "A short turn.".to_owned(),
        words: Vec::new(),
        session: None,
    };
    assert_eq!(
        caption_chunks(&turn_segment),
        vec![CaptionChunk {
            start: 20.0,
            end: 24.0,
            text: "A short turn.".to_owned(),
            words: Vec::new(),
        }]
    );

    // Crowd ambience transcribed as one sparse turn is not speech.
    let ambient = TranscriptSegment {
        start: 2.86,
        end: 18.94,
        speaker: None,
        text: "LT".to_owned(),
        words: Vec::new(),
        session: None,
    };
    assert_eq!(caption_chunks(&ambient), Vec::new());
}

#[test]
fn captions_drop_stutters_and_break_at_sentences_not_mid_phrase() {
    let text = "The slow part was out of of Bothell, Washington. Short clips get the good \
                bits and and a person talking stays a person. After curation if it's if it's \
                not sure it says so. It's called Eddie Eddie's a video editor. It's still a \
                normal editor too so if it's a little off I can fix it by hand. What \
                you're about to see are my initial tests. Thanks for watching";
    let words: Vec<TranscriptWord> = text
        .split_whitespace()
        .enumerate()
        .map(|(index, word)| TranscriptWord {
            start: index as f64 * 0.3,
            end: index as f64 * 0.3 + 0.25,
            text: word.to_owned(),
            confidence: None,
            estimated: false,
        })
        .collect();
    let segment = TranscriptSegment {
        start: 0.0,
        end: words.last().unwrap().end,
        speaker: None,
        text: text.to_owned(),
        words,
        session: None,
    };
    let chunks = caption_chunks(&segment);
    let lines: Vec<&str> = chunks.iter().map(|chunk| chunk.text.as_str()).collect();
    let all = lines.join(" ");
    for stutter in ["of of", "and and", "if it's if it's"] {
        assert!(!all.contains(stutter), "{stutter:?} remains in {lines:?}");
    }
    assert!(all.contains("out of Bothell, Washington."), "{all}");
    assert!(
        all.contains("Eddie Eddie's"),
        "a stem is not a stutter: {all}"
    );
    assert!(
        lines.iter().any(|line| line.ends_with("stays a person.")),
        "a sentence end closes its caption: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("After curation")),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"What you're about to see are my initial tests."),
        "a sentence finishes on its own line: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| line.split(' ').count() > 1),
        "no one-word leftovers: {lines:?}"
    );
    for line in &lines {
        assert!(line.len() <= 50, "{line:?}");
        let last = line.rsplit(' ').next().unwrap();
        assert!(
            !["a", "the", "I", "to", "of", "it", "and", "by"].contains(&last),
            "no dangling word ends a caption: {lines:?}"
        );
    }
    assert!(chunks.windows(2).all(|pair| pair[0].end <= pair[1].start));
    // The first copy of a stutter keeps its start time.
    let bothell = chunks
        .iter()
        .find(|c| c.text.contains("out of Bothell"))
        .unwrap();
    assert!(bothell.start <= 4.0 * 0.3 + 1e-9, "{bothell:?}");

    // Turn-only text loses its stutters too.
    let turn = TranscriptSegment {
        start: 0.0,
        end: 3.0,
        speaker: None,
        text: "trying trying to find great moments".to_owned(),
        words: Vec::new(),
        session: None,
    };
    assert_eq!(
        caption_chunks(&turn)[0].text,
        "trying to find great moments"
    );
}

#[test]
fn local_classifier_categories_and_tags() {
    let base = SweepSegment {
        start: 0.0,
        end: 5.0,
        mean_motion: 0.01,
        peak_motion: 0.02,
        stability: 0.9,
        audio_energy: 0.5,
        audio_peak: 0.8,
        audio_coverage: 0.5,
        scene_change: 0.0,
        visual: None,
    };
    let (category, tags) = classify_local(&base, Speech::Heard, 0.1, 0.3);
    assert_eq!(category, "talking");
    assert!(tags.iter().any(|t| t == "speech"));
    assert!(tags.iter().any(|t| t == "steady"));
    let (category, _) = classify_local(&base, Speech::Silent, 0.1, 0.6);
    assert_eq!(category, "reaction", "loud short peak reads as reaction");
    let (category, _) = classify_local(&base, Speech::Silent, 0.9, 0.2);
    assert_eq!(category, "action");
    // Every shot but a file's first starts on a cut, so that alone is
    // b-roll with a tag, not an establishing shot.
    let opener = SweepSegment {
        scene_change: 0.3,
        audio_peak: 0.2,
        ..base
    };
    let (category, tags) = classify_local(&opener, Speech::Silent, 0.1, 0.2);
    assert_eq!(category, "broll");
    assert!(tags.iter().any(|t| t == "scene-open"));
    let wide = SweepSegment {
        visual: Some(SegmentVisualEvidence {
            subject_coverage: 0.01,
            subject_motion: 0.0,
            center_saliency: 0.2,
            ..centered_visual()
        }),
        ..opener.clone()
    };
    let (category, _) = classify_local(&wide, Speech::Silent, 0.1, 0.2);
    assert_eq!(category, "establishing");
}

/// A steady shot of something busy (0.05 measured on a phone game seen
/// over a shoulder) is b-roll; only a subject crossing the frame is
/// action. The old measure read 1.0 for both.
#[test]
fn only_a_travelling_subject_makes_a_quiet_shot_action() {
    let shot = |subject_motion: f32| SweepSegment {
        start: 0.0,
        end: 5.0,
        mean_motion: 0.016,
        peak_motion: 0.03,
        stability: 0.84,
        audio_energy: 0.1,
        audio_peak: 0.2,
        audio_coverage: 0.2,
        scene_change: 0.4,
        visual: Some(SegmentVisualEvidence {
            subject_motion,
            ..centered_visual()
        }),
    };
    let classify = |subject_motion: f32| {
        let segment = shot(subject_motion);
        classify_local(&segment, Speech::Silent, motion_score(&segment), 0.2)
    };
    let (category, tags) = classify(0.05);
    assert_eq!(category, "broll");
    assert!(!tags.iter().any(|t| t == "subject-moving"));
    assert!(!tags.iter().any(|t| t == "fast-motion"));
    let (category, tags) = classify(0.13);
    assert_eq!(category, "broll", "a pan moves the subject without action");
    assert!(tags.iter().any(|t| t == "subject-moving"));
    assert_eq!(classify(0.3).0, "action");
}

/// One seated 45 s shot used to score motion 1.0 and read `fast-motion`,
/// because motion was divided by the file's own maximum.
#[test]
fn motion_is_scored_on_one_scale_for_every_file() {
    let seated = SweepSegment {
        start: 0.0,
        end: 45.0,
        mean_motion: 0.0101,
        peak_motion: 0.03,
        stability: 0.899,
        audio_energy: 0.028,
        audio_peak: 0.11,
        audio_coverage: 0.3,
        scene_change: 0.0,
        visual: None,
    };
    let candidate = local_candidates(&test_info(), &[seated], &[], true)
        .pop()
        .unwrap();
    assert!((candidate.motion_score - 0.101).abs() < 0.001);
    assert!(candidate.tags.iter().any(|tag| tag == "slow"));
    assert!(!candidate.tags.iter().any(|tag| tag == "fast-motion"));
    assert_ne!(candidate.category, "action");
}

#[test]
fn visual_evidence_adds_subject_tags_without_claiming_a_face() {
    let info = test_info();
    let segment = SweepSegment {
        start: 0.0,
        end: 5.0,
        mean_motion: 0.01,
        peak_motion: 0.02,
        stability: 0.9,
        audio_energy: 0.3,
        audio_peak: 0.4,
        audio_coverage: 0.5,
        scene_change: 0.0,
        visual: Some(centered_visual()),
    };
    let (category, tags) = classify_local(&segment, Speech::Heard, 0.1, 0.3);
    assert_eq!(category, "talking");
    assert!(tags.iter().any(|tag| tag == "subject-centered"));
    assert!(tags.iter().any(|tag| tag == "subject-large"));
    assert!(tags.iter().any(|tag| tag == "subject-moving"));

    let norms = segment_norms(std::slice::from_ref(&segment));
    let candidate = candidate_from_segment(
        &info,
        0,
        &segment,
        &norms,
        80.0,
        "Centered speech.".to_owned(),
        vec![TranscriptSegment {
            start: 0.0,
            end: 4.0,
            speaker: Some("A".to_owned()),
            text: "Hello there.".to_owned(),
            words: Vec::new(),
            session: None,
        }],
        Speech::Heard,
    );
    assert!(candidate.talking_score() > 0.8);
    let visual = candidate.visual_json();
    assert_eq!(visual["subjectCoverage"], 0.25);
    assert!(!candidate.tags.iter().any(|tag| tag == "face-visible"));
}

#[test]
fn prime_is_scarce_and_score_gated() {
    let info = test_info();
    let segment = SweepSegment {
        start: 0.0,
        end: 5.0,
        mean_motion: 0.01,
        peak_motion: 0.02,
        stability: 0.9,
        audio_energy: 0.5,
        audio_peak: 0.8,
        audio_coverage: 0.5,
        scene_change: 0.0,
        visual: None,
    };
    let norms = segment_norms(std::slice::from_ref(&segment));
    let mut candidates: Vec<SweepCandidate> =
        [95.0, 90.0, 85.0, 80.0, 60.0, 55.0, 50.0, 45.0, 40.0, 35.0]
            .iter()
            .enumerate()
            .map(|(i, score)| {
                candidate_from_segment(
                    &info,
                    i,
                    &segment,
                    &norms,
                    *score,
                    "r".into(),
                    Vec::new(),
                    Speech::Silent,
                )
            })
            .collect();
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    apply_prime(&mut candidates);
    let primes: Vec<_> = candidates.iter().filter(|c| c.is_prime()).collect();
    assert_eq!(primes.len(), 2, "top 20 percent of 10 at or over 75");
    assert!(primes.iter().all(|c| c.score >= 90.0));
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.category == "reaction"),
        "quality does not replace clip type"
    );
}

#[test]
fn music_bed_blocks_audio_reaction_and_tags() {
    let bed = SweepSegment {
        start: 0.0,
        end: 5.0,
        mean_motion: 0.01,
        peak_motion: 0.02,
        stability: 0.9,
        audio_energy: 0.55,
        audio_peak: 0.95,
        audio_coverage: 0.97,
        scene_change: 0.0,
        visual: None,
    };
    assert!(is_music_bed(&bed, Speech::Silent));
    let (category, tags) = classify_local(&bed, Speech::Silent, 0.1, 0.9);
    assert_ne!(category, "reaction");
    assert!(tags.iter().any(|t| t == "music-bed"));
    assert!(!tags.iter().any(|t| t == "loud-peak"));
    let mut candidate = local_candidates(&test_info(), std::slice::from_ref(&bed), &[], true)
        .pop()
        .unwrap();
    candidate.reason = "No reaction here.".to_owned();
    assert!(!candidate.is_reaction());
    assert_eq!(candidate.reaction_score(), 0.0);

    // Loud continuous speech is not a music bed anywhere: not in the
    // tag, the flag, or the score, which keeps its loudness terms.
    let spoken = TranscriptSegment {
        start: 0.5,
        end: 4.5,
        speaker: None,
        text: "This booth is the busiest one here.".to_owned(),
        words: Vec::new(),
        session: None,
    };
    let bed_score = candidate.score;
    let talking = local_candidates(&test_info(), std::slice::from_ref(&bed), &[spoken], true)
        .pop()
        .unwrap();
    assert!(!talking.music_bed);
    assert!(!talking.tags.iter().any(|tag| tag == "music-bed"));
    assert!(talking.tags.iter().any(|tag| tag == "loud-peak"));
    // 0.42*0.9 + 0.28 + 0.15 + 0.15 = 0.958, plus 8 for speech, capped.
    assert_eq!(talking.score, 100.0);
    // 0.65*0.9 + 0.35 = 0.935.
    assert!((bed_score - 93.5).abs() < 0.01, "{bed_score}");
}

#[test]
fn a_picture_that_stops_early_is_noted() {
    let at = |times: &[f64]| -> Vec<MotionSample> {
        times
            .iter()
            .map(|time| MotionSample {
                time: *time,
                difference: 0.0,
                scene_score: 0.0,
                visual: Default::default(),
            })
            .collect()
    };
    // 30 pictures a second ending 0.2 s before the sound does.
    assert_eq!(picture_end_note(&at(&[9.7, 9.7333, 9.7667]), 10.0), None);
    // The same picture stream in a 75 s file.
    assert_eq!(
        picture_end_note(&at(&[9.7, 9.7333, 9.7667]), 75.0).as_deref(),
        Some("Picture ends at 0:10; nothing after it was analyzed.")
    );
    // A slideshow at one picture every 4 s ends on a 4 s picture.
    assert_eq!(picture_end_note(&at(&[0.0, 4.0, 8.0]), 12.0), None);
    assert!(picture_end_note(&at(&[0.0, 4.0, 8.0]), 30.0).is_some());
    assert_eq!(picture_end_note(&at(&[0.0]), 30.0), None);
}

/// Without a whisper model every curation's transcription fails. Loud
/// talking then looked like a music bed, was added muted, and
/// tightenSilence refused it.
#[test]
fn failed_transcription_is_unknown_not_a_music_bed() {
    let loud = SweepSegment {
        start: 0.0,
        end: 5.0,
        mean_motion: 0.01,
        peak_motion: 0.02,
        stability: 0.9,
        audio_energy: 0.55,
        audio_peak: 0.95,
        audio_coverage: 0.97,
        scene_change: 0.0,
        visual: None,
    };
    let candidate = local_candidates(&test_info(), std::slice::from_ref(&loud), &[], false)
        .pop()
        .unwrap();
    assert!(!candidate.music_bed);
    assert!(!candidate.tags.iter().any(|tag| tag == "music-bed"));
    assert!(!candidate.tags.iter().any(|tag| tag == "speech"));
    assert_ne!(candidate.category, "reaction");
    assert_ne!(candidate.category, "talking");

    // A curation saved before the flag existed transcribed successfully
    // or said so in its note; it loads as transcribed.
    let saved: SweepData = serde_json::from_value(serde_json::json!({
        "status": "Ready", "note": "", "transcript": [],
        "candidates": [], "scene_clips": [],
    }))
    .unwrap();
    assert!(saved.transcribed);
}

/// Whisper hears "Thank you." across 30 s of music. That is not speech,
/// so it must not turn a music montage into talking candidates.
#[test]
fn words_imagined_over_music_do_not_make_a_shot_talking() {
    let bed = SweepSegment {
        start: 4.0,
        end: 9.0,
        mean_motion: 0.01,
        peak_motion: 0.02,
        stability: 0.9,
        audio_energy: 0.55,
        audio_peak: 0.95,
        audio_coverage: 0.97,
        scene_change: 0.0,
        visual: None,
    };
    let imagined = TranscriptSegment {
        start: 0.0,
        end: 29.98,
        speaker: None,
        text: "Thank you.".to_owned(),
        words: Vec::new(),
        session: None,
    };
    let spoken = TranscriptSegment {
        start: 5.0,
        end: 8.0,
        text: "This booth is the busiest one here.".to_owned(),
        ..imagined.clone()
    };
    let candidate = local_candidates(
        &test_info(),
        std::slice::from_ref(&bed),
        std::slice::from_ref(&imagined),
        true,
    )
    .pop()
    .unwrap();
    assert_ne!(candidate.category, "talking");
    assert!(!candidate.tags.iter().any(|tag| tag == "speech"));
    assert!(candidate.tags.iter().any(|tag| tag == "music-bed"));
    assert!(candidate.music_bed, "the tag and the flag agree");
    assert!(candidate.transcript.is_empty());
    assert!(
        !candidate.reason.to_lowercase().contains("speech"),
        "{}",
        candidate.reason
    );

    let candidate = local_candidates(&test_info(), &[bed], &[imagined, spoken.clone()], true)
        .pop()
        .unwrap();
    assert_eq!(candidate.category, "talking");
    assert_eq!(candidate.transcript, vec![spoken]);
    assert!(!candidate.music_bed);
}

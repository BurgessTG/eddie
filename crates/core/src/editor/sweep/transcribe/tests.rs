use std::sync::Arc;

use whisper_rs::DtwModelPreset;

use super::super::transcript::parse_transcript_response;
use super::super::{gpu, speech, TranscriptSession, TranscriptWord, SAMPLE_RATE};
use super::cloud::{inside_chunk, parse_openai, upload_chunks};
use super::local::{dtw_preset, local_windows, quiet_cut, VAD_MODEL};
use super::models::{local_model, model_file, place, Placed};
use super::words::{turns_from_words, words_from_tokens, Sound, Speech, Token};
use super::*;

#[test]
fn openai_words_land_in_the_segment_holding_their_midpoint() {
    let value = serde_json::json!({
        "text": "Hello there. Second line.",
        "duration": 4.0,
        "segments": [
            {"start": 0.0, "end": 1.6, "text": " Hello there."},
            {"start": 1.6, "end": 4.0, "text": " Second line."}
        ],
        "words": [
            {"word": "Hello", "start": 0.1, "end": 0.5},
            {"word": "there.", "start": 0.6, "end": 1.5},
            // Straddles the boundary but mostly after it.
            {"word": "Second", "start": 1.5, "end": 2.1},
            {"word": "line.", "start": 2.2, "end": 2.8}
        ]
    });
    let segments = parse_openai(&value, 100.0, 4.0);
    assert_eq!(segments.len(), 2);
    assert_eq!((segments[0].start, segments[0].end), (100.0, 101.6));
    let texts =
        |i: usize| -> Vec<&str> { segments[i].words.iter().map(|w| w.text.as_str()).collect() };
    assert_eq!(texts(0), ["Hello", "there."]);
    assert_eq!(texts(1), ["Second", "line."]);
    assert_eq!(segments[1].words[0].start, 101.5);
    assert_eq!(segments[0].timing_precision(), "word");
}

fn turn(start: f64, end: f64, text: &str) -> TranscriptSegment {
    TranscriptSegment {
        start,
        end,
        speaker: None,
        text: text.to_owned(),
        words: Vec::new(),
        session: None,
    }
}

#[test]
fn turns_from_a_service_stay_inside_what_was_sent() {
    let word = |start: f64, end: f64| TranscriptWord {
        start,
        end,
        text: "w".to_owned(),
        confidence: None,
        estimated: false,
    };
    let mut worded = turn(405.0, 9_000.0, "runs far past the chunk");
    worded.words = vec![word(405.0, 405.0), word(406.0, 405.5), word(900.0, 901.0)];
    let turns = inside_chunk(
        vec![
            worded,
            turn(400.0, 400.0, "no times at all"),
            turn(410.0, 409.0, "ends before it starts"),
            turn(f64::NAN, 420.0, "not a number"),
            turn(390.0, 402.0, "starts before the chunk"),
        ],
        400.0,
        800.0,
    );
    let kept: Vec<(&str, f64, f64)> = turns
        .iter()
        .map(|turn| (turn.text.as_str(), turn.start, turn.end))
        .collect();
    assert_eq!(
        kept,
        [
            ("starts before the chunk", 400.0, 402.0),
            ("runs far past the chunk", 405.0, 800.0),
        ]
    );
    // A word without length is real; one running backwards is not.
    let words: Vec<(f64, f64)> = turns[1].words.iter().map(|w| (w.start, w.end)).collect();
    assert_eq!(words, [(405.0, 405.0), (800.0, 800.0)]);
}

#[test]
fn an_answer_with_text_alone_spans_what_was_sent() {
    let value = serde_json::json!({"text": " Only the words. "});
    let turns = parse_openai(&value, 400.0, 30.0);
    assert_eq!(turns.len(), 1);
    assert_eq!((turns[0].start, turns[0].end), (400.0, 430.0));
    assert_eq!(turns[0].text, "Only the words.");

    let value = serde_json::json!({"transcript": "Only the words."});
    let turns = parse_transcript_response(&value, 540.0, 30.0);
    assert_eq!(turns.len(), 1);
    assert_eq!((turns[0].start, turns[0].end), (540.0, 570.0));
}

#[test]
fn english_only_models_use_their_own_alignment_heads() {
    let preset = |name: &str| dtw_preset(Path::new(name));
    assert!(matches!(
        preset("ggml-base.en.bin"),
        Some(DtwModelPreset::BaseEn)
    ));
    assert!(matches!(
        preset("ggml-base.bin"),
        Some(DtwModelPreset::Base)
    ));
    assert!(matches!(
        preset("ggml-small.en-q5_1.bin"),
        Some(DtwModelPreset::SmallEn)
    ));
    assert!(matches!(
        preset("ggml-large-v3-turbo.bin"),
        Some(DtwModelPreset::LargeV3Turbo)
    ));
    assert!(preset("mystery.bin").is_none());
}

#[test]
fn installed_models_list_whisper_models_with_the_preferred_ones_first() {
    let dir = std::env::temp_dir().join(format!("eddie-models-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("ggml-folder.bin")).unwrap();
    for file in [
        "ggml-base.bin",
        "ggml-tiny.en.bin",
        "ggml-large-v3-turbo.bin",
        "ggml-medium.bin.part",
        "notes.txt",
        VAD_MODEL,
    ] {
        std::fs::write(dir.join(file), b"").unwrap();
    }
    let models = installed_models(Some(&dir));
    let best = best_installed(Some(&dir));
    let named = local_model(
        &TranscribeSettings {
            model: "tiny.en".into(),
            model_dir: Some(dir.clone()),
            ..TranscribeSettings::default()
        },
        gpu::BUILD,
    );
    std::fs::remove_dir_all(&dir).unwrap();

    // Turbo leads base in both the CPU and the GPU order.
    assert_eq!(models, ["large-v3-turbo", "base", "tiny.en"]);
    assert_eq!(best, Some("large-v3-turbo"));
    assert_eq!(named, Ok(dir.join("ggml-tiny.en.bin")));
    assert!(installed_models(None).is_empty());
    assert!(installed_models(Some(&dir)).is_empty(), "a missing folder");
    assert_eq!(best_installed(None), None);
}

/// whisper.cpp crashes the process when a model does not fit the GPU,
/// so the choice is made from the memory that is free.
#[test]
fn a_model_goes_on_the_gpu_only_when_it_fits() {
    let dir = std::env::temp_dir().join(format!("eddie-place-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sizes = [
        ("large-v3", 3_000),
        ("large-v3-turbo", 1_600),
        ("base", 150),
    ];
    for (name, bytes) in sizes {
        std::fs::write(model_file(&dir, name), vec![0; bytes]).unwrap();
    }
    let settings = |model: &str| TranscribeSettings {
        model: model.into(),
        model_dir: Some(dir.clone()),
        ..TranscribeSettings::default()
    };
    // Room for a model of `bytes` and nothing larger.
    let room = |bytes: u64| {
        Some(gpu::Memory {
            free: gpu::need(bytes),
            total: 8 << 30,
        })
    };
    let file = |name: &str| model_file(&dir, name);

    let no_gpu = place(&settings(""), None);
    let no_gpu_named = place(&settings("large-v3"), None);
    let roomy = place(&settings(""), room(3_000));
    let tight = place(&settings(""), room(1_600));
    let tight_named = place(&settings("large-v3"), room(1_600));
    let tight_turbo = place(&settings("large-v3-turbo"), room(1_600));
    let base_only = place(&settings(""), room(150));
    let unknown = place(&settings(""), Some(gpu::Memory { free: 0, total: 0 }));
    std::fs::remove_dir_all(&dir).unwrap();

    let on = |model: &str, on_gpu: bool| Placed {
        model: file(model),
        on_gpu,
        note: None,
    };
    assert_eq!(no_gpu, Ok(on("large-v3-turbo", false)), "the CPU's order");
    assert_eq!(no_gpu_named, Ok(on("large-v3", false)));
    assert_eq!(roomy, Ok(on("large-v3", true)));
    assert_eq!(tight_turbo, Ok(on("large-v3-turbo", true)));
    assert_eq!(unknown, Ok(on("large-v3", true)));

    let tight = tight.unwrap();
    assert_eq!((tight.model, tight.on_gpu), (file("large-v3-turbo"), true));
    let note = tight.note.unwrap();
    assert!(note.starts_with("ggml-large-v3.bin needs about "), "{note}");
    assert!(note.ends_with("; using ggml-large-v3-turbo.bin"), "{note}");

    let refused = tight_named.unwrap_err();
    assert!(
        refused.starts_with("ggml-large-v3.bin needs about "),
        "{refused}"
    );
    assert!(refused.ends_with("choose a smaller model"), "{refused}");

    // base would fit, but turbo on the CPU hears far more.
    let base_only = base_only.unwrap();
    assert_eq!(
        (base_only.model, base_only.on_gpu),
        (file("large-v3-turbo"), false)
    );
    let note = base_only.note.unwrap();
    assert!(
        note.ends_with("; using ggml-large-v3-turbo.bin on the CPU"),
        "{note}"
    );
}

/// 3 s of picture whose sound stops after the first second.
#[test]
fn chunks_without_sound_are_not_sent_and_stay_private() {
    let dir = std::env::temp_dir().join(format!("eddie-chunks-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("short-sound.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("color=c=blue:s=64x36:r=30:d=3")
        .args(["-f", "lavfi", "-i", "sine=f=440:d=1"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(made.success());
    let info = openreel_media::probe(path.to_str().unwrap()).unwrap();
    assert!(info.duration > 2.5, "{}", info.duration);

    let mut sent = Vec::new();
    let turns = upload_chunks(&info, 1.0, u64::MAX, &mut |_| {}, |wav, start, seconds| {
        let folder = wav.parent().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(folder).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "only this user reads a chunk");
        }
        sent.push((wav.to_path_buf(), start));
        Ok(vec![turn(start + 0.2, start + seconds + 5.0, "Heard.")])
    })
    .unwrap();
    assert_eq!(sent.len(), 1, "only the second with sound: {sent:?}");
    assert_eq!(sent[0].1, 0.0);
    assert_eq!((turns[0].start, turns[0].end), (0.2, 1.0));
    assert!(!sent[0].0.parent().unwrap().exists(), "chunks are removed");

    // A failed upload fails the transcription and still cleans up.
    let mut folder = None;
    let failed = upload_chunks(&info, 1.0, u64::MAX, &mut |_| {}, |wav, _, _| {
        folder = wav.parent().map(Path::to_path_buf);
        Err("Couldn't reach transcription.".to_owned())
    });
    assert_eq!(failed.unwrap_err(), "Couldn't reach transcription.");
    assert!(!folder.unwrap().exists());
    let _ = std::fs::remove_dir_all(&dir);
}

fn token(start: f64, end: f64, text: &str) -> Token {
    Token {
        text: text.to_owned(),
        start,
        end,
        probability: 0.9,
        opens_segment: false,
    }
}

/// Sound at 0.5 except in the `quiet` (start, end) seconds.
fn pcm_with_pauses(seconds: f64, quiet: &[(f64, f64)]) -> Vec<f32> {
    let sample = |at: f64| (at * SAMPLE_RATE as f64).round() as usize;
    let mut pcm = vec![0.5_f32; sample(seconds)];
    for (start, end) in quiet {
        pcm[sample(*start)..sample(*end)].fill(0.0);
    }
    pcm
}

fn spans(words: &[TranscriptWord]) -> Vec<(&str, f64, f64)> {
    let round = |at: f64| (at * 100.0).round() / 100.0;
    words
        .iter()
        .map(|w| (w.text.as_str(), round(w.start), round(w.end)))
        .collect()
}

/// talk.mp4 opens "Hi, my name is": whisper's DTW ends "Hi" at 0.28,
/// the comma at 0.92 and "my" at 1.08, and the sound stops from 0.36
/// to 1.00. Words used to run from one DTW time to the next, so "Hi,"
/// ran through the pause and "my" had no length.
#[test]
fn a_word_ends_where_its_sound_does_and_the_next_starts_after_the_pause() {
    let tokens = [
        token(0.0, 0.28, " Hi"),
        token(0.28, 0.92, ","),
        token(0.92, 1.08, " my"),
        token(1.08, 1.20, " na"),
        token(1.20, 1.38, "me"),
        token(1.38, 1.60, "."),
    ];
    let mut pcm = pcm_with_pauses(3.0, &[(0.36, 1.0), (1.5, 3.0)]);
    // A breath in the pause: over the silence level (0.02 here), never
    // loud. "my" used to start on it.
    pcm[SAMPLE_RATE * 82 / 100..SAMPLE_RATE * 84 / 100].fill(0.03);
    let sound = Sound::new(&pcm, &Speech::whole(pcm.len()));
    let words = words_from_tokens(&tokens, &sound, 10.0);
    assert_eq!(
        spans(&words),
        [
            ("Hi,", 10.0, 10.36),
            ("my", 11.0, 11.08),
            // No pause between "my" and "name": they touch.
            ("name.", 11.08, 11.5),
        ]
    );
    assert!((words[2].confidence.unwrap() - 0.9).abs() < 1e-6);
}

/// A stop consonant closes the mouth for a few hundredths of a second.
/// Without punctuation that is not a pause, and beside a real pause the
/// longer one is the pause.
#[test]
fn a_consonant_closure_is_not_a_pause() {
    let tokens = [
        token(0.0, 0.40, " that"),
        token(0.40, 0.90, " kept"),
        token(0.90, 1.30, "."),
        token(1.30, 1.80, " Then"),
    ];
    // 60 ms closures at 0.5 and 0.94, a pause from 1.06 to 1.4.
    let pcm = pcm_with_pauses(3.0, &[(0.5, 0.56), (0.94, 1.0), (1.06, 1.4)]);
    let sound = Sound::new(&pcm, &Speech::whole(pcm.len()));
    let words = words_from_tokens(&tokens, &sound, 0.0);
    assert_eq!(
        spans(&words),
        [("that", 0.0, 0.4), ("kept.", 0.4, 1.06), ("Then", 1.4, 1.8),]
    );
}

/// Only detected speech reaches whisper. Times it reports map back to
/// the file, and a word after removed music starts where speech resumes.
#[test]
fn times_heard_without_the_music_map_back_to_the_file() {
    let samples = SAMPLE_RATE * 30;
    // Speech at 1-4 s and 4.5-6 s (one stretch) and at 20-22 s.
    let speech = Speech::from_spans([(1.0, 4.0), (4.5, 6.0), (20.0, 22.0)].into_iter(), samples);
    let seconds = |(from, to): (usize, usize)| {
        (
            from as f64 / SAMPLE_RATE as f64,
            to as f64 / SAMPLE_RATE as f64,
        )
    };
    let kept: Vec<_> = speech.kept.iter().copied().map(seconds).collect();
    assert_eq!(kept, [(0.8, 6.2), (19.8, 22.2)]);
    let pcm = vec![0.5_f32; samples];
    // 5.4 s, 0.1 s of silence, 2.4 s.
    assert_eq!(speech.samples(&pcm).len(), SAMPLE_RATE * 79 / 10);
    let near = |heard: f64, original: f64| {
        let mapped = speech.original(heard);
        assert!((mapped - original).abs() < 1e-6, "{heard} -> {mapped}");
    };
    near(0.0, 0.8);
    near(3.0, 3.8);
    // Inside the inserted silence: the end of the speech before it.
    near(5.45, 6.2);
    near(5.5, 19.8);
    near(7.0, 21.3);
    near(60.0, 22.2);

    // "music." ends before the cut and "Next" is heard right after it.
    let tokens = [
        token(4.2, 4.9, " music"),
        token(4.9, 5.4, "."),
        token(5.4, 6.0, " Next"),
    ]
    .map(|token| Token {
        start: speech.original(token.start),
        end: speech.original(token.end),
        ..token
    });
    let words = words_from_tokens(&tokens, &Sound::new(&pcm, &speech), 0.0);
    assert_eq!(spans(&words), [("music.", 5.0, 6.2), ("Next", 19.8, 20.3)]);

    assert!(Speech::from_spans(std::iter::empty(), samples)
        .kept
        .is_empty());
}

/// whisper's own segments tile the file and split mid-sentence. Turns
/// are runs of words with no long pause, on their words' edges.
#[test]
fn turns_hold_speech_without_a_long_pause() {
    let word = |start: f64, end: f64, text: &str| TranscriptWord {
        start,
        end,
        text: text.to_owned(),
        confidence: Some(0.9),
        estimated: false,
    };
    let session = Arc::new(TranscriptSession {
        id: "local-test".to_owned(),
        start: 0.0,
        end: Some(30.0),
    });
    let turns = turns_from_words(
        vec![
            word(1.0, 1.4, "Hi,"),
            word(2.0, 2.3, "there."),
            word(2.6, 3.0, "Welcome"),
            word(3.0, 3.5, "back."),
            word(4.4, 4.8, "Next."),
        ],
        &session,
    );
    let seen: Vec<_> = turns
        .iter()
        .map(|turn| (turn.start, turn.end, turn.text.as_str(), turn.words.len()))
        .collect();
    assert_eq!(
        seen,
        [
            (1.0, 3.5, "Hi, there. Welcome back.", 4),
            (4.4, 4.8, "Next.", 1)
        ]
    );
    assert!(turns.iter().all(|turn| turn.session.is_some()));
    // The sentence end inside the first turn is a cut point with the
    // pause left out on both sides.
    let utterances: Vec<_> = speech::utterances(&turns)
        .into_iter()
        .map(|u| (u.start, u.end))
        .collect();
    assert_eq!(utterances, [(1.0, 2.3), (2.6, 3.5), (4.4, 4.8)]);
}

#[test]
fn a_window_ends_in_its_quietest_stretch() {
    let mut pcm = vec![0.5_f32; SAMPLE_RATE * 40];
    // A pause from 31.0 to 31.6 s.
    for sample in &mut pcm[SAMPLE_RATE * 31..SAMPLE_RATE * 31 + SAMPLE_RATE * 6 / 10] {
        *sample = 0.0;
    }
    let cut = quiet_cut(&pcm, 35.0, 10.0) as f64 / SAMPLE_RATE as f64;
    assert!((31.0..31.6).contains(&cut), "cut at {cut}");
}

/// The windows `local_windows` hears of `sound` seconds in a file
/// `duration` seconds long, as (start, seconds).
fn heard_windows(duration: f64, sound: f64) -> Vec<(f64, f64)> {
    let mut heard = Vec::new();
    let mut reads = 0;
    local_windows(
        duration,
        |start, length| {
            reads += 1;
            assert!(reads < 20, "the windows stopped advancing at {start}");
            let seconds = length.min(sound - start).max(0.0);
            Ok(vec![0.5; (seconds * SAMPLE_RATE as f64).round() as usize])
        },
        |start, pcm| {
            heard.push((start, pcm.len() as f64 / SAMPLE_RATE as f64));
            Ok(())
        },
    )
    .unwrap();
    heard
}

#[test]
fn windows_end_where_the_sound_does() {
    // A picture that outlasts its sound by a frame and a half used to
    // ask for the missing 54 ms forever.
    assert_eq!(heard_windows(45.066667, 45.013333).len(), 1);
    let (start, seconds) = heard_windows(45.066667, 45.013333)[0];
    assert!(start == 0.0 && (seconds - 45.013333).abs() < 0.001);

    // Sound that ends inside a later window is heard to its end, once.
    let windows = heard_windows(1500.0, 650.0);
    assert_eq!(windows.len(), 2, "{windows:?}");
    let (second, seconds) = windows[1];
    assert!(
        (windows[0].1 - second).abs() < 1e-6,
        "contiguous: {windows:?}"
    );
    assert!((570.0..=600.0).contains(&second), "cut inside the search");
    assert!((second + seconds - 650.0).abs() < 0.001, "{windows:?}");

    // Full-length sound still splits at the window size.
    let windows = heard_windows(700.0, 700.0);
    assert_eq!(windows.len(), 2, "{windows:?}");
    assert!((windows[1].0 + windows[1].1 - 700.0).abs() < 0.001);

    assert!(heard_windows(30.0, 0.0).is_empty(), "no sound at all");
    assert!(heard_windows(3000.0, 0.0).is_empty(), "nor in a long file");
}

#[test]
fn sound_that_starts_after_the_first_window_is_still_heard() {
    // 200 s of sound from 700 s in a 1500 s file. A window that holds
    // sound comes back with its silent lead, as `pcm_mono_16k` reads it.
    let (from, to) = (700.0, 900.0);
    let mut heard = Vec::new();
    local_windows(
        1500.0,
        |start, length| {
            let seconds = if start + length <= from || start >= to {
                0.0
            } else {
                length.min(to - start)
            };
            Ok(vec![0.5; (seconds * SAMPLE_RATE as f64).round() as usize])
        },
        |start, pcm| {
            heard.push((start, pcm.len() as f64 / SAMPLE_RATE as f64));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(heard.len(), 1, "{heard:?}");
    let (start, seconds) = heard[0];
    assert!(
        start <= from && (start + seconds - to).abs() < 0.001,
        "{heard:?}"
    );
}

#[test]
fn settings_round_trip_and_default_to_local() {
    assert_eq!(
        TranscribeSettings::from_json(&serde_json::Value::Null).provider,
        Provider::Local
    );
    let settings = TranscribeSettings {
        provider: Provider::OpenAi,
        model: "whisper-large-v3".to_owned(),
        language: "en".to_owned(),
        vocabulary: "Eddie, Muse Wonder".to_owned(),
        base_url: "https://api.groq.com/openai/v1".to_owned(),
        api_key: "k".to_owned(),
        model_dir: None,
    };
    assert_eq!(TranscribeSettings::from_json(&settings.to_json()), settings);
}

//! Local transcription with whisper.cpp, in ten-minute windows.

use std::path::Path;
use std::sync::Arc;

use openreel_media::MediaInfo;
use whisper_rs::{
    DtwMode, DtwModelPreset, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters, WhisperState, WhisperTokenId, WhisperVadContext,
    WhisperVadContextParams, WhisperVadParams,
};

use super::super::{gpu, TranscriptSegment, TranscriptSession, SAMPLE_RATE};
use super::models::{local_model, model_name, place, Placed};
use super::words::{turns_from_words, words_from_tokens, Sound, Speech, Token};
use super::TranscribeSettings;

/// Local audio is decoded and heard this much at a time, so an hour-long
/// file never sits in memory whole. Each window ends in its quietest half
/// second near this length, so a word is not split between windows.
const LOCAL_WINDOW_SECONDS: f64 = 600.0;

const LOCAL_CUT_SEARCH_SECONDS: f64 = 30.0;

/// Silero voice detection (`ggml-silero-v5.1.2.bin`), beside the models.
pub(super) const VAD_MODEL: &str = "ggml-silero-v5.1.2.bin";

/// One local model at a time, transcribing or aligning: each holds up to
/// 3 GB and already uses every core (or the GPU), so parallel curations
/// wait their turn here.
pub(super) fn one_model_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(super) fn dtw_preset(model: &Path) -> Option<DtwModelPreset> {
    let name = model.file_name()?.to_string_lossy().to_lowercase();
    // English-only models have their own alignment heads; the multilingual
    // preset of the same size gives wrong word onsets without an error.
    let english = name.contains(".en.") || name.contains(".en-");
    Some(if name.contains("large-v3-turbo") {
        DtwModelPreset::LargeV3Turbo
    } else if name.contains("large-v3") {
        DtwModelPreset::LargeV3
    } else if name.contains("large-v2") {
        DtwModelPreset::LargeV2
    } else if name.contains("medium") {
        if english {
            DtwModelPreset::MediumEn
        } else {
            DtwModelPreset::Medium
        }
    } else if name.contains("small") {
        if english {
            DtwModelPreset::SmallEn
        } else {
            DtwModelPreset::Small
        }
    } else if name.contains("base") {
        if english {
            DtwModelPreset::BaseEn
        } else {
            DtwModelPreset::Base
        }
    } else if name.contains("tiny") {
        if english {
            DtwModelPreset::TinyEn
        } else {
            DtwModelPreset::Tiny
        }
    } else {
        return None;
    })
}

pub(super) fn local(
    settings: &TranscribeSettings,
    info: &MediaInfo,
    progress: &mut dyn FnMut(&str),
) -> Result<(Vec<TranscriptSegment>, String, Option<String>), String> {
    // A model that is not installed fails before waiting for a turn.
    local_model(settings, gpu::BUILD)?;
    let _turn = one_model_at_a_time();
    whisper_rs::install_logging_hooks();
    // Read once the turn is ours: the model before us has left the GPU.
    let Placed {
        model,
        on_gpu,
        note,
    } = place(settings, gpu::memory())?;
    let model_name = model_name(&model);
    let detector = model.with_file_name(VAD_MODEL);
    progress(&format!("Loading {model_name}…"));
    // A detector that will not load is the same as none installed.
    let mut detector = detector.is_file().then_some(detector).and_then(|path| {
        WhisperVadContext::new(&path.to_string_lossy(), WhisperVadContextParams::default()).ok()
    });
    let mut context_params = WhisperContextParameters {
        use_gpu: on_gpu,
        ..Default::default()
    };
    if let Some(model_preset) = dtw_preset(&model) {
        context_params.dtw_parameters.mode = DtwMode::ModelPreset { model_preset };
    }
    let context = WhisperContext::new_with_params(&model, context_params)
        .map_err(|error| format!("{model_name} could not load: {error}"))?;
    let mut state = context
        .create_state()
        .map_err(|error| format!("{model_name} could not start: {error}"))?;
    let eot = context.token_eot();
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(8)) as i32;
    let session = Arc::new(TranscriptSession {
        id: format!("local-{model_name}"),
        start: 0.0,
        end: Some(info.duration),
    });

    let mut segments = Vec::new();
    let decode = |start: f64, length: f64| {
        openreel_media::pcm_mono_16k(&info.path, start, length)
            .map_err(|error| format!("audio could not be decoded: {error}"))
    };
    local_windows(info.duration, decode, |start, pcm| {
        let heard = pcm.len() as f64 / SAMPLE_RATE as f64;
        progress(&format!(
            "Transcribing {}–{} of {}…",
            clock(start),
            clock(start + heard),
            clock(info.duration)
        ));
        let speech = match &mut detector {
            Some(detector) => {
                let spans = detector
                    .segments_from_samples(WhisperVadParams::default(), pcm)
                    .map_err(|error| format!("voice detection failed: {error}"))?;
                Speech::from_spans(
                    spans.map(|span| (span.start as f64 / 100.0, span.end as f64 / 100.0)),
                    pcm.len(),
                )
            }
            None => Speech::whole(pcm.len()),
        };
        if speech.kept.is_empty() {
            return Ok(());
        }
        // whisper.cpp rejects input shorter than one second.
        let mut padded = speech.samples(pcm);
        padded.resize(padded.len().max(SAMPLE_RATE * 11 / 10), 0.0);
        state
            .full(decoding(settings, threads), &padded)
            .map_err(|error| format!("{model_name} failed: {error}"))?;
        let tokens = heard_tokens(&state, &speech, eot);
        let words = words_from_tokens(&tokens, &Sound::new(pcm, &speech), start);
        segments.extend(turns_from_words(words, &session));
        Ok(())
    })?;
    let detector = if detector.is_some() {
        ""
    } else {
        ", no voice detector"
    };
    Ok((segments, format!("local:{model_name}{detector}"), note))
}

/// How every window is decoded: beam search of five, token times, and no
/// text carried over from the window before.
fn decoding(settings: &TranscribeSettings, threads: i32) -> FullParams<'_, '_> {
    let mut params = FullParams::new(SamplingStrategy::BeamSearch {
        beam_size: 5,
        patience: -1.0,
    });
    params.set_language(Some(if settings.language.is_empty() {
        "auto"
    } else {
        settings.language.as_str()
    }));
    params.set_token_timestamps(true);
    // Conditioning on earlier text lets one mistake repeat for minutes.
    params.set_no_context(true);
    params.set_suppress_nst(true);
    params.set_n_threads(threads);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    params.set_print_timestamps(false);
    if !settings.vocabulary.is_empty() {
        params.set_initial_prompt(&settings.vocabulary);
    }
    params
}

/// The text tokens whisper just heard, timed on the window's own clock
/// (`speech` maps whisper's times back across the silence it never heard).
fn heard_tokens(state: &WhisperState, speech: &Speech, eot: WhisperTokenId) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    for segment in state.as_iter() {
        let opens = speech.original(segment.start_timestamp() as f64 / 100.0);
        let mut opens_segment = true;
        for index in 0..segment.n_tokens() {
            let Some(token) = segment.get_token(index) else {
                continue;
            };
            if token.token_id() >= eot {
                continue;
            }
            let Ok(piece) = token.to_str_lossy() else {
                continue;
            };
            let data = token.token_data();
            let before = tokens.last().map(|token| token.end);
            // whisper.cpp stamps a token with the time its DTW path
            // leaves it, so `t_dtw` is where the token ends and the
            // token before it says where it starts. Without DTW the
            // token carries whisper's own looser guess of both.
            let (token_start, token_end) = if data.t_dtw >= 0 {
                (
                    before.unwrap_or(opens),
                    speech.original(data.t_dtw as f64 / 100.0),
                )
            } else {
                (
                    speech.original(data.t0 as f64 / 100.0),
                    speech.original(data.t1 as f64 / 100.0),
                )
            };
            let token_start = token_start.max(before.unwrap_or(0.0));
            tokens.push(Token {
                text: piece.into_owned(),
                start: token_start,
                end: token_end.max(token_start),
                probability: token.token_probability(),
                opens_segment: std::mem::take(&mut opens_segment),
            });
        }
    }
    tokens
}

/// Walk a file's sound in windows of about ten minutes, each ending in a
/// quiet half second. `decode(start, seconds)` returns 16 kHz samples whose
/// first sample is at `start` (silence fills any lead) and `hear(start,
/// samples)` takes each window. The sound can end before the file does (a
/// picture that runs a frame longer, or a silent tail), so a read can come
/// back short. A read with nothing in it moves on a whole window: sound
/// that starts later is still heard, and the walk always ends.
pub(super) fn local_windows(
    duration: f64,
    mut decode: impl FnMut(f64, f64) -> Result<Vec<f32>, String>,
    mut hear: impl FnMut(f64, &[f32]) -> Result<(), String>,
) -> Result<(), String> {
    let least = SAMPLE_RATE / 20;
    let mut start = 0.0;
    while start < duration - 0.05 {
        let length = (LOCAL_WINDOW_SECONDS + LOCAL_CUT_SEARCH_SECONDS).min(duration - start);
        let mut pcm = decode(start, length)?;
        if pcm.len() < least {
            start += length;
            continue;
        }
        let asked = (length * SAMPLE_RATE as f64) as usize;
        let last = start + length >= duration || pcm.len() + least < asked;
        if !last {
            let keep = quiet_cut(&pcm, LOCAL_WINDOW_SECONDS, LOCAL_CUT_SEARCH_SECONDS);
            pcm.truncate(keep);
        }
        hear(start, &pcm)?;
        start += pcm.len() as f64 / SAMPLE_RATE as f64;
    }
    Ok(())
}

/// Sample index of the quietest half second within `search` seconds
/// before `target`, so a window never ends inside a word.
pub(super) fn quiet_cut(pcm: &[f32], target: f64, search: f64) -> usize {
    let frame = SAMPLE_RATE / 2;
    let end = ((target * SAMPLE_RATE as f64) as usize).min(pcm.len());
    let from = end.saturating_sub((search * SAMPLE_RATE as f64) as usize);
    let mut best = (f32::MAX, end);
    let mut at = from;
    while at + frame <= end {
        let energy: f32 = pcm[at..at + frame].iter().map(|s| s * s).sum();
        if energy < best.0 {
            best = (energy, at + frame / 2);
        }
        at += SAMPLE_RATE / 10;
    }
    best.1.max(SAMPLE_RATE)
}

pub(super) fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

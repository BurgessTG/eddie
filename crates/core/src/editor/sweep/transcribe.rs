//! Transcription providers: local whisper.cpp by default, or a cloud API.
//!
//! Local runs anywhere without a key. It uses the most accurate model the
//! build can afford (large-v3 with a GPU, large-v3-turbo on the CPU), the
//! Silero voice detector when its model is present so music and room tone
//! are not "heard" as words, beam search, and DTW word timing. Cloud
//! providers are Meta's Voice Transcribe (turns with speakers; words come
//! from local alignment) and any OpenAI-style `/audio/transcriptions`
//! endpoint (OpenAI, Groq and similar) with word timestamps.
//!
//! Finished transcripts are saved per file beside saved curations, so a file
//! is transcribed once unless asked again.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openreel_media::MediaInfo;
use whisper_rs::{
    DtwMode, DtwModelPreset, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters, WhisperVadParams,
};

use super::{align, store, TranscriptSegment, TranscriptSession, TranscriptWord};

const SAMPLE_RATE: usize = 16_000;
/// Local audio is decoded and heard this much at a time, so an hour-long
/// file never sits in memory whole. Each window ends in its quietest half
/// second near this length, so a word is not split between windows.
const LOCAL_WINDOW_SECONDS: f64 = 600.0;
const LOCAL_CUT_SEARCH_SECONDS: f64 = 30.0;
/// Silero voice detection (`ggml-silero-v5.1.2.bin`), beside the models.
const VAD_MODEL: &str = "ggml-silero-v5.1.2.bin";
/// Whether this build runs whisper on a GPU (`cuda` or `vulkan` feature).
const GPU: bool = cfg!(any(feature = "cuda", feature = "vulkan"));

const META_MODEL: &str = "muse-voice-transcribe-1.0";
const META_BASE_URL: &str = "https://api.meta.ai/v1";
// Voice Transcribe accepts WAV mono 16-bit PCM at 16/24 kHz, at most
// 10 minutes / 32 MB per request (see `openreel_media::write_wav_24k`).
const META_CHUNK_SECONDS: f64 = 540.0;
const META_MAX_BYTES: u64 = 32 * 1024 * 1024;

const OPENAI_MODEL: &str = "whisper-1";
const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
/// OpenAI-style endpoints take at most 25 MB; 400 s of 24 kHz WAV is 19 MB.
const OPENAI_CHUNK_SECONDS: f64 = 400.0;
const OPENAI_MAX_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Provider {
    #[default]
    Local,
    Meta,
    OpenAi,
}

impl Provider {
    pub const ALL: [Self; 3] = [Self::Local, Self::Meta, Self::OpenAi];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Meta => "meta",
            Self::OpenAi => "openai",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|provider| provider.as_str() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Local (whisper)",
            Self::Meta => "Meta Voice Transcribe",
            Self::OpenAi => "OpenAI-compatible",
        }
    }
}

/// Saved in the settings file under `transcription`. Empty strings mean the
/// provider's default; a Meta provider without its own key uses the main one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscribeSettings {
    pub provider: Provider,
    /// Local: a model name such as `large-v3` or a path to a ggml file.
    /// Cloud: the provider's model ID.
    pub model: String,
    /// An ISO code such as `en`; empty detects the language.
    pub language: String,
    /// Names and terms to spell right, such as "Eddie, Muse Wonder". Sent
    /// as whisper's prompt; a short list, never a script, since a long
    /// prompt makes whisper repeat it instead of listening.
    pub vocabulary: String,
    pub base_url: String,
    pub api_key: String,
    /// Where installed local models are looked up; never saved. `None`
    /// leaves only `EDDIE_WHISPER_DIR` and `EDDIE_WHISPER_MODEL`, which
    /// keeps tests off whatever the user has installed.
    pub model_dir: Option<PathBuf>,
}

impl TranscribeSettings {
    pub fn from_json(value: &serde_json::Value) -> Self {
        let text = |key: &str| value[key].as_str().unwrap_or_default().trim().to_owned();
        Self {
            provider: Provider::from_name(&text("provider")).unwrap_or_default(),
            model: text("model"),
            language: text("language"),
            vocabulary: text("vocabulary"),
            base_url: text("base_url"),
            api_key: text("api_key"),
            model_dir: None,
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "provider": self.provider.as_str(),
            "model": self.model,
            "language": self.language,
            "vocabulary": self.vocabulary,
            "base_url": self.base_url,
            "api_key": self.api_key,
        })
    }
}

/// Keyless endpoints must be parsed localhost or loopback addresses, not
/// hostname-prefix matches.
pub fn is_local_endpoint(base_url: &str) -> bool {
    let Ok(uri) = base_url.trim().parse::<ureq::http::Uri>() else {
        return false;
    };
    let Some(host) = uri.host() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

fn api_error_for_status(status: u16) -> String {
    match status {
        401 | 403 => "Check the speech key in Settings.".to_owned(),
        404 => "That speech model or URL was not found. Check Settings.".to_owned(),
        429 => "The transcription service is busy. Try again shortly.".to_owned(),
        500..=599 => "The transcription service had a problem. Try again shortly.".to_owned(),
        _ => "The transcription request was rejected. Check Settings and try again.".to_owned(),
    }
}

/// A finished transcript and how it was made.
pub struct Transcript {
    pub segments: Vec<TranscriptSegment>,
    /// Provider and model, such as `local:ggml-large-v3.bin`.
    pub source: String,
    /// One short line for curation notes and the command line.
    pub note: String,
    pub cached: bool,
}

/// The saved transcript of this exact file, else a new one from the
/// provider, saved for next time. `force` skips the saved one.
pub fn transcribe_cached(
    settings: &TranscribeSettings,
    store_dir: Option<&Path>,
    info: &MediaInfo,
    force: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<Transcript, String> {
    if let Some((segments, source)) = store_dir
        .filter(|_| !force)
        .and_then(|dir| store::load_transcript(dir, &info.path))
    {
        return Ok(Transcript {
            note: format!("Transcription: {} turns, saved ({source}).", segments.len()),
            segments,
            source,
            cached: true,
        });
    }
    let transcript = transcribe(settings, info, progress)?;
    if let Some(dir) = store_dir {
        if let Err(error) =
            store::save_transcript(dir, &info.path, &transcript.segments, &transcript.source)
        {
            eprintln!("transcript for {} was not saved: {error}", info.name);
        }
    }
    Ok(transcript)
}

pub fn transcribe(
    settings: &TranscribeSettings,
    info: &MediaInfo,
    progress: &mut dyn FnMut(&str),
) -> Result<Transcript, String> {
    if !info.has_audio || info.duration <= 0.0 {
        return Ok(Transcript {
            segments: Vec::new(),
            source: "none".to_owned(),
            note: "Transcription: no audio stream.".to_owned(),
            cached: false,
        });
    }
    let (mut segments, source) = match settings.provider {
        Provider::Local => local(settings, info, progress)?,
        Provider::Meta => meta(settings, info, progress)?,
        Provider::OpenAi => openai(settings, info, progress)?,
    };
    let mut note = format!("Transcription: {} turns ({source}).", segments.len());
    // Turn-level providers gain word timing from local alignment.
    if segments.iter().any(|segment| segment.words.is_empty()) {
        let model = align::model_path(settings.model_dir.as_deref());
        if model.is_some() {
            progress("Aligning words…");
        }
        match align::align_transcript(info, model.as_deref(), &mut segments) {
            Ok((_, 0)) => {}
            Ok((aligned, candidates)) => note.push_str(&format!(
                " Word timing: {aligned}/{candidates} multi-sentence turns aligned locally."
            )),
            Err(error) => note.push_str(&format!(" Word timing: {error}.")),
        }
    }
    Ok(Transcript {
        segments,
        source,
        note,
        cached: false,
    })
}

// -- local -------------------------------------------------------------------

/// Where the user's local models are installed: `whisper/` beside the
/// settings file.
pub fn installed_model_dir() -> Option<PathBuf> {
    Some(
        crate::editor::settings::config_path()?
            .parent()?
            .join("whisper"),
    )
}

/// Where local models are looked up: `EDDIE_WHISPER_DIR`, else `installed`.
fn model_dir(installed: Option<&Path>) -> Option<PathBuf> {
    std::env::var_os("EDDIE_WHISPER_DIR")
        .map(PathBuf::from)
        .or_else(|| installed.map(Path::to_path_buf))
}

/// Most accurate first, for what this build can run in reasonable time:
/// on the CPU, large-v3 takes several times longer than turbo for a small
/// accuracy gain, so turbo leads there.
fn preferred_models() -> &'static [&'static str] {
    if GPU {
        &[
            "large-v3",
            "large-v3-turbo",
            "large-v3-turbo-q5_0",
            "medium",
            "small",
            "base",
        ]
    } else {
        &[
            "large-v3-turbo",
            "large-v3-turbo-q5_0",
            "large-v3",
            "medium",
            "small",
            "base",
        ]
    }
}

/// The model file to use: a configured name or path, else the best installed.
pub fn local_model(settings: &TranscribeSettings) -> Result<PathBuf, String> {
    let requested = settings.model.as_str();
    let dir = model_dir(settings.model_dir.as_deref());
    let file = |name: &str| dir.as_ref().map(|dir| dir.join(format!("ggml-{name}.bin")));
    if !requested.is_empty() {
        let path = PathBuf::from(requested);
        if path.is_file() {
            return Ok(path);
        }
        return file(requested)
            .filter(|path| path.is_file())
            .ok_or_else(|| missing_model(dir.as_deref(), requested));
    }
    preferred_models()
        .iter()
        .find_map(|name| file(name).filter(|path| path.is_file()))
        .ok_or_else(|| missing_model(dir.as_deref(), preferred_models()[0]))
}

fn missing_model(dir: Option<&Path>, name: &str) -> String {
    format!(
        "no whisper model ggml-{name}.bin in {}; download it from \
         https://huggingface.co/ggerganov/whisper.cpp",
        dir.map_or("the whisper folder".into(), |dir| dir.display().to_string())
    )
}

fn dtw_preset(model: &Path) -> Option<DtwModelPreset> {
    let name = model.file_name()?.to_string_lossy().to_lowercase();
    Some(if name.contains("large-v3-turbo") {
        DtwModelPreset::LargeV3Turbo
    } else if name.contains("large-v3") {
        DtwModelPreset::LargeV3
    } else if name.contains("large-v2") {
        DtwModelPreset::LargeV2
    } else if name.contains("medium") {
        DtwModelPreset::Medium
    } else if name.contains("small") {
        DtwModelPreset::Small
    } else if name.contains("base") {
        DtwModelPreset::Base
    } else if name.contains("tiny") {
        DtwModelPreset::Tiny
    } else {
        return None;
    })
}

fn local(
    settings: &TranscribeSettings,
    info: &MediaInfo,
    progress: &mut dyn FnMut(&str),
) -> Result<(Vec<TranscriptSegment>, String), String> {
    let model = local_model(settings)?;
    // One local model at a time: each holds 1.6-3 GB and already uses every
    // core (or the GPU), so parallel curations wait their turn here.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let model_name = model
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let vad = model.with_file_name(VAD_MODEL);
    let vad = vad.is_file().then(|| vad.to_string_lossy().into_owned());
    progress(&format!("Loading {model_name}…"));
    whisper_rs::install_logging_hooks();
    let mut context_params = WhisperContextParameters::default();
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
    let mut start = 0.0;
    while start < info.duration - 0.05 {
        let length = (LOCAL_WINDOW_SECONDS + LOCAL_CUT_SEARCH_SECONDS).min(info.duration - start);
        let mut pcm = openreel_media::pcm_mono_16k(&info.path, start, length)
            .map_err(|error| format!("audio could not be decoded: {error}"))?;
        let keep = if start + length < info.duration {
            quiet_cut(&pcm, LOCAL_WINDOW_SECONDS, LOCAL_CUT_SEARCH_SECONDS)
        } else {
            pcm.len()
        };
        pcm.truncate(keep);
        let heard = keep as f64 / SAMPLE_RATE as f64;
        progress(&format!(
            "Transcribing {}–{} of {}…",
            clock(start),
            clock(start + heard),
            clock(info.duration)
        ));
        // whisper.cpp rejects input shorter than one second.
        let mut padded = pcm.clone();
        padded.resize(padded.len().max(SAMPLE_RATE * 11 / 10), 0.0);
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
        if let Some(vad) = &vad {
            params.set_vad_model_path(Some(vad));
            params.set_vad_params(WhisperVadParams::default());
            params.enable_vad(true);
        }
        state
            .full(params, &padded)
            .map_err(|error| format!("{model_name} failed: {error}"))?;
        for segment in state.as_iter() {
            let Ok(text) = segment.to_str_lossy() else {
                continue;
            };
            let text = text.trim().to_owned();
            if text.is_empty() {
                continue;
            }
            let mut pieces: Vec<(f64, f64, String, f32)> = Vec::new();
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
                let t0 = data.t0 as f64 / 100.0;
                let t1 = data.t1 as f64 / 100.0;
                // DTW times a token's onset more tightly than t0.
                let onset = if data.t_dtw >= 0 {
                    data.t_dtw as f64 / 100.0
                } else {
                    t0
                };
                pieces.push((
                    onset,
                    t1.max(onset),
                    piece.into_owned(),
                    token.token_probability(),
                ));
            }
            let mut words = words_from_tokens(&pieces, start);
            let seg_start = start + segment.start_timestamp() as f64 / 100.0;
            let seg_end = start + segment.end_timestamp() as f64 / 100.0;
            for word in &mut words {
                word.start = word.start.clamp(seg_start, seg_end);
                word.end = word.end.clamp(word.start, seg_end);
            }
            align::snap_breaks(&mut words, &pcm, start);
            segments.push(TranscriptSegment {
                start: seg_start,
                end: seg_end.max(seg_start),
                speaker: None,
                text,
                words,
                session: Some(Arc::clone(&session)),
            });
        }
        start += heard;
    }
    let detector = if vad.is_some() {
        ""
    } else {
        ", no voice detector"
    };
    Ok((segments, format!("local:{model_name}{detector}")))
}

/// Join whisper tokens into words: a token starting with a space starts a
/// new word. Each word starts at its first token's onset and ends where
/// the next word starts or its last token ends, whichever is first.
fn words_from_tokens(pieces: &[(f64, f64, String, f32)], offset: f64) -> Vec<TranscriptWord> {
    let mut words: Vec<(f64, f64, String, Vec<f32>)> = Vec::new();
    for (onset, end, piece, probability) in pieces {
        match words.last_mut() {
            Some(word) if !piece.starts_with(' ') => {
                word.1 = word.1.max(*end);
                word.2.push_str(piece);
                word.3.push(*probability);
            }
            _ => words.push((*onset, *end, piece.clone(), vec![*probability])),
        }
    }
    let starts: Vec<f64> = words.iter().map(|word| word.0).collect();
    words
        .into_iter()
        .enumerate()
        .filter_map(|(index, (start, end, text, probabilities))| {
            let text = text.trim().to_owned();
            if text.is_empty() || text.chars().all(|c| !c.is_alphanumeric()) {
                return None;
            }
            let end = starts.get(index + 1).map_or(end, |next| end.min(*next));
            Some(TranscriptWord {
                start: offset + start,
                end: offset + end.max(start),
                text,
                confidence: Some(
                    probabilities.iter().sum::<f32>() / probabilities.len().max(1) as f32,
                ),
                estimated: false,
            })
        })
        .collect()
}

/// Sample index of the quietest half second within `search` seconds
/// before `target`, so a window never ends inside a word.
fn quiet_cut(pcm: &[f32], target: f64, search: f64) -> usize {
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

fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

// -- cloud -------------------------------------------------------------------

/// `base` with any trailing slash or chat path removed, then `path`.
fn endpoint(base: &str, path: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    let base = base.strip_suffix("/chat/completions").unwrap_or(base);
    format!("{base}/{path}")
}

fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(300)))
            .http_status_as_error(false)
            .build(),
    )
}

/// Upload each chunk of 24 kHz WAV through `send`, in order.
fn upload_chunks(
    info: &MediaInfo,
    chunk_seconds: f64,
    max_bytes: u64,
    progress: &mut dyn FnMut(&str),
    mut send: impl FnMut(&Path, f64) -> Result<Vec<TranscriptSegment>, String>,
) -> Result<Vec<TranscriptSegment>, String> {
    // Concurrent transcriptions in one process must not share a chunk file.
    static CHUNK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut segments = Vec::new();
    let mut start = 0.0;
    while start < info.duration {
        let duration = chunk_seconds.min(info.duration - start);
        progress(&format!(
            "Transcribing {}–{} of {}…",
            clock(start),
            clock(start + duration),
            clock(info.duration)
        ));
        let temp = std::env::temp_dir().join(format!(
            "eddie-asr-{}-{}.wav",
            std::process::id(),
            CHUNK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        let result = openreel_media::write_wav_24k(&info.path, start, duration, &temp)
            .map_err(|_| "Transcription audio could not be prepared.".to_owned())
            .and_then(|_| {
                if std::fs::metadata(&temp).map_or(0, |meta| meta.len()) > max_bytes {
                    return Err("Transcription chunk was too large.".to_owned());
                }
                send(&temp, start)
            });
        let _ = std::fs::remove_file(&temp);
        segments.extend(result?);
        start += duration;
    }
    Ok(segments)
}

fn wav_part<'a>(path: &Path) -> Result<ureq::unversioned::multipart::Part<'a>, String> {
    ureq::unversioned::multipart::Part::file(path)
        .map_err(|_| "Transcription audio could not be opened.".to_owned())?
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|_| "Transcription audio could not be prepared.".to_owned())
}

fn post_form(
    url: &str,
    key: &str,
    form: ureq::unversioned::multipart::Form<'_>,
) -> Result<serde_json::Value, String> {
    let mut call = agent().post(url);
    if !key.is_empty() {
        call = call.header("Authorization", format!("Bearer {key}"));
    }
    let mut response = call
        .send(form)
        .map_err(|_| "Couldn't reach transcription.".to_owned())?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(api_error_for_status(status));
    }
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|_| "Couldn't read the transcript.".to_owned())?;
    serde_json::from_str(&text).map_err(|_| "Couldn't read the transcript.".to_owned())
}

fn meta(
    settings: &TranscribeSettings,
    info: &MediaInfo,
    progress: &mut dyn FnMut(&str),
) -> Result<(Vec<TranscriptSegment>, String), String> {
    if settings.api_key.is_empty() {
        return Err("Add a Meta API key for transcription.".to_owned());
    }
    let base = if settings.base_url.is_empty() {
        META_BASE_URL
    } else {
        &settings.base_url
    };
    let url = endpoint(base, "asr/transcribe");
    let model = if settings.model.is_empty() {
        META_MODEL
    } else {
        &settings.model
    };
    let request = serde_json::json!({
        "mode": "DIARIZATION",
        "model": model,
        "audioEncoding": "WAV",
    })
    .to_string();
    let segments = upload_chunks(
        info,
        META_CHUNK_SECONDS,
        META_MAX_BYTES,
        progress,
        |wav, offset| {
            let form = ureq::unversioned::multipart::Form::new()
                .text("request", &request)
                .part("audio", wav_part(wav)?);
            let value = post_form(&url, &settings.api_key, form)?;
            Ok(super::parse_transcript_response(&value, offset))
        },
    )?;
    Ok((segments, format!("meta:{model}")))
}

fn openai(
    settings: &TranscribeSettings,
    info: &MediaInfo,
    progress: &mut dyn FnMut(&str),
) -> Result<(Vec<TranscriptSegment>, String), String> {
    let base = if settings.base_url.is_empty() {
        OPENAI_BASE_URL
    } else {
        &settings.base_url
    };
    let url = endpoint(base, "audio/transcriptions");
    let keyless = is_local_endpoint(base);
    if settings.api_key.is_empty() && !keyless {
        return Err("Add an API key for transcription.".to_owned());
    }
    let model = if settings.model.is_empty() {
        OPENAI_MODEL
    } else {
        &settings.model
    };
    let segments = upload_chunks(
        info,
        OPENAI_CHUNK_SECONDS,
        OPENAI_MAX_BYTES,
        progress,
        |wav, offset| {
            let mut form = ureq::unversioned::multipart::Form::new()
                .text("model", model)
                .text("response_format", "verbose_json")
                .text("timestamp_granularities[]", "word")
                .text("timestamp_granularities[]", "segment");
            if !settings.language.is_empty() {
                form = form.text("language", &settings.language);
            }
            if !settings.vocabulary.is_empty() {
                form = form.text("prompt", &settings.vocabulary);
            }
            let form = form.part("file", wav_part(wav)?);
            let value = post_form(&url, &settings.api_key, form)?;
            Ok(parse_openai(&value, offset))
        },
    )?;
    Ok((segments, format!("openai:{model}")))
}

/// `verbose_json`: segments with text and times, and (with word
/// granularity) one top-level word list, placed in the segment holding
/// each word's midpoint.
fn parse_openai(value: &serde_json::Value, offset: f64) -> Vec<TranscriptSegment> {
    let words: Vec<TranscriptWord> = value["words"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|word| {
            let text = word["word"].as_str()?.trim();
            let start = word["start"].as_f64()?;
            let end = word["end"].as_f64()?;
            (!text.is_empty() && start.is_finite() && end >= start).then(|| TranscriptWord {
                start: offset + start,
                end: offset + end,
                text: text.to_owned(),
                confidence: None,
                estimated: false,
            })
        })
        .collect();
    let mut segments: Vec<TranscriptSegment> = value["segments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let text = row["text"].as_str()?.trim();
            let start = row["start"].as_f64()?;
            let end = row["end"].as_f64()?;
            (!text.is_empty() && end >= start).then(|| TranscriptSegment {
                start: offset + start,
                end: offset + end,
                speaker: None,
                text: text.to_owned(),
                words: Vec::new(),
                session: None,
            })
        })
        .collect();
    if segments.is_empty() {
        if let Some(text) = value["text"]
            .as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            segments.push(TranscriptSegment {
                start: words.first().map_or(offset, |word| word.start),
                end: words.last().map_or(offset, |word| word.end),
                speaker: None,
                text: text.to_owned(),
                words: Vec::new(),
                session: None,
            });
        }
    }
    let last = segments.len().saturating_sub(1);
    for word in words {
        let middle = (word.start + word.end) / 2.0;
        let index = segments
            .iter()
            .position(|segment| middle < segment.end)
            .unwrap_or(last);
        if let Some(segment) = segments.get_mut(index) {
            segment.words.push(word);
        }
    }
    let session = Arc::new(TranscriptSession {
        id: format!("openai-chunk-{offset:.3}"),
        start: offset,
        end: value["duration"].as_f64().map(|duration| offset + duration),
    });
    for segment in &mut segments {
        segment.session = Some(Arc::clone(&session));
    }
    segments
}

#[cfg(test)]
mod tests {
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
        let segments = parse_openai(&value, 100.0);
        assert_eq!(segments.len(), 2);
        assert_eq!((segments[0].start, segments[0].end), (100.0, 101.6));
        let texts =
            |i: usize| -> Vec<&str> { segments[i].words.iter().map(|w| w.text.as_str()).collect() };
        assert_eq!(texts(0), ["Hello", "there."]);
        assert_eq!(texts(1), ["Second", "line."]);
        assert_eq!(segments[1].words[0].start, 101.5);
        assert_eq!(segments[0].timing_precision(), "word");
    }

    #[test]
    fn tokens_join_into_words_that_end_where_the_next_begins() {
        let pieces = [
            (0.10, 0.40, " Hel".to_owned(), 0.9),
            (0.40, 0.60, "lo".to_owned(), 0.7),
            (0.55, 0.90, ",".to_owned(), 0.9),
            (0.50, 1.20, " world".to_owned(), 0.8),
            (1.20, 1.30, ".".to_owned(), 0.9),
        ];
        let words = words_from_tokens(&pieces, 10.0);
        let texts: Vec<_> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["Hello,", "world."]);
        // "Hello," would run to 0.9 but the next word starts at 0.5.
        assert_eq!((words[0].start, words[0].end), (10.1, 10.5));
        assert_eq!((words[1].start, words[1].end), (10.5, 11.3));
        assert!((words[0].confidence.unwrap() - 2.5 / 3.0).abs() < 1e-6);
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
}

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
    WhisperContextParameters, WhisperVadContext, WhisperVadContextParams, WhisperVadParams,
};

use super::{align, speech, store, TranscriptSegment, TranscriptSession, TranscriptWord};

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
    /// Where installed local models are looked up (`installed_model_dir`);
    /// never saved. `None` means no installed model is used, not even one
    /// named by `EDDIE_WHISPER_DIR` or `EDDIE_WHISPER_MODEL`, which keeps
    /// tests off the developer's models.
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
        let aligned = {
            let _turn = one_model_at_a_time();
            align::align_transcript(info, model.as_deref(), &mut segments)
        };
        match aligned {
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

/// Where the user's local models are installed: `EDDIE_WHISPER_DIR`, else
/// `whisper/` beside the settings file. Only the window and the CLI call
/// this, so the environment never reaches an editor built for a test.
pub fn installed_model_dir() -> Option<PathBuf> {
    std::env::var_os("EDDIE_WHISPER_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            Some(
                crate::editor::settings::config_path()?
                    .parent()?
                    .join("whisper"),
            )
        })
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
    let dir = settings.model_dir.as_deref();
    let file = |name: &str| dir.map(|dir| dir.join(format!("ggml-{name}.bin")));
    if !requested.is_empty() {
        let path = PathBuf::from(requested);
        if path.is_file() {
            return Ok(path);
        }
        return file(requested)
            .filter(|path| path.is_file())
            .ok_or_else(|| missing_model(dir, requested));
    }
    preferred_models()
        .iter()
        .find_map(|name| file(name).filter(|path| path.is_file()))
        .ok_or_else(|| missing_model(dir, preferred_models()[0]))
}

fn missing_model(dir: Option<&Path>, name: &str) -> String {
    format!(
        "no whisper model ggml-{name}.bin in {}; download it from \
         https://huggingface.co/ggerganov/whisper.cpp",
        dir.map_or("the whisper folder".into(), |dir| dir.display().to_string())
    )
}

/// One local model at a time, transcribing or aligning: each holds up to
/// 3 GB and already uses every core (or the GPU), so parallel curations
/// wait their turn here.
fn one_model_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn dtw_preset(model: &Path) -> Option<DtwModelPreset> {
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

fn local(
    settings: &TranscribeSettings,
    info: &MediaInfo,
    progress: &mut dyn FnMut(&str),
) -> Result<(Vec<TranscriptSegment>, String), String> {
    let model = local_model(settings)?;
    let _turn = one_model_at_a_time();
    let model_name = model
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let detector = model.with_file_name(VAD_MODEL);
    progress(&format!("Loading {model_name}…"));
    whisper_rs::install_logging_hooks();
    // A detector that will not load is the same as none installed.
    let mut detector = detector.is_file().then_some(detector).and_then(|path| {
        WhisperVadContext::new(&path.to_string_lossy(), WhisperVadContextParams::default()).ok()
    });
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
        state
            .full(params, &padded)
            .map_err(|error| format!("{model_name} failed: {error}"))?;
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
        let words = words_from_tokens(&tokens, &Sound::new(pcm, &speech), start);
        segments.extend(turns_from_words(words, &session));
        Ok(())
    })?;
    let detector = if detector.is_some() {
        ""
    } else {
        ", no voice detector"
    };
    Ok((segments, format!("local:{model_name}{detector}")))
}

/// The stretches of a window that hold speech. whisper hears only these,
/// so music and room tone are never "heard" as words; times it reports
/// are mapped back to the window with `original`.
struct Speech {
    /// First and end sample of each stretch, in order.
    kept: Vec<(usize, usize)>,
}

/// Sound kept either side of detected speech, so word edges are heard whole.
const SPEECH_PAD: f64 = 0.2;
/// Speech this close together stays one stretch: whisper reads sentence
/// ends from ordinary pauses, so only longer non-speech is removed.
const SPEECH_JOIN: f64 = 1.0;
/// Silence put where sound was removed, as whisper.cpp's own detector does.
const SPEECH_GAP: usize = SAMPLE_RATE / 10;

impl Speech {
    fn whole(samples: usize) -> Self {
        Self {
            kept: vec![(0, samples)],
        }
    }

    /// `spans` are the detector's (start, end) seconds, in order.
    fn from_spans(spans: impl Iterator<Item = (f64, f64)>, samples: usize) -> Self {
        let sample = |seconds: f64| ((seconds.max(0.0) * SAMPLE_RATE as f64) as usize).min(samples);
        let mut kept: Vec<(usize, usize)> = Vec::new();
        for (start, end) in spans {
            let (from, to) = (sample(start - SPEECH_PAD), sample(end + SPEECH_PAD));
            if to <= from {
                continue;
            }
            match kept.last_mut() {
                Some(last) if sample(start - SPEECH_JOIN) <= last.1 => last.1 = last.1.max(to),
                _ => kept.push((from, to)),
            }
        }
        Self { kept }
    }

    /// What whisper hears: the kept stretches with short silences between.
    fn samples(&self, pcm: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        for (index, (from, to)) in self.kept.iter().enumerate() {
            if index > 0 {
                out.resize(out.len() + SPEECH_GAP, 0.0);
            }
            out.extend_from_slice(&pcm[*from..*to]);
        }
        out
    }

    /// Window time of a time in `samples`. A time in an inserted silence is
    /// the end of the speech before it.
    fn original(&self, heard: f64) -> f64 {
        let seconds = |samples: usize| samples as f64 / SAMPLE_RATE as f64;
        let mut at = 0.0;
        let mut last_end = 0.0;
        for (index, (from, to)) in self.kept.iter().enumerate() {
            if index > 0 {
                at += seconds(SPEECH_GAP);
            }
            if heard < at {
                return last_end;
            }
            let length = seconds(to - from);
            if heard < at + length {
                return seconds(*from) + (heard - at);
            }
            at += length;
            last_end = seconds(*to);
        }
        last_end
    }

    fn holds(&self, sample: usize) -> bool {
        self.kept
            .iter()
            .any(|(from, to)| (*from..*to).contains(&sample))
    }
}

/// Which 20 ms frames of a window hold sound: inside the speech whisper
/// heard, above that speech's own silence level, and in a run that gets
/// well above it. A breath or a click in a pause is over the silence level
/// for a frame or two and never loud; a word's soft start or tail is quiet
/// too, but runs into the word.
struct Sound {
    frames: Vec<bool>,
}

const SOUND_FRAME: usize = SAMPLE_RATE / 50;
/// A run of sound must reach this many times the silence level.
const SOUND_REACH: f32 = 3.0;

impl Sound {
    fn new(pcm: &[f32], speech: &Speech) -> Self {
        let peaks: Vec<f32> = pcm
            .chunks(SOUND_FRAME)
            .enumerate()
            .map(|(frame, samples)| {
                if speech.holds(frame * SOUND_FRAME + SOUND_FRAME / 2) {
                    samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()))
                } else {
                    0.0
                }
            })
            .collect();
        let spoken: Vec<f32> = peaks.iter().copied().filter(|peak| *peak > 0.0).collect();
        let silent = openreel_media::silence_threshold(&spoken);
        let mut frames = vec![false; peaks.len()];
        let mut at = 0;
        while at < peaks.len() {
            let begins = at;
            while at < peaks.len() && peaks[at] > silent {
                at += 1;
            }
            if peaks[begins..at]
                .iter()
                .any(|peak| *peak >= silent * SOUND_REACH)
            {
                frames[begins..at].fill(true);
            }
            at = at.max(begins + 1);
        }
        Self { frames }
    }

    /// Runs of frames without sound that begin in `from..to` (seconds), as
    /// (start, end). A run already under way at `from` begins there, and a
    /// run is followed to its end even past `to`.
    fn pauses(&self, from: f64, to: f64) -> Vec<(f64, f64)> {
        let seconds = |frame: usize| (frame * SOUND_FRAME) as f64 / SAMPLE_RATE as f64;
        let frame = |at: f64| (at.max(0.0) * SAMPLE_RATE as f64 / SOUND_FRAME as f64) as usize;
        let mut runs = Vec::new();
        let mut at = frame(from);
        while at < self.frames.len() && seconds(at) < to {
            if self.frames[at] {
                at += 1;
                continue;
            }
            let begins = at;
            while at < self.frames.len() && !self.frames[at] {
                at += 1;
            }
            runs.push((seconds(begins).max(from), seconds(at)));
        }
        runs
    }
}

/// One whisper token on its window's clock.
struct Token {
    text: String,
    /// Where the token before it ends, or its segment starts.
    start: f64,
    end: f64,
    probability: f32,
    /// A segment's first token starts a word even without a leading space.
    opens_segment: bool,
}

/// A pause this short between two words is only trusted where whisper put
/// punctuation; elsewhere it may be a consonant's closure inside a word.
const PAUSE_SECONDS: f64 = 0.16;
/// A pause shorter than this is not looked for at all.
const MIN_PAUSE_SECONDS: f64 = 0.04;

/// Join whisper tokens into words: a token starting with a space starts a
/// new word. A word runs from its first token's start to the end of its
/// last token that is not punctuation. Punctuation time, and the start of
/// the next word, hold any pause between two words: where the sound stops
/// there the word ends, and the next starts where sound returns. Words
/// with no pause between them touch.
fn words_from_tokens(tokens: &[Token], sound: &Sound, offset: f64) -> Vec<TranscriptWord> {
    struct Raw {
        start: f64,
        /// End of the first token: the word's sound has begun by here.
        begun: f64,
        end: f64,
        text: String,
        probabilities: Vec<f32>,
    }
    let spoken = |text: &str| text.chars().any(char::is_alphanumeric);
    let mut raw: Vec<Raw> = Vec::new();
    for token in tokens {
        match raw.last_mut() {
            Some(word) if !token.text.starts_with(' ') && !token.opens_segment => {
                if spoken(&token.text) {
                    word.end = token.end;
                }
                word.text.push_str(&token.text);
                word.probabilities.push(token.probability);
            }
            _ => raw.push(Raw {
                start: token.start,
                begun: token.end,
                end: token.end,
                text: token.text.clone(),
                probabilities: vec![token.probability],
            }),
        }
    }
    let last_token_end = tokens.last().map_or(0.0, |token| token.end);
    raw.retain(|word| spoken(&word.text));

    // Where the word before a gap stops and the word after it starts.
    // Either may be missing: the gap before the first word or after the last.
    let gap = |before: Option<&Raw>, word: Option<&Raw>| -> (f64, f64) {
        let from = match (before, word) {
            (Some(before), _) => before.end,
            (None, Some(word)) => word.start,
            (None, None) => 0.0,
        };
        // Punctuation after `before` ends where `word` starts.
        let lead = word.map_or(last_token_end, |word| word.start).max(from);
        let until = word.map_or(lead + align::SNAP_SECONDS, |word| {
            (word.begun - align::MIN_KEEP_SECONDS).max(lead)
        });
        // The longest quiet run is the pause. A shorter one beside it is a
        // consonant's closure in one of the two words.
        sound
            .pauses(from, (lead + align::SNAP_SECONDS).min(until).max(lead))
            .into_iter()
            .map(|(start, end)| (start, end.min(until)))
            .filter(|(start, end)| {
                let least = if *start <= lead {
                    MIN_PAUSE_SECONDS
                } else {
                    PAUSE_SECONDS
                };
                end - start >= least - 1e-9
            })
            .max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)))
            // No pause: punctuation time stays with the earlier word.
            .unwrap_or((lead, lead))
    };

    let mut words: Vec<TranscriptWord> = Vec::with_capacity(raw.len());
    for index in 0..=raw.len() {
        let word = raw.get(index);
        let (stops, starts) = gap(index.checked_sub(1).map(|index| &raw[index]), word);
        if let Some(earlier) = words.last_mut() {
            earlier.end = (offset + stops).max(earlier.start);
        }
        if let Some(word) = word {
            words.push(TranscriptWord {
                start: offset + starts,
                end: offset + word.end.max(starts),
                text: word.text.trim().to_owned(),
                confidence: Some(
                    word.probabilities.iter().sum::<f32>() / word.probabilities.len().max(1) as f32,
                ),
                estimated: false,
            });
        }
    }
    words
}

/// Group a window's words into turns: a turn is speech without a pause of
/// `speech::CONTINUATION_SECONDS`, and starts and ends on its own words.
/// whisper's segments are not used: they split sentences and run through
/// pauses.
fn turns_from_words(
    words: Vec<TranscriptWord>,
    session: &Arc<TranscriptSession>,
) -> Vec<TranscriptSegment> {
    let mut turns: Vec<TranscriptSegment> = Vec::new();
    for word in words {
        match turns.last_mut() {
            Some(turn) if word.start - turn.end < speech::CONTINUATION_SECONDS => {
                turn.end = word.end;
                turn.text.push(' ');
                turn.text.push_str(&word.text);
                turn.words.push(word);
            }
            _ => turns.push(TranscriptSegment {
                start: word.start,
                end: word.end,
                speaker: None,
                text: word.text.clone(),
                words: vec![word],
                session: Some(Arc::clone(session)),
            }),
        }
    }
    turns
}

/// Walk a file's sound in windows of about ten minutes, each ending in a
/// quiet half second. `decode(start, seconds)` returns 16 kHz samples whose
/// first sample is at `start` (silence fills any lead) and `hear(start,
/// samples)` takes each window. The sound can end before the file does (a
/// picture that runs a frame longer, or a silent tail), so a read can come
/// back short. A read with nothing in it moves on a whole window: sound
/// that starts later is still heard, and the walk always ends.
fn local_windows(
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

/// At most this much file (a header and 0.1 s of 24 kHz 16-bit mono) is
/// a chunk with nothing to hear: the sound starts later or has ended.
const SILENT_CHUNK_BYTES: u64 = 78 + 4_800;

/// A folder only this user can read, for audio on its way to a service.
/// The shared temp folder alone would leave each chunk readable by every
/// local user while it uploads.
struct ChunkDir(PathBuf);

impl ChunkDir {
    fn new() -> Result<Self, String> {
        // Concurrent transcriptions in one process must not share a folder.
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "eddie-asr-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            // Never an existing folder: someone else could own it.
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err("Transcription audio could not be prepared.".to_owned()),
            }
        }
    }
}

impl Drop for ChunkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A service's turns for the chunk `start..end`, in order, with every
/// time clamped to the chunk. A turn left without length is dropped:
/// nothing can be cut or captioned from it, and it would read as "no
/// speech here". One reaching far past the chunk would have alignment
/// hear the rest of the file.
fn inside_chunk(turns: Vec<TranscriptSegment>, start: f64, end: f64) -> Vec<TranscriptSegment> {
    // A time that is not a number stays one and fails both comparisons.
    let clamp = |from: f64, to: f64| (from.clamp(start, end), to.clamp(start, end));
    let mut turns: Vec<TranscriptSegment> = turns
        .into_iter()
        .filter_map(|mut turn| {
            (turn.start, turn.end) = clamp(turn.start, turn.end);
            turn.words.retain_mut(|word| {
                (word.start, word.end) = clamp(word.start, word.end);
                // Services do return words without length.
                word.end >= word.start
            });
            (turn.end > turn.start).then_some(turn)
        })
        .collect();
    turns.sort_by(|a, b| a.start.total_cmp(&b.start));
    turns
}

/// Upload each chunk of 24 kHz WAV through `send(file, start, seconds)`,
/// in order. A chunk without sound is not sent.
fn upload_chunks(
    info: &MediaInfo,
    chunk_seconds: f64,
    max_bytes: u64,
    progress: &mut dyn FnMut(&str),
    mut send: impl FnMut(&Path, f64, f64) -> Result<Vec<TranscriptSegment>, String>,
) -> Result<Vec<TranscriptSegment>, String> {
    let scratch = ChunkDir::new()?;
    let temp = scratch.0.join("chunk.wav");
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
        openreel_media::write_wav_24k(&info.path, start, duration, &temp)
            .map_err(|_| "Transcription audio could not be prepared.".to_owned())?;
        let bytes = std::fs::metadata(&temp).map_or(0, |meta| meta.len());
        if bytes > max_bytes {
            return Err("Transcription chunk was too large.".to_owned());
        }
        if bytes > SILENT_CHUNK_BYTES {
            let turns = send(&temp, start, duration)?;
            segments.extend(inside_chunk(turns, start, start + duration));
        }
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
        |wav, offset, seconds| {
            let form = ureq::unversioned::multipart::Form::new()
                .text("request", &request)
                .part("audio", wav_part(wav)?);
            let value = post_form(&url, &settings.api_key, form)?;
            Ok(super::parse_transcript_response(&value, offset, seconds))
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
        |wav, offset, seconds| {
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
            Ok(parse_openai(&value, offset, seconds))
        },
    )?;
    Ok((segments, format!("openai:{model}")))
}

/// `verbose_json` for the `seconds` of sound sent from `offset`: segments
/// with text and times, and (with word granularity) one top-level word
/// list, placed in the segment holding each word's midpoint. An answer
/// with text alone was said somewhere in what was sent, so it spans it.
fn parse_openai(value: &serde_json::Value, offset: f64, seconds: f64) -> Vec<TranscriptSegment> {
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
            (!text.is_empty() && end > start).then(|| TranscriptSegment {
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
                end: words.last().map_or(offset + seconds, |word| word.end),
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
        let turns = super::super::parse_transcript_response(&value, 540.0, 30.0);
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
        let speech =
            Speech::from_spans([(1.0, 4.0), (4.5, 6.0), (20.0, 22.0)].into_iter(), samples);
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
}

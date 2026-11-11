//! Transcription providers: local whisper.cpp by default, or a cloud API.
//!
//! Local runs anywhere without a key. It uses the most accurate model that
//! runs well (large-v3 on a GPU with room for it, large-v3-turbo on the
//! CPU), the
//! Silero voice detector when its model is present so music and room tone
//! are not "heard" as words, beam search, and DTW word timing. Cloud
//! providers are Meta's Voice Transcribe (turns with speakers; words come
//! from local alignment) and any OpenAI-style `/audio/transcriptions`
//! endpoint (OpenAI, Groq and similar) with word timestamps.
//!
//! Finished transcripts are saved per file beside saved curations, so a file
//! is transcribed once unless asked again.

use std::path::{Path, PathBuf};

use openreel_media::MediaInfo;

use super::{align, store, TranscriptSegment};
use cloud::{meta, openai};
use local::{local, one_model_at_a_time};

mod cloud;
mod local;
mod models;
mod words;
pub use models::{best_installed, gpu_build, installed_model_dir, installed_models};
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
    let (mut segments, source, placed) = match settings.provider {
        Provider::Local => local(settings, info, progress)?,
        Provider::Meta => {
            meta(settings, info, progress).map(|(turns, source)| (turns, source, None))?
        }
        Provider::OpenAi => {
            openai(settings, info, progress).map(|(turns, source)| (turns, source, None))?
        }
    };
    let mut note = format!("Transcription: {} turns ({source}).", segments.len());
    // Why a local model other than the preferred one was used.
    if let Some(placed) = placed {
        note.push_str(&format!(" {placed}."));
    }
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

#[cfg(test)]
mod tests;

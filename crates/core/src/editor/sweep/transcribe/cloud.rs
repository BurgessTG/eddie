//! Cloud transcription: Meta Voice Transcribe and OpenAI-style
//! `/audio/transcriptions`, sent as private WAV chunks.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openreel_media::MediaInfo;

use super::super::transcript::parse_transcript_response;
use super::super::{TranscriptSegment, TranscriptSession, TranscriptWord};
use super::local::clock;
use super::{is_local_endpoint, TranscribeSettings};

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

fn api_error_for_status(status: u16) -> String {
    match status {
        401 | 403 => "Check the speech key in Settings.".to_owned(),
        404 => "That speech model or URL was not found. Check Settings.".to_owned(),
        429 => "The transcription service is busy. Try again shortly.".to_owned(),
        500..=599 => "The transcription service had a problem. Try again shortly.".to_owned(),
        _ => "The transcription request was rejected. Check Settings and try again.".to_owned(),
    }
}

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
pub(super) fn inside_chunk(
    turns: Vec<TranscriptSegment>,
    start: f64,
    end: f64,
) -> Vec<TranscriptSegment> {
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
pub(super) fn upload_chunks(
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

pub(super) fn meta(
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
            Ok(parse_transcript_response(&value, offset, seconds))
        },
    )?;
    Ok((segments, format!("meta:{model}")))
}

pub(super) fn openai(
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
pub(super) fn parse_openai(
    value: &serde_json::Value,
    offset: f64,
    seconds: f64,
) -> Vec<TranscriptSegment> {
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

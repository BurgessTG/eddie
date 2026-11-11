//! What was said in a file: sessions, turns and words, and reading a
//! provider's reply into them.

use std::sync::Arc;

use openreel_media::SweepSegment;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSession {
    pub(super) id: String,
    pub(super) start: f64,
    pub(super) end: Option<f64>,
}

/// Whether a word or turn text ends on a sentence mark. A trailing ellipsis
/// leaves the thought open.
pub fn ends_sentence(text: &str) -> bool {
    let text = text
        .trim_end()
        .trim_end_matches(['"', '\'', ')', ']', '\u{201d}', '\u{2019}']);
    !text.ends_with("...")
        && !text.ends_with('\u{2026}')
        && text.ends_with(['.', '?', '!', '\u{3002}', '\u{ff1f}', '\u{ff01}'])
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptWord {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub confidence: Option<f32>,
    /// Interpolated between heard words by local alignment; good enough for
    /// captions, never a cut point.
    pub estimated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub start: f64,
    pub end: f64,
    pub speaker: Option<String>,
    pub text: String,
    /// Word timings. Voice Transcribe is turn-level only; these come from
    /// a provider that returns words, or from local alignment (`align`).
    pub words: Vec<TranscriptWord>,
    /// Speaker labels only identify speakers inside this upload/session.
    pub session: Option<Arc<TranscriptSession>>,
}

/// A turn this long with fewer letters per second than this carries no
/// thought to protect. Crowd ambience comes back as one long turn with a word
/// or two ("LT" across 16 s); speech, even slow or in CJK, is far denser.
const AMBIENT_MIN_SECONDS: f64 = 2.0;

const AMBIENT_LETTERS_PER_SECOND: f64 = 1.5;

impl TranscriptSegment {
    pub fn timing_precision(&self) -> &'static str {
        if self.words.is_empty() {
            "turn"
        } else {
            "word"
        }
    }

    pub fn session_json(&self) -> serde_json::Value {
        self.session
            .as_ref()
            .map_or(serde_json::Value::Null, |session| {
                serde_json::json!({"id":session.id, "sourceStart":session.start,
                "sourceEnd":session.end})
            })
    }

    /// Whether a turn's text is too sparse for its span to be evidence that
    /// someone speaks throughout it.
    pub fn is_ambient(&self) -> bool {
        let seconds = self.end - self.start;
        seconds >= AMBIENT_MIN_SECONDS
            && (self.text.chars().filter(|c| c.is_alphanumeric()).count() as f64)
                < AMBIENT_LETTERS_PER_SECOND * seconds
    }
}

/// The turns spoken during a shot. Sparse turns are left out: they are
/// ambience, or words a speech model imagined over music ("Thank you." across
/// 30 s), and would make every shot under them a talking one.
pub(super) fn overlapping_transcript(
    segment: &SweepSegment,
    transcript: &[TranscriptSegment],
) -> Vec<TranscriptSegment> {
    transcript
        .iter()
        .filter(|turn| !turn.is_ambient())
        .filter(|turn| turn.end > segment.start && turn.start < segment.end)
        .cloned()
        .collect()
}

fn transcript_time(
    row: &serde_json::Value,
    millisecond_key: &str,
    second_key: &str,
    offset: f64,
) -> Option<f64> {
    if let Some(milliseconds) = row[millisecond_key].as_f64() {
        return Some(offset + milliseconds / 1000.0);
    }
    if let Some(seconds) = row[second_key].as_f64() {
        return Some(offset + seconds);
    }
    if let Some(timestamp) = row["timestamp"].as_array() {
        let index = if second_key == "start" { 0 } else { 1 };
        return timestamp
            .get(index)
            .and_then(|value| value.as_f64())
            .map(|seconds| offset + seconds);
    }
    if row["timestamp"].is_object() {
        return transcript_time(&row["timestamp"], millisecond_key, second_key, offset);
    }
    None
}

fn parse_transcript_words(row: &serde_json::Value, offset: f64) -> Vec<TranscriptWord> {
    let Some(rows) = row["words"].as_array() else {
        return Vec::new();
    };
    let mut words: Vec<TranscriptWord> = rows
        .iter()
        .filter_map(|word| {
            let text = word["word"]
                .as_str()
                .or_else(|| word["text"].as_str())
                .or_else(|| word["transcript"].as_str())?
                .trim();
            if text.is_empty() {
                return None;
            }
            let start = transcript_time(word, "startMs", "start", offset)?;
            let end = transcript_time(word, "endMs", "end", offset)?;
            if !start.is_finite() || !end.is_finite() || end < start {
                return None;
            }
            Some(TranscriptWord {
                start,
                end,
                text: text.to_owned(),
                confidence: word["confidence"]
                    .as_f64()
                    .map(|value| value.clamp(0.0, 1.0) as f32),
                estimated: false,
            })
        })
        .collect();
    words.sort_by(|a, b| a.start.total_cmp(&b.start));
    words
}

fn parse_transcript_row(row: &serde_json::Value, offset: f64) -> Option<TranscriptSegment> {
    let text = row["transcript"]
        .as_str()
        .or_else(|| row["text"].as_str())?
        .trim();
    if text.is_empty() {
        return None;
    }
    let speaker = match &row["speaker"] {
        serde_json::Value::String(speaker) if !speaker.is_empty() => Some(speaker.clone()),
        serde_json::Value::Number(speaker) => Some(speaker.to_string()),
        _ => None,
    };
    Some(TranscriptSegment {
        start: transcript_time(row, "startMs", "start", offset).unwrap_or(offset),
        end: transcript_time(row, "endMs", "end", offset).unwrap_or(offset),
        speaker,
        text: text.to_owned(),
        words: parse_transcript_words(row, offset),
        session: None,
    })
}

/// Voice Transcribe's answer for the `seconds` of sound sent from
/// `offset`. An answer with text but no turns was said somewhere in what
/// was sent, so it spans it.
pub(super) fn parse_transcript_response(
    value: &serde_json::Value,
    offset: f64,
    seconds: f64,
) -> Vec<TranscriptSegment> {
    let rows = value["turns"]
        .as_array()
        .or_else(|| value["segments"].as_array());
    let mut turns: Vec<TranscriptSegment> = rows
        .map(|rows| {
            rows.iter()
                .filter_map(|row| parse_transcript_row(row, offset))
                .collect()
        })
        .unwrap_or_default();
    if turns.is_empty() {
        if let Some(text) = value["transcript"]
            .as_str()
            .or_else(|| value["text"].as_str())
            .filter(|text| !text.trim().is_empty())
        {
            let words = parse_transcript_words(value, offset);
            let duration = value["audioDurationMs"]
                .as_f64()
                .map_or(seconds, |milliseconds| milliseconds / 1000.0);
            let end = words
                .last()
                .map(|word| word.end)
                .unwrap_or_else(|| offset + duration);
            turns.push(TranscriptSegment {
                start: offset,
                end,
                speaker: None,
                text: text.trim().to_owned(),
                words,
                session: None,
            });
        }
    }
    let session = Arc::new(TranscriptSession {
        id: value["sessionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("source-chunk-{offset:.9}")),
        start: offset,
        end: value["audioDurationMs"]
            .as_f64()
            .filter(|duration| duration.is_finite() && *duration >= 0.0)
            .map(|duration| offset + duration / 1000.0),
    });
    for turn in &mut turns {
        turn.session = Some(Arc::clone(&session));
    }
    turns
}

/// Transcript segment IDs in script state: `tx:<asset path>#<index>`.
pub fn transcript_segment_id(asset_path: &str, index: usize) -> String {
    format!("tx:{asset_path}#{index}")
}

//! Explicit curation: complete shot inventory, Muse Voice Transcribe,
//! and timestamped visual review of all source ranges.
//!
//! SAM tracks requested visual concepts before the final editorial decision.
//! Its source-timed boxes and detail crops inform the reviewer, not a separate
//! quality bonus. Mask coverage and expressions remain unknown.

use std::sync::mpsc::{channel, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use eframe::egui;
use openreel_media::{
    MediaInfo, SegmentVisualEvidence, SubjectSegmentationStatus, SweepSegment, VisualEvidenceSource,
};
use serde::{Deserialize, Serialize};

use super::chat::{api_endpoint, api_error_for_status, check_model_connection};
use super::media::AssetState;
use super::{ChatSettings, EditorApp};

mod align;
mod curation;
#[cfg(test)]
mod curation_tests;
mod store;
mod subjects;
pub(super) use curation::{ClipReview, ReviewStatus, ReviewUsage};
pub(super) use store::{default_dir as default_curation_dir, load as load_curation};

const TRANSCRIBE_MODEL: &str = "muse-voice-transcribe-1.0";
const SAM_MODEL: &str = "sam-3.1";

/// Clip type stays distinct from the app-assigned `prime` quality tag.
pub(super) const CURATOR_CATEGORIES: [&str; 5] =
    ["reaction", "talking", "action", "broll", "establishing"];

/// Controlled tag vocabulary. Local classification derives these
/// from measured signals; model tags are validated against the
/// same list so chat prompts can filter on stable words.
pub(super) const CURATOR_TAGS: [&str; 27] = [
    "prime",
    "speech",
    "quiet",
    "loud-peak",
    "steady",
    "shaky",
    "fast-motion",
    "slow",
    "scene-open",
    "long-take",
    "short",
    "music-bed",
    "subject-centered",
    "subject-large",
    "subject-moving",
    "face-visible",
    "detail",
    "wide-shot",
    "demonstration",
    "interaction",
    "reveal",
    "subject-entering",
    "subject-exiting",
    "occluded",
    "camera-move",
    "expressive",
    "on-screen-text",
];

fn is_model_tag(tag: &str) -> bool {
    CURATOR_TAGS.contains(&tag) && tag != "prime" && tag != "face-visible"
}

/// A mixed music bed (finished montages, scored footage) keeps
/// nearly every audio bucket audible. This is only a hint: continuous
/// speech can do the same, so it must not gate transcription.
fn is_music_bed(segment: &SweepSegment) -> bool {
    segment.audio_coverage >= 0.75 && segment.audio_energy >= 0.2
}

/// Prime quota: at most this share of one asset's candidates, and
/// only at or above `PRIME_MIN_SCORE`.
const PRIME_SHARE: f32 = 0.2;
const PRIME_MIN_SCORE: f32 = 75.0;
// Voice Transcribe accepts WAV mono 16-bit PCM at 16/24 kHz, at most
// 10 minutes / 32 MB per request (see `openreel_media::write_wav_24k`),
// and returns turn-level timestamps only — one speaker label per
// turn, no word timing. Reaction/speech logic is built on turns.
const ASR_CHUNK_SECONDS: f64 = 540.0;
const ASR_MAX_BYTES: u64 = 32 * 1024 * 1024;
const SPEECH_HANDLE_SECONDS: f64 = 0.12;

/// Files curated at once. Review mostly waits on the provider, but local
/// shot analysis decodes every frame, so weak machines stay at one.
fn parallel_sweeps() -> usize {
    thread::available_parallelism().map_or(1, |cores| (cores.get() / 4).clamp(1, 3))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct TranscriptSession {
    id: String,
    start: f64,
    end: Option<f64>,
}

/// Whether a word or turn text ends on a sentence mark. A trailing ellipsis
/// leaves the thought open.
pub(super) fn ends_sentence(text: &str) -> bool {
    let text = text
        .trim_end()
        .trim_end_matches(['"', '\'', ')', ']', '\u{201d}', '\u{2019}']);
    !text.ends_with("...")
        && !text.ends_with('\u{2026}')
        && text.ends_with(['.', '?', '!', '\u{3002}', '\u{ff1f}', '\u{ff01}'])
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct TranscriptWord {
    pub(super) start: f64,
    pub(super) end: f64,
    pub(super) text: String,
    pub(super) confidence: Option<f32>,
    /// Interpolated between heard words by local alignment; good enough for
    /// captions, never a cut point.
    pub(super) estimated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct TranscriptSegment {
    pub(super) start: f64,
    pub(super) end: f64,
    pub(super) speaker: Option<String>,
    pub(super) text: String,
    /// Word timings. Voice Transcribe is turn-level only; these come from
    /// a provider that returns words, or from local alignment (`align`).
    pub(super) words: Vec<TranscriptWord>,
    /// Speaker labels only identify speakers inside this upload/session.
    pub(super) session: Option<Arc<TranscriptSession>>,
}

/// A turn this long with fewer letters per second than this carries no
/// thought to protect. Crowd ambience comes back as one long turn with a word
/// or two ("LT" across 16 s); speech, even slow or in CJK, is far denser.
const AMBIENT_MIN_SECONDS: f64 = 2.0;
const AMBIENT_LETTERS_PER_SECOND: f64 = 1.5;

impl TranscriptSegment {
    pub(super) fn timing_precision(&self) -> &'static str {
        if self.words.is_empty() {
            "turn"
        } else {
            "word"
        }
    }

    pub(super) fn session_json(&self) -> serde_json::Value {
        self.session
            .as_ref()
            .map_or(serde_json::Value::Null, |session| {
                serde_json::json!({"id":session.id, "sourceStart":session.start,
                "sourceEnd":session.end})
            })
    }

    /// Whether a turn's text is too sparse for its span to be evidence that
    /// someone speaks throughout it.
    pub(super) fn is_ambient(&self) -> bool {
        let seconds = self.end - self.start;
        seconds >= AMBIENT_MIN_SECONDS
            && (self.text.chars().filter(|c| c.is_alphanumeric()).count() as f64)
                < AMBIENT_LETTERS_PER_SECOND * seconds
    }

    fn reaches_internal_asr_edge(&self, source_end: f64) -> bool {
        self.session.as_ref().is_some_and(|session| {
            (session.start > 0.0 && self.start <= session.start + SPEECH_HANDLE_SECONDS)
                || session.end.is_some_and(|end| {
                    end < source_end - SPEECH_HANDLE_SECONDS
                        && self.end >= end - SPEECH_HANDLE_SECONDS
                })
        })
    }
}

/// Curation frees gigabytes of decode and analysis buffers from worker
/// threads; glibc keeps them mapped (measured 1.3 GB of 2.9 GB RSS on the
/// expo clips) until asked to return them.
fn release_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        extern "C" {
            fn malloc_trim(pad: usize) -> std::ffi::c_int;
        }
        // SAFETY: malloc_trim only releases free heap pages; it has no
        // preconditions and is thread-safe.
        unsafe {
            malloc_trim(0);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CaptionChunk {
    pub(super) start: f64,
    pub(super) end: f64,
    pub(super) text: String,
    /// Source start time of each word in `text`, only from real word timing.
    pub(super) words: Vec<f64>,
}

/// A word's letters and apostrophes, lowercased, for comparing repeats.
fn caption_key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '’')
        .flat_map(char::to_lowercase)
        .collect()
}

fn ends_with_any(text: &str, marks: &str) -> bool {
    text.trim_end_matches(['"', '”', ')'])
        .chars()
        .last()
        .is_some_and(|c| marks.contains(c))
}

/// Remove spoken stutters: a run of one to three words immediately
/// repeated ("out of of", "if it's if it's"). The first copy keeps its
/// start, the second's end and punctuation. Different words that only
/// share a stem ("Eddie Eddie's") are kept.
fn drop_stutters(words: Vec<(f64, f64, String)>) -> Vec<(f64, f64, String)> {
    let mut kept: Vec<(f64, f64, String)> = Vec::new();
    let mut index = 0;
    'words: while index < words.len() {
        for run in (1..=3).rev() {
            if kept.len() < run || index + run > words.len() {
                continue;
            }
            let earlier = &kept[kept.len() - run..];
            let later = &words[index..index + run];
            // A sentence end between the copies is a real repeat, not a stutter.
            if ends_with_any(&earlier[run - 1].2, ".?!") {
                continue;
            }
            let same = earlier.iter().zip(later).all(|(a, b)| {
                !caption_key(&a.2).is_empty() && caption_key(&a.2) == caption_key(&b.2)
            });
            if same {
                let last = kept.last_mut().expect("run is non-empty");
                let (_, end, text) = &later[run - 1];
                last.1 = last.1.max(*end);
                if ends_with_any(text, ".?!,;:") && !ends_with_any(&last.2, ".?!,;:") {
                    last.2 = text.clone();
                }
                index += run;
                continue 'words;
            }
        }
        kept.push(words[index].clone());
        index += 1;
    }
    kept
}

/// How many words of a full caption line to keep when the next word does
/// not fit: after a clause mark, else before a conjunction, in the later
/// two thirds; else all but a dangling article, preposition or pronoun.
fn caption_break(group: &[(f64, f64, String)]) -> usize {
    const JOINS: [&str; 9] = [
        "and", "but", "so", "because", "while", "when", "after", "then", "or",
    ];
    const DANGLING: [&str; 24] = [
        "a", "an", "the", "i", "to", "of", "in", "on", "at", "by", "from", "into", "with", "for",
        "my", "our", "your", "it", "it's", "and", "but", "so", "or", "is",
    ];
    let earliest = group.len().div_ceil(3).max(1);
    if let Some(keep) = (earliest..group.len())
        .rev()
        .find(|&keep| ends_with_any(&group[keep - 1].2, ",;:"))
    {
        return keep;
    }
    if let Some(keep) = (earliest..group.len())
        .rev()
        .find(|&keep| JOINS.contains(&caption_key(&group[keep].2).as_str()))
    {
        return keep;
    }
    let mut keep = group.len();
    while keep > earliest && DANGLING.contains(&caption_key(&group[keep - 1].2).as_str()) {
        keep -= 1;
    }
    keep
}

/// Split one transcript turn into readable caption chunks. Real
/// word timings are used when present; turn-only text is allocated
/// proportionally and therefore remains an estimate.
pub(super) fn caption_chunks(segment: &TranscriptSegment) -> Vec<CaptionChunk> {
    const MAX_CHARS: usize = 42;
    const MAX_DURATION: f64 = 3.5;
    if segment.text.trim().is_empty() || segment.end <= segment.start || segment.is_ambient() {
        return Vec::new();
    }

    if !segment.words.is_empty() {
        let mut words: Vec<&TranscriptWord> = segment
            .words
            .iter()
            .filter(|word| {
                word.start.is_finite()
                    && word.end.is_finite()
                    && word.end >= word.start
                    && !word.text.trim().is_empty()
            })
            .collect();
        words.sort_by(|a, b| a.start.total_cmp(&b.start));
        let words = drop_stutters(
            words
                .into_iter()
                .map(|word| (word.start, word.end, word.text.trim().to_owned()))
                .collect(),
        );
        if !words.is_empty() {
            let mut chunks: Vec<CaptionChunk> = Vec::new();
            let mut group: Vec<(f64, f64, String)> = Vec::new();
            let push_group = |group: &[(f64, f64, String)], chunks: &mut Vec<CaptionChunk>| {
                let (Some(first), Some(last)) = (group.first(), group.last()) else {
                    return;
                };
                let (start, last_end) = (first.0, last.1);
                chunks.push(CaptionChunk {
                    start,
                    end: last_end.max(start + 0.8).min(segment.end.max(last_end)),
                    text: group
                        .iter()
                        .map(|word| word.2.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                    words: group.iter().map(|word| word.0).collect(),
                });
            };
            for word in words {
                let next_len = group.iter().map(|w| w.2.len() + 1).sum::<usize>() + word.2.len();
                let next_duration = group.first().map(|first| word.1 - first.0).unwrap_or(0.0);
                let ends_sentence = ends_with_any(&word.2, ".?!");
                // A sentence may run a little long rather than leave its
                // last word alone on the next caption.
                let max_chars = if ends_sentence {
                    MAX_CHARS + 8
                } else {
                    MAX_CHARS
                };
                if !group.is_empty() && (next_len > max_chars || next_duration > MAX_DURATION) {
                    let keep = caption_break(&group);
                    let carried = group.split_off(keep);
                    push_group(&group, &mut chunks);
                    group = carried;
                }
                group.push(word);
                if ends_sentence {
                    push_group(&group, &mut chunks);
                    group.clear();
                }
            }
            push_group(&group, &mut chunks);
            for index in 0..chunks.len().saturating_sub(1) {
                let next_start = chunks[index + 1].start;
                if chunks[index].end > next_start {
                    chunks[index].end = next_start;
                }
            }
            return chunks
                .into_iter()
                .filter(|chunk| chunk.end > chunk.start)
                .collect();
        }
    }

    let mut pieces: Vec<String> = Vec::new();
    let mut current = String::new();
    let words = drop_stutters(
        segment
            .text
            .split_whitespace()
            .map(|word| (0.0, 0.0, word.to_owned()))
            .collect(),
    );
    for (_, _, word) in &words {
        if !current.is_empty() && current.len() + 1 + word.len() > MAX_CHARS {
            pieces.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    if pieces.is_empty() {
        return Vec::new();
    }
    let total_chars: usize = pieces.iter().map(|piece| piece.len()).sum();
    let duration = segment.end - segment.start;
    let mut cursor = segment.start;
    let last_index = pieces.len() - 1;
    pieces
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let end = if index == last_index {
                segment.end
            } else if total_chars > 0 {
                cursor + duration * text.len() as f64 / total_chars as f64
            } else {
                segment.end
            };
            let chunk = CaptionChunk {
                start: cursor,
                end,
                text,
                words: Vec::new(),
            };
            cursor = end;
            chunk
        })
        .collect()
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SweepCandidate {
    pub(super) id: String,
    pub(super) asset_path: String,
    pub(super) asset_name: String,
    pub(super) title: String,
    pub(super) source_in: f64,
    pub(super) duration: f64,
    pub(super) score: f32,
    pub(super) reason: String,
    pub(super) stability: f32,
    pub(super) audio_energy: f32,
    pub(super) audio_peak: f32,
    /// One of `CURATOR_CATEGORIES`.
    pub(super) category: String,
    /// Subset of `CURATOR_TAGS`.
    pub(super) tags: Vec<String>,
    /// Quality subscores, 0..1 per asset: normalized mean motion
    /// and a blended audio score (energy + peak).
    pub(super) motion_score: f32,
    pub(super) audio_score: f32,
    /// Baked-in music bed suspected: audio signals were discounted
    /// for this candidate and JS can filter on it.
    pub(super) music_bed: bool,
    pub(super) visual: Option<SegmentVisualEvidence>,
    pub(super) has_video: bool,
    pub(super) transcript: Vec<TranscriptSegment>,
    pub(super) review: ClipReview,
    pub(super) dismissed: bool,
}

impl SweepCandidate {
    pub(super) fn end(&self) -> f64 {
        self.source_in + self.duration
    }

    pub(super) fn reaction_score(&self) -> f32 {
        if !self.is_reaction() {
            return 0.0;
        }
        if self.review.status == ReviewStatus::Reviewed {
            return self.score / 100.0;
        }
        (0.6 * self.audio_peak + 0.4 * (self.audio_energy * 1.5).clamp(0.0, 1.0)).clamp(0.0, 1.0)
    }

    pub(super) fn is_reaction(&self) -> bool {
        self.category == "reaction"
    }

    pub(super) fn is_prime(&self) -> bool {
        self.tags.iter().any(|tag| tag == "prime")
    }

    /// Cross-modal talking evidence: transcript speech is primary;
    /// a centered, comparatively still foreground raises it. This
    /// does not claim lip movement or active-speaker detection.
    pub(super) fn talking_score(&self) -> f32 {
        let base = talking_score(!self.transcript.is_empty(), self.visual.as_ref());
        if self.transcript.is_empty() {
            return base;
        }
        let Some(visual) = self.visual.as_ref() else {
            return base;
        };
        if visual.face_present != Some(true) {
            return base;
        }
        let center_factor = match (visual.sam_face_box_center_x, visual.sam_face_box_center_y) {
            (Some(x), Some(y)) => {
                let distance = ((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt();
                (1.0 - distance / std::f32::consts::FRAC_1_SQRT_2).clamp(0.0, 1.0)
            }
            _ => 0.5,
        };
        let confidence = visual.sam_face_confidence.unwrap_or(0.75);
        base.max(0.7 + 0.3 * center_factor * confidence)
            .clamp(0.0, 1.0)
    }

    pub(super) fn visual_json(&self) -> serde_json::Value {
        visual_evidence_json(self.visual.as_ref(), self.talking_score())
    }

    pub(super) fn review_json(&self) -> serde_json::Value {
        let mut review = self.review.json(self.source_in, self.end());
        review["speech"] = serde_json::json!(self
            .transcript
            .iter()
            .map(|turn| {
                serde_json::json!({"turnStart":turn.start, "turnEnd":turn.end,
                "sessionId":turn.session.as_ref().map(|session| &session.id),
                "retainedStart":self.source_in.max(turn.start),
                "retainedEnd":self.end().min(turn.end),
                "completeAvailableTurn":self.source_in <= turn.start && self.end() >= turn.end,
                "timingPrecision":turn.timing_precision()})
            })
            .collect::<Vec<_>>());
        review
    }

    pub(super) fn transcript_text(&self) -> String {
        self.transcript
            .iter()
            .map(|segment| segment.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn visual_center_factor(visual: &SegmentVisualEvidence) -> f32 {
    let centroid_closeness = match (visual.subject_center_x, visual.subject_center_y) {
        (Some(x), Some(y)) => {
            let distance = ((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt();
            (1.0 - distance / std::f32::consts::FRAC_1_SQRT_2).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };
    (0.45 * visual.center_saliency
        + 0.35 * centroid_closeness
        + 0.2 * (1.0 - visual.subject_motion))
        .clamp(0.0, 1.0)
}

fn talking_score(has_speech: bool, visual: Option<&SegmentVisualEvidence>) -> f32 {
    if has_speech {
        visual
            .map(|visual| 0.45 + 0.55 * visual_center_factor(visual))
            .unwrap_or(0.55)
    } else {
        visual
            .map(|visual| 0.2 * visual_center_factor(visual))
            .unwrap_or(0.0)
    }
    .clamp(0.0, 1.0)
}

/// Stable evidence shape shared by model ranking, JS state, and
/// chat. Unknown semantic values serialize as JSON `null`.
pub(super) fn visual_evidence_json(
    visual: Option<&SegmentVisualEvidence>,
    talking_score: f32,
) -> serde_json::Value {
    match visual {
        Some(visual) => serde_json::json!({
            "source": match visual.source {
                VisualEvidenceSource::LocalProxy => "local-proxy",
                VisualEvidenceSource::Sam => "sam",
            },
            "samStatus": match visual.sam_status {
                SubjectSegmentationStatus::Unavailable => "unavailable",
                SubjectSegmentationStatus::Ready => "ready",
                SubjectSegmentationStatus::Error => "error",
            },
            "samUnavailableReason": if visual.sam_status == SubjectSegmentationStatus::Unavailable {
                openreel_media::sam_unavailable_reason()
            } else {
                ""
            },
            "subjectCoverage": visual.subject_coverage,
            "subjectCenter": {
                "x": visual.subject_center_x,
                "y": visual.subject_center_y,
            },
            "subjectMotion": visual.subject_motion,
            "centerSaliency": visual.center_saliency,
            "edgeDensity": visual.edge_density,
            "luminance": visual.luminance,
            "contrast": visual.contrast,
            "skinToneRatio": visual.skin_tone_ratio,
            "samMaskCoverage": visual.sam_mask_coverage,
            "samMaskCenter": {
                "x": visual.sam_mask_center_x,
                "y": visual.sam_mask_center_y,
            },
            "samBoxCoverage": visual.sam_box_coverage,
            "samBoxCenter": {
                "x": visual.sam_box_center_x,
                "y": visual.sam_box_center_y,
            },
            "samConfidence": visual.sam_confidence,
            "samObjectCount": visual.sam_object_count,
            "samFaceBoxCoverage": visual.sam_face_box_coverage,
            "samFaceBoxCenter": {
                "x": visual.sam_face_box_center_x,
                "y": visual.sam_face_box_center_y,
            },
            "samFaceConfidence": visual.sam_face_confidence,
            "samFaceCount": visual.sam_face_count,
            "samFrameTime": visual.sam_frame_time,
            "samError": visual.sam_error,
            "facePresent": visual.face_present,
            "expression": visual.expression,
            "talkingScore": talking_score,
        }),
        None => serde_json::json!({
            "source": "none",
            "samStatus": "unavailable",
            "samUnavailableReason": openreel_media::sam_unavailable_reason(),
            "subjectCoverage": null,
            "subjectCenter": {"x": null, "y": null},
            "subjectMotion": null,
            "centerSaliency": null,
            "edgeDensity": null,
            "luminance": null,
            "contrast": null,
            "skinToneRatio": null,
            "samMaskCoverage": null,
            "samMaskCenter": {"x": null, "y": null},
            "samBoxCoverage": null,
            "samBoxCenter": {"x": null, "y": null},
            "samConfidence": null,
            "samObjectCount": null,
            "samFaceBoxCoverage": null,
            "samFaceBoxCenter": {"x": null, "y": null},
            "samFaceConfidence": null,
            "samFaceCount": null,
            "samFrameTime": null,
            "samError": null,
            "facePresent": null,
            "expression": null,
            "talkingScore": talking_score,
        }),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum SweepStatus {
    NotStarted,
    Running,
    Ready,
    Failed,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SweepData {
    pub(super) status: SweepStatus,
    pub(super) note: String,
    pub(super) transcript: Vec<TranscriptSegment>,
    pub(super) candidates: Vec<SweepCandidate>,
    /// Complete source-ordered scene breakdown. Unlike the
    /// ranked `candidates` picks, these spans are meant to cover
    /// the whole asset so an edited video can be deconstructed.
    pub(super) scene_clips: Vec<SweepCandidate>,
    pub(super) usage: ReviewUsage,
}

impl Default for SweepData {
    fn default() -> Self {
        Self {
            status: SweepStatus::NotStarted,
            note: String::new(),
            transcript: Vec::new(),
            candidates: Vec::new(),
            scene_clips: Vec::new(),
            usage: ReviewUsage::default(),
        }
    }
}

impl SweepData {
    /// Reviewed ranges override whole-shot hints only where they overlap.
    pub(super) fn music_bed_overlap(&self, start: f64, end: f64) -> f64 {
        let overlap = |clip: &SweepCandidate, start: f64, end: f64| {
            (clip.end().min(end) - clip.source_in.max(start)).max(0.0)
        };
        let reviewed: Vec<_> = self
            .candidates
            .iter()
            .filter(|clip| {
                clip.review.status == ReviewStatus::Reviewed && overlap(clip, start, end) > 0.0
            })
            .collect();
        let baseline = if self.scene_clips.is_empty() {
            &self.candidates
        } else {
            &self.scene_clips
        };
        let unreviewed: f64 = baseline
            .iter()
            .filter(|clip| clip.music_bed)
            .map(|clip| {
                let replaced: f64 = reviewed
                    .iter()
                    .map(|review| overlap(review, start.max(clip.source_in), end.min(clip.end())))
                    .sum();
                (overlap(clip, start, end) - replaced).max(0.0)
            })
            .sum();
        let reviewed_bed: f64 = reviewed
            .iter()
            .filter(|clip| clip.music_bed)
            .map(|clip| overlap(clip, start, end))
            .sum();
        (unreviewed + reviewed_bed).clamp(0.0, (end - start).max(0.0))
    }
}

pub(super) enum SweepMsg {
    Progress { path: String, message: String },
    Done { path: String, data: SweepData },
    Failed { path: String, message: String },
}

impl EditorApp {
    /// Explicit user-triggered curation for one ready asset.
    /// Re-curation replaces the previous transcript and candidates,
    /// which is how a later API-key session upgrades local results.
    pub(super) fn curate_asset(&mut self, ctx: &egui::Context, path: String) {
        let Some(asset) = self.assets.iter_mut().find(|asset| asset.info.path == path) else {
            return;
        };
        if asset.state != AssetState::Ready
            || asset.sweep.status == SweepStatus::Running
            || self.sweep_queue.contains(&path)
        {
            return;
        }
        if asset.sweep.status != SweepStatus::NotStarted {
            asset.sweep = SweepData::default();
        }
        asset.sweep.status = SweepStatus::Running;
        asset.sweep.note = "Curating…".to_owned();
        self.sweep_queue.push(path);
        self.open_curator_gallery();
        self.start_next_sweep(ctx);
    }

    /// Queue every ready asset that has never been curated.
    pub(super) fn curate_all_new(&mut self, ctx: &egui::Context) {
        let paths: Vec<String> = self
            .assets
            .iter()
            .filter(|asset| {
                asset.state == AssetState::Ready && asset.sweep.status == SweepStatus::NotStarted
            })
            .map(|asset| asset.info.path.clone())
            .collect();
        for path in paths {
            self.curate_asset(ctx, path);
        }
    }

    /// Start queued curations up to `parallel_sweeps()` at once. Model
    /// review is mostly waiting on the provider, so files overlap.
    fn start_next_sweep(&mut self, ctx: &egui::Context) {
        while self.sweep_running.len() < parallel_sweeps() && !self.sweep_queue.is_empty() {
            let path = self.sweep_queue.remove(0);
            let Some(asset) = self.assets.iter().find(|asset| asset.info.path == path) else {
                continue;
            };
            let info = asset.info.clone();
            let peaks = asset.peaks.clone();
            let settings = self.chat_settings.clone();
            let store_dir = self.curation_dir.clone();
            let tx = match &self.sweep_tx {
                Some(tx) => tx.clone(),
                None => {
                    let (tx, rx) = channel::<SweepMsg>();
                    self.sweep_rx = Some(rx);
                    self.sweep_tx = Some(tx.clone());
                    tx
                }
            };
            self.sweep_running.push(path);
            let ctx = ctx.clone();
            thread::spawn(move || {
                let progress = &mut |message: &str| {
                    let _ = tx.send(SweepMsg::Progress {
                        path: info.path.clone(),
                        message: message.to_owned(),
                    });
                    ctx.request_repaint();
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_sweep(&info, &peaks, &settings, progress)
                }))
                .unwrap_or_else(|_| {
                    Err("Curation stopped unexpectedly; retry this video.".to_owned())
                });
                let msg = match result {
                    Ok(data) => {
                        if let Some(dir) = &store_dir {
                            if let Err(error) = store::save(dir, &info.path, &data) {
                                eprintln!("curation for {} was not saved: {error}", info.name);
                            }
                        }
                        SweepMsg::Done {
                            path: info.path.clone(),
                            data,
                        }
                    }
                    Err(message) => SweepMsg::Failed {
                        path: info.path.clone(),
                        message,
                    },
                };
                let _ = tx.send(msg);
                ctx.request_repaint();
            });
        }
    }

    pub(super) fn poll_sweeps(&mut self, ctx: &egui::Context) {
        let mut finished = false;
        loop {
            let Some(receiver) = self.sweep_rx.as_ref() else {
                return;
            };
            let msg = match receiver.try_recv() {
                Ok(message) => message,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    for asset in &mut self.assets {
                        if asset.sweep.status == SweepStatus::Running
                            && !self.sweep_queue.contains(&asset.info.path)
                        {
                            asset.sweep.status = SweepStatus::Failed;
                            asset.sweep.note =
                                "Curation worker stopped; retry this video.".to_owned();
                        }
                    }
                    self.status = "Curation worker stopped; retry this video.".to_owned();
                    self.sweep_running.clear();
                    finished = true;
                    break;
                }
            };
            match msg {
                SweepMsg::Progress { path, message } => {
                    if let Some(asset) =
                        self.assets.iter_mut().find(|asset| asset.info.path == path)
                    {
                        asset.sweep.note = message.clone();
                        self.status = message;
                    }
                }
                SweepMsg::Done { path, data } => {
                    self.sweep_running.retain(|running| *running != path);
                    if let Some(asset) =
                        self.assets.iter_mut().find(|asset| asset.info.path == path)
                    {
                        let pick_count = data.candidates.len();
                        let scene_count = data.scene_clips.len();
                        asset.sweep = data;
                        self.status = format!(
                            "Curator found {pick_count} clip{} and {scene_count} scene{} in {}.",
                            if pick_count == 1 { "" } else { "s" },
                            if scene_count == 1 { "" } else { "s" },
                            asset.info.name
                        );
                    }
                    finished = true;
                }
                SweepMsg::Failed { path, message } => {
                    self.sweep_running.retain(|running| *running != path);
                    if let Some(asset) =
                        self.assets.iter_mut().find(|asset| asset.info.path == path)
                    {
                        asset.sweep.status = SweepStatus::Failed;
                        asset.sweep.note = message.clone();
                        self.status = message;
                    }
                    finished = true;
                }
            }
        }
        if !finished {
            ctx.request_repaint_after(Duration::from_millis(180));
            return;
        }
        if self.sweep_running.is_empty() {
            self.sweep_rx = None;
            self.sweep_tx = None;
        }
        release_freed_memory();
        // The sweep changed: this is a pinned-summary refresh point
        // (the only other one is chat submit, as a safety net).
        self.refresh_pinned_summary();
        self.start_next_sweep(ctx);
        if self.sweep_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(180));
        }
    }

    pub(super) fn visible_sweep_candidates(&self) -> Vec<(usize, usize)> {
        let mut refs: Vec<(usize, usize, f32)> = self
            .assets
            .iter()
            .enumerate()
            .flat_map(|(asset_idx, asset)| {
                asset
                    .sweep
                    .candidates
                    .iter()
                    .enumerate()
                    .filter(|(_, candidate)| !candidate.dismissed)
                    .map(move |(candidate_idx, candidate)| {
                        (asset_idx, candidate_idx, candidate.score)
                    })
            })
            .collect();
        refs.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        refs.into_iter().map(|(a, c, _)| (a, c)).collect()
    }

    pub(super) fn visible_scene_clips(&self) -> Vec<(usize, usize)> {
        self.assets
            .iter()
            .enumerate()
            .flat_map(|(asset_idx, asset)| {
                asset
                    .sweep
                    .scene_clips
                    .iter()
                    .enumerate()
                    .map(move |(scene_idx, _)| (asset_idx, scene_idx))
            })
            .collect()
    }

    pub(super) fn dismiss_sweep_candidate(&mut self, candidate_id: &str) {
        for asset in &mut self.assets {
            if let Some(candidate) = asset
                .sweep
                .candidates
                .iter_mut()
                .chain(asset.sweep.scene_clips.iter_mut())
                .find(|candidate| candidate.id == candidate_id)
            {
                candidate.dismissed = true;
                self.status = "Candidate dismissed.".to_owned();
                return;
            }
        }
    }

    pub(super) fn add_sweep_candidate(&mut self, candidate_id: &str) {
        let candidate = self
            .assets
            .iter()
            .flat_map(|asset| {
                asset
                    .sweep
                    .candidates
                    .iter()
                    .chain(asset.sweep.scene_clips.iter())
            })
            .find(|candidate| candidate.id == candidate_id && !candidate.dismissed)
            .cloned();
        if let Some(candidate) = candidate {
            let at = self.timeline.end();
            self.add_candidate_at(&candidate, at);
        }
    }

    pub(super) fn add_all_sweep_candidates(&mut self) {
        let (candidates, skipped): (Vec<SweepCandidate>, usize) =
            self.fitting_format(self.curator_candidate_refs(false).into_iter().map(
                |(asset, candidate)| (asset, &self.assets[asset].sweep.candidates[candidate]),
            ));
        if candidates.is_empty() {
            self.status = if skipped > 0 {
                "No shown clips match the project format.".to_owned()
            } else {
                "No sweep clips to add.".to_owned()
            };
            return;
        }
        let mut cursor = self.timeline.end();
        for candidate in &candidates {
            self.add_candidate_at(candidate, cursor);
            cursor += candidate.duration;
        }
        self.status = added_message(candidates.len(), "sweep clip", skipped);
    }

    pub(super) fn add_all_scene_clips(&mut self) {
        let (clips, skipped): (Vec<SweepCandidate>, usize) =
            self.fitting_format(self.curator_candidate_refs(true).into_iter().map(
                |(asset, candidate)| (asset, &self.assets[asset].sweep.scene_clips[candidate]),
            ));
        if clips.is_empty() {
            self.status = if skipped > 0 {
                "No scenes match the project format.".to_owned()
            } else {
                "No scene clips to add.".to_owned()
            };
            return;
        }
        let mut cursor = self.timeline.end();
        for candidate in &clips {
            self.add_candidate_at(candidate, cursor);
            cursor += candidate.duration;
        }
        self.status = added_message(clips.len(), "scene clip", skipped);
    }

    /// Bulk adds skip sources whose orientation clashes with the
    /// project instead of failing or pillarboxing them.
    fn fitting_format<'a>(
        &self,
        clips: impl Iterator<Item = (usize, &'a SweepCandidate)>,
    ) -> (Vec<SweepCandidate>, usize) {
        let mut skipped = 0;
        let fitting = clips
            .filter(|(asset, _)| {
                let clashes = self.clashes_with_format(&self.assets[*asset].info);
                skipped += usize::from(clashes);
                !clashes
            })
            .map(|(_, clip)| clip.clone())
            .collect();
        (fitting, skipped)
    }

    fn add_candidate_at(&mut self, candidate: &SweepCandidate, at: f64) {
        self.add_asset_range_to_timeline(
            &candidate.asset_path,
            &candidate.title,
            None,
            at,
            candidate.source_in,
            candidate.duration,
        );
    }
}

fn added_message(count: usize, noun: &str, skipped: usize) -> String {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let added = format!("Added {count} {noun}{}", plural(count));
    if skipped == 0 {
        format!("{added}.")
    } else {
        format!("{added}; skipped {skipped} in the other format.")
    }
}

fn run_sweep(
    info: &MediaInfo,
    peaks: &[f32],
    settings: &ChatSettings,
    progress: &mut dyn FnMut(&str),
) -> Result<SweepData, String> {
    run_sweep_with_mode(info, peaks, settings, progress, subjects::Mode::Automatic)
}

fn run_sweep_with_mode(
    info: &MediaInfo,
    peaks: &[f32],
    settings: &ChatSettings,
    progress: &mut dyn FnMut(&str),
    mode: subjects::Mode,
) -> Result<SweepData, String> {
    progress("Analyzing scenes…");
    let motion = if info.has_video {
        openreel_media::motion_samples(&info.path, info)
            .map_err(|error| format!("Scene analysis failed: {error}"))?
    } else {
        Vec::new()
    };
    if info.has_video && motion.is_empty() {
        return Err("Scene analysis decoded no video frames.".to_owned());
    }
    let segments = openreel_media::build_scene_segments(
        info.duration,
        &motion,
        peaks,
        openreel_media::WAVEFORM_PEAKS_PER_SEC,
    );
    if segments.is_empty() {
        return Err(format!(
            "Curator found no usable segments in {}.",
            info.name
        ));
    }
    let mut stage_notes = Vec::new();
    let model_ready = if settings.can_call_model() {
        progress("Checking model access…");
        match check_model_connection(settings) {
            Ok(()) => true,
            Err(error) => {
                stage_notes.push(format!("Model review unavailable: {error}"));
                false
            }
        }
    } else {
        stage_notes.push("Local analysis only; add a Meta API key for visual review.".to_owned());
        false
    };
    let (transcript, transcript_available) = if model_ready && info.has_audio {
        // Waveform coverage cannot distinguish a soundtrack from continuous
        // speech. It may discount energy, but must not suppress transcription.
        progress("Transcribing audio…");
        match transcribe_asset(settings, info) {
            Ok(mut transcript) => {
                stage_notes.push(format!(
                    "Transcription: {} speaker turns.",
                    transcript.len()
                ));
                // Voice Transcribe has no word timing. A local model adds it
                // to multi-sentence turns so cuts can fall between sentences.
                if align::model_path().is_some() {
                    progress("Aligning words…");
                }
                match align::align_transcript(info, &mut transcript) {
                    Ok((_, 0)) => {}
                    Ok((aligned, candidates)) => stage_notes.push(format!(
                        "Word timing: {aligned}/{candidates} multi-sentence turns aligned locally."
                    )),
                    Err(error) => stage_notes.push(format!("Word timing: {error}.")),
                }
                (transcript, true)
            }
            Err(error) => {
                stage_notes.push(format!("Transcription failed: {error}"));
                (Vec::new(), false)
            }
        }
    } else {
        stage_notes.push(
            if info.has_audio {
                "Transcription: unavailable without model access."
            } else {
                "Transcription: no audio stream."
            }
            .to_owned(),
        );
        (Vec::new(), !info.has_audio)
    };

    // Model selections never remove shots from the source inventory.
    let mut scene_clips = local_candidates(info, &segments, &transcript);
    for (index, scene) in scene_clips.iter_mut().enumerate() {
        scene.id = format!("{}#scene-{index}", info.path);
    }
    let covered_seconds: f64 = segments.iter().map(|segment| segment.duration()).sum();
    let mut usage = ReviewUsage::default();
    let mut candidates = if model_ready {
        let review = curation::review(
            settings,
            info,
            &motion,
            peaks,
            &transcript,
            transcript_available,
            &mut scene_clips,
            progress,
            mode,
        );
        stage_notes.push(review.note);
        usage = review.usage;
        review.candidates
    } else {
        let mut local = local_candidates(info, &segments, &transcript);
        if settings.can_call_model() {
            for candidate in local.iter_mut().chain(scene_clips.iter_mut()) {
                candidate.review.status = ReviewStatus::NeedsReview;
                candidate.reason = "Needs review; model access check failed.".to_owned();
            }
        }
        local
    };
    stage_notes.push(format!(
        "Clips: {}. Scenes: {}, {:.1}s covered.",
        candidates.len(),
        scene_clips.len(),
        covered_seconds,
    ));
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    apply_prime(&mut candidates);
    Ok(SweepData {
        status: SweepStatus::Ready,
        transcript,
        candidates,
        scene_clips,
        note: stage_notes.join(" "),
        usage,
    })
}

struct SegmentNorms {
    max_energy: f32,
    max_peak: f32,
    max_motion: f32,
}

fn segment_norms(segments: &[SweepSegment]) -> SegmentNorms {
    SegmentNorms {
        max_energy: segments
            .iter()
            .map(|segment| segment.audio_energy)
            .fold(0.0_f32, f32::max)
            .max(0.001),
        max_peak: segments
            .iter()
            .map(|segment| segment.audio_peak)
            .fold(0.0_f32, f32::max)
            .max(0.001),
        max_motion: segments
            .iter()
            .map(|segment| segment.mean_motion)
            .fold(0.0_f32, f32::max)
            .max(0.001),
    }
}

/// Local category + tags from measured signals only. Rule order
/// matters: reactions and speech are the strongest editorial
/// signals, then motion, then scene-openers, else b-roll. `prime`
/// is never returned here; `apply_prime` promotes after ranking.
fn classify_local(
    segment: &SweepSegment,
    has_speech: bool,
    motion_score: f32,
    audio_score: f32,
) -> (String, Vec<String>) {
    let mut tags: Vec<String> = Vec::new();
    let mut tag = |name: &str| tags.push(name.to_owned());
    if has_speech {
        tag("speech");
    }
    let music_bed = is_music_bed(segment);
    if music_bed {
        tag("music-bed");
    } else if segment.audio_peak >= 0.75 {
        tag("loud-peak");
    }
    if audio_score < 0.15 {
        tag("quiet");
    }
    if segment.stability >= 0.7 {
        tag("steady");
    } else if segment.stability < 0.4 {
        tag("shaky");
    }
    if motion_score >= 0.65 {
        tag("fast-motion");
    } else if motion_score < 0.2 {
        tag("slow");
    }
    if segment.scene_change >= 0.18 {
        tag("scene-open");
    }
    if segment.duration() >= 8.0 {
        tag("long-take");
    } else if segment.duration() <= 3.0 {
        tag("short");
    }
    let mut wide_without_subject = false;
    if let Some(visual) = &segment.visual {
        let centered_centroid = match (visual.subject_center_x, visual.subject_center_y) {
            (Some(x), Some(y)) => (0.3..=0.7).contains(&x) && (0.25..=0.75).contains(&y),
            _ => false,
        };
        if visual.center_saliency >= 0.55 && centered_centroid {
            tag("subject-centered");
        }
        if visual.subject_coverage >= 0.18 {
            tag("subject-large");
        }
        if visual.subject_motion >= 0.08 {
            tag("subject-moving");
        }
        wide_without_subject = visual.subject_coverage <= 0.03
            && visual.center_saliency < 0.35
            && segment.stability >= 0.6
            && segment.duration() >= 3.0;
    }
    let reactionish =
        !music_bed && segment.audio_peak >= 0.75 && audio_score >= 0.4 && segment.duration() <= 7.0;
    let category = if reactionish {
        "reaction"
    } else if has_speech {
        "talking"
    } else if motion_score >= 0.65
        || segment
            .visual
            .as_ref()
            .map(|visual| visual.subject_motion >= 0.22)
            .unwrap_or(false)
    {
        "action"
    } else if (segment.scene_change >= 0.18 && segment.stability >= 0.6) || wide_without_subject {
        "establishing"
    } else {
        "broll"
    };
    (category.to_owned(), tags)
}

/// Quality must not erase content type: a prime reaction is still a reaction.
fn apply_prime(candidates: &mut [SweepCandidate]) {
    let eligible = |candidate: &SweepCandidate| {
        matches!(
            candidate.review.status,
            ReviewStatus::Local | ReviewStatus::Reviewed
        )
    };
    let quota = (candidates
        .iter()
        .filter(|candidate| eligible(candidate))
        .count() as f32
        * PRIME_SHARE)
        .ceil() as usize;
    for candidate in candidates.iter_mut() {
        candidate.tags.retain(|tag| tag != "prime");
    }
    for candidate in candidates
        .iter_mut()
        .filter(|candidate| eligible(candidate))
        .take(quota)
    {
        if candidate.score >= PRIME_MIN_SCORE {
            candidate.tags.push("prime".to_owned());
        }
    }
}

fn local_candidates(
    info: &MediaInfo,
    segments: &[SweepSegment],
    transcript: &[TranscriptSegment],
) -> Vec<SweepCandidate> {
    let norms = segment_norms(segments);
    let max_energy = norms.max_energy;
    let max_peak = norms.max_peak;
    segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            let energy = segment.audio_energy / max_energy;
            let peak = segment.audio_peak / max_peak;
            let duration = segment.duration();
            let duration_score = if (3.0..=7.0).contains(&duration) {
                1.0
            } else if duration < 3.0 {
                (duration / 3.0) as f32
            } else {
                (1.0 - ((duration - 7.0) / 5.0).min(0.5)) as f32
            };
            let stability = if info.has_video {
                segment.stability
            } else {
                0.75
            };
            let overlaps = overlapping_transcript(segment, transcript);
            let mut score = if is_music_bed(segment) {
                100.0 * (0.65 * stability + 0.35 * duration_score)
            } else {
                100.0 * (0.42 * stability + 0.28 * energy + 0.15 * peak + 0.15 * duration_score)
            };
            if !overlaps.is_empty() {
                score = (score + 8.0).min(100.0);
            }
            if let Some(visual) = &segment.visual {
                let visual_bonus = 3.0 * visual.center_saliency
                    + 2.0 * (visual.subject_coverage / 0.25).clamp(0.0, 1.0);
                score = (score + visual_bonus).min(100.0);
            }
            let reason = local_reason(info, segment, &overlaps);
            candidate_from_segment(info, index, segment, &norms, score, reason, overlaps, None)
        })
        .collect()
}

fn local_reason(
    info: &MediaInfo,
    segment: &SweepSegment,
    transcript: &[TranscriptSegment],
) -> String {
    let at = minute_second(segment.start);
    if let Some(first) = transcript.first() {
        let excerpt: String = first.text.chars().take(48).collect();
        let centered = segment
            .visual
            .as_ref()
            .map(|visual| visual_center_factor(visual) >= 0.65)
            .unwrap_or(false);
        return if centered && segment.stability >= 0.65 {
            format!("Steady centered speech at {at} — “{excerpt}”.")
        } else if segment.stability >= 0.65 {
            format!("Steady speech at {at} — “{excerpt}”.")
        } else {
            format!("Speech lands at {at} — “{excerpt}”.")
        };
    }
    if !info.has_video {
        return format!("Lively audio around {at}, with a clean in and out.");
    }
    if segment
        .visual
        .as_ref()
        .map(|visual| {
            visual.center_saliency >= 0.55
                && (visual.subject_coverage >= 0.18 || visual.subject_motion >= 0.08)
        })
        .unwrap_or(false)
    {
        format!("Active centered shot starting at {at}.")
    } else if segment.stability >= 0.75 && segment.audio_energy >= 0.25 {
        format!("Steady shot with an energy lift at {at}.")
    } else if segment.audio_peak >= 0.75 {
        format!("Audio peaks at {at}; worth a look despite movement.")
    } else if segment.stability >= 0.75 {
        format!("Steady, clean section starting at {at}.")
    } else if segment.scene_change >= 0.18 {
        format!("Fresh scene at {at}, with usable motion.")
    } else {
        format!("Usable section starting at {at}.")
    }
}

fn candidate_from_segment(
    info: &MediaInfo,
    index: usize,
    segment: &SweepSegment,
    norms: &SegmentNorms,
    score: f32,
    reason: String,
    transcript: Vec<TranscriptSegment>,
    model_call: Option<(String, Vec<String>)>,
) -> SweepCandidate {
    let motion_score = (segment.mean_motion / norms.max_motion).clamp(0.0, 1.0);
    let audio_score = (0.6 * (segment.audio_energy / norms.max_energy)
        + 0.4 * (segment.audio_peak / norms.max_peak))
        .clamp(0.0, 1.0);
    let (local_category, local_tags) =
        classify_local(segment, !transcript.is_empty(), motion_score, audio_score);
    let (category, tags, music_bed) = match model_call {
        Some((category, tags)) if CURATOR_CATEGORIES.contains(&category.as_str()) => {
            let tags: Vec<String> = tags.into_iter().filter(|tag| is_model_tag(tag)).collect();
            let music_bed = tags.iter().any(|tag| tag == "music-bed")
                && !tags.iter().any(|tag| tag == "speech")
                && category != "talking";
            (category, tags, music_bed)
        }
        _ => (
            local_category,
            local_tags,
            is_music_bed(segment) && transcript.is_empty(),
        ),
    };
    SweepCandidate {
        id: format!("{}#{index}", info.path),
        asset_path: info.path.clone(),
        asset_name: info.name.clone(),
        title: format!("{} · {}", info.name, minute_second(segment.start)),
        source_in: segment.start,
        duration: segment.duration(),
        score: score.clamp(0.0, 100.0),
        reason,
        stability: segment.stability,
        audio_energy: segment.audio_energy,
        audio_peak: segment.audio_peak,
        category,
        tags,
        motion_score,
        audio_score,
        music_bed,
        visual: segment.visual.clone(),
        has_video: info.has_video,
        transcript,
        review: ClipReview::default(),
        dismissed: false,
    }
}

fn overlapping_transcript(
    segment: &SweepSegment,
    transcript: &[TranscriptSegment],
) -> Vec<TranscriptSegment> {
    transcript
        .iter()
        .filter(|turn| turn.end > segment.start && turn.start < segment.end)
        .cloned()
        .collect()
}

pub(super) fn minute_second(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SamBox {
    object_id: String,
    x1: i64,
    y1: i64,
    x2: i64,
    y2: i64,
    width: u32,
    height: u32,
    confidence: Option<f32>,
}

impl SamBox {
    fn coverage(&self) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 0.0;
        }
        let width = (self.x2 + 1 - self.x1).max(0) as f32 / self.width as f32;
        let height = (self.y2 + 1 - self.y1).max(0) as f32 / self.height as f32;
        (width * height).clamp(0.0, 1.0)
    }

    fn center_x(&self) -> f32 {
        if self.width == 0 {
            0.0
        } else {
            ((self.x1 + self.x2 + 1) as f32 / 2.0 / self.width as f32).clamp(0.0, 1.0)
        }
    }

    fn center_y(&self) -> f32 {
        if self.height == 0 {
            0.0
        } else {
            ((self.y1 + self.y2 + 1) as f32 / 2.0 / self.height as f32).clamp(0.0, 1.0)
        }
    }
}

fn sam_responses_url(settings: &ChatSettings) -> String {
    api_endpoint(settings, "responses")
}

pub(super) fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0] as u32;
        let second = *chunk.get(1).unwrap_or(&0) as u32;
        let third = *chunk.get(2).unwrap_or(&0) as u32;
        let combined = (first << 16) | (second << 8) | third;
        for shift in [18, 12, 6, 0] {
            output.push(ALPHABET[((combined >> shift) & 0x3f) as usize] as char);
        }
        if chunk.len() < 3 {
            output.truncate(output.len() - (3 - chunk.len()));
            output.push_str(&"=".repeat(3 - chunk.len()));
        }
    }
    output
}

fn sam_request_body_with_prompt(jpeg: &[u8], prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "model": SAM_MODEL,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": prompt},
                {
                    "type": "input_image",
                    "image_url": format!("data:image/jpeg;base64,{}", base64_encode(jpeg)),
                },
            ],
        }],
        "stream": true,
        "metadata": {
            "include_confidence": "true",
            "mask_encoding": "one_bit",
        },
    })
}

fn collect_sam_output_text(value: &serde_json::Value, output: &mut String) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_sam_output_text(item, output);
            }
        }
        serde_json::Value::Object(map) => {
            if map.get("type").and_then(|value| value.as_str()) == Some("output_text") {
                if let Some(text) = map.get("text").and_then(|value| value.as_str()) {
                    output.push_str(text);
                }
            }
            for key in ["output", "content", "response"] {
                if let Some(child) = map.get(key) {
                    collect_sam_output_text(child, output);
                }
            }
        }
        _ => {}
    }
}

fn sam_api_error(value: &serde_json::Value) -> Option<String> {
    if value.get("error").is_some_and(|error| !error.is_null())
        || matches!(
            value["type"].as_str(),
            Some("response.failed" | "response.incomplete" | "error")
        )
        || matches!(
            value["status"].as_str(),
            Some("failed" | "incomplete" | "cancelled")
        )
    {
        return Some("SAM request did not complete successfully.".to_owned());
    }
    value.get("response").and_then(sam_api_error)
}

fn sam_output_text(body: &str) -> Result<String, String> {
    let trimmed = body.trim();
    if trimmed.starts_with('{') {
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|_| "Couldn't read the SAM response.".to_owned())?;
        if let Some(error) = sam_api_error(&value) {
            return Err(error);
        }
        if value["status"].as_str() != Some("completed") {
            return Err("SAM request did not complete successfully.".to_owned());
        }
        if let Some(text) = value["output_text"].as_str() {
            return Ok(text.to_owned());
        }
        let mut output = String::new();
        collect_sam_output_text(&value, &mut output);
        return Ok(output);
    }

    let mut output = String::new();
    let mut completed = false;
    let mut saw_delta = false;
    let mut finalized = false;
    let mut finalizers = std::collections::BTreeSet::new();
    let mut lane = None;
    for line in body.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            continue;
        }
        let value = serde_json::from_str::<serde_json::Value>(data)
            .map_err(|_| "Couldn't read the SAM response.".to_owned())?;
        if let Some(error) = sam_api_error(&value) {
            return Err(error);
        }
        let kind = value["type"].as_str().unwrap_or("");
        if kind.starts_with("response.refusal.") || value["part"]["type"] == "refusal" {
            return Err("SAM refused the request.".to_owned());
        }
        let text_event = kind == "response.output_text.delta"
            || kind == "response.output_text.done"
            || (kind == "response.content_part.done" && value["part"]["type"] == "output_text");
        if text_event {
            let identity = (
                value["item_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or("Missing SAM output identity")?
                    .to_owned(),
                value["output_index"]
                    .as_u64()
                    .ok_or("Missing SAM output index")?,
                value["content_index"]
                    .as_u64()
                    .ok_or("Missing SAM content index")?,
            );
            if completed || lane.as_ref().is_some_and(|expected| expected != &identity) {
                return Err("SAM returned conflicting output streams.".to_owned());
            }
            lane = Some(identity);
            if kind == "response.output_text.delta" {
                if finalized {
                    return Err("SAM sent text after finalization.".to_owned());
                }
                output.push_str(value["delta"].as_str().ok_or("Invalid SAM text delta")?);
                saw_delta = true;
            } else {
                let text = if kind == "response.output_text.done" {
                    &value["text"]
                } else {
                    &value["part"]["text"]
                };
                let text = text.as_str().ok_or("Missing SAM final text")?;
                if !finalizers.insert(kind.to_owned())
                    || ((saw_delta || finalized) && output != text)
                {
                    return Err("SAM final text conflicts with its evidence stream.".to_owned());
                }
                if !saw_delta && !finalized {
                    output.push_str(text);
                }
                finalized = true;
            }
        }
        if kind == "response.completed" {
            if completed || !finalized || value["response"]["status"].as_str() != Some("completed")
            {
                return Err("SAM evidence did not finish successfully.".to_owned());
            }
            completed = true;
        }
    }
    if completed {
        Ok(output)
    } else {
        Err("Couldn't read the SAM response.".to_owned())
    }
}

fn parse_sam_boxes(text: &str) -> Result<Vec<SamBox>, String> {
    let mut boxes = Vec::new();
    let mut search = 0_usize;
    while let Some(relative_start) = text[search..].find("<|box;") {
        let start = search + relative_start;
        let Some(relative_end) = text[start..].find("|>") else {
            break;
        };
        let end = start + relative_end;
        let mut id_start = start;
        while id_start > 0 && text.as_bytes()[id_start - 1].is_ascii_digit() {
            id_start -= 1;
        }
        let object_id = text[id_start..start].to_owned();
        let body = &text[start + "<|box;".len()..end];
        let fields: Vec<(&str, &str)> = body
            .split(';')
            .filter_map(|field| {
                let (key, value) = field.split_once('=')?;
                Some((key.trim(), value.trim()))
            })
            .collect();
        let field = |key: &str| {
            fields
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .map(|(_, value)| *value)
        };
        let parsed = (|| {
            Some(SamBox {
                object_id,
                x1: field("x1")?.parse().ok()?,
                y1: field("y1")?.parse().ok()?,
                x2: field("x2")?.parse().ok()?,
                y2: field("y2")?.parse().ok()?,
                width: field("w")?.parse().ok()?,
                height: field("h")?.parse().ok()?,
                confidence: match field("c") {
                    Some(value) => Some(
                        value
                            .parse::<f32>()
                            .ok()
                            .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))?,
                    ),
                    None => None,
                },
            })
        })();
        if let Some(sam_box) = parsed {
            if sam_box.width > 0
                && sam_box.height > 0
                && sam_box.x1 >= 0
                && sam_box.y1 >= 0
                && sam_box.x2 >= sam_box.x1
                && sam_box.y2 >= sam_box.y1
                && sam_box.x2 < sam_box.width as i64
                && sam_box.y2 < sam_box.height as i64
            {
                boxes.push(sam_box);
            }
        }
        search = end + 2;
    }
    if (!text.trim().is_empty() && boxes.is_empty()) || boxes.len() != text.matches("<|box").count()
    {
        Err("SAM returned malformed box evidence.".to_owned())
    } else {
        Ok(boxes)
    }
}

fn apply_sam_evidence(candidate: &mut SweepCandidate, boxes: &[SamBox], frame_time: f64) {
    if candidate.visual.is_none() {
        return;
    }
    let primary = boxes
        .iter()
        .max_by(|a, b| a.coverage().total_cmp(&b.coverage()));
    let centered = primary
        .map(|sam_box| {
            (0.3..=0.7).contains(&sam_box.center_x()) && (0.25..=0.75).contains(&sam_box.center_y())
        })
        .unwrap_or(false);
    if let Some(visual) = candidate.visual.as_mut() {
        visual.source = VisualEvidenceSource::Sam;
        visual.sam_status = SubjectSegmentationStatus::Ready;
        visual.sam_frame_time = Some(frame_time);
        visual.sam_error = None;
        visual.sam_object_count = Some(boxes.len());
        match primary {
            Some(sam_box) => {
                visual.sam_box_coverage = Some(sam_box.coverage());
                visual.sam_box_center_x = Some(sam_box.center_x());
                visual.sam_box_center_y = Some(sam_box.center_y());
                visual.sam_confidence = sam_box.confidence;
            }
            None => {
                visual.sam_box_coverage = Some(0.0);
                visual.sam_box_center_x = None;
                visual.sam_box_center_y = None;
                visual.sam_confidence = Some(0.0);
            }
        }
    }
    let Some(primary) = primary else {
        return;
    };
    let push_tag = |candidate: &mut SweepCandidate, tag: &str| {
        if !candidate.tags.iter().any(|existing| existing == tag) {
            candidate.tags.push(tag.to_owned());
        }
    };
    if centered {
        push_tag(candidate, "subject-centered");
    }
    if primary.coverage() >= 0.18 {
        push_tag(candidate, "subject-large");
    }
}

fn apply_sam_face_evidence(candidate: &mut SweepCandidate, boxes: &[SamBox]) {
    if candidate.visual.is_none() {
        return;
    }
    let primary = boxes
        .iter()
        .max_by(|a, b| a.coverage().total_cmp(&b.coverage()));
    if let Some(visual) = candidate.visual.as_mut() {
        visual.sam_face_count = Some(boxes.len());
        visual.face_present = Some(!boxes.is_empty());
        match primary {
            Some(face) => {
                visual.sam_face_box_coverage = Some(face.coverage());
                visual.sam_face_box_center_x = Some(face.center_x());
                visual.sam_face_box_center_y = Some(face.center_y());
                visual.sam_face_confidence = face.confidence;
            }
            None => {
                visual.sam_face_box_coverage = Some(0.0);
                visual.sam_face_box_center_x = None;
                visual.sam_face_box_center_y = None;
                visual.sam_face_confidence = Some(0.0);
            }
        }
    }
    if primary.is_some() {
        if !candidate.tags.iter().any(|tag| tag == "face-visible") {
            candidate.tags.push("face-visible".to_owned());
        }
    } else {
        candidate.tags.retain(|tag| tag != "face-visible");
    }
}

fn transcribe_asset(
    settings: &ChatSettings,
    info: &MediaInfo,
) -> Result<Vec<TranscriptSegment>, String> {
    if !info.has_audio || info.duration <= 0.0 {
        return Ok(Vec::new());
    }
    if !settings.can_call_model() {
        return Err("Add an API key for transcription.".to_owned());
    }
    let url = api_endpoint(settings, "asr/transcribe");
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(180)))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let mut transcript = Vec::new();
    let mut start = 0.0;
    while start < info.duration {
        let duration = ASR_CHUNK_SECONDS.min(info.duration - start);
        // Concurrent curations in one process must not share or delete a chunk.
        static CHUNK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temp = std::env::temp_dir().join(format!(
            "openreel-asr-{}-{}-{}.wav",
            std::process::id(),
            CHUNK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            (start * 1000.0) as u64
        ));
        let result = openreel_media::write_wav_24k(&info.path, start, duration, &temp)
            .map_err(|_| "Transcription audio could not be prepared.".to_owned())
            .and_then(|_| transcribe_chunk(&agent, &url, settings, &temp, start));
        let _ = std::fs::remove_file(&temp);
        transcript.extend(result?);
        start += duration;
    }
    Ok(transcript)
}

fn transcribe_chunk(
    agent: &ureq::Agent,
    url: &str,
    settings: &ChatSettings,
    wav_path: &std::path::Path,
    offset: f64,
) -> Result<Vec<TranscriptSegment>, String> {
    if std::fs::metadata(wav_path)
        .map(|meta| meta.len())
        .unwrap_or(0)
        > ASR_MAX_BYTES
    {
        return Err("Transcription chunk was too large.".to_owned());
    }
    let request = serde_json::json!({
        "mode": "DIARIZATION",
        "model": TRANSCRIBE_MODEL,
        "audioEncoding": "WAV",
    })
    .to_string();
    let audio = ureq::unversioned::multipart::Part::file(wav_path)
        .map_err(|_| "Transcription audio could not be opened.".to_owned())?
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|_| "Transcription audio could not be prepared.".to_owned())?;
    let form = ureq::unversioned::multipart::Form::new()
        .text("request", &request)
        .part("audio", audio);
    let mut call = agent.post(url);
    if !settings.api_key.trim().is_empty() {
        call = call.header(
            "Authorization",
            format!("Bearer {}", settings.api_key.trim()),
        );
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
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| "Couldn't read the transcript.".to_owned())?;
    Ok(parse_transcript_response(&value, offset))
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

fn parse_transcript_response(value: &serde_json::Value, offset: f64) -> Vec<TranscriptSegment> {
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
            let duration = value["audioDurationMs"].as_f64().unwrap_or(0.0) / 1000.0;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn centered_visual() -> SegmentVisualEvidence {
        SegmentVisualEvidence {
            source: VisualEvidenceSource::LocalProxy,
            sam_status: SubjectSegmentationStatus::Unavailable,
            subject_coverage: 0.25,
            subject_center_x: Some(0.5),
            subject_center_y: Some(0.45),
            subject_motion: 0.12,
            center_saliency: 0.8,
            edge_density: 0.3,
            luminance: 0.6,
            contrast: 0.2,
            skin_tone_ratio: 0.1,
            sam_mask_coverage: None,
            sam_mask_center_x: None,
            sam_mask_center_y: None,
            sam_box_coverage: None,
            sam_box_center_x: None,
            sam_box_center_y: None,
            sam_confidence: None,
            sam_object_count: None,
            sam_face_box_coverage: None,
            sam_face_box_center_x: None,
            sam_face_box_center_y: None,
            sam_face_confidence: None,
            sam_face_count: None,
            sam_frame_time: None,
            sam_error: None,
            face_present: None,
            expression: None,
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
        let result = run_sweep(&info, &[], &ChatSettings::default(), &mut |_| {});
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
        let turns = parse_transcript_response(&value, 10.0);
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
            &ChatSettings::default(),
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
    fn sam_request_and_streamed_boxes_parse_without_a_live_call() {
        let settings = ChatSettings::default();
        assert_eq!(
            sam_responses_url(&settings),
            "https://api.meta.ai/v1/responses"
        );
        assert_eq!(base64_encode(b"Eddie"), "RWRkaWU=");
        let body = sam_request_body_with_prompt(b"\xff\xd8fake-jpeg\xff\xd9", "person");
        assert_eq!(body["model"], SAM_MODEL);
        assert_eq!(body["metadata"]["include_confidence"], "true");
        assert_eq!(body["metadata"]["mask_encoding"], "one_bit");
        let image_url = body["input"][0]["content"][1]["image_url"]
            .as_str()
            .expect("data URL");
        assert!(image_url.starts_with("data:image/jpeg;base64,"));

        let sse = format!(
            "data: {}\n\ndata: {}\n\n",
            serde_json::json!({"type":"response.output_text.done", "item_id":"fixture", "output_index":0, "content_index":0,
                "text":"<0f>0<|box;x1=320;y1=90;x2=639;y2=449;w=1280;h=720;c=0.91|><|mask;x=0;y=0;c=0.91;data=10,10,!payload|>"}),
            serde_json::json!({"type":"response.completed", "response":{"status":"completed"}})
        );
        let text = sam_output_text(&sse).expect("streamed SAM text");
        let boxes = parse_sam_boxes(&text).expect("complete boxes");
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].object_id, "0");
        assert!((boxes[0].coverage() - 0.125).abs() < 0.001);
        assert!((boxes[0].center_x() - 0.375).abs() < 0.001);
        assert!((boxes[0].confidence.expect("confidence") - 0.91).abs() < 0.001);

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
        let norms = segment_norms(std::slice::from_ref(&segment));
        let mut candidate = candidate_from_segment(
            &info,
            0,
            &segment,
            &norms,
            80.0,
            "Speech.".to_owned(),
            Vec::new(),
            None,
        );
        apply_sam_evidence(&mut candidate, &boxes, 2.5);
        let visual = candidate.visual.as_ref().expect("visual evidence");
        assert_eq!(visual.source, VisualEvidenceSource::Sam);
        assert_eq!(visual.sam_status, SubjectSegmentationStatus::Ready);
        assert_eq!(visual.sam_object_count, Some(1));
        assert!(
            visual.sam_mask_coverage.is_none(),
            "box is not decoded mask coverage"
        );
        assert!(candidate.tags.iter().any(|tag| tag == "subject-centered"));
        assert_eq!(candidate.score, 80.0, "geometry is not editorial quality");

        candidate.transcript = vec![TranscriptSegment {
            start: 0.0,
            end: 4.0,
            speaker: Some("A".to_owned()),
            text: "Hello.".to_owned(),
            words: Vec::new(),
            session: None,
        }];
        apply_sam_face_evidence(
            &mut candidate,
            &[SamBox {
                object_id: "0".to_owned(),
                x1: 560,
                y1: 180,
                x2: 719,
                y2: 419,
                width: 1280,
                height: 720,
                confidence: Some(0.95),
            }],
        );
        let visual = candidate.visual.as_ref().expect("visual evidence");
        assert_eq!(visual.face_present, Some(true));
        assert_eq!(visual.sam_face_count, Some(1));
        assert!(candidate.tags.iter().any(|tag| tag == "face-visible"));
        assert!(candidate.talking_score() > 0.9);
        apply_sam_face_evidence(&mut candidate, &[]);
        assert_eq!(candidate.visual.as_ref().unwrap().face_present, Some(false));
        assert!(!candidate.tags.iter().any(|tag| tag == "face-visible"));
    }

    #[test]
    fn sam_partial_and_failed_responses_are_not_negative_detections() {
        for body in [
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"<0f>0<|box;x1=3\"}\n\n",
            "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"message\":\"private upstream detail\"}}}\n\ndata: [DONE]\n\n",
            "{\"status\":\"incomplete\",\"output_text\":\"<0f>\"}",
        ] {
            let result = sam_output_text(body);
            assert!(result.is_err(), "must not treat incomplete evidence as zero people: {result:?}");
            assert!(!result.unwrap_err().contains("private upstream detail"));
        }
        assert_eq!(
            sam_output_text(concat!(
                "data: {\"type\":\"response.output_text.done\",\"item_id\":\"fixture\",\"output_index\":0,\"content_index\":0,\"text\":\"\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
            )).unwrap(),
            ""
        );
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
        let turns = parse_transcript_response(&value, 10.0);
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
        let (category, tags) = classify_local(&base, true, 0.1, 0.3);
        assert_eq!(category, "talking");
        assert!(tags.iter().any(|t| t == "speech"));
        assert!(tags.iter().any(|t| t == "steady"));
        let (category, _) = classify_local(&base, false, 0.1, 0.6);
        assert_eq!(category, "reaction", "loud short peak reads as reaction");
        let (category, _) = classify_local(&base, false, 0.9, 0.2);
        assert_eq!(category, "action");
        let opener = SweepSegment {
            scene_change: 0.3,
            audio_peak: 0.2,
            ..base
        };
        let (category, _) = classify_local(&opener, false, 0.1, 0.2);
        assert_eq!(category, "establishing");
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
        let (category, tags) = classify_local(&segment, true, 0.1, 0.3);
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
            None,
        );
        assert!(candidate.talking_score() > 0.8);
        let visual = candidate.visual_json();
        assert_eq!(visual["source"], "local-proxy");
        assert_eq!(visual["samStatus"], "unavailable");
        assert!(visual["facePresent"].is_null());
        assert!(visual["expression"].is_null());
        assert!(visual["samMaskCoverage"].is_null());
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
                        None,
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
        assert!(is_music_bed(&bed));
        let (category, tags) = classify_local(&bed, false, 0.1, 0.9);
        assert_ne!(category, "reaction");
        assert!(tags.iter().any(|t| t == "music-bed"));
        assert!(!tags.iter().any(|t| t == "loud-peak"));
        let mut candidate = local_candidates(&test_info(), &[bed], &[]).pop().unwrap();
        candidate.reason = "No reaction here.".to_owned();
        assert!(!candidate.is_reaction());
        assert_eq!(candidate.reaction_score(), 0.0);
    }
}

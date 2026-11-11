//! Curation: the complete shot inventory of a file, its transcript, and
//! ranked local suggestions, all measured on this computer (transcription
//! may use a cloud provider). Agents and scripts make the editorial calls.

use std::sync::mpsc::{channel, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use openreel_media::{MediaInfo, MotionSample, SegmentVisualEvidence, SweepSegment};
use serde::{Deserialize, Serialize};

use super::media::AssetState;
use super::Editor;

mod align;
mod gpu;
mod speech;
mod store;
pub mod transcribe;
pub use speech::utterances;
pub(crate) use store::path_hash;
pub use store::{
    default_dir as default_curation_dir, load as load_curation, load_peaks, load_shots,
    load_transcript, save_peaks, save_shots,
};

/// Clip type stays distinct from the app-assigned `prime` quality tag.
pub const CURATOR_CATEGORIES: [&str; 5] =
    ["reaction", "talking", "action", "broll", "establishing"];

/// Controlled tag vocabulary. Local classification derives a subset from
/// measured signals; curations saved by the removed model reviewer can
/// carry the rest, so scripts can filter on stable words.
#[allow(dead_code)] // Read by the classifier's vocabulary test.
pub const CURATOR_TAGS: [&str; 27] = [
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

/// What transcription says about a range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Speech {
    Heard,
    /// Transcription ran and found none here.
    Silent,
    /// Transcription failed: uncertainty, not silence.
    Unknown,
}

/// A mixed music bed (finished montages, scored footage) keeps nearly
/// every audio bucket audible. Continuous speech does the same, so this
/// waveform hint never gates transcription.
fn continuous_audio(segment: &SweepSegment) -> bool {
    segment.audio_coverage >= 0.75 && segment.audio_energy >= 0.2
}

/// Continuous audio is a music bed only where a transcription that ran
/// heard no speech.
fn is_music_bed(segment: &SweepSegment, speech: Speech) -> bool {
    speech == Speech::Silent && continuous_audio(segment)
}

/// Continuous audio without heard speech may be a soundtrack, so its
/// loudness says nothing about this range and is left out of the score.
fn unattributed_audio(segment: &SweepSegment, speech: Speech) -> bool {
    speech != Speech::Heard && continuous_audio(segment)
}

/// Mean frame difference on the scale `stability` uses, so it compares
/// across files: 0 is a locked shot, 1 handheld shake or a busy picture.
fn motion_score(segment: &SweepSegment) -> f32 {
    (1.0 - segment.stability).clamp(0.0, 1.0)
}

/// Prime quota: at most this share of one asset's candidates, and
/// only at or above `PRIME_MIN_SCORE`.
const PRIME_SHARE: f32 = 0.2;
const PRIME_MIN_SCORE: f32 = 75.0;

/// Files curated at once. Shot analysis decodes every frame, so weak
/// machines stay at one.
fn parallel_sweeps() -> usize {
    thread::available_parallelism().map_or(1, |cores| (cores.get() / 4).clamp(1, 3))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSession {
    id: String,
    start: f64,
    end: Option<f64>,
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
pub struct CaptionChunk {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Source start time of each word in `text`, only from real word timing.
    pub words: Vec<f64>,
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
pub fn caption_chunks(segment: &TranscriptSegment) -> Vec<CaptionChunk> {
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
pub struct SweepCandidate {
    pub id: String,
    pub asset_path: String,
    pub asset_name: String,
    pub title: String,
    pub source_in: f64,
    pub duration: f64,
    pub score: f32,
    pub reason: String,
    pub stability: f32,
    pub audio_energy: f32,
    pub audio_peak: f32,
    /// One of `CURATOR_CATEGORIES`.
    pub category: String,
    /// Subset of `CURATOR_TAGS`.
    pub tags: Vec<String>,
    /// Quality subscores, 0..1: mean motion on one scale for every file,
    /// and a blended audio score (energy + peak) against the file's own
    /// loudest range.
    pub motion_score: f32,
    pub audio_score: f32,
    /// Baked-in music bed suspected: audio signals were discounted
    /// for this candidate and JS can filter on it.
    pub music_bed: bool,
    pub visual: Option<SegmentVisualEvidence>,
    pub has_video: bool,
    pub transcript: Vec<TranscriptSegment>,
    pub review: ClipReview,
    pub dismissed: bool,
}

impl SweepCandidate {
    pub fn end(&self) -> f64 {
        self.source_in + self.duration
    }

    pub fn reaction_score(&self) -> f32 {
        if !self.is_reaction() {
            return 0.0;
        }
        if self.review.status == ReviewStatus::Reviewed {
            return self.score / 100.0;
        }
        (0.6 * self.audio_peak + 0.4 * (self.audio_energy * 1.5).clamp(0.0, 1.0)).clamp(0.0, 1.0)
    }

    pub fn is_reaction(&self) -> bool {
        self.category == "reaction"
    }

    pub fn is_prime(&self) -> bool {
        self.tags.iter().any(|tag| tag == "prime")
    }

    /// Cross-modal talking evidence: transcript speech is primary;
    /// a centered, comparatively still foreground raises it. This
    /// does not claim lip movement or active-speaker detection.
    pub fn talking_score(&self) -> f32 {
        talking_score(!self.transcript.is_empty(), self.visual.as_ref())
    }

    pub fn visual_json(&self) -> serde_json::Value {
        visual_evidence_json(self.visual.as_ref(), self.talking_score())
    }

    pub fn review_json(&self) -> serde_json::Value {
        let mut review = self.review.json();
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

    pub fn transcript_text(&self) -> String {
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

/// Stable evidence shape for scripts. Every value is a measured proxy from
/// local frame analysis, not a person, face or expression detector.
pub fn visual_evidence_json(
    visual: Option<&SegmentVisualEvidence>,
    talking_score: f32,
) -> serde_json::Value {
    match visual {
        Some(visual) => serde_json::json!({
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
            "talkingScore": talking_score,
        }),
        None => serde_json::json!({
            "subjectCoverage": null,
            "subjectCenter": {"x": null, "y": null},
            "subjectMotion": null,
            "centerSaliency": null,
            "edgeDensity": null,
            "luminance": null,
            "contrast": null,
            "skinToneRatio": null,
            "talkingScore": talking_score,
        }),
    }
}

/// Whether a range was judged by anyone. Every new range is `Local`; the
/// others survive in curations saved by the removed model reviewer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewStatus {
    #[default]
    Local,
    Reviewed,
    NeedsReview,
    Rejected,
}

impl ReviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Reviewed => "reviewed",
            Self::NeedsReview => "needs-review",
            Self::Rejected => "rejected",
        }
    }
}

/// Fields the removed reviewer also saved (frames, SAM evidence, coverage,
/// usage) are ignored on load.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ClipReview {
    pub status: ReviewStatus,
    /// The model that reviewed a saved range, if one did.
    pub model: Option<String>,
}

impl ClipReview {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({"status": self.status.as_str(), "model": self.model})
    }
}

/// Transcript segment IDs in script state: `tx:<asset path>#<index>`.
pub fn transcript_segment_id(asset_path: &str, index: usize) -> String {
    format!("tx:{asset_path}#{index}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SweepStatus {
    NotStarted,
    Running,
    Ready,
    Failed,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SweepData {
    pub status: SweepStatus,
    pub note: String,
    pub transcript: Vec<TranscriptSegment>,
    pub candidates: Vec<SweepCandidate>,
    /// Complete source-ordered scene breakdown. Unlike the
    /// ranked `candidates` picks, these spans are meant to cover
    /// the whole asset so an edited video can be deconstructed.
    pub scene_clips: Vec<SweepCandidate>,
    /// False when this curation's transcription failed. Its ranges then
    /// carry no speech and no music-bed hint: what they hold is unknown.
    #[serde(default = "yes")]
    pub transcribed: bool,
}

fn yes() -> bool {
    true
}

impl Default for SweepData {
    fn default() -> Self {
        Self {
            status: SweepStatus::NotStarted,
            note: String::new(),
            transcript: Vec::new(),
            candidates: Vec::new(),
            scene_clips: Vec::new(),
            transcribed: true,
        }
    }
}

impl SweepData {
    /// Reviewed ranges override whole-shot hints only where they overlap.
    pub fn music_bed_overlap(&self, start: f64, end: f64) -> f64 {
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

pub enum SweepMsg {
    Progress { path: String, message: String },
    Done { path: String, data: SweepData },
    Failed { path: String, message: String },
}

impl Editor {
    /// Explicit user-triggered curation for one ready asset.
    /// Re-curation replaces the previous candidates and scenes. A saved
    /// transcript is kept; `eddie transcribe --force` replaces that.
    pub fn curate_asset(&mut self, path: String) {
        if self.queue_curation(path) {
            self.open_curator_gallery();
        }
    }

    /// `curate_asset` without opening the Gallery, for a request from
    /// outside the window. False when the asset is not ready or is already
    /// being curated.
    pub fn queue_curation(&mut self, path: String) -> bool {
        let Some(asset) = self.assets.iter_mut().find(|asset| asset.info.path == path) else {
            return false;
        };
        if asset.state != AssetState::Ready
            || asset.sweep.status == SweepStatus::Running
            || self.sweep_queue.contains(&path)
        {
            return false;
        }
        if asset.sweep.status != SweepStatus::NotStarted {
            asset.sweep = SweepData::default();
        }
        asset.sweep.status = SweepStatus::Running;
        asset.sweep.note = "Curating…".to_owned();
        self.sweep_queue.push(path);
        self.start_next_sweep();
        true
    }

    /// Queue every ready asset that has never been curated.
    pub fn curate_all_new(&mut self) {
        let paths: Vec<String> = self
            .assets
            .iter()
            .filter(|asset| {
                asset.state == AssetState::Ready && asset.sweep.status == SweepStatus::NotStarted
            })
            .map(|asset| asset.info.path.clone())
            .collect();
        for path in paths {
            self.curate_asset(path);
        }
    }

    /// Start queued curations up to `parallel_sweeps()` at once.
    fn start_next_sweep(&mut self) {
        while self.sweep_running.len() < parallel_sweeps() && !self.sweep_queue.is_empty() {
            let path = self.sweep_queue.remove(0);
            let Some(asset) = self.assets.iter().find(|asset| asset.info.path == path) else {
                continue;
            };
            let info = asset.info.clone();
            let peaks = asset.peaks.clone();
            let settings = self.settings.transcription(self.model_dir.as_deref());
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
            let wake = self.wake.clone();
            thread::spawn(move || {
                let progress = &mut |message: &str| {
                    let _ = tx.send(SweepMsg::Progress {
                        path: info.path.clone(),
                        message: message.to_owned(),
                    });
                    wake.wake();
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_sweep(&info, &peaks, &settings, store_dir.as_deref(), progress)
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
                wake.wake();
            });
        }
    }

    pub fn poll_sweeps(&mut self) {
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
                        let transcribed = data.transcribed;
                        asset.sweep = data;
                        self.status = format!(
                            "Curator found {pick_count} clip{} and {scene_count} scene{} in {}{}.",
                            if pick_count == 1 { "" } else { "s" },
                            if scene_count == 1 { "" } else { "s" },
                            asset.info.name,
                            if transcribed {
                                ""
                            } else {
                                "; transcription failed"
                            }
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
            self.frame_within(Duration::from_millis(180));
            return;
        }
        if self.sweep_running.is_empty() {
            self.sweep_rx = None;
            self.sweep_tx = None;
        }
        release_freed_memory();
        self.start_next_sweep();
        if self.sweep_rx.is_some() {
            self.frame_within(Duration::from_millis(180));
        }
    }

    pub fn visible_sweep_candidates(&self) -> Vec<(usize, usize)> {
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

    pub fn visible_scene_clips(&self) -> Vec<(usize, usize)> {
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

    pub fn dismiss_sweep_candidate(&mut self, candidate_id: &str) {
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

    pub fn add_sweep_candidate(&mut self, candidate_id: &str) {
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

    pub fn add_all_sweep_candidates(&mut self) {
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

    pub fn add_all_scene_clips(&mut self) {
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
    settings: &transcribe::TranscribeSettings,
    store_dir: Option<&std::path::Path>,
    progress: &mut dyn FnMut(&str),
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
    stage_notes.extend(picture_end_note(&motion, info.duration));
    // Waveform coverage cannot tell a soundtrack from continuous speech, so
    // it never suppresses transcription.
    let mut transcribed = true;
    let transcript = if info.has_audio {
        progress("Transcribing audio…");
        match transcribe::transcribe_cached(settings, store_dir, info, false, progress) {
            Ok(transcript) => {
                stage_notes.push(transcript.note);
                transcript.segments
            }
            Err(error) => {
                transcribed = false;
                stage_notes.push(format!("Transcription failed: {error}"));
                Vec::new()
            }
        }
    } else {
        stage_notes.push("Transcription: no audio stream.".to_owned());
        Vec::new()
    };
    let mut scene_clips = local_candidates(info, &segments, &transcript, transcribed);
    for (index, scene) in scene_clips.iter_mut().enumerate() {
        scene.id = format!("{}#scene-{index}", info.path);
    }
    let covered_seconds: f64 = segments.iter().map(|segment| segment.duration()).sum();
    let mut candidates = local_candidates(info, &segments, &transcript, transcribed);
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
        transcribed,
    })
}

/// A picture stream may end before the file does (sound runs on, or the
/// file is cut short). The last shot still reaches the file's end, so the
/// note says nothing was seen there.
fn picture_end_note(motion: &[MotionSample], duration: f64) -> Option<String> {
    let [.., before, last] = motion else {
        return None;
    };
    // A slow frame rate legitimately leaves a long last picture.
    let allowed = (3.0 * (last.time - before.time)).max(1.0);
    (duration - last.time > allowed).then(|| {
        format!(
            "Picture ends at {}; nothing after it was analyzed.",
            minute_second(last.time)
        )
    })
}

/// Audio is scored against the file's own loudest range, because a quiet
/// recording of speech is still that file's speech. Motion is not: it uses
/// one scale for every file (`motion_score`).
struct SegmentNorms {
    max_energy: f32,
    max_peak: f32,
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
    }
}

/// Local category + tags from measured signals only. Rule order
/// matters: reactions and speech are the strongest editorial
/// signals, then motion, then wide views without a subject, else b-roll.
/// `prime` is never returned here; `apply_prime` promotes after ranking.
fn classify_local(
    segment: &SweepSegment,
    speech: Speech,
    motion_score: f32,
    audio_score: f32,
) -> (String, Vec<String>) {
    let mut tags: Vec<String> = Vec::new();
    let mut tag = |name: &str| tags.push(name.to_owned());
    let has_speech = speech == Speech::Heard;
    if has_speech {
        tag("speech");
    }
    let music_bed = is_music_bed(segment, speech);
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
    // A reaction is a burst against a quieter background. Continuous sound,
    // whether a soundtrack or someone talking loudly, has no such burst.
    let reactionish = !continuous_audio(segment)
        && segment.audio_peak >= 0.75
        && audio_score >= 0.4
        && segment.duration() <= 7.0;
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
    } else if wide_without_subject {
        // Starting on a cut says nothing: every shot but a file's first does.
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

/// `transcribed` is false when transcription failed, so an empty
/// `transcript` is not evidence of silence.
fn local_candidates(
    info: &MediaInfo,
    segments: &[SweepSegment],
    transcript: &[TranscriptSegment],
    transcribed: bool,
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
            let speech = if !overlaps.is_empty() {
                Speech::Heard
            } else if transcribed {
                Speech::Silent
            } else {
                Speech::Unknown
            };
            let mut score = if unattributed_audio(segment, speech) {
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
            candidate_from_segment(
                info, index, segment, &norms, score, reason, overlaps, speech,
            )
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

#[allow(clippy::too_many_arguments)]
fn candidate_from_segment(
    info: &MediaInfo,
    index: usize,
    segment: &SweepSegment,
    norms: &SegmentNorms,
    score: f32,
    reason: String,
    transcript: Vec<TranscriptSegment>,
    speech: Speech,
) -> SweepCandidate {
    let motion_score = motion_score(segment);
    let audio_score = (0.6 * (segment.audio_energy / norms.max_energy)
        + 0.4 * (segment.audio_peak / norms.max_peak))
        .clamp(0.0, 1.0);
    let (category, tags) = classify_local(segment, speech, motion_score, audio_score);
    let music_bed = is_music_bed(segment, speech);
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

/// The turns spoken during a shot. Sparse turns are left out: they are
/// ambience, or words a speech model imagined over music ("Thank you." across
/// 30 s), and would make every shot under them a talking one.
fn overlapping_transcript(
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

pub fn minute_second(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
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
fn parse_transcript_response(
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

#[cfg(test)]
mod tests {
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
}

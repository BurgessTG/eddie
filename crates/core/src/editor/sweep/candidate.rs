//! A suggested range of a file: its category, tags, scores and review.

use openreel_media::SegmentVisualEvidence;
use serde::{Deserialize, Serialize};

use super::TranscriptSegment;

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

pub(super) fn visual_center_factor(visual: &SegmentVisualEvidence) -> f32 {
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

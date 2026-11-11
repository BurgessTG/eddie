//! Local classification: categories, tags and scores from measured
//! picture and sound, then the ranked suggestions.

use openreel_media::{MediaInfo, MotionSample, SweepSegment};

use super::candidate::visual_center_factor;
use super::transcript::overlapping_transcript;
use super::{ClipReview, ReviewStatus, SweepCandidate, TranscriptSegment};

/// What transcription says about a range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Speech {
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
pub(super) fn is_music_bed(segment: &SweepSegment, speech: Speech) -> bool {
    speech == Speech::Silent && continuous_audio(segment)
}

/// Continuous audio without heard speech may be a soundtrack, so its
/// loudness says nothing about this range and is left out of the score.
fn unattributed_audio(segment: &SweepSegment, speech: Speech) -> bool {
    speech != Speech::Heard && continuous_audio(segment)
}

/// Mean frame difference on the scale `stability` uses, so it compares
/// across files: 0 is a locked shot, 1 handheld shake or a busy picture.
pub(super) fn motion_score(segment: &SweepSegment) -> f32 {
    (1.0 - segment.stability).clamp(0.0, 1.0)
}

/// Prime quota: at most this share of one asset's candidates, and
/// only at or above `PRIME_MIN_SCORE`.
const PRIME_SHARE: f32 = 0.2;

const PRIME_MIN_SCORE: f32 = 75.0;

/// A picture stream may end before the file does (sound runs on, or the
/// file is cut short). The last shot still reaches the file's end, so the
/// note says nothing was seen there.
pub(super) fn picture_end_note(motion: &[MotionSample], duration: f64) -> Option<String> {
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
pub(super) struct SegmentNorms {
    max_energy: f32,
    max_peak: f32,
}

pub(super) fn segment_norms(segments: &[SweepSegment]) -> SegmentNorms {
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
pub(super) fn classify_local(
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
pub(super) fn apply_prime(candidates: &mut [SweepCandidate]) {
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
pub(super) fn local_candidates(
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
pub(super) fn candidate_from_segment(
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

pub fn minute_second(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

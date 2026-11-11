//! What a script reads: the timeline's clips and the whole editor state
//! as JSON (`state()`).

use openreel_engine::{ClipKind, Orientation};

use super::super::media::{AssetState, BinAsset};
use super::super::{inspect, sweep};
use crate::Editor;

impl Editor {
    /// The timeline's clips as scripts read them (`state().clips`).
    pub fn script_clips(&self) -> Vec<serde_json::Value> {
        self.timeline
            .clips
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "name": c.name,
                    "kind": match c.kind {
                        ClipKind::Video => "video",
                        ClipKind::Audio => "audio",
                        ClipKind::Title => "title",
                    },
                    "track": c.track,
                    "start": c.start,
                    "duration": c.duration,
                    "sourceIn": c.source_in,
                    "source": c.source,
                    "opacity": c.opacity,
                    "volume": c.volume,
                    "fadeIn": c.fade_in,
                    "fadeOut": c.fade_out,
                    "duck": c.duck,
                    "cleanVoice": c.clean_voice,
                    "smoothFades": c.smooth_fades,
                    "embeddedAudio": c.embedded_audio,
                    "linkedClip": c.linked_clip,
                    "titleStyle": c.title_style.as_str(),
                })
            })
            .collect()
    }

    pub fn script_state_json(&self) -> String {
        let mut sweep_candidates: Vec<serde_json::Value> = self
            .assets
            .iter()
            .flat_map(|asset| asset.sweep.candidates.iter())
            .filter(|candidate| !candidate.dismissed)
            .map(candidate_json)
            .collect();
        sweep_candidates.sort_by(|a, b| {
            b["score"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&a["score"].as_f64().unwrap_or(0.0))
        });
        let scene_clips: Vec<serde_json::Value> = self
            .assets
            .iter()
            .flat_map(|asset| asset.sweep.scene_clips.iter())
            .map(candidate_json)
            .collect();
        let transcript_segments: Vec<serde_json::Value> =
            self.assets.iter().flat_map(segments_json).collect();
        serde_json::json!({
            "playhead": self.playhead,
            "playing": self.playing,
            "selected": self.selected,
            "duration": self.timeline.end(),
            "fps": self.timeline.fps,
            "magnet": self.viewport.magnet,
            "musicBedMuted": self.mute_music_beds,
            "format": self.timeline.orientation.as_str(),
            "clips": self.script_clips(),
            "assets": self.assets.iter().map(asset_json).collect::<Vec<_>>(),
            "sweepCandidates": sweep_candidates,
            "sceneClips": scene_clips,
            "transcriptSegments": transcript_segments,
            "inspection": inspect::inspect(&self.timeline, &self.assets),
            "audio": self.audio_report(),
        })
        .to_string()
    }
}

/// One file in the bin (`state().assets[]`).
fn asset_json(a: &BinAsset) -> serde_json::Value {
    let peaks_per_sec = if a.info.duration > 0.0 && !a.peaks.is_empty() {
        openreel_media::WAVEFORM_PEAKS_PER_SEC as f64
    } else {
        0.0
    };
    let silence_ranges: Vec<serde_json::Value> = if peaks_per_sec > 0.0 {
        let threshold = openreel_media::silence_threshold(&a.peaks);
        openreel_media::silence_ranges(&a.peaks, peaks_per_sec, threshold, 0.35)
            .iter()
            .map(|range| {
                serde_json::json!({
                    "start": range.start,
                    "end": range.end,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    serde_json::json!({
        "name": a.info.name,
        "path": a.info.path,
        "duration": a.info.duration,
        "width": a.info.width,
        "height": a.info.height,
        "hasVideo": a.info.has_video,
        "hasAudio": a.info.has_audio,
        "orientation": Orientation::of(a.info.width, a.info.height).as_str(),
        "ready": a.state == AssetState::Ready,
        "waveformPeaksPerSec": peaks_per_sec,
        "silenceRanges": silence_ranges,
        "tempo": a.beats.as_ref().map(|grid| (grid.bpm * 10.0).round() / 10.0),
        "utterances": sweep::utterances(&a.sweep.transcript)
            .into_iter()
            .map(|u| serde_json::json!({"start": u.start, "end": u.end, "text": u.text}))
            .collect::<Vec<_>>(),
        "beats": a.beats.as_ref().map(|grid| {
            grid.beats
                .iter()
                .map(|t| (t * 1000.0).round() / 1000.0)
                .collect::<Vec<_>>()
        }),
    })
}

/// One curated range: a ranked suggestion (`sweepCandidates()`) or a
/// detected shot (`sceneClips()`).
fn candidate_json(candidate: &sweep::SweepCandidate) -> serde_json::Value {
    serde_json::json!({
        "id": candidate.id,
        "asset": candidate.asset_path,
        "assetName": candidate.asset_name,
        "title": candidate.title,
        "sourceIn": candidate.source_in,
        "duration": candidate.duration,
        "score": candidate.score,
        "reason": candidate.reason,
        "stability": candidate.stability,
        "audioEnergy": candidate.audio_energy,
        "audioPeak": candidate.audio_peak,
        "musicBed": candidate.music_bed,
        "isReaction": candidate.is_reaction(),
        "reactionScore": candidate.reaction_score(),
        "category": candidate.category,
        "tags": candidate.tags,
        "quality": {
            "stability": candidate.stability,
            "audio": candidate.audio_score,
            "motion": candidate.motion_score,
        },
        "hasVideo": candidate.has_video,
        "transcript": candidate.transcript_text(),
        "talkingScore": candidate.talking_score(),
        "visual": candidate.visual_json(),
        "review": candidate.review_json(),
    })
}

/// A file's transcript (`state().transcriptSegments`). Segments are
/// normally turn-level; provider word timings are preserved when present.
/// IDs match the pinned digest so requests like "the part where he talks
/// about pricing" resolve against real ranges.
fn segments_json(asset: &BinAsset) -> Vec<serde_json::Value> {
    asset
        .sweep
        .transcript
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            let words: Vec<serde_json::Value> = segment
                .words
                .iter()
                .map(|word| {
                    serde_json::json!({
                        "start": word.start,
                        "end": word.end,
                        "text": word.text,
                        "confidence": word.confidence,
                        "estimated": word.estimated,
                    })
                })
                .collect();
            serde_json::json!({
                "id": sweep::transcript_segment_id(&asset.info.path, index),
                "asset": asset.info.path,
                "assetName": asset.info.name,
                "start": segment.start,
                "end": segment.end,
                "speaker": segment.speaker,
                "text": segment.text,
                "timingPrecision": segment.timing_precision(),
                "asrSession": segment.session_json(),
                "words": words,
            })
        })
        .collect()
}

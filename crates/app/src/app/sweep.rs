//! Import-time QC sweep: local segment analysis, Muse Voice Transcribe,
//! and a Muse ranking pass with a deterministic local fallback.
//!
//! SAM 3.1 subject presence is intentionally not wired in this slice: there
//! is no existing local/API integration in the app, and stability, audio
//! energy, scene boundaries, and speech already produce useful candidates.

use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use eframe::egui;
use openreel_media::{MediaInfo, SweepSegment};

use super::chat::{api_error_for_status, chat_content, extract_code, post_chat_completion};
use super::media::AssetState;
use super::{ChatSettings, EditorApp};

const TRANSCRIBE_MODEL: &str = "muse-voice-transcribe-1.0";
const MAX_CANDIDATES_PER_ASSET: usize = 8;

/// Curator categories. Every candidate gets exactly one. `prime`
/// is scarce by construction (see `apply_prime`); the model never
/// assigns it directly.
pub(super) const CURATOR_CATEGORIES: [&str; 6] = [
    "prime",
    "reaction",
    "talking",
    "action",
    "broll",
    "establishing",
];

/// Controlled tag vocabulary. Local classification derives these
/// from measured signals; model tags are validated against the
/// same list so chat prompts can filter on stable words.
pub(super) const CURATOR_TAGS: [&str; 11] = [
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
];

/// A mixed music bed (finished montages, scored footage) keeps
/// nearly every audio bucket audible. Speech breathes between
/// phrases. Above this coverage the audio is bed, not signal:
/// peaks stop meaning reactions and energy stops meaning quality.
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

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TranscriptSegment {
    pub(super) start: f64,
    pub(super) end: f64,
    pub(super) speaker: Option<String>,
    pub(super) text: String,
}

#[derive(Clone)]
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
    pub(super) has_video: bool,
    pub(super) transcript: Vec<TranscriptSegment>,
    pub(super) dismissed: bool,
}

impl SweepCandidate {
    pub(super) fn end(&self) -> f64 {
        self.source_in + self.duration
    }

    /// Reaction heuristic over data the sweep already computed: a
    /// loud peak on a short, energetic segment. Used by the local
    /// "Reactions first" intent and exposed to the model so chat
    /// requests like "build it from the reaction shots" resolve
    /// against the same definition.
    pub(super) fn reaction_score(&self) -> f32 {
        (0.6 * self.audio_peak + 0.4 * (self.audio_energy * 1.5).clamp(0.0, 1.0)).clamp(0.0, 1.0)
    }

    pub(super) fn is_reaction(&self) -> bool {
        self.reaction_score() >= 0.45 || self.reason.to_lowercase().contains("reaction")
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SweepStatus {
    NotStarted,
    Running,
    Ready,
    Failed,
}

#[derive(Clone)]
pub(super) struct SweepData {
    pub(super) status: SweepStatus,
    pub(super) note: String,
    pub(super) transcript: Vec<TranscriptSegment>,
    pub(super) candidates: Vec<SweepCandidate>,
}

impl Default for SweepData {
    fn default() -> Self {
        Self {
            status: SweepStatus::NotStarted,
            note: String::new(),
            transcript: Vec::new(),
            candidates: Vec::new(),
        }
    }
}

pub(super) enum SweepMsg {
    Done {
        path: String,
        transcript: Vec<TranscriptSegment>,
        candidates: Vec<SweepCandidate>,
        note: String,
    },
    Failed {
        path: String,
        message: String,
    },
}

impl EditorApp {
    pub(super) fn queue_sweep(&mut self, ctx: &egui::Context, path: String) {
        let Some(asset) = self.assets.iter_mut().find(|asset| asset.info.path == path) else {
            return;
        };
        if asset.state != AssetState::Ready || asset.sweep.status != SweepStatus::NotStarted {
            return;
        }
        asset.sweep.status = SweepStatus::Running;
        asset.sweep.note = "Sweeping…".to_owned();
        if !self.sweep_queue.contains(&path) {
            self.sweep_queue.push(path);
        }
        self.start_next_sweep(ctx);
    }

    fn start_next_sweep(&mut self, ctx: &egui::Context) {
        if self.sweep_rx.is_some() || self.sweep_queue.is_empty() {
            return;
        }
        let path = self.sweep_queue.remove(0);
        let Some(asset) = self.assets.iter().find(|asset| asset.info.path == path) else {
            self.start_next_sweep(ctx);
            return;
        };
        let info = asset.info.clone();
        let peaks = asset.peaks.clone();
        let settings = self.chat_settings.clone();
        let (tx, rx) = channel::<SweepMsg>();
        self.sweep_rx = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let msg = match run_sweep(&info, &peaks, &settings) {
                Ok((transcript, candidates, note)) => SweepMsg::Done {
                    path: info.path.clone(),
                    transcript,
                    candidates,
                    note,
                },
                Err(message) => SweepMsg::Failed {
                    path: info.path.clone(),
                    message,
                },
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    pub(super) fn poll_sweeps(&mut self, ctx: &egui::Context) {
        let Some(msg) = self.sweep_rx.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            if self.sweep_rx.is_some() {
                ctx.request_repaint_after(Duration::from_millis(180));
            }
            return;
        };
        self.sweep_rx = None;
        match msg {
            SweepMsg::Done {
                path,
                transcript,
                candidates,
                note,
            } => {
                if let Some(asset) = self.assets.iter_mut().find(|asset| asset.info.path == path) {
                    let count = candidates.len();
                    asset.sweep.status = SweepStatus::Ready;
                    asset.sweep.note = note;
                    asset.sweep.transcript = transcript;
                    asset.sweep.candidates = candidates;
                    self.status = if count == 1 {
                        format!("Sweep found 1 clip in {}.", asset.info.name)
                    } else {
                        format!("Sweep found {count} clips in {}.", asset.info.name)
                    };
                }
            }
            SweepMsg::Failed { path, message } => {
                if let Some(asset) = self.assets.iter_mut().find(|asset| asset.info.path == path) {
                    asset.sweep.status = SweepStatus::Failed;
                    asset.sweep.note = message.clone();
                    self.status = message;
                }
            }
        }
        // The sweep changed: this is a pinned-summary refresh point
        // (the only other one is chat submit, as a safety net).
        self.refresh_pinned_summary();
        self.start_next_sweep(ctx);
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

    pub(super) fn dismiss_sweep_candidate(&mut self, candidate_id: &str) {
        for asset in &mut self.assets {
            if let Some(candidate) = asset
                .sweep
                .candidates
                .iter_mut()
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
            .flat_map(|asset| asset.sweep.candidates.iter())
            .find(|candidate| candidate.id == candidate_id && !candidate.dismissed)
            .cloned();
        if let Some(candidate) = candidate {
            let at = self.timeline.end();
            self.add_candidate_at(&candidate, at);
        }
    }

    pub(super) fn add_all_sweep_candidates(&mut self) {
        let mut candidates: Vec<SweepCandidate> = self
            .assets
            .iter()
            .flat_map(|asset| asset.sweep.candidates.iter())
            .filter(|candidate| !candidate.dismissed)
            .cloned()
            .collect();
        candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
        if candidates.is_empty() {
            self.status = "No sweep clips to add.".to_owned();
            return;
        }
        let mut cursor = self.timeline.end();
        for candidate in &candidates {
            self.add_candidate_at(candidate, cursor);
            cursor += candidate.duration;
        }
        self.status = if candidates.len() == 1 {
            "Added 1 sweep clip.".to_owned()
        } else {
            format!("Added {} sweep clips.", candidates.len())
        };
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

fn run_sweep(
    info: &MediaInfo,
    peaks: &[f32],
    settings: &ChatSettings,
) -> Result<(Vec<TranscriptSegment>, Vec<SweepCandidate>, String), String> {
    let motion = if info.has_video {
        openreel_media::motion_samples(&info.path, info).unwrap_or_default()
    } else {
        Vec::new()
    };
    let segments = openreel_media::build_segments(info.duration, &motion, peaks, 20);
    if segments.is_empty() {
        return Err(format!("Sweep found no usable segments in {}.", info.name));
    }

    let music_dominant = !segments.is_empty()
        && segments
            .iter()
            .filter(|segment| is_music_bed(segment))
            .count() as f32
            >= segments.len() as f32 * 0.8;
    let (transcript, transcript_note) = if music_dominant {
        // Lyrics would pollute speech logic and burn credits.
        (
            Vec::new(),
            Some("Music bed throughout; skipped transcription and discounted audio.".to_owned()),
        )
    } else {
        match transcribe_asset(settings, info) {
            Ok(transcript) => (transcript, None),
            Err(note) => (Vec::new(), Some(note)),
        }
    };

    let local = local_candidates(info, &segments, &transcript);
    let (mut candidates, mut note) = if settings.can_call_model() {
        match rank_with_model(settings, info, &segments, &transcript) {
            Ok(ranked) if !ranked.is_empty() => (ranked, "Model-ranked sweep ready.".to_owned()),
            Ok(_) | Err(_) => (local, "Locally ranked sweep ready.".to_owned()),
        }
    } else {
        (local, "Locally ranked sweep ready.".to_owned())
    };
    if let Some(transcript_note) = transcript_note {
        note = format!("{note} {transcript_note}");
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    candidates.truncate(MAX_CANDIDATES_PER_ASSET);
    // Prime share is of the clips the editor actually sees.
    apply_prime(&mut candidates);
    Ok((transcript, candidates, note))
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
    let reactionish =
        !music_bed && segment.audio_peak >= 0.75 && audio_score >= 0.4 && segment.duration() <= 7.0;
    let category = if reactionish {
        "reaction"
    } else if has_speech {
        "talking"
    } else if motion_score >= 0.65 {
        "action"
    } else if segment.scene_change >= 0.18 && segment.stability >= 0.6 {
        "establishing"
    } else {
        "broll"
    };
    (category.to_owned(), tags)
}

/// Promote the scarce top slice to `prime`: at most `PRIME_SHARE`
/// of the (score-sorted) candidates and only at `PRIME_MIN_SCORE`.
fn apply_prime(candidates: &mut [SweepCandidate]) {
    let quota = (candidates.len() as f32 * PRIME_SHARE).ceil() as usize;
    for candidate in candidates.iter_mut().take(quota) {
        if candidate.score >= PRIME_MIN_SCORE {
            candidate.category = "prime".to_owned();
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
        return if segment.stability >= 0.65 {
            format!("Steady speech at {at} — “{excerpt}”.")
        } else {
            format!("Speech lands at {at} — “{excerpt}”.")
        };
    }
    if !info.has_video {
        return format!("Lively audio around {at}, with a clean in and out.");
    }
    if segment.stability >= 0.75 && segment.audio_energy >= 0.25 {
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
    let (category, tags) = match model_call {
        Some((category, tags))
            if CURATOR_CATEGORIES.contains(&category.as_str()) && category != "prime" =>
        {
            let tags: Vec<String> = tags
                .into_iter()
                .filter(|tag| CURATOR_TAGS.contains(&tag.as_str()))
                .collect();
            if tags.is_empty() {
                (category, local_tags)
            } else {
                (category, tags)
            }
        }
        _ => (local_category, local_tags),
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
        music_bed: is_music_bed(segment),
        has_video: info.has_video,
        transcript,
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

fn rank_with_model(
    settings: &ChatSettings,
    info: &MediaInfo,
    segments: &[SweepSegment],
    transcript: &[TranscriptSegment],
) -> Result<Vec<SweepCandidate>, String> {
    let segment_json: Vec<serde_json::Value> = segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            let speech = overlapping_transcript(segment, transcript)
                .iter()
                .map(|turn| turn.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            serde_json::json!({
                "segment": index,
                "start": segment.start,
                "end": segment.end,
                "duration": segment.duration(),
                "stability": segment.stability,
                "meanMotion": segment.mean_motion,
                "audioEnergy": segment.audio_energy,
                "audioPeak": segment.audio_peak,
                "sceneChange": segment.scene_change,
                "audioCoverage": segment.audio_coverage,
                "musicBed": is_music_bed(segment),
                "transcript": speech,
            })
        })
        .collect();
    let body = serde_json::json!({
        "model": settings.model,
        "temperature": 0.1,
        "stream": false,
        "messages": [
            {
                "role": "developer",
                "content": "You are the curator for a human video editor. Rank and classify segments so the editor can prompt cuts later (for example reactions first, or prime shots only). Prefer stable footage, a clear subject or speech, reaction/energy peaks, and clean in/out points. Category is exactly one of reaction, talking, action, broll, establishing (never prime; the app promotes its own top picks). Tags come only from: speech, quiet, loud-peak, steady, shaky, fast-motion, slow, scene-open, long-take, short, music-bed. When musicBed is true the audio is baked-in music, not scene sound: never call it a reaction from audio and judge it on visuals. Return ONLY a JSON array, no markdown: [{\"segment\":0,\"score\":87,\"reason\":\"one short plain-language reason\",\"category\":\"talking\",\"tags\":[\"speech\",\"steady\"]}]. Use each segment at most once.",
            },
            {
                "role": "user",
                "content": format!(
                    "Rank these sweep segments for {}:\n{}",
                    info.name,
                    serde_json::to_string_pretty(&segment_json).unwrap_or_default()
                ),
            },
        ],
    });
    let response = post_chat_completion(settings, &body)?;
    parse_ranking_response(&response, info, segments, transcript)
}

fn parse_ranking_response(
    response: &serde_json::Value,
    info: &MediaInfo,
    segments: &[SweepSegment],
    transcript: &[TranscriptSegment],
) -> Result<Vec<SweepCandidate>, String> {
    let content = extract_code(&chat_content(response));
    let value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|_| "Couldn't read the sweep ranking.".to_owned())?;
    let rows = value
        .as_array()
        .or_else(|| value["candidates"].as_array())
        .or_else(|| value["rankings"].as_array())
        .ok_or_else(|| "Couldn't read the sweep ranking.".to_owned())?;
    let norms = segment_norms(segments);
    let mut seen = Vec::new();
    let mut candidates = Vec::new();
    for row in rows {
        let Some(index) = row["segment"].as_u64().map(|value| value as usize) else {
            continue;
        };
        if seen.contains(&index) {
            continue;
        }
        let Some(segment) = segments.get(index) else {
            continue;
        };
        let Some(reason) = row["reason"]
            .as_str()
            .filter(|text| !text.trim().is_empty())
        else {
            continue;
        };
        seen.push(index);
        let model_call = row["category"].as_str().map(|category| {
            let tags = row["tags"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|tag| tag.as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            (category.trim().to_lowercase(), tags)
        });
        candidates.push(candidate_from_segment(
            info,
            index,
            segment,
            &norms,
            row["score"].as_f64().unwrap_or(50.0) as f32,
            reason.trim().to_owned(),
            overlapping_transcript(segment, transcript),
            model_call,
        ));
    }
    if candidates.is_empty() {
        Err("Couldn't read the sweep ranking.".to_owned())
    } else {
        Ok(candidates)
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
    let base = settings.base_url.trim().trim_end_matches('/');
    let url = if base.ends_with("/asr/transcribe") {
        base.to_owned()
    } else {
        format!("{base}/asr/transcribe")
    };
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(180)))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let mut transcript = Vec::new();
    let mut start = 0.0;
    while start < info.duration {
        let duration = ASR_CHUNK_SECONDS.min(info.duration - start);
        let temp = std::env::temp_dir().join(format!(
            "openreel-asr-{}-{}.wav",
            std::process::id(),
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

fn parse_transcript_response(value: &serde_json::Value, offset: f64) -> Vec<TranscriptSegment> {
    let mut turns = Vec::new();
    if let Some(rows) = value["turns"].as_array() {
        for row in rows {
            let Some(text) = row["transcript"]
                .as_str()
                .filter(|text| !text.trim().is_empty())
            else {
                continue;
            };
            let speaker = match &row["speaker"] {
                serde_json::Value::String(speaker) if !speaker.is_empty() => Some(speaker.clone()),
                serde_json::Value::Number(speaker) => Some(speaker.to_string()),
                _ => None,
            };
            turns.push(TranscriptSegment {
                start: offset + row["startMs"].as_f64().unwrap_or(0.0) / 1000.0,
                end: offset + row["endMs"].as_f64().unwrap_or(0.0) / 1000.0,
                speaker,
                text: text.trim().to_owned(),
            });
        }
    }
    if turns.is_empty() {
        if let Some(text) = value["transcript"]
            .as_str()
            .filter(|text| !text.trim().is_empty())
        {
            let duration = value["audioDurationMs"].as_f64().unwrap_or(0.0) / 1000.0;
            turns.push(TranscriptSegment {
                start: offset,
                end: offset + duration,
                speaker: None,
                text: text.trim().to_owned(),
            });
        }
    }
    turns
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let primes: Vec<_> = candidates
            .iter()
            .filter(|c| c.category == "prime")
            .collect();
        assert_eq!(primes.len(), 2, "top 20 percent of 10 at or over 75");
        assert!(primes.iter().all(|c| c.score >= 90.0));
    }

    #[test]
    fn model_prime_and_unknown_tags_fall_back_locally() {
        let info = test_info();
        let segments = vec![SweepSegment {
            start: 0.0,
            end: 5.0,
            mean_motion: 0.01,
            peak_motion: 0.02,
            stability: 0.9,
            audio_energy: 0.5,
            audio_peak: 0.8,
            audio_coverage: 0.5,
            scene_change: 0.0,
        }];
        let response = serde_json::json!({
            "choices": [{"message": {"content": "[{\"segment\":0,\"score\":91,\"reason\":\"Great.\",\"category\":\"prime\",\"tags\":[\"steady\",\"bogus\"]}]"}}]
        });
        let ranked = parse_ranking_response(&response, &info, &segments, &[]).unwrap();
        assert_ne!(ranked[0].category, "prime", "model may not assign prime");
        assert!(ranked[0]
            .tags
            .iter()
            .all(|t| CURATOR_TAGS.contains(&t.as_str())));
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
        };
        assert!(is_music_bed(&bed));
        let (category, tags) = classify_local(&bed, false, 0.1, 0.9);
        assert_ne!(category, "reaction");
        assert!(tags.iter().any(|t| t == "music-bed"));
        assert!(!tags.iter().any(|t| t == "loud-peak"));
    }

    #[test]
    fn parses_model_ranking_and_ignores_bad_rows() {
        let info = test_info();
        let segments = vec![SweepSegment {
            start: 0.0,
            end: 5.0,
            mean_motion: 0.01,
            peak_motion: 0.02,
            stability: 0.9,
            audio_energy: 0.5,
            audio_peak: 0.8,
            audio_coverage: 0.5,
            scene_change: 0.0,
        }];
        let response = serde_json::json!({
            "choices": [{"message": {"content": "[{\"segment\":99,\"score\":1,\"reason\":\"bad\"},{\"segment\":0,\"score\":91,\"reason\":\"Steady and clear.\"}]"}}]
        });
        let ranked = parse_ranking_response(&response, &info, &segments, &[]).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].reason, "Steady and clear.");
    }
}

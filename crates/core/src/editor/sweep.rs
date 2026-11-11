//! Curation: the complete shot inventory of a file, its transcript, and
//! ranked local suggestions, all measured on this computer (transcription
//! may use a cloud provider). Agents and scripts make the editorial calls.

use std::sync::mpsc::{channel, TryRecvError};
use std::thread;
use std::time::Duration;

use openreel_media::MediaInfo;
use serde::{Deserialize, Serialize};

use super::media::AssetState;
use super::Editor;
use classify::{apply_prime, local_candidates, picture_end_note};

mod align;
mod candidate;
mod captions;
mod classify;
pub(crate) mod gpu;
mod speech;
mod store;
pub mod transcribe;
mod transcript;
pub use candidate::{
    visual_evidence_json, ClipReview, ReviewStatus, SweepCandidate, CURATOR_CATEGORIES,
    CURATOR_TAGS,
};
pub use captions::{caption_chunks, CaptionChunk};
pub use classify::minute_second;
pub use speech::utterances;
pub(crate) use store::path_hash;
pub use store::{
    default_dir as default_curation_dir, load as load_curation, load_peaks, load_shots,
    load_transcript, save_peaks, save_shots,
};
pub use transcript::{
    ends_sentence, transcript_segment_id, TranscriptSegment, TranscriptSession, TranscriptWord,
};

/// whisper.cpp hears mono sound at this rate.
const SAMPLE_RATE: usize = 16_000;

/// Files curated at once. Shot analysis decodes every frame, so weak
/// machines stay at one.
fn parallel_sweeps() -> usize {
    thread::available_parallelism().map_or(1, |cores| (cores.get() / 4).clamp(1, 3))
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

#[cfg(test)]
mod tests;

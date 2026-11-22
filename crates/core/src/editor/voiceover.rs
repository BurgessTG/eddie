//! Recording a voiceover, as in CapCut: Record counts down three seconds,
//! then the microphone writes a take while the timeline plays from the
//! playhead, silent unless Settings says to hear it. Record, Space or Esc
//! stops. The take is a WAV beside the project; it is imported and placed
//! on a free audio lane where the playhead was when its first sample was
//! captured, so what was said lines up with what was on screen.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use openreel_engine::ClipKind;
use openreel_media::{Microphone, Take};

use super::Editor;

/// Seconds between pressing Record and the take starting.
pub const COUNTDOWN: f64 = 3.0;

/// How far the meter falls in a second after a peak (about 26 dB).
const METER_FALL: f32 = 0.05;

/// The microphone, while the Voiceover panel is open or a take runs.
pub struct Voiceover {
    mic: Microphone,
    /// The input level for a meter, 0 to 1: peaks, falling back slowly.
    pub meter: f32,
    phase: Phase,
}

enum Phase {
    Idle,
    /// The take starts when the editor's clock reaches `at`.
    Countdown {
        at: f64,
        left: f64,
    },
    Recording(Recording),
}

struct Recording {
    take: Take,
    /// The playhead when the take started.
    start: f64,
    /// An instant and the timeline time heard then, once playback is under
    /// way, so the take's first sample can be put on the timeline.
    anchor: Option<(Instant, f64)>,
}

/// A finished take, waiting for its import before it is placed.
pub struct PendingTake {
    pub path: String,
    pub name: String,
    /// Where it goes, and the part of its start that falls before 0.
    pub start: f64,
    pub source_in: f64,
    pub seconds: f64,
    /// Its waveform, drawn until the clip replaces it.
    pub peaks: Vec<f32>,
    project: Option<PathBuf>,
}

impl Voiceover {
    /// What the microphone is, as the panel names it.
    pub fn microphone(&self) -> &str {
        &self.mic.name
    }

    /// Whole seconds left of the countdown, while it runs.
    pub fn countdown(&self) -> Option<u32> {
        match self.phase {
            Phase::Countdown { left, .. } => Some(left.ceil().max(1.0) as u32),
            _ => None,
        }
    }

    /// The take being recorded and where it starts on the timeline.
    pub fn recording(&self) -> Option<(f64, &Take)> {
        match &self.phase {
            Phase::Recording(recording) => Some((recording.start, &recording.take)),
            _ => None,
        }
    }

    /// Counting down or recording.
    pub fn busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }
}

/// Where a take's first sample goes on the timeline: the time being heard
/// when it was captured. Without a capture time or a moving playhead it
/// goes where the take started.
pub fn take_start(
    started: f64,
    first_sample: Option<Instant>,
    anchor: Option<(Instant, f64)>,
) -> f64 {
    match (first_sample, anchor) {
        (Some(first), Some((at, heard))) if first <= at => {
            heard - at.duration_since(first).as_secs_f64()
        }
        (Some(first), Some((at, heard))) => heard + first.duration_since(at).as_secs_f64(),
        _ => started,
    }
}

impl Editor {
    pub fn voiceover(&self) -> Option<&Voiceover> {
        self.voiceover.as_ref()
    }

    /// Takes recorded and still importing.
    pub fn pending_takes(&self) -> &[PendingTake] {
        &self.pending_takes
    }

    /// Open the microphone, for the Voiceover panel's meter and a take.
    /// False, with the reason in the status bar, when there is none.
    pub fn open_microphone(&mut self) -> bool {
        if self.voiceover.is_some() {
            return true;
        }
        let mic = if self.manual_audio {
            Ok(Microphone::manual())
        } else {
            Microphone::open()
        };
        match mic {
            Ok(mic) => {
                self.voiceover = Some(Voiceover {
                    mic,
                    meter: 0.0,
                    phase: Phase::Idle,
                });
                true
            }
            Err(error) => {
                self.status = format!("No microphone: {error}.");
                false
            }
        }
    }

    /// Close the microphone, unless a take counts down or records.
    pub fn close_microphone(&mut self) {
        if !self.voiceover.as_ref().is_some_and(Voiceover::busy) {
            self.voiceover = None;
        }
    }

    /// Counting down or recording.
    pub fn voiceover_busy(&self) -> bool {
        self.voiceover.as_ref().is_some_and(Voiceover::busy)
    }

    /// Record: count down, then record from the playhead. Again stops the
    /// take, or cancels the countdown.
    pub fn toggle_voiceover(&mut self) {
        if self.voiceover_busy() {
            self.stop_voiceover();
            return;
        }
        if let Err(error) = self.takes_folder() {
            self.status = error;
            return;
        }
        if !self.open_microphone() {
            return;
        }
        let now = self.last_now.unwrap_or(0.0);
        self.set_playing(false);
        if let Some(voiceover) = &mut self.voiceover {
            voiceover.phase = Phase::Countdown {
                at: now + COUNTDOWN,
                left: COUNTDOWN,
            };
        }
        self.status = "Recording in 3 seconds.".to_owned();
        self.frame_within(Duration::from_millis(50));
    }

    /// Stop the take and put it on the timeline once it is imported, or
    /// cancel the countdown.
    pub fn stop_voiceover(&mut self) {
        let Some(voiceover) = &mut self.voiceover else {
            return;
        };
        match std::mem::replace(&mut voiceover.phase, Phase::Idle) {
            Phase::Idle => {}
            Phase::Countdown { .. } => self.status = "Recording cancelled.".to_owned(),
            Phase::Recording(recording) => self.finish_take(recording),
        }
    }

    fn finish_take(&mut self, recording: Recording) {
        self.set_playing(false);
        if let Some(audio) = &self.audio {
            audio.set_muted(false);
        }
        let peaks = recording.take.peaks().clone();
        let recorded = match recording.take.finish() {
            Ok(recorded) => recorded,
            Err(error) => {
                self.status = format!("Recording failed: {error}.");
                return;
            }
        };
        if recorded.seconds <= 0.0 {
            let _ = std::fs::remove_file(&recorded.path);
            self.status = "Nothing was recorded.".to_owned();
            return;
        }
        let start = take_start(recording.start, recorded.first_sample, recording.anchor);
        let path = recorded.path.to_string_lossy().into_owned();
        let name = recorded.path.file_stem().map_or_else(
            || "Voiceover".to_owned(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        self.status = if recorded.dropped > 0.0 {
            format!(
                "Recorded {name}, but {:.1} s were lost: the disk was too slow.",
                recorded.dropped
            )
        } else {
            format!("Recorded {name}.")
        };
        self.pending_takes.push(PendingTake {
            path: path.clone(),
            name,
            start: start.max(0.0),
            source_in: (-start).max(0.0),
            seconds: recorded.seconds,
            peaks,
            project: self.project_path.clone(),
        });
        self.spawn_import(vec![recorded.path]);
    }

    /// The folder takes go in: beside the project, named after it.
    fn takes_folder(&self) -> Result<PathBuf, String> {
        let project = self
            .project_path
            .as_ref()
            .ok_or("Save the project first: takes are kept beside it.")?;
        let stem = project
            .file_stem()
            .map_or_else(|| "Eddie".into(), |stem| stem.to_string_lossy());
        let folder = project.parent().unwrap_or(project);
        Ok(folder.join(format!("{stem} voiceovers")))
    }

    /// Move the countdown and the take on, `dt` seconds after the last
    /// frame, and meter the microphone.
    pub(super) fn poll_voiceover(&mut self, now: f64, dt: f64) {
        let playing = self.playing;
        let heard = self
            .audio
            .as_ref()
            .filter(|_| playing)
            .map(|audio| audio.clock());
        let Some(voiceover) = &mut self.voiceover else {
            return;
        };
        voiceover.mic.advance(dt);
        let level = voiceover.mic.level();
        voiceover.meter = level.max(voiceover.meter * METER_FALL.powf(dt.max(0.0) as f32));
        let error = voiceover.mic.take_error();
        let (mut begin, mut grown) = (false, None);
        match &mut voiceover.phase {
            Phase::Idle => {}
            Phase::Countdown { at, left } => {
                *left = *at - now;
                begin = *left <= 0.0;
            }
            Phase::Recording(recording) => {
                // Once the clock is under way, one reading maps every
                // instant of the take to the timeline.
                if let Some(heard) = heard.filter(|_| recording.anchor.is_none()) {
                    if heard >= recording.start + 0.2 {
                        recording.anchor = Some((Instant::now(), heard));
                    }
                }
                grown = Some(recording.start + recording.take.seconds());
            }
        }
        if let Some(error) = error {
            self.status = format!("Microphone: {error}.");
        }
        if begin {
            self.begin_take();
        }
        if let Some(end) = grown {
            if let Some(audio) = &self.audio {
                audio.set_muted(!self.settings.hear_timeline_while_recording);
            }
            // Past the timeline's end the take goes on growing in view.
            if !self.playing {
                self.viewport.reveal(end);
            }
        }
        // The meter, the countdown and the take keep moving.
        self.frame_within(Duration::from_millis(30));
    }

    fn begin_take(&mut self) {
        let path = match self.takes_folder() {
            Ok(folder) => (1..)
                .map(|n| folder.join(format!("Voiceover {n}.wav")))
                .find(|path| !path.exists())
                .expect("a free name"),
            Err(error) => {
                self.status = error;
                return;
            }
        };
        let Some(voiceover) = &mut self.voiceover else {
            return;
        };
        let take = match voiceover.mic.record(&path) {
            Ok(take) => take,
            Err(error) => {
                voiceover.phase = Phase::Idle;
                self.status = format!("Can't record: {error}.");
                return;
            }
        };
        voiceover.phase = Phase::Recording(Recording {
            take,
            start: self.playhead,
            anchor: None,
        });
        let mute = !self.settings.hear_timeline_while_recording;
        self.audio_engine().set_muted(mute);
        if self.playhead < self.timeline.end() {
            self.set_playing(true);
        }
        self.status = "Recording. Press R or Space to stop.".to_owned();
    }

    /// Put a take on the timeline once its import is ready.
    pub(super) fn place_take(&mut self, path: &str) {
        let Some(index) = self.pending_takes.iter().position(|take| take.path == path) else {
            return;
        };
        let take = self.pending_takes.remove(index);
        if take.project != self.project_path {
            return;
        }
        let Some(asset) = self.asset(path) else {
            return;
        };
        let duration = asset.info.duration - take.source_in;
        match self.try_add_asset_range_to_timeline(
            path,
            &take.name,
            None,
            take.start,
            take.source_in,
            duration,
        ) {
            Ok(id) => {
                // A voiceover is the speech music ducks under, never music.
                if let Some(clip) = self.timeline.clip_mut(id) {
                    if clip.kind == ClipKind::Audio {
                        clip.duck = false;
                    }
                }
            }
            Err(error) => self.status = error,
        }
    }

    /// A take whose import failed is not waited for.
    pub(super) fn forget_take(&mut self, path: &str) {
        self.pending_takes.retain(|take| take.path != path);
    }
}

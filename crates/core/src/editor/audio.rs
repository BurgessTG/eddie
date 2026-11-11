//! What the timeline sounds like: the mixer's segments, the speech that
//! ducks music, and the measured loudness of the mix.

use std::collections::HashMap;
use std::sync::mpsc::{channel, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use openreel_engine::ClipKind;
use openreel_media::{AudioEngine, AudioSegment};

use crate::Editor;

impl Editor {
    pub fn audio_engine(&mut self) -> &mut AudioEngine {
        // An unplugged device (or a restarted sound server) stops calling
        // back, and the transport clock with it. Start again on whatever is
        // the default output now.
        if self.audio.as_ref().is_some_and(AudioEngine::device_lost) {
            self.audio = None;
            self.clock_playhead = None;
            self.status = "Audio device lost; switched output.".to_owned();
        }
        if self.audio.is_none() {
            let engine = if self.manual_audio {
                AudioEngine::manual()
            } else {
                AudioEngine::open()
            };
            eprintln!("audio output: {}", engine.description);
            if engine.description.starts_with("no audio device") {
                self.status = "No audio device; playing silently.".to_owned();
            }
            if self.match_export_loudness {
                engine.set_loudness(self.loudness_gain);
            }
            self.audio = Some(engine);
        }
        self.audio.as_mut().unwrap()
    }

    pub fn audio_running(&self) -> bool {
        self.audio
            .as_ref()
            .is_some_and(|audio| !audio.voice_ids().is_empty())
    }

    /// Where each source that a non-ducking clip plays has speech, in
    /// source seconds: its transcript turns, leaving out sparse ambience
    /// turns. An untranscribed source falls back to its waveform outside the
    /// silences tightenSilence uses, since missing transcription is
    /// uncertainty, not silence. Empty when no clip ducks, so playback pays
    /// nothing for it.
    pub fn speech_map(&self) -> HashMap<String, Vec<(f64, f64)>> {
        if !self.timeline.clips.iter().any(|clip| clip.duck) {
            return HashMap::new();
        }
        let mut map = HashMap::new();
        for clip in self.timeline.clips.iter().filter(|clip| !clip.duck) {
            let Some(source) = clip.source.as_ref() else {
                continue;
            };
            if map.contains_key(source) {
                continue;
            }
            let Some(asset) = self.asset(source) else {
                continue;
            };
            if !asset.sweep.transcript.is_empty() {
                let turns = asset
                    .sweep
                    .transcript
                    .iter()
                    .filter(|turn| turn.end > turn.start && !turn.is_ambient())
                    .map(|turn| (turn.start, turn.end))
                    .collect();
                map.insert(source.clone(), turns);
                continue;
            }
            if !asset.info.has_audio || asset.peaks.is_empty() {
                continue;
            }
            // Peaks keep their own rate; sound can end before the container.
            let rate = openreel_media::WAVEFORM_PEAKS_PER_SEC as f64;
            let sound_end = asset.peaks.len() as f64 / rate;
            let mut audible = Vec::new();
            let mut cursor = 0.0;
            let threshold = openreel_media::silence_threshold(&asset.peaks);
            for quiet in openreel_media::silence_ranges(&asset.peaks, rate, threshold, 0.35) {
                if quiet.start > cursor {
                    audible.push((cursor, quiet.start));
                }
                cursor = quiet.end;
            }
            if cursor < sound_end {
                audible.push((cursor, sound_end));
            }
            map.insert(source.clone(), audible);
        }
        map
    }

    /// Give the mixer every audible clip under the playhead or starting
    /// within the look-ahead, so it starts decoding before their edges.
    pub fn sync_audio(&mut self) {
        const LOOK_AHEAD: f64 = 0.75;
        let segments = self.audio_segments(Some((self.playhead, self.playhead + LOOK_AHEAD)));
        let audio = self.audio_engine();
        audio.set_voices(&segments);
        if let Some(error) = audio.take_error() {
            if !self.audio_failed {
                self.audio_failed = true;
                self.status = format!("Audio playback failed: {error}");
            }
        }
    }

    /// Every audible clip as the mixer plays it (ducking included), or only
    /// those overlapping `window`.
    pub fn audio_segments(&self, window: Option<(f64, f64)>) -> Vec<(u64, AudioSegment)> {
        let speech = self.speech_map();
        self.timeline
            .clips
            .iter()
            .filter(|c| {
                window.is_none_or(|(from, to)| c.start < to && c.end() > from)
                    && (c.kind == ClipKind::Audio
                        || (c.kind == ClipKind::Video && c.embedded_audio))
            })
            .filter_map(|c| {
                let source = c.source.as_ref()?;
                self.asset(source)?.info.has_audio.then(|| {
                    (
                        c.id,
                        AudioSegment {
                            path: source.clone(),
                            start: c.start,
                            duration: c.duration,
                            source_in: c.source_in,
                            volume: c.volume,
                            fade_in: c.fade_in,
                            fade_out: c.fade_out,
                            duck: self.timeline.duck_ranges(c, &speech),
                            clean_voice: c.clean_voice,
                            smooth_fades: c.smooth_fades,
                        },
                    )
                })
            })
            .collect()
    }

    /// The measured mix for scripts: loudness before export's gain,
    /// what export will do to it, quiet stretches, and the mix's loudness
    /// while each audible clip plays.
    pub fn audio_report(&self) -> serde_json::Value {
        let round = |value: f64| (value * 100.0).round() / 100.0;
        let status = if self.audio_segments(None).is_empty() {
            "silent"
        } else if !self.mix_measured() {
            "measuring"
        } else if self.loudness.is_none() {
            "failed"
        } else {
            "measured"
        };
        let Some(loudness) = self.loudness.as_ref().filter(|_| status == "measured") else {
            return serde_json::json!({ "status": status });
        };
        let whole = 0.0..loudness.duration();
        let integrated = loudness.integrated(whole.clone());
        let peak = loudness.true_peak(whole);
        let gain =
            integrated.map(|lufs| (openreel_media::LOUDNESS_TARGET - lufs).clamp(-30.0, 30.0));
        let export_peak = peak.zip(gain).map(|(peak, gain)| peak + gain);
        let clips: Vec<serde_json::Value> = self
            .loudness_mix
            .iter()
            .map(|(id, segment)| {
                let range = segment.start..segment.start + segment.duration;
                serde_json::json!({
                    "id": id,
                    "integratedLufs": loudness.integrated(range.clone()).map(round),
                    "truePeakDbtp": loudness.true_peak(range).map(round),
                })
            })
            .collect();
        serde_json::json!({
            "status": status,
            "integratedLufs": integrated.map(round),
            "truePeakDbtp": peak.map(round),
            "maxMomentaryLufs": loudness.max_momentary().map(round),
            "clippedSamples": loudness.clipped,
            "exportTargetLufs": openreel_media::LOUDNESS_TARGET,
            "exportGainDb": gain.map(round),
            "exportPeakDbtp": export_peak.map(round),
            // Export's limiter holds peaks at 0.8 (-1.94 dBFS).
            "limited": export_peak.is_some_and(|peak| peak > 20.0 * 0.8_f64.log10()),
            "quietRanges": loudness
                .quiet_ranges(-60.0, 1.0)
                .into_iter()
                .map(|range| [round(range.start), round(range.end)])
                .collect::<Vec<_>>(),
            "clips": clips,
        })
    }

    /// Keep the preview at the export's loudness. Half a second after the
    /// mix changes (and with no pointer held), the whole mix is rendered and
    /// measured off the UI thread; the preview then plays with the gain
    /// export would apply and the same 0.8 limiter.
    pub fn poll_loudness(&mut self, pointer_down: bool) {
        self.take_loudness();
        if !self.match_export_loudness
            || pointer_down
            || self.loudness_checked.elapsed() < Duration::from_millis(500)
        {
            return;
        }
        self.measure_mix();
    }

    /// Take in a finished measurement.
    fn take_loudness(&mut self) {
        if let Some(rx) = &self.loudness_rx {
            match rx.try_recv() {
                Ok((mix, result)) => {
                    self.loudness_rx = None;
                    if mix == self.loudness_mix {
                        self.loudness = result.ok();
                        let lufs = self
                            .loudness
                            .as_ref()
                            .and_then(|loudness| loudness.integrated(0.0..loudness.duration()));
                        let gain = lufs.map(|lufs| {
                            let db = (openreel_media::LOUDNESS_TARGET - lufs).clamp(-30.0, 30.0);
                            10f64.powf(db / 20.0) as f32
                        });
                        self.loudness_gain = gain;
                        self.match_preview_loudness();
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.loudness_rx = None,
            }
        }
    }

    /// Only an editor with a preview plays the measured gain; a measurement
    /// made for a script changes nothing anyone hears.
    fn match_preview_loudness(&mut self) {
        if self.match_export_loudness {
            let gain = self.loudness_gain;
            self.audio_engine().set_loudness(gain);
        }
    }

    /// Whether the last measurement is of the mix the timeline has now.
    pub fn mix_measured(&self) -> bool {
        self.loudness_rx.is_none() && self.loudness_mix == self.audio_segments(None)
    }

    /// Measure the mix off this thread unless that measurement exists or is
    /// under way. Its worker wakes the editor; call again, or
    /// `poll_loudness`, to take the result in.
    pub fn measure_mix(&mut self) {
        self.take_loudness();
        self.loudness_checked = Instant::now();
        let mix = self.audio_segments(None);
        if mix == self.loudness_mix {
            return;
        }
        if let Some(cancel) = self.loudness_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.loudness_mix = mix.clone();
        if mix.is_empty() {
            self.loudness_rx = None;
            self.loudness_gain = None;
            self.loudness = None;
            self.match_preview_loudness();
            return;
        }
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.loudness_cancel = Some(Arc::clone(&cancel));
        let (tx, rx) = channel();
        self.loudness_rx = Some(rx);
        let end = self.timeline.end();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let result = openreel_media::measure_mix(&mix, end, &cancel);
            let _ = tx.send((mix, result));
            wake.wake();
        });
    }
}

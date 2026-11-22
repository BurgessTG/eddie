//! What noise reduction needs to know: the first clip of a source that
//! reduces noise has that source's noise measured off the UI thread
//! (`openreel_media::measure_noise`), saved beside its curation and used by
//! every clip of it in preview and export. Until then the clip plays as
//! recorded.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use openreel_engine::ClipKind;
use openreel_media::NoiseProfile;

use super::media::AssetState;
use super::sweep::{load_noise, save_noise};
use super::Editor;

type Measured = (String, Result<NoiseProfile, String>);

/// Sources measured, being measured, or that could not be.
pub(crate) struct Noise {
    profiles: HashMap<String, NoiseProfile>,
    measuring: HashSet<String>,
    failed: HashSet<String>,
    tx: Sender<Measured>,
    rx: Receiver<Measured>,
}

impl Default for Noise {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            profiles: HashMap::new(),
            measuring: HashSet::new(),
            failed: HashSet::new(),
            tx,
            rx,
        }
    }
}

impl Editor {
    /// The measured noise of `source`, once it is measured.
    pub(crate) fn noise_profile(&self, source: &str) -> Option<NoiseProfile> {
        self.noise.profiles.get(source).copied()
    }

    /// Every measured source, for export.
    pub(crate) fn noise_profiles(&self) -> HashMap<String, NoiseProfile> {
        self.noise.profiles.clone()
    }

    /// Imported sources whose sound a clip reduces the noise of and that
    /// have no measurement.
    fn unmeasured_noise(&self) -> Vec<String> {
        let mut sources: Vec<String> = Vec::new();
        for clip in &self.timeline.clips {
            let sounds = clip.kind == ClipKind::Audio
                || (clip.kind == ClipKind::Video && clip.embedded_audio);
            let Some(source) = clip.source.as_ref().filter(|_| clip.reduce_noise && sounds) else {
                continue;
            };
            if self.noise.profiles.contains_key(source)
                || self.noise.failed.contains(source)
                || sources.contains(source)
            {
                continue;
            }
            if self
                .asset(source)
                .is_some_and(|asset| asset.state == AssetState::Ready && asset.info.has_audio)
            {
                sources.push(source.clone());
            }
        }
        sources
    }

    /// Once per frame: take in finished measurements and start the ones a
    /// clip waits for.
    pub(super) fn poll_noise(&mut self) {
        while let Ok((source, result)) = self.noise.rx.try_recv() {
            self.noise.measuring.remove(&source);
            match result {
                Ok(profile) => {
                    self.noise.profiles.insert(source, profile);
                }
                Err(error) => {
                    let name = Path::new(&source)
                        .file_name()
                        .map_or(source.clone(), |name| name.to_string_lossy().into_owned());
                    self.status = format!("Could not measure the noise in {name}: {error}");
                    self.noise.failed.insert(source);
                }
            }
        }
        for source in self.unmeasured_noise() {
            if !self.noise.measuring.insert(source.clone()) {
                continue;
            }
            let tx = self.noise.tx.clone();
            let store = self.curation_dir.clone();
            let wake = self.wake.clone();
            thread::spawn(move || {
                let saved = store.as_deref().and_then(|dir| load_noise(dir, &source));
                let result = match saved {
                    Some(profile) => Ok(profile),
                    None => openreel_media::measure_noise(&source).inspect(|profile| {
                        if let Some(dir) = &store {
                            let _ = save_noise(dir, &source, profile);
                        }
                    }),
                };
                let _ = tx.send((source, result));
                wake.wake();
            });
        }
    }

    /// A clip reduces the noise of a source still being measured, so the
    /// mix is not yet what it will be.
    pub fn noise_pending(&self) -> bool {
        !self.noise.measuring.is_empty() || !self.unmeasured_noise().is_empty()
    }
}

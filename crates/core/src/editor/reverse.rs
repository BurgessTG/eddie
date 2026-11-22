//! Clips played backwards. Reversing a clip writes the stretch of its file
//! that it shows, and [`HANDLE`] more each side, backwards into a file of
//! its own (`openreel_media::reverse_range`) in `reversed/` beside the
//! settings, on a worker thread. The clip then plays that file, from the
//! other end, and preview, export and pictures read it like any file.
//! Like a shape's drawing, it is kept in `Editor::made`, out of the bin,
//! and a project carries only what each clip reverses (`Clip::reversed`),
//! so a project whose file is gone makes it again when it opens.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use openreel_engine::{ClipId, ClipKind, Reversed};

use super::media::{AssetState, BinAsset};
use super::sweep::{path_hash, SweepData};
use super::Editor;

/// How much of the file past each end of the clip is reversed with it, so
/// the reversed clip can be trimmed longer and a transition has pictures.
pub const HANDLE: f64 = 1.0;

/// `reversed/` beside the settings file.
pub fn default_dir() -> Option<PathBuf> {
    Some(super::settings::config_path()?.parent()?.join("reversed"))
}

enum Msg {
    Progress(String, f64),
    Done(String, Result<Box<BinAsset>, String>),
}

/// A reversed file being written or read.
struct Running {
    spec: Reversed,
    progress: f64,
    cancel: Arc<AtomicBool>,
}

/// The reversals under way, and what waits for them.
pub(crate) struct Reversals {
    running: HashMap<String, Running>,
    /// Files that could not be made, and why.
    failed: HashMap<String, String>,
    /// Clips the window turns round once their file is made.
    waiting: Vec<(ClipId, String)>,
    /// Files a script's edit asked for that are not made yet: `run`
    /// makes them, then runs the script again.
    pub(crate) wanted: Vec<(String, Reversed)>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl Default for Reversals {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            running: HashMap::new(),
            failed: HashMap::new(),
            waiting: Vec::new(),
            wanted: Vec::new(),
            tx,
            rx,
        }
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or(path.to_owned(), |name| name.to_string_lossy().into_owned())
}

impl Editor {
    /// Where reversed files are kept. An editor without the user's config
    /// (a test) keeps them in the system's temporary folder.
    fn reversed_folder(&self) -> PathBuf {
        self.reversed_dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("eddie-reversed"))
    }

    /// The file `spec` is written to, named by the original's name and a
    /// hash of its path, size, modification time and the range, so a
    /// changed original is reversed again.
    fn reversed_path(&self, spec: &Reversed) -> String {
        let (size, modified) = std::fs::metadata(&spec.original)
            .map(|meta| {
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map_or(0, |since| since.as_nanos());
                (meta.len(), modified)
            })
            .unwrap_or_default();
        let key = format!(
            "{}\n{size}\n{modified}\n{:?}\n{:?}",
            spec.original, spec.start, spec.end
        );
        let stem = Path::new(&spec.original)
            .file_stem()
            .map_or("clip".into(), |stem| stem.to_string_lossy());
        self.reversed_folder()
            .join(format!("{stem}-{:016x}.mov", path_hash(&key)))
            .to_string_lossy()
            .into_owned()
    }

    /// Clip `id` and its linked partner, when that plays the same file.
    fn same_file_pair(&self, id: ClipId) -> Vec<ClipId> {
        let Some(clip) = self.timeline.clip(id) else {
            return Vec::new();
        };
        let mut ids = vec![id];
        if let Some(partner) = clip
            .linked_clip
            .and_then(|linked| self.timeline.clip(linked))
            .filter(|partner| partner.source == clip.source && partner.reversed == clip.reversed)
        {
            ids.push(partner.id);
        }
        ids
    }

    /// What reversing clip `id` writes: the stretch of its file that it
    /// and its linked partner show, with [`HANDLE`] each side, in whole
    /// frames.
    fn reverse_spec(&self, id: ClipId) -> Result<Reversed, String> {
        let clip = self
            .timeline
            .clip(id)
            .ok_or_else(|| format!("there is no clip {id}"))?;
        if !matches!(clip.kind, ClipKind::Video | ClipKind::Audio) {
            return Err("only video and audio clips play backwards".to_owned());
        }
        if clip.still() || clip.graphic.is_some() {
            return Err("a photo, freeze frame, shape or sticker has no direction".to_owned());
        }
        let source = clip.source.as_deref().ok_or("the clip has no file")?;
        let asset = self
            .asset(source)
            .filter(|asset| asset.state == AssetState::Ready)
            .ok_or_else(|| format!("{} is not imported yet", file_name(source)))?;
        let (from, to) = self
            .same_file_pair(id)
            .into_iter()
            .filter_map(|id| self.timeline.clip(id))
            .fold((f64::INFINITY, 0.0_f64), |(from, to), clip| {
                (from.min(clip.source_in), to.max(clip.source_end()))
            });
        let length = asset.info.duration;
        let (start, end) = openreel_media::frame_range(
            asset.info.fps,
            (from - HANDLE).max(0.0),
            (to + HANDLE).min(length.max(to)),
        );
        Ok(Reversed {
            original: source.to_owned(),
            start,
            end,
        })
    }

    /// Play clip `id` and its partner from `file`, which holds `spec`.
    fn turn_round(&mut self, id: ClipId, file: &str, spec: &Reversed) {
        for id in self.same_file_pair(id) {
            if let Some(clip) = self.timeline.clip_mut(id) {
                clip.source_in = (spec.end - clip.source_end()).max(0.0);
                clip.source = Some(file.to_owned());
                clip.reversed = Some(spec.clone());
            }
        }
    }

    /// Play reversed clip `id` and its partner forwards again.
    fn turn_forward(&mut self, id: ClipId) {
        for id in self.same_file_pair(id) {
            if let Some(clip) = self.timeline.clip_mut(id) {
                if let Some(spec) = clip.reversed.take() {
                    clip.source_in = (spec.end - clip.source_end()).max(0.0);
                    clip.source = Some(spec.original);
                }
            }
        }
    }

    /// A script's `setReverse`. A file not made yet goes on the wanted
    /// list and leaves the clip as it is; `apply_edit` then refuses the
    /// edit, and `run` makes the file and runs the script again.
    pub(crate) fn set_reverse(&mut self, id: ClipId, on: bool) -> Result<(), String> {
        let clip = self
            .timeline
            .clip(id)
            .ok_or_else(|| format!("there is no clip {id}"))?;
        if on == clip.reversed.is_some() {
            return Ok(());
        }
        if clip.still() {
            return Err("a photo or freeze frame has no direction".to_owned());
        }
        if !on {
            self.turn_forward(id);
            return Ok(());
        }
        let spec = self.reverse_spec(id)?;
        let file = self.reversed_path(&spec);
        if self.made.iter().any(|asset| asset.info.path == file) {
            self.turn_round(id, &file, &spec);
        } else if let Some(error) = self.reversals.failed.get(&file) {
            return Err(format!(
                "could not reverse {}: {error}",
                file_name(&spec.original)
            ));
        } else if !self
            .reversals
            .wanted
            .iter()
            .any(|(wanted, _)| *wanted == file)
        {
            self.reversals.wanted.push((file, spec));
        }
        Ok(())
    }

    /// Reverse, the toolbar's and menus' action: play the selected clip
    /// backwards once its file is made, or forwards again. On a clip still
    /// waiting for its file, stop waiting.
    pub fn reverse_selected(&mut self) {
        let Some(id) = self.selected else {
            self.status = "Select a clip to reverse.".to_owned();
            return;
        };
        let Some(clip) = self.timeline.clip(id) else {
            return;
        };
        let name = clip.name.clone();
        if let Some(at) = self.reversals.waiting.iter().position(|(c, _)| *c == id) {
            let (_, file) = self.reversals.waiting.remove(at);
            if !self.reversals.waiting.iter().any(|(_, f)| *f == file) {
                if let Some(running) = self.reversals.running.get(&file) {
                    running.cancel.store(true, Ordering::Relaxed);
                }
            }
            self.status = format!("Stopped reversing {name}.");
            return;
        }
        if clip.reversed.is_some() {
            self.turn_forward(id);
            self.status = format!("{name} plays forwards again.");
            return;
        }
        let spec = match self.reverse_spec(id) {
            Ok(spec) => spec,
            Err(why) => {
                self.status = format!("Cannot reverse {name}: {why}.");
                return;
            }
        };
        let file = self.reversed_path(&spec);
        if self.made.iter().any(|asset| asset.info.path == file) {
            self.turn_round(id, &file, &spec);
            self.status = format!("{name} plays backwards.");
            return;
        }
        self.reversals.waiting.push((id, file.clone()));
        self.start_reversal(file, spec);
        self.status = format!("Reversing {name}…");
    }

    /// Whether clip `id` waits for its reversed file.
    pub fn reversing_clip(&self, id: ClipId) -> bool {
        self.reversals.waiting.iter().any(|(clip, _)| *clip == id)
    }

    /// Whether any reversed file is being written or read.
    pub fn reversing(&self) -> bool {
        !self.reversals.running.is_empty()
    }

    /// How far along the reversals are, 0 to 1, while any run.
    pub fn reverse_progress(&self) -> Option<f64> {
        let running = &self.reversals.running;
        (!running.is_empty())
            .then(|| running.values().map(|r| r.progress).sum::<f64>() / running.len() as f64)
    }

    /// Whether `file` is being made, for a job that waits for it.
    pub(crate) fn reversal_running(&self, file: &str) -> Option<f64> {
        self.reversals.running.get(file).map(|r| r.progress)
    }

    /// Make the files a refused script edit wanted. Returns them.
    pub(crate) fn make_wanted_reversals(&mut self) -> Vec<String> {
        let wanted = std::mem::take(&mut self.reversals.wanted);
        wanted
            .into_iter()
            .map(|(file, spec)| {
                self.start_reversal(file.clone(), spec);
                file
            })
            .collect()
    }

    /// Write `file` from `spec` unless it is there, then read it in.
    fn start_reversal(&mut self, file: String, spec: Reversed) {
        if self.reversals.running.contains_key(&file) {
            return;
        }
        self.reversals.failed.remove(&file);
        let cancel = Arc::new(AtomicBool::new(false));
        self.reversals.running.insert(
            file.clone(),
            Running {
                spec: spec.clone(),
                progress: 0.0,
                cancel: Arc::clone(&cancel),
            },
        );
        let tx = self.reversals.tx.clone();
        let wake = self.wake.clone();
        let thumbnails = self.make_thumbnails;
        thread::spawn(move || {
            let mut progress = |share: f64| {
                let _ = tx.send(Msg::Progress(file.clone(), share));
                wake.wake();
            };
            let made = make(&file, &spec, thumbnails, &cancel, &mut progress);
            let _ = tx.send(Msg::Done(file, made.map(Box::new)));
            wake.wake();
        });
    }

    /// Once per frame: take in finished files and turn waiting clips round.
    pub(super) fn poll_reversals(&mut self) {
        while let Ok(msg) = self.reversals.rx.try_recv() {
            match msg {
                Msg::Progress(file, share) => {
                    if let Some(running) = self.reversals.running.get_mut(&file) {
                        running.progress = share;
                    }
                }
                Msg::Done(file, made) => self.reversed(file, made),
            }
        }
        if self.reversing() {
            self.frame_within(Duration::from_millis(250));
        }
    }

    fn reversed(&mut self, file: String, made: Result<Box<BinAsset>, String>) {
        let Some(running) = self.reversals.running.remove(&file) else {
            return;
        };
        let name = file_name(&running.spec.original);
        let waiting: Vec<ClipId> = self
            .reversals
            .waiting
            .iter()
            .filter(|(_, f)| *f == file)
            .map(|(clip, _)| *clip)
            .collect();
        self.reversals.waiting.retain(|(_, f)| *f != file);
        let asset = match made {
            Ok(asset) => *asset,
            Err(error) => {
                if !running.cancel.load(Ordering::Relaxed) {
                    self.status = format!("Could not reverse {name}: {error}");
                    self.reversals.failed.insert(file, error);
                }
                return;
            }
        };
        self.made.retain(|known| known.info.path != file);
        self.made.push(asset);
        let spec = running.spec;
        for id in waiting {
            // The clip may have moved on while its file was written.
            let still_fits = self.timeline.clip(id).is_some_and(|clip| {
                clip.reversed.is_none()
                    && clip.source.as_deref() == Some(spec.original.as_str())
                    && spec.holds(clip.source_in, clip.source_end())
            });
            if still_fits {
                self.turn_round(id, &file, &spec);
                self.status = format!("{name} plays backwards.");
            } else if self.timeline.clip(id).is_some() {
                self.status = format!("{name} changed while it was reversed; reverse it again.");
            }
        }
    }

    /// Point every reversed clip at its file, making it again where it is
    /// gone. Called when a project opens and after missing media is found.
    pub(crate) fn restore_reversals(&mut self) {
        let reversed: Vec<(usize, Reversed, Option<String>)> = self
            .timeline
            .clips
            .iter()
            .enumerate()
            .filter_map(|(index, clip)| Some((index, clip.reversed.clone()?, clip.source.clone())))
            .collect();
        for (index, spec, source) in reversed {
            let file = source
                .filter(|file| Path::new(file).exists())
                .unwrap_or_else(|| self.reversed_path(&spec));
            self.timeline.clips[index].source = Some(file.clone());
            let known = self.made.iter().any(|asset| asset.info.path == file);
            if !known && Path::new(&spec.original).exists() {
                self.start_reversal(file, spec);
            }
        }
    }
}

/// On a worker: write `file` from `spec` unless it is there, then read it
/// as an import would.
fn make(
    file: &str,
    spec: &Reversed,
    thumbnails: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<BinAsset, String> {
    let path = Path::new(file);
    if path.exists() {
        // Kept from `graphics::prune` while a project uses it.
        let _ = std::fs::File::options()
            .append(true)
            .open(path)
            .and_then(|f| f.set_modified(SystemTime::now()));
    } else {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
        }
        openreel_media::reverse_range(
            &spec.original,
            spec.start,
            spec.end,
            path,
            progress,
            cancel,
        )?;
    }
    let info = openreel_media::probe(file)?;
    let rate = openreel_media::WAVEFORM_PEAKS_PER_SEC;
    let peaks = if info.has_audio {
        openreel_media::waveform_peaks(file, info.duration, rate).unwrap_or_default()
    } else {
        Vec::new()
    };
    let thumbs = if thumbnails && info.has_video {
        openreel_media::thumbnails(file, info.width, info.height, info.duration).unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(BinAsset {
        size_bytes: std::fs::metadata(file).map_or(0, |meta| meta.len()),
        info,
        state: AssetState::Ready,
        peaks,
        beats: None,
        thumbs,
        sweep: SweepData::default(),
    })
}

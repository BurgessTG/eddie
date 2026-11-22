//! Clips played backwards or stabilized. Either writes the stretch of the
//! clip's file that it shows, and [`HANDLE`] more each side, into a file of
//! its own (`openreel_media::remake`) in `remade/` beside the settings, on
//! a worker thread. The clip then plays that file, and preview, export and
//! pictures read it like any file. Like a shape's drawing, it is kept in
//! `Editor::made`, out of the bin, and a project carries only what each
//! clip's file holds (`Clip::remade`), so a project whose file is gone
//! makes it again when it opens.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use openreel_engine::{Clip, ClipId, ClipKind, Remade, Stabilize};

use super::media::{AssetState, BinAsset};
use super::sweep::{path_hash, SweepData};
use super::Editor;

/// How much of the file past each end of the clip goes into its file, so
/// the clip can be trimmed longer and a transition has pictures.
pub const HANDLE: f64 = 1.0;

/// `remade/` beside the settings file.
pub fn default_dir() -> Option<PathBuf> {
    Some(super::settings::config_path()?.parent()?.join("remade"))
}

enum Msg {
    Progress(String, f64),
    Done(String, Result<Box<BinAsset>, String>),
}

/// A file being written or read.
struct Running {
    spec: Remade,
    progress: f64,
    cancel: Arc<AtomicBool>,
}

/// A clip the window changes once its file is made.
struct Waiting {
    clip: ClipId,
    file: String,
    /// What the clip played when it was asked: one changed since is left
    /// as it is.
    from: Option<Remade>,
}

/// The files under way, and what waits for them.
pub(crate) struct Remakes {
    running: HashMap<String, Running>,
    /// Files that could not be made, and why.
    failed: HashMap<String, String>,
    waiting: Vec<Waiting>,
    /// Files a script's edit asked for that are not made yet: `run`
    /// makes them, then runs the script again.
    pub(crate) wanted: Vec<(String, Remade)>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl Default for Remakes {
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

/// Whether a clip plays backwards, and how it is steadied.
fn looks(clip: &Clip) -> (bool, Option<Stabilize>) {
    clip.remade
        .as_ref()
        .map_or((false, None), |remade| (remade.reverse, remade.stabilize))
}

/// Making `spec`'s file, as in "Stabilizing talk.mp4…".
pub fn doing(spec: &Remade) -> &'static str {
    match (spec.stabilize.is_some(), spec.reverse) {
        (true, true) => "Stabilizing and reversing",
        (true, false) => "Stabilizing",
        _ => "Reversing",
    }
}

/// What clip `name` plays now that it plays `spec`'s file, or its own.
fn plays(name: &str, spec: Option<&Remade>) -> String {
    match spec.map(|spec| (spec.stabilize.is_some(), spec.reverse)) {
        None => format!("{name} plays as shot again."),
        Some((true, true)) => format!("{name} is steadied and plays backwards."),
        Some((true, false)) => format!("{name} is steadied."),
        Some((false, _)) => format!("{name} plays backwards."),
    }
}

impl Editor {
    /// Where remade files are kept. An editor without the user's config
    /// (a test) keeps them in the system's temporary folder.
    fn remade_folder(&self) -> PathBuf {
        self.remade_dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("eddie-remade"))
    }

    /// The file `spec` is written to, named by the original's name and a
    /// hash of its path, size, modification time, the range and what is
    /// done to it, so a changed original is made again.
    fn remade_path(&self, spec: &Remade) -> String {
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
            "{}\n{size}\n{modified}\n{:?}\n{:?}\n{}\n{}",
            spec.original,
            spec.start,
            spec.end,
            spec.reverse,
            spec.stabilize.map_or("", Stabilize::name),
        );
        let stem = Path::new(&spec.original)
            .file_stem()
            .map_or("clip".into(), |stem| stem.to_string_lossy());
        self.remade_folder()
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
            .filter(|partner| partner.source == clip.source && partner.remade == clip.remade)
        {
            ids.push(partner.id);
        }
        ids
    }

    /// What clip `id` and its partner play once backwards as `reverse`
    /// says and steadied as `stabilize` says: the stretch of the original
    /// they show, with [`HANDLE`] each side, in whole frames. `None` is the
    /// original itself.
    fn remade_spec(
        &self,
        id: ClipId,
        reverse: bool,
        stabilize: Option<Stabilize>,
    ) -> Result<Option<Remade>, String> {
        let clip = self
            .timeline
            .clip(id)
            .ok_or_else(|| format!("there is no clip {id}"))?;
        if !reverse && stabilize.is_none() {
            return Ok(None);
        }
        if !matches!(clip.kind, ClipKind::Video | ClipKind::Audio) {
            return Err("only video and audio clips play backwards".to_owned());
        }
        if clip.still() || clip.graphic.is_some() {
            return Err("a photo, freeze frame, shape or sticker plays no footage".to_owned());
        }
        let (original, _, _) = clip.shown_range().ok_or("the clip has no file")?;
        let asset = self
            .asset(original)
            .filter(|asset| asset.state == AssetState::Ready)
            .ok_or_else(|| format!("{} is not imported yet", file_name(original)))?;
        if stabilize.is_some() && !asset.info.has_video {
            return Err(format!("{} has no picture to steady", file_name(original)));
        }
        let (from, to) = self
            .same_file_pair(id)
            .into_iter()
            .filter_map(|id| self.timeline.clip(id)?.shown_range())
            .fold((f64::INFINITY, 0.0_f64), |(from, to), (_, start, end)| {
                (from.min(start), to.max(end))
            });
        let length = asset.info.duration;
        let (start, end) = openreel_media::frame_range(
            asset.info.fps,
            (from - HANDLE).max(0.0),
            (to + HANDLE).min(length.max(to)),
        );
        Ok(Some(Remade {
            original: original.to_owned(),
            start,
            end,
            reverse,
            stabilize,
        }))
    }

    /// Play clip `id` and its partner from `file`, which holds `spec`, or
    /// from their original with `None`. Each keeps the stretch it shows.
    fn play_remade(&mut self, id: ClipId, target: Option<(&str, &Remade)>) {
        for id in self.same_file_pair(id) {
            let Some(clip) = self.timeline.clip_mut(id) else {
                continue;
            };
            let Some((original, from, to)) = clip
                .shown_range()
                .map(|(original, from, to)| (original.to_owned(), from, to))
            else {
                continue;
            };
            match target {
                Some((file, spec)) => {
                    clip.source_in = spec.to_file_range(from, to).0.max(0.0);
                    clip.source = Some(file.to_owned());
                    clip.remade = Some(spec.clone());
                }
                None => {
                    clip.source_in = from;
                    clip.source = Some(original);
                    clip.remade = None;
                }
            }
        }
    }

    /// A script's `setReverse`.
    pub(crate) fn set_reverse(&mut self, id: ClipId, on: bool) -> Result<(), String> {
        let clip = self
            .timeline
            .clip(id)
            .ok_or_else(|| format!("there is no clip {id}"))?;
        let (_, stabilize) = looks(clip);
        self.script_remake(id, on, stabilize)
    }

    /// A script's `setStabilize`.
    pub(crate) fn set_stabilize(
        &mut self,
        id: ClipId,
        level: Option<Stabilize>,
    ) -> Result<(), String> {
        let clip = self
            .timeline
            .clip(id)
            .ok_or_else(|| format!("there is no clip {id}"))?;
        if level.is_some() && clip.kind != ClipKind::Video {
            return Err("only video clips are stabilized; their linked sound follows".to_owned());
        }
        let (reverse, _) = looks(clip);
        self.script_remake(id, reverse, level)
    }

    /// Change what clip `id` plays for a script. A file not made yet goes
    /// on the wanted list and leaves the clip as it is; `apply_edit` then
    /// refuses the edit, and `run` makes the file and runs the script
    /// again.
    fn script_remake(
        &mut self,
        id: ClipId,
        reverse: bool,
        stabilize: Option<Stabilize>,
    ) -> Result<(), String> {
        let clip = self
            .timeline
            .clip(id)
            .ok_or_else(|| format!("there is no clip {id}"))?;
        if looks(clip) == (reverse, stabilize) {
            return Ok(());
        }
        let Some(spec) = self.remade_spec(id, reverse, stabilize)? else {
            self.play_remade(id, None);
            return Ok(());
        };
        let file = self.remade_path(&spec);
        if self.made.iter().any(|asset| asset.info.path == file) {
            self.play_remade(id, Some((&file, &spec)));
        } else if let Some(error) = self.remakes.failed.get(&file) {
            return Err(format!(
                "could not make {}'s file: {error}",
                file_name(&spec.original)
            ));
        } else if !self
            .remakes
            .wanted
            .iter()
            .any(|(wanted, _)| *wanted == file)
        {
            self.remakes.wanted.push((file, spec));
        }
        Ok(())
    }

    /// Reverse, the toolbar's and menus' action: play the selected clip
    /// backwards once its file is made, or forwards again. On a clip still
    /// waiting for a file, it turns round what that file will play, so
    /// pressing it on a clip waiting to play backwards stops that.
    pub fn reverse_selected(&mut self) {
        let Some(id) = self.selected else {
            self.status = "Select a clip to reverse.".to_owned();
            return;
        };
        let Some((reverse, stabilize)) = self.remake_target(id) else {
            return;
        };
        self.change_remake(id, !reverse, stabilize, "reverse");
    }

    /// Details' Stabilize list: steady clip `id` as `level` says, or play
    /// it as shot with `None`, keeping its direction.
    pub fn stabilize_clip(&mut self, id: ClipId, level: Option<Stabilize>) {
        let Some((reverse, stabilize)) = self.remake_target(id) else {
            return;
        };
        if stabilize != level {
            self.change_remake(id, reverse, level, "stabilize");
        }
    }

    /// What clip `id` plays, or will once the file it waits for is made.
    fn remake_target(&self, id: ClipId) -> Option<(bool, Option<Stabilize>)> {
        Some(match self.remake_pending(id) {
            Some(spec) => (spec.reverse, spec.stabilize),
            None => looks(self.timeline.clip(id)?),
        })
    }

    /// Play clip `id` as `reverse` and `stabilize` say instead of what it
    /// plays or waits for. Asking for what it plays now stops the wait.
    fn change_remake(
        &mut self,
        id: ClipId,
        reverse: bool,
        stabilize: Option<Stabilize>,
        what: &str,
    ) {
        let Some(clip) = self.timeline.clip(id) else {
            return;
        };
        let name = clip.name.clone();
        let current = looks(clip);
        if let Some(spec) = self.stop_waiting(id) {
            if (reverse, stabilize) == current {
                self.status = format!("Stopped {} {name}.", doing(&spec).to_lowercase());
                return;
            }
        }
        if (reverse, stabilize) != current {
            self.start_remake(id, reverse, stabilize, what);
        }
    }

    /// Stop clip `id` waiting for a file, and stop making it when no other
    /// clip waits for it. Returns what it waited for.
    fn stop_waiting(&mut self, id: ClipId) -> Option<Remade> {
        let at = self.remakes.waiting.iter().position(|w| w.clip == id)?;
        let waited = self.remakes.waiting.remove(at);
        let running = self.remakes.running.get(&waited.file)?;
        if !self.remakes.waiting.iter().any(|w| w.file == waited.file) {
            running.cancel.store(true, Ordering::Relaxed);
        }
        Some(running.spec.clone())
    }

    /// Play clip `id` as `reverse` and `stabilize` say, at once when its
    /// file is there, else once it is made. `what` names the change for a
    /// refusal.
    fn start_remake(
        &mut self,
        id: ClipId,
        reverse: bool,
        stabilize: Option<Stabilize>,
        what: &str,
    ) {
        let Some(clip) = self.timeline.clip(id) else {
            return;
        };
        let name = clip.name.clone();
        let from = clip.remade.clone();
        let spec = match self.remade_spec(id, reverse, stabilize) {
            Ok(Some(spec)) => spec,
            Ok(None) => {
                self.play_remade(id, None);
                self.status = plays(&name, None);
                return;
            }
            Err(why) => {
                self.status = format!("Cannot {what} {name}: {why}.");
                return;
            }
        };
        let file = self.remade_path(&spec);
        if self.made.iter().any(|asset| asset.info.path == file) {
            self.play_remade(id, Some((&file, &spec)));
            self.status = plays(&name, Some(&spec));
            return;
        }
        self.status = format!("{} {name}…", doing(&spec));
        self.remakes.waiting.push(Waiting {
            clip: id,
            file: file.clone(),
            from,
        });
        self.start_file(file, spec);
    }

    /// What clip `id` waits for its file to play, if it waits.
    pub fn remake_pending(&self, id: ClipId) -> Option<&Remade> {
        let waiting = self.remakes.waiting.iter().find(|w| w.clip == id)?;
        self.remakes
            .running
            .get(&waiting.file)
            .map(|running| &running.spec)
    }

    /// Whether any file is being written or read.
    pub fn remaking(&self) -> bool {
        !self.remakes.running.is_empty()
    }

    /// How far along the files are, 0 to 1, while any is made, and what
    /// is being done: "Stabilizing" while any is steadied, else
    /// "Reversing".
    pub fn remake_progress(&self) -> Option<(f64, &'static str)> {
        let running = &self.remakes.running;
        if running.is_empty() {
            return None;
        }
        let share = running.values().map(|r| r.progress).sum::<f64>() / running.len() as f64;
        let steadying = running.values().any(|r| r.spec.stabilize.is_some());
        Some((
            share,
            if steadying {
                "Stabilizing"
            } else {
                "Reversing"
            },
        ))
    }

    /// How far `file` is, and what is done to it, while it is being made,
    /// for a job that waits for it.
    pub(crate) fn remake_running(&self, file: &str) -> Option<(f64, &'static str)> {
        self.remakes
            .running
            .get(file)
            .map(|running| (running.progress, doing(&running.spec)))
    }

    /// Make the files a refused script edit wanted. Returns them.
    pub(crate) fn make_wanted_remakes(&mut self) -> Vec<String> {
        let wanted = std::mem::take(&mut self.remakes.wanted);
        wanted
            .into_iter()
            .map(|(file, spec)| {
                self.start_file(file.clone(), spec);
                file
            })
            .collect()
    }

    /// Write `file` from `spec` unless it is there, then read it in.
    fn start_file(&mut self, file: String, spec: Remade) {
        if self.remakes.running.contains_key(&file) {
            return;
        }
        self.remakes.failed.remove(&file);
        let cancel = Arc::new(AtomicBool::new(false));
        self.remakes.running.insert(
            file.clone(),
            Running {
                spec: spec.clone(),
                progress: 0.0,
                cancel: Arc::clone(&cancel),
            },
        );
        let tx = self.remakes.tx.clone();
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

    /// Once per frame: take in finished files and change waiting clips.
    pub(super) fn poll_remakes(&mut self) {
        while let Ok(msg) = self.remakes.rx.try_recv() {
            match msg {
                Msg::Progress(file, share) => {
                    if let Some(running) = self.remakes.running.get_mut(&file) {
                        running.progress = share;
                    }
                }
                Msg::Done(file, made) => self.file_made(file, made),
            }
        }
        if self.remaking() {
            self.frame_within(Duration::from_millis(250));
        }
    }

    fn file_made(&mut self, file: String, made: Result<Box<BinAsset>, String>) {
        let Some(running) = self.remakes.running.remove(&file) else {
            return;
        };
        let spec = running.spec;
        let name = file_name(&spec.original);
        let (waiting, others): (Vec<_>, Vec<_>) = std::mem::take(&mut self.remakes.waiting)
            .into_iter()
            .partition(|w| w.file == file);
        self.remakes.waiting = others;
        let asset = match made {
            Ok(asset) => *asset,
            Err(error) => {
                if !running.cancel.load(Ordering::Relaxed) {
                    self.status = format!("Could not make {name}'s file: {error}");
                    self.remakes.failed.insert(file, error);
                } else if !waiting.is_empty() {
                    // Stopped, then asked for again before it ended.
                    self.remakes.waiting.extend(waiting);
                    self.start_file(file, spec);
                }
                return;
            }
        };
        self.made.retain(|known| known.info.path != file);
        self.made.push(asset);
        for waited in waiting {
            let Some(clip) = self.timeline.clip(waited.clip) else {
                continue;
            };
            // The clip may have moved on while its file was written.
            let still_fits = clip.remade == waited.from
                && clip.shown_range().is_some_and(|(original, from, to)| {
                    original == spec.original && spec.holds(from, to)
                });
            if still_fits {
                let name = clip.name.clone();
                self.play_remade(waited.clip, Some((&file, &spec)));
                self.status = plays(&name, Some(&spec));
            } else {
                self.status = format!("{} changed while its file was made; try again.", clip.name);
            }
        }
    }

    /// Point every remade clip at its file, making it again where it is
    /// gone. Called when a project opens and after missing media is found.
    pub(crate) fn restore_remakes(&mut self) {
        let remade: Vec<(usize, Remade, Option<String>)> = self
            .timeline
            .clips
            .iter()
            .enumerate()
            .filter_map(|(index, clip)| Some((index, clip.remade.clone()?, clip.source.clone())))
            .collect();
        for (index, spec, source) in remade {
            let file = source
                .filter(|file| Path::new(file).exists())
                .unwrap_or_else(|| self.remade_path(&spec));
            self.timeline.clips[index].source = Some(file.clone());
            let known = self.made.iter().any(|asset| asset.info.path == file);
            if !known && Path::new(&spec.original).exists() {
                self.start_file(file, spec);
            }
        }
    }
}

/// On a worker: write `file` from `spec` unless it is there, then read it
/// as an import would.
fn make(
    file: &str,
    spec: &Remade,
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
        openreel_media::remake(spec, path, progress, cancel)?;
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

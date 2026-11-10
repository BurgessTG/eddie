//! Project files: the timeline and the media it was cut from. Curation is
//! not stored here; each file's saved curation returns when it is imported.
//!
//! Media is stored by absolute path plus the project's own folder, so a
//! project moved together with its footage still finds it. Anything still
//! missing can be located by hand.

use std::fs::{File, TryLockError};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use openreel_engine::Timeline;
use serde_json::json;

use super::media::AssetState;
use super::sweep::{SweepData, SweepStatus};
use super::Editor;

const VERSION: u64 = 1;
pub const EXTENSION: &str = "eddie";

/// Take the hold on a project's file that an editor keeps while it may save
/// it, so two editors never write over each other's changes. Waits up to
/// `wait` for whoever has it. `Ok(None)` means there is nothing to hold (no
/// file yet, or a filesystem without locks) and the project is used without.
///
/// Take it before reading the file: what was read is then what gets changed.
pub(crate) fn lock_project(path: &Path, wait: Duration) -> Result<Option<File>, String> {
    let deadline = Instant::now() + wait;
    loop {
        let Ok(file) = File::open(path) else {
            return Ok(None);
        };
        match file.try_lock() {
            // A save replaces the file. One that landed while this waited
            // leaves the lock on the file it replaced: ask for the new one.
            Ok(()) if same_file(&file, path) => return Ok(Some(file)),
            Ok(()) => continue,
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(_)) => return Ok(None),
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} is open in an Eddie this command cannot reach, or another command is \
                 still changing it",
                file_name(path)
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What an editor says when another one has the project it was given.
pub(crate) fn held_elsewhere(project: &Path) -> String {
    format!("Another Eddie already has {} open.", file_name(project))
}

/// Whether `path` still names the open `file`.
fn same_file(file: &File, path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (file.metadata(), std::fs::metadata(path)) {
            (Ok(held), Ok(named)) => (held.dev(), held.ino()) == (named.dev(), named.ino()),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, path);
        true
    }
}

impl Editor {
    /// A window or `eddie serve` holds its project's file, so a command
    /// that cannot reach this editor waits instead of changing the file
    /// under it. False when someone else holds it.
    pub(crate) fn hold_project(&mut self) -> bool {
        if self.project_lock.is_some() {
            return true;
        }
        let Some(path) = &self.project_path else {
            return true;
        };
        match lock_project(path, Duration::ZERO) {
            Ok(lock) => {
                self.project_lock = lock;
                true
            }
            Err(_) => false,
        }
    }

    /// What a save would write differs from the last project saved or
    /// opened: the timeline, or the music-bed choice stored with it.
    pub fn has_unsaved_changes(&self) -> bool {
        self.timeline != self.saved_timeline || self.mute_music_beds != self.saved_mute_music_beds
    }

    pub fn save_project_to(&mut self, path: &Path) -> Result<(), String> {
        let path = if path.extension().is_some_and(|ext| ext == EXTENSION) {
            path.to_path_buf()
        } else {
            // Added, not swapped in: "cut v1.2" must not become "cut v1".
            let mut name = path.as_os_str().to_owned();
            name.push(".");
            name.push(EXTENSION);
            PathBuf::from(name)
        };
        let path = std::path::absolute(&path).unwrap_or(path);
        let body = json!({
            "version": VERSION,
            "timeline": self.timeline,
            // Missing files stay listed until they are located.
            "media": self
                .assets
                .iter()
                .map(|asset| &asset.info.path)
                .chain(&self.missing_media)
                .collect::<Vec<_>>(),
            "projectDir": path.parent().map(Path::to_string_lossy),
            "muteMusicBeds": self.mute_music_beds,
        });
        let name = file_name(&path);
        let temporary = path.with_extension(format!("{EXTENSION}.{}.tmp", std::process::id()));
        let hold = self.listen || self.project_lock.is_some();
        let result = (|| -> std::io::Result<Option<File>> {
            let mut file = File::create(&temporary)?;
            file.write_all(serde_json::to_string_pretty(&body)?.as_bytes())?;
            file.sync_all()?;
            // Held before it has the project's name, so the hold passes to
            // the new file with no moment in between.
            let held = hold && file.try_lock().is_ok();
            std::fs::rename(&temporary, &path)?;
            Ok(held.then_some(file))
        })();
        match result {
            Ok(lock) => self.project_lock = lock,
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                return Err(format!("Couldn't save {name}: {error}"));
            }
        }
        self.project_path = Some(path);
        self.saved_timeline = self.timeline.clone();
        self.saved_mute_music_beds = self.mute_music_beds;
        self.status = format!("Saved {name}.");
        Ok(())
    }

    /// Replace the timeline with a saved one and import its media. The
    /// media bin keeps what is already there.
    pub fn open_project(&mut self, path: &Path) -> Result<(), String> {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let name = file_name(&path);
        // Read first: a file that is not a project stops nobody's editor.
        let mut read = read_project(&path)?;
        if self.take_over(&path) {
            // What that editor saved last.
            read = read_project(&path)?;
        }
        let (mut value, mut timeline) = read;
        timeline.ensure_fresh_ids();
        let media: Vec<String> = serde_json::from_value(value["media"].take()).unwrap_or_default();
        let saved_dir = value["projectDir"].as_str().map(PathBuf::from);

        self.set_playing(false);
        // Reopening the held project keeps the hold. Let go, it could not
        // always be taken back: a child process started meanwhile keeps a
        // copy until it execs.
        let held = self.project_path.as_deref() == Some(path.as_path())
            && self
                .project_lock
                .as_ref()
                .is_some_and(|file| same_file(file, &path));
        if !held {
            self.project_lock = None;
        }
        self.settings.format = timeline.orientation;
        self.timeline = timeline;
        self.selected = None;
        self.playhead = 0.0;
        self.forget_stills();
        if let Some(mute) = value["muteMusicBeds"].as_bool() {
            self.mute_music_beds = mute;
        }

        let mut wanted = media;
        for clip in &self.timeline.clips {
            if let Some(source) = &clip.source {
                if !wanted.contains(source) {
                    wanted.push(source.clone());
                }
            }
        }
        // Media that moved with the project keeps its place relative to it.
        if let (Some(old_dir), Some(new_dir)) = (saved_dir, path.parent()) {
            for source in &mut wanted {
                if Path::new(source).exists() {
                    continue;
                }
                let Some(moved) = relative_to(Path::new(source), &old_dir)
                    .map(|relative| new_dir.join(relative))
                    .filter(|moved| moved.exists())
                else {
                    continue;
                };
                let moved = moved.to_string_lossy().into_owned();
                relink_clips(&mut self.timeline, source, &moved);
                *source = moved;
            }
        }
        self.history.reset(&self.timeline);
        self.saved_timeline = self.timeline.clone();
        self.saved_mute_music_beds = self.mute_music_beds;

        let (present, missing): (Vec<_>, Vec<_>) = wanted
            .into_iter()
            .filter(|path| !self.assets.iter().any(|asset| asset.info.path == *path))
            .partition(|path| Path::new(path).exists());
        self.spawn_import(present.into_iter().map(PathBuf::from).collect());
        self.project_path = Some(path.clone());
        if self.listen {
            self.hold_project();
        }
        self.missing_media = missing;
        self.status = match self.missing_media.len() {
            0 => format!("Opened {name}."),
            count => format!(
                "Opened {name}; {count} media file{} missing. Use File → Locate missing media.",
                if count == 1 { " is" } else { "s are" }
            ),
        };
        Ok(())
    }

    /// Point the first missing file at `picked`. Other missing files with
    /// the same name in that folder are relinked too.
    pub fn locate_missing_media(&mut self, picked: &Path) {
        if self.missing_media.is_empty() {
            return;
        }
        let first = self.missing_media.remove(0);
        let mut found = vec![(first, picked.to_path_buf())];
        if let Some(folder) = picked.parent() {
            self.missing_media.retain(|missing| {
                let candidate = folder.join(file_name(Path::new(missing)));
                if candidate.exists() {
                    found.push((missing.clone(), candidate));
                    false
                } else {
                    true
                }
            });
        }
        for (old, new) in &found {
            let new = new.to_string_lossy();
            relink_clips(&mut self.timeline, old, &new);
            // Earlier steps too: undo must not bring the lost path back
            // once it is off the missing list.
            self.history
                .rewrite(|timeline| relink_clips(timeline, old, &new));
        }
        let count = found.len();
        self.spawn_import(found.into_iter().map(|(_, new)| new).collect());
        self.status = match self.missing_media.len() {
            0 => format!("Relinked {count} media file{}.", plural(count)),
            left => format!(
                "Relinked {count} media file{}; {left} still missing.",
                plural(count)
            ),
        };
    }

    /// An empty timeline and bin, in the current format. Files still
    /// importing or being curated stay so their work is not lost.
    pub fn new_project(&mut self) {
        self.set_playing(false);
        self.timeline = Timeline::empty_project().with_orientation(self.timeline.orientation);
        self.history.reset(&self.timeline);
        self.saved_timeline = self.timeline.clone();
        self.saved_mute_music_beds = self.mute_music_beds;
        self.project_path = None;
        self.project_lock = None;
        self.missing_media.clear();
        self.selected = None;
        self.playhead = 0.0;
        self.forget_stills();
        self.sweep_queue.clear();
        let running = &self.sweep_running;
        self.assets
            .retain(|asset| asset.state != AssetState::Ready || running.contains(&asset.info.path));
        for asset in &mut self.assets {
            if asset.sweep.status == SweepStatus::Running && !running.contains(&asset.info.path) {
                asset.sweep = SweepData::default();
            }
        }
        self.selected_asset = None;
        self.status = match self.assets.len() {
            0 => "New project.".to_owned(),
            kept => format!(
                "New project; kept {kept} file{} still importing or curating.",
                plural(kept)
            ),
        };
    }
}

/// A project file's contents and its timeline, checked.
fn read_project(path: &Path) -> Result<(serde_json::Value, Timeline), String> {
    let name = file_name(path);
    let unreadable = || format!("{name} is not an Eddie project.");
    let raw = std::fs::read(path).map_err(|error| format!("Couldn't open {name}: {error}"))?;
    let mut value: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| unreadable())?;
    if value["version"].as_u64() != Some(VERSION) {
        return Err(unreadable());
    }
    let timeline: Timeline =
        serde_json::from_value(value["timeline"].take()).map_err(|_| unreadable())?;
    // A hand-edited or damaged file: a zero frame rate or a clip days
    // away would break every later calculation.
    if !timeline.in_supported_range() {
        return Err(unreadable());
    }
    Ok((value, timeline))
}

fn relink_clips(timeline: &mut Timeline, old: &str, new: &str) {
    for clip in &mut timeline.clips {
        if clip.source.as_deref() == Some(old) {
            clip.source = Some(new.to_owned());
        }
    }
}

/// `path` relative to `base`, both absolute, using `..` where needed.
fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || !base.is_absolute() {
        return None;
    }
    let path: Vec<Component> = path.components().collect();
    let base: Vec<Component> = base.components().collect();
    let shared = path.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let mut relative: PathBuf = base[shared..]
        .iter()
        .map(|_| Component::ParentDir)
        .collect();
    relative.extend(&path[shared..]);
    Some(relative)
}

pub fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

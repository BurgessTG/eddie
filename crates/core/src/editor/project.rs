//! Project files: the timeline and the media it was cut from. Curation is
//! not stored here; each file's saved curation returns when it is imported.
//!
//! Media is stored by absolute path plus the project's own folder, so a
//! project moved together with its footage still finds it. Anything still
//! missing can be located by hand.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use openreel_engine::Timeline;
use serde_json::json;

use super::media::AssetState;
use super::sweep::{SweepData, SweepStatus};
use super::Editor;

const VERSION: u64 = 1;
pub const EXTENSION: &str = "eddie";

impl Editor {
    /// The timeline differs from the last one saved or opened.
    pub fn has_unsaved_changes(&self) -> bool {
        self.timeline != self.saved_timeline
    }

    pub fn save_project_to(&mut self, path: &Path) -> Result<(), String> {
        let path = if path.extension().is_some_and(|ext| ext == EXTENSION) {
            path.to_path_buf()
        } else {
            path.with_extension(EXTENSION)
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
            "projectDir": path.parent(),
            "muteMusicBeds": self.mute_music_beds,
        });
        let name = file_name(&path);
        let temporary = path.with_extension(format!("{EXTENSION}.{}.tmp", std::process::id()));
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(serde_json::to_string_pretty(&body)?.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temporary, &path)
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("Couldn't save {name}: {error}"));
        }
        self.project_path = Some(path);
        self.saved_timeline = self.timeline.clone();
        self.status = format!("Saved {name}.");
        Ok(())
    }

    /// Replace the timeline with a saved one and import its media. The
    /// media bin keeps what is already there.
    pub fn open_project(&mut self, path: &Path) -> Result<(), String> {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let name = file_name(&path);
        let unreadable = || format!("{name} is not an Eddie project.");
        let raw = std::fs::read(&path).map_err(|error| format!("Couldn't open {name}: {error}"))?;
        let mut value: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| unreadable())?;
        if value["version"].as_u64() != Some(VERSION) {
            return Err(unreadable());
        }
        let mut timeline: Timeline =
            serde_json::from_value(value["timeline"].take()).map_err(|_| unreadable())?;
        timeline.ensure_fresh_ids();
        let media: Vec<String> = serde_json::from_value(value["media"].take()).unwrap_or_default();
        let saved_dir = value["projectDir"].as_str().map(PathBuf::from);

        self.set_playing(false);
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

        let (present, missing): (Vec<_>, Vec<_>) = wanted
            .into_iter()
            .filter(|path| !self.assets.iter().any(|asset| asset.info.path == *path))
            .partition(|path| Path::new(path).exists());
        self.spawn_import(present.into_iter().map(PathBuf::from).collect());
        self.project_path = Some(path.clone());
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
            relink_clips(&mut self.timeline, old, &new.to_string_lossy());
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
        self.project_path = None;
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

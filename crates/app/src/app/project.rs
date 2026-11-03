//! Project files: the timeline and the media it was cut from. Curation is
//! not stored here; each file's saved curation returns when it is imported.

use std::io::Write;
use std::path::{Path, PathBuf};

use eframe::egui;
use openreel_engine::Timeline;
use serde_json::json;

use super::EditorApp;

const VERSION: u64 = 1;
pub(super) const EXTENSION: &str = "eddie";

impl EditorApp {
    pub(super) fn save_project_to(&mut self, path: &Path) -> Result<(), String> {
        let path = if path.extension().is_some_and(|ext| ext == EXTENSION) {
            path.to_path_buf()
        } else {
            path.with_extension(EXTENSION)
        };
        let body = json!({
            "version": VERSION,
            "timeline": self.timeline,
            "media": self.assets.iter().map(|asset| &asset.info.path).collect::<Vec<_>>(),
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
        self.status = format!("Saved {name}.");
        Ok(())
    }

    /// Replace the timeline with a saved one and import its media. The
    /// media bin keeps what is already there.
    pub(super) fn open_project(&mut self, ctx: &egui::Context, path: &Path) -> Result<(), String> {
        let name = file_name(path);
        let unreadable = || format!("{name} is not an Eddie project.");
        let raw = std::fs::read(path).map_err(|error| format!("Couldn't open {name}: {error}"))?;
        let mut value: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| unreadable())?;
        if value["version"].as_u64() != Some(VERSION) {
            return Err(unreadable());
        }
        let mut timeline: Timeline =
            serde_json::from_value(value["timeline"].take()).map_err(|_| unreadable())?;
        timeline.ensure_fresh_ids();
        let media: Vec<String> = serde_json::from_value(value["media"].take()).unwrap_or_default();

        self.cancel_chat();
        self.set_playing(false);
        self.chat_undo = None;
        self.chat_settings.format = timeline.orientation;
        self.timeline = timeline;
        self.history.reset(&self.timeline);
        self.selected = None;
        self.playhead = 0.0;
        self.still_key = None;
        self.still_cache.clear();
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
        let (present, missing): (Vec<_>, Vec<_>) = wanted
            .into_iter()
            .filter(|path| !self.assets.iter().any(|asset| asset.info.path == *path))
            .partition(|path| Path::new(path).exists());
        self.spawn_import(ctx, present.into_iter().map(PathBuf::from).collect());
        self.project_path = Some(path.to_path_buf());
        self.status = if missing.is_empty() {
            format!("Opened {name}.")
        } else {
            format!(
                "Opened {name}; {} media file{} missing.",
                missing.len(),
                if missing.len() == 1 { " is" } else { "s are" }
            )
        };
        Ok(())
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

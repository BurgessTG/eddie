//! A window that has no project file yet still has one: an untitled project
//! in Eddie's own folder (`untitled/` beside the settings), made when the
//! window needs it. Agents reach a window through its project's path, so
//! with this every window can be found (`eddie editors`) and driven, saved
//! or not. To its user it stays unsaved: Save asks where to put it.
//!
//! An untitled file with no clips is deleted when its window leaves it
//! (Save as, Open, New, quitting), and at the next start when a crash left
//! it behind. One an agent saved clips into is kept.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::project::{lock_project, EXTENSION};
use super::Editor;

/// The folder for untitled projects, beside the settings file.
pub fn default_dir() -> Option<PathBuf> {
    Some(super::settings::config_path()?.parent()?.join("untitled"))
}

impl Editor {
    /// The open project is the untitled one Eddie made, not one the user
    /// named.
    pub fn is_untitled(&self) -> bool {
        self.untitled.is_some() && self.untitled == self.project_path
    }

    /// A window without a project gets an untitled one; one that moved to
    /// another project lets its untitled file go.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn keep_untitled(&mut self) {
        if self.untitled.is_some() && self.untitled != self.project_path {
            // Its socket and file lock go first, so nobody holds it.
            self.server = None;
            if let Some(old) = self.untitled.take() {
                remove_if_empty(&old, OWN);
            }
        }
        if self.project_path.is_none() {
            if let Some(dir) = self.untitled_dir.clone() {
                self.start_untitled(&dir);
            }
        }
    }

    fn start_untitled(&mut self, dir: &Path) {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        // What a crash left: empty, and nobody holds it.
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == EXTENSION) {
                    remove_if_empty(&path, Duration::ZERO);
                }
            }
        }
        let path = (1..)
            .map(|n| match n {
                1 => dir.join(format!("Untitled.{EXTENSION}")),
                n => dir.join(format!("Untitled {n}.{EXTENSION}")),
            })
            .find(|path| !path.exists())
            .expect("a free name");
        // Saving says so in the status bar; making the file is not news.
        let status = std::mem::take(&mut self.status);
        let saved = self.save_project_to(&path);
        self.status = status;
        if saved.is_ok() {
            self.untitled = Some(path);
        }
    }
}

impl Drop for Editor {
    /// An untitled project with no clips goes with its editor.
    fn drop(&mut self) {
        if let Some(path) = self.untitled.take() {
            self.server = None;
            self.project_lock = None;
            remove_if_empty(&path, OWN);
        }
    }
}

/// How long an editor waits to take back the lock it just let go of on its
/// own untitled file: a child process forked at that moment holds a copy
/// until it execs. Leftovers from other editors are never waited for.
const OWN: Duration = Duration::from_millis(500);

/// Delete an untitled project that has no clips and that no editor or
/// command holds.
fn remove_if_empty(path: &Path, wait: Duration) {
    let Ok(Some(_held)) = lock_project(path, wait) else {
        return;
    };
    let empty = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .is_some_and(|project| {
            project["timeline"]["clips"]
                .as_array()
                .is_none_or(Vec::is_empty)
        });
    if empty {
        let _ = std::fs::remove_file(path);
    }
}

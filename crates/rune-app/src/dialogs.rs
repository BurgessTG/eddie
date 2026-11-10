//! File dialogs and the "Save changes?" prompt around the editor's project
//! files.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eddie_core::editor::MEDIA_EXTS;
use eddie_core::editor::project::{EXTENSION, file_name};
use rune::{El, col, el, row};

use crate::app::Ui;
use crate::theme::{self, Variant, c};

/// What a file dialog's answer is for. Dialogs run on a worker thread:
/// GTK's blocking dialog would stop the UI loop, and the window manager
/// would mark Eddie "not responding" while the user picks a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePurpose {
    Import,
    Export,
    OpenProject,
    /// Save the project there, then carry on with the pending discard.
    SaveProject(Option<Discard>),
    LocateMedia,
}

pub type PendingFileDialog = (FilePurpose, Receiver<Option<Vec<PathBuf>>>);

/// What waits on the "Save changes?" answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Discard {
    Close,
    Open,
    New,
}

impl Ui {
    pub fn pick_import(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("Media", MEDIA_EXTS)
            .set_title("Import media");
        self.open_file_dialog(FilePurpose::Import, dialog);
    }

    pub fn pick_export(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("MP4 video", &["mp4"])
            .set_file_name("export.mp4")
            .set_title("Export MP4");
        self.open_file_dialog(FilePurpose::Export, dialog);
    }

    /// Save to the current project file, asking for one when there is none
    /// (or always, for Save as). `then` runs once the save succeeds.
    pub fn save_project(&mut self, ask: bool, then: Option<Discard>) {
        match (&self.editor.project_path, ask) {
            (Some(path), false) => {
                let path = path.clone();
                self.save_project_at(&path, then);
            }
            _ => self.open_file_dialog(
                FilePurpose::SaveProject(then),
                rfd::FileDialog::new()
                    .add_filter("Eddie project", &[EXTENSION])
                    .set_file_name("project.eddie")
                    .set_title("Save project"),
            ),
        }
    }

    fn save_project_at(&mut self, path: &Path, then: Option<Discard>) {
        match self.editor.save_project_to(path) {
            Ok(()) => {
                if let Some(discard) = then {
                    self.discard(discard);
                }
            }
            Err(error) => self.editor.status = error,
        }
    }

    /// Show `dialog` without blocking the UI; `poll_file_dialog` acts on
    /// the answer. One dialog at a time.
    fn open_file_dialog(&mut self, purpose: FilePurpose, dialog: rfd::FileDialog) {
        if self.file_dialog.is_some() {
            self.editor.status = "A file dialog is already open.".to_owned();
            return;
        }
        let (tx, rx) = channel();
        let wake = self.waker();
        std::thread::spawn(move || {
            let picked = match purpose {
                FilePurpose::Import => dialog.pick_files(),
                FilePurpose::Export | FilePurpose::SaveProject(_) => {
                    dialog.save_file().map(|path| vec![path])
                }
                FilePurpose::OpenProject | FilePurpose::LocateMedia => {
                    dialog.pick_file().map(|path| vec![path])
                }
            };
            let _ = tx.send(picked);
            wake();
        });
        self.file_dialog = Some((purpose, rx));
    }

    pub fn poll_file_dialog(&mut self) {
        let Some((purpose, rx)) = &self.file_dialog else {
            return;
        };
        let purpose = *purpose;
        let picked = match rx.try_recv() {
            Ok(picked) => picked,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => None,
        };
        self.file_dialog = None;
        let Some(mut paths) = picked.filter(|paths| !paths.is_empty()) else {
            return;
        };
        match purpose {
            FilePurpose::Import => self.editor.spawn_import(paths),
            FilePurpose::Export => self.editor.start_export(paths.remove(0)),
            FilePurpose::SaveProject(then) => self.save_project_at(&paths[0], then),
            FilePurpose::OpenProject => {
                if let Err(error) = self.editor.open_project(&paths[0]) {
                    self.editor.status = error;
                }
            }
            FilePurpose::LocateMedia => self.editor.locate_missing_media(&paths[0]),
        }
    }

    fn pick_and_open_project(&mut self) {
        self.open_file_dialog(
            FilePurpose::OpenProject,
            rfd::FileDialog::new()
                .add_filter("Eddie project", &[EXTENSION])
                .set_title("Open project"),
        );
    }

    pub fn pick_missing_media(&mut self) {
        let Some(first) = self.editor.missing_media.first() else {
            return;
        };
        let dialog = rfd::FileDialog::new()
            .add_filter("Media", MEDIA_EXTS)
            .set_title(format!("Locate {}", file_name(Path::new(first))));
        self.open_file_dialog(FilePurpose::LocateMedia, dialog);
    }

    /// Ask before Close, Open or New drop an unsaved timeline.
    pub fn request_discard(&mut self, discard: Discard) {
        if self.editor.has_unsaved_changes() {
            self.pending_discard = Some(discard);
        } else {
            self.discard(discard);
        }
    }

    fn discard(&mut self, discard: Discard) {
        self.pending_discard = None;
        match discard {
            Discard::Close => self.close(),
            Discard::Open => self.pick_and_open_project(),
            Discard::New => self.editor.new_project(),
        }
    }

    /// The prompt's answer: save first, or drop the edits.
    pub fn answer_discard(&mut self, discard: Discard, save: bool) {
        if save {
            self.pending_discard = None;
            self.save_project(false, Some(discard));
        } else {
            self.discard(discard);
        }
    }
}

/// The "Save changes?" prompt, over a scrim that keeps the editor inert.
pub fn unsaved(ui: &Ui, overlays: &mut Vec<El>) {
    let Some(discard) = ui.pending_discard else {
        return;
    };
    let c = c();
    let name = ui
        .editor
        .project_path
        .as_deref()
        .map_or_else(|| "this project".to_owned(), file_name);
    let cancel = |ui: &mut Ui| ui.pending_discard = None;
    let buttons = row()
        .gap(8.0)
        .child(
            theme::button(
                "unsaved-save",
                "Save",
                Variant::Primary,
                "Save the project, then continue (Enter)",
                true,
            )
            .on_press(move |ui: &mut Ui| ui.answer_discard(discard, true)),
        )
        .child(
            theme::button(
                "unsaved-drop",
                "Don't save",
                Variant::Secondary,
                "Continue and drop the unsaved edits",
                true,
            )
            .on_press(move |ui: &mut Ui| ui.answer_discard(discard, false)),
        )
        .child(
            theme::button(
                "unsaved-cancel",
                "Cancel",
                Variant::Ghost,
                "Keep editing (Esc)",
                true,
            )
            .on_press(cancel),
        );
    let card = col()
        .key("unsaved-changes")
        .w(380.0)
        .padding(20.0)
        .gap(10.0)
        .fill(c.card)
        .border(1.0, c.border)
        .radius(theme::R_LG)
        .shadow(8.0, 28.0, rune::Color::BLACK.alpha(140.0 / 255.0))
        .modal(false, cancel)
        .child(theme::panel_title("Save changes?"))
        .child(theme::caption(format!(
            "The timeline in {name} has unsaved edits."
        )))
        .child(buttons);
    overlays.push(
        el().key("unsaved-scrim")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .fill(rune::Color::BLACK.alpha(0.5)),
    );
    overlays.push(
        row()
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .center()
            .child(card),
    );
}

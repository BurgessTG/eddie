//! File dialogs and the "Save changes?" prompt around the editor's
//! project files.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use eddie_core::editor::project::{file_name, EXTENSION};
use eddie_core::editor::MEDIA_EXTS;
use eframe::egui;

use super::EditorApp;
use crate::theme;

/// What a file dialog's answer is for. Dialogs run on a worker thread:
/// GTK's blocking dialog would stop the UI loop, and the window manager
/// would mark Eddie "not responding" while the user picks a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FilePurpose {
    Import,
    Export,
    OpenProject,
    /// Save the project there, then carry on with the pending discard.
    SaveProject(Option<Discard>),
    LocateMedia,
}

pub(super) type PendingFileDialog = (FilePurpose, Receiver<Option<Vec<PathBuf>>>);

/// What waits on the "Save changes?" answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Discard {
    Close,
    Open,
    New,
}

impl EditorApp {
    /// Save to the current project file, asking for one when there is none
    /// (or always, for Save as). `then` runs once the save succeeds.
    pub(super) fn save_project(&mut self, ctx: &egui::Context, ask: bool, then: Option<Discard>) {
        match (&self.editor.project_path, ask) {
            (Some(path), false) => {
                let path = path.clone();
                self.save_project_at(ctx, &path, then);
            }
            _ => self.open_file_dialog(
                ctx,
                FilePurpose::SaveProject(then),
                rfd::FileDialog::new()
                    .add_filter("Eddie project", &[EXTENSION])
                    .set_file_name("project.eddie")
                    .set_title("Save project"),
            ),
        }
    }

    fn save_project_at(&mut self, ctx: &egui::Context, path: &Path, then: Option<Discard>) {
        match self.editor.save_project_to(path) {
            Ok(()) => {
                if let Some(discard) = then {
                    self.discard(ctx, discard);
                }
            }
            Err(error) => self.editor.status = error,
        }
    }

    /// Show `dialog` without blocking the UI; `poll_file_dialog` acts on
    /// the answer. One dialog at a time.
    pub(super) fn open_file_dialog(
        &mut self,
        ctx: &egui::Context,
        purpose: FilePurpose,
        dialog: rfd::FileDialog,
    ) {
        if self.file_dialog.is_some() {
            self.editor.status = "A file dialog is already open.".to_owned();
            return;
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
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
            ctx.request_repaint();
        });
        self.file_dialog = Some((purpose, rx));
    }

    pub(super) fn poll_file_dialog(&mut self, ctx: &egui::Context) {
        let Some((purpose, rx)) = &self.file_dialog else {
            return;
        };
        let purpose = *purpose;
        let picked = match rx.try_recv() {
            Ok(picked) => picked,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        self.file_dialog = None;
        let Some(mut paths) = picked.filter(|paths| !paths.is_empty()) else {
            return;
        };
        match purpose {
            FilePurpose::Import => self.editor.spawn_import(paths),
            FilePurpose::Export => self.editor.start_export(paths.remove(0)),
            FilePurpose::SaveProject(then) => self.save_project_at(ctx, &paths[0], then),
            FilePurpose::OpenProject => {
                if let Err(error) = self.editor.open_project(&paths[0]) {
                    self.editor.status = error;
                }
            }
            FilePurpose::LocateMedia => self.editor.locate_missing_media(&paths[0]),
        }
    }

    pub(super) fn pick_and_open_project(&mut self, ctx: &egui::Context) {
        self.open_file_dialog(
            ctx,
            FilePurpose::OpenProject,
            rfd::FileDialog::new()
                .add_filter("Eddie project", &[EXTENSION])
                .set_title("Open project"),
        );
    }

    pub(super) fn pick_missing_media(&mut self, ctx: &egui::Context) {
        let Some(first) = self.editor.missing_media.first() else {
            return;
        };
        let dialog = rfd::FileDialog::new()
            .add_filter("Media", MEDIA_EXTS)
            .set_title(format!("Locate {}", file_name(Path::new(first))));
        self.open_file_dialog(ctx, FilePurpose::LocateMedia, dialog);
    }

    /// Ask before Close or Open drop an unsaved timeline.
    pub(super) fn request_discard(&mut self, ctx: &egui::Context, discard: Discard) {
        if self.editor.has_unsaved_changes() {
            self.pending_discard = Some(discard);
        } else {
            self.discard(ctx, discard);
        }
    }

    fn discard(&mut self, ctx: &egui::Context, discard: Discard) {
        self.pending_discard = None;
        match discard {
            Discard::Close => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Discard::Open => self.pick_and_open_project(ctx),
            Discard::New => self.editor.new_project(),
        }
    }

    pub(super) fn guard_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && !self.allow_close
            && self.editor.has_unsaved_changes()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending_discard = Some(Discard::Close);
        }
    }

    pub(super) fn unsaved_dialog(&mut self, ctx: &egui::Context) {
        let Some(discard) = self.pending_discard else {
            return;
        };
        let name = self
            .editor
            .project_path
            .as_deref()
            .map_or_else(|| "this project".to_owned(), file_name);
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::new("unsaved-changes"))
            .frame(theme::card_frame().inner_margin(egui::Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(340.0);
                ui.spacing_mut().item_spacing.y = 10.0;
                theme::panel_title(ui, "Save changes?");
                ui.label(theme::caption(format!(
                    "The timeline in {name} has unsaved edits."
                )));
                ui.horizontal(|ui| {
                    if theme::primary_button(ui, "Save")
                        .on_hover_text("Save the project, then continue (Enter)")
                        .clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        choice = Some(true);
                    }
                    if theme::secondary_button(ui, "Don't save")
                        .on_hover_text("Continue and drop the unsaved edits")
                        .clicked()
                    {
                        choice = Some(false);
                    }
                    if theme::ghost_button(ui, "Cancel")
                        .on_hover_text("Keep editing (Esc)")
                        .clicked()
                    {
                        self.pending_discard = None;
                    }
                });
            });
        if modal.should_close() {
            self.pending_discard = None;
        }
        match choice {
            Some(true) => {
                self.pending_discard = None;
                self.save_project(ctx, false, Some(discard));
            }
            Some(false) => self.discard(ctx, discard),
            None => {}
        }
    }
}

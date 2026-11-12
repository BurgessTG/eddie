//! The File, Edit and View menus. Eddie draws its own so every entry has a
//! tooltip and the shortcut hints come from the one shortcut table.

use eddie_core::Action;
use eddie_core::shortcuts;
use rune::{Align, Color, Cursor, El, Shape, Side, col, el, row, spacer, text};

use crate::app::Ui;
use crate::theme::{self, c};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    File,
    Edit,
    View,
}

impl Menu {
    pub const ALL: [Menu; 3] = [Menu::File, Menu::Edit, Menu::View];

    pub fn label(self) -> &'static str {
        match self {
            Menu::File => "File",
            Menu::Edit => "Edit",
            Menu::View => "View",
        }
    }

    /// The key of the menu bar button this menu hangs from.
    pub fn anchor(self) -> &'static str {
        match self {
            Menu::File => "menu-file",
            Menu::Edit => "menu-edit",
            Menu::View => "menu-view",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Menu::File => "Project, import and export",
            Menu::Edit => "Undo, split, delete and titles",
            Menu::View => "Panels, zoom and the playhead",
        }
    }
}

/// Entries share one width so shortcut hints line up.
const WIDTH: f32 = 236.0;
const ROW_H: f32 = 26.0;

fn row_frame(key: &str, tooltip: String, enabled: bool) -> El {
    let c = c();
    let frame = row()
        .key(key.to_owned())
        .w(WIDTH)
        .h(ROW_H)
        .px(8.0)
        .gap(8.0)
        .center_y()
        .radius(theme::R_SM)
        .help(tooltip);
    if enabled {
        frame
            .hover(c.secondary)
            .press(c.hover)
            .cursor(Cursor::Pointer)
    } else {
        frame.disabled(true)
    }
}

/// A menu entry with a right-aligned shortcut hint. It closes the menu.
fn item(label: &str, tooltip: &str, enabled: bool, action: Action) -> El {
    let c = c();
    let fg = if enabled {
        c.foreground
    } else {
        c.foreground.alpha(0.45)
    };
    let tooltip = if enabled {
        tooltip.to_owned()
    } else {
        format!("{tooltip}. Select a clip first")
    };
    let mut entry = row_frame(&format!("menu-item-{action:?}"), tooltip, enabled)
        .child(
            text(label.to_owned())
                .font(theme::BODY)
                .color(fg)
                .single_line(),
        )
        .child(spacer());
    let shortcut = shortcuts::label(action);
    if !shortcut.is_empty() {
        entry = entry.child(
            text(shortcut)
                .key(format!("menu-hint-{action:?}"))
                .font(theme::BODY)
                .color(c.subtle_fg)
                .single_line(),
        );
    }
    if enabled {
        entry.on_press(move |ui: &mut Ui| {
            ui.menu = None;
            ui.act(action);
        })
    } else {
        entry
    }
}

/// A row in the destructive colour, for something that discards work. The
/// caller adds `on_press`.
pub fn destructive_row(key: &str, label: &str, tooltip: &str) -> El {
    row_frame(key, tooltip.to_owned(), true).child(
        text(label.to_owned())
            .font(theme::BODY)
            .color(c().destructive)
            .single_line(),
    )
}

/// A menu entry for an action that discards work. It closes the menu.
fn destructive(label: &str, tooltip: &str, action: Action) -> El {
    destructive_row(&format!("menu-item-{action:?}"), label, tooltip).on_press(
        move |ui: &mut Ui| {
            ui.menu = None;
            ui.act(action);
        },
    )
}

/// The raised box a menu's rows sit in.
pub fn frame(key: &str) -> El {
    let c = c();
    col()
        .key(key.to_owned())
        .padding(4.0)
        .fill(c.card)
        .border(1.0, c.border)
        .radius(theme::R_MD)
        .shadow(8.0, 24.0, Color::BLACK.alpha(0.45))
}

/// A panel toggle: the menu stays open so several can be switched.
fn check(key: &str, label: &str, tooltip: &str, on: bool, toggle: fn(&mut Ui)) -> El {
    let c = c();
    let mut mark = Shape::new().view_box(14.0, 14.0);
    if on {
        mark = mark.stroke(vec![[3.4, 7.3], [6.0, 9.8], [10.6, 4.4]], 1.6, c.primary_fg);
    }
    let tick = el()
        .size(14.0, 14.0)
        .radius(4.0)
        .fill(if on { c.primary } else { c.input })
        .border(1.0, if on { c.primary } else { c.border_strong })
        .shape(mark);
    row_frame(key, tooltip.to_owned(), true)
        .child(tick)
        .child(
            text(label.to_owned())
                .font(theme::BODY)
                .color(c.foreground)
                .single_line(),
        )
        .on_press(toggle)
}

fn separator() -> El {
    el().w(WIDTH).h(1.0).my(4.0).fill(c().border)
}

fn entries(ui: &Ui, menu: Menu) -> Vec<El> {
    match menu {
        Menu::File => file(ui),
        Menu::Edit => edit(ui),
        Menu::View => view(ui),
    }
}

fn file(ui: &Ui) -> Vec<El> {
    let missing = ui.editor.missing_media.len();
    let locate = if missing > 0 {
        format!("Locate missing media ({missing})…")
    } else {
        "Locate missing media…".to_owned()
    };
    // Unlike the other disabled entries, this one needs no clip.
    let locate = if missing > 0 {
        item(
            &locate,
            "Find a project's moved media files",
            true,
            Action::LocateMedia,
        )
    } else {
        row_frame(
            "menu-item-LocateMedia",
            "Find a project's moved media files. No media is missing".to_owned(),
            false,
        )
        .child(
            text(locate)
                .font(theme::BODY)
                .color(c().foreground.alpha(0.45))
                .single_line(),
        )
    };
    vec![
        item(
            "New project",
            "Start an empty project with an empty media bin",
            true,
            Action::NewProject,
        ),
        item(
            "Open project…",
            "Open a saved timeline and its media",
            true,
            Action::OpenProject,
        ),
        item(
            "Save project",
            "Save the timeline and its media list",
            true,
            Action::SaveProject,
        ),
        item(
            "Save project as…",
            "Save the project to a new file",
            true,
            Action::SaveProjectAs,
        ),
        locate,
        separator(),
        item(
            "Import media…",
            "Import video or audio files",
            true,
            Action::Import,
        ),
        item(
            "Export MP4…",
            "Export the timeline as an MP4",
            true,
            Action::Export,
        ),
        separator(),
        destructive(
            "Clear timeline",
            "Remove every clip from the timeline; imported media stays",
            Action::ClearTimeline,
        ),
    ]
}

fn edit(ui: &Ui) -> Vec<El> {
    let has_selection = ui.editor.selected.is_some();
    vec![
        item("Undo", "Undo the last timeline edit", true, Action::Undo),
        item("Redo", "Redo the edit you undid", true, Action::Redo),
        separator(),
        item(
            "Split at playhead",
            "Split the selected clip at the playhead",
            has_selection,
            Action::SplitSelected,
        ),
        item(
            "Delete clip",
            "Delete the selected clip",
            has_selection,
            Action::DeleteSelected,
        ),
        separator(),
        item(
            "Add title",
            "Add a title clip at the playhead",
            true,
            Action::AddTitle,
        ),
    ]
}

fn view(ui: &Ui) -> Vec<El> {
    vec![
        check(
            "menu-check-media",
            "Media bin",
            "Show the media bin and curator card",
            ui.show_media,
            |ui| ui.show_media = !ui.show_media,
        ),
        check(
            "menu-check-details",
            "Details",
            "Show the selected clip's details in the right dock",
            ui.show_inspector,
            |ui| ui.show_inspector = !ui.show_inspector,
        ),
        check(
            "menu-check-console",
            "JS console",
            "Debug console for the editing script API",
            ui.show_console,
            |ui| ui.show_console = !ui.show_console,
        ),
        separator(),
        item(
            "Zoom in",
            "Zoom the timeline in around the playhead",
            true,
            Action::ZoomIn,
        ),
        item(
            "Zoom out",
            "Zoom the timeline out around the playhead",
            true,
            Action::ZoomOut,
        ),
        item(
            "Fit timeline",
            "Zoom so the whole timeline fits",
            true,
            Action::ZoomFit,
        ),
        separator(),
        item(
            "Go to start",
            "Move the playhead to the start",
            true,
            Action::GoStart,
        ),
        item(
            "Go to end",
            "Move the playhead to the end",
            true,
            Action::GoEnd,
        ),
        separator(),
        item(
            "Keyboard shortcuts",
            "List every keyboard shortcut",
            true,
            Action::ShowShortcuts,
        ),
    ]
}

/// The open menu, hung under its menu bar button. A press outside closes it.
pub fn overlay(ui: &Ui, overlays: &mut Vec<El>) {
    let Some(menu) = ui.menu else {
        return;
    };
    let close = |ui: &mut Ui| ui.menu = None;
    overlays.push(rune_kit::scrim("menu-scrim", close));
    overlays.push(
        frame("menu")
            .children(entries(ui, menu))
            .anchored(menu.anchor(), Side::Bottom, Align::Start, 4.0)
            .modal(true, close),
    );
}

#[cfg(test)]
mod tests {
    use eddie_core::shortcuts::BINDINGS;

    use super::Menu;
    use crate::app::Ui;
    use crate::testing::{frames, window};

    /// At 210 points wide the "Save project as..." shortcut ran into its
    /// row's padding, further with "Cmd" than with "Ctrl".
    #[test]
    fn every_shortcut_hint_stays_inside_its_row() {
        let mut rt = window(Ui::headless());
        let mut hints = 0;
        for menu in Menu::ALL {
            rt.state.menu = Some(menu);
            frames(&mut rt, 30);
            for binding in BINDINGS {
                let action = binding.action;
                let row = rt.presented(&format!("menu-item-{action:?}"));
                let hint = rt.presented(&format!("menu-hint-{action:?}"));
                let (Some((row, _)), Some((hint, _))) = (row, hint) else {
                    continue;
                };
                hints += 1;
                let past = (hint[0] + hint[2]) - (row[0] + row[2] - 8.0);
                assert!(
                    past <= 0.0,
                    "{action:?}: the shortcut runs {past} points into the row's padding"
                );
            }
        }
        assert!(hints >= 12, "only {hints} shortcut hints were measured");
    }
}

//! Eddie's window, drawn with Rune. The editor itself is `eddie-core`; this
//! crate only shows it and turns pointer and key input into its calls.

mod app;
mod console;
mod details;
mod dialogs;
#[cfg(target_os = "macos")]
mod finder;
mod gallery;
mod icons;
mod keys;
mod media;
mod menus;
mod monitor;
mod settings;
mod shell;
#[cfg(test)]
mod testing;
mod theme;
mod timeline;
mod updates;

use std::path::PathBuf;

use rune::{App, AppSpec};

use app::Ui;

fn main() {
    // Before anything reads settings, saved curations or local models.
    eddie_core::editor::settings::adopt_old_config();
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(code) = eddie_core::cli_main(&args) {
        std::process::exit(code);
    }
    // Rune reads its own flags (`--snap`, `--size`, ...) and hands back the
    // rest: media and project files to open.
    let imports: Vec<PathBuf> = rune::extra_args()
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .map(PathBuf::from)
        .collect();
    // Matches eddie.desktop, so docks and launchers show the icon.
    rune::set_app_id("eddie");
    let spec = AppSpec {
        min_size: Some((1024.0, 640.0)),
        // Readouts and the console use the system's monospace font.
        families: ("Inter", "ui-monospace, Inter"),
        poll: Some(app::poll),
        on_key: Some(app::on_key),
        ..AppSpec::new("Eddie", (1440.0, 900.0), Ui::unopened, app::view)
    };
    let mut app = App::new(spec)
        .keep_running()
        .should_close(app::should_close)
        .should_quit(app::should_close)
        .on_close(app::on_close);
    // macOS draws no titlebar of its own over a Rune window, so the menu bar
    // is the titlebar there. Linux keeps its native frame.
    if cfg!(target_os = "macos") {
        app = app.titlebar(shell::titlebar());
    }
    let handle = app.handle();
    // Finder hands over files as events, not arguments. One that arrives
    // before the window exists waits in the handle's queue for it.
    #[cfg(target_os = "macos")]
    {
        let opened = handle.clone();
        finder::listen(move |paths| opened.update(move |ui, _| ui.open_files(paths)));
    }
    app.init(move || Ui::open(handle, imports)).run();
}

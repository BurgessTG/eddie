//! Eddie's window, drawn with Rune. The editor itself is `eddie-core`; this
//! crate only shows it and turns pointer and key input into its calls.

mod app;
mod details;
mod dialogs;
mod icons;
mod media;
mod menus;
mod monitor;
mod shell;
mod theme;
mod timeline;

use std::path::PathBuf;

use rune::{App, AppSpec};

use app::Ui;

fn main() {
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
    let app = App::new(spec).keep_running().on_close(app::on_close);
    let handle = app.handle();
    app.init(move || Ui::open(handle, imports)).run();
}

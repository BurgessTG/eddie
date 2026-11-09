mod app;
mod preview;
mod theme;
mod timeline_ui;

use app::EditorApp;

fn main() -> eframe::Result<()> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(code) = eddie_core::cli_main(&args) {
        std::process::exit(code);
    }
    let initial_imports: Vec<std::path::PathBuf> = std::env::args_os()
        .skip(1)
        .filter(|arg| !arg.to_string_lossy().starts_with('-'))
        .map(std::path::PathBuf::from)
        .collect();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Eddie")
            // Matches eddie.desktop, so docks and launchers show the icon.
            .with_app_id("eddie")
            .with_icon(eframe::egui::IconData {
                rgba: theme::icon_rgba(256),
                width: 256,
                height: 256,
            })
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([1024.0, 640.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "Eddie",
        options,
        Box::new(move |cc| Ok(Box::new(EditorApp::new(cc, initial_imports)))),
    )
}

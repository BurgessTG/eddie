mod app;
#[cfg(test)]
mod dev_harness;
mod preview;
mod scripting;
mod theme;
mod timeline_ui;

use app::EditorApp;

fn main() -> eframe::Result<()> {
    let initial_imports: Vec<std::path::PathBuf> = std::env::args_os()
        .skip(1)
        .filter(|arg| !arg.to_string_lossy().starts_with('-'))
        .map(std::path::PathBuf::from)
        .collect();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("OpenReel")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([1024.0, 640.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "OpenReel",
        options,
        Box::new(move |cc| Ok(Box::new(EditorApp::new(cc, initial_imports)))),
    )
}

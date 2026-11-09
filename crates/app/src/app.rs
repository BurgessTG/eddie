//! The egui window around `eddie_core::Editor`: panels, dialogs and the
//! pixels of the program monitor. Everything the editor does without a
//! window lives in the core.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use eddie_core::editor::MEDIA_EXTS;
use eddie_core::{Action, Editor, PreviewSink, Wake};
use eframe::egui::{
    self, ColorImage, CornerRadius, RichText, Sense, Shape, Stroke, TextureHandle, TextureOptions,
    Ui, Vec2,
};
use openreel_engine::{format_timecode, ClipKind, TitleStyle};
use openreel_media::RawFrame;

use crate::preview;
use crate::theme;

mod gallery;
mod media;
mod project;
#[cfg(test)]
mod screenshot;
mod settings;
mod shortcuts;
use crate::timeline_ui::{ClipVisual, TimelineView, VisualMap};
use gallery::GalleryThumbnails;

/// Preview pictures kept for reuse once the renderer has uploaded them.
const PREVIEW_IMAGES: usize = 3;

/// The program monitor's texture. The editor fills it through
/// `MonitorSink`; the central panel draws it.
struct Monitor {
    ctx: egui::Context,
    texture: Option<TextureHandle>,
    /// Pixels of recent uploads; one the renderer has released is refilled
    /// instead of allocating a frame-sized image per upload.
    images: Vec<Arc<ColorImage>>,
}

struct MonitorSink(Rc<RefCell<Monitor>>);

impl PreviewSink for MonitorSink {
    fn show(&mut self, frame: &RawFrame) {
        let monitor = &mut *self.0.borrow_mut();
        let size = [frame.width as usize, frame.height as usize];
        monitor.images.retain(|image| image.size == size);
        // The renderer drops its reference after uploading, so a released
        // image is refilled. Decoded video is opaque: RGBA is premultiplied.
        let free = monitor
            .images
            .iter_mut()
            .position(|image| Arc::get_mut(image).is_some());
        let index = free.unwrap_or_else(|| {
            if monitor.images.len() >= PREVIEW_IMAGES {
                monitor.images.remove(0);
            }
            let pixels = vec![egui::Color32::BLACK; size[0] * size[1]];
            monitor.images.push(Arc::new(ColorImage::new(size, pixels)));
            monitor.images.len() - 1
        });
        Arc::get_mut(&mut monitor.images[index])
            .expect("unshared preview image")
            .as_raw_mut()
            .copy_from_slice(&frame.rgba);
        let image = Arc::clone(&monitor.images[index]);
        match monitor.texture.as_mut() {
            Some(tex) if tex.size() == size => tex.set(image, TextureOptions::LINEAR),
            _ => {
                monitor.texture = Some(monitor.ctx.load_texture(
                    "preview",
                    image,
                    TextureOptions::LINEAR,
                ));
            }
        }
    }

    fn clear(&mut self) {
        self.0.borrow_mut().texture = None;
    }
}

pub struct EditorApp {
    editor: Editor,
    timeline_view: TimelineView,
    monitor: Rc<RefCell<Monitor>>,
    /// Filmstrip textures by media path, taken from each finished import.
    bin_thumbs: HashMap<String, Vec<(f64, TextureHandle)>>,
    gallery_thumbnails: Option<GalleryThumbnails>,
    search: String,
    /// Text typed into the playhead readout's jump box, while it is open.
    jump_text: Option<String>,

    show_media: bool,
    show_inspector: bool,
    show_console: bool,
    console_input: String,

    pending_discard: Option<project::Discard>,
    /// The open file dialog, answered on a worker thread.
    file_dialog: Option<project::PendingFileDialog>,
    /// Set once the user chose to close without (or after) saving.
    allow_close: bool,
    /// Focus the Details text box next frame (monitor caption double-click).
    focus_title_text: bool,
    show_shortcuts: bool,
    show_settings: bool,

    frame_ms_ema: f32,
    rss_mb: u64,
    frame_count: u64,
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial_imports: Vec<PathBuf>) -> Self {
        Self::around(&cc.egui_ctx, |wake, sink| {
            Editor::open(wake, sink, initial_imports)
        })
    }

    /// For tests: the same window around an editor without the user's
    /// settings, saved curations or an audio device.
    #[cfg(test)]
    fn headless(ctx: &egui::Context) -> Self {
        Self::around(ctx, Editor::headless_with)
    }

    fn around(
        ctx: &egui::Context,
        editor: impl FnOnce(Wake, Box<dyn PreviewSink>) -> Editor,
    ) -> Self {
        theme::apply(ctx);
        let monitor = Rc::new(RefCell::new(Monitor {
            ctx: ctx.clone(),
            texture: None,
            images: Vec::new(),
        }));
        let wake_ctx = ctx.clone();
        let editor = editor(
            Wake::new(move || wake_ctx.request_repaint()),
            Box::new(MonitorSink(Rc::clone(&monitor))),
        );
        Self {
            editor,
            timeline_view: TimelineView::default(),
            monitor,
            bin_thumbs: HashMap::new(),
            gallery_thumbnails: None,
            search: String::new(),
            jump_text: None,
            show_media: true,
            show_inspector: true,
            show_console: false,
            console_input: String::new(),
            pending_discard: None,
            file_dialog: None,
            allow_close: false,
            focus_title_text: false,
            show_shortcuts: false,
            show_settings: false,
            frame_ms_ema: 0.0,
            rss_mb: read_rss_mb(),
            frame_count: 0,
        }
    }

    /// Move finished imports' filmstrip stills into textures, and drop the
    /// textures of media no longer in the bin.
    fn take_thumbnails(&mut self, ctx: &egui::Context) {
        for asset in &mut self.editor.assets {
            if asset.thumbs.is_empty() {
                continue;
            }
            let textures = std::mem::take(&mut asset.thumbs)
                .into_iter()
                .enumerate()
                .map(|(i, (time, frame))| {
                    let image = ColorImage::from_rgba_unmultiplied(
                        [frame.width as usize, frame.height as usize],
                        &frame.rgba,
                    );
                    let handle = ctx.load_texture(
                        format!("thumb-{}-{i}", asset.info.name),
                        image,
                        TextureOptions::LINEAR,
                    );
                    (time, handle)
                })
                .collect();
            self.bin_thumbs.insert(asset.info.path.clone(), textures);
        }
        if self.bin_thumbs.len() > self.editor.assets.len() {
            let assets = &self.editor.assets;
            self.bin_thumbs
                .retain(|path, _| assets.iter().any(|asset| asset.info.path == *path));
        }
    }

    fn thumbs(&self, path: &str) -> &[(f64, TextureHandle)] {
        self.bin_thumbs.get(path).map_or(&[], Vec::as_slice)
    }

    fn handle_action(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Import => {
                let dialog = rfd::FileDialog::new()
                    .add_filter("Media", MEDIA_EXTS)
                    .set_title("Import media");
                self.open_file_dialog(ctx, project::FilePurpose::Import, dialog);
            }
            Action::Export => {
                let dialog = rfd::FileDialog::new()
                    .add_filter("MP4 video", &["mp4"])
                    .set_file_name("export.mp4")
                    .set_title("Export MP4");
                self.open_file_dialog(ctx, project::FilePurpose::Export, dialog);
            }
            Action::NewProject => self.request_discard(ctx, project::Discard::New),
            Action::OpenProject => self.request_discard(ctx, project::Discard::Open),
            Action::SaveProject => self.save_project(ctx, false, None),
            Action::SaveProjectAs => self.save_project(ctx, true, None),
            Action::LocateMedia => self.pick_missing_media(ctx),
            Action::OpenSettings => self.show_settings = !self.show_settings,
            Action::ShowShortcuts => self.show_shortcuts = !self.show_shortcuts,
            // Esc closes the shortcuts window before it deselects.
            Action::Deselect if self.show_shortcuts => self.show_shortcuts = false,
            _ => {
                self.editor.perform(action);
            }
        }
    }

    fn console_panel(&mut self, ctx: &egui::Context) {
        if !self.show_console {
            return;
        }
        let mut submitted: Option<String> = None;
        egui::TopBottomPanel::bottom("console")
            .resizable(true)
            .default_height(150.0)
            .height_range(90.0..=340.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    theme::panel_title(ui, "Console");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::ghost_button(ui, "Clear")
                            .on_hover_text("Clear console output")
                            .clicked()
                        {
                            self.editor.console_output.clear();
                        }
                    });
                });
                let output_height = (ui.available_height() - 38.0).max(24.0);
                egui::ScrollArea::vertical()
                    .max_height(output_height)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.editor.console_output {
                            ui.monospace(line);
                        }
                    });
                ui.horizontal(|ui| {
                    ui.monospace(">");
                    let response = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.console_input)
                                .hint_text("JavaScript. help() lists the editor API.")
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace),
                        )
                        .on_hover_text(
                            "Run JavaScript against the editor. Enter runs. Up and Down recall history.",
                        );
                    if response.has_focus() || response.lost_focus() {
                        if ui.input(|i| i.key_pressed(egui::Key::ArrowUp))
                            && !self.editor.console_history.is_empty()
                        {
                            let idx = match self.editor.console_history_idx {
                                Some(0) => 0,
                                Some(i) => i - 1,
                                None => self.editor.console_history.len() - 1,
                            };
                            self.editor.console_history_idx = Some(idx);
                            self.console_input = self.editor.console_history[idx].clone();
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                            match self.editor.console_history_idx {
                                Some(i) if i + 1 < self.editor.console_history.len() => {
                                    self.editor.console_history_idx = Some(i + 1);
                                    self.console_input =
                                        self.editor.console_history[i + 1].clone();
                                }
                                _ => {
                                    self.editor.console_history_idx = None;
                                    self.console_input.clear();
                                }
                            }
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            submitted = Some(std::mem::take(&mut self.console_input));
                            response.request_focus();
                        }
                    }
                });
            });
        if let Some(code) = submitted {
            self.editor.run_console(code);
        }
    }

    fn menu_bar(&mut self, ctx: &egui::Context) -> Option<Action> {
        let mut action: Option<Action> = None;
        let key = |action| shortcuts::label(ctx, action);
        egui::TopBottomPanel::top("menu_bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    brand_mark(ui);
                    ui.label(RichText::new("Eddie").family(theme::semibold()).size(15.0));
                    ui.add_space(10.0);
                    ui.menu_button("File", |ui| {
                        if menu_item(
                            ui,
                            "New project",
                            &key(Action::NewProject),
                            "Start an empty project with an empty media bin",
                            true,
                        ) {
                            action = Some(Action::NewProject);
                        }
                        if menu_item(
                            ui,
                            "Open project…",
                            &key(Action::OpenProject),
                            "Open a saved timeline and its media",
                            true,
                        ) {
                            action = Some(Action::OpenProject);
                        }
                        if menu_item(
                            ui,
                            "Save project",
                            &key(Action::SaveProject),
                            "Save the timeline and its media list",
                            true,
                        ) {
                            action = Some(Action::SaveProject);
                        }
                        if menu_item(
                            ui,
                            "Save project as…",
                            &key(Action::SaveProjectAs),
                            "Save the project to a new file",
                            true,
                        ) {
                            action = Some(Action::SaveProjectAs);
                        }
                        let missing = self.editor.missing_media.len();
                        if menu_item(
                            ui,
                            &if missing > 0 {
                                format!("Locate missing media ({missing})…")
                            } else {
                                "Locate missing media…".to_owned()
                            },
                            "",
                            "Find a project's moved media files",
                            missing > 0,
                        ) {
                            action = Some(Action::LocateMedia);
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            "Import media…",
                            &key(Action::Import),
                            "Import video or audio files",
                            true,
                        ) {
                            action = Some(Action::Import);
                        }
                        if menu_item(
                            ui,
                            "Export MP4…",
                            &key(Action::Export),
                            "Export the timeline as an MP4",
                            true,
                        ) {
                            action = Some(Action::Export);
                        }
                        ui.separator();
                        if menu_item_destructive(
                            ui,
                            "Clear timeline",
                            "Remove every clip from the timeline; imported media stays",
                        ) {
                            action = Some(Action::ClearTimeline);
                        }
                    });
                    ui.menu_button("Edit", |ui| {
                        if menu_item(
                            ui,
                            "Undo",
                            &key(Action::Undo),
                            "Undo the last timeline edit",
                            true,
                        ) {
                            action = Some(Action::Undo);
                        }
                        if menu_item(
                            ui,
                            "Redo",
                            &key(Action::Redo),
                            "Redo the edit you undid",
                            true,
                        ) {
                            action = Some(Action::Redo);
                        }
                        ui.separator();
                        let has_selection = self.editor.selected.is_some();
                        if menu_item(
                            ui,
                            "Split at playhead",
                            &key(Action::SplitSelected),
                            "Split the selected clip at the playhead",
                            has_selection,
                        ) {
                            action = Some(Action::SplitSelected);
                        }
                        if menu_item(
                            ui,
                            "Delete clip",
                            &key(Action::DeleteSelected),
                            "Delete the selected clip",
                            has_selection,
                        ) {
                            action = Some(Action::DeleteSelected);
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            "Add title",
                            &key(Action::AddTitle),
                            "Add a title clip at the playhead",
                            true,
                        ) {
                            action = Some(Action::AddTitle);
                        }
                    });
                    ui.menu_button("View", |ui| {
                        ui.checkbox(&mut self.show_media, "Media bin")
                            .on_hover_text("Show the media bin and curator card");
                        ui.checkbox(&mut self.show_inspector, "Details")
                            .on_hover_text("Show the selected clip's details in the right dock");
                        ui.checkbox(&mut self.show_console, "JS console")
                            .on_hover_text("Debug console for the editing script API");
                        ui.separator();
                        if menu_item(
                            ui,
                            "Zoom in",
                            &key(Action::ZoomIn),
                            "Zoom the timeline in around the playhead",
                            true,
                        ) {
                            action = Some(Action::ZoomIn);
                        }
                        if menu_item(
                            ui,
                            "Zoom out",
                            &key(Action::ZoomOut),
                            "Zoom the timeline out around the playhead",
                            true,
                        ) {
                            action = Some(Action::ZoomOut);
                        }
                        if menu_item(
                            ui,
                            "Fit timeline",
                            &key(Action::ZoomFit),
                            "Zoom so the whole timeline fits",
                            true,
                        ) {
                            action = Some(Action::ZoomFit);
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            "Go to start",
                            &key(Action::GoStart),
                            "Move the playhead to the start",
                            true,
                        ) {
                            action = Some(Action::GoStart);
                        }
                        if menu_item(
                            ui,
                            "Go to end",
                            &key(Action::GoEnd),
                            "Move the playhead to the end",
                            true,
                        ) {
                            action = Some(Action::GoEnd);
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            "Keyboard shortcuts",
                            &key(Action::ShowShortcuts),
                            "List every keyboard shortcut",
                            true,
                        ) {
                            action = Some(Action::ShowShortcuts);
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::primary_button(ui, "Export")
                            .on_hover_text(shortcuts::tip(
                                ctx,
                                "Export the timeline as an MP4",
                                Action::Export,
                            ))
                            .clicked()
                        {
                            action = Some(Action::Export);
                        }
                        let tip = shortcuts::tip(
                            ctx,
                            "Settings: playback, speech and format",
                            Action::OpenSettings,
                        );
                        if tool_icon_button(ui, ToolIcon::Settings, self.show_settings, &tip)
                            .clicked()
                        {
                            action = Some(Action::OpenSettings);
                        }
                    });
                });
            });
        action
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let just_exported = self
            .editor
            .exported
            .as_ref()
            .is_some_and(|(_, message)| *message == self.editor.status);
        let mut open_dir = None;
        egui::TopBottomPanel::bottom("status_bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(14, 5)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let busy = self.editor.importing()
                        || self.editor.exporting()
                        || self.editor.curating();
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    if busy {
                        egui::Spinner::new()
                            .color(theme::MUTED_FG)
                            .paint_at(ui, rect.expand(1.0));
                    } else if let Some((shown, target)) = self.editor.playback_rate() {
                        let color = rate_health(shown as f64, target);
                        ui.painter().circle_filled(rect.center(), 4.0, color);
                        ui.interact(rect, ui.id().with("playback_rate"), Sense::hover())
                            .on_hover_text(format!(
                                "Playback: {shown} of {target:.0} pictures per second"
                            ));
                    } else {
                        ui.painter()
                            .circle_filled(rect.center(), 3.0, theme::SUBTLE_FG);
                    }
                    // The readout lays out first; the message truncates into
                    // what is left, so a long error never runs under it.
                    let status = |ui: &mut egui::Ui| {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&self.editor.status).size(12.0).color(
                                        if just_exported {
                                            theme::FOREGROUND
                                        } else {
                                            theme::MUTED_FG
                                        },
                                    ),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&self.editor.status);
                        });
                    };
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(p) = self.editor.export_progress {
                            ui.add(
                                egui::ProgressBar::new(p as f32)
                                    .desired_width(180.0)
                                    .show_percentage(),
                            )
                            .on_hover_text("Export progress");
                            ui.label(RichText::new("Exporting").size(12.0).color(theme::MUTED_FG));
                            ui.add_space(12.0);
                            status(ui);
                            return;
                        }
                        if let Some((path, _)) = self
                            .editor
                            .exported
                            .as_ref()
                            .filter(|(_, message)| *message == self.editor.status)
                        {
                            let open = theme::secondary_button(ui, "Open folder").on_hover_text(
                                format!("Show the exported file's folder\n{}", path.display()),
                            );
                            if open.clicked() {
                                open_dir = path.parent().map(Path::to_path_buf);
                            }
                            ui.add_space(12.0);
                        }
                        // Right to left: memory, UI frame time, length, rate.
                        let segments = [
                            (format!("{} MB", self.rss_mb), "Memory in use"),
                            (format!("{:.1} ms", self.frame_ms_ema), "UI frame time"),
                            (
                                format_timecode(
                                    self.editor.timeline.end(),
                                    self.editor.timeline.fps,
                                ),
                                "Timeline length",
                            ),
                            (
                                format!("{:.0} fps", self.editor.timeline.fps),
                                "Project frame rate",
                            ),
                        ];
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for (index, (text, tip)) in segments.into_iter().enumerate() {
                            if index > 0 {
                                ui.add_space(8.0);
                                ui.label(RichText::new("·").size(11.0).color(theme::SUBTLE_FG));
                                ui.add_space(8.0);
                            }
                            ui.label(
                                RichText::new(text)
                                    .monospace()
                                    .size(11.0)
                                    .color(theme::SUBTLE_FG),
                            )
                            .on_hover_text(tip);
                        }
                        ui.add_space(12.0);
                        status(ui);
                    });
                });
            });
        if let Some(dir) = open_dir {
            if let Err(error) = open_folder(&dir) {
                self.editor.status = format!("Could not open the folder: {error}");
            }
        }
    }

    fn timeline_panel(&mut self, ctx: &egui::Context) {
        // Short windows give the monitor room; this only sets the first
        // height, and a user resize is remembered.
        let screen_h = ctx.screen_rect().height();
        let (id, height) = if self.editor.curator.open {
            ("gallery_timeline", 170.0)
        } else {
            ("timeline", (screen_h * 0.27).clamp(170.0, 240.0))
        };
        egui::TopBottomPanel::bottom(id)
            .resizable(true)
            .default_height(height)
            .height_range(170.0..=460.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    theme::panel_title(ui, "Timeline");
                    ui.add_space(10.0);
                    let tip = |text, action| shortcuts::tip(ctx, text, action);
                    let mut action = None;
                    if tool_icon_button(
                        ui,
                        ToolIcon::Magnet,
                        self.editor.viewport.magnet,
                        &tip(
                            "Magnet: snap clip edges while dragging",
                            Action::ToggleMagnet,
                        ),
                    )
                    .clicked()
                    {
                        action = Some(Action::ToggleMagnet);
                    }
                    toolbar_divider(ui);
                    for (icon, text, button_action) in [
                        (
                            ToolIcon::Title,
                            "Add a title clip at the playhead",
                            Action::AddTitle,
                        ),
                        (
                            ToolIcon::Split,
                            "Split the clip under the playhead",
                            Action::SplitSelected,
                        ),
                        (
                            ToolIcon::Delete,
                            "Delete the selected clip",
                            Action::DeleteSelected,
                        ),
                    ] {
                        if tool_icon_button(ui, icon, false, &tip(text, button_action)).clicked() {
                            action = Some(button_action);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if tool_icon_button(
                            ui,
                            ToolIcon::ZoomIn,
                            false,
                            &tip("Zoom in", Action::ZoomIn),
                        )
                        .clicked()
                        {
                            action = Some(Action::ZoomIn);
                        }
                        ui.spacing_mut().slider_width = 110.0;
                        ui.add(
                            egui::Slider::new(&mut self.editor.viewport.px_per_sec, 4.0..=600.0)
                                .logarithmic(true)
                                .show_value(false),
                        )
                        .on_hover_text("Timeline zoom. Ctrl+scroll over the timeline also zooms.");
                        if tool_icon_button(
                            ui,
                            ToolIcon::ZoomOut,
                            false,
                            &tip("Zoom out", Action::ZoomOut),
                        )
                        .clicked()
                        {
                            action = Some(Action::ZoomOut);
                        }
                    });
                    if let Some(action) = action {
                        self.handle_action(ctx, action);
                    }
                });
                ui.add_space(8.0);
                let bin_thumbs = &self.bin_thumbs;
                let visuals: VisualMap<'_> = self
                    .editor
                    .assets
                    .iter()
                    .map(|a| {
                        (
                            a.info.path.as_str(),
                            ClipVisual {
                                duration: a.info.duration,
                                peaks: &a.peaks,
                                thumbs: bin_thumbs.get(&a.info.path).map_or(&[], Vec::as_slice),
                            },
                        )
                    })
                    .collect();
                // The editor owns the viewport; the view draws with it and
                // hands back this frame's scroll, zoom, width and gestures.
                self.timeline_view.port = self.editor.viewport;
                self.timeline_view.show(
                    ui,
                    &mut self.editor.timeline,
                    &mut self.editor.playhead,
                    &mut self.editor.selected,
                    &visuals,
                );
                self.editor.viewport = self.timeline_view.port;
            });
    }

    /// Right dock: the selected clip's Details, beside the full-height
    /// timeline.
    fn right_dock(&mut self, ctx: &egui::Context) {
        if !self.show_inspector {
            return;
        }
        egui::SidePanel::right("right_dock")
            .resizable(true)
            .default_width(340.0)
            .width_range(280.0..=460.0)
            .frame(theme::panel_frame())
            .show(ctx, |ui| {
                theme::panel_title(ui, "Details");
                ui.add_space(12.0);
                egui::ScrollArea::vertical()
                    .id_salt("inspector_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.inspector_content(ui));
            });
    }

    fn inspector_content(&mut self, ui: &mut Ui) {
        let Some(id) = self.editor.selected else {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No clip selected").color(theme::MUTED_FG));
                ui.label(theme::caption("Click a clip on the timeline to edit it."));
            });
            return;
        };

        let clip_snapshot = self.editor.timeline.clip(id).cloned();
        let linked_snapshot = clip_snapshot
            .as_ref()
            .and_then(|clip| clip.linked_clip)
            .and_then(|linked| self.editor.timeline.clip(linked).cloned());
        let source_has_audio = clip_snapshot
            .as_ref()
            .and_then(|clip| clip.source.as_ref())
            .and_then(|source| self.editor.asset(source))
            .map(|asset| asset.info.has_audio)
            .unwrap_or(false);
        let has_transcript = clip_snapshot
            .as_ref()
            .and_then(|clip| clip.source.as_ref())
            .and_then(|source| self.editor.asset(source))
            .map(|asset| !asset.sweep.transcript.is_empty())
            .unwrap_or(false);
        let mut caption_style = self.editor.caption_style;
        let mut do_split = false;
        let mut do_delete = false;
        let mut do_detach = false;
        let mut do_unlink = false;
        let mut do_tighten = false;
        let mut do_captions = false;
        let mut start_changed = false;
        let mut edited_start = clip_snapshot.as_ref().map(|clip| clip.start).unwrap_or(0.0);
        if let Some(clip) = self.editor.timeline.clip_mut(id) {
            let (kind_label, kind_color) = match clip.kind {
                ClipKind::Video => ("Video", theme::CLIP_VIDEO),
                ClipKind::Audio => ("Audio", theme::CLIP_AUDIO),
                ClipKind::Title => ("Title", theme::CLIP_TITLE),
            };
            // One header row: kind dot, then the editable name. The kind
            // and source file live in the tooltips.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                ui.painter()
                    .circle_filled(dot.center(), 4.0, kind_color.gamma_multiply(1.6));
                let hover = match &clip.source {
                    Some(path) => format!("{kind_label} clip name\nSource: {path}"),
                    None => format!("{kind_label} clip name"),
                };
                if clip.kind == ClipKind::Title {
                    // A title's name is its on-screen text: wrap it so long
                    // captions are readable. Export renders one line, so
                    // Enter and pasted line breaks become spaces.
                    let text = ui
                        .add(
                            egui::TextEdit::multiline(&mut clip.name)
                                .desired_width(f32::INFINITY)
                                .desired_rows(2)
                                .return_key(None)
                                .margin(egui::Margin::symmetric(8, 6)),
                        )
                        .on_hover_text("On-screen text");
                    if text.changed() && clip.name.contains(['\n', '\r']) {
                        clip.name = clip.name.replace(['\r', '\n'], " ");
                    }
                    if std::mem::take(&mut self.focus_title_text) {
                        text.request_focus();
                    }
                } else {
                    ui.add(
                        egui::TextEdit::singleline(&mut clip.name)
                            .desired_width(f32::INFINITY)
                            .margin(egui::Margin::symmetric(8, 6)),
                    )
                    .on_hover_text(hover);
                }
            });
            ui.add_space(10.0);
            let is_audio = clip.kind == ClipKind::Audio;
            let has_source = clip.source.is_some();
            inspector_section(ui, "Timing", |ui| {
                start_changed =
                    prop_row(ui, "Start", |ui| seconds_field(ui, &mut edited_start, 0.0))
                        .on_hover_text("Where the clip starts on the timeline")
                        .changed();
                prop_row(ui, "Duration", |ui| {
                    seconds_field(ui, &mut clip.duration, 0.1)
                })
                .on_hover_text("How long the clip plays");
                if has_source {
                    prop_row(ui, "Source in", |ui| {
                        seconds_field(ui, &mut clip.source_in, 0.0)
                    })
                    .on_hover_text("Where playback starts inside the source file");
                }
            });
            ui.add_space(8.0);
            let look_title = match clip.kind {
                ClipKind::Video => "Picture & sound",
                ClipKind::Audio => "Sound",
                ClipKind::Title => "Look",
            };
            let kind = clip.kind;
            let video_audio = kind == ClipKind::Video && source_has_audio;
            let linked = clip.linked_clip.is_some();
            let can_detach = video_audio && clip.embedded_audio && !linked;
            let can_tighten = source_has_audio && kind != ClipKind::Title;
            // Sound controls share the card with picture controls, so a video
            // clip's whole inspector fits without scrolling.
            inspector_section(ui, look_title, |ui| {
                if !is_audio {
                    prop_row(ui, "Opacity", |ui| {
                        percent_slider(ui, &mut clip.opacity, 1.0)
                    })
                    .on_hover_text("Fades the picture against black");
                }
                if has_source {
                    prop_row(ui, "Volume", |ui| percent_slider(ui, &mut clip.volume, 1.5))
                        .on_hover_text("Clip gain; 100% is unity");
                }
                // Audio clips only, so a video clip's card keeps its height;
                // scripts can fade any clip's sound with setFade.
                if is_audio && has_source {
                    prop_row(ui, "Fades", |ui| {
                        let half = ((ui.available_width() - 6.0) / 2.0).max(40.0);
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let fade_in = ui
                            .allocate_ui(Vec2::new(half, 24.0), |ui| {
                                seconds_field(ui, &mut clip.fade_in, 0.0)
                            })
                            .inner
                            .on_hover_text("Fade in: seconds to rise from silence");
                        let fade_out = ui
                            .allocate_ui(Vec2::new(half, 24.0), |ui| {
                                seconds_field(ui, &mut clip.fade_out, 0.0)
                            })
                            .inner
                            .on_hover_text("Fade out: seconds to fall to silence");
                        fade_in.union(fade_out)
                    });
                    ui.checkbox(&mut clip.smooth_fades, "Smooth fades")
                        .on_hover_text(
                            "Fade on an equal-power curve, which stays fuller through a crossfade",
                        );
                    ui.checkbox(&mut clip.duck, "Duck under speech")
                        .on_hover_text("Dip this sound while another clip is talking");
                    ui.checkbox(&mut clip.clean_voice, "Clean voice")
                        .on_hover_text(
                        "Cut low rumble and even out loud and quiet speech, in preview and export",
                    );
                }
                if kind == ClipKind::Title {
                    prop_row(ui, "Style", |ui| {
                        egui::ComboBox::from_id_salt("title_style")
                            .width(ui.available_width())
                            .selected_text(clip.title_style.label())
                            .show_ui(ui, |ui| {
                                for style in TitleStyle::ALL {
                                    ui.selectable_value(
                                        &mut clip.title_style,
                                        style,
                                        style.label(),
                                    );
                                }
                            })
                            .response
                    })
                    .on_hover_text("Choose how this title or caption is drawn");
                }
                // Detached sound plays from the linked clip; the line below
                // says so, so the checkbox only shows while it applies.
                if video_audio && !linked {
                    ui.checkbox(&mut clip.embedded_audio, "Embedded audio")
                        .on_hover_text("Play this video clip's own sound");
                }
                if let Some(partner) = &linked_snapshot {
                    ui.add(
                        egui::Label::new(theme::caption(format!("Linked to {}", partner.name)))
                            .truncate(),
                    )
                    .on_hover_text("Picture and sound move, trim and split together");
                }
                let mut buttons: Vec<(&str, &str, &mut bool)> = Vec::new();
                if can_detach {
                    buttons.push((
                        "Detach audio",
                        "Move this clip's sound to a linked audio clip you can edit separately",
                        &mut do_detach,
                    ));
                }
                if linked_snapshot.is_some() {
                    buttons.push((
                        "Unlink",
                        "Let picture and sound move independently for J/L cuts or replacement audio",
                        &mut do_unlink,
                    ));
                }
                if can_tighten {
                    buttons.push((
                        "Remove silence",
                        "Cut waveform pauses from this speech clip and ripple later clips left",
                        &mut do_tighten,
                    ));
                }
                button_row(ui, buttons, false);
                // Captions come from this clip's sound, so they share its card:
                // one row with the style and the action.
                if kind != ClipKind::Title && has_transcript {
                    prop_row(ui, "Captions", |ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let add = theme::secondary_button(ui, "Add").on_hover_text(
                                "Generate caption clips from this clip's transcript in the chosen style",
                            );
                            if add.clicked() {
                                do_captions = true;
                            }
                            egui::ComboBox::from_id_salt("caption_style")
                                .width(ui.available_width() - 20.0)
                                .truncate()
                                .selected_text(caption_style.label())
                                .show_ui(ui, |ui| {
                                    for style in TitleStyle::ALL {
                                        ui.selectable_value(
                                            &mut caption_style,
                                            style,
                                            style.label(),
                                        );
                                    }
                                })
                                .response
                                .on_hover_text("Choose the style for generated captions");
                            add
                        })
                        .inner
                    });
                }
            });
            ui.add_space(12.0);
            let split_tip = shortcuts::tip(
                ui.ctx(),
                "Split this clip at the playhead",
                Action::SplitSelected,
            );
            let delete_tip = shortcuts::tip(ui.ctx(), "Delete this clip", Action::DeleteSelected);
            button_row(
                ui,
                vec![
                    ("Split at playhead", &split_tip, &mut do_split),
                    ("Delete", &delete_tip, &mut do_delete),
                ],
                true,
            );
        }

        self.editor.caption_style = caption_style;

        // Merely viewing Details must not quantize a detected scene boundary.
        if start_changed {
            self.editor.timeline.move_clip(id, edited_start);
        }
        let source_duration = self
            .editor
            .timeline
            .clip(id)
            .and_then(|c| c.source.as_ref())
            .and_then(|s| self.editor.asset(s))
            .map(|a| a.info.duration);
        if let (Some(max_dur), Some(clip)) = (source_duration, self.editor.timeline.clip_mut(id)) {
            clip.source_in = clip.source_in.clamp(0.0, max_dur);
            clip.duration = clip.duration.min((max_dur - clip.source_in).max(0.1));
        }

        // Fine-grained duration/source edits keep a linked pair in
        // sync by the same deltas start edits get from `move_clip`.
        if let (Some(original), Some(current), Some(linked_original)) = (
            clip_snapshot.as_ref(),
            self.editor.timeline.clip(id).cloned(),
            linked_snapshot.as_ref(),
        ) {
            let duration_delta = current.duration - original.duration;
            let source_delta = current.source_in - original.source_in;
            if duration_delta != 0.0 || source_delta != 0.0 {
                let min_duration = 1.0 / self.editor.timeline.fps;
                if let Some(linked) = self.editor.timeline.clip_mut(linked_original.id) {
                    linked.duration = (linked_original.duration + duration_delta).max(min_duration);
                    linked.source_in = (linked_original.source_in + source_delta).max(0.0);
                }
            }
        }

        if do_detach {
            if source_has_audio {
                match self.editor.timeline.detach_audio(id) {
                    Some(audio) => {
                        self.editor.selected = Some(audio);
                        self.editor.status = "Audio detached.".to_owned();
                    }
                    None => {
                        self.editor.status = "This clip's audio is already detached.".to_owned();
                    }
                }
            } else {
                self.editor.status = "This clip has no detachable audio.".to_owned();
            }
        }
        if do_unlink {
            if self.editor.timeline.unlink_clip(id) {
                self.editor.status = "Picture and audio unlinked.".to_owned();
            } else {
                self.editor.status = "This clip is not linked.".to_owned();
            }
        }
        if do_captions {
            match self.editor.add_captions_for_clip(id, caption_style) {
                Ok(0) => {
                    self.editor.status = "Captions already added or none overlap.".to_owned();
                }
                Ok(count) => {
                    self.editor.status = format!(
                        "Added {count} caption{}.",
                        if count == 1 { "" } else { "s" }
                    );
                }
                Err(error) => self.editor.status = error,
            }
        }
        if do_tighten {
            match self.editor.tighten_clip_silence(id, 0.03, 0.35, 0.08) {
                Ok(1) => self.editor.status = "No removable silence found.".to_owned(),
                Ok(pieces) => {
                    self.editor.status = format!("Removed silence into {pieces} phrases.");
                }
                Err(error) => self.editor.status = error,
            }
        }
        if do_split {
            match self.editor.timeline.split_at(id, self.editor.playhead) {
                Some(new_id) => {
                    self.editor.selected = Some(new_id);
                    self.editor.status = "Clip split at playhead.".to_owned();
                }
                None => {
                    self.editor.status = "Playhead is not inside the selected clip.".to_owned();
                }
            }
        }
        if do_delete {
            self.editor.delete_clip(id);
            self.editor.selected = None;
            self.editor.status = "Clip deleted.".to_owned();
        }
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(16, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let picked = theme::segmented(
                        ui,
                        self.editor.curator.open,
                        &[
                            (
                                false,
                                "Program",
                                "Show the timeline preview and playback controls",
                            ),
                            (true, "Gallery", "Pause playback and browse curated clips"),
                        ],
                    );
                    match picked {
                        Some(false) => self.editor.curator.open = false,
                        Some(true) => self.editor.open_curator_gallery(),
                        None => {}
                    }
                });
                ui.add_space(10.0);
                if self.editor.curator.open {
                    self.gallery_content(ui);
                    return;
                }

                let avail = ui.available_size();
                let (rect, _) = ui.allocate_exact_size(
                    Vec2::new(avail.x, (avail.y - 72.0).clamp(0.0, avail.y)),
                    Sense::hover(),
                );
                let audio_peaks = self
                    .editor
                    .timeline
                    .clips
                    .iter()
                    .find(|c| c.kind == ClipKind::Audio && c.contains(self.editor.playhead))
                    .and_then(|c| c.source.as_ref())
                    .and_then(|s| self.editor.asset(s))
                    .map(|a| (a.peaks.as_slice(), a.info.duration));
                let picture = self
                    .monitor
                    .borrow()
                    .texture
                    .clone()
                    .filter(|_| self.editor.preview_is_current());
                let shown_title = preview::paint(
                    ui,
                    rect,
                    &self.editor.timeline,
                    self.editor.playhead,
                    picture.as_ref(),
                    audio_peaks,
                    openreel_media::export_canvas(
                        self.editor.timeline.orientation,
                        self.editor
                            .timeline
                            .clips
                            .iter()
                            .filter(|c| c.kind == ClipKind::Video)
                            .filter_map(|c| self.editor.asset(c.source.as_deref()?))
                            .map(|a| (a.info.width, a.info.height)),
                    ),
                    !self.editor.assets.is_empty(),
                );
                if let Some((title_id, title_rect)) = shown_title {
                    let title = ui
                        .interact(title_rect, ui.id().with("monitor_title"), Sense::click())
                        .on_hover_text("Double-click to edit this text");
                    if title.double_clicked() {
                        self.editor.selected = Some(title_id);
                        self.show_inspector = true;
                        self.focus_title_text = true;
                    }
                }

                ui.add_space(4.0);
                let row = ui.available_rect_before_wrap();
                let bar_w = 520.0_f32.min(row.width());
                // Below this the length readout would overlap the controls.
                let show_length = bar_w >= 470.0;
                let scrub = egui::Rect::from_center_size(
                    egui::pos2(row.center().x, row.top() + 7.0),
                    Vec2::new(bar_w - 16.0, 14.0),
                );
                self.transport_scrubber(ui, scrub);
                let bar = egui::Rect::from_center_size(
                    egui::pos2(row.center().x, row.top() + 14.0 + 22.0),
                    Vec2::new(bar_w, 44.0),
                );
                ui.painter().rect(
                    bar,
                    CornerRadius::same(theme::R_LG),
                    theme::SURFACE,
                    Stroke::new(1.0_f32, theme::BORDER),
                    egui::StrokeKind::Inside,
                );
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(bar.shrink2(Vec2::new(14.0, 6.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        self.playhead_readout(ui);
                        let controls_w = 38.0 * 5.0 + 2.0 * 4.0;
                        let readout_w = ui.min_rect().width();
                        let left_pad = if show_length {
                            (ui.available_width() + readout_w - controls_w) / 2.0 - readout_w
                        } else {
                            ui.available_width() - controls_w
                        };
                        ui.add_space(left_pad.max(8.0));
                        let ctx = ui.ctx().clone();
                        let play = if self.editor.playing {
                            (TransportIcon::Pause, "Pause")
                        } else {
                            (TransportIcon::Play, "Play")
                        };
                        let mut pressed = None;
                        for (icon, text, button_action) in [
                            (TransportIcon::ToStart, "Go to start", Action::GoStart),
                            (TransportIcon::StepBack, "Back one frame", Action::StepBack),
                            (play.0, play.1, Action::PlayPause),
                            (
                                TransportIcon::StepForward,
                                "Forward one frame",
                                Action::StepForward,
                            ),
                            (TransportIcon::ToEnd, "Go to end", Action::GoEnd),
                        ] {
                            if transport_button(
                                ui,
                                icon,
                                &shortcuts::tip(&ctx, text, button_action),
                            )
                            .clicked()
                            {
                                pressed = Some(button_action);
                            }
                        }
                        if let Some(action) = pressed {
                            self.handle_action(&ctx, action);
                        }
                        if !show_length {
                            return;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(timecode_job(
                                &format_timecode(
                                    self.editor.timeline.end(),
                                    self.editor.timeline.fps,
                                ),
                                theme::MUTED_FG,
                            ))
                            .on_hover_text("Timeline length");
                        });
                    },
                );
            });
    }
}

impl EditorApp {
    /// Thin whole-edit position track above the transport. Click or drag
    /// to move the playhead; while held it previews like a ruler scrub.
    fn transport_scrubber(&mut self, ui: &mut Ui, rect: egui::Rect) {
        let end = self.editor.timeline.end();
        let mut response = ui.interact(
            rect,
            ui.id().with("transport_scrub"),
            Sense::click_and_drag(),
        );
        if !response.is_pointer_button_down_on() {
            // No tooltip over the readout while dragging.
            response = response.on_hover_text("Click or drag to move the playhead");
        }
        if end > 0.0 {
            if let Some(pos) = response.interact_pointer_pos() {
                if response.is_pointer_button_down_on() || response.clicked() {
                    let fraction = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                    self.editor.playhead = self.editor.timeline.snap(fraction as f64 * end);
                }
            }
        }
        let held = response.is_pointer_button_down_on();
        if self.editor.transport_scrubbing && !held {
            // Settle at full preview quality where the pointer let go.
            self.editor.drop_preview();
        }
        self.editor.transport_scrubbing = held;
        let active = held || response.hovered();
        let track = egui::Rect::from_center_size(
            rect.center(),
            Vec2::new(rect.width(), if active { 5.0 } else { 3.0 }),
        );
        let painter = ui.painter();
        painter.rect_filled(track, CornerRadius::same(3), theme::SECONDARY);
        let fraction = if end > 0.0 {
            (self.editor.playhead / end).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let x = track.left() + track.width() * fraction;
        painter.rect_filled(
            egui::Rect::from_min_max(track.min, egui::pos2(x, track.max.y)),
            CornerRadius::same(3),
            if active {
                theme::FOREGROUND
            } else {
                theme::MUTED_FG
            },
        );
        if active {
            painter.circle_filled(egui::pos2(x, track.center().y), 6.0, theme::FOREGROUND);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    /// Post-panel work for one frame: playback sync, then the
    /// deferred UI action. Split out of `eframe::App::update` so
    /// tests can drive the real seam. Gap closing lives at the edit
    /// sites (drag release, delete, script CloseGaps) where the
    /// intent is known; a per-frame close here cannot tell a gap a
    /// left trim opened on purpose from one a delete left behind.
    fn finish_frame(&mut self, ctx: &egui::Context, action: Option<Action>) {
        // Act first, so a play, pause or seek reaches playback this frame.
        if let Some(action) = action {
            self.handle_action(ctx, action);
        }
        self.editor.sync_playback();
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let update_start = Instant::now();
        // Phase times for the slow-frame log below.
        let mut phases: Vec<(&str, f32)> = Vec::new();
        let mut last = update_start;
        let mut mark = |name| {
            let now = Instant::now();
            phases.push((name, (now - last).as_secs_f32() * 1000.0));
            last = now;
        };
        let events = self.editor.begin_frame(ctx.input(|i| i.time));
        if events.exported {
            // Flash the taskbar entry so a finished export is noticed.
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }
        mark("editor");
        self.take_thumbnails(ctx);
        self.poll_file_dialog(ctx);
        mark("polls");

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.editor.spawn_import(dropped);
        }

        self.guard_close(ctx);
        mark("drops");
        let mut action = self.menu_bar(ctx);
        self.status_bar(ctx);
        self.console_panel(ctx);
        self.timeline_panel(ctx);
        self.settings_window(ctx);
        if let Some(a) = self.media_bin(ctx) {
            action = Some(a);
        }
        self.right_dock(ctx);
        self.central(ctx);
        self.unsaved_dialog(ctx);
        shortcuts::window(ctx, &mut self.show_shortcuts);
        mark("panels");
        if action.is_none() && self.pending_discard.is_none() {
            let typing = ctx.memory(|m| m.focused().is_some());
            action = shortcuts::take_action(ctx, typing);
        }
        self.finish_frame(ctx, action);
        mark("action+sync");
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        let typing = ctx.memory(|m| m.focused().is_some());
        self.editor.end_frame(pointer_down, typing);
        if let Some(after) = self.editor.repaint_after() {
            ctx.request_repaint_after(after);
        }
        mark("history");
        let ms = update_start.elapsed().as_secs_f32() * 1000.0;
        // A frame this long risks the window manager's "not responding".
        if ms > 250.0 {
            let detail: Vec<String> = phases
                .iter()
                .filter(|(_, phase)| *phase >= 1.0)
                .map(|(name, phase)| format!("{name} {phase:.0}"))
                .collect();
            eprintln!("slow UI frame: {ms:.0} ms ({})", detail.join(", "));
        }
        self.frame_ms_ema = if self.frame_ms_ema == 0.0 {
            ms
        } else {
            self.frame_ms_ema * 0.9 + ms * 0.1
        };
        self.frame_count += 1;
        if self.frame_count % 60 == 0 {
            self.rss_mb = read_rss_mb();
        }
    }
}

/// Green at 80% of the target rate, amber at 50%, red below.
fn rate_health(shown: f64, target: f64) -> egui::Color32 {
    if shown >= target * 0.8 {
        theme::SUCCESS
    } else if shown >= target * 0.5 {
        theme::WARNING
    } else {
        theme::DESTRUCTIVE
    }
}

/// Resident set size from `/proc` (Linux). Returns 0 elsewhere.
/// Open `dir` in the desktop's file manager without blocking the UI.
fn open_folder(dir: &Path) -> std::io::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(opener).arg(dir).spawn()?;
    // Reap the launcher so it does not linger as a zombie.
    thread::spawn(move || child.wait());
    Ok(())
}

fn read_rss_mb() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb / 1024)
        .unwrap_or(0)
}

enum ToolIcon {
    Magnet,
    Title,
    Split,
    Delete,
    ZoomIn,
    ZoomOut,
    Settings,
}

/// App mark (timeline-clip E with a playhead), drawn as shapes.
fn brand_mark(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
    theme::paint_mark(ui.painter(), rect);
}

/// Titled card grouping related inspector controls.
fn inspector_section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 8.0;
        theme::panel_title(ui, title);
        add(ui);
    });
}

/// Muted label in a fixed column, control filling the rest of the row.
fn prop_row(
    ui: &mut Ui,
    label: &str,
    add: impl FnOnce(&mut Ui) -> egui::Response,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(78.0, 26.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(78.0);
                ui.label(RichText::new(label).color(theme::MUTED_FG));
            },
        );
        add(ui)
    })
    .inner
}

/// Seconds at a fixed two decimals; the exact value is in its tooltip.
fn seconds_field(ui: &mut Ui, value: &mut f64, min: f64) -> egui::Response {
    let exact = format!("{:.3} s", *value);
    ui.add_sized(
        [ui.available_width(), 24.0],
        egui::DragValue::new(value)
            .speed(0.05)
            .range(min..=86400.0)
            .clamp_existing_to_range(false)
            .fixed_decimals(2)
            .suffix(" s"),
    )
    .on_hover_text(exact)
}

/// 0..=max gain or opacity as a slider plus a fixed-width percentage
/// field, so the row never asks the dock for more width than it has.
fn percent_slider(ui: &mut Ui, value: &mut f32, max: f32) -> egui::Response {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let field = ui.add_sized(
            [56.0, 24.0],
            egui::DragValue::new(value)
                .speed(0.01)
                .range(0.0..=max)
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                .custom_parser(|text| {
                    text.trim()
                        .trim_end_matches('%')
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .map(|v| v / 100.0)
                }),
        );
        ui.spacing_mut().slider_width = (ui.available_width() - 4.0).max(40.0);
        let slider = ui.add(egui::Slider::new(value, 0.0..=max).show_value(false));
        slider.union(field)
    })
    .inner
}

/// Equal-width buttons sharing one row; with `destructive_last` the final
/// button is the destructive variant.
fn button_row(ui: &mut Ui, buttons: Vec<(&str, &str, &mut bool)>, destructive_last: bool) {
    if buttons.is_empty() {
        return;
    }
    let count = buttons.len();
    let gap = 6.0;
    let width = ((ui.available_width() - gap * (count - 1) as f32) / count as f32).floor();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (index, (label, tooltip, flag)) in buttons.into_iter().enumerate() {
            ui.allocate_ui(Vec2::new(width, 30.0), |ui| {
                let response = if destructive_last && index + 1 == count {
                    theme::destructive_button_wide(ui, label)
                } else {
                    theme::secondary_button_wide(ui, label)
                };
                if response.on_hover_text(tooltip).clicked() {
                    *flag = true;
                }
            });
        }
    });
}

/// Monospace timecode with its leading zero groups dimmed, so the moving digits
/// stand out, as in most editors' transport readouts.
impl EditorApp {
    /// The playhead timecode. Clicking it opens a jump box that takes any
    /// time spec (`1:05`, `3.2s`, `120f`, `50%`, `clip:7@end`).
    fn playhead_readout(&mut self, ui: &mut egui::Ui) {
        let Some(text) = self.jump_text.as_mut() else {
            let readout = ui
                .add(
                    egui::Label::new(timecode_job(
                        &format_timecode(self.editor.playhead, self.editor.timeline.fps),
                        theme::FOREGROUND,
                    ))
                    .sense(egui::Sense::click()),
                )
                .on_hover_cursor(egui::CursorIcon::Text)
                .on_hover_text("Playhead position. Click to jump to a time: 1:05, 3.2s, 120f, 50% or clip:7@end");
            if readout.clicked() {
                self.jump_text = Some(format_timecode(
                    self.editor.playhead,
                    self.editor.timeline.fps,
                ));
            }
            return;
        };
        let edit = egui::TextEdit::singleline(text)
            .id(egui::Id::new("jump-box"))
            .font(egui::FontId::monospace(13.0))
            .desired_width(96.0)
            .show(ui);
        let response = edit.response;
        if !response.has_focus() && !response.lost_focus() {
            // Opened this frame: focus with the old time selected, so typing replaces it.
            response.request_focus();
            let mut state = edit.state;
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(text.chars().count()),
                )));
            state.store(ui.ctx(), response.id);
        }
        if response.lost_focus() {
            let entered = ui.input(|input| input.key_pressed(egui::Key::Enter));
            let spec = self.jump_text.take().unwrap_or_default();
            if entered {
                match self.editor.timeline.time_index().time(&spec) {
                    Ok(time) => self
                        .editor
                        .seek(time.clamp(0.0, self.editor.timeline.end())),
                    Err(error) => self.editor.status = format!("Can't jump: {error}."),
                }
            }
        }
    }
}

fn timecode_job(timecode: &str, bright: egui::Color32) -> egui::text::LayoutJob {
    // Dim whole leading `00:` groups; seconds and frames always stay bright.
    let mut dim = 0;
    while timecode[dim..].starts_with("00:") && dim + 3 + 5 <= timecode.len() {
        dim += 3;
    }
    let mut job = egui::text::LayoutJob::default();
    let font = egui::FontId::monospace(13.0);
    for (text, color) in [
        (&timecode[..dim], theme::SUBTLE_FG),
        (&timecode[dim..], bright),
    ] {
        job.append(
            text,
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            },
        );
    }
    job
}

/// Menu entries share one width so shortcut hints line up.
const MENU_WIDTH: f32 = 210.0;

/// Menu entry with a right-aligned shortcut hint; closes the menu on click.
fn menu_item(ui: &mut Ui, label: &str, shortcut: &str, tooltip: &str, enabled: bool) -> bool {
    let response = ui
        .add_enabled(
            enabled,
            egui::Button::new(label)
                .shortcut_text(RichText::new(shortcut).color(theme::SUBTLE_FG))
                .min_size(Vec2::new(MENU_WIDTH, 26.0)),
        )
        .on_hover_text(tooltip)
        .on_disabled_hover_text(format!("{tooltip}. Select a clip first"));
    if response.clicked() {
        ui.close();
    }
    response.clicked()
}

/// Menu entry for an action that discards work, in the destructive colour.
pub(super) fn menu_item_destructive(ui: &mut Ui, label: &str, tooltip: &str) -> bool {
    let response = ui
        .add(
            egui::Button::new(RichText::new(label).color(theme::DESTRUCTIVE))
                .min_size(Vec2::new(MENU_WIDTH, 26.0)),
        )
        .on_hover_text(tooltip);
    if response.clicked() {
        ui.close();
    }
    response.clicked()
}

/// Thin vertical rule separating toolbar groups.
fn toolbar_divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(13.0, 18.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(1.0_f32, theme::BORDER_STRONG),
    );
}

/// Timeline tool glyphs drawn as shapes, like the transport icons.
/// Borderless at rest; hover and the active toggle carry the fill.
fn tool_icon_button(ui: &mut Ui, icon: ToolIcon, active: bool, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(30.0, 30.0), Sense::click());
    let painter = ui.painter();
    if active {
        painter.rect(
            rect,
            CornerRadius::same(theme::R_MD),
            theme::SECONDARY,
            Stroke::NONE,
            egui::StrokeKind::Outside,
        );
    } else if response.is_pointer_button_down_on() {
        painter.rect(
            rect,
            CornerRadius::same(theme::R_MD),
            egui::Color32::from_rgb(0x40, 0x40, 0x49),
            Stroke::NONE,
            egui::StrokeKind::Outside,
        );
    } else if response.hovered() {
        painter.rect(
            rect,
            CornerRadius::same(theme::R_MD),
            theme::ACCENT,
            Stroke::NONE,
            egui::StrokeKind::Outside,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let c = rect.center();
    let fg = if active || response.hovered() {
        theme::FOREGROUND
    } else {
        theme::MUTED_FG
    };
    let line = Stroke::new(1.6_f32, fg);
    match icon {
        ToolIcon::ZoomIn | ToolIcon::ZoomOut => {
            painter.circle_stroke(c + Vec2::new(-1.5, -1.5), 5.5, line);
            painter.line_segment([c + Vec2::new(2.5, 2.5), c + Vec2::new(7.0, 7.0)], line);
            painter.line_segment(
                [c + Vec2::new(-4.0, -1.5), c + Vec2::new(1.0, -1.5)],
                Stroke::new(1.4_f32, fg),
            );
            if matches!(icon, ToolIcon::ZoomIn) {
                painter.line_segment(
                    [c + Vec2::new(-1.5, -4.0), c + Vec2::new(-1.5, 1.0)],
                    Stroke::new(1.4_f32, fg),
                );
            }
        }
        ToolIcon::Magnet => {
            // Horseshoe with separated pole tips, so it reads as a magnet
            // rather than a letter U.
            let mut body = vec![c + Vec2::new(-5.0, -3.5)];
            body.extend((0..=12).map(|i| {
                let angle = std::f32::consts::PI * (i as f32 / 12.0);
                c + Vec2::new(-5.0 * angle.cos(), 1.0 + 5.0 * angle.sin())
            }));
            body.push(c + Vec2::new(5.0, -3.5));
            painter.add(Shape::line(body, Stroke::new(3.0_f32, fg)));
            for x in [-5.0_f32, 5.0] {
                let tip = egui::Rect::from_center_size(c + Vec2::new(x, -7.0), Vec2::new(3.0, 3.0));
                painter.rect_filled(tip, CornerRadius::same(1), fg);
            }
        }
        ToolIcon::Settings => {
            // Gear: ring plus eight teeth.
            painter.circle_stroke(c, 4.6, line);
            for i in 0..8 {
                let angle = std::f32::consts::TAU * i as f32 / 8.0;
                let dir = Vec2::new(angle.cos(), angle.sin());
                painter.line_segment([c + dir * 5.4, c + dir * 7.6], Stroke::new(2.2_f32, fg));
            }
            painter.circle_filled(c, 1.6, fg);
        }
        ToolIcon::Title => {
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                "T",
                egui::FontId::proportional(16.0),
                fg,
            );
        }
        ToolIcon::Split => {
            // One bar cut in two, slightly separated.
            let left = egui::Rect::from_min_size(c + Vec2::new(-9.0, -5.0), Vec2::new(7.0, 10.0));
            let right = egui::Rect::from_min_size(c + Vec2::new(2.0, -5.0), Vec2::new(7.0, 10.0));
            painter.rect(
                left,
                CornerRadius::same(2),
                egui::Color32::TRANSPARENT,
                line,
                egui::StrokeKind::Inside,
            );
            painter.rect(
                right,
                CornerRadius::same(2),
                egui::Color32::TRANSPARENT,
                line,
                egui::StrokeKind::Inside,
            );
            painter.line_segment(
                [c + Vec2::new(0.0, -8.0), c + Vec2::new(0.0, 8.0)],
                Stroke::new(1.4_f32, theme::PLAYHEAD),
            );
        }
        ToolIcon::Delete => {
            // Trash can: lid, body, ribs.
            painter.line_segment([c + Vec2::new(-7.0, -5.0), c + Vec2::new(7.0, -5.0)], line);
            painter.line_segment([c + Vec2::new(-3.0, -8.0), c + Vec2::new(3.0, -8.0)], line);
            let body = egui::Rect::from_min_size(c + Vec2::new(-5.5, -5.0), Vec2::new(11.0, 13.0));
            painter.rect(
                body,
                CornerRadius::same(2),
                egui::Color32::TRANSPARENT,
                line,
                egui::StrokeKind::Inside,
            );
            for dx in [-2.5_f32, 0.0, 2.5] {
                painter.line_segment(
                    [c + Vec2::new(dx, -1.5), c + Vec2::new(dx, 5.0)],
                    Stroke::new(1.2_f32, fg),
                );
            }
        }
    }
    response.on_hover_text(tooltip)
}

enum TransportIcon {
    ToStart,
    StepBack,
    Play,
    Pause,
    StepForward,
    ToEnd,
}

/// Transport glyphs drawn as shapes — no icon-font dependency, always sharp.
fn transport_button(ui: &mut Ui, icon: TransportIcon, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(38.0, 32.0), Sense::click());
    let painter = ui.painter();
    if matches!(icon, TransportIcon::Play | TransportIcon::Pause) {
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    } else if response.is_pointer_button_down_on() {
        painter.rect(
            rect,
            CornerRadius::same(theme::R_MD),
            egui::Color32::from_rgb(0x40, 0x40, 0x49),
            Stroke::NONE,
            egui::StrokeKind::Outside,
        );
    } else if response.hovered() {
        painter.rect(
            rect,
            CornerRadius::same(theme::R_MD),
            theme::ACCENT,
            Stroke::NONE,
            egui::StrokeKind::Outside,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let c = rect.center();
    let primary = matches!(icon, TransportIcon::Play | TransportIcon::Pause);
    let fg = if primary {
        // The play control is the one filled, high-contrast button.
        let fill = if response.is_pointer_button_down_on() {
            egui::Color32::from_rgb(0xD4, 0xD4, 0xD8)
        } else if response.hovered() {
            egui::Color32::from_rgb(0xE4, 0xE4, 0xE7)
        } else {
            theme::PRIMARY
        };
        painter.circle_filled(c, 15.0, fill);
        theme::PRIMARY_FG
    } else {
        theme::FOREGROUND
    };
    let c = if matches!(icon, TransportIcon::Play) {
        c + Vec2::new(1.0, 0.0)
    } else {
        c
    };
    let tri = |flip: f32| {
        vec![
            c + Vec2::new(-5.0 * flip, -7.0),
            c + Vec2::new(7.0 * flip, 0.0),
            c + Vec2::new(-5.0 * flip, 7.0),
        ]
    };
    match icon {
        TransportIcon::StepBack | TransportIcon::StepForward => {
            // One-frame step: a small chevron beside a frame bar.
            let flip = if matches!(icon, TransportIcon::StepForward) {
                1.0
            } else {
                -1.0
            };
            let stroke = Stroke::new(1.8_f32, fg);
            painter.line_segment(
                [
                    c + Vec2::new(-3.0 * flip, -5.0),
                    c + Vec2::new(2.0 * flip, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    c + Vec2::new(2.0 * flip, 0.0),
                    c + Vec2::new(-3.0 * flip, 5.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    c + Vec2::new(5.5 * flip, -5.5),
                    c + Vec2::new(5.5 * flip, 5.5),
                ],
                stroke,
            );
        }
        TransportIcon::Play => {
            painter.add(Shape::convex_polygon(tri(1.0), fg, Stroke::NONE));
        }
        TransportIcon::Pause => {
            for dx in [-5.0, 2.0] {
                painter.rect_filled(
                    egui::Rect::from_min_size(c + Vec2::new(dx, -7.0), Vec2::new(3.5, 14.0)),
                    CornerRadius::same(1),
                    fg,
                );
            }
        }
        TransportIcon::ToStart => {
            painter.rect_filled(
                egui::Rect::from_min_size(c + Vec2::new(-8.0, -7.0), Vec2::new(2.5, 14.0)),
                CornerRadius::ZERO,
                fg,
            );
            painter.add(Shape::convex_polygon(tri(-1.0), fg, Stroke::NONE));
        }
        TransportIcon::ToEnd => {
            painter.add(Shape::convex_polygon(tri(1.0), fg, Stroke::NONE));
            painter.rect_filled(
                egui::Rect::from_min_size(c + Vec2::new(5.5, -7.0), Vec2::new(2.5, 14.0)),
                CornerRadius::ZERO,
                fg,
            );
        }
    }
    response.on_hover_text(tooltip)
}

#[cfg(test)]
mod tests {
    use eddie_core::editor::media::{AssetState, BinAsset};
    use eddie_core::editor::sweep;
    use openreel_engine::Timeline;
    use openreel_media::MediaInfo;

    use super::*;

    fn pointer_button(x: f32, y: f32, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: egui::Pos2::new(x, y),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn run_view_frames(app: &mut EditorApp, ctx: &egui::Context, frames: &[Vec<egui::Event>]) {
        let visuals: VisualMap<'_> = [
            (
                "/media/a.mp4",
                ClipVisual {
                    duration: 30.0,
                    peaks: &[],
                    thumbs: &[],
                },
            ),
            (
                "/media/b.mp4",
                ClipVisual {
                    duration: 30.0,
                    peaks: &[],
                    thumbs: &[],
                },
            ),
        ]
        .into();
        let EditorApp {
            editor,
            timeline_view,
            ..
        } = app;
        let Editor {
            timeline,
            playhead,
            selected,
            viewport,
            ..
        } = editor;
        timeline_view.port = *viewport;
        for (i, events) in frames.iter().enumerate() {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1200.0, 400.0),
                )),
                time: Some(i as f64 * 0.016),
                events: events.clone(),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        timeline_view.show(ui, timeline, playhead, selected, &visuals);
                    });
            });
        }
    }

    #[test]
    fn finished_export_offers_open_folder_until_the_status_moves_on() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless(&ctx);
        app.editor.status = "Exported montage.mp4".to_owned();
        app.editor.exported = Some((
            PathBuf::from("/videos/cut/montage.mp4"),
            app.editor.status.clone(),
        ));
        let shows_open_folder = |app: &mut EditorApp| {
            ctx.run(egui::RawInput::default(), |ctx| app.status_bar(ctx))
            .shapes
            .iter()
            .any(|shape| {
                matches!(&shape.shape, Shape::Text(t) if t.galley.text() == "Open folder")
            })
        };
        assert!(shows_open_folder(&mut app));
        app.editor.status = "Split clip.".to_owned();
        assert!(!shows_open_folder(&mut app), "stale exports offer nothing");
    }

    #[test]
    fn double_clicking_a_monitor_caption_opens_its_text_for_editing() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless(&ctx);
        let text = "A person talking stays a person talking";
        let title = app
            .editor
            .timeline
            .add_clip(text, ClipKind::Title, 0, 0.0, 4.0, 0.0);
        app.editor.playhead = 1.0;
        let mut time = 0.0;
        let mut render = |app: &mut EditorApp, events: Vec<egui::Event>| {
            time += 0.05;
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1280.0, 720.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    app.right_dock(ctx);
                    app.central(ctx);
                },
            )
        };
        render(&mut app, Vec::new());
        let output = render(&mut app, Vec::new());
        let caption = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                Shape::Text(t) if t.galley.text() == text => {
                    Some(t.visual_bounding_rect().center())
                }
                _ => None,
            })
            .expect("caption on the monitor");
        let click = |pressed| pointer_button(caption.x, caption.y, pressed);
        render(
            &mut app,
            vec![
                egui::Event::PointerMoved(caption),
                click(true),
                click(false),
            ],
        );
        render(&mut app, vec![click(true), click(false)]);
        assert_eq!(app.editor.selected, Some(title));
        render(&mut app, Vec::new());
        render(&mut app, vec![egui::Event::Text(" now".into())]);
        assert_eq!(
            app.editor.timeline.clip(title).unwrap().name,
            format!("{text} now"),
            "typing goes into the focused text box"
        );
    }

    #[test]
    fn console_enter_runs_edits_and_keeps_input_ready() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless(&ctx);
        app.show_console = true;
        let mut time = 0.0;
        let mut render = |app: &mut EditorApp, events: Vec<egui::Event>| {
            time += 0.02;
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(800.0, 600.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| app.console_panel(ctx),
            )
        };
        render(&mut app, Vec::new());
        let output = render(&mut app, Vec::new());
        let input = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                Shape::Text(text)
                    if text.galley.text() == "JavaScript. help() lists the editor API." =>
                {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            })
            .expect("console input");
        for pressed in [true, false] {
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(input),
                    pointer_button(input.x, input.y, pressed),
                ],
            );
        }
        for (code, expected_count) in [
            ("addTitle('Intro', 3, 2)", 1),
            ("", 1),
            ("addTitle('Outro', 7, 1)", 2),
        ] {
            render(&mut app, vec![egui::Event::Paste(code.to_owned())]);
            render(
                &mut app,
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                }],
            );
            render(
                &mut app,
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                }],
            );
            assert_eq!(
                app.editor.timeline.clips.len(),
                expected_count,
                "submitted {code}"
            );
            assert!(app.console_input.is_empty());
        }
        let intro = &app.editor.timeline.clips[0];
        assert_eq!(
            (&*intro.name, intro.start, intro.duration),
            ("Intro", 3.0, 2.0)
        );
        let outro = &app.editor.timeline.clips[1];
        assert_eq!(
            (&*outro.name, outro.start, outro.duration),
            ("Outro", 7.0, 1.0)
        );
    }

    #[test]
    fn inspecting_media_does_not_change_scene_timing() {
        for (start, duration) in [(0.157, 0.001), (2.135, 3.2866666666666666 - 2.135)] {
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless(&ctx);
            let video = app.editor.timeline.add_media_clip(
                "Scene",
                ClipKind::Video,
                0,
                start,
                duration,
                2.135,
                "/media/scene.mp4",
            );
            let audio = app.editor.timeline.detach_audio(video).unwrap();
            let expected = app.editor.timeline.clone();
            let mut time = 0.0;
            for selected in [video, audio] {
                app.editor.selected = Some(selected);
                for _ in 0..3 {
                    time += 0.02;
                    let _ = ctx.run(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                Vec2::new(1200.0, 800.0),
                            )),
                            time: Some(time),
                            ..Default::default()
                        },
                        |ctx| app.right_dock(ctx),
                    );
                    assert_eq!(
                        app.editor.timeline, expected,
                        "viewing Details must not edit media"
                    );
                }
            }
        }
    }

    #[test]
    fn inspector_start_moves_both_members_of_a_linked_pair() {
        for edit_audio in [false, true] {
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless(&ctx);
            let video = app.editor.timeline.add_media_clip(
                "Picture",
                ClipKind::Video,
                0,
                0.0,
                4.0,
                0.0,
                "/media/a.mp4",
            );
            let audio = app.editor.timeline.detach_audio(video).unwrap();
            app.editor.selected = Some(if edit_audio { audio } else { video });
            let mut time = 0.0;
            let mut render = |app: &mut EditorApp, events: Vec<egui::Event>| {
                time += 0.02;
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(400.0, 800.0),
                        )),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| app.inspector_content(ui));
                    },
                )
            };
            render(&mut app, Vec::new());
            let output = render(&mut app, Vec::new());
            let start = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Text(text)
                        if text
                            .galley
                            .text()
                            .trim_end_matches(" s")
                            .parse::<f64>()
                            .ok()
                            == Some(0.0) =>
                    {
                        Some(text.visual_bounding_rect().center())
                    }
                    _ => None,
                })
                .expect("Start numeric field");
            for pressed in [true, false, true, false] {
                render(
                    &mut app,
                    vec![
                        egui::Event::PointerMoved(start),
                        pointer_button(start.x, start.y, pressed),
                    ],
                );
            }
            render(
                &mut app,
                vec![
                    egui::Event::Key {
                        key: egui::Key::A,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers {
                            ctrl: true,
                            command: true,
                            ..Default::default()
                        },
                    },
                    egui::Event::Paste("5".to_owned()),
                ],
            );
            render(
                &mut app,
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                }],
            );
            render(&mut app, Vec::new());
            let selected = app
                .editor
                .timeline
                .clip(app.editor.selected.unwrap())
                .unwrap();
            assert_eq!(selected.start, 5.0, "the real inspector field was edited");
            assert_eq!(app.editor.timeline.clip(video).unwrap().start, 5.0);
            assert_eq!(app.editor.timeline.clip(audio).unwrap().start, 5.0);
            assert_eq!(
                app.editor.timeline.clip(video).unwrap().linked_clip,
                Some(audio)
            );
            assert_eq!(
                app.editor.timeline.clip(audio).unwrap().linked_clip,
                Some(video)
            );
        }
    }

    /// Regression: left-trimming clip B (which follows clip A) with the
    /// magnet on must keep B's end fixed after a full app frame, not
    /// just inside the timeline view. `EditorApp::update` used to run
    /// its own per-frame `close_gaps` after the panels, rippling B
    /// back to A's end in the same frame the drag ended and defeating
    /// the view-level TrimLeft carve-out.
    #[test]
    fn left_trim_survives_app_frame_with_magnet_on() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless(&ctx);
        assert!(app.editor.viewport.magnet);
        let a_id = app.editor.timeline.add_media_clip(
            "a.mp4",
            ClipKind::Video,
            1,
            0.0,
            10.0,
            0.0,
            "/media/a.mp4",
        );
        let b_id = app.editor.timeline.add_media_clip(
            "b.mp4",
            ClipKind::Video,
            1,
            12.0,
            10.0,
            0.0,
            "/media/b.mp4",
        );
        // B spans t=12..22, so its left edge is x=720 (144 + 12*48) at
        // track row y=99. Drag the edge 144px right (+3s) and release.
        let frames = vec![
            vec![],
            vec![pointer_button(720.0, 99.0, true)],
            vec![egui::Event::PointerMoved(egui::Pos2::new(864.0, 99.0))],
            vec![pointer_button(864.0, 99.0, false)],
        ];
        run_view_frames(&mut app, &ctx, &frames);
        let b = app.editor.timeline.clip(b_id).unwrap();
        assert!(
            (b.start - 15.0).abs() < 0.02 && (b.end() - 22.0).abs() < 1e-6,
            "drag alone should land B at 15..22, got {}..{}",
            b.start,
            b.end()
        );

        // One full app frame tail, exactly as update() runs it.
        app.finish_frame(&ctx, None);
        let a = app.editor.timeline.clip(a_id).unwrap();
        assert_eq!(a.start, 0.0);
        assert!((a.end() - 10.0).abs() < 1e-6);
        let b = app.editor.timeline.clip(b_id).unwrap();
        assert!(
            (b.start - 15.0).abs() < 0.02,
            "left trim gap must survive the app frame, got start {}",
            b.start
        );
        assert!(
            (b.end() - 22.0).abs() < 1e-6,
            "left trim must keep the end fixed across app frames, got {}",
            b.end()
        );
        assert!((b.source_in - 3.0).abs() < 0.02);
    }

    /// Clicking the playhead readout opens a jump box; typing a time spec
    /// over the selected timecode and pressing Enter seeks there.
    #[test]
    fn playhead_readout_jumps_to_a_typed_time_spec() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless(&ctx);
        app.editor
            .timeline
            .add_clip("Intro", ClipKind::Title, 0, 0.0, 8.0, 0.0);
        let outro = app
            .editor
            .timeline
            .add_clip("Outro", ClipKind::Title, 0, 8.0, 4.0, 0.0);
        let mut time = 0.0;
        let mut render = |app: &mut EditorApp, events| {
            time += 0.05;
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(900.0, 600.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| app.central(ctx),
            )
        };
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        render(&mut app, Vec::new());
        let output = render(&mut app, Vec::new());
        let readout = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                Shape::Text(text) if text.galley.text() == format_timecode(0.0, 30.0) => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            })
            .expect("playhead readout");
        let mut jump = |app: &mut EditorApp, spec: &str| {
            render(app, vec![egui::Event::PointerMoved(readout)]);
            for pressed in [true, false] {
                render(app, vec![pointer_button(readout.x, readout.y, pressed)]);
            }
            render(app, Vec::new());
            render(app, vec![egui::Event::Text(spec.to_owned())]);
            render(app, vec![enter.clone()]);
            render(app, Vec::new());
        };
        jump(&mut app, &format!("clip:{outro}@50%"));
        assert!(
            (app.editor.playhead - 10.0).abs() < 1e-6,
            "got {}",
            app.editor.playhead
        );
        assert!(app.jump_text.is_none(), "Enter closes the box");

        jump(&mut app, "clip:99");
        assert!(
            (app.editor.playhead - 10.0).abs() < 1e-6,
            "a bad spec does not move"
        );
        assert!(
            app.editor.status.contains("no clip 99"),
            "{}",
            app.editor.status
        );
    }

    /// The monitor scrubber spans the whole edit: a click a quarter of the
    /// way along moves the playhead a quarter of the way through it.
    #[test]
    fn transport_scrubber_click_seeks_proportionally() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless(&ctx);
        app.editor
            .timeline
            .add_clip("Intro", ClipKind::Title, 0, 0.0, 8.0, 0.0);
        let mut time = 0.0;
        let mut render = |app: &mut EditorApp, events| {
            time += 0.05;
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(900.0, 600.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| app.central(ctx),
            )
        };
        render(&mut app, Vec::new());
        let output = render(&mut app, Vec::new());
        let text_rect = |label: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Text(text) if text.galley.text() == label => {
                        Some(text.visual_bounding_rect())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("transport readout {label}"))
        };
        let position = text_rect(&format_timecode(0.0, app.editor.timeline.fps));
        let length = text_rect(&format_timecode(8.0, app.editor.timeline.fps));
        // The track runs from 6 px left of the position readout to 6 px
        // right of the length readout, 29 px above their centre line.
        let (left, right) = (position.left() - 6.0, length.right() + 6.0);
        let target = egui::pos2(left + (right - left) * 0.25, position.center().y - 29.0);
        render(&mut app, vec![egui::Event::PointerMoved(target)]);
        for pressed in [true, false] {
            render(&mut app, vec![pointer_button(target.x, target.y, pressed)]);
        }
        assert!(
            (app.editor.playhead - 2.0).abs() < 0.1,
            "a click at 25% of the scrubber seeks to 2 s, got {}",
            app.editor.playhead
        );
        assert!(!app.editor.transport_scrubbing, "release ends the scrub");
    }

    #[test]
    fn gallery_cards_filters_and_search_reach_the_timeline_at_both_sizes() {
        use eddie_core::editor::gallery::{
            GalleryDuration, GalleryFilters, GalleryQuality, GalleryView,
        };

        let ctx = egui::Context::default();
        let mut app = EditorApp::headless(&ctx);
        for (path, name) in [
            ("/media/expo.mp4", "Expo.mp4"),
            ("/media/interview.mov", "Interview.mov"),
        ] {
            let candidates: Vec<_> = [
                (0.25, 1.5, 82.0, "action", "Rocket launch ready"),
                (10.0, 2.0, 96.0, "action", "Rocket launch now"),
                (20.0, 7.5, 87.0, "action", "Launch later"),
                (40.0, 30.0, 88.0, "talking", "Rocket payload"),
                (80.0, 8.0, 77.0, "action", "Slow launch"),
                (100.0, 29.5, 80.0, "action", "Rocket flame"),
            ]
            .into_iter()
            .enumerate()
            .map(
                |(index, (source_in, duration, score, category, text))| sweep::SweepCandidate {
                    id: format!("{path}#pick-{index}"),
                    asset_path: path.to_owned(),
                    asset_name: name.to_owned(),
                    title: format!("Take {index}"),
                    source_in,
                    duration,
                    score,
                    reason: if index == 2 {
                        "Crowd portrait"
                    } else {
                        "Useful range"
                    }
                    .to_owned(),
                    stability: 0.9,
                    audio_energy: 0.1,
                    audio_peak: 0.2,
                    category: category.to_owned(),
                    tags: vec![if matches!(index, 0 | 1 | 3 | 5) {
                        "prime"
                    } else {
                        "steady"
                    }
                    .to_owned()],
                    motion_score: 0.3,
                    audio_score: 0.4,
                    music_bed: false,
                    visual: None,
                    has_video: true,
                    transcript: vec![sweep::TranscriptSegment {
                        start: source_in,
                        end: source_in + duration,
                        speaker: None,
                        text: text.to_owned(),
                        words: Vec::new(),
                        session: None,
                    }],
                    review: sweep::ClipReview {
                        status: match index {
                            2 => sweep::ReviewStatus::NeedsReview,
                            4 => sweep::ReviewStatus::Local,
                            _ => sweep::ReviewStatus::Reviewed,
                        },
                        ..Default::default()
                    },
                    dismissed: index == 5,
                },
            )
            .collect();
            let mut scene = candidates[0].clone();
            scene.id = format!("{path}#scene-0");
            scene.source_in = 0.0;
            scene.duration = 200.0;
            scene.review.status = sweep::ReviewStatus::Rejected;
            app.editor.assets.push(BinAsset {
                info: MediaInfo {
                    path: path.to_owned(),
                    name: name.to_owned(),
                    duration: 200.0,
                    width: 64,
                    height: 36,
                    fps: 30.0,
                    has_video: true,
                    has_audio: false,
                },
                size_bytes: 0,
                state: AssetState::Ready,
                peaks: Vec::new(),
                beats: None,
                thumbs: Vec::new(),
                sweep: sweep::SweepData {
                    status: sweep::SweepStatus::Ready,
                    candidates,
                    scene_clips: vec![scene],
                    ..Default::default()
                },
            });
        }
        app.editor.curator.filters = GalleryFilters::default();
        app.editor.curator.view = GalleryView::Scenes;
        let scene = app.editor.assets[0].sweep.scene_clips[0].clone();
        app.editor.assets[0].sweep.scene_clips = (0..48)
            .map(|index| {
                let mut scene = scene.clone();
                scene.id = format!("scene-{index}");
                scene.source_in = index as f64 * 4.0;
                scene.duration = 4.0;
                scene
            })
            .collect();
        app.editor.open_curator_gallery();
        let mut time = 0.0;
        for size in [Vec2::new(1440.0, 900.0), Vec2::new(1024.0, 640.0)] {
            app.editor.timeline = Timeline::empty_project();
            let mut render = |app: &mut EditorApp, events| {
                time += 0.1;
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        app.menu_bar(ctx);
                        app.status_bar(ctx);
                        app.timeline_panel(ctx);
                        app.media_bin(ctx);
                        app.right_dock(ctx);
                        app.central(ctx);
                    },
                )
            };
            render(&mut app, Vec::new());
            render(&mut app, Vec::new());
            let scroll_position = egui::pos2(size.x / 2.0, size.y - 300.0);
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(scroll_position),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(0.0, 10000.0),
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
            for _ in 0..10 {
                render(&mut app, Vec::new());
            }
            let output = render(&mut app, Vec::new());
            let add = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Text(text) if text.galley.text() == "Add" => {
                        Some(text.visual_bounding_rect().center())
                    }
                    _ => None,
                })
                .expect("first card's Add button");
            // Move before pressing, as a native pointer does, so the scroll area's
            // drag handling cannot mistake the move from the wheel location for a drag.
            render(&mut app, vec![egui::Event::PointerMoved(add)]);
            for pressed in [true, false] {
                render(&mut app, vec![pointer_button(add.x, add.y, pressed)]);
            }
            assert_eq!(
                app.editor.timeline.clips.len(),
                1,
                "first card actions fit the gallery at {size:?}"
            );
            assert_eq!(app.editor.timeline.clips[0].source_in, 0.0);
            app.editor.timeline = Timeline::empty_project();
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(scroll_position),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(0.0, -10000.0),
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
            for _ in 0..10 {
                render(&mut app, Vec::new());
            }
            let output = render(&mut app, Vec::new());
            let add = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    Shape::Text(text) if text.galley.text() == "Add" => {
                        Some(text.visual_bounding_rect().center())
                    }
                    _ => None,
                })
                .max_by(|a, b| a.y.total_cmp(&b.y))
                .expect("last card's Add button");
            render(&mut app, vec![egui::Event::PointerMoved(add)]);
            for pressed in [true, false] {
                render(&mut app, vec![pointer_button(add.x, add.y, pressed)]);
            }
            assert_eq!(
                app.editor.timeline.clips.len(),
                1,
                "last card stays reachable after scrolling at {size:?}"
            );
            let clip = &app.editor.timeline.clips[0];
            assert_eq!(clip.source.as_deref(), Some("/media/interview.mov"));
            assert_eq!((clip.source_in, clip.duration), (0.0, 200.0));

            if size.x == 1024.0 {
                let mut click_text = |app: &mut EditorApp, label: &str| {
                    render(app, Vec::new());
                    let output = render(app, Vec::new());
                    let position = output
                        .shapes
                        .iter()
                        .rev()
                        .find_map(|shape| match &shape.shape {
                            Shape::Text(text) if text.galley.text() == label => {
                                let rect = text.visual_bounding_rect();
                                shape
                                    .clip_rect
                                    .contains(rect.center())
                                    .then_some(rect.center())
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("visible gallery control {label}"));
                    render(app, vec![egui::Event::PointerMoved(position)]);
                    for pressed in [true, false] {
                        render(app, vec![pointer_button(position.x, position.y, pressed)]);
                    }
                };
                click_text(&mut app, "Clips");
                for (menu, control, value) in [
                    ("Filters", "All types", "Action"),
                    ("Filters (1)", "Any quality", "Prime"),
                    ("Filters (2)", "Any duration", "Under 2 s"),
                ] {
                    click_text(&mut app, menu);
                    click_text(&mut app, control);
                    click_text(&mut app, value);
                }
                assert_eq!(
                    app.editor.curator.filters.category.as_deref(),
                    Some("action")
                );
                assert!(app.editor.curator.filters.quality == GalleryQuality::Prime);
                assert!(app.editor.curator.filters.duration == GalleryDuration::UnderTwo);
                assert_eq!(
                    app.editor.timeline.clips.len(),
                    1,
                    "filtering never adds clips"
                );
                click_text(&mut app, "Add shown");
                assert_eq!(app.editor.timeline.clips.len(), 3);
                for clip in &app.editor.timeline.clips[1..] {
                    assert_eq!((clip.source_in, clip.duration), (0.25, 1.5));
                }
                click_text(&mut app, "Filters (3)");
                click_text(&mut app, "Reset");
                assert!(app.editor.curator.filters == GalleryFilters::default());
                assert_eq!(
                    app.editor.timeline.clips.len(),
                    3,
                    "reset only changes the query"
                );
                click_text(&mut app, "Search clips…");
                for character in "rocket launch".chars() {
                    render(&mut app, vec![egui::Event::Text(character.to_string())]);
                }
                assert_eq!(app.editor.curator.filters.search, "rocket launch");
            }
        }
    }

    /// Open and New ask before dropping unsaved edits. A saved project is
    /// replaced without a question.
    #[test]
    fn open_and_new_project_ask_before_dropping_unsaved_edits() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless(&ctx);
        app.editor
            .run_console("addTitle('Booth', 3, 2);".to_owned());
        assert!(app.editor.has_unsaved_changes(), "{}", app.editor.status);
        for (action, discard) in [
            (Action::OpenProject, project::Discard::Open),
            (Action::NewProject, project::Discard::New),
        ] {
            app.handle_action(&ctx, action);
            assert_eq!(app.pending_discard, Some(discard));
            assert_eq!(
                app.editor.timeline.clips.len(),
                1,
                "nothing is dropped before an answer"
            );
            app.pending_discard = None;
        }

        let dir = std::env::temp_dir().join(format!("eddie-discard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        app.editor.save_project_to(&dir.join("saved")).unwrap();
        app.handle_action(&ctx, Action::NewProject);
        assert_eq!(app.pending_discard, None);
        assert!(app.editor.timeline.clips.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Once the renderer has uploaded a preview picture, the next upload
    /// refills its pixels instead of allocating another frame.
    #[test]
    fn preview_uploads_reuse_released_pixels() {
        let ctx = egui::Context::default();
        let app = EditorApp::headless(&ctx);
        let mut uploaded = Vec::new();
        for value in [10_u8, 200] {
            let frame = RawFrame {
                width: 4,
                height: 2,
                rgba: [value, value, value, 255].repeat(8),
            };
            MonitorSink(Rc::clone(&app.monitor)).show(&frame);
            let delta = ctx.tex_manager().write().take_delta();
            let egui::ImageData::Color(image) = &delta.set.last().unwrap().1.image;
            assert_eq!(image.pixels[0], egui::Color32::from_gray(value));
            uploaded.push(image.pixels.as_ptr());
        }
        assert_eq!(uploaded[0], uploaded[1], "second upload allocated");
    }
}

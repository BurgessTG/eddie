//! The Settings window: project format, hardware decode and the
//! transcription provider. `eddie_core` owns loading and saving.

use eddie_core::editor::sweep::transcribe::Provider;
use eframe::egui::{self, RichText};
use openreel_engine::Orientation;

use super::EditorApp;
use crate::theme;

impl EditorApp {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let mut format_choice = None;
        let before = self.editor.settings.clone();
        let mut close_clicked = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        egui::Window::new("Settings")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(egui::vec2(420.0, 0.0))
            .frame(
                egui::Frame::new()
                    .fill(theme::CARD)
                    .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                    .corner_radius(egui::CornerRadius::same(theme::R_LG))
                    .inner_margin(egui::Margin::same(20))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 8],
                        blur: 28,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(140),
                    }),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Settings")
                            .family(theme::semibold())
                            .size(15.0)
                            .color(theme::FOREGROUND),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if close_icon_button(ui).on_hover_text("Close settings (Esc)").clicked() {
                            close_clicked = true;
                        }
                    });
                });
                ui.add_space(16.0);
                egui::Grid::new("settings")
                    .num_columns(2)
                    .spacing([16.0, 10.0])
                    .min_col_width(84.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Playback").color(theme::MUTED_FG));
                        ui.checkbox(&mut self.editor.settings.hardware_decode, "Hardware decode")
                            .on_hover_text(
                                "Play HEVC, AV1, VP9 and footage above 1440p on the GPU (VAAPI) when available. Scrubbing stays on the CPU; a GPU error switches back to the CPU.",
                            );
                        ui.end_row();

                        let has_meta_key = self.editor.settings.has_meta_key();
                        let speech = &mut self.editor.settings.transcription;
                        ui.label(RichText::new("Speech").color(theme::MUTED_FG));
                        egui::ComboBox::from_id_salt("speech_provider")
                            .width(FIELD_W + 16.0)
                            .selected_text(speech.provider.label())
                            .show_ui(ui, |ui| {
                                for provider in Provider::ALL {
                                    ui.selectable_value(
                                        &mut speech.provider,
                                        provider,
                                        provider.label(),
                                    );
                                }
                            })
                            .response
                            .on_hover_text(
                                "Who turns speech into text. Local runs whisper on this computer with no key; cloud providers upload the audio.",
                            );
                        ui.end_row();

                        let (model_hint, model_tip) = match speech.provider {
                            Provider::Local => (
                                "Best installed",
                                "A whisper model name such as large-v3 or large-v3-turbo, or a path to a ggml file. Empty uses the most accurate one installed.",
                            ),
                            Provider::Meta => {
                                ("muse-voice-transcribe-1.0", "Meta transcription model ID")
                            }
                            Provider::OpenAi => (
                                "whisper-1",
                                "Model ID. Word timing needs a whisper model, such as whisper-1 or Groq's whisper-large-v3.",
                            ),
                        };
                        ui.label(RichText::new("Speech model").color(theme::MUTED_FG));
                        ui.add(field(&mut speech.model, model_hint))
                            .on_hover_text(model_tip);
                        ui.end_row();

                        if speech.provider != Provider::Local {
                            let (url_hint, key_hint) = match speech.provider {
                                Provider::Meta if has_meta_key => {
                                    ("https://api.meta.ai/v1", "Saved Meta key")
                                }
                                Provider::Meta => ("https://api.meta.ai/v1", "Not set"),
                                _ => ("https://api.openai.com/v1", "Not set"),
                            };
                            ui.label(RichText::new("Speech URL").color(theme::MUTED_FG));
                            ui.add(field(&mut speech.base_url, url_hint)).on_hover_text(
                                "Transcription API base URL, such as https://api.groq.com/openai/v1",
                            );
                            ui.end_row();

                            ui.label(RichText::new("Speech key").color(theme::MUTED_FG));
                            ui.add(field(&mut speech.api_key, key_hint).password(true))
                                .on_hover_text(
                                    "Transcription API key. Stored only in the local app config.",
                                );
                            ui.end_row();
                        }

                        ui.label(RichText::new("Language").color(theme::MUTED_FG));
                        ui.add(field(&mut speech.language, "Detect")).on_hover_text(
                            "Spoken language as a code such as en or es. Empty detects it.",
                        );
                        ui.end_row();

                        ui.label(RichText::new("Vocabulary").color(theme::MUTED_FG));
                        ui.add(field(&mut speech.vocabulary, "Names to spell right"))
                            .on_hover_text(
                                "A short, comma-separated list of names and terms, such as Eddie, Bothell. Local and OpenAI-compatible providers use it; it can also make other words worse.",
                            );
                        ui.end_row();

                        ui.label(RichText::new("Format").color(theme::MUTED_FG));
                        format_choice = theme::segmented(
                            ui,
                            self.editor.settings.format,
                            &[
                                (
                                    Orientation::Landscape,
                                    "Landscape",
                                    "Wide project; portrait video is not added",
                                ),
                                (
                                    Orientation::Portrait,
                                    "Portrait",
                                    "Tall project; landscape video is not added",
                                ),
                            ],
                        );
                        ui.end_row();
                    });
                ui.add_space(18.0);
                theme::divider(ui);
                ui.add_space(14.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::primary_button(ui, "Done")
                        .on_hover_text("Close settings; changes save as you edit")
                        .clicked()
                    {
                        close_clicked = true;
                    }
                });
            });
        self.show_settings = open && !close_clicked;
        if let Some(format) = format_choice {
            self.editor.settings.format = format;
        }
        self.editor.settings_changed(&before);
    }
}

const FIELD_W: f32 = 300.0;

fn field<'a>(text: &'a mut String, hint: &str) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text)
        .hint_text(hint)
        .margin(egui::Margin::symmetric(8, 6))
        .desired_width(FIELD_W)
}

/// Borderless square close button with an X glyph.
fn close_icon_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, egui::CornerRadius::same(theme::R_MD), theme::ACCENT);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let fg = if response.hovered() {
        theme::FOREGROUND
    } else {
        theme::MUTED_FG
    };
    let c = rect.center();
    let stroke = egui::Stroke::new(1.6_f32, fg);
    painter.line_segment(
        [c + egui::vec2(-5.0, -5.0), c + egui::vec2(5.0, 5.0)],
        stroke,
    );
    painter.line_segment(
        [c + egui::vec2(5.0, -5.0), c + egui::vec2(-5.0, 5.0)],
        stroke,
    );
    response
}
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eddie_core::editor::settings::{config_path, Settings};

    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "openreel-settings-test-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Runs `run` in a child test process whose config directory is a fresh
    /// temporary one, so tests never touch the user's settings.
    fn with_isolated_config(test: &str, run: impl FnOnce(PathBuf)) {
        if std::env::var("OPENREEL_ISOLATED_SETTINGS_TEST").as_deref() == Ok(test) {
            run(config_path().expect("isolated settings path"));
            return;
        }
        let dir = TestDir::new();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env("OPENREEL_ISOLATED_SETTINGS_TEST", test)
            .env("HOME", &dir.0)
            .env("XDG_CONFIG_HOME", &dir.0)
            .env("APPDATA", &dir.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated settings test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    fn settings_frame(ctx: &egui::Context, app: &mut EditorApp, events: Vec<egui::Event>) {
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1024.0, 768.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| app.settings_window(ctx),
        );
        drop(output);
    }

    fn click_settings_label(ctx: &egui::Context, app: &mut EditorApp, label: &str) {
        settings_frame(ctx, app, Vec::new());
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1024.0, 768.0),
                )),
                ..Default::default()
            },
            |ctx| app.settings_window(ctx),
        );
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            })
            .expect("settings control");
        for pressed in [true, false] {
            settings_frame(
                ctx,
                app,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
        }
    }

    #[test]
    fn settings_save_failure_is_displayed_and_cleans_temporary_file() {
        with_isolated_config(
            "app::settings::tests::settings_save_failure_is_displayed_and_cleans_temporary_file",
            |path| {
                // A directory where the file should be makes the rename fail.
                std::fs::create_dir_all(&path).unwrap();
                let previous = path.join("previous.json");
                std::fs::write(&previous, "keep this file").unwrap();
                let error = Settings::default().save().unwrap_err();
                assert_eq!(error, "Couldn't save settings.");

                let ctx = egui::Context::default();
                let mut app = EditorApp::headless(&ctx);
                app.show_settings = true;
                click_settings_label(&ctx, &mut app, "Hardware decode");
                assert!(app.editor.settings.hardware_decode);
                assert_eq!(app.editor.status, error);
                openreel_media::set_hardware_decode(false);
                assert_eq!(std::fs::read_to_string(previous).unwrap(), "keep this file");
                assert_eq!(
                    std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                    1
                );
            },
        );
    }
}

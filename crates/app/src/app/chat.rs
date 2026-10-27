//! Chat editing panel and Meta Model API client.
//!
//! The network call lives here in app code, on a worker thread. The model
//! returns JavaScript only; the app executes it through the QuickJS script
//! engine so every mutation still arrives as a `ScriptCommand`. The API key
//! is only read from the private settings file and sent as a header. It is
//! never logged, never put in status text, and never included in prompts.

use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use super::EditorApp;
use crate::theme;
use eframe::egui::{self, RichText};

pub(super) const MODEL_SPARK: &str = "muse-spark";
pub(super) const MODEL_WONDER: &str = "muse-wonder";
const DEFAULT_BASE_URL: &str = "https://api.meta.ai/v1";

fn model_label(model: &str) -> &str {
    match model {
        MODEL_SPARK => "Muse Spark",
        MODEL_WONDER => "Muse Wonder",
        other => other,
    }
}

#[derive(Clone)]
pub(super) struct ChatSettings {
    pub(super) api_key: String,
    pub(super) base_url: String,
    pub(super) model: String,
}

impl Default for ChatSettings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: MODEL_SPARK.to_owned(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatRole {
    User,
    Assistant,
}

#[derive(Clone)]
pub(super) struct ChatMessage {
    pub(super) role: ChatRole,
    pub(super) text: String,
}

pub(super) struct PendingChat {
    request: String,
    attempt: u8,
}

pub(super) enum ChatWorkerResult {
    Generated(String),
    Failed(String),
}

fn config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(|dir| PathBuf::from(dir).join("OpenReel").join("config.json"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home).join("Library/Application Support/OpenReel/config.json")
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
            Some(PathBuf::from(dir).join("openreel/config.json"))
        } else {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".config/openreel/config.json"))
        }
    }
    #[cfg(not(any(unix, target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

impl ChatSettings {
    pub(super) fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Self::default();
        };
        let mut settings = Self::default();
        if let Some(v) = value["api_key"].as_str() {
            settings.api_key = v.to_owned();
        }
        if let Some(v) = value["base_url"].as_str() {
            if !v.trim().is_empty() {
                settings.base_url = v.to_owned();
            }
        }
        if let Some(v) = value["model"].as_str() {
            if !v.trim().is_empty() {
                settings.model = v.to_owned();
            }
        }
        settings
    }

    pub(super) fn save(&self) {
        let Some(path) = config_path() else { return };
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let body = serde_json::json!({
            "api_key": self.api_key,
            "base_url": self.base_url,
            "model": self.model,
        });
        if std::fs::write(&path, body.to_string()).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }

    pub(super) fn can_call_model(&self) -> bool {
        !self.api_key.trim().is_empty() || self.allows_missing_key()
    }

    fn allows_missing_key(&self) -> bool {
        let base = self.base_url.to_lowercase();
        base.contains("://localhost") || base.contains("://127.0.0.1")
    }
}

pub(crate) const SYSTEM_PROMPT: &str = "\
You edit a video timeline inside OpenReel by writing JavaScript. \
Reply with ONLY executable JavaScript. No markdown fences. No explanation outside the code. \
End with exactly one console.log(\"Done — ...\") line containing a short plain-language summary for the user. \
Available functions: state(), clips(), assets(), sweepCandidates(), play(), pause(), toggle(), seek(t), step(frames), \
selectClip(id), split(at), splitClip(id, at), deleteClip(id?), moveClip(id, start), \
trimClip(id, left, right) where -1 keeps an edge, renameClip(id, name), setOpacity(id, v), \
setVolume(id, v), addTitle(text, at?, dur?), addAsset(nameOrPath, at?), \
addAssetRange(nameOrPath, at, sourceIn, dur), \
setTool('select'|'split'), setMagnet(bool), setZoom(pxPerSec), closeGaps(). \
state() returns {playhead, playing, selected, duration, fps, tool, magnet, clips, assets, sweepCandidates}. \
For “build the montage from the sweep”, replace the current timeline clips with the highest-ranked video sweepCandidates() (all candidates only if none has video), placed sequentially from 0 with addAssetRange(candidate.asset, cursor, candidate.sourceIn, candidate.duration). \
Clip fields: id, name, kind ('video'|'audio'|'title'), track, start, duration, sourceIn, source, opacity, volume. \
Tracks: 0 titles, 1 main video, 2 audio. Times are seconds. \
Refer to clips by the ids in state(). Do not invent ids or asset names. \
Keep edits minimal and preserve source limits. If the request is impossible with the given state, \
throw new Error('short reason').";

/// User-message text shared by the API path and the dev harness, so both
/// drivers see the identical prompt contract.
pub(crate) fn chat_user_content(
    request: &str,
    state_json: &str,
    recent: &[(bool, String)],
    correction: Option<(&str, &str)>,
) -> String {
    let mut user = format!("Timeline state JSON:\n{state_json}\n\nRequest:\n{request}");
    if !recent.is_empty() {
        let history: Vec<String> = recent
            .iter()
            .map(|(is_user, text)| {
                format!("{}: {}", if *is_user { "User" } else { "Assistant" }, text)
            })
            .collect();
        user = format!("Recent conversation:\n{}\n\n{user}", history.join("\n"));
    }
    if let Some((failed_js, error)) = correction {
        user = format!(
            "Your previous JavaScript failed in the sandbox and made no edit.\n\
             Error:\n{error}\n\nPrevious JavaScript:\n{failed_js}\n\n\
             Reply with corrected JavaScript only.\n\n{user}"
        );
    }
    user
}

pub(super) fn build_request_body(
    settings: &ChatSettings,
    request: &str,
    state_json: &str,
    recent: &[(bool, String)],
    correction: Option<(&str, &str)>,
) -> serde_json::Value {
    let user = chat_user_content(request, state_json, recent, correction);
    serde_json::json!({
        "model": settings.model,
        "temperature": 0.2,
        "stream": false,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user},
        ],
    })
}

pub(crate) fn extract_code(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(start) = raw.find("```") {
        let after = &raw[start + 3..];
        let after = after
            .strip_prefix("javascript")
            .or_else(|| after.strip_prefix("js"))
            .unwrap_or(after);
        if let Some(end) = after.find("```") {
            return after[..end].trim().to_owned();
        }
    }
    raw.to_owned()
}

pub(super) fn chat_content(value: &serde_json::Value) -> String {
    let content = &value["choices"][0]["message"]["content"];
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str().or_else(|| part.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub(super) fn parse_generated_js(value: &serde_json::Value) -> Result<String, String> {
    let text = chat_content(value);
    let code = extract_code(&text);
    if code.trim().is_empty() {
        Err("The model returned an empty response.".to_owned())
    } else {
        Ok(code)
    }
}

pub(super) fn api_error_for_status(status: u16) -> String {
    match status {
        401 | 403 => "Check your API key in Settings.".to_owned(),
        404 => "That model was not found. Check the model in Settings.".to_owned(),
        429 => "The model service is busy. Try again shortly.".to_owned(),
        500..=599 => "The model service had a problem. Try again shortly.".to_owned(),
        _ => "The model request was rejected. Check Settings and try again.".to_owned(),
    }
}

fn request_javascript(
    settings: &ChatSettings,
    request: &str,
    state_json: &str,
    recent: Vec<(bool, String)>,
    correction: Option<(String, String)>,
) -> Result<String, String> {
    let correction_ref = correction
        .as_ref()
        .map(|(js, err)| (js.as_str(), err.as_str()));
    let body = build_request_body(settings, request, state_json, &recent, correction_ref);
    let value = post_chat_completion(settings, &body)?;
    parse_generated_js(&value)
}

pub(super) fn post_chat_completion(
    settings: &ChatSettings,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let base = settings.base_url.trim().trim_end_matches('/');
    let url = if base.ends_with("/chat/completions") {
        base.to_owned()
    } else {
        format!("{base}/chat/completions")
    };
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut call = agent.post(&url).header("Content-Type", "application/json");
    if !settings.api_key.trim().is_empty() {
        call = call.header(
            "Authorization",
            format!("Bearer {}", settings.api_key.trim()),
        );
    }
    let mut response = call
        .send(body.to_string())
        .map_err(|_| "Couldn't reach the model service.".to_owned())?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(api_error_for_status(status));
    }
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|_| "Couldn't read the model response.".to_owned())?;
    serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|_| "Couldn't read the model response.".to_owned())
}

impl EditorApp {
    fn spawn_chat_request(&mut self, ctx: &egui::Context, correction: Option<(String, String)>) {
        let Some(pending) = self.chat_pending.as_ref() else {
            return;
        };
        let request = pending.request.clone();
        let settings = self.chat_settings.clone();
        let state = self.script_state_json();
        let recent: Vec<(bool, String)> = self
            .chat_messages
            .iter()
            .rev()
            .take(8)
            .rev()
            .map(|m| (m.role == ChatRole::User, m.text.clone()))
            .collect();
        let (tx, rx) = channel::<ChatWorkerResult>();
        self.chat_rx = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = match request_javascript(&settings, &request, &state, recent, correction) {
                Ok(js) => ChatWorkerResult::Generated(js),
                Err(message) => ChatWorkerResult::Failed(message),
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn submit_chat(&mut self, ctx: &egui::Context, text: String) {
        let text = text.trim().to_owned();
        if text.is_empty() || self.chat_pending.is_some() {
            return;
        }
        self.chat_messages.push(ChatMessage {
            role: ChatRole::User,
            text: text.clone(),
        });
        if self.chat_settings.api_key.trim().is_empty() && !self.chat_settings.allows_missing_key()
        {
            self.show_chat_settings = true;
            let message = "Add your Meta Model API key in Settings first.".to_owned();
            self.chat_messages.push(ChatMessage {
                role: ChatRole::Assistant,
                text: message.clone(),
            });
            self.status = message;
            return;
        }
        self.chat_pending = Some(PendingChat {
            request: text,
            attempt: 0,
        });
        self.spawn_chat_request(ctx, None);
    }

    pub(super) fn poll_chat(&mut self, ctx: &egui::Context) {
        let Some(result) = self.chat_rx.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            if self.chat_pending.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
            return;
        };
        self.chat_rx = None;
        match result {
            ChatWorkerResult::Failed(message) => {
                self.chat_pending = None;
                self.chat_messages.push(ChatMessage {
                    role: ChatRole::Assistant,
                    text: message.clone(),
                });
                self.status = message;
            }
            ChatWorkerResult::Generated(js) => {
                self.last_generated_js = js.clone();
                let state = self.script_state_json();
                let Some(engine) = self.script_engine.as_mut() else {
                    self.chat_pending = None;
                    let message = "I couldn't do that one — scripting is unavailable.".to_owned();
                    self.chat_messages.push(ChatMessage {
                        role: ChatRole::Assistant,
                        text: message.clone(),
                    });
                    self.status = message;
                    return;
                };
                match engine.eval_checked(&js, &state) {
                    Ok((_, commands)) => {
                        let command_count = commands.len();
                        let notes = self.apply_script_commands(ctx, commands);
                        let errors: Vec<String> = notes
                            .iter()
                            .filter(|n| n.starts_with("error:"))
                            .cloned()
                            .collect();
                        self.chat_pending = None;
                        let message = if !errors.is_empty() {
                            "I couldn't finish that one — try naming the clip.".to_owned()
                        } else {
                            summarize_chat_result(&notes, command_count)
                        };
                        self.chat_messages.push(ChatMessage {
                            role: ChatRole::Assistant,
                            text: message.clone(),
                        });
                        self.status = message;
                    }
                    Err(error) => {
                        let should_retry = self
                            .chat_pending
                            .as_ref()
                            .map(|p| p.attempt == 0)
                            .unwrap_or(false);
                        if should_retry {
                            if let Some(pending) = self.chat_pending.as_mut() {
                                pending.attempt = 1;
                            }
                            self.spawn_chat_request(ctx, Some((js, error)));
                        } else {
                            self.chat_pending = None;
                            let message =
                                "I couldn't do that one — try naming the clip.".to_owned();
                            self.chat_messages.push(ChatMessage {
                                role: ChatRole::Assistant,
                                text: message.clone(),
                            });
                            self.status = message;
                        }
                    }
                }
            }
        }
        if self.chat_messages.len() > 100 {
            let excess = self.chat_messages.len() - 100;
            self.chat_messages.drain(..excess);
        }
    }
}

impl EditorApp {
    pub(super) fn chat_panel(&mut self, ctx: &egui::Context) {
        if !self.show_chat {
            return;
        }
        let focus_requested = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::K));
        let mut submitted: Option<String> = None;
        egui::TopBottomPanel::bottom("chat")
            .resizable(true)
            .default_height(176.0)
            .height_range(120.0..=380.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    theme::panel_title(ui, "CHAT");
                    if self.chat_pending.is_some() {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label(RichText::new("Working…").color(theme::MUTED_FG));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::ghost_button(ui, "Settings")
                            .on_hover_text("Meta Model API settings")
                            .clicked()
                        {
                            self.show_chat_settings = !self.show_chat_settings;
                        }
                        let debug = if self.show_chat_debug {
                            theme::outline_button(ui, "Debug")
                        } else {
                            theme::ghost_button(ui, "Debug")
                        };
                        if debug
                            .on_hover_text("Show the last generated JavaScript")
                            .clicked()
                        {
                            self.show_chat_debug = !self.show_chat_debug;
                        }
                    });
                });
                ui.add_space(4.0);

                let debug_height = if self.show_chat_debug { 58.0 } else { 0.0 };
                let messages_height = (ui.available_height() - 42.0 - debug_height).max(28.0);
                egui::ScrollArea::vertical()
                    .id_salt("chat_messages")
                    .max_height(messages_height)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if self.chat_messages.is_empty() {
                            ui.label(
                                RichText::new("Ask for an edit in plain words.")
                                    .color(theme::MUTED_FG),
                            );
                        }
                        for message in &self.chat_messages {
                            let (who, color) = match message.role {
                                ChatRole::User => ("You", theme::FOREGROUND),
                                ChatRole::Assistant => ("OpenReel", theme::MUTED_FG),
                            };
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(who).strong().color(color));
                                ui.label(&message.text);
                            });
                        }
                    });

                if self.show_chat_debug && !self.last_generated_js.is_empty() {
                    egui::ScrollArea::vertical()
                        .id_salt("chat_debug_js")
                        .max_height(54.0)
                        .show(ui, |ui| {
                            ui.monospace(&self.last_generated_js);
                        });
                }

                ui.horizontal(|ui| {
                    let input_width = (ui.available_width() - 76.0).max(80.0);
                    let response = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.chat_input)
                                .hint_text("Describe the edit…")
                                .desired_width(input_width),
                        )
                        .on_hover_text("Describe an edit. Enter sends it. Ctrl+K focuses chat.");
                    if focus_requested {
                        response.request_focus();
                    }
                    let send_clicked = theme::primary_button(ui, "Send")
                        .on_hover_text("Send this edit request")
                        .clicked();
                    if send_clicked
                        || ((response.has_focus() || response.lost_focus())
                            && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        submitted = Some(std::mem::take(&mut self.chat_input));
                        response.request_focus();
                    }
                });
            });
        if let Some(text) = submitted {
            self.submit_chat(ctx, text);
        }
    }

    pub(super) fn chat_settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_chat_settings {
            return;
        }
        let mut open = true;
        let mut dirty = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::Grid::new("chat_settings")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("API key");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.chat_settings.api_key)
                                    .password(true)
                                    .desired_width(280.0),
                            )
                            .on_hover_text(
                                "Meta Model API key. Stored only in the local app config.",
                            )
                            .changed()
                        {
                            dirty = true;
                        }
                        ui.end_row();

                        ui.label("Base URL");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.chat_settings.base_url)
                                    .desired_width(280.0),
                            )
                            .on_hover_text("OpenAI-compatible API base URL")
                            .changed()
                        {
                            dirty = true;
                        }
                        ui.end_row();

                        ui.label("Model");
                        let before = self.chat_settings.model.clone();
                        egui::ComboBox::from_id_salt("chat_model")
                            .selected_text(model_label(&self.chat_settings.model).to_owned())
                            .show_ui(ui, |ui| {
                                for model in [MODEL_SPARK, MODEL_WONDER] {
                                    ui.selectable_value(
                                        &mut self.chat_settings.model,
                                        model.to_owned(),
                                        model_label(model),
                                    );
                                }
                            })
                            .response
                            .on_hover_text("Model used for chat editing");
                        if self.chat_settings.model != before {
                            dirty = true;
                        }
                        ui.end_row();
                    });
            });
        self.show_chat_settings = open;
        if dirty {
            self.chat_settings.save();
        }
    }
}

fn summarize_chat_result(notes: &[String], command_count: usize) -> String {
    if let Some(log) = notes.iter().rev().find(|n| !n.starts_with("error:")) {
        return log.clone();
    }
    if command_count == 0 {
        "Done. Nothing needed changing.".to_owned()
    } else if command_count == 1 {
        "Done — made 1 edit.".to_owned()
    } else {
        format!("Done — made {command_count} edits.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_code_strips_fences() {
        assert_eq!(extract_code("```js\nseek(1)\n```"), "seek(1)");
        assert_eq!(extract_code("seek(1)"), "seek(1)");
    }

    #[test]
    fn parse_generated_js_reads_openai_content() {
        let value = serde_json::json!({
            "choices": [{"message": {"content": "```javascript\nplay()\n```"}}]
        });
        assert_eq!(parse_generated_js(&value).unwrap(), "play()");
    }

    #[test]
    fn request_body_includes_correction() {
        let settings = ChatSettings::default();
        let body = build_request_body(
            &settings,
            "tighten it",
            "{}",
            &[],
            Some(("bad()", "ReferenceError")),
        );
        let user = body["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("corrected JavaScript only"));
        assert_eq!(body["model"], MODEL_SPARK);
    }
}

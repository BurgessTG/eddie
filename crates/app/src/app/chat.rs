//! Chat editing panel and Meta Model API client.
//!
//! The network call lives here in app code, on a worker thread. The
//! model returns a structured `{code, explanation}` envelope; the
//! app executes `code` through the QuickJS script engine so every
//! mutation still arrives as a `ScriptCommand`. Session memory,
//! pinned context, and compaction live in `session.rs`; this module
//! assembles each request in cache order (developer contract →
//! few-shot → pinned summary → compacted summary → recent history →
//! timeline JSON last) and keeps the prefix stable for automatic
//! prompt caching. The API key is only read from the private
//! settings file and sent as a header. It is never logged, never
//! put in status text, and never included in prompts.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, RichText};
use openreel_engine::{ClipId, ClipKind, Timeline};

use super::intents::{intent_by_id, CandidateView, IntentKind, INTENTS};
use super::media::AssetState;
use super::session::{
    build_pinned_summary, estimate_tokens, pinned_fingerprint, transcript_segment_id, ChatMessage,
    ChatRole, ChatSession, PinnedAsset, PinnedCandidate, PinnedTranscriptSegment, TOKEN_BUDGET,
};
use super::EditorApp;
use crate::theme;

pub(super) const MODEL_SPARK: &str = "muse-spark";
pub(super) const MODEL_WONDER: &str = "muse-wonder";
const DEFAULT_BASE_URL: &str = "https://api.meta.ai/v1";
/// Internal marker for a cancelled worker. Never shown to the user;
/// the cancel path clears pending state before this can surface.
const CANCELLED: &str = "__openreel_cancelled__";
/// Retry attempts for 429/500/503 (the only retried statuses).
pub(super) const MAX_MODEL_ATTEMPTS: usize = 4;

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
    /// Contributor models let Meta train on prompts/content. This is
    /// a disclosure gate only: off by default, no request changes
    /// unless a contributor model is deliberately selected later.
    pub(super) contributor_opt_in: bool,
}

impl Default for ChatSettings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: MODEL_SPARK.to_owned(),
            contributor_opt_in: false,
        }
    }
}

pub(super) struct PendingChat {
    request: String,
    attempt: u8,
    generation: u64,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<String>>,
}

impl PendingChat {
    #[cfg(test)]
    pub(super) fn for_test(request: &str, progress: &str) -> Self {
        Self {
            request: request.to_owned(),
            attempt: 0,
            generation: 1,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Mutex::new(progress.to_owned())),
        }
    }

    fn progress_label(&self) -> String {
        self.progress
            .lock()
            .map(|label| label.clone())
            .unwrap_or_else(|_| "Working…".to_owned())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GeneratedEdit {
    pub(crate) code: String,
    pub(crate) explanation: Option<String>,
}

pub(super) enum ChatWorkerResult {
    Generated(GeneratedEdit),
    Failed(String),
}

/// Timeline snapshot taken before an AI/local-intent command batch
/// is applied, so the edit is one-click undoable.
#[derive(Clone)]
pub(super) struct TimelineSnapshot {
    timeline: Timeline,
    selected: Option<ClipId>,
    playhead: f64,
}

impl TimelineSnapshot {
    pub(super) fn capture(timeline: &Timeline, selected: Option<ClipId>, playhead: f64) -> Self {
        Self {
            timeline: timeline.clone(),
            selected,
            playhead,
        }
    }

    pub(super) fn restore_into(
        &self,
        timeline: &mut Timeline,
        selected: &mut Option<ClipId>,
        playhead: &mut f64,
    ) {
        *timeline = self.timeline.clone();
        *selected = self.selected;
        *playhead = self.playhead;
    }
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
        if let Some(v) = value["contributor_opt_in"].as_bool() {
            settings.contributor_opt_in = v;
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
            "contributor_opt_in": self.contributor_opt_in,
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

/// The developer-role contract. Sent first in every request and
/// never rewritten per turn, so it anchors the cached prefix.
pub(crate) const DEVELOPER_CONTRACT: &str = "\
You edit a video timeline inside OpenReel by writing JavaScript. \
Reply with a single JSON object {\"code\": \"...\", \"explanation\": \"...\"} and nothing else. \
\"code\" is executable JavaScript only, with no markdown fences inside it, ending with exactly one console.log(\"Done — ...\") line. \
\"explanation\" is one short plain-language summary for the user. \
Available functions: state(), clips(), assets(), sweepCandidates(), transcriptSegments(), play(), pause(), toggle(), seek(t), step(frames), \
selectClip(id), split(at), splitClip(id, at), deleteClip(id?), moveClip(id, start), \
trimClip(id, left, right) where -1 keeps an edge, renameClip(id, name), setOpacity(id, v), \
setVolume(id, v), addTitle(text, at?, dur?), addAsset(nameOrPath, at?), \
addAssetRange(nameOrPath, at, sourceIn, dur), \
setMagnet(bool), setZoom(pxPerSec), closeGaps(). \
state() returns {playhead, playing, selected, duration, fps, tool, magnet, clips, assets, sweepCandidates, transcriptSegments}. \
Sweep candidate fields: id, asset, assetName, title, sourceIn, duration, score, reason, stability, audioEnergy, audioPeak, isReaction, reactionScore, hasVideo, transcript. \
Transcript segment fields: id, asset, assetName, start, end, speaker, text. Transcript timestamps are turn-level only. \
For “build the montage from the sweep”, replace the current timeline clips with the highest-ranked video sweepCandidates() (all candidates only if none has video), placed sequentially from 0 with addAssetRange(candidate.asset, cursor, candidate.sourceIn, candidate.duration). \
For “build it from the reaction shots”, use sweepCandidates() filtered to isReaction (if none, the highest reactionScore first), in the order given, placed sequentially from 0 the same way. \
For requests like “use the part where he talks about pricing”, find the transcriptSegments() whose text matches and place that source range with addAssetRange(segment.asset, cursor, segment.start, segment.end - segment.start). \
Clip fields: id, name, kind ('video'|'audio'|'title'), track, start, duration, sourceIn, source, opacity, volume. \
Tracks: 0 titles, 1 main video, 2 audio. Times are seconds. \
Refer to clips, sweep candidates, and transcript segments by the ids in state(). Do not invent ids or asset names. \
Prefer var or block scope over top-level const/let; the console persists between evals and redeclaration throws. \
Keep edits minimal and preserve source limits. If the request is impossible with the given state, \
make code throw new Error('short reason').";

/// Back-compat name for the dev harness and older prompts.
#[cfg(test)]
pub(crate) const SYSTEM_PROMPT: &str = DEVELOPER_CONTRACT;

const FEW_SHOT_USER: &str = r#"Example request:
Build it from the reaction shots.

Timeline state JSON:
{"playhead":0,"playing":false,"selected":null,"duration":0,"fps":30,"tool":"select","magnet":true,"clips":[],"assets":[{"name":"Rushes.mp4","path":"media/Rushes.mp4","duration":60,"width":1280,"height":720,"hasVideo":true,"hasAudio":true,"ready":true}],"sweepCandidates":[{"id":"media/Rushes.mp4#1","asset":"media/Rushes.mp4","assetName":"Rushes.mp4","title":"Rushes.mp4 · 0:12","sourceIn":12,"duration":4,"score":88,"reason":"Big laugh, steady frame.","stability":0.8,"audioEnergy":0.5,"audioPeak":0.9,"isReaction":true,"reactionScore":0.84,"hasVideo":true,"transcript":""}],"transcriptSegments":[]}"#;

const FEW_SHOT_ASSISTANT: &str = r#"{"code": "var shots = sweepCandidates().filter(function (c) { return c.isReaction; });\nclips().forEach(function (c) { deleteClip(c.id); });\nvar cursor = 0;\nshots.forEach(function (c) { addAssetRange(c.asset, cursor, c.sourceIn, c.duration); cursor += c.duration; });\nconsole.log(\"Done — built the cut from the reaction shots.\");", "explanation": "Built the cut from the reaction shots."}"#;

/// Final (volatile) user message: correction context if any, then
/// the request, then the timeline JSON last so the shared prefix
/// stays cacheable. The timeline is always current-state, never
/// accumulated history.
pub(crate) fn final_user_content(
    request: &str,
    state_json: &str,
    correction: Option<(&str, &str)>,
) -> String {
    let mut user = format!("Request:\n{request}\n\nTimeline state JSON:\n{state_json}");
    if let Some((failed_js, error)) = correction {
        user = format!(
            "Your previous JavaScript failed in the sandbox and made no edit.\n\
             Error:\n{error}\n\nPrevious JavaScript:\n{failed_js}\n\n\
             Reply with corrected JavaScript only, in the same JSON envelope.\n\n{user}"
        );
    }
    user
}

/// Assemble request messages in cache order. `recent` excludes the
/// in-flight request, which rides in the final message instead.
pub(crate) fn build_context_messages(
    session: &ChatSession,
    recent: &[ChatMessage],
    request: &str,
    state_json: &str,
    correction: Option<(&str, &str)>,
) -> Vec<serde_json::Value> {
    let mut messages = vec![
        serde_json::json!({"role": "developer", "content": DEVELOPER_CONTRACT}),
        serde_json::json!({"role": "user", "content": FEW_SHOT_USER}),
        serde_json::json!({"role": "assistant", "content": FEW_SHOT_ASSISTANT}),
    ];
    if !session.pinned_summary.trim().is_empty() {
        messages.push(serde_json::json!({
            "role": "user",
            "content": session.pinned_summary,
        }));
    }
    if !session.compacted_summary.trim().is_empty() {
        // History policy: old turns collapse to one assistant message.
        messages.push(serde_json::json!({
            "role": "assistant",
            "content": session.compacted_summary,
        }));
    }
    for message in recent {
        messages.push(serde_json::json!({
            "role": message.role.as_str(),
            "content": message.text,
        }));
    }
    messages.push(serde_json::json!({
        "role": "user",
        "content": final_user_content(request, state_json, correction),
    }));
    messages
}

pub(crate) fn estimate_messages_tokens(messages: &[serde_json::Value]) -> usize {
    messages
        .iter()
        .map(|message| estimate_tokens(message["content"].as_str().unwrap_or("")) + 4)
        .sum()
}

/// Compact oldest exchanges until the assembled request estimates
/// inside `TOKEN_BUDGET`, or only the verbatim tail remains. The
/// current timeline is never dropped to make room.
pub(crate) fn ensure_within_budget(
    session: &mut ChatSession,
    request: &str,
    state_json: &str,
    correction: Option<(&str, &str)>,
) {
    loop {
        let recent = recent_excluding_current(session);
        let messages = build_context_messages(session, recent, request, state_json, correction);
        if estimate_messages_tokens(&messages) <= TOKEN_BUDGET {
            return;
        }
        if !session.compact_oldest_exchange() {
            return;
        }
    }
}

/// Verbatim history minus the in-flight final user message (pushed
/// at submit time; also still last on a self-correction retry).
fn recent_excluding_current(session: &ChatSession) -> &[ChatMessage] {
    let recent = session.recent_messages();
    if recent
        .last()
        .map(|message| message.role == ChatRole::User)
        .unwrap_or(false)
    {
        &recent[..recent.len() - 1]
    } else {
        recent
    }
}

fn edit_response_format() -> serde_json::Value {
    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "openreel_edit",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "code": {"type": "string"},
                    "explanation": {"type": "string"},
                },
                "required": ["code", "explanation"],
                "additionalProperties": false,
            },
        },
    })
}

pub(super) fn build_request_body(
    settings: &ChatSettings,
    session: &ChatSession,
    recent: &[ChatMessage],
    request: &str,
    state_json: &str,
    correction: Option<(&str, &str)>,
) -> serde_json::Value {
    serde_json::json!({
        "model": settings.model,
        "temperature": 0.2,
        "stream": false,
        "prompt_cache_key": session.project_id,
        "prompt_cache_retention": "24h",
        "response_format": edit_response_format(),
        "messages": build_context_messages(session, recent, request, state_json, correction),
    })
}

/// Flatten messages for single-prompt drivers (the dev harness), so
/// the stand-in model sees the same contract and ordering.
#[cfg(test)]
pub(crate) fn render_messages_as_prompt(messages: &[serde_json::Value]) -> String {
    messages
        .iter()
        .map(|message| {
            format!(
                "{}: {}",
                message["role"].as_str().unwrap_or("user"),
                message["content"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn extract_code(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(start) = raw.find("```") {
        let after = &raw[start + 3..];
        // Drop a language-tag line (js / javascript / json) whole;
        // prefix-stripping "js" would mangle "json".
        let after = match after.find('\n') {
            Some(newline)
                if !after[..newline].trim().is_empty()
                    && after[..newline]
                        .trim()
                        .chars()
                        .all(|c| c.is_ascii_alphabetic()) =>
            {
                &after[newline + 1..]
            }
            _ => after,
        };
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

/// Plan: structured `{code, explanation}` (constrained decoding).
/// Fallback: tolerant free-text extraction for models/endpoints
/// that ignore `response_format` — never the other way round.
pub(crate) fn parse_generated_text(raw: &str) -> Result<GeneratedEdit, String> {
    let stripped = extract_code(raw);
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&stripped) {
        if let Some(code) = value["code"].as_str() {
            if !code.trim().is_empty() {
                return Ok(GeneratedEdit {
                    code: extract_code(code),
                    explanation: value["explanation"]
                        .as_str()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(str::to_owned),
                });
            }
        }
    }
    if stripped.trim().is_empty() {
        Err("The model returned an empty response.".to_owned())
    } else {
        Ok(GeneratedEdit {
            code: stripped,
            explanation: None,
        })
    }
}

pub(crate) fn parse_generated_edit(value: &serde_json::Value) -> Result<GeneratedEdit, String> {
    parse_generated_text(&chat_content(value))
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

/// Error `code` is often null on this API: branch on HTTP status
/// plus the error `type` when present, never on `code` alone.
fn api_error_for_response(status: u16, body: &str) -> String {
    let error_type = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value["error"]["type"]
                .as_str()
                .map(|kind| kind.to_lowercase())
        });
    match error_type.as_deref() {
        Some(kind)
            if kind.contains("auth") || kind.contains("api_key") || kind.contains("permission") =>
        {
            "Check your API key in Settings.".to_owned()
        }
        Some(kind) if kind.contains("rate") || kind.contains("overloaded") => {
            "The model service is busy. Try again shortly.".to_owned()
        }
        Some(kind) if kind.contains("not_found") || kind.contains("model") && status == 404 => {
            "That model was not found. Check the model in Settings.".to_owned()
        }
        _ => api_error_for_status(status),
    }
}

/// Only these statuses are retried. Everything else fails fast.
pub(super) fn should_retry(status: u16) -> bool {
    matches!(status, 429 | 500 | 503)
}

/// `Retry-After` in delta-seconds form. HTTP-date values are not
/// produced by this API in practice and fall back to backoff.
pub(super) fn parse_retry_after(raw: &str) -> Option<Duration> {
    raw.trim()
        .parse::<f64>()
        .ok()
        .filter(|s| *s > 0.0)
        .map(|s| Duration::from_secs_f64(s.min(30.0)))
}

/// 0.5–1 s initial backoff doubling per attempt, plus jitter.
/// `Retry-After` wins when the server sends it.
pub(super) fn backoff_delay(
    attempt: usize,
    retry_after: Option<Duration>,
    jitter_ms: u64,
) -> Duration {
    if let Some(delay) = retry_after {
        return delay;
    }
    let base_ms = 500_u64.saturating_mul(1 << attempt.min(5));
    Duration::from_millis(base_ms + jitter_ms.min(500))
}

fn jitter_for_attempt(attempt: usize) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    (nanos ^ (attempt as u64).wrapping_mul(0x9E37_79B9)) % 500
}

fn is_cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel
        .map(|flag| flag.load(Ordering::Relaxed))
        .unwrap_or(false)
}

/// Sleep in short slices so Cancel stays responsive during backoff.
/// Returns true if cancellation arrived.
fn sleep_cancellable(delay: Duration, cancel: Option<&AtomicBool>) -> bool {
    let mut remaining = delay;
    while remaining > Duration::ZERO {
        if is_cancelled(cancel) {
            return true;
        }
        let slice = remaining.min(Duration::from_millis(50));
        thread::sleep(slice);
        remaining -= slice;
    }
    is_cancelled(cancel)
}

fn set_progress(progress: Option<&Mutex<String>>, label: &str) {
    if let Some(progress) = progress {
        if let Ok(mut current) = progress.lock() {
            *current = label.to_owned();
        }
    }
}

fn progress_for_request(request: &str) -> &'static str {
    let lower = request.to_lowercase();
    if ["sweep", "montage", "rank", "reaction", "clip"]
        .iter()
        .any(|word| lower.contains(word))
    {
        "Ranking clips…"
    } else {
        "Writing the edit…"
    }
}

fn chat_completions_url(settings: &ChatSettings) -> String {
    let base = settings.base_url.trim().trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_owned()
    } else {
        format!("{base}/chat/completions")
    }
}

/// One attempt. Returns (status, Retry-After, body text); transport
/// failures are plain errors and are not retried.
fn post_once(
    agent: &ureq::Agent,
    settings: &ChatSettings,
    body: &serde_json::Value,
) -> Result<(u16, Option<Duration>, String), String> {
    let url = chat_completions_url(settings);
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
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after);
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|_| "Couldn't read the model response.".to_owned())?;
    Ok((status, retry_after, text))
}

pub(super) fn post_chat_completion(
    settings: &ChatSettings,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    post_chat_completion_cancellable(settings, body, None, None)
}

/// Retrying chat-completion POST. There is no API-side
/// idempotency; retries only re-ask for generation. Application
/// stays safe because a generated script is checked once and
/// applied once as a single command batch (with an undo snapshot).
pub(super) fn post_chat_completion_cancellable(
    settings: &ChatSettings,
    body: &serde_json::Value,
    cancel: Option<&AtomicBool>,
    progress: Option<&Mutex<String>>,
) -> Result<serde_json::Value, String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut attempt = 0_usize;
    loop {
        if is_cancelled(cancel) {
            return Err(CANCELLED.to_owned());
        }
        let (status, retry_after, text) = post_once(&agent, settings, body)?;
        if (200..300).contains(&status) {
            return serde_json::from_str::<serde_json::Value>(&text)
                .map_err(|_| "Couldn't read the model response.".to_owned());
        }
        if should_retry(status) && attempt + 1 < MAX_MODEL_ATTEMPTS {
            set_progress(progress, "Service busy — retrying…");
            let delay = backoff_delay(attempt, retry_after, jitter_for_attempt(attempt));
            if sleep_cancellable(delay, cancel) {
                return Err(CANCELLED.to_owned());
            }
            attempt += 1;
            continue;
        }
        return Err(api_error_for_response(status, &text));
    }
}

fn request_generated_edit(
    settings: &ChatSettings,
    body: &serde_json::Value,
    cancel: &AtomicBool,
    progress: &Mutex<String>,
) -> Result<GeneratedEdit, String> {
    let value = post_chat_completion_cancellable(settings, body, Some(cancel), Some(progress))?;
    parse_generated_edit(&value)
}

impl EditorApp {
    fn save_session(&self) {
        self.chat_session.save();
    }

    fn push_chat(&mut self, role: ChatRole, text: String) {
        self.chat_session.push(role, text);
        self.save_session();
    }

    /// Rebuild the pinned summary only when the sweep/asset/
    /// transcript inputs actually moved (fingerprint check), so
    /// the cached request prefix stays stable across turns.
    pub(super) fn refresh_pinned_summary(&mut self) {
        // Owned transcript excerpts/ids outlive the borrowed views.
        let excerpt_by_candidate: Vec<String> = self
            .assets
            .iter()
            .flat_map(|asset| asset.sweep.candidates.iter())
            .filter(|candidate| !candidate.dismissed)
            .map(|candidate| candidate.transcript_text())
            .collect();
        let segment_ids: Vec<String> = self
            .assets
            .iter()
            .flat_map(|asset| {
                asset
                    .sweep
                    .transcript
                    .iter()
                    .enumerate()
                    .map(|(index, _)| transcript_segment_id(&asset.info.path, index))
                    .collect::<Vec<_>>()
            })
            .collect();
        let assets: Vec<PinnedAsset<'_>> = self
            .assets
            .iter()
            .map(|asset| PinnedAsset {
                name: &asset.info.name,
                path: &asset.info.path,
                duration: asset.info.duration,
                has_video: asset.info.has_video,
                has_audio: asset.info.has_audio,
                ready: asset.state == AssetState::Ready,
            })
            .collect();
        let mut sorted: Vec<&super::sweep::SweepCandidate> = self
            .assets
            .iter()
            .flat_map(|asset| asset.sweep.candidates.iter())
            .filter(|candidate| !candidate.dismissed)
            .collect();
        // Excerpts were collected in asset order; candidates are
        // presented best-first, so pair by id instead of position.
        let excerpt_for = |id: &str| -> &str {
            self.assets
                .iter()
                .flat_map(|asset| asset.sweep.candidates.iter())
                .filter(|candidate| !candidate.dismissed)
                .position(|candidate| candidate.id == id)
                .map(|index| excerpt_by_candidate[index].as_str())
                .unwrap_or("")
        };
        sorted.sort_by(|a, b| b.score.total_cmp(&a.score));
        let candidates: Vec<PinnedCandidate<'_>> = sorted
            .iter()
            .map(|candidate| PinnedCandidate {
                id: &candidate.id,
                asset_name: &candidate.asset_name,
                source_in: candidate.source_in,
                duration: candidate.duration,
                score: candidate.score,
                stability: candidate.stability,
                audio_energy: candidate.audio_energy,
                audio_peak: candidate.audio_peak,
                is_reaction: candidate.is_reaction(),
                reason: &candidate.reason,
                transcript: excerpt_for(&candidate.id),
            })
            .collect();
        let mut segment_index = 0_usize;
        let mut segments: Vec<PinnedTranscriptSegment<'_>> = Vec::new();
        for asset in &self.assets {
            for segment in &asset.sweep.transcript {
                segments.push(PinnedTranscriptSegment {
                    id: &segment_ids[segment_index],
                    asset_name: &asset.info.name,
                    start: segment.start,
                    end: segment.end,
                    speaker: segment.speaker.as_deref(),
                    text: &segment.text,
                });
                segment_index += 1;
            }
        }
        let fingerprint = pinned_fingerprint(&assets, &candidates, &segments);
        if fingerprint != self.chat_session.pinned_fingerprint {
            self.chat_session.pinned_summary =
                build_pinned_summary(&assets, &candidates, &segments);
            self.chat_session.pinned_fingerprint = fingerprint;
            self.save_session();
        }
    }

    fn snapshot_for_undo(&mut self) {
        self.chat_undo = Some(TimelineSnapshot::capture(
            &self.timeline,
            self.selected,
            self.playhead,
        ));
    }

    fn undo_chat_edit(&mut self) {
        let Some(snapshot) = self.chat_undo.take() else {
            return;
        };
        self.stop_sources();
        self.playing = false;
        snapshot.restore_into(&mut self.timeline, &mut self.selected, &mut self.playhead);
        self.still_key = None;
        self.still_cache.clear();
        let message = "Undid the last edit.".to_owned();
        self.push_chat(ChatRole::Assistant, message.clone());
        self.status = message;
    }

    fn cancel_chat(&mut self) {
        let Some(pending) = self.chat_pending.take() else {
            return;
        };
        pending.cancel.store(true, Ordering::Relaxed);
        // Dropping the receiver leaves the worker's send undelivered;
        // the retry loop also observes the cancel flag directly.
        self.chat_rx = None;
        let message = "Cancelled.".to_owned();
        self.push_chat(ChatRole::Assistant, message.clone());
        self.status = message;
    }

    fn spawn_chat_request(&mut self, ctx: &egui::Context, correction: Option<(String, String)>) {
        self.refresh_pinned_summary();
        let state = self.script_state_json();
        let Some(pending) = self.chat_pending.as_ref() else {
            return;
        };
        let request = pending.request.clone();
        let generation = pending.generation;
        let cancel = pending.cancel.clone();
        let progress = pending.progress.clone();
        let settings = self.chat_settings.clone();
        let correction_ref = correction
            .as_ref()
            .map(|(js, err)| (js.as_str(), err.as_str()));
        ensure_within_budget(&mut self.chat_session, &request, &state, correction_ref);
        self.save_session();
        let recent: Vec<ChatMessage> = recent_excluding_current(&self.chat_session).to_vec();
        let body = build_request_body(
            &settings,
            &self.chat_session,
            &recent,
            &request,
            &state,
            correction_ref,
        );
        let (tx, rx) = channel::<(u64, ChatWorkerResult)>();
        self.chat_rx = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = match request_generated_edit(&settings, &body, &cancel, &progress) {
                Ok(edit) => ChatWorkerResult::Generated(edit),
                Err(message) => ChatWorkerResult::Failed(message),
            };
            let _ = tx.send((generation, result));
            ctx.request_repaint();
        });
    }

    fn submit_chat(&mut self, ctx: &egui::Context, text: String) {
        let text = text.trim().to_owned();
        let progress = progress_for_request(&text);
        self.submit_chat_request(ctx, text.clone(), text, progress);
    }

    /// `display` is what the user sees as their turn (a button
    /// label for model intents); `request` is what the model gets.
    fn submit_chat_request(
        &mut self,
        ctx: &egui::Context,
        display: String,
        request: String,
        progress: &'static str,
    ) {
        if display.trim().is_empty() || self.chat_pending.is_some() {
            return;
        }
        self.push_chat(ChatRole::User, display);
        if self.chat_settings.api_key.trim().is_empty() && !self.chat_settings.allows_missing_key()
        {
            self.show_chat_settings = true;
            let message = "Add your Meta Model API key in Settings first.".to_owned();
            self.push_chat(ChatRole::Assistant, message.clone());
            self.status = message;
            return;
        }
        self.chat_generation += 1;
        self.chat_pending = Some(PendingChat {
            request,
            attempt: 0,
            generation: self.chat_generation,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Mutex::new(progress.to_owned())),
        });
        self.spawn_chat_request(ctx, None);
    }

    /// One registry entry point for every button. Local intents
    /// never touch the network; model intents reuse `submit_chat`.
    fn dispatch_intent(&mut self, ctx: &egui::Context, intent_id: &str) {
        let Some(intent) = intent_by_id(intent_id) else {
            return;
        };
        match intent.kind {
            IntentKind::Model => {
                if let Some(prompt) = intent.prompt {
                    self.submit_chat_request(
                        ctx,
                        intent.label.to_owned(),
                        prompt.to_owned(),
                        intent.progress,
                    );
                }
            }
            IntentKind::Local(sort) => {
                if self.chat_pending.is_some() {
                    return;
                }
                let ordered = super::intents::ordered_candidates(
                    &self
                        .assets
                        .iter()
                        .flat_map(|asset| asset.sweep.candidates.iter())
                        .filter(|candidate| !candidate.dismissed)
                        .map(CandidateView::from)
                        .collect::<Vec<_>>(),
                    sort,
                );
                self.push_chat(ChatRole::User, intent.label.to_owned());
                if ordered.is_empty() {
                    let message = "No sweep clips yet. Import media first.".to_owned();
                    self.push_chat(ChatRole::Assistant, message.clone());
                    self.status = message;
                    return;
                }
                // Arrange replaces the main video track's clips;
                // titles and audio stay put.
                let existing: Vec<u64> = self
                    .timeline
                    .clips
                    .iter()
                    .filter(|clip| clip.kind == ClipKind::Video)
                    .map(|clip| clip.id)
                    .collect();
                let commands = super::intents::local_commands(&existing, &ordered);
                self.snapshot_for_undo();
                let notes = self.apply_script_commands(ctx, commands);
                let message = summarize_chat_result(&notes, ordered.len());
                self.push_chat(ChatRole::Assistant, message.clone());
                self.status = message;
            }
        }
    }

    pub(super) fn poll_chat(&mut self, ctx: &egui::Context) {
        let Some((generation, result)) = self.chat_rx.as_ref().and_then(|rx| rx.try_recv().ok())
        else {
            if self.chat_pending.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
            return;
        };
        // A stale result (cancelled or superseded generation) made
        // no edit and gets no message.
        let current = self
            .chat_pending
            .as_ref()
            .map(|pending| pending.generation == generation)
            .unwrap_or(false);
        if !current {
            self.chat_rx = None;
            return;
        }
        self.chat_rx = None;
        match result {
            ChatWorkerResult::Failed(message) => {
                self.chat_pending = None;
                if message == CANCELLED {
                    let message = "Cancelled.".to_owned();
                    self.push_chat(ChatRole::Assistant, message.clone());
                    self.status = message;
                    return;
                }
                self.push_chat(ChatRole::Assistant, message.clone());
                self.status = message;
            }
            ChatWorkerResult::Generated(edit) => {
                let js = edit.code;
                self.last_generated_js = js.clone();
                let state = self.script_state_json();
                let Some(engine) = self.script_engine.as_mut() else {
                    self.chat_pending = None;
                    let message = "I couldn't do that one — scripting is unavailable.".to_owned();
                    self.push_chat(ChatRole::Assistant, message.clone());
                    self.status = message;
                    return;
                };
                match engine.eval_checked(&js, &state) {
                    Ok((_, commands)) => {
                        let command_count = commands.len();
                        if command_count > 0 {
                            self.snapshot_for_undo();
                        }
                        let notes = self.apply_script_commands(ctx, commands);
                        let errors: Vec<String> = notes
                            .iter()
                            .filter(|n| n.starts_with("error:"))
                            .cloned()
                            .collect();
                        self.chat_pending = None;
                        let message = if !errors.is_empty() {
                            "I couldn't finish that one — try naming the clip.".to_owned()
                        } else if let Some(explanation) =
                            edit.explanation.filter(|text| !text.trim().is_empty())
                        {
                            explanation
                        } else {
                            summarize_chat_result(&notes, command_count)
                        };
                        self.push_chat(ChatRole::Assistant, message.clone());
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
                                if let Ok(mut label) = pending.progress.lock() {
                                    *label = "Writing the edit…".to_owned();
                                }
                            }
                            self.spawn_chat_request(ctx, Some((js, error)));
                        } else {
                            self.chat_pending = None;
                            let message =
                                "I couldn't do that one — try naming the clip.".to_owned();
                            self.push_chat(ChatRole::Assistant, message.clone());
                            self.status = message;
                        }
                    }
                }
            }
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
        let mut clicked_intent: Option<&'static str> = None;
        let mut cancel_clicked = false;
        let mut undo_clicked = false;
        egui::TopBottomPanel::bottom("chat")
            .resizable(true)
            .default_height(208.0)
            .height_range(150.0..=420.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    theme::panel_title(ui, "CHAT");
                    if let Some(pending) = &self.chat_pending {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label(RichText::new(pending.progress_label()).color(theme::MUTED_FG));
                        if theme::ghost_button(ui, "Cancel")
                            .on_hover_text("Cancel the in-flight model call")
                            .clicked()
                        {
                            cancel_clicked = true;
                        }
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
                        if self.chat_undo.is_some()
                            && theme::ghost_button(ui, "Undo edit")
                                .on_hover_text("Restore the timeline from before the last AI edit")
                                .clicked()
                        {
                            undo_clicked = true;
                        }
                    });
                });
                ui.add_space(4.0);

                // Button intents: one registry, local first. New
                // buttons are declared in `intents.rs`.
                ui.horizontal_wrapped(|ui| {
                    for intent in INTENTS {
                        let response = match intent.kind {
                            IntentKind::Local(_) => theme::outline_button(ui, intent.label),
                            IntentKind::Model => theme::ghost_button(ui, intent.label),
                        };
                        if response.on_hover_text(intent.tooltip).clicked() {
                            clicked_intent = Some(intent.id);
                        }
                    }
                });
                ui.add_space(4.0);

                let debug_height = if self.show_chat_debug { 58.0 } else { 0.0 };
                let messages_height = (ui.available_height() - 74.0 - debug_height).max(28.0);
                egui::ScrollArea::vertical()
                    .id_salt("chat_messages")
                    .max_height(messages_height)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if self.chat_session.messages.is_empty() {
                            ui.label(
                                RichText::new("Ask for an edit in plain words.")
                                    .color(theme::MUTED_FG),
                            );
                        }
                        for message in &self.chat_session.messages {
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
        if cancel_clicked {
            self.cancel_chat();
        }
        if undo_clicked {
            self.undo_chat_edit();
        }
        if let Some(intent_id) = clicked_intent {
            self.dispatch_intent(ctx, intent_id);
        }
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

                        ui.label("Contributor");
                        let before = self.chat_settings.contributor_opt_in;
                        ui.checkbox(
                            &mut self.chat_settings.contributor_opt_in,
                            "Contributor models",
                        )
                        .on_hover_text(
                            "Contributor models let Meta train on your prompts and content. Standard models do not.",
                        );
                        if self.chat_settings.contributor_opt_in != before {
                            dirty = true;
                        }
                        ui.end_row();
                    });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Contributor models let Meta train on prompts and content. Off by default; Standard is used otherwise.",
                    )
                    .size(11.5)
                    .color(theme::MUTED_FG),
                );
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
    use super::super::session::RECENT_MESSAGES_KEPT_VERBATIM;
    use super::*;

    #[test]
    fn extract_code_strips_fences() {
        assert_eq!(extract_code("```js\nseek(1)\n```"), "seek(1)");
        assert_eq!(extract_code("seek(1)"), "seek(1)");
    }

    #[test]
    fn parse_generated_edit_reads_openai_content() {
        let value = serde_json::json!({
            "choices": [{"message": {"content": "```javascript\nplay()\n```"}}]
        });
        assert_eq!(parse_generated_edit(&value).unwrap().code, "play()");
    }

    #[test]
    fn structured_envelope_is_the_plan_free_text_the_fallback() {
        let structured = serde_json::json!({
            "choices": [{"message": {"content": "{\"code\": \"seek(2)\", \"explanation\": \"Moved to 2 seconds.\"}"}}]
        });
        let edit = parse_generated_edit(&structured).unwrap();
        assert_eq!(edit.code, "seek(2)");
        assert_eq!(edit.explanation.as_deref(), Some("Moved to 2 seconds."));
        // Fallback: bare JS still parses with no explanation.
        let bare = parse_generated_text("seek(3)").unwrap();
        assert_eq!(bare.code, "seek(3)");
        assert_eq!(bare.explanation, None);
        assert!(parse_generated_text("").is_err());
        // JSON inside fences unwraps once.
        let fenced = parse_generated_text("```json\n{\"code\":\"play()\"}\n```").unwrap();
        assert_eq!(fenced.code, "play()");
    }

    #[test]
    fn request_body_orders_cache_prefix_and_puts_timeline_last() {
        let settings = ChatSettings::default();
        let mut session = ChatSession::new("proj-cache-1");
        session.pinned_summary = "Project summary (pinned):\nAssets:\n- a.mp4".to_owned();
        session.compacted_summary = "Summary of earlier conversation: stuff".to_owned();
        let recent = vec![
            ChatMessage {
                role: ChatRole::User,
                text: "earlier ask".to_owned(),
            },
            ChatMessage {
                role: ChatRole::Assistant,
                text: "earlier done".to_owned(),
            },
        ];
        let body = build_request_body(
            &settings,
            &session,
            &recent,
            "tighten it",
            "{\"clips\":[]}",
            Some(("bad()", "ReferenceError")),
        );
        assert_eq!(body["model"], MODEL_SPARK);
        assert_eq!(body["prompt_cache_key"], "proj-cache-1");
        assert_eq!(body["prompt_cache_retention"], "24h");
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(
            body["response_format"]["json_schema"]["name"],
            "openreel_edit"
        );
        let messages = body["messages"].as_array().unwrap();
        let roles: Vec<&str> = messages
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            roles,
            vec![
                "developer", // contract
                "user",      // few-shot
                "assistant", // few-shot answer
                "user",      // pinned summary
                "assistant", // compacted summary (single message)
                "user",      // recent verbatim
                "assistant", // recent verbatim
                "user",      // final: request + timeline last
            ]
        );
        assert_eq!(messages[0]["content"], DEVELOPER_CONTRACT);
        let final_body = messages.last().unwrap()["content"].as_str().unwrap();
        assert!(
            final_body.contains("corrected JavaScript only"),
            "{final_body}"
        );
        let request_at = final_body.find("Request:\ntighten it").unwrap();
        let timeline_at = final_body.find("Timeline state JSON:").unwrap();
        assert!(request_at < timeline_at, "timeline JSON must be last");
        assert!(final_body.trim_end().ends_with("{\"clips\":[]}"));
    }

    #[test]
    fn retry_policy_only_retries_429_500_503_with_doubling_backoff() {
        for status in [429, 500, 503] {
            assert!(should_retry(status), "{status}");
        }
        for status in [200, 400, 401, 403, 404, 501, 502, 504] {
            assert!(!should_retry(status), "{status}");
        }
        assert_eq!(backoff_delay(0, None, 0), Duration::from_millis(500));
        assert_eq!(backoff_delay(1, None, 0), Duration::from_millis(1000));
        assert_eq!(backoff_delay(2, None, 100), Duration::from_millis(2100));
        // Retry-After is honoured over the computed backoff.
        assert_eq!(
            backoff_delay(0, Some(Duration::from_secs(7)), 0),
            Duration::from_secs(7)
        );
        assert_eq!(parse_retry_after("3"), Some(Duration::from_secs(3)));
        assert_eq!(parse_retry_after("soon"), None);
        assert_eq!(parse_retry_after("0"), None);
        assert!((3..=5).contains(&MAX_MODEL_ATTEMPTS));
        // Errors branch on type+status, with code often null.
        assert_eq!(
            api_error_for_response(
                400,
                "{\"error\":{\"type\":\"authentication_error\",\"code\":null}}"
            ),
            "Check your API key in Settings."
        );
        assert_eq!(
            api_error_for_response(429, "{\"error\":{\"type\":null,\"code\":null}}"),
            "The model service is busy. Try again shortly."
        );
    }

    #[test]
    fn thirty_turn_session_compacts_inside_token_budget() {
        let mut session = ChatSession::new("proj-long");
        session.pinned_summary = "Project summary (pinned): one asset.".to_owned();
        for turn in 0..30 {
            session.push(
                ChatRole::User,
                format!(
                    "turn {turn}: please adjust the edit in considerable detail {}",
                    " padding to make this turn estimate like a real long request".repeat(1600)
                ),
            );
            session.push(
                ChatRole::Assistant,
                format!(
                    "turn {turn}: done, here is what changed in reasonable detail {}",
                    " with extra detail so the history grows past the budget".repeat(1600)
                ),
            );
        }
        // In-flight 31st request, pushed exactly as submit does.
        session.push(ChatRole::User, "turn 30: one more thing".to_owned());
        let state = "{\"clips\":[],\"assets\":[],\"sweepCandidates\":[],\"transcriptSegments\":[]}";
        let before = estimate_messages_tokens(&build_context_messages(
            &session,
            recent_excluding_current(&session),
            "turn 30: one more thing",
            state,
            None,
        ));
        assert!(
            before > TOKEN_BUDGET,
            "fixture must start over budget ({before})"
        );
        ensure_within_budget(&mut session, "turn 30: one more thing", state, None);
        let messages = build_context_messages(
            &session,
            recent_excluding_current(&session),
            "turn 30: one more thing",
            state,
            None,
        );
        let after = estimate_messages_tokens(&messages);
        assert!(after <= TOKEN_BUDGET, "must end inside budget ({after})");
        assert!(
            session.compacted_through > 0,
            "early turns must be summarized"
        );
        // Early turns are summarized, not resent raw; the newest stay verbatim.
        let payload = render_messages_as_prompt(&messages);
        assert!(payload.contains("turns compacted"), "{payload}");
        assert!(
            session.recent_messages().len() > RECENT_MESSAGES_KEPT_VERBATIM
                && session.recent_messages().len() < 61,
            "compacts only as far as the budget requires (recent={})",
            session.recent_messages().len()
        );
        let recent_text: Vec<&str> = session
            .recent_messages()
            .iter()
            .map(|m| m.text.as_str())
            .collect();
        assert!(recent_text.iter().any(|t| t.starts_with("turn 29:")));
        assert!(!recent_text.iter().any(|t| t.starts_with("turn 0:")));
        // Full UI history survives compaction untouched.
        assert_eq!(session.messages.len(), 61);
        // Timeline JSON is still last after compaction.
        let final_body = messages.last().unwrap()["content"].as_str().unwrap();
        assert!(final_body.trim_end().ends_with(state));
    }

    #[test]
    fn undo_snapshot_restores_timeline_selection_and_playhead() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_clip("a.mp4", ClipKind::Video, 1, 0.0, 5.0, 0.0);
        let snapshot = TimelineSnapshot::capture(&timeline, Some(id), 2.5);
        timeline.remove_clip(id);
        let other = timeline.add_clip("b.mp4", ClipKind::Video, 1, 9.0, 3.0, 0.0);
        let (mut selected, mut playhead) = (Some(other), 9.0);
        snapshot.restore_into(&mut timeline, &mut selected, &mut playhead);
        assert_eq!(timeline.clips.len(), 1);
        assert_eq!(timeline.clips[0].id, id);
        assert_eq!(selected, Some(id));
        assert_eq!(playhead, 2.5);
    }
}

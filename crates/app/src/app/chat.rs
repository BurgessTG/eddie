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

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use openreel_engine::{ClipId, ClipKind, Orientation, Timeline};

use super::intents::{intent_by_id, CandidateView, IntentKind, INTENTS};
use super::media::AssetState;
use super::session::{
    build_pinned_summary, estimate_tokens, pinned_fingerprint, transcript_segment_id, ChatMessage,
    ChatRole, ChatSession, PinnedAsset, PinnedCandidate, PinnedTranscriptSegment, TOKEN_BUDGET,
};
use super::EditorApp;
use crate::scripting::ScriptCommand;
use crate::theme;

pub(super) const MODEL_SPARK: &str = "muse-spark-1.3";
pub(super) const MODEL_WONDER: &str = "muse-wonder-1.0";
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

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ChatSettings {
    pub(super) api_key: String,
    pub(super) base_url: String,
    pub(super) model: String,
    /// Contributor models let Meta train on prompts/content. This is
    /// a disclosure gate only: off by default, no request changes
    /// unless a contributor model is deliberately selected later.
    pub(super) contributor_opt_in: bool,
    /// Picture format for new and cleared projects; Settings also
    /// applies it to the open project.
    pub(super) format: Orientation,
}

impl Default for ChatSettings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: MODEL_WONDER.to_owned(),
            contributor_opt_in: false,
            format: Orientation::Landscape,
        }
    }
}

pub(super) struct PendingConnectionCheck {
    settings: ChatSettings,
    rx: Receiver<Result<(), String>>,
}

pub(super) struct PendingChat {
    request: String,
    timeline: Timeline,
    state_json: String,
    attempt: u8,
    generation: u64,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<String>>,
}

impl PendingChat {
    #[cfg(test)]
    pub(super) fn for_test(
        request: &str,
        progress: &str,
        timeline: &Timeline,
        state_json: String,
    ) -> Self {
        Self {
            request: request.to_owned(),
            timeline: timeline.clone(),
            state_json,
            // A fixture has no HTTP worker: never send a correction to a
            // real provider if the supplied JavaScript fails evaluation.
            attempt: 1,
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

pub(super) fn config_path() -> Option<PathBuf> {
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
                settings.model = match v {
                    "muse-spark" => MODEL_SPARK,
                    "muse-wonder" => MODEL_WONDER,
                    other => other,
                }
                .to_owned();
            }
        }
        if let Some(v) = value["contributor_opt_in"].as_bool() {
            settings.contributor_opt_in = v;
        }
        if let Some(v) = value["format"].as_str().and_then(Orientation::from_name) {
            settings.format = v;
        }
        settings
    }

    pub(super) fn save(&self) -> Result<(), String> {
        let save_error = || "Couldn't save settings.".to_owned();
        let path = config_path().ok_or_else(save_error)?;
        let parent = path.parent().ok_or_else(save_error)?;
        std::fs::create_dir_all(parent).map_err(|_| save_error())?;
        let body = serde_json::json!({
            "api_key": self.api_key,
            "base_url": self.base_url,
            "model": self.model,
            "contributor_opt_in": self.contributor_opt_in,
            "format": self.format.as_str(),
        });
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        let (temporary, mut file) = loop {
            let temporary = parent.join(format!(
                ".config-{}-{}.tmp",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
            ));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&temporary) {
                Ok(file) => break (temporary, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(save_error()),
            }
        };
        let result = (|| -> std::io::Result<()> {
            file.write_all(body.to_string().as_bytes())?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result.map_err(|_| save_error())
    }

    /// Every setting except the project format: a changed one
    /// discards a pending connection result.
    fn connection(&self) -> (&str, &str, &str, bool) {
        (
            &self.api_key,
            &self.base_url,
            &self.model,
            self.contributor_opt_in,
        )
    }

    pub(super) fn can_call_model(&self) -> bool {
        !self.api_key.trim().is_empty() || self.allows_missing_key()
    }

    fn allows_missing_key(&self) -> bool {
        let Ok(uri) = self.base_url.trim().parse::<ureq::http::Uri>() else {
            return false;
        };
        let Some(host) = uri.host() else {
            return false;
        };
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false)
    }
}

/// The developer-role contract. Sent first in every request and
/// never rewritten per turn, so it anchors the cached prefix.
pub(crate) const DEVELOPER_CONTRACT: &str = "\
You edit a video timeline inside Eddie by writing JavaScript. \
Reply with a single JSON object {\"code\": \"...\", \"explanation\": \"...\"} and nothing else. \
\"code\" is executable JavaScript only, with no markdown fences inside it, ending with exactly one console.log(\"Done — ...\") line. \
\"explanation\" is one short plain-language summary for the user. \
If you make no edits because you need the user's decision or cannot do part of the request, the code makes no edits and console.logs each problem or question as its own plain sentence before the Done line; the user sees those lines. \
Available functions: state(), clips(), assets(), sweepCandidates(), sceneClips(), transcriptSegments(), play(), pause(), toggle(), seek(t), step(frames), \
selectClip(id), split(at), splitClip(id, at), deleteClip(id?), moveClip(id, start), setClipLane(id, lane), \
trimClip(id, left, right) where -1 keeps an edge, renameClip(id, name), setOpacity(id, v), \
setVolume(id, v), setFade(id, inSeconds, outSeconds) (linear audio fades; 0 removes one), detachAudio(id), unlinkClip(id), setEmbeddedAudio(id, bool), tightenSilence(id, threshold?, minSilence?, padding?), addTitle(text, at?, dur?, style?), setTitleStyle(id, style), addCaptionsForClip(id, style?), addAsset(nameOrPath, at?), \
addAssetRange(nameOrPath, at, sourceIn, dur), \
setMagnet(bool), setMusicBedMuted(bool), setZoom(pxPerSec), closeGaps(). \
closeGaps() runs when it is reached in the queue. It removes only time that no video or audio clip covers on any lane (titles do not count) and shifts later clips on every lane left by the same amount, so music still spanning a gap keeps it open. To close a picture gap under longer music, shorten the music first or move the later clips yourself with moveClip. \
state() returns {playhead, playing, selected, duration, fps, tool, magnet, musicBedMuted, format, clips, assets, sweepCandidates, sceneClips, transcriptSegments}. \
format is the project orientation; each asset has an orientation, and video whose orientation differs from format cannot be added, so never use it. \
Sweep candidates contain all curated workable ranges, not a top-N selection; they can include multiple useful moments inside one long shot. sceneClips are the complete contiguous physical-shot inventory, including rejected material, in source order. To deconstruct or rebuild an edited video shot by shot, use sceneClips grouped by asset and sorted by sourceIn. Use sweepCandidates for editorial ranges, optionally filtered by category or quality. Both inventories have no clip-count cap. Fields: id, asset, assetName, title, sourceIn, duration, score, reason, category, tags, musicBed, quality {stability, audio, motion}, stability, audioEnergy, audioPeak, isReaction, reactionScore, hasVideo, transcript, talkingScore, visual, review. review.status is local, reviewed, needs-review or rejected. Local ranges are measured suggestions, not semantic review; needs-review means missing or failed review, not unusable footage. Model review uses sampled images and transcript, not exhaustive frame understanding; its score is an editorial judgment. \
This request carries compact records without repeated candidate transcripts, low-level visual measurements or review frame arrays. transcriptSegments contains speech once. Sandbox queries still return the complete request-time snapshot, including those omitted fields. review.subjectEvidence summarizes source-timed SAM checks used by the visual curator before its final decision: concept, status, checked range, sampled/observed frames and first/last observations. Tracking is sampled, detection gaps are uncertain, IDs are request-local, and boxes are not masks, emotion or proof of lip sync. visual includes representative box/face measurements at samFrameTime. SAM can check objects besides people; use the concept, not a box alone, to interpret the evidence. Local subject-centered, subject-large and subject-moving tags are measured proxies; face-visible requires SAM face evidence. \
category is one of reaction, talking, action, broll, establishing. Ranges tagged on-screen-text contain burned-in titles, captions, news bars or graphics from an earlier edit; never place them unless the user explicitly asks for that text. Small logos filmed in the scene are not tagged and are fine. prime is a scarce quality tag, not a category. musicBed true marks suspected baked-in montage music: judge it on visuals, not soundtrack peaks. When state().musicBedMuted is true the app adds such video clips muted so old soundtracks never stack (audio-only music tracks always stay audible); call setMusicBedMuted(false) only when the user explicitly wants the baked-in score kept, and setMusicBedMuted(true) when building a new soundtrack. \
Transcript segment fields: id, asset, assetName, start, end, speaker, text, timingPrecision ('turn'|'word'), words [{start,end,text,confidence}], asrSession {id,sourceStart,sourceEnd}. Speaker labels are scoped to that source/session, not identities across uploads. Full sandbox review.speech records identify retained fragments of source turns; completeAvailableTurn means the reported turn is retained, not verified sentence completion. Only timingPrecision 'word' supports exact word/filler boundaries; turn timings locate the spoken passage, not each word. \
Asset fields include waveformPeaksPerSec and silenceRanges [{start,end}] from local waveform DSP. Audio-only music assets with a steady beat also have tempo (BPM) and beats (source seconds, null when none was found); to cut to the beat, place each music-relative cut on a beat. tightenSilence(id) removes pauses at least 0.35s from a speech clip, keeps 0.08s around phrases, ripples later clips left, rebuilds linked detached audio in matching phrases, and rejects mostly music-bed clips. Waveforms locate silence only; use transcript text/meaning for filler-word decisions, and do not claim exact filler boundaries from turn timestamps. \
For “build the montage from the sweep”, replace the current timeline clips with the highest-ranked video sweepCandidates() (all candidates only if none has video), placed sequentially from 0 with addAssetRange(candidate.asset, cursor, candidate.sourceIn, candidate.duration). \
For “build it from the reaction shots”, use sweepCandidates() filtered to isReaction (if none, the highest reactionScore first), in the order given, placed sequentially from 0 the same way. \
For requests like “use the part where he talks about pricing”, find the transcriptSegments() whose text matches and place that source range with addAssetRange(segment.asset, cursor, segment.start, segment.end - segment.start). \
Clip fields: id, name, kind ('video'|'audio'|'title'), track, start, duration, sourceIn, source, opacity, volume, embeddedAudio, linkedClip, titleStyle ('lower-third'|'caption'|'pop-caption'|'minimal-caption'). To caption an already placed media clip in the snapshot, call addCaptionsForClip(id, style?); it uses word times when available and turn-proportional estimates otherwise, and skips exact duplicates. Queries read the request-time snapshot and edits are queued, so newly placed clips do not appear in clips() during this run. addAsset, addAssetRange and addTitle return a handle for the clip they create; pass it as the id to later calls in the same code (setFade, moveClip, trimClip, addCaptionsForClip and so on). Never compute or guess clip IDs. embeddedAudio false means a video's own sound is off; detachAudio(id) creates a linked audio clip and turns the picture's embedded sound off so it never doubles. Linked picture/audio pairs move, trim, and split together; call unlinkClip(id) before offsetting one side on purpose (J/L cuts, replacing dialogue, or laying sound under another shot). \
Lanes (track indexes) are kind-agnostic and grow as needed; add functions choose a free lane automatically. Where video clips overlap, the lowest lane index is the picture shown (lane 0 wins), and a lower-opacity winner fades to black, never revealing another lane: there is no picture-in-picture or video overlay. To cut away from a long base clip (an interview, a monologue) and keep its sound, first setClipLane(baseId, 1), then add the cutaway, then setClipLane(cutawayHandle, 0) and setEmbeddedAudio(cutawayHandle, false) if its own sound is unwanted. Times are seconds. \
For a recap or mashup across several videos or places: group sweepCandidates() by asset (assetName often carries the place or event), open each group with an establishing or prime-tagged candidate, then vary action, talking, and reaction picks, addTitle a short place card when the asset changes, and keep each source range within its candidate duration. \
Refer to clips, sweep candidates, and transcript segments by the ids in state(). Do not invent ids or asset names. \
Top-level const/let are isolated per checked chat eval, so retries may reuse names. \
Keep edits minimal and preserve source limits. If the request is impossible with the given state, \
make code throw new Error('short reason').";

/// Back-compat name for the dev harness and older prompts.
#[cfg(test)]
pub(crate) const SYSTEM_PROMPT: &str = DEVELOPER_CONTRACT;

const FEW_SHOT_USER: &str = r#"Example request:
Build it from the reaction shots.

Timeline state JSON:
{"playhead":0,"playing":false,"selected":null,"duration":0,"fps":30,"tool":"select","magnet":true,"musicBedMuted":true,"clips":[],"assets":[{"name":"Rushes.mp4","path":"media/Rushes.mp4","duration":60,"width":1280,"height":720,"hasVideo":true,"hasAudio":true,"ready":true}],"sceneClips":[],"sweepCandidates":[{"id":"media/Rushes.mp4#1","asset":"media/Rushes.mp4","assetName":"Rushes.mp4","title":"Rushes.mp4 · 0:12","sourceIn":12,"duration":4,"score":88,"reason":"Big laugh, steady frame.","category":"reaction","tags":["loud-peak","steady"],"musicBed":false,"quality":{"stability":0.8,"audio":0.7,"motion":0.3},"stability":0.8,"audioEnergy":0.5,"audioPeak":0.9,"isReaction":true,"reactionScore":0.84,"hasVideo":true,"transcript":""}],"transcriptSegments":[]}"#;

const FEW_SHOT_ASSISTANT: &str = r#"{"code": "var shots = sweepCandidates().filter(function (c) { return c.isReaction; });\nclips().forEach(function (c) { deleteClip(c.id); });\nvar cursor = 0;\nshots.forEach(function (c) { addAssetRange(c.asset, cursor, c.sourceIn, c.duration); cursor += c.duration; });\nconsole.log(\"Done — built the cut from the reaction shots.\");", "explanation": "Built the cut from the reaction shots."}"#;

fn model_context_state(snapshot: &str) -> String {
    let Ok(mut state) = serde_json::from_str::<serde_json::Value>(snapshot) else {
        return snapshot.to_owned();
    };
    let mut changed = false;
    for field in ["sweepCandidates", "sceneClips"] {
        let Some(ranges) = state[field].as_array_mut() else {
            continue;
        };
        for range in ranges {
            let Some(range) = range.as_object_mut() else {
                continue;
            };
            changed = true;
            range.remove("transcript");
            if let Some(review) = range
                .get_mut("review")
                .and_then(|value| value.as_object_mut())
            {
                review.retain(|key, _| matches!(key.as_str(), "status" | "subjectEvidence"));
                if let Some(evidence) = review
                    .get_mut("subjectEvidence")
                    .and_then(|v| v.as_array_mut())
                {
                    for observation in evidence {
                        if let Some(fields) = observation.as_object_mut() {
                            fields.retain(|key, value| {
                                !value.is_null()
                                    && matches!(
                                        key.as_str(),
                                        "concept"
                                            | "status"
                                            | "checkedStart"
                                            | "checkedEnd"
                                            | "sampledFrames"
                                            | "observedFrames"
                                            | "observedFraction"
                                            | "firstObserved"
                                            | "lastObserved"
                                            | "maxSampleSpacing"
                                    )
                            });
                        }
                    }
                }
            }
            if let Some(visual) = range
                .get_mut("visual")
                .and_then(|value| value.as_object_mut())
            {
                visual.retain(|key, value| {
                    !value.is_null()
                        && matches!(
                            key.as_str(),
                            "source"
                                | "samStatus"
                                | "facePresent"
                                | "samFrameTime"
                                | "samBoxCoverage"
                                | "samFaceBoxCoverage"
                                | "samConfidence"
                                | "samFaceConfidence"
                        )
                });
            }
        }
    }
    if changed {
        state.to_string()
    } else {
        snapshot.to_owned()
    }
}

/// Final (volatile) user message: correction context if any, then
/// the request, then the timeline JSON last so the shared prefix
/// stays cacheable. The timeline is always current-state, never
/// accumulated history.
pub(crate) fn final_user_content(
    request: &str,
    state_json: &str,
    correction: Option<(&str, &str)>,
) -> String {
    let state_json = model_context_state(state_json);
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
/// inside `TOKEN_BUDGET`. Reject when the verbatim tail and state cannot
/// fit; dropping source ranges to make room would change the edit contract.
pub(crate) fn ensure_within_budget(
    session: &mut ChatSession,
    request: &str,
    state_json: &str,
    correction: Option<(&str, &str)>,
) -> Result<(), String> {
    loop {
        let recent = recent_excluding_current(session);
        let messages = build_context_messages(session, recent, request, state_json, correction);
        if estimate_messages_tokens(&messages) <= TOKEN_BUDGET {
            return Ok(());
        }
        if !session.compact_oldest_exchange() {
            return Err("Request exceeds the model context budget; use a smaller project or a shorter conversation.".to_owned());
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

pub(super) fn api_endpoint(settings: &ChatSettings, endpoint: &str) -> String {
    let base = settings.base_url.trim().trim_end_matches('/');
    let base = base.strip_suffix("/chat/completions").unwrap_or(base);
    format!("{base}/{}", endpoint.trim_start_matches('/'))
}

/// A connection error as shown inside the Settings dialog. The status bar
/// keeps the original text, which points to Settings from outside it.
fn in_settings_wording(error: &str) -> &str {
    match error {
        "Add your API key in Settings first." => "Enter an API key first.",
        "Choose a model in Settings." => "Choose a model first.",
        "Check your API key in Settings." => "The API key was not accepted.",
        "That model was not found. Check the model in Settings." => "That model was not found.",
        "The model request was rejected. Check Settings and try again." => {
            "The request was rejected."
        }
        other => other,
    }
}

/// Validates authentication and catalog availability, not inference capability.
pub(super) fn check_model_connection(settings: &ChatSettings) -> Result<(), String> {
    if !settings.can_call_model() {
        return Err("Add your API key in Settings first.".to_owned());
    }
    if settings.model.trim().is_empty() {
        return Err("Choose a model in Settings.".to_owned());
    }
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut call = agent
        .get(api_endpoint(settings, "models"))
        .header("Accept", "application/json");
    if !settings.api_key.trim().is_empty() {
        call = call.header(
            "Authorization",
            format!("Bearer {}", settings.api_key.trim()),
        );
    }
    let mut response = call
        .call()
        .map_err(|_| "Couldn't reach the model service.".to_owned())?;
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            404 => "Couldn't find the model catalog; check Base URL.".to_owned(),
            status => api_error_for_status(status),
        });
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_to_string()
        .map_err(|_| "Couldn't read the model catalog.".to_owned())?;
    let catalog: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| "The model catalog was invalid.".to_owned())?;
    let models = catalog["data"]
        .as_array()
        .ok_or_else(|| "The model catalog was invalid.".to_owned())?;
    if !models
        .iter()
        .any(|model| model["id"].as_str() == Some(settings.model.as_str()))
    {
        return Err("Selected model isn't available for this key.".to_owned());
    }
    Ok(())
}

/// One attempt. Returns (status, Retry-After, body text); transport
/// failures are plain errors and are not retried.
fn post_once(
    agent: &ureq::Agent,
    settings: &ChatSettings,
    body: &serde_json::Value,
) -> Result<(u16, Option<Duration>, String), String> {
    let url = api_endpoint(settings, "chat/completions");
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
    // Editing a large curated project can take the model over two minutes
    // (a 262k-token Spark request took 129 s). The user can cancel a chat
    // edit, so it gets a long limit; uncancellable callers keep 120 s.
    let timeout = if cancel.is_some() { 600 } else { 120 };
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout)))
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
        if self.persist_session {
            self.chat_session.save();
        }
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

    /// Model and local-intent edits either finish completely or restore the
    /// editor. Explicit console I/O uses the same command executor without
    /// this transaction; generated code cannot authorize filesystem effects.
    fn apply_edit_commands(
        &mut self,
        ctx: &egui::Context,
        commands: Vec<ScriptCommand>,
    ) -> Result<Vec<String>, String> {
        for command in &commands {
            if matches!(command, ScriptCommand::Import(_) | ScriptCommand::Export(_)) {
                return Err("use the Import or Export controls for files".to_owned());
            }
            command.validate()?;
        }
        let before = TimelineSnapshot::capture(&self.timeline, self.selected, self.playhead);
        let was_playing = self.playing;
        let magnet = self.timeline_view.magnet;
        let zoom = self.timeline_view.px_per_sec;
        let mute_music_beds = self.mute_music_beds;
        let started = Instant::now();
        let result = (|| {
            let mut notes = Vec::new();
            let mut created = std::collections::HashMap::new();
            for command in commands {
                if let Some(note) = self.apply_script_command(ctx, command, &mut created)? {
                    notes.push(note);
                }
                // Finite input can still overflow frame-grid arithmetic.
                if !self.playhead.is_finite()
                    || self.timeline.clips.iter().any(|clip| {
                        !clip.start.is_finite()
                            || !clip.duration.is_finite()
                            || !clip.source_in.is_finite()
                            || !clip.end().is_finite()
                    })
                {
                    return Err("edit exceeds the supported timeline range".to_owned());
                }
                // Cooperative bound: individual native commands are not
                // preemptible, but a long finite batch cannot run unchecked.
                if started.elapsed() > Duration::from_millis(250) {
                    return Err("edit took too long; try a smaller batch".to_owned());
                }
            }
            Ok(notes)
        })();
        match result {
            Ok(notes) => {
                if self.timeline != before.timeline {
                    self.chat_undo = Some(before);
                    self.stop_sources();
                    self.still_key = None;
                    self.still_cache.clear();
                }
                Ok(notes)
            }
            Err(error) => {
                before.restore_into(&mut self.timeline, &mut self.selected, &mut self.playhead);
                self.playing = was_playing;
                self.timeline_view.magnet = magnet;
                self.timeline_view.px_per_sec = zoom;
                self.mute_music_beds = mute_music_beds;
                self.stop_sources();
                self.last_now = None;
                self.still_key = None;
                self.still_cache.clear();
                Err(error)
            }
        }
    }

    pub(super) fn undo_chat_edit(&mut self) {
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

    pub(super) fn cancel_chat(&mut self) {
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
        let Some(pending) = self.chat_pending.as_ref() else {
            return;
        };
        // Corrections use the same basis as the initial request.
        let state = pending.state_json.clone();
        let request = pending.request.clone();
        let generation = pending.generation;
        let cancel = pending.cancel.clone();
        let progress = pending.progress.clone();
        let settings = self.chat_settings.clone();
        let correction_ref = correction
            .as_ref()
            .map(|(js, err)| (js.as_str(), err.as_str()));
        if let Err(message) =
            ensure_within_budget(&mut self.chat_session, &request, &state, correction_ref)
        {
            cancel.store(true, Ordering::Relaxed);
            self.chat_pending = None;
            self.chat_rx = None;
            self.push_chat(ChatRole::Assistant, message.clone());
            self.status = message;
            return;
        }
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
            timeline: self.timeline.clone(),
            state_json: self.script_state_json(),
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
                        .filter(|asset| !self.clashes_with_format(&asset.info))
                        .flat_map(|asset| asset.sweep.candidates.iter())
                        .filter(|candidate| {
                            !candidate.dismissed
                                && !candidate.tags.iter().any(|tag| tag == "on-screen-text")
                        })
                        .map(CandidateView::from)
                        .collect::<Vec<_>>(),
                    sort,
                );
                self.push_chat(ChatRole::User, intent.label.to_owned());
                if ordered.is_empty() {
                    let message = if self.assets.is_empty() {
                        "No clips yet. Import media first."
                    } else {
                        "No curated clips yet. Curate a video first."
                    }
                    .to_owned();
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
                let message = match self.apply_edit_commands(ctx, commands) {
                    Ok(notes) => summarize_chat_result(&notes, ordered.len()),
                    Err(error) => format!("No changes made: {}.", error.trim_end_matches('.')),
                };
                self.push_chat(ChatRole::Assistant, message.clone());
                self.status = message;
            }
        }
    }

    fn start_connection_check(&mut self, ctx: &egui::Context) {
        if self.connection_check.is_some() {
            return;
        }
        let settings = self.chat_settings.clone();
        let (tx, rx) = channel();
        self.connection_check = Some(PendingConnectionCheck {
            settings: settings.clone(),
            rx,
        });
        self.connection_result = None;
        self.status = "Checking connection…".to_owned();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(check_model_connection(&settings));
            ctx.request_repaint();
        });
    }

    fn poll_connection_check(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.connection_check.as_ref() else {
            return;
        };
        if pending.settings.connection() != self.chat_settings.connection() {
            self.connection_check = None;
            return;
        }
        let result = match pending.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(120));
                return;
            }
            Err(TryRecvError::Disconnected) => {
                Err("Connection check stopped; try again.".to_owned())
            }
        };
        self.connection_check = None;
        self.status = match &result {
            Ok(()) => "Connection checked; selected model is listed.".to_owned(),
            Err(error) => error.clone(),
        };
        self.connection_result = Some(result);
    }

    pub(super) fn poll_chat(&mut self, ctx: &egui::Context) {
        self.poll_connection_check(ctx);
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
                let pending = self.chat_pending.as_ref().expect("current generation");
                if pending.timeline != self.timeline {
                    self.chat_pending = None;
                    let message = "Timeline changed; send the edit again.".to_owned();
                    self.push_chat(ChatRole::Assistant, message.clone());
                    self.status = message;
                    return;
                }
                // Selection and playhead may change without editing the
                // timeline. Defaults still refer to what the model saw.
                let state = pending.state_json.clone();
                let Some(engine) = self.script_engine.as_mut() else {
                    self.chat_pending = None;
                    let message = "I couldn't do that one — scripting is unavailable.".to_owned();
                    self.push_chat(ChatRole::Assistant, message.clone());
                    self.status = message;
                    return;
                };
                match engine.eval_checked(&js, &state) {
                    Ok((_, commands)) => {
                        // A reply without edits is a report or a question; its
                        // details are in the script's log, not the summary.
                        let reports = commands
                            .iter()
                            .all(|command| matches!(command, ScriptCommand::Log(_)));
                        let command_count = commands.len();
                        let result = self.apply_edit_commands(ctx, commands);
                        self.chat_pending = None;
                        let message = match result {
                            Err(error) => {
                                format!("No changes made: {}.", error.trim_end_matches('.'))
                            }
                            Ok(notes) => {
                                let summary = edit
                                    .explanation
                                    .filter(|text| !text.trim().is_empty())
                                    .unwrap_or_else(|| {
                                        summarize_chat_result(&notes, command_count)
                                    });
                                let details = notes.iter().map(|note| note.trim()).filter(|note| {
                                    reports
                                        && !note.is_empty()
                                        && !note.starts_with("Done")
                                        && *note != summary
                                });
                                std::iter::once(summary.as_str())
                                    .chain(details)
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            }
                        };
                        // The status bar keeps one line; chat keeps the report.
                        self.status = message.lines().next().unwrap_or_default().to_owned();
                        self.push_chat(ChatRole::Assistant, message);
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
    /// Chat tab body for the right dock. Panel ownership moved to
    /// `EditorApp::right_dock`; this only paints into the given Ui.
    pub(super) fn chat_content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let ui = &mut *ui;
        let focus_requested = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::K));
        let mut submitted: Option<String> = None;
        let mut clicked_intent: Option<&'static str> = None;
        let mut cancel_clicked = false;
        {
            if let Some(pending) = &self.chat_pending {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.add(
                        egui::Label::new(
                            RichText::new(pending.progress_label()).color(theme::MUTED_FG),
                        )
                        .truncate(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::ghost_button(ui, "Cancel")
                            .on_hover_text("Cancel the in-flight model call")
                            .clicked()
                        {
                            cancel_clicked = true;
                        }
                    });
                });
                ui.add_space(6.0);
            }

            // Button intents: one registry, local first. New buttons are
            // declared in `intents.rs`. Compact chips at natural width wrap
            // like suggestions instead of a grid of full-width buttons; the
            // tooltip says which run offline.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                for intent in INTENTS {
                    let tooltip = match intent.kind {
                        IntentKind::Local(_) => format!("{} Runs locally.", intent.tooltip),
                        IntentKind::Model => format!("{} Uses the model.", intent.tooltip),
                    };
                    if theme::secondary_button(ui, intent.label)
                        .on_hover_text(tooltip)
                        .clicked()
                    {
                        clicked_intent = Some(intent.id);
                    }
                }
            });
            ui.add_space(8.0);
            theme::divider(ui);
            ui.add_space(4.0);

            let debug_height = if self.show_chat_debug { 58.0 } else { 0.0 };
            let messages_height = (ui.available_height() - 74.0 - debug_height).max(28.0);
            egui::ScrollArea::vertical()
                .id_salt("chat_messages")
                .max_height(messages_height)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if self.chat_session.messages.is_empty() {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new("Ask for an edit in plain words.")
                                    .color(theme::MUTED_FG),
                            );
                            ui.label(theme::caption(
                                "For example: cut the dead air from the intro.",
                            ));
                        });
                    }
                    let width = ui.available_width();
                    for message in &self.chat_session.messages {
                        let user = matches!(message.role, ChatRole::User);
                        let layout = if user {
                            egui::Layout::right_to_left(egui::Align::Min)
                        } else {
                            egui::Layout::left_to_right(egui::Align::Min)
                        };
                        // Requests sit in bubbles on the right; replies read
                        // as plain text across the dock, like a transcript.
                        ui.with_layout(layout, |ui| {
                            if user {
                                egui::Frame::new()
                                    .fill(theme::SECONDARY)
                                    .corner_radius(egui::CornerRadius::same(theme::R_LG))
                                    .inner_margin(egui::Margin::symmetric(11, 7))
                                    .show(ui, |ui| {
                                        ui.set_max_width(width * 0.8);
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&message.text)
                                                    .color(theme::FOREGROUND),
                                            )
                                            .wrap(),
                                        );
                                    });
                            } else {
                                ui.add_space(2.0);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&message.text).color(theme::MUTED_FG),
                                    )
                                    .wrap(),
                                );
                            }
                        });
                        ui.add_space(10.0);
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

            // Pin the composer to the bottom of the dock: one input surface
            // with the send action inside it.
            ui.add_space((ui.available_height() - 42.0).max(0.0));
            egui::Frame::new()
                .fill(theme::INPUT)
                .corner_radius(egui::CornerRadius::same(theme::R_LG))
                .inner_margin(egui::Margin {
                    left: 4,
                    right: 5,
                    top: 4,
                    bottom: 4,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        // TextEdit adds its margin outside the desired width;
                        // the row must fit exactly or the dock grows each frame.
                        let input_width = (ui.available_width() - 28.0 - 4.0 - 16.0).max(80.0);
                        let response = ui
                            .add(
                                egui::TextEdit::singleline(&mut self.chat_input)
                                    .hint_text("Describe the edit…")
                                    .frame(false)
                                    .margin(egui::Margin::symmetric(8, 6))
                                    .desired_width(input_width),
                            )
                            .on_hover_text(
                                "Describe an edit. Enter sends it. Ctrl+K focuses chat.",
                            );
                        if focus_requested {
                            response.request_focus();
                        }
                        let ready = !self.chat_input.trim().is_empty();
                        let send_clicked = send_button(ui, ready)
                            .on_hover_text("Send this edit request (Enter)")
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
        }
        if cancel_clicked {
            self.cancel_chat();
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
        let mut check_connection = false;
        let mut format_choice = None;
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
                ui.label(theme::caption("Meta Model API for chat edits and curation."));
                ui.add_space(16.0);
                let field_w = 300.0;
                egui::Grid::new("chat_settings")
                    .num_columns(2)
                    .spacing([16.0, 10.0])
                    .min_col_width(84.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new("API key").color(theme::MUTED_FG));
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.chat_settings.api_key)
                                    .password(true)
                                    .hint_text("Not set")
                                    .margin(egui::Margin::symmetric(8, 6))
                                    .desired_width(field_w),
                            )
                            .on_hover_text(
                                "Meta Model API key. Stored only in the local app config.",
                            )
                            .changed()
                        {
                            dirty = true;
                        }
                        ui.end_row();

                        ui.label(RichText::new("Base URL").color(theme::MUTED_FG));
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.chat_settings.base_url)
                                    .margin(egui::Margin::symmetric(8, 6))
                                    .desired_width(field_w),
                            )
                            .on_hover_text("OpenAI-compatible API base URL")
                            .changed()
                        {
                            dirty = true;
                        }
                        ui.end_row();

                        ui.label(RichText::new("Model").color(theme::MUTED_FG));
                        let before = self.chat_settings.model.clone();
                        egui::ComboBox::from_id_salt("chat_model")
                            .width(field_w + 16.0)
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
                            .on_hover_text("Model used for chat editing and curation");
                        if self.chat_settings.model != before {
                            dirty = true;
                        }
                        ui.end_row();

                        ui.label(RichText::new("Training").color(theme::MUTED_FG));
                        let before = self.chat_settings.contributor_opt_in;
                        ui.checkbox(
                            &mut self.chat_settings.contributor_opt_in,
                            "Contributor models",
                        )
                        .on_hover_text(
                            "Contributor models let Meta train on your prompts and content. Off by default; Standard models do not.",
                        );
                        if self.chat_settings.contributor_opt_in != before {
                            dirty = true;
                        }
                        ui.end_row();

                        ui.label(RichText::new("Format").color(theme::MUTED_FG));
                        format_choice = theme::segmented(
                            ui,
                            self.chat_settings.format,
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
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    check_connection = ui
                        .add_enabled_ui(self.connection_check.is_none(), |ui| {
                            theme::secondary_button(ui, "Check connection")
                                .on_hover_text(
                                    "Check authentication and model availability without running inference",
                                )
                                .clicked()
                        })
                        .inner;
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::primary_button(ui, "Done")
                            .on_hover_text("Close settings; changes save as you edit")
                            .clicked()
                        {
                            close_clicked = true;
                        }
                        // The result sits beside the button that produced it.
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            if self.connection_check.is_some() {
                                ui.add(egui::Spinner::new().size(14.0));
                                ui.label(theme::caption("Checking…"));
                            } else if let Some(result) = &self.connection_result {
                                let (text, color) = match result {
                                    Ok(()) => ("Connected".to_owned(), theme::FOREGROUND),
                                    Err(error) => {
                                        (in_settings_wording(error).to_owned(), theme::DESTRUCTIVE)
                                    }
                                };
                                ui.add(
                                    egui::Label::new(RichText::new(&text).size(12.0).color(color))
                                        .truncate(),
                                )
                                .on_hover_text(text);
                            }
                        });
                    });
                });
            });
        self.show_chat_settings = open && !close_clicked;
        if let Some(format) = format_choice.filter(|f| *f != self.chat_settings.format) {
            // Not a connection setting: keep any check result.
            self.chat_settings.format = format;
            self.set_format(format);
            if let Err(error) = self.chat_settings.save() {
                self.status = error;
            }
        }
        if dirty {
            self.connection_check = None;
            self.connection_result = None;
            if let Err(error) = self.chat_settings.save() {
                self.status = error;
                return;
            }
            self.status = "Settings saved.".to_owned();
        }
        if check_connection {
            self.start_connection_check(ctx);
        }
    }
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

/// Round send button with an up arrow; bright once there is text to send.
fn send_button(ui: &mut egui::Ui, ready: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    let fill = if !ready {
        theme::SECONDARY
    } else if response.hovered() {
        theme::MUTED_FG
    } else {
        theme::PRIMARY
    };
    let fg = if ready {
        theme::PRIMARY_FG
    } else {
        theme::SUBTLE_FG
    };
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 13.0, fill);
    let c = rect.center();
    let stroke = egui::Stroke::new(1.8_f32, fg);
    painter.line_segment(
        [c + egui::vec2(0.0, 5.5), c + egui::vec2(0.0, -5.0)],
        stroke,
    );
    painter.line_segment(
        [c + egui::vec2(-4.5, -0.5), c + egui::vec2(0.0, -5.0)],
        stroke,
    );
    painter.line_segment(
        [c + egui::vec2(4.5, -0.5), c + egui::vec2(0.0, -5.0)],
        stroke,
    );
    if ready && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
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

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("openreel-chat-test-{}-{nonce}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn with_isolated_config(test: &str, run: impl FnOnce(PathBuf)) {
        if std::env::var("OPENREEL_ISOLATED_CHAT_TEST").as_deref() == Ok(test) {
            run(config_path().expect("isolated settings path"));
            return;
        }
        let dir = TestDir::new();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env("OPENREEL_ISOLATED_CHAT_TEST", test)
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

    fn settings_frame(
        ctx: &egui::Context,
        app: &mut EditorApp,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1024.0, 768.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                app.poll_chat(ctx);
                app.chat_settings_window(ctx);
            },
        )
    }

    fn click_settings_label(ctx: &egui::Context, app: &mut EditorApp, label: &str) {
        settings_frame(ctx, app, Vec::new());
        let output = settings_frame(ctx, app, Vec::new());
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

    fn fake_model_api(
        respond: impl FnOnce(&str) -> (u16, String) + Send + 'static,
    ) -> (ChatSettings, thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let settings = ChatSettings {
            api_key: "fixture-key-only".to_owned(),
            base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            ..ChatSettings::default()
        };
        let server = thread::spawn(move || {
            use std::io::{BufRead, Read};

            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "expected a local HTTP request");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("local HTTP accept failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = String::new();
            {
                let mut reader = std::io::BufReader::new(&mut stream);
                while !request.ends_with("\r\n\r\n") {
                    assert!(reader.read_line(&mut request).unwrap() > 0);
                    assert!(request.len() < 64 * 1024);
                }
                let length = request
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                    .unwrap_or(0);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                request.push_str(&String::from_utf8(body).unwrap());
            }
            let (status, body) = respond(&request);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            request
        });
        (settings, server)
    }

    fn catalog(models: &[&str]) -> String {
        serde_json::json!({
            "object": "list",
            "data": models.iter().map(|id| serde_json::json!({
                "id": id,
                "object": "model",
                "created": 1,
                "owned_by": "meta",
            })).collect::<Vec<_>>(),
        })
        .to_string()
    }

    #[test]
    fn connection_check_authenticates_and_finds_the_exact_selected_model() {
        for suffix in ["", "/", "/chat/completions", "/chat/completions/"] {
            for model in [MODEL_SPARK, MODEL_WONDER, "custom-model"] {
                let (mut settings, server) = fake_model_api(|_| {
                    (200, catalog(&[MODEL_SPARK, MODEL_WONDER, "custom-model"]))
                });
                settings.base_url.push_str(suffix);
                settings.model = model.to_owned();
                settings.api_key = "  fixture-key-only  ".to_owned();
                assert_eq!(check_model_connection(&settings), Ok(()));
                let request = server.join().unwrap();
                assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
                assert!(request
                    .to_ascii_lowercase()
                    .contains("\r\nauthorization: bearer fixture-key-only\r\n"));
                assert!(request.ends_with("\r\n\r\n"), "no inference body");
            }
        }
    }

    #[test]
    fn connection_check_allows_keyless_local_mock() {
        let (mut settings, server) = fake_model_api(|_| (200, catalog(&[MODEL_WONDER])));
        settings.api_key.clear();
        assert_eq!(check_model_connection(&settings), Ok(()));
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
    }

    #[test]
    fn connection_check_rejects_invalid_auth_without_provider_text() {
        for status in [401, 403, 404, 429, 500, 503] {
            let (settings, server) = fake_model_api(move |_| {
                (
                    status,
                    r#"{"error":{"message":"provider-private-detail fixture-key-only"}}"#
                        .to_owned(),
                )
            });
            let error = check_model_connection(&settings).unwrap_err();
            server.join().unwrap();
            assert!(!error.contains("provider-private-detail"));
            assert!(!error.contains("fixture-key-only"));
            match status {
                401 | 403 => assert!(error.contains("API key")),
                404 => assert!(error.contains("Base URL")),
                429 => assert!(error.contains("busy")),
                _ => assert!(error.contains("service had a problem")),
            }
        }
    }

    #[test]
    fn connection_check_does_not_substitute_contributor_or_other_models() {
        for contributor in [false, true] {
            let (mut settings, server) =
                fake_model_api(|_| (200, catalog(&[MODEL_SPARK, "muse-wonder-1.0-contributor"])));
            settings.contributor_opt_in = contributor;
            let error = check_model_connection(&settings).unwrap_err();
            server.join().unwrap();
            assert!(error.contains("Selected model"));
            assert_eq!(settings.model, MODEL_WONDER);
        }
        let (mut settings, server) =
            fake_model_api(|_| (200, catalog(&["muse-spark-1.3-contributor"])));
        settings.model = "muse-spark-1.3-contributor".to_owned();
        assert_eq!(check_model_connection(&settings), Ok(()));
        server.join().unwrap();
    }

    #[test]
    fn connection_check_rejects_invalid_or_oversized_catalogs_safely() {
        for body in [
            "provider-private-detail fixture-key-only".to_owned(),
            r#"{"models":["muse-spark-1.3"]}"#.to_owned(),
            r#"{"data":{}}"#.to_owned(),
            "x".repeat(1024 * 1024 + 1),
        ] {
            let oversized = body.len() > 1024 * 1024;
            let (settings, server) = fake_model_api(move |_| (200, body));
            let error = check_model_connection(&settings).unwrap_err();
            server.join().unwrap();
            assert!(!error.contains("provider-private-detail"));
            assert!(!error.contains("fixture-key-only"));
            assert_eq!(
                error,
                if oversized {
                    "Couldn't read the model catalog."
                } else {
                    "The model catalog was invalid."
                },
            );
        }
    }

    #[test]
    fn connection_check_reports_invalid_settings_without_echoing_them() {
        let mut settings = ChatSettings {
            base_url: "not a URL with provider-private-detail".to_owned(),
            ..ChatSettings::default()
        };
        assert!(check_model_connection(&settings)
            .unwrap_err()
            .contains("API key"));
        settings.api_key = "fixture-key-only".to_owned();
        assert_eq!(
            check_model_connection(&settings).unwrap_err(),
            "Couldn't reach the model service.",
        );
        settings.model.clear();
        assert_eq!(
            check_model_connection(&settings).unwrap_err(),
            "Choose a model in Settings.",
        );
    }

    #[test]
    fn normalized_endpoints_keep_existing_chat_requests_working() {
        for suffix in ["", "/", "/chat/completions", "/chat/completions/"] {
            let (mut settings, server) = fake_model_api(|_| {
                (
                    200,
                    r#"{"choices":[{"message":{"content":"ok"}}]}"#.to_owned(),
                )
            });
            let base = settings.base_url.clone();
            settings.base_url = format!("  {base}{suffix}  ");
            assert_eq!(api_endpoint(&settings, "models"), format!("{base}/models"));
            assert_eq!(
                api_endpoint(&settings, "/responses"),
                format!("{base}/responses")
            );
            assert_eq!(
                api_endpoint(&settings, "asr/transcribe"),
                format!("{base}/asr/transcribe"),
            );
            let body = serde_json::json!({"model": MODEL_SPARK, "messages": []});
            let response = post_chat_completion(&settings, &body).unwrap();
            assert_eq!(chat_content(&response), "ok");
            let request = server.join().unwrap();
            assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
            let (_, sent_body) = request.split_once("\r\n\r\n").unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(sent_body).unwrap(),
                body
            );
        }
    }

    #[test]
    fn connection_button_uses_a_worker_and_reports_after_the_dialog_closes() {
        for status in [200, 401] {
            let (seen_tx, seen_rx) = channel();
            let (release_tx, release_rx) = channel();
            let (settings, server) = fake_model_api(move |_| {
                seen_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("the UI must return before the HTTP reply");
                (status, catalog(&[MODEL_WONDER]))
            });
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            app.chat_settings = settings;
            app.show_chat_settings = true;
            click_settings_label(&ctx, &mut app, "Check connection");
            seen_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(app.connection_check.is_some());
            assert_eq!(app.status, "Checking connection…");
            click_settings_label(&ctx, &mut app, "Check connection");
            app.show_chat_settings = false;
            release_tx.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.connection_check.is_some() {
                app.poll_chat(&ctx);
                assert!(
                    Instant::now() < deadline,
                    "connection result must reach the UI"
                );
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                app.status,
                if status == 200 {
                    "Connection checked; selected model is listed."
                } else {
                    "Check your API key in Settings."
                },
            );
            assert!(app.chat_session.messages.is_empty());
            server.join().unwrap();
        }
    }

    #[test]
    fn connection_results_ignore_every_changed_setting() {
        for field in ["key", "base", "model", "contributor"] {
            for result in [Ok(()), Err("Check your API key in Settings.".to_owned())] {
                let ctx = egui::Context::default();
                let mut app = EditorApp::headless_for_test(&ctx);
                let (tx, rx) = channel();
                app.connection_check = Some(PendingConnectionCheck {
                    settings: app.chat_settings.clone(),
                    rx,
                });
                tx.send(result).unwrap();
                match field {
                    "key" => app.chat_settings.api_key = "new-fixture-key".to_owned(),
                    "base" => app.chat_settings.base_url = "http://localhost:8000/v1".to_owned(),
                    "model" => app.chat_settings.model = MODEL_SPARK.to_owned(),
                    _ => app.chat_settings.contributor_opt_in = true,
                }
                app.status = "Newer status.".to_owned();
                app.poll_chat(&ctx);
                assert!(app.connection_check.is_none());
                assert_eq!(app.status, "Newer status.", "{field}");
            }
        }
    }

    #[test]
    fn settings_save_atomically_replaces_previous_file() {
        with_isolated_config(
            "app::chat::tests::settings_save_atomically_replaces_previous_file",
            |path| {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                let original = r#"{"model":"muse-wonder-1.0"}"#;
                std::fs::write(&path, original).unwrap();
                let previous = path.with_file_name("previous.json");
                std::fs::hard_link(&path, &previous).unwrap();
                let settings = ChatSettings::default();
                let _ = settings.save();
                assert_eq!(ChatSettings::load().model, MODEL_WONDER);
                assert_eq!(
                    std::fs::read_to_string(previous).unwrap(),
                    original,
                    "a reader of the previous file must never see a partial replacement"
                );
            },
        );
    }

    #[test]
    fn settings_save_reloads_private_credentials_and_preserves_schema() {
        with_isolated_config(
            "app::chat::tests::settings_save_reloads_private_credentials_and_preserves_schema",
            |path| {
                let mut settings = ChatSettings {
                    api_key: "fixture-key-only".to_owned(),
                    model: MODEL_WONDER.to_owned(),
                    ..ChatSettings::default()
                };
                for (contributor, format) in [
                    (false, Orientation::Landscape),
                    (true, Orientation::Portrait),
                ] {
                    settings.contributor_opt_in = contributor;
                    settings.format = format;
                    settings.save().unwrap();
                    assert!(ChatSettings::load() == settings, "settings round-trip");
                    let value: serde_json::Value =
                        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                    assert_eq!(value.as_object().unwrap().len(), 5);
                    assert_eq!(value["model"], "muse-wonder-1.0");
                    assert_eq!(value["format"], settings.format.as_str());
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        assert_eq!(
                            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                            0o600,
                        );
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                            .unwrap();
                    }
                }
                assert_eq!(
                    std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                    1
                );
            },
        );
    }

    #[test]
    fn settings_load_only_migrates_unversioned_builtin_models() {
        with_isolated_config(
            "app::chat::tests::settings_load_only_migrates_unversioned_builtin_models",
            |path| {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                for (stored, expected) in [
                    ("muse-spark", "muse-spark-1.3"),
                    ("muse-wonder", "muse-wonder-1.0"),
                    ("muse-spark-1.2", "muse-spark-1.2"),
                    ("muse-spark-1.3-contributor", "muse-spark-1.3-contributor"),
                    ("custom-model", "custom-model"),
                ] {
                    std::fs::write(
                        &path,
                        serde_json::json!({"model": stored, "contributor_opt_in": false})
                            .to_string(),
                    )
                    .unwrap();
                    let settings = ChatSettings::load();
                    assert_eq!(settings.model, expected);
                    assert!(!settings.contributor_opt_in);
                }
            },
        );
    }

    #[test]
    fn settings_save_failure_is_displayed_and_cleans_temporary_file() {
        with_isolated_config(
            "app::chat::tests::settings_save_failure_is_displayed_and_cleans_temporary_file",
            |path| {
                std::fs::create_dir_all(&path).unwrap();
                let previous = path.join("previous.json");
                std::fs::write(&previous, "keep this file").unwrap();
                let settings = ChatSettings::default();
                let error = settings.save().unwrap_err();
                assert_eq!(error, "Couldn't save settings.");

                let ctx = egui::Context::default();
                let mut app = EditorApp::headless_for_test(&ctx);
                app.show_chat_settings = true;
                click_settings_label(&ctx, &mut app, "Contributor models");
                assert!(app.chat_settings.contributor_opt_in);
                assert_eq!(app.chat_settings.model, MODEL_WONDER);
                assert_eq!(app.status, error);
                assert_eq!(std::fs::read_to_string(previous).unwrap(), "keep this file");
                assert_eq!(
                    std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                    1
                );
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn settings_partial_write_failure_preserves_previous_file() {
        with_isolated_config(
            "app::chat::tests::settings_partial_write_failure_preserves_previous_file",
            |path| {
                if std::env::var_os("OPENREEL_SETTINGS_FILE_LIMIT").is_some() {
                    let settings = ChatSettings {
                        model: "large-fixture-model".repeat(1024),
                        ..ChatSettings::default()
                    };
                    assert_eq!(settings.save().unwrap_err(), "Couldn't save settings.");
                    return;
                }
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                let original = r#"{"model":"muse-wonder-1.0"}"#;
                std::fs::write(&path, original).unwrap();
                let output = std::process::Command::new("sh")
                    .args([
                        "-c",
                        "ulimit -f 1; trap '' XFSZ; exec \"$@\"",
                        "settings-test",
                    ])
                    .arg(std::env::current_exe().unwrap())
                    .args([
                        "app::chat::tests::settings_partial_write_failure_preserves_previous_file",
                        "--exact",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env("OPENREEL_SETTINGS_FILE_LIMIT", "1")
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "limited-file test failed:\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
                assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
                assert_eq!(
                    std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                    1
                );
            },
        );
    }

    #[test]
    fn settings_edits_invalidate_checks_even_after_changing_back() {
        with_isolated_config(
            "app::chat::tests::settings_edits_invalidate_checks_even_after_changing_back",
            |_| {
                let ctx = egui::Context::default();
                let mut app = EditorApp::headless_for_test(&ctx);
                app.show_chat_settings = true;
                let original = app.chat_settings.clone();
                let (tx, rx) = channel();
                app.connection_check = Some(PendingConnectionCheck {
                    settings: original.clone(),
                    rx,
                });
                click_settings_label(&ctx, &mut app, "Contributor models");
                assert!(app.chat_settings.contributor_opt_in);
                assert!(app.connection_check.is_none());
                click_settings_label(&ctx, &mut app, "Contributor models");
                assert!(app.chat_settings == original);
                assert!(tx.send(Ok(())).is_err(), "the old receiver was dropped");
                app.poll_chat(&ctx);
                assert_eq!(app.status, "Settings saved.");
            },
        );
    }

    /// Inside Settings, a failed check must not tell the user to go to
    /// Settings; the status bar keeps that pointer for after it closes.
    #[test]
    fn missing_key_check_reads_right_inside_settings() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        app.chat_settings.api_key.clear();
        app.show_chat_settings = true;
        click_settings_label(&ctx, &mut app, "Check connection");
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.connection_check.is_some() {
            app.poll_chat(&ctx);
            assert!(Instant::now() < deadline, "check must finish");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.status, "Add your API key in Settings first.");
        let mut texts = Vec::new();
        for _ in 0..2 {
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                app.chat_settings_window(ctx);
            });
            texts = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
        }
        assert!(
            texts.iter().any(|t| t == "Enter an API key first."),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("in Settings")),
            "{texts:?}"
        );
    }

    #[test]
    fn settings_window_offers_connection_check() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        app.show_chat_settings = true;
        let mut output = None;
        for _ in 0..2 {
            output = Some(ctx.run(egui::RawInput::default(), |ctx| {
                app.chat_settings_window(ctx);
            }));
        }
        assert!(output.unwrap().shapes.iter().any(|shape| {
            matches!(
                &shape.shape,
                egui::Shape::Text(text) if text.galley.text() == "Check connection"
            )
        }));
    }

    #[test]
    fn missing_keys_are_only_allowed_for_loopback_hosts() {
        for base in [
            "https://localhost.example/v1",
            "https://127.0.0.1.example/v1",
            "https://example.test/://localhost",
        ] {
            let settings = ChatSettings {
                base_url: base.to_owned(),
                ..ChatSettings::default()
            };
            assert!(!settings.can_call_model(), "{base} is not a loopback host");
        }
        for base in [
            "http://localhost:8000/v1",
            "http://127.0.0.1:8000/v1/chat/completions",
            "http://[::1]:8000/v1",
        ] {
            let settings = ChatSettings {
                base_url: base.to_owned(),
                ..ChatSettings::default()
            };
            assert!(settings.can_call_model(), "{base} is a loopback host");
        }
    }

    fn queue_reply(app: &mut EditorApp) -> std::sync::mpsc::Sender<(u64, ChatWorkerResult)> {
        let (tx, rx) = channel();
        app.chat_pending = Some(PendingChat::for_test(
            "Edit the timeline",
            "Editing…",
            &app.timeline,
            app.script_state_json(),
        ));
        app.chat_rx = Some(rx);
        tx
    }

    fn deliver_reply(
        app: &mut EditorApp,
        ctx: &egui::Context,
        tx: &std::sync::mpsc::Sender<(u64, ChatWorkerResult)>,
        code: &str,
    ) {
        // A cancelled request may already have dropped its receiver.
        let _ = tx.send((
            1,
            ChatWorkerResult::Generated(GeneratedEdit {
                code: code.to_owned(),
                explanation: Some("Edited the timeline.".to_owned()),
            }),
        ));
        app.poll_chat(ctx);
    }

    #[test]
    fn a_reply_without_edits_shows_the_models_questions() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let tx = queue_reply(&mut app);
        deliver_reply(
            &mut app,
            &ctx,
            &tx,
            "console.log('The monologue has no transcript yet. Curate it first?'); \
             console.log('Overlays over a picture are not supported; use full-screen cutaways?'); \
             console.log('Done — waiting for your answers.');",
        );
        assert_eq!(app.status, "Edited the timeline.");
        assert_eq!(
            app.chat_session.messages.last().unwrap().text,
            "Edited the timeline.\nThe monologue has no transcript yet. Curate it first?\n\
             Overlays over a picture are not supported; use full-screen cutaways?"
        );
    }

    #[test]
    fn local_montage_skips_ranges_with_burned_in_text() {
        use super::super::sweep;
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let candidate = |source_in: f64, score: f32, tags: &[&str]| sweep::SweepCandidate {
            id: format!("ready.mp4#{source_in}"),
            asset_path: "ready.mp4".to_owned(),
            asset_name: "ready.mp4".to_owned(),
            title: "ready.mp4".to_owned(),
            source_in,
            duration: 2.0,
            score,
            reason: "test".to_owned(),
            stability: 0.9,
            audio_energy: 0.1,
            audio_peak: 0.1,
            category: "broll".to_owned(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            motion_score: 0.3,
            audio_score: 0.1,
            music_bed: false,
            visual: None,
            has_video: true,
            transcript: Vec::new(),
            review: sweep::ClipReview::default(),
            dismissed: false,
        };
        app.assets.push(super::super::media::BinAsset {
            info: openreel_media::MediaInfo {
                path: "ready.mp4".to_owned(),
                name: "ready.mp4".to_owned(),
                duration: 10.0,
                width: 320,
                height: 180,
                fps: 30.0,
                has_video: true,
                has_audio: false,
            },
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumb_textures: Vec::new(),
            sweep: sweep::SweepData {
                // The title card outranks the clean shot, so a missing filter places it first.
                candidates: vec![
                    candidate(0.0, 95.0, &["on-screen-text"]),
                    candidate(4.0, 60.0, &[]),
                ],
                ..Default::default()
            },
        });
        app.dispatch_intent(&ctx, "by-score");
        let placed: Vec<_> = app.timeline.clips.iter().map(|c| c.source_in).collect();
        assert_eq!(placed, vec![4.0]);
    }

    #[test]
    fn a_cutaway_moved_to_lane_zero_shows_over_the_base_clip() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        for name in ["talk.mp4", "broll.mp4"] {
            app.assets.push(super::super::media::BinAsset {
                info: openreel_media::MediaInfo {
                    path: name.to_owned(),
                    name: name.to_owned(),
                    duration: 20.0,
                    width: 320,
                    height: 180,
                    fps: 30.0,
                    has_video: true,
                    has_audio: true,
                },
                size_bytes: 0,
                state: AssetState::Ready,
                peaks: Vec::new(),
                beats: None,
                thumb_textures: Vec::new(),
                sweep: super::super::sweep::SweepData::default(),
            });
        }
        let tx = queue_reply(&mut app);
        deliver_reply(
            &mut app,
            &ctx,
            &tx,
            "var talk = addAssetRange('talk.mp4', 0, 0, 12); setClipLane(talk, 1); \
             var cut = addAssetRange('broll.mp4', 4, 2, 3); setClipLane(cut, 0); \
             setEmbeddedAudio(cut, false); console.log('Done — cut away.');",
        );
        let shown = |t: f64| app.timeline.active_video(t).map(|c| c.name.clone());
        assert_eq!(shown(2.0).as_deref(), Some("talk.mp4"));
        assert_eq!(shown(5.0).as_deref(), Some("broll.mp4"));
        assert_eq!(shown(8.0).as_deref(), Some("talk.mp4"));
        let cut = app
            .timeline
            .clips
            .iter()
            .find(|c| c.name == "broll.mp4")
            .unwrap();
        assert!(!cut.embedded_audio);
    }

    #[test]
    fn generated_additions_return_handles_for_later_commands() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        app.assets.push(super::super::media::BinAsset {
            info: openreel_media::MediaInfo {
                path: "ready.mp4".to_owned(),
                name: "ready.mp4".to_owned(),
                duration: 10.0,
                width: 320,
                height: 180,
                fps: 30.0,
                has_video: true,
                has_audio: true,
            },
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumb_textures: Vec::new(),
            sweep: super::super::sweep::SweepData::default(),
        });
        let keep = app
            .timeline
            .add_clip("Keep", ClipKind::Title, 0, 0.0, 1.0, 0.0);
        // A deleted clip makes "highest ID + 1" a wrong guess.
        let gone = app
            .timeline
            .add_clip("Gone", ClipKind::Title, 0, 1.0, 1.0, 0.0);
        app.timeline.remove_clip(gone);
        let tx = queue_reply(&mut app);
        deliver_reply(
            &mut app,
            &ctx,
            &tx,
            "var shot = addAssetRange('ready.mp4', 2, 0, 4); setFade(shot, 0, 1.5); \
             var card = addTitle('Card', 0, 1); moveClip(card, 6); \
             console.log('Done — placed.');",
        );
        assert_eq!(app.status, "Edited the timeline.");
        let shot = app
            .timeline
            .clips
            .iter()
            .find(|c| c.name == "ready.mp4")
            .unwrap();
        assert_eq!((shot.start, shot.fade_out), (2.0, 1.5));
        let card = app
            .timeline
            .clips
            .iter()
            .find(|c| c.name == "Card")
            .unwrap();
        assert_eq!(card.start, 6.0);
        assert_eq!(app.timeline.clip(keep).unwrap().start, 0.0);
    }

    #[test]
    fn failed_generated_batch_restores_the_entire_edit() {
        for (state, failing_command) in [
            (AssetState::Ready, "addAsset('missing.mp4', 0)"),
            (AssetState::Ready, "addAssetRange('ready.mp4', 0, 11, 1)"),
            (AssetState::Processing, "addAsset('ready.mp4', 0)"),
            (
                AssetState::Processing,
                "addAssetRange('ready.mp4', 0, 1, 2)",
            ),
            (AssetState::Ready, "moveClip(999, 4)"),
            (
                AssetState::Ready,
                "addTitle('Overflow', Number.MAX_VALUE, 2)",
            ),
        ] {
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            app.assets.push(super::super::media::BinAsset {
                info: openreel_media::MediaInfo {
                    path: "ready.mp4".to_owned(),
                    name: "ready.mp4".to_owned(),
                    duration: 10.0,
                    width: 320,
                    height: 180,
                    fps: 30.0,
                    has_video: true,
                    has_audio: false,
                },
                size_bytes: 0,
                state,
                peaks: Vec::new(),
                beats: None,
                thumb_textures: Vec::new(),
                sweep: super::super::sweep::SweepData::default(),
            });
            let id = app
                .timeline
                .add_clip("Keep", ClipKind::Title, 0, 0.0, 5.0, 0.0);
            app.selected = Some(id);
            app.playhead = 1.0;
            app.playing = true;
            let before = app.timeline.clone();
            let zoom = app.timeline_view.px_per_sec;
            let tx = queue_reply(&mut app);
            deliver_reply(
                &mut app,
                &ctx,
                &tx,
                &format!(
                    "deleteClip({id}); addTitle('Transient', 0, 2); pause(); seek(0); \
                     setMagnet(false); setMusicBedMuted(false); setZoom(123); {failing_command}"
                ),
            );
            assert_eq!(app.timeline, before, "{failing_command}");
            assert_eq!(app.selected, Some(id));
            assert_eq!(app.playhead, 1.0);
            assert!(app.playing);
            assert!(app.timeline_view.magnet && app.mute_music_beds);
            assert_eq!(app.timeline_view.px_per_sec, zoom);
            assert!(app.chat_undo.is_none());
            assert!(app.chat_pending.is_none());
            assert_ne!(app.status, "Edited the timeline.");
        }
    }

    #[test]
    fn generated_reply_cannot_delete_replacement_after_clear() {
        for console_clear in [false, true] {
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            let old = app
                .timeline
                .add_clip("Keep", ClipKind::Title, 0, 0.0, 5.0, 0.0);
            let before = app.timeline.clone();
            let tx = queue_reply(&mut app);
            if console_clear {
                app.run_console(&ctx, "clearTimeline()".to_owned());
            } else {
                app.handle_action(&ctx, super::super::UiAction::ClearTimeline);
            }
            let replacement = app
                .timeline
                .add_clip("Keep", ClipKind::Title, 0, 0.0, 5.0, 0.0);
            assert_eq!(old, replacement, "fixture exercises recycled IDs");
            assert_eq!(
                app.timeline, before,
                "cancellation must cover identical content too"
            );
            deliver_reply(&mut app, &ctx, &tx, &format!("deleteClip({old})"));
            assert_eq!(app.timeline.clip(replacement).unwrap().name, "Keep");
            assert!(app.chat_pending.is_none());
        }
    }

    #[test]
    fn generated_reply_rejects_manual_timeline_changes() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let id = app
            .timeline
            .add_clip("Keep", ClipKind::Title, 0, 0.0, 5.0, 0.0);
        let tx = queue_reply(&mut app);
        app.timeline.move_clip(id, 7.0);
        deliver_reply(&mut app, &ctx, &tx, &format!("deleteClip({id})"));
        assert_eq!(app.timeline.clip(id).unwrap().start, 7.0);
        assert!(app.chat_pending.is_none());
        assert!(app.chat_undo.is_none());
    }

    #[test]
    fn generated_defaults_use_the_requested_selection_and_playhead() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let first = app
            .timeline
            .add_clip("First", ClipKind::Title, 0, 0.0, 5.0, 0.0);
        let second = app
            .timeline
            .add_clip("Second", ClipKind::Title, 1, 0.0, 7.0, 0.0);
        app.selected = Some(first);
        app.playhead = 1.0;
        let tx = queue_reply(&mut app);
        app.selected = Some(second);
        app.playhead = 4.0;
        deliver_reply(&mut app, &ctx, &tx, "split()");
        assert_eq!(app.timeline.clip(first).unwrap().duration, 1.0);
        assert_eq!(app.timeline.clip(second).unwrap().duration, 7.0);
        assert_eq!(app.timeline.clips.len(), 3);
        app.undo_chat_edit();
        assert_eq!(app.timeline.clip(first).unwrap().duration, 5.0);
        assert_eq!(app.selected, Some(second));
        assert_eq!(app.playhead, 4.0);
    }

    #[test]
    fn generated_filesystem_commands_are_rejected_before_editing() {
        for operation in ["importMedia", "exportTo"] {
            let ctx = egui::Context::default();
            let mut app = EditorApp::headless_for_test(&ctx);
            let id = app
                .timeline
                .add_clip("Keep", ClipKind::Title, 0, 0.0, 5.0, 0.0);
            let before = app.timeline.clone();
            let path = std::env::temp_dir()
                .join(format!(
                    "openreel_denied_io_{}_{}",
                    std::process::id(),
                    operation
                ))
                .join("output.mp4");
            assert!(!path.exists());
            let tx = queue_reply(&mut app);
            deliver_reply(
                &mut app,
                &ctx,
                &tx,
                &format!(
                    "renameClip({id}, 'Changed'); {operation}({})",
                    serde_json::to_string(&path).unwrap(),
                ),
            );
            assert_eq!(app.timeline, before, "{operation}");
            assert!(app.assets.is_empty(), "model must not start an import");
            assert!(!path.exists(), "model must not write an export");
            assert!(app.status.contains("Import or Export"), "{}", app.status);
        }
    }

    #[test]
    fn successful_edit_is_undoable_after_a_log_only_reply() {
        let ctx = egui::Context::default();
        let mut app = EditorApp::headless_for_test(&ctx);
        let id = app
            .timeline
            .add_clip("Before", ClipKind::Title, 0, 0.0, 5.0, 0.0);
        let tx = queue_reply(&mut app);
        deliver_reply(&mut app, &ctx, &tx, &format!("renameClip({id}, 'After')"));
        assert_eq!(app.timeline.clip(id).unwrap().name, "After");
        let tx = queue_reply(&mut app);
        deliver_reply(
            &mut app,
            &ctx,
            &tx,
            &format!("deleteClip({id}); addAsset('missing.mp4', 0)"),
        );
        assert_eq!(app.timeline.clip(id).unwrap().name, "After");
        let tx = queue_reply(&mut app);
        deliver_reply(
            &mut app,
            &ctx,
            &tx,
            "console.log('No further edit needed.')",
        );
        app.undo_chat_edit();
        assert_eq!(app.timeline.clip(id).unwrap().name, "Before");
    }

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
        assert_eq!(body["model"], "muse-wonder-1.0");
        let wonder_settings = ChatSettings {
            model: MODEL_SPARK.to_owned(),
            contributor_opt_in: true,
            ..settings.clone()
        };
        let wonder_body = build_request_body(
            &wonder_settings,
            &session,
            &recent,
            "tighten it",
            "{\"clips\":[]}",
            None,
        );
        assert_eq!(wonder_body["model"], "muse-spark-1.3");
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
    fn model_context_keeps_every_range_and_full_repl_evidence_without_repeating_it() {
        let ranges: Vec<_> = (0..503)
            .map(|index| serde_json::json!({
                "id": format!("source.mp4#scene-{index}"), "asset": "source.mp4",
                "assetName": "source.mp4", "title": "Source scene",
                "sourceIn": index as f64 * 0.25, "duration": 0.25,
                "category": "action", "tags": ["subject-moving"], "score": 81,
                "quality": {"stability": 0.8, "audio": 0.6, "motion": 0.9},
                "transcript": "A repeated passage from the same speaker turn.",
                "review": {"status": "reviewed", "model": MODEL_SPARK, "frameTimes": [0.0, 0.2],
                    "subjectEvidence":[{"concept":"vehicle", "status":"ready", "sampledFrames":8,
                        "observedFrames":6, "firstObserved":0.25, "lastObserved":0.875,
                        "tracks":[{"requestLocalId":"12", "imagePlaneTravel":0.5}]}]},
                "visual": {"source": "sam", "samStatus": "ready", "facePresent": true,
                    "samFrameTime": 0.2, "samBoxCoverage": 0.4, "samConfidence": 0.9,
                    "subjectCoverage": 0.3, "edgeDensity": 0.17, "samMaskCoverage": null},
            }))
            .collect();
        let state = serde_json::json!({
            "clips": [], "sweepCandidates": ranges, "sceneClips": ranges,
            "transcriptSegments": [{"id": "turn-0", "asset": "source.mp4", "start": 0,
                "end": 125.75, "text": "A repeated passage from the same speaker turn.",
                "timingPrecision": "turn", "words": []}],
        })
        .to_string();
        let body = build_request_body(
            &ChatSettings::default(),
            &ChatSession::new("context-test"),
            &[],
            "Use all action clips",
            &state,
            None,
        );
        let message = body["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap();
        let compact: serde_json::Value =
            serde_json::from_str(message.split_once("Timeline state JSON:\n").unwrap().1).unwrap();
        for field in ["sweepCandidates", "sceneClips"] {
            assert_eq!(compact[field].as_array().unwrap().len(), 503);
            for (before, after) in ranges.iter().zip(compact[field].as_array().unwrap()) {
                for key in [
                    "id", "asset", "sourceIn", "duration", "category", "tags", "score", "quality",
                ] {
                    assert_eq!(after[key], before[key], "preserve {field}.{key}");
                }
                assert_eq!(after["review"]["status"], "reviewed");
                assert_eq!(after["review"]["subjectEvidence"][0]["concept"], "vehicle");
                assert_eq!(after["review"]["subjectEvidence"][0]["observedFrames"], 6);
                assert!(after["review"]["subjectEvidence"][0]
                    .get("tracks")
                    .is_none());
                assert_eq!(after["visual"]["facePresent"], true);
                assert!(
                    after.get("transcript").is_none(),
                    "one transcript source, not per range"
                );
                assert!(after["review"].get("frameTimes").is_none());
            }
        }
        assert_eq!(
            compact["transcriptSegments"][0]["text"],
            "A repeated passage from the same speaker turn."
        );
        let mut engine = crate::scripting::ScriptEngine::new().unwrap();
        let (_, commands) = engine.eval_checked(
            "if (sceneClips().length !== 503 || sweepCandidates()[502].visual.edgeDensity !== 0.17 || sweepCandidates()[0].review.frameTimes.length !== 2) throw Error('missing evidence'); addAssetRange(sceneClips()[502].asset, 0, sceneClips()[502].sourceIn, sceneClips()[502].duration);",
            &state,
        ).unwrap();
        assert!(
            matches!(&commands[..], [ScriptCommand::AddAssetRange { source_in, duration, .. }]
            if *source_in == 125.5 && *duration == 0.25)
        );
        println!("Full snapshot {} bytes; model snapshot {} bytes; 503 ranges in each inventory retained.", state.len(), compact.to_string().len());
    }

    #[test]
    fn oversized_context_is_rejected_before_starting_a_model_request() {
        with_isolated_config(
            "app::chat::tests::oversized_context_is_rejected_before_starting_a_model_request",
            |_| {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                listener.set_nonblocking(true).unwrap();
                let ctx = egui::Context::default();
                let mut app = EditorApp::headless_for_test(&ctx);
                app.chat_settings.base_url =
                    format!("http://{}/v1", listener.local_addr().unwrap());
                app.submit_chat_request(
                    &ctx,
                    "Oversized request".to_owned(),
                    "x".repeat(TOKEN_BUDGET * 4 + 100),
                    "Testing",
                );
                assert!(
                    app.chat_pending.is_none(),
                    "oversized context must not start a request"
                );
                assert!(app.chat_rx.is_none());
                assert!(app.status.contains("context budget"), "{}", app.status);
                assert_eq!(
                    listener.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                assert!(app.timeline.clips.is_empty());
            },
        );
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
        ensure_within_budget(&mut session, "turn 30: one more thing", state, None).unwrap();
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

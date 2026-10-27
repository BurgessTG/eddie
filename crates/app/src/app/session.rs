//! Chat session layer: persisted history, a pinned project summary, and
//! token-budget compaction.
//!
//! Adapted from Mitsuro (Jacob's Rust agent workspace,
//! github.com/honeycomb-Technologies/Mitsuro): the client/session state
//! store shape from `mitsuro-client-state` / `mitsuro-core`, and the
//! history-summarization policy from its compaction work — keep recent
//! turns verbatim, fold older turns into one summary, keep a stable
//! pinned prefix. Mitsuro's coding-agent loop (tool calls, approvals,
//! workspace semantics) is deliberately not carried over; OpenReel's
//! loop is the QuickJS console.
//!
//! The Meta Model API is stateless, so this module is the memory. The
//! request prefix stays byte-stable for automatic prompt caching:
//! developer contract, few-shot, pinned summary, recent history, and
//! only then the volatile timeline JSON (assembled in `chat.rs`).
//! Context assembly is app code; nothing here runs inside the sandbox.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// Whole-request token budget (estimated). The model window is
/// 1,048,576 tokens, so this is a cost budget, not a capacity limit:
/// past it, older turns are compacted instead of resent raw.
/// Raised 16k to 256k at Jacob's direction (2026-10-26): 16k compacted
/// far too early; prompt caching keeps the longer verbatim history cheap.
pub(crate) const TOKEN_BUDGET: usize = 256_000;
/// Messages always kept verbatim at the tail (roughly five exchanges,
/// plus the in-flight request). Compaction never touches these.
pub(crate) const RECENT_MESSAGES_KEPT_VERBATIM: usize = 10;
/// Cap on the compacted summary so summarizing cannot itself blow the
/// budget on a very long session.
const COMPACTED_SUMMARY_MAX_CHARS: usize = 3_200;
const COMPACTED_TURN_MAX_CHARS: usize = 140;
const PINNED_MAX_CANDIDATES: usize = 12;
const PINNED_MAX_TRANSCRIPT_SEGMENTS: usize = 24;
const PINNED_TRANSCRIPT_MAX_CHARS: usize = 120;
const DEFAULT_PROJECT_ID: &str = "openreel-project-1";

/// Rough token estimate (~4 chars/token). Good enough for a budget
/// gate; the API remains the only exact counter.
pub(crate) fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ChatRole {
    User,
    Assistant,
}

impl ChatRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChatMessage {
    pub(crate) role: ChatRole,
    pub(crate) text: String,
}

/// The persisted chat brain for one project. `messages` is the full
/// visible history; the model only ever sees
/// `messages[compacted_through..]` verbatim plus the two summaries.
#[derive(Clone, Debug)]
pub(crate) struct ChatSession {
    /// Stable per-project id. Doubles as `prompt_cache_key`.
    pub(crate) project_id: String,
    pub(crate) messages: Vec<ChatMessage>,
    /// Assets + sweep highlights + transcript digest. Refreshed only
    /// when `pinned_fingerprint` changes, never per turn.
    pub(crate) pinned_summary: String,
    pub(crate) pinned_fingerprint: String,
    /// Single-message summary of `messages[..compacted_through]`.
    pub(crate) compacted_summary: String,
    pub(crate) compacted_through: usize,
}

impl ChatSession {
    pub(crate) fn new(project_id: &str) -> Self {
        Self {
            project_id: project_id.to_owned(),
            messages: Vec::new(),
            pinned_summary: String::new(),
            pinned_fingerprint: String::new(),
            compacted_summary: String::new(),
            compacted_through: 0,
        }
    }

    pub(crate) fn push(&mut self, role: ChatRole, text: String) {
        self.messages.push(ChatMessage { role, text });
    }

    /// Turns still sent verbatim (everything not yet compacted).
    pub(crate) fn recent_messages(&self) -> &[ChatMessage] {
        &self.messages[self.compacted_through.min(self.messages.len())..]
    }

    /// Highest index compaction may reach: the verbatim tail,
    /// including an in-flight final user message, always survives.
    pub(crate) fn max_compacted_through(&self) -> usize {
        self.messages
            .len()
            .saturating_sub(RECENT_MESSAGES_KEPT_VERBATIM)
    }

    pub(crate) fn can_compact(&self) -> bool {
        self.compacted_through < self.max_compacted_through()
    }

    /// Fold one exchange (up to two messages) into the summary.
    /// Returns false when the verbatim tail would be touched.
    pub(crate) fn compact_oldest_exchange(&mut self) -> bool {
        if !self.can_compact() {
            return false;
        }
        let next = (self.compacted_through + 2).min(self.max_compacted_through());
        self.compacted_through = next.max(self.compacted_through + 1);
        self.rebuild_compacted_summary();
        true
    }

    /// Deterministic local summarization (no model call): one short
    /// line per compacted turn, newest kept when the cap bites. The
    /// originals stay in `messages` for the UI and for persistence;
    /// they are simply no longer resent to the model.
    pub(crate) fn rebuild_compacted_summary(&mut self) {
        if self.compacted_through == 0 {
            self.compacted_summary.clear();
            return;
        }
        let mut lines: Vec<String> = self.messages[..self.compacted_through]
            .iter()
            .map(|message| {
                let who = match message.role {
                    ChatRole::User => "User",
                    ChatRole::Assistant => "Assistant",
                };
                format!(
                    "- {who}: {}",
                    truncate_chars(&message.text, COMPACTED_TURN_MAX_CHARS)
                )
            })
            .collect();
        let mut summary = format!(
            "Summary of earlier conversation ({} turns compacted):\n{}",
            self.compacted_through,
            lines.join("\n")
        );
        while summary.chars().count() > COMPACTED_SUMMARY_MAX_CHARS && lines.len() > 1 {
            lines.remove(0);
            summary = format!(
                "Summary of earlier conversation ({} turns compacted; oldest omitted):\n{}",
                self.compacted_through,
                lines.join("\n")
            );
        }
        self.compacted_summary = summary;
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let kept: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

/// `m:ss` for pinned digests.
fn minute_second(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

// -- pinned project summary ---------------------------------------------------
//
// Plain input views so this module stays decoupled from the sweep and
// media-bin types; `chat.rs` converts. The same fields feed the
// fingerprint, so the summary is rebuilt exactly when its inputs move.

pub(crate) struct PinnedAsset<'a> {
    pub name: &'a str,
    pub path: &'a str,
    pub duration: f64,
    pub has_video: bool,
    pub has_audio: bool,
    pub ready: bool,
}

pub(crate) struct PinnedCandidate<'a> {
    pub id: &'a str,
    pub asset_name: &'a str,
    pub source_in: f64,
    pub duration: f64,
    pub score: f32,
    pub stability: f32,
    pub audio_energy: f32,
    pub audio_peak: f32,
    pub is_reaction: bool,
    pub reason: &'a str,
    pub transcript: &'a str,
}

pub(crate) struct PinnedTranscriptSegment<'a> {
    pub id: &'a str,
    pub asset_name: &'a str,
    pub start: f64,
    pub end: f64,
    pub speaker: Option<&'a str>,
    pub text: &'a str,
}

/// Stable turn-level transcript id, shared by the pinned digest and
/// the `state()` JSON so both name the same segment.
pub(crate) fn transcript_segment_id(asset_path: &str, index: usize) -> String {
    format!("tx:{asset_path}#{index}")
}

pub(crate) fn pinned_fingerprint(
    assets: &[PinnedAsset<'_>],
    candidates: &[PinnedCandidate<'_>],
    segments: &[PinnedTranscriptSegment<'_>],
) -> String {
    let mut hasher = DefaultHasher::new();
    for asset in assets {
        asset.path.hash(&mut hasher);
        asset.name.hash(&mut hasher);
        asset.duration.to_bits().hash(&mut hasher);
        asset.has_video.hash(&mut hasher);
        asset.has_audio.hash(&mut hasher);
        asset.ready.hash(&mut hasher);
    }
    for candidate in candidates {
        candidate.id.hash(&mut hasher);
        candidate.source_in.to_bits().hash(&mut hasher);
        candidate.duration.to_bits().hash(&mut hasher);
        candidate.score.to_bits().hash(&mut hasher);
        candidate.stability.to_bits().hash(&mut hasher);
        candidate.audio_energy.to_bits().hash(&mut hasher);
        candidate.audio_peak.to_bits().hash(&mut hasher);
        candidate.is_reaction.hash(&mut hasher);
        candidate.reason.hash(&mut hasher);
        candidate.transcript.hash(&mut hasher);
    }
    for segment in segments {
        segment.id.hash(&mut hasher);
        segment.start.to_bits().hash(&mut hasher);
        segment.end.to_bits().hash(&mut hasher);
        segment.speaker.hash(&mut hasher);
        segment.text.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Build the pinned summary. Candidates arrive best-first and
/// transcripts in timeline order; both are capped so the pinned
/// prefix stays a fixed, cache-friendly size class.
pub(crate) fn build_pinned_summary(
    assets: &[PinnedAsset<'_>],
    candidates: &[PinnedCandidate<'_>],
    segments: &[PinnedTranscriptSegment<'_>],
) -> String {
    let mut out = String::from("Project summary (pinned):\nAssets:");
    if assets.is_empty() {
        out.push_str("\n- No media imported yet.");
    }
    for asset in assets {
        let kind = if asset.has_video {
            "video"
        } else if asset.has_audio {
            "audio"
        } else {
            "media"
        };
        out.push_str(&format!(
            "\n- {} ({}) {:.1}s {}{}",
            asset.name,
            asset.path,
            asset.duration,
            kind,
            if asset.ready { "" } else { ", importing" }
        ));
    }
    out.push_str("\nSweep highlights (best first; ids are sweepCandidates() ids):");
    if candidates.is_empty() {
        out.push_str("\n- No sweep candidates yet.");
    }
    for candidate in candidates.iter().take(PINNED_MAX_CANDIDATES) {
        out.push_str(&format!(
            "\n- [{}] {} {}–{} score {:.0} stability {:.0}% energy {:.0}% peak {:.0}%{} — {}",
            candidate.id,
            candidate.asset_name,
            minute_second(candidate.source_in),
            minute_second(candidate.source_in + candidate.duration),
            candidate.score,
            candidate.stability * 100.0,
            candidate.audio_energy * 100.0,
            candidate.audio_peak * 100.0,
            if candidate.is_reaction {
                " reaction"
            } else {
                ""
            },
            candidate.reason,
        ));
        if !candidate.transcript.trim().is_empty() {
            out.push_str(&format!(
                " Speech: “{}”",
                truncate_chars(candidate.transcript, PINNED_TRANSCRIPT_MAX_CHARS)
            ));
        }
    }
    out.push_str("\nTranscript digest (turn-level; ids are transcriptSegments() ids):");
    if segments.is_empty() {
        out.push_str("\n- No transcript yet.");
    }
    for segment in segments.iter().take(PINNED_MAX_TRANSCRIPT_SEGMENTS) {
        out.push_str(&format!(
            "\n- [{}] {} {}–{}{}: “{}”",
            segment.id,
            segment.asset_name,
            minute_second(segment.start),
            minute_second(segment.end),
            segment
                .speaker
                .map(|speaker| format!(" speaker {speaker}"))
                .unwrap_or_default(),
            truncate_chars(segment.text, PINNED_TRANSCRIPT_MAX_CHARS)
        ));
    }
    out
}

// -- persistence --------------------------------------------------------------

fn session_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|dir| {
            PathBuf::from(dir)
                .join("OpenReel")
                .join("chat_session.json")
        })
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home).join("Library/Application Support/OpenReel/chat_session.json")
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
            Some(PathBuf::from(dir).join("openreel/chat_session.json"))
        } else {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".config/openreel/chat_session.json"))
        }
    }
    #[cfg(not(any(unix, target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

impl ChatSession {
    /// Load the persisted session, or a fresh one. The session file
    /// sits with the app config because the app has a single open
    /// project; `project_id` inside it is the cache key.
    pub(crate) fn load() -> Self {
        match session_path() {
            Some(path) => Self::load_from(&path),
            None => Self::new(DEFAULT_PROJECT_ID),
        }
    }

    pub(crate) fn load_from(path: &std::path::Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::new(DEFAULT_PROJECT_ID);
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Self::new(DEFAULT_PROJECT_ID);
        };
        if value["version"].as_u64() != Some(1) {
            return Self::new(DEFAULT_PROJECT_ID);
        }
        let project_id = value["project_id"]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or(DEFAULT_PROJECT_ID)
            .to_owned();
        let mut session = Self::new(&project_id);
        if let Some(messages) = value["messages"].as_array() {
            session.messages = messages
                .iter()
                .filter_map(|message| {
                    let text = message["text"].as_str()?.to_owned();
                    let role = match message["role"].as_str() {
                        Some("assistant") => ChatRole::Assistant,
                        _ => ChatRole::User,
                    };
                    Some(ChatMessage { role, text })
                })
                .collect();
        }
        session.pinned_summary = value["pinned_summary"].as_str().unwrap_or("").to_owned();
        session.pinned_fingerprint = value["pinned_fingerprint"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        session.compacted_summary = value["compacted_summary"].as_str().unwrap_or("").to_owned();
        session.compacted_through =
            (value["compacted_through"].as_u64().unwrap_or(0) as usize).min(session.messages.len());
        if session.compacted_through > 0 && session.compacted_summary.trim().is_empty() {
            session.rebuild_compacted_summary();
        }
        session
    }

    pub(crate) fn save(&self) {
        if let Some(path) = session_path() {
            self.save_to(&path);
        }
    }

    pub(crate) fn save_to(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let messages: Vec<serde_json::Value> = self
            .messages
            .iter()
            .map(|message| serde_json::json!({"role": message.role.as_str(), "text": message.text}))
            .collect();
        let body = serde_json::json!({
            "version": 1,
            "project_id": self.project_id,
            "pinned_summary": self.pinned_summary,
            "pinned_fingerprint": self.pinned_fingerprint,
            "compacted_summary": self.compacted_summary,
            "compacted_through": self.compacted_through,
            "messages": messages,
        });
        if std::fs::write(path, body.to_string()).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with_turns(turns: usize, text_len: usize) -> ChatSession {
        let mut session = ChatSession::new("test-project");
        for turn in 0..turns {
            session.push(
                ChatRole::User,
                format!("turn {turn} request {}", "word ".repeat(text_len / 5)),
            );
            session.push(
                ChatRole::Assistant,
                format!("turn {turn} done {}", "done ".repeat(text_len / 5)),
            );
        }
        session
    }

    #[test]
    fn persistence_round_trip_restores_chat_and_context() {
        let mut session = session_with_turns(8, 40);
        session.pinned_summary = "pinned".to_owned();
        session.pinned_fingerprint = "abc".to_owned();
        session.compact_oldest_exchange();
        let path =
            std::env::temp_dir().join(format!("openreel-session-test-{}.json", std::process::id()));
        session.save_to(&path);
        let restored = ChatSession::load_from(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(restored.project_id, "test-project");
        assert_eq!(restored.messages, session.messages);
        assert_eq!(restored.pinned_summary, "pinned");
        assert_eq!(restored.pinned_fingerprint, "abc");
        assert_eq!(restored.compacted_through, session.compacted_through);
        assert_eq!(restored.compacted_summary, session.compacted_summary);
    }

    #[test]
    fn compaction_keeps_recent_verbatim_and_summarizes_early_turns() {
        let mut session = session_with_turns(15, 40);
        while session.compact_oldest_exchange() {}
        assert!(session.compacted_through > 0);
        assert_eq!(
            session.recent_messages().len(),
            RECENT_MESSAGES_KEPT_VERBATIM
        );
        // The newest turn survives verbatim; the oldest is only in the
        // summary now, truncated.
        let recent_text: Vec<&str> = session
            .recent_messages()
            .iter()
            .map(|m| m.text.as_str())
            .collect();
        assert!(recent_text.iter().any(|t| t.starts_with("turn 14 request")));
        assert!(!recent_text.iter().any(|t| t.starts_with("turn 0 request")));
        assert!(session.compacted_summary.contains("turns compacted"));
        assert!(session.compacted_summary.contains("turn 0"));
        assert!(
            session.compacted_summary.chars().count() <= COMPACTED_SUMMARY_MAX_CHARS + 200,
            "summary must stay capped"
        );
        // Full history is retained for the UI/persistence.
        assert_eq!(session.messages.len(), 30);
    }

    #[test]
    fn pinned_fingerprint_only_changes_with_its_inputs() {
        let assets = vec![PinnedAsset {
            name: "a.mp4",
            path: "/m/a.mp4",
            duration: 20.0,
            has_video: true,
            has_audio: true,
            ready: true,
        }];
        let candidates = vec![PinnedCandidate {
            id: "/m/a.mp4#0",
            asset_name: "a.mp4",
            source_in: 4.0,
            duration: 5.0,
            score: 90.0,
            stability: 0.9,
            audio_energy: 0.4,
            audio_peak: 0.8,
            is_reaction: true,
            reason: "Big reaction.",
            transcript: "Wow.",
        }];
        let segments = vec![PinnedTranscriptSegment {
            id: "tx:/m/a.mp4#0",
            asset_name: "a.mp4",
            start: 4.0,
            end: 6.0,
            speaker: Some("A"),
            text: "Pricing is simple.",
        }];
        let fp = pinned_fingerprint(&assets, &candidates, &segments);
        assert_eq!(fp, pinned_fingerprint(&assets, &candidates, &segments));
        let changed = vec![PinnedCandidate {
            score: 50.0,
            ..PinnedCandidate {
                id: "/m/a.mp4#0",
                asset_name: "a.mp4",
                source_in: 4.0,
                duration: 5.0,
                score: 90.0,
                stability: 0.9,
                audio_energy: 0.4,
                audio_peak: 0.8,
                is_reaction: true,
                reason: "Big reaction.",
                transcript: "Wow.",
            }
        }];
        assert_ne!(fp, pinned_fingerprint(&assets, &changed, &segments));
        let summary = build_pinned_summary(&assets, &candidates, &segments);
        assert!(summary.contains("[/m/a.mp4#0]"), "{summary}");
        assert!(summary.contains("reaction"), "{summary}");
        assert!(summary.contains("tx:/m/a.mp4#0"), "{summary}");
        assert!(summary.contains("Pricing is simple."), "{summary}");
    }

    #[test]
    fn token_estimate_is_roughly_four_chars_per_token() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }
}

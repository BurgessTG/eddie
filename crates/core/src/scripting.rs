//! In-app JavaScript control via rquickjs (QuickJS).
//!
//! A persistent context backs the REPL console; checked evals use a fresh
//! runtime and context. JS functions do not touch editor state directly.
//! They enqueue `ScriptCommand`s that the app applies on the UI thread
//! right after evaluation. Queries read a JSON snapshot (`state()`)
//! refreshed before every eval.
//!
//! Sandboxed: no module loader, no OS access, and a 32 MB memory cap.
//! A 500 ms cooperative deadline is checked by the VM and command callbacks;
//! it cannot preempt a single native operation.
//! Each eval can queue at most 2,048 commands and 1 MiB of owned string bytes.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use rquickjs::{function::Opt, CString, Context, Function, Runtime};

#[derive(Debug, PartialEq)]
pub enum ScriptCommand {
    Play,
    Pause,
    TogglePlay,
    Seek(f64),
    StepFrames(i64),
    SelectClip(u64),
    SplitClip {
        clip: u64,
        at: f64,
        /// Names the new right-hand piece for later commands in the run.
        handle: Option<u64>,
    },
    DeleteClip(u64),
    MoveClip {
        clip: u64,
        start: f64,
    },
    TrimClip {
        clip: u64,
        left: Option<f64>,
        right: Option<f64>,
    },
    RenameClip {
        clip: u64,
        name: String,
    },
    SetOpacity {
        clip: u64,
        value: f32,
    },
    SetVolume {
        clip: u64,
        value: f32,
    },
    SetCleanVoice {
        clip: u64,
        enabled: bool,
    },
    SetSmoothFades {
        clip: u64,
        enabled: bool,
    },
    SetFade {
        clip: u64,
        fade_in: f64,
        fade_out: f64,
    },
    DetachAudio {
        clip: u64,
    },
    UnlinkClip {
        clip: u64,
    },
    SetClipLane {
        clip: u64,
        lane: u64,
    },
    SetEmbeddedAudio {
        clip: u64,
        enabled: bool,
    },
    SetDuck {
        clip: u64,
        enabled: bool,
    },
    InsertTime {
        at: f64,
        seconds: f64,
    },
    TightenSilence {
        clip: u64,
        /// `None` is the source's own level (`silence_threshold`).
        threshold: Option<f32>,
        min_silence: f64,
        padding: f64,
    },
    AddTitle {
        text: String,
        at: f64,
        duration: f64,
        style: String,
        handle: Option<u64>,
    },
    SetTitleStyle {
        clip: u64,
        style: String,
    },
    AddCaptionsForClip {
        clip: u64,
        style: String,
    },
    AddAsset {
        asset: String,
        at: f64,
        handle: Option<u64>,
    },
    AddAssetRange {
        asset: String,
        at: f64,
        source_in: f64,
        duration: f64,
        handle: Option<u64>,
    },
    Import(String),
    Export(String),
    ClearTimeline,
    SetMagnet(bool),
    SetMusicBedMuted(bool),
    SetZoom(f32),
    CloseGaps,
    Log(String),
}

/// First handle that `addAsset`, `addAssetRange` and `addTitle` return.
/// Handles stand for clips a batch has not created yet; real clip IDs
/// never reach this range.
pub const CLIP_HANDLE_BASE: u64 = 1 << 40;

impl ScriptCommand {
    /// The clip this command targets, if any.
    pub fn target_mut(&mut self) -> Option<&mut u64> {
        match self {
            Self::SelectClip(id) | Self::DeleteClip(id) => Some(id),
            Self::SplitClip { clip, .. }
            | Self::MoveClip { clip, .. }
            | Self::TrimClip { clip, .. }
            | Self::RenameClip { clip, .. }
            | Self::SetOpacity { clip, .. }
            | Self::SetVolume { clip, .. }
            | Self::SetFade { clip, .. }
            | Self::SetCleanVoice { clip, .. }
            | Self::SetSmoothFades { clip, .. }
            | Self::DetachAudio { clip }
            | Self::UnlinkClip { clip }
            | Self::SetClipLane { clip, .. }
            | Self::SetEmbeddedAudio { clip, .. }
            | Self::SetDuck { clip, .. }
            | Self::TightenSilence { clip, .. }
            | Self::SetTitleStyle { clip, .. }
            | Self::AddCaptionsForClip { clip, .. } => Some(clip),
            _ => None,
        }
    }

    /// Replace a handle returned earlier in this batch with the clip its
    /// addition created. `created` maps handles to clip IDs.
    pub fn resolve_handle(
        &mut self,
        created: &std::collections::HashMap<u64, u64>,
    ) -> Result<(), String> {
        if let Some(id) = self.target_mut().filter(|id| **id >= CLIP_HANDLE_BASE) {
            *id = *created
                .get(id)
                .ok_or("that clip handle is not from an earlier addition in this run")?;
        }
        Ok(())
    }

    /// Validate numeric inputs without changing finite values or clamping policy.
    pub fn validate(&self) -> Result<(), String> {
        // A title's name is drawn by FFmpeg, whose arguments end at a NUL.
        if let Self::AddTitle { text, .. } | Self::RenameClip { name: text, .. } = self {
            if text.chars().any(|c| c.is_control() && !c.is_whitespace()) {
                return Err("clip names and title text cannot hold control characters".to_owned());
            }
        }
        let finite = match self {
            Self::Seek(value)
            | Self::SplitClip { at: value, .. }
            | Self::MoveClip { start: value, .. }
            | Self::AddAsset { at: value, .. } => value.is_finite(),
            Self::SetFade {
                fade_in, fade_out, ..
            } => fade_in.is_finite() && fade_out.is_finite(),
            Self::InsertTime { at, seconds } => at.is_finite() && seconds.is_finite(),
            Self::TrimClip { left, right, .. } => {
                left.iter().chain(right).all(|value| value.is_finite())
            }
            Self::SetOpacity { value, .. }
            | Self::SetVolume { value, .. }
            | Self::SetZoom(value) => value.is_finite(),
            Self::TightenSilence {
                threshold,
                min_silence,
                padding,
                ..
            } => {
                threshold.is_none_or(f32::is_finite)
                    && min_silence.is_finite()
                    && padding.is_finite()
            }
            Self::AddTitle { at, duration, .. } => at.is_finite() && duration.is_finite(),
            Self::AddAssetRange {
                at,
                source_in,
                duration,
                ..
            } => at.is_finite() && source_in.is_finite() && duration.is_finite(),
            Self::Play
            | Self::Pause
            | Self::TogglePlay
            | Self::StepFrames(_)
            | Self::SelectClip(_)
            | Self::DeleteClip(_)
            | Self::RenameClip { .. }
            | Self::DetachAudio { .. }
            | Self::UnlinkClip { .. }
            | Self::SetClipLane { .. }
            | Self::SetEmbeddedAudio { .. }
            | Self::SetDuck { .. }
            | Self::SetCleanVoice { .. }
            | Self::SetSmoothFades { .. }
            | Self::SetTitleStyle { .. }
            | Self::AddCaptionsForClip { .. }
            | Self::Import(_)
            | Self::Export(_)
            | Self::ClearTimeline
            | Self::SetMagnet(_)
            | Self::SetMusicBedMuted(_)
            | Self::CloseGaps
            | Self::Log(_) => true,
        };
        if finite {
            Ok(())
        } else {
            Err("command numeric arguments must be finite".to_owned())
        }
    }
}

const MAX_COMMANDS: usize = 2048;
const MAX_COMMAND_STRING_BYTES: usize = 1024 * 1024;
const EVAL_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Default)]
struct CommandQueue {
    commands: Vec<ScriptCommand>,
    string_bytes: usize,
    error: Option<String>,
    deadline: Option<Instant>,
    /// Never reused within one engine, so a REPL variable holding an old
    /// handle cannot alias a later batch's addition.
    next_handle: u64,
    /// The script looked at `state().audio`.
    read_audio: bool,
}

impl CommandQueue {
    fn check_active(&mut self) -> rquickjs::Result<()> {
        if let Some(error) = &self.error {
            return Err(rquickjs::Error::new_from_js_message(
                "JavaScript",
                "ScriptCommand",
                error.clone(),
            ));
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(self.reject(format!(
                "evaluation time limit exceeded ({} ms)",
                EVAL_TIMEOUT.as_millis()
            )));
        }
        Ok(())
    }

    fn push(
        &mut self,
        string_bytes: usize,
        command: impl FnOnce() -> ScriptCommand,
    ) -> rquickjs::Result<()> {
        self.check_active()?;
        if self.commands.len() >= MAX_COMMANDS {
            return Err(self.reject(format!(
                "command limit exceeded (maximum {MAX_COMMANDS} per eval)"
            )));
        }
        if string_bytes > MAX_COMMAND_STRING_BYTES - self.string_bytes {
            return Err(self.reject(format!(
                "command string byte limit exceeded (maximum {MAX_COMMAND_STRING_BYTES} per eval)"
            )));
        }
        // String arguments borrow QuickJS-owned UTF-8 until these checks pass,
        // so even a rejected single large argument is never copied into Rust.
        let mut command = command();
        if let Err(error) = command.validate() {
            return Err(self.reject(error));
        }
        // Validate raw edges first: NaN and -Infinity must not become None.
        if let ScriptCommand::TrimClip { left, right, .. } = &mut command {
            *left = left.filter(|value| *value >= 0.0);
            *right = right.filter(|value| *value >= 0.0);
        }
        self.string_bytes += string_bytes;
        self.commands.push(command);
        Ok(())
    }

    /// Queue an addition and return the handle later commands can target.
    fn push_addition(
        &mut self,
        string_bytes: usize,
        command: impl FnOnce(Option<u64>) -> ScriptCommand,
    ) -> rquickjs::Result<f64> {
        let handle = CLIP_HANDLE_BASE.max(self.next_handle);
        self.push(string_bytes, || command(Some(handle)))?;
        self.next_handle = handle + 1;
        Ok(handle as f64)
    }

    fn reject(&mut self, error: String) -> rquickjs::Error {
        self.commands.clear();
        // Sticky until the eval ends, even if JavaScript catches the exception.
        self.error = Some(error.clone());
        rquickjs::Error::new_from_js_message("JavaScript", "ScriptCommand", error)
    }
}

type Queue = Rc<RefCell<CommandQueue>>;

/// A clip ID, handle or lane from JavaScript. rquickjs casts numbers with
/// `as`, which made `deleteClip(1.9)` delete clip 1 and `NaN` name clip 0.
struct Whole(u64);

impl<'js> rquickjs::FromJs<'js> for Whole {
    fn from_js(_: &rquickjs::Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<Self> {
        // Every whole number below 2^53 is exact in a double.
        const LIMIT: f64 = 9_007_199_254_740_992.0;
        match value.as_number() {
            Some(number) if (0.0..LIMIT).contains(&number) && number.fract() == 0.0 => {
                Ok(Self(number as u64))
            }
            _ => Err(rquickjs::Error::new_from_js_message(
                value.type_name(),
                "whole number",
                "clip IDs and lanes are whole numbers, 0 or more",
            )),
        }
    }
}

pub struct ScriptEngine {
    context: Context,
    queue: Queue,
    next_handle: u64,
    read_audio: bool,
    /// The last eval's value was a promise.
    returned_promise: bool,
}

const PRELUDE: &str = r#"
function state() {
  const s = JSON.parse(__state_json);
  if ("audio" in s) {
    const audio = s.audio;
    Object.defineProperty(s, "audio", { enumerable: true, get() { __audio_read(); return audio; } });
  }
  return s;
}
function clips() { return state().clips; }
function assets() { return state().assets; }
function sweepCandidates() { return state().sweepCandidates || []; }
function sceneClips() { return state().sceneClips || []; }
function transcriptSegments() { return state().transcriptSegments || []; }
function inspect() { return state().inspection || []; }
function audioReport() { return state().audio || { status: "silent" }; }
function __asset(nameOrPath) {
  const wanted = String(nameOrPath);
  const all = assets();
  const byPath = all.find((x) => x.path === wanted);
  if (byPath) return byPath;
  const named = all.filter((x) => x.name === wanted);
  if (named.length > 1) throw new Error(named.length + " assets are named '" + wanted + "'; use the path");
  if (!named.length) throw new Error("no asset " + nameOrPath);
  return named[0];
}
function utterances(nameOrPath) { return __asset(nameOrPath).utterances || []; }
function snapToSpeech(nameOrPath, start, end, handle) {
  const a = __asset(nameOrPath);
  const u = a.utterances || [];
  const h = handle === undefined ? 0.12 : handle;
  let s = start, e = end;
  for (let i = 0; i < u.length; i++) {
    const before = i > 0 ? u[i - 1].end : 0;
    const after = i + 1 < u.length ? u[i + 1].start : a.duration;
    if (u[i].start - h < s && s < u[i].end - 0.05) s = Math.max(u[i].start - h, before, 0);
    if (u[i].start + 0.05 < e && e < u[i].end + h) e = Math.min(u[i].end + h, after, a.duration);
  }
  return { start: s, end: e };
}
function help() {
  return [
    "state() clips() assets() sweepCandidates() sceneClips() transcriptSegments() inspect() audioReport()",
    "utterances(asset)  // [{start,end,text}] speech that should not be cut inside",
    "snapToSpeech(asset, start, end, handle?)  // {start,end} widened out of any utterance, 0.12s handles",
    "Times: seconds, or a string such as '3.2s' '120f' '1:05.5' '50%' 'end' 'clip:7@end' 'clip:7@1.5s' (request-time clips); time(spec) timeRange('a..b' | 'clip:7')",
    "play() pause() toggle() seek(t) step(nFrames)",
    "selectClip(id) split(at?) splitClip(id, at) deleteClip(id?)",
    "moveClip(id, start) setClipLane(id, lane)",
    "trimClip(id, left, right)  // the clip's new start and end as timeline times; null or -1 keeps an edge",
    "renameClip(id, name) setOpacity(id, 0..1) setVolume(id, gain)  // linear gain 0..2; 0.5 is about 6 dB quieter",
    "setFade(id, inSec, outSec) setDuck(id, bool) setCleanVoice(id, bool) setSmoothFades(id, bool)",
    "insertTime(at, seconds)  // splits clips crossing at and moves everything after later",
    "detachAudio(id) unlinkClip(id) setEmbeddedAudio(id, bool)",
    "tightenSilence(id, threshold?, minSilence?, padding?)  // removes pauses of minSilence (0.35 s) or more, keeping padding (0.08 s); no threshold uses the recording's own quiet level",
    "addTitle(text, at?, dur?, style?)  // defaults: playhead, 4 s, 'lower-third'",
    "setTitleStyle(id, style) addCaptionsForClip(id, style?)  // styles: lower-third caption pop-caption minimal-caption highlight-caption; captions default to 'caption'",
    "addAsset(nameOrPath, at?) addAssetRange(nameOrPath, at, sourceIn, dur)  // lands on the first lane free for its span",
    "  // adds and splits return a handle for the new clip (a split's is its right piece); pass it to later calls in the same run",
    "importMedia(path) exportTo(path) clearTimeline()",
    "setMagnet(bool) setMusicBedMuted(bool) setZoom(pxPerSec) closeGaps()",
    "return reports back as `output` (JSON text); console.log lines and command reports land in `notes`. The in-app console keeps variables between lines; each eddie run starts fresh."
  ].join("\n");
}
var console = { log: (...a) => __log(a.map(String).join(" ")) };
function __resolve(spec, range) {
  const s = state();
  const clips = (s.clips || []).flatMap((c) => [c.id, c.start, c.start + c.duration]);
  return __resolve_time(String(spec), s.fps, s.duration, clips, range);
}
function time(spec) { return typeof spec === "number" ? spec : __resolve(spec, false)[0]; }
function timeRange(spec) { const r = __resolve(spec, true); return { start: r[0], end: r[1] }; }
function __t(v) { return typeof v === "string" ? time(v) : v; }
function seek(t) { __seek(__t(t)); }
function moveClip(id, start) { __move_clip(id, __t(start)); }
function insertTime(at, seconds) { __insert_time(__t(at), seconds); }
function __edge(v) { return v === undefined || v === null ? -1 : __t(v); }
function trimClip(id, left, right) { __trim_clip(id, __edge(left), __edge(right)); }
function split(at) {
  const s = state();
  if (s.selected == null) throw new Error("no clip selected");
  return __split_clip(s.selected, at === undefined ? s.playhead : __t(at));
}
function splitClip(id, at) { return __split_clip(id, __t(at)); }
function deleteClip(id) {
  if (id === undefined) {
    id = state().selected;
    if (id == null) throw new Error("no clip selected");
  }
  __delete_clip(id);
}
function addTitle(text, at, dur, style) {
  return __add_title(String(text), at === undefined ? state().playhead : __t(at), dur === undefined ? 4 : dur, style === undefined ? "lower-third" : String(style));
}
function addCaptionsForClip(id, style) {
  __add_captions_for_clip(id, style === undefined ? "caption" : String(style));
}
function addAsset(nameOrPath, at) {
  return __add_asset(String(nameOrPath), at === undefined ? state().playhead : __t(at));
}
function addAssetRange(nameOrPath, at, sourceIn, dur) {
  return __add_asset_range(String(nameOrPath), __t(at), sourceIn, dur);
}
function tightenSilence(id, threshold, minSilence, padding) {
  const rest = [
    id,
    minSilence === undefined ? 0.35 : minSilence,
    padding === undefined ? 0.08 : padding,
  ];
  if (threshold === undefined || threshold === null) __tighten_silence(...rest);
  else __tighten_silence(...rest, threshold);
}
"#;

impl ScriptEngine {
    pub fn new() -> Result<Self, String> {
        let runtime = Runtime::new().map_err(|e| e.to_string())?;
        runtime.set_memory_limit(32 * 1024 * 1024);
        runtime.set_max_stack_size(256 * 1024);
        let queue: Queue = Rc::new(RefCell::new(CommandQueue::default()));
        let handler_queue = Rc::clone(&queue);
        runtime.set_interrupt_handler(Some(Box::new(move || {
            handler_queue
                .borrow()
                .deadline
                .map(|d| Instant::now() >= d)
                .unwrap_or(false)
        })));
        let context = Context::full(&runtime).map_err(|e| e.to_string())?;

        context
            .with(|ctx| -> rquickjs::Result<()> {
                let globals = ctx.globals();
                macro_rules! reg {
                    ($name:expr, $f:expr) => {
                        globals.set($name, Function::new(ctx.clone(), $f)?)?
                    };
                }
                let q = Rc::clone(&queue);
                reg!("__log", move |msg: CString<'_>| q
                    .borrow_mut()
                    .push(msg.len(), || ScriptCommand::Log(
                        msg.as_str().to_owned()
                    )));
                let q = Rc::clone(&queue);
                reg!("__audio_read", move || q.borrow_mut().read_audio = true);
                let q = Rc::clone(&queue);
                reg!("play", move || q
                    .borrow_mut()
                    .push(0, || ScriptCommand::Play));
                let q = Rc::clone(&queue);
                reg!("pause", move || q
                    .borrow_mut()
                    .push(0, || ScriptCommand::Pause));
                let q = Rc::clone(&queue);
                reg!("toggle", move || q
                    .borrow_mut()
                    .push(0, || ScriptCommand::TogglePlay));
                let q = Rc::clone(&queue);
                reg!("__seek", move |t: f64| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::Seek(t)));
                let q = Rc::clone(&queue);
                reg!("step", move |n: i64| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::StepFrames(n)));
                let q = Rc::clone(&queue);
                reg!("selectClip", move |Whole(id): Whole| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SelectClip(id)));
                let q = Rc::clone(&queue);
                reg!("__split_clip", move |Whole(clip): Whole, at: f64| q
                    .borrow_mut()
                    .push_addition(0, |handle| ScriptCommand::SplitClip {
                        clip,
                        at,
                        handle
                    }));
                let q = Rc::clone(&queue);
                reg!("__delete_clip", move |Whole(clip): Whole| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::DeleteClip(clip)));
                let q = Rc::clone(&queue);
                reg!("__move_clip", move |Whole(clip): Whole, start: f64| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::MoveClip { clip, start }));
                let q = Rc::clone(&queue);
                reg!(
                    "__trim_clip",
                    move |Whole(clip): Whole, left: f64, right: f64| q.borrow_mut().push(0, || {
                        ScriptCommand::TrimClip {
                            clip,
                            left: Some(left),
                            right: Some(right),
                        }
                    })
                );
                let q = Rc::clone(&queue);
                reg!(
                    "renameClip",
                    move |Whole(clip): Whole, name: CString<'_>| q.borrow_mut().push(
                        name.len(),
                        || ScriptCommand::RenameClip {
                            clip,
                            name: name.as_str().to_owned(),
                        }
                    )
                );
                let q = Rc::clone(&queue);
                reg!("setOpacity", move |Whole(clip): Whole, value: f32| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SetOpacity { clip, value }));
                let q = Rc::clone(&queue);
                reg!("setVolume", move |Whole(clip): Whole, value: f32| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SetVolume { clip, value }));
                let q = Rc::clone(&queue);
                reg!("setCleanVoice", move |Whole(clip): Whole, enabled: bool| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SetCleanVoice { clip, enabled }));
                let q = Rc::clone(&queue);
                reg!(
                    "setSmoothFades",
                    move |Whole(clip): Whole, enabled: bool| q
                        .borrow_mut()
                        .push(0, || ScriptCommand::SetSmoothFades { clip, enabled })
                );
                let q = Rc::clone(&queue);
                reg!(
                    "setFade",
                    move |Whole(clip): Whole, fade_in: f64, fade_out: f64| q.borrow_mut().push(
                        0,
                        || ScriptCommand::SetFade {
                            clip,
                            fade_in,
                            fade_out
                        }
                    )
                );
                let q = Rc::clone(&queue);
                reg!("detachAudio", move |Whole(clip): Whole| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::DetachAudio { clip }));
                let q = Rc::clone(&queue);
                reg!("unlinkClip", move |Whole(clip): Whole| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::UnlinkClip { clip }));
                let q = Rc::clone(&queue);
                reg!(
                    "setClipLane",
                    move |Whole(clip): Whole, Whole(lane): Whole| q
                        .borrow_mut()
                        .push(0, || ScriptCommand::SetClipLane { clip, lane })
                );
                let q = Rc::clone(&queue);
                reg!(
                    "setEmbeddedAudio",
                    move |Whole(clip): Whole, enabled: bool| q
                        .borrow_mut()
                        .push(0, || ScriptCommand::SetEmbeddedAudio { clip, enabled })
                );
                let q = Rc::clone(&queue);
                reg!("setDuck", move |Whole(clip): Whole, enabled: bool| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SetDuck { clip, enabled }));
                let q = Rc::clone(&queue);
                reg!("__insert_time", move |at: f64, seconds: f64| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::InsertTime { at, seconds }));
                let q = Rc::clone(&queue);
                reg!(
                    "__tighten_silence",
                    move |Whole(clip): Whole,
                          min_silence: f64,
                          padding: f64,
                          threshold: Opt<f32>| q.borrow_mut().push(0, || {
                        ScriptCommand::TightenSilence {
                            clip,
                            threshold: threshold.0,
                            min_silence,
                            padding,
                        }
                    })
                );
                let q = Rc::clone(&queue);
                reg!(
                    "__add_title",
                    move |text: CString<'_>, at: f64, duration: f64, style: CString<'_>| q
                        .borrow_mut()
                        .push_addition(text.len().saturating_add(style.len()), |handle| {
                            ScriptCommand::AddTitle {
                                text: text.as_str().to_owned(),
                                at,
                                duration,
                                style: style.as_str().to_owned(),
                                handle,
                            }
                        })
                );
                let q = Rc::clone(&queue);
                reg!(
                    "setTitleStyle",
                    move |Whole(clip): Whole, style: CString<'_>| q.borrow_mut().push(
                        style.len(),
                        || ScriptCommand::SetTitleStyle {
                            clip,
                            style: style.as_str().to_owned(),
                        }
                    )
                );
                let q = Rc::clone(&queue);
                reg!(
                    "__add_captions_for_clip",
                    move |Whole(clip): Whole, style: CString<'_>| q.borrow_mut().push(
                        style.len(),
                        || {
                            ScriptCommand::AddCaptionsForClip {
                                clip,
                                style: style.as_str().to_owned(),
                            }
                        }
                    )
                );
                let q = Rc::clone(&queue);
                reg!("__add_asset", move |asset: CString<'_>, at: f64| q
                    .borrow_mut()
                    .push_addition(asset.len(), |handle| {
                        ScriptCommand::AddAsset {
                            asset: asset.as_str().to_owned(),
                            at,
                            handle,
                        }
                    }));
                let q = Rc::clone(&queue);
                reg!(
                    "__add_asset_range",
                    move |asset: CString<'_>, at: f64, source_in: f64, duration: f64| q
                        .borrow_mut()
                        .push_addition(asset.len(), |handle| ScriptCommand::AddAssetRange {
                            asset: asset.as_str().to_owned(),
                            at,
                            source_in,
                            duration,
                            handle,
                        })
                );
                let q = Rc::clone(&queue);
                reg!("importMedia", move |path: CString<'_>| q
                    .borrow_mut()
                    .push(path.len(), || ScriptCommand::Import(
                        path.as_str().to_owned()
                    )));
                let q = Rc::clone(&queue);
                reg!("exportTo", move |path: CString<'_>| q
                    .borrow_mut()
                    .push(path.len(), || ScriptCommand::Export(
                        path.as_str().to_owned()
                    )));
                let q = Rc::clone(&queue);
                reg!("clearTimeline", move || q
                    .borrow_mut()
                    .push(0, || ScriptCommand::ClearTimeline));
                let q = Rc::clone(&queue);
                reg!("setMagnet", move |on: bool| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SetMagnet(on)));
                let q = Rc::clone(&queue);
                reg!("setMusicBedMuted", move |on: bool| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SetMusicBedMuted(on)));
                let q = Rc::clone(&queue);
                reg!("setZoom", move |px: f32| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SetZoom(px)));
                let q = Rc::clone(&queue);
                reg!("closeGaps", move || q
                    .borrow_mut()
                    .push(0, || ScriptCommand::CloseGaps));

                reg!(
                    "__resolve_time",
                    |ctx: rquickjs::Ctx<'_>,
                     spec: String,
                     fps: f64,
                     end: f64,
                     clips: Vec<f64>,
                     range: bool| {
                        let index = openreel_engine::time_spec::TimeIndex {
                            fps,
                            end,
                            clips: clips
                                .as_chunks::<3>()
                                .0
                                .iter()
                                .map(|clip| (clip[0] as u64, clip[1], clip[2]))
                                .collect(),
                        };
                        let resolved = if range {
                            index.range(&spec).map(|(start, end)| vec![start, end])
                        } else {
                            index.time(&spec).map(|time| vec![time])
                        };
                        resolved.map_err(|error| rquickjs::Exception::throw_message(&ctx, &error))
                    }
                );

                globals.set("__state_json", "{}")?;
                ctx.eval::<(), _>(PRELUDE)?;
                Ok(())
            })
            .map_err(|e: rquickjs::Error| e.to_string())?;

        Ok(Self {
            context,
            queue,
            next_handle: CLIP_HANDLE_BASE,
            read_audio: false,
            returned_promise: false,
        })
    }

    /// Whether the last evaluation looked at the measured mix
    /// (`audioReport()`, `state().audio`). Measuring takes a moment, so a
    /// caller that had no measurement yet can make one and evaluate again.
    pub fn read_audio(&self) -> bool {
        self.read_audio
    }

    /// Evaluate one REPL line. Returns display text and queued commands.
    pub fn eval(&mut self, code: &str, state_json: &str) -> (String, Vec<ScriptCommand>) {
        let (output, commands, _) = self.eval_inner(code, state_json);
        (output, commands)
    }

    /// Evaluate for `eddie run` in a fresh runtime and context. Global assignments
    /// and prototype changes cannot affect the REPL or later requests.
    /// Keep the function wrapper for top-level declarations and `return`.
    /// On a JavaScript error the queued commands are discarded, so
    /// a rejected script makes no edit at all.
    pub fn eval_checked(
        &mut self,
        code: &str,
        state_json: &str,
    ) -> Result<(String, Vec<ScriptCommand>), String> {
        let mut engine = Self::new()?;
        let isolated = format!("(() => {{\n{code}\n}})()");
        let (output, commands, success) = engine.eval_inner(&isolated, state_json);
        self.read_audio = engine.read_audio;
        if !success {
            return Err(output);
        }
        // Nothing runs a promise's continuation, so the commands after an
        // `await` or inside a `then` would be dropped and the rest applied.
        if engine.returned_promise || engine.context.runtime().is_job_pending() {
            return Err(
                "error: scripts run synchronously; remove async, await and promise callbacks"
                    .to_owned(),
            );
        }
        Ok((output, commands))
    }

    fn eval_inner(&mut self, code: &str, state_json: &str) -> (String, Vec<ScriptCommand>, bool) {
        *self.queue.borrow_mut() = CommandQueue {
            deadline: Some(Instant::now() + EVAL_TIMEOUT),
            next_handle: self.next_handle,
            ..CommandQueue::default()
        };
        let state_json = state_json.to_owned();
        let mut returned_promise = false;
        let (output, success) = self.context.with(|ctx| -> (String, bool) {
            if ctx.globals().set("__state_json", state_json).is_err() {
                return ("error: could not refresh state".to_owned(), false);
            }
            match ctx.eval::<rquickjs::Value, _>(code) {
                Ok(value) => {
                    returned_promise = value.is_promise();
                    let _ = ctx.globals().set("__last", value);
                    let text = ctx
                        .eval::<String, _>(
                            "typeof __last === 'string' ? __last \
                             : (__last instanceof Error ? String(__last.stack || __last) \
                             : (JSON.stringify(__last) ?? String(__last)))",
                        )
                        .unwrap_or_else(|_| "undefined".to_owned());
                    (text, true)
                }
                Err(_) => {
                    let caught = ctx.catch();
                    let _ = ctx.globals().set("__last", caught);
                    let text = ctx
                        .eval::<String, _>(
                            "String(__last) + (__last && __last.stack ? '\\n' + __last.stack : '')",
                        )
                        .map(|s| format!("error: {s}"))
                        .unwrap_or_else(|_| "error".to_owned());
                    (text, false)
                }
            }
        });
        // Native built-ins and result formatting may run between VM polls.
        // Reject an expired eval before exposing commands, even without a final callback.
        let _ = self.queue.borrow_mut().check_active();
        let queue = std::mem::take(&mut *self.queue.borrow_mut());
        self.next_handle = queue.next_handle;
        self.read_audio = queue.read_audio;
        self.returned_promise = returned_promise;
        if let Some(error) = queue.error {
            (format!("error: {error}"), Vec::new(), false)
        } else {
            (output, queue.commands, success)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_queues_commands_and_persists_variables() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (out, _) = engine.eval("var answer = 40", r#"{"selected": 3, "playhead": 1.5}"#);
        assert_eq!(out, "undefined"); // declaration completion value
        let (out, commands) = engine.eval(
            "seek(2.5); split(); answer + 2",
            r#"{"selected": 3, "playhead": 1.5}"#,
        );
        assert_eq!(out, "42");
        assert_eq!(
            commands,
            vec![
                ScriptCommand::Seek(2.5),
                ScriptCommand::SplitClip {
                    clip: 3,
                    at: 1.5,
                    handle: Some(CLIP_HANDLE_BASE),
                },
            ]
        );
    }

    #[test]
    fn add_asset_range_queues_subclip() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (_, commands) = engine.eval(
            "addAssetRange('a.mp4', 12, 3.5, 4)",
            r#"{"sweepCandidates": []}"#,
        );
        assert_eq!(
            commands,
            vec![ScriptCommand::AddAssetRange {
                asset: "a.mp4".to_owned(),
                at: 12.0,
                source_in: 3.5,
                duration: 4.0,
                handle: Some(CLIP_HANDLE_BASE),
            }]
        );
    }

    #[test]
    fn music_bed_mute_policy_queues_command() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (_, commands) = engine.eval("setMusicBedMuted(false)", "{}");
        assert_eq!(commands, vec![ScriptCommand::SetMusicBedMuted(false)]);
    }

    #[test]
    fn detach_link_commands_queue_in_order() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (_, commands) = engine.eval(
            "detachAudio(4); unlinkClip(4); setEmbeddedAudio(4, false)",
            "{}",
        );
        assert_eq!(
            commands,
            vec![
                ScriptCommand::DetachAudio { clip: 4 },
                ScriptCommand::UnlinkClip { clip: 4 },
                ScriptCommand::SetEmbeddedAudio {
                    clip: 4,
                    enabled: false,
                },
            ]
        );
    }

    #[test]
    fn tighten_silence_queues_defaults_and_custom_thresholds() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (_, commands) = engine.eval(
            "tightenSilence(9); tightenSilence(10, 0.05, 0.5, 0.12)",
            "{}",
        );
        assert_eq!(
            commands,
            vec![
                ScriptCommand::TightenSilence {
                    clip: 9,
                    threshold: None,
                    min_silence: 0.35,
                    padding: 0.08,
                },
                ScriptCommand::TightenSilence {
                    clip: 10,
                    threshold: Some(0.05),
                    min_silence: 0.5,
                    padding: 0.12,
                },
            ]
        );
    }

    #[test]
    fn title_and_caption_commands_queue_styles() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (_, commands) = engine.eval(
            "addTitle('Intro', 1, 2, 'pop-caption'); setTitleStyle(8, 'minimal-caption'); addCaptionsForClip(3); addCaptionsForClip(4, 'caption')",
            "{}",
        );
        assert_eq!(
            commands,
            vec![
                ScriptCommand::AddTitle {
                    text: "Intro".to_owned(),
                    at: 1.0,
                    duration: 2.0,
                    style: "pop-caption".to_owned(),
                    handle: Some(CLIP_HANDLE_BASE),
                },
                ScriptCommand::SetTitleStyle {
                    clip: 8,
                    style: "minimal-caption".to_owned(),
                },
                ScriptCommand::AddCaptionsForClip {
                    clip: 3,
                    style: "caption".to_owned(),
                },
                ScriptCommand::AddCaptionsForClip {
                    clip: 4,
                    style: "caption".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn time_specs_resolve_against_the_request_time_clips() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let state = r#"{"fps":30,"duration":20,"playhead":0,"selected":7,
            "clips":[{"id":7,"start":10,"duration":4},{"id":9,"start":14,"duration":6}]}"#;
        let (out, commands) = engine
            .eval_checked(
                "seek('clip:7@end'); moveClip(9, '0:01.5'); splitClip(7, 'clip:7@50%');
                 addTitle('Hi', '25%', 1); seek(2); split('90f'); insertTime('clip:9', 2);
                 trimClip(7, 'clip:7@1s', null); trimClip(9, -1, 18);
                 const r = timeRange('clip:7..clip:9');
                 try { seek('clip:8'); } catch (e) { console.log(String(e)); }
                 return [time('end'), r.start, r.end].join(',');",
                state,
            )
            .expect("valid specs run");
        assert_eq!(out, "20,10,20");
        assert_eq!(
            commands,
            vec![
                ScriptCommand::Seek(14.0),
                ScriptCommand::MoveClip {
                    clip: 9,
                    start: 1.5
                },
                ScriptCommand::SplitClip {
                    clip: 7,
                    at: 12.0,
                    handle: Some(CLIP_HANDLE_BASE),
                },
                ScriptCommand::AddTitle {
                    text: "Hi".to_owned(),
                    at: 5.0,
                    duration: 1.0,
                    style: "lower-third".to_owned(),
                    handle: Some(CLIP_HANDLE_BASE + 1),
                },
                ScriptCommand::Seek(2.0),
                ScriptCommand::SplitClip {
                    clip: 7,
                    at: 3.0,
                    handle: Some(CLIP_HANDLE_BASE + 2),
                },
                ScriptCommand::InsertTime {
                    at: 14.0,
                    seconds: 2.0
                },
                // A trim edge is a timeline time too; null or -1 keeps one.
                ScriptCommand::TrimClip {
                    clip: 7,
                    left: Some(11.0),
                    right: None,
                },
                ScriptCommand::TrimClip {
                    clip: 9,
                    left: None,
                    right: Some(18.0),
                },
                ScriptCommand::Log("Error: no clip 8; clips: 7, 9".to_owned()),
            ]
        );

        // An uncaught bad spec rejects the whole run, as any thrown error does.
        let error = engine
            .eval_checked("seek(1); seek('soonish');", state)
            .expect_err("bad spec");
        assert!(error.contains("can not parse \"soonish\""), "{error}");
    }

    #[test]
    fn scene_clips_read_complete_breakdown_from_state() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let state = r#"{"sceneClips":[{"id":"clip.mp4#scene-0","sourceIn":0,"duration":2},{"id":"clip.mp4#scene-1","sourceIn":2,"duration":3}]}"#;
        let (out, _) = engine.eval(
            "sceneClips().map(c => c.id + ':' + c.sourceIn + '+' + c.duration).join(',')",
            state,
        );
        assert_eq!(out, "clip.mp4#scene-0:0+2,clip.mp4#scene-1:2+3");
    }

    #[test]
    fn repl_handles_are_never_reused_across_runs() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (first, _) = engine.eval("var a = addTitle('A', 0, 1); a", "{}");
        let (_, mut commands) = engine.eval("addTitle('B', 0, 1); setFade(a, 0, 1)", "{}");
        assert_eq!(first, CLIP_HANDLE_BASE.to_string());
        assert_eq!(
            commands[0],
            ScriptCommand::AddTitle {
                text: "B".to_owned(),
                at: 0.0,
                duration: 1.0,
                style: "lower-third".to_owned(),
                handle: Some(CLIP_HANDLE_BASE + 1),
            }
        );
        // The second run created only handle BASE + 1, so `a` cannot alias it.
        let created = std::collections::HashMap::from([(CLIP_HANDLE_BASE + 1, 5)]);
        assert!(commands[1].resolve_handle(&created).is_err());
    }

    #[test]
    fn transcript_segments_reads_state() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (out, commands) = engine.eval(
            "addAssetRange('a.mp4', 0, transcriptSegments()[0].start, transcriptSegments()[0].end - transcriptSegments()[0].start); transcriptSegments().length",
            r#"{"transcriptSegments": [{"id": "tx:a.mp4#0", "asset": "a.mp4", "start": 4, "end": 7.5, "text": "pricing"}]}"#,
        );
        assert_eq!(out, "1");
        assert_eq!(
            commands,
            vec![ScriptCommand::AddAssetRange {
                asset: "a.mp4".to_owned(),
                at: 0.0,
                source_in: 4.0,
                duration: 3.5,
                handle: Some(CLIP_HANDLE_BASE),
            }]
        );
    }

    #[test]
    fn eval_reports_errors_without_poisoning_context() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (out, _) = engine.eval("throw new Error('boom')", "{}");
        assert!(out.contains("boom"), "{out}");
        let (out, _) = engine.eval("1 + 1", "{}");
        assert_eq!(out, "2");
    }

    fn assert_rejected(engine: &mut ScriptEngine, code: &str, message: &str) {
        let code = format!("(() => {{\n{code}\n}})()");
        let state = r#"{"selected":1,"playhead":0}"#;
        let (output, commands) = engine.eval(&code, state);
        assert!(output.contains(message), "{code}: {output}");
        assert!(
            commands.is_empty(),
            "{code}: rejected eval returned {} commands",
            commands.len()
        );
        let result = engine.eval_checked(&format!("return {code};"), state);
        assert!(
            result.is_err(),
            "{code}: checked eval unexpectedly succeeded"
        );
        let error = result.unwrap_err();
        assert!(error.contains(message), "{code}: {error}");
    }

    #[test]
    fn command_budget_rejects_overflow_even_when_caught() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        assert_rejected(
            &mut engine,
            "for (let i = 0; i < 2049; i++) { try { play(); } catch (_) {} }",
            "command limit",
        );
        let (_, commands) = engine.eval("seek(2)", "{}");
        assert_eq!(commands, vec![ScriptCommand::Seek(2.0)]);
        let (_, commands) = engine.eval_checked("pause()", "{}").unwrap();
        assert_eq!(commands, vec![ScriptCommand::Pause]);
    }

    #[test]
    fn string_budget_covers_every_owned_command_field() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        for call in [
            "console.log(text)",
            "renameClip(1, text)",
            "addTitle(text, 0, 1, '')",
            "addTitle('', 0, 1, text)",
            "setTitleStyle(1, text)",
            "addCaptionsForClip(1, text)",
            "addAsset(text, 0)",
            "addAssetRange(text, 0, 0, 1)",
            "importMedia(text)",
            "exportTo(text)",
        ] {
            assert_rejected(
                &mut engine,
                &format!(
                    "play(); const text = 'x'.repeat(1024 * 1024 + 1); try {{ {call}; }} catch (_) {{}} pause();"
                ),
                "string byte limit",
            );
        }
    }

    #[test]
    fn string_budget_counts_combined_fields_and_commands_in_utf8_bytes() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        for code in [
            "const text = 'x'.repeat(512 * 1024); console.log(text); renameClip(1, text); try { renameClip(2, 'x'); } catch (_) {}",
            "const text = 'x'.repeat(512 * 1024); try { addTitle(text, 0, 1, text + 'x'); } catch (_) {}",
            "try { console.log('é'.repeat(512 * 1024 + 1)); } catch (_) {}",
        ] {
            assert_rejected(&mut engine, code, "string byte limit");
        }
    }

    #[test]
    fn command_and_string_budget_boundaries_reset_between_evals() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let code = "for (let i = 0; i < 2047; i++) play(); console.log('é'.repeat(512 * 1024));";
        for _ in 0..2 {
            let (output, commands) = engine.eval(code, "{}");
            assert_eq!(output, "undefined");
            assert_eq!(commands.len(), 2048);
            assert!(commands[..2047]
                .iter()
                .all(|command| *command == ScriptCommand::Play));
            assert!(
                matches!(commands.last(), Some(ScriptCommand::Log(text)) if text.len() == 1024 * 1024)
            );
            let (_, commands) = engine.eval_checked(code, "{}").unwrap();
            assert_eq!(commands.len(), 2048);
            assert!(
                matches!(commands.last(), Some(ScriptCommand::Log(text)) if text.len() == 1024 * 1024)
            );
        }
        assert_rejected(
            &mut engine,
            "for (let i = 0; i < 2049; i++) { try { play(); } catch (_) {} }",
            "command limit",
        );
        assert_rejected(
            &mut engine,
            "try { console.log('x'.repeat(1024 * 1024 + 1)); } catch (_) {}",
            "string byte limit",
        );
        let (output, commands) = engine.eval(code, "{}");
        assert_eq!(output, "undefined");
        assert_eq!(commands.len(), 2048);
        let (_, commands) = engine
            .eval_checked(
                "const text = 'x'.repeat(512 * 1024); addTitle(text, 0, 1, text);",
                "{}",
            )
            .unwrap();
        assert!(
            matches!(&commands[..], [ScriptCommand::AddTitle { text, style, .. }] if text.len() + style.len() == 1024 * 1024)
        );
    }

    #[test]
    fn command_budget_also_covers_result_formatting_callbacks() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        assert_rejected(
            &mut engine,
            "return { toJSON() { for (let i = 0; i < 2049; i++) { try { play(); } catch (_) {} } return 'done'; } };",
            "command limit",
        );
    }

    #[test]
    fn failed_checked_eval_cannot_poison_checked_or_console_globals() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        assert!(engine
            .eval_checked(
                "seek(1); globalThis.seek = () => {}; throw new Error('boom');",
                "{}",
            )
            .is_err());
        let (_, commands) = engine.eval_checked("seek(2)", "{}").unwrap();
        assert_eq!(commands, vec![ScriptCommand::Seek(2.0)]);
        let (_, commands) = engine.eval("seek(3)", "{}");
        assert_eq!(commands, vec![ScriptCommand::Seek(3.0)]);
    }

    #[test]
    fn successful_checked_eval_cannot_poison_globals_or_prototypes() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (_, commands) = engine
            .eval_checked(
                "globalThis.seek = () => {}; Object.prototype.poisoned = true; globalThis.__log = () => {};",
                "{}",
            )
            .unwrap();
        assert!(commands.is_empty());
        let code =
            "if (({}).poisoned) throw new Error('poisoned prototype'); seek(2); console.log('ok');";
        let (_, commands) = engine.eval_checked(code, "{}").unwrap();
        assert_eq!(
            commands,
            vec![
                ScriptCommand::Seek(2.0),
                ScriptCommand::Log("ok".to_owned())
            ]
        );
        let (output, commands) = engine.eval(code, "{}");
        assert_eq!(output, "undefined");
        assert_eq!(
            commands,
            vec![
                ScriptCommand::Seek(2.0),
                ScriptCommand::Log("ok".to_owned())
            ]
        );
    }

    #[test]
    fn checked_eval_does_not_inherit_console_globals_or_erase_variables() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (output, _) = engine.eval(
            "var answer = 40; let lexical = 2; globalThis.seek = () => {}; JSON.parse = () => { throw new Error('console JSON'); };",
            "{}",
        );
        assert!(!output.starts_with("error"), "{output}");
        for _ in 0..2 {
            let (output, commands) = engine
                .eval_checked(
                    "if (typeof answer !== 'undefined' || typeof lexical !== 'undefined') throw new Error('console variables leaked'); const s = state(); seek(s.playhead); return s.playhead;",
                    r#"{"playhead":4}"#,
                )
                .unwrap();
            assert_eq!(output, "4");
            assert_eq!(commands, vec![ScriptCommand::Seek(4.0)]);
        }
        let (output, commands) = engine.eval("answer + lexical", "{}");
        assert_eq!(output, "42");
        assert!(commands.is_empty());
        let (_, commands) = engine.eval("seek(1)", "{}");
        assert!(commands.is_empty(), "console's own globals must persist");
    }

    #[test]
    fn nonfinite_numeric_arguments_reject_the_entire_eval_even_when_caught() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        for call in [
            "seek(BAD)",
            "split(BAD)",
            "splitClip(1, BAD)",
            "moveClip(1, BAD)",
            "setOpacity(1, BAD)",
            "setVolume(1, BAD)",
            "setZoom(BAD)",
            "tightenSilence(1, BAD, 0.35, 0.08)",
            "tightenSilence(1, 0.03, BAD, 0.08)",
            "tightenSilence(1, 0.03, 0.35, BAD)",
            "addTitle('title', BAD, 1)",
            "addTitle('title', 0, BAD)",
            "addAsset('asset', BAD)",
            "addAssetRange('asset', BAD, 0, 1)",
            "addAssetRange('asset', 0, BAD, 1)",
            "addAssetRange('asset', 0, 0, BAD)",
        ] {
            for bad in ["NaN", "Infinity", "-Infinity"] {
                let call = call.replace("BAD", bad);
                assert_rejected(
                    &mut engine,
                    &format!("play(); try {{ {call}; }} catch (_) {{}} pause();"),
                    "finite",
                );
            }
        }
        let (_, commands) = engine.eval("seek(1)", "{}");
        assert_eq!(commands, vec![ScriptCommand::Seek(1.0)]);
    }

    #[test]
    fn trim_clip_rejects_nonfinite_edges_before_keep_sentinel_conversion() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        for bad in ["NaN", "Infinity", "-Infinity"] {
            for call in [
                format!("trimClip(1, {bad}, -1)"),
                format!("trimClip(1, -1, {bad})"),
            ] {
                assert_rejected(
                    &mut engine,
                    &format!("play(); try {{ {call}; }} catch (_) {{}} pause();"),
                    "finite",
                );
            }
        }
    }

    #[test]
    fn numeric_arguments_that_overflow_f32_are_rejected() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        for call in [
            "setOpacity(1, 1e100)",
            "setVolume(1, -1e100)",
            "setZoom(1e100)",
            "tightenSilence(1, 1e100)",
        ] {
            assert_rejected(&mut engine, call, "finite");
        }
    }

    #[test]
    fn command_validation_checks_every_nonfinite_field_without_javascript() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for command in [
                ScriptCommand::Seek(value),
                ScriptCommand::SplitClip {
                    clip: 1,
                    at: value,
                    handle: None,
                },
                ScriptCommand::MoveClip {
                    clip: 1,
                    start: value,
                },
                ScriptCommand::TrimClip {
                    clip: 1,
                    left: Some(value),
                    right: None,
                },
                ScriptCommand::TrimClip {
                    clip: 1,
                    left: None,
                    right: Some(value),
                },
                ScriptCommand::SetOpacity {
                    clip: 1,
                    value: value as f32,
                },
                ScriptCommand::SetVolume {
                    clip: 1,
                    value: value as f32,
                },
                ScriptCommand::SetZoom(value as f32),
                ScriptCommand::TightenSilence {
                    clip: 1,
                    threshold: Some(value as f32),
                    min_silence: 0.35,
                    padding: 0.08,
                },
                ScriptCommand::TightenSilence {
                    clip: 1,
                    threshold: Some(0.03),
                    min_silence: value,
                    padding: 0.08,
                },
                ScriptCommand::TightenSilence {
                    clip: 1,
                    threshold: Some(0.03),
                    min_silence: 0.35,
                    padding: value,
                },
                ScriptCommand::AddTitle {
                    text: String::new(),
                    at: value,
                    duration: 1.0,
                    style: String::new(),
                    handle: None,
                },
                ScriptCommand::AddTitle {
                    text: String::new(),
                    at: 0.0,
                    duration: value,
                    style: String::new(),
                    handle: None,
                },
                ScriptCommand::AddAsset {
                    asset: String::new(),
                    at: value,
                    handle: None,
                },
                ScriptCommand::AddAssetRange {
                    asset: String::new(),
                    at: value,
                    source_in: 0.0,
                    duration: 1.0,
                    handle: None,
                },
                ScriptCommand::AddAssetRange {
                    asset: String::new(),
                    at: 0.0,
                    source_in: value,
                    duration: 1.0,
                    handle: None,
                },
                ScriptCommand::AddAssetRange {
                    asset: String::new(),
                    at: 0.0,
                    source_in: 0.0,
                    duration: value,
                    handle: None,
                },
            ] {
                assert!(command.validate().is_err(), "{command:?}");
            }
        }
    }

    #[test]
    fn command_validation_leaves_finite_ranges_and_permissions_to_the_executor() {
        for command in [
            ScriptCommand::Seek(f64::MIN),
            ScriptCommand::Seek(f64::MAX),
            ScriptCommand::SetZoom(f32::MIN),
            ScriptCommand::SetZoom(f32::MAX),
            ScriptCommand::TrimClip {
                clip: 1,
                left: Some(-1.0),
                right: None,
            },
            ScriptCommand::StepFrames(i64::MIN),
            ScriptCommand::SelectClip(u64::MAX),
            ScriptCommand::Import("source.mp4".to_owned()),
            ScriptCommand::Export("output.mp4".to_owned()),
        ] {
            assert!(command.validate().is_ok(), "{command:?}");
        }
    }

    #[test]
    fn string_callbacks_preserve_utf8_nul_and_path_commands() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let code = r#"renameClip(1, 'café 🎬'); console.log('résumé\0', 3); importMedia('source.mp4'); exportTo('output.mp4');"#;
        let expected = vec![
            ScriptCommand::RenameClip {
                clip: 1,
                name: "café 🎬".to_owned(),
            },
            ScriptCommand::Log("résumé\0 3".to_owned()),
            ScriptCommand::Import("source.mp4".to_owned()),
            ScriptCommand::Export("output.mp4".to_owned()),
        ];
        let (output, commands) = engine.eval(code, "{}");
        assert_eq!(output, "undefined");
        assert_eq!(commands, expected);
        let (_, commands) = engine.eval_checked(code, "{}").unwrap();
        assert_eq!(commands, expected);
    }

    #[test]
    fn drawn_text_refuses_control_characters_but_keeps_line_breaks() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        for code in [
            r#"renameClip(1, 'nul\0tail')"#,
            r#"addTitle('bell\u0007', 0, 2)"#,
            r#"addTitle('escape\u001b[0m', 0, 2)"#,
        ] {
            let error = engine.eval_checked(code, "{}").unwrap_err();
            assert!(error.contains("control characters"), "{code}: {error}");
        }
        // Preview and export both draw a line break as a space.
        let (_, commands) = engine
            .eval_checked(r#"addTitle('two\nlines\there', 0, 2)"#, "{}")
            .unwrap();
        assert_eq!(commands.len(), 1);
    }

    #[test]
    fn clip_ids_and_lanes_must_be_whole_numbers() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let state = r#"{"selected": null, "playhead": 0}"#;
        for code in [
            "deleteClip(1.9)",
            "moveClip(2.7, 20)",
            "selectClip(NaN)",
            "deleteClip(-5)",
            "setClipLane(1, 0.5)",
            "setClipLane(1, -1)",
            "setOpacity(Infinity, 1)",
            "renameClip('1', 'name')",
            "detachAudio(2 ** 60)",
        ] {
            let error = engine.eval_checked(code, state).unwrap_err();
            assert!(error.contains("whole numbers"), "{code}: {error}");
        }
        // Nothing selected is not "delete nothing".
        let error = engine.eval_checked("deleteClip()", state).unwrap_err();
        assert!(error.contains("no clip selected"), "{error}");
        let (_, commands) = engine
            .eval_checked("deleteClip(7); setClipLane(7, 0); deleteClip(7.0)", state)
            .unwrap();
        assert_eq!(
            commands,
            vec![
                ScriptCommand::DeleteClip(7),
                ScriptCommand::SetClipLane { clip: 7, lane: 0 },
                ScriptCommand::DeleteClip(7),
            ]
        );
    }

    #[test]
    fn asynchronous_scripts_are_refused_whole() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        for code in [
            // Only the first title would have been queued.
            "return (async () => { addTitle('t1', 10, 1); await null; addTitle('t2', 12, 1); })()",
            // The callback would never have run.
            "Promise.resolve().then(() => addTitle('late', 10, 1)); return 'sync done'",
            "return Promise.resolve(3)",
        ] {
            let error = engine.eval_checked(code, "{}").unwrap_err();
            assert!(error.contains("synchronously"), "{code}: {error}");
        }
        // The next run is a fresh runtime with nothing pending.
        let (output, commands) = engine
            .eval_checked("addTitle('t', 0, 1); return 4", "{}")
            .unwrap();
        assert_eq!((output.as_str(), commands.len()), ("4", 1));
    }

    #[test]
    fn finite_negative_arguments_and_trim_sentinels_are_preserved() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (_, commands) = engine
            .eval_checked(
                "seek(-2); splitClip(1, -3); moveClip(1, -4); trimClip(1, -1, -2); trimClip(1, 0, 2); setOpacity(1, -0.5); setVolume(1, 2); setZoom(-1); tightenSilence(1, -0.1, -0.2, -0.3); addTitle('title', -1, -2); addAsset('asset', -1); addAssetRange('asset', -1, -2, -3);",
                "{}",
            )
            .unwrap();
        assert!(commands.iter().all(|command| command.validate().is_ok()));
        assert_eq!(
            commands,
            vec![
                ScriptCommand::Seek(-2.0),
                ScriptCommand::SplitClip {
                    clip: 1,
                    at: -3.0,
                    handle: Some(CLIP_HANDLE_BASE),
                },
                ScriptCommand::MoveClip {
                    clip: 1,
                    start: -4.0
                },
                ScriptCommand::TrimClip {
                    clip: 1,
                    left: None,
                    right: None
                },
                ScriptCommand::TrimClip {
                    clip: 1,
                    left: Some(0.0),
                    right: Some(2.0)
                },
                ScriptCommand::SetOpacity {
                    clip: 1,
                    value: -0.5
                },
                ScriptCommand::SetVolume {
                    clip: 1,
                    value: 2.0
                },
                ScriptCommand::SetZoom(-1.0),
                ScriptCommand::TightenSilence {
                    clip: 1,
                    threshold: Some(-0.1),
                    min_silence: -0.2,
                    padding: -0.3
                },
                ScriptCommand::AddTitle {
                    text: "title".to_owned(),
                    at: -1.0,
                    duration: -2.0,
                    style: "lower-third".to_owned(),
                    handle: Some(CLIP_HANDLE_BASE + 1),
                },
                ScriptCommand::AddAsset {
                    asset: "asset".to_owned(),
                    at: -1.0,
                    handle: Some(CLIP_HANDLE_BASE + 2),
                },
                ScriptCommand::AddAssetRange {
                    asset: "asset".to_owned(),
                    at: -1.0,
                    source_in: -2.0,
                    duration: -3.0,
                    handle: Some(CLIP_HANDLE_BASE + 3),
                },
            ]
        );
    }

    #[test]
    fn ordinary_javascript_errors_keep_console_semantics_but_reject_checked_edits() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let code = "seek(1); throw new Error('boom');";
        let (output, commands) = engine.eval(code, "{}");
        assert!(output.contains("boom"), "{output}");
        assert_eq!(commands, vec![ScriptCommand::Seek(1.0)]);
        assert!(engine.eval_checked(code, "{}").is_err());
        let (_, commands) = engine.eval("pause()", "{}");
        assert_eq!(commands, vec![ScriptCommand::Pause]);
    }

    #[test]
    fn eval_deadline_still_interrupts_and_recovers_in_both_contexts() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        assert!(engine.eval_checked("while (true) {}", "{}").is_err());
        let (_, commands) = engine.eval_checked("seek(1)", "{}").unwrap();
        assert_eq!(commands, vec![ScriptCommand::Seek(1.0)]);
        let (output, commands) = engine.eval("while (true) {}", "{}");
        assert!(output.starts_with("error"), "{output}");
        assert!(commands.is_empty());
        let (output, commands) = engine.eval("seek(2); 42", "{}");
        assert_eq!(output, "42");
        assert_eq!(commands, vec![ScriptCommand::Seek(2.0)]);
    }

    fn sequential_title_snapshot(count: usize) -> String {
        let clips: Vec<_> = (0..count)
            .map(|index| {
                serde_json::json!({
                    "id": index + 1,
                    "name": "seed",
                    "kind": "title",
                    "track": 0,
                    "start": index as f64,
                    "duration": 1.0,
                    "sourceIn": 0.0,
                    "source": null,
                    "opacity": 1.0,
                    "volume": 1.0,
                    "embeddedAudio": false,
                    "linkedClip": null,
                    "titleStyle": "lower-third",
                })
            })
            .collect();
        serde_json::json!({
            "playhead": 0.0,
            "playing": false,
            "selected": (count > 0).then_some(count),
            "duration": count as f64,
            "fps": 30.0,
            "magnet": true,
            "musicBedMuted": true,
            "clips": clips,
            "assets": [],
            "sweepCandidates": [],
            "sceneClips": [],
            "transcriptSegments": [],
        })
        .to_string()
    }

    #[test]
    fn explicit_command_batch_fits_the_budget_with_a_large_snapshot() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (_, commands) = engine
            .eval_checked(
                "clearTimeline(); for (let i = 0; i < 2047; i++) addTitle('Shot ' + i, i, 1);",
                &sequential_title_snapshot(500),
            )
            .expect("explicit edits must not exhaust their budget rereading the snapshot");
        let expected = std::iter::once(ScriptCommand::ClearTimeline)
            .chain((0..2047).map(|index| ScriptCommand::AddTitle {
                text: format!("Shot {index}"),
                at: index as f64,
                duration: 1.0,
                style: "lower-third".to_owned(),
                handle: Some(CLIP_HANDLE_BASE + index as u64),
            }))
            .collect::<Vec<_>>();
        assert_eq!(commands, expected);
    }

    #[test]
    fn command_defaults_use_saved_state_and_respect_explicit_arguments() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let (_, commands) = engine
            .eval_checked(
                "addTitle('explicit', 2, 1); addAsset('a.mp4', 3); deleteClip(7); addTitle('default'); addAsset('a.mp4'); deleteClip(); split(4);",
                r#"{"playhead":6.5,"selected":9}"#,
            )
            .unwrap();
        assert_eq!(
            commands,
            vec![
                ScriptCommand::AddTitle {
                    text: "explicit".to_owned(),
                    at: 2.0,
                    duration: 1.0,
                    style: "lower-third".to_owned(),
                    handle: Some(CLIP_HANDLE_BASE),
                },
                ScriptCommand::AddAsset {
                    asset: "a.mp4".to_owned(),
                    at: 3.0,
                    handle: Some(CLIP_HANDLE_BASE + 1),
                },
                ScriptCommand::DeleteClip(7),
                ScriptCommand::AddTitle {
                    text: "default".to_owned(),
                    at: 6.5,
                    duration: 4.0,
                    style: "lower-third".to_owned(),
                    handle: Some(CLIP_HANDLE_BASE + 2),
                },
                ScriptCommand::AddAsset {
                    asset: "a.mp4".to_owned(),
                    at: 6.5,
                    handle: Some(CLIP_HANDLE_BASE + 3),
                },
                ScriptCommand::DeleteClip(9),
                ScriptCommand::SplitClip {
                    clip: 9,
                    at: 4.0,
                    handle: Some(CLIP_HANDLE_BASE + 4),
                },
            ]
        );
    }

    #[test]
    fn command_callback_deadline_is_sticky_after_native_work() {
        let mut engine = ScriptEngine::new().expect("quickjs engine");
        let snapshot = sequential_title_snapshot(500);
        // JSON.parse does native work between VM interrupt polls. Expiration
        // must discard commands whether the VM or a caught callback notices it.
        let native_work = "play(); const until = Date.now() + 600; while (Date.now() < until) JSON.parse(__state_json);";
        let code = format!("{native_work} try {{ seek(2); }} catch (_) {{}}");
        let result = engine.eval_checked(&code, &snapshot);
        let error = result.expect_err("expired callback must reject the entire eval");
        assert!(error.contains("time limit"), "{error}");
        let (_, commands) = engine.eval_checked("seek(1)", "{}").unwrap();
        assert_eq!(commands, vec![ScriptCommand::Seek(1.0)]);

        let (output, commands) = engine.eval(&code, &snapshot);
        assert!(output.contains("time limit"), "{output}");
        assert!(commands.is_empty());
        let (_, commands) = engine.eval("seek(1)", "{}");
        assert_eq!(commands, vec![ScriptCommand::Seek(1.0)]);

        let error = engine.eval_checked(native_work, &snapshot).unwrap_err();
        assert!(error.contains("time limit"), "{error}");
    }
}

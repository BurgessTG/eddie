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

use rquickjs::{CString, Context, Function, Runtime};

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
    DetachAudio {
        clip: u64,
    },
    UnlinkClip {
        clip: u64,
    },
    SetEmbeddedAudio {
        clip: u64,
        enabled: bool,
    },
    TightenSilence {
        clip: u64,
        threshold: f32,
        min_silence: f64,
        padding: f64,
    },
    AddTitle {
        text: String,
        at: f64,
        duration: f64,
        style: String,
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
    },
    AddAssetRange {
        asset: String,
        at: f64,
        source_in: f64,
        duration: f64,
    },
    Import(String),
    Export(String),
    ClearTimeline,
    SetTool(String),
    SetMagnet(bool),
    SetMusicBedMuted(bool),
    SetZoom(f32),
    CloseGaps,
    Log(String),
}

impl ScriptCommand {
    /// Validate numeric inputs without changing finite values or clamping policy.
    pub(crate) fn validate(&self) -> Result<(), String> {
        let finite = match self {
            Self::Seek(value)
            | Self::SplitClip { at: value, .. }
            | Self::MoveClip { start: value, .. }
            | Self::AddAsset { at: value, .. } => value.is_finite(),
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
            } => threshold.is_finite() && min_silence.is_finite() && padding.is_finite(),
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
            | Self::SetEmbeddedAudio { .. }
            | Self::SetTitleStyle { .. }
            | Self::AddCaptionsForClip { .. }
            | Self::Import(_)
            | Self::Export(_)
            | Self::ClearTimeline
            | Self::SetTool(_)
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

    fn reject(&mut self, error: String) -> rquickjs::Error {
        self.commands.clear();
        // Sticky until the eval ends, even if JavaScript catches the exception.
        self.error = Some(error.clone());
        rquickjs::Error::new_from_js_message("JavaScript", "ScriptCommand", error)
    }
}

type Queue = Rc<RefCell<CommandQueue>>;

pub struct ScriptEngine {
    context: Context,
    queue: Queue,
}

const PRELUDE: &str = r#"
function state() { return JSON.parse(__state_json); }
function clips() { return state().clips; }
function assets() { return state().assets; }
function sweepCandidates() { return state().sweepCandidates || []; }
function sceneClips() { return state().sceneClips || []; }
function transcriptSegments() { return state().transcriptSegments || []; }
function help() {
  return [
    "state() clips() assets() sweepCandidates() sceneClips() transcriptSegments()",
    "play() pause() toggle() seek(t) step(nFrames)",
    "selectClip(id) split(at?) splitClip(id, at) deleteClip(id?)",
    "moveClip(id, start) trimClip(id, left, right)  // -1 keeps an edge",
    "renameClip(id, name) setOpacity(id, v) setVolume(id, v)",
    "detachAudio(id) unlinkClip(id) setEmbeddedAudio(id, bool) tightenSilence(id, threshold?, minSilence?, padding?)",
    "addTitle(text, at?, dur?, style?) setTitleStyle(id, style) addCaptionsForClip(id, style?) addAsset(nameOrPath, at?) addAssetRange(nameOrPath, at, sourceIn, dur)",
    "importMedia(path) exportTo(path) clearTimeline()",
    "setMagnet(bool) setMusicBedMuted(bool) setZoom(pxPerSec) closeGaps()",
    "console.log(...) works. Variables persist between lines."
  ].join("\n");
}
var console = { log: (...a) => __log(a.map(String).join(" ")) };
function split(at) {
  const s = state();
  if (s.selected == null) throw new Error("no clip selected");
  __split_clip(s.selected, at === undefined ? s.playhead : at);
}
function splitClip(id, at) { __split_clip(id, at); }
function deleteClip(id) {
  __delete_clip(id === undefined ? (state().selected ?? -1) : id);
}
function addTitle(text, at, dur, style) {
  __add_title(String(text), at === undefined ? state().playhead : at, dur === undefined ? 4 : dur, style === undefined ? "lower-third" : String(style));
}
function addCaptionsForClip(id, style) {
  __add_captions_for_clip(id, style === undefined ? "caption" : String(style));
}
function addAsset(nameOrPath, at) {
  __add_asset(String(nameOrPath), at === undefined ? state().playhead : at);
}
function addAssetRange(nameOrPath, at, sourceIn, dur) {
  __add_asset_range(String(nameOrPath), at, sourceIn, dur);
}
function tightenSilence(id, threshold, minSilence, padding) {
  __tighten_silence(
    id,
    threshold === undefined ? 0.03 : threshold,
    minSilence === undefined ? 0.35 : minSilence,
    padding === undefined ? 0.08 : padding
  );
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
                reg!("seek", move |t: f64| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::Seek(t)));
                let q = Rc::clone(&queue);
                reg!("step", move |n: i64| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::StepFrames(n)));
                let q = Rc::clone(&queue);
                reg!("selectClip", move |id: u64| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SelectClip(id)));
                let q = Rc::clone(&queue);
                reg!("__split_clip", move |clip: u64, at: f64| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SplitClip { clip, at }));
                let q = Rc::clone(&queue);
                reg!("__delete_clip", move |clip: i64| -> rquickjs::Result<()> {
                    if clip >= 0 {
                        q.borrow_mut()
                            .push(0, || ScriptCommand::DeleteClip(clip as u64))
                    } else {
                        q.borrow_mut().check_active()
                    }
                });
                let q = Rc::clone(&queue);
                reg!("moveClip", move |clip: u64, start: f64| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::MoveClip { clip, start }));
                let q = Rc::clone(&queue);
                reg!("trimClip", move |clip: u64, left: f64, right: f64| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::TrimClip {
                        clip,
                        left: Some(left),
                        right: Some(right),
                    }));
                let q = Rc::clone(&queue);
                reg!("renameClip", move |clip: u64, name: CString<'_>| q
                    .borrow_mut()
                    .push(name.len(), || ScriptCommand::RenameClip {
                        clip,
                        name: name.as_str().to_owned(),
                    }));
                let q = Rc::clone(&queue);
                reg!("setOpacity", move |clip: u64, value: f32| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SetOpacity { clip, value }));
                let q = Rc::clone(&queue);
                reg!("setVolume", move |clip: u64, value: f32| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SetVolume { clip, value }));
                let q = Rc::clone(&queue);
                reg!("detachAudio", move |clip: u64| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::DetachAudio { clip }));
                let q = Rc::clone(&queue);
                reg!("unlinkClip", move |clip: u64| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::UnlinkClip { clip }));
                let q = Rc::clone(&queue);
                reg!("setEmbeddedAudio", move |clip: u64, enabled: bool| q
                    .borrow_mut()
                    .push(0, || ScriptCommand::SetEmbeddedAudio {
                        clip,
                        enabled
                    }));
                let q = Rc::clone(&queue);
                reg!(
                    "__tighten_silence",
                    move |clip: u64, threshold: f32, min_silence: f64, padding: f64| q
                        .borrow_mut()
                        .push(0, || ScriptCommand::TightenSilence {
                            clip,
                            threshold,
                            min_silence,
                            padding,
                        })
                );
                let q = Rc::clone(&queue);
                reg!(
                    "__add_title",
                    move |text: CString<'_>, at: f64, duration: f64, style: CString<'_>| q
                        .borrow_mut()
                        .push(text.len().saturating_add(style.len()), || {
                            ScriptCommand::AddTitle {
                                text: text.as_str().to_owned(),
                                at,
                                duration,
                                style: style.as_str().to_owned(),
                            }
                        })
                );
                let q = Rc::clone(&queue);
                reg!("setTitleStyle", move |clip: u64, style: CString<'_>| q
                    .borrow_mut()
                    .push(style.len(), || ScriptCommand::SetTitleStyle {
                        clip,
                        style: style.as_str().to_owned(),
                    }));
                let q = Rc::clone(&queue);
                reg!(
                    "__add_captions_for_clip",
                    move |clip: u64, style: CString<'_>| q.borrow_mut().push(style.len(), || {
                        ScriptCommand::AddCaptionsForClip {
                            clip,
                            style: style.as_str().to_owned(),
                        }
                    })
                );
                let q = Rc::clone(&queue);
                reg!("__add_asset", move |asset: CString<'_>, at: f64| q
                    .borrow_mut()
                    .push(asset.len(), || ScriptCommand::AddAsset {
                        asset: asset.as_str().to_owned(),
                        at,
                    }));
                let q = Rc::clone(&queue);
                reg!(
                    "__add_asset_range",
                    move |asset: CString<'_>, at: f64, source_in: f64, duration: f64| q
                        .borrow_mut()
                        .push(asset.len(), || ScriptCommand::AddAssetRange {
                            asset: asset.as_str().to_owned(),
                            at,
                            source_in,
                            duration,
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
                reg!("setTool", move |tool: CString<'_>| q
                    .borrow_mut()
                    .push(tool.len(), || ScriptCommand::SetTool(
                        tool.as_str().to_owned()
                    )));
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

                globals.set("__state_json", "{}")?;
                ctx.eval::<(), _>(PRELUDE)?;
                Ok(())
            })
            .map_err(|e: rquickjs::Error| e.to_string())?;

        Ok(Self { context, queue })
    }

    /// Evaluate one REPL line. Returns display text and queued commands.
    pub fn eval(&mut self, code: &str, state_json: &str) -> (String, Vec<ScriptCommand>) {
        let (output, commands, _) = self.eval_inner(code, state_json);
        (output, commands)
    }

    /// Evaluate for chat in a fresh runtime and context. Global assignments
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
        if success {
            Ok((output, commands))
        } else {
            Err(output)
        }
    }

    fn eval_inner(&mut self, code: &str, state_json: &str) -> (String, Vec<ScriptCommand>, bool) {
        *self.queue.borrow_mut() = CommandQueue {
            deadline: Some(Instant::now() + EVAL_TIMEOUT),
            ..CommandQueue::default()
        };
        let state_json = state_json.to_owned();
        let (output, success) = self.context.with(|ctx| -> (String, bool) {
            if ctx.globals().set("__state_json", state_json).is_err() {
                return ("error: could not refresh state".to_owned(), false);
            }
            match ctx.eval::<rquickjs::Value, _>(code) {
                Ok(value) => {
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
                ScriptCommand::SplitClip { clip: 3, at: 1.5 },
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
                    threshold: 0.03,
                    min_silence: 0.35,
                    padding: 0.08,
                },
                ScriptCommand::TightenSilence {
                    clip: 10,
                    threshold: 0.05,
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
            "setTool(text)",
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
            "const text = 'x'.repeat(512 * 1024); console.log(text); renameClip(1, text); try { setTool('x'); } catch (_) {}",
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
                ScriptCommand::SplitClip { clip: 1, at: value },
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
                    threshold: value as f32,
                    min_silence: 0.35,
                    padding: 0.08,
                },
                ScriptCommand::TightenSilence {
                    clip: 1,
                    threshold: 0.03,
                    min_silence: value,
                    padding: 0.08,
                },
                ScriptCommand::TightenSilence {
                    clip: 1,
                    threshold: 0.03,
                    min_silence: 0.35,
                    padding: value,
                },
                ScriptCommand::AddTitle {
                    text: String::new(),
                    at: value,
                    duration: 1.0,
                    style: String::new(),
                },
                ScriptCommand::AddTitle {
                    text: String::new(),
                    at: 0.0,
                    duration: value,
                    style: String::new(),
                },
                ScriptCommand::AddAsset {
                    asset: String::new(),
                    at: value,
                },
                ScriptCommand::AddAssetRange {
                    asset: String::new(),
                    at: value,
                    source_in: 0.0,
                    duration: 1.0,
                },
                ScriptCommand::AddAssetRange {
                    asset: String::new(),
                    at: 0.0,
                    source_in: value,
                    duration: 1.0,
                },
                ScriptCommand::AddAssetRange {
                    asset: String::new(),
                    at: 0.0,
                    source_in: 0.0,
                    duration: value,
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
        let code = r#"renameClip(1, 'café\0🎬'); console.log('résumé', 3); importMedia('source.mp4'); exportTo('output.mp4'); setTool('select');"#;
        let expected = vec![
            ScriptCommand::RenameClip {
                clip: 1,
                name: "café\0🎬".to_owned(),
            },
            ScriptCommand::Log("résumé 3".to_owned()),
            ScriptCommand::Import("source.mp4".to_owned()),
            ScriptCommand::Export("output.mp4".to_owned()),
            ScriptCommand::SetTool("select".to_owned()),
        ];
        let (output, commands) = engine.eval(code, "{}");
        assert_eq!(output, "undefined");
        assert_eq!(commands, expected);
        let (_, commands) = engine.eval_checked(code, "{}").unwrap();
        assert_eq!(commands, expected);
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
                ScriptCommand::SplitClip { clip: 1, at: -3.0 },
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
                    threshold: -0.1,
                    min_silence: -0.2,
                    padding: -0.3
                },
                ScriptCommand::AddTitle {
                    text: "title".to_owned(),
                    at: -1.0,
                    duration: -2.0,
                    style: "lower-third".to_owned()
                },
                ScriptCommand::AddAsset {
                    asset: "asset".to_owned(),
                    at: -1.0
                },
                ScriptCommand::AddAssetRange {
                    asset: "asset".to_owned(),
                    at: -1.0,
                    source_in: -2.0,
                    duration: -3.0
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
            "tool": "select",
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
                    style: "lower-third".to_owned()
                },
                ScriptCommand::AddAsset {
                    asset: "a.mp4".to_owned(),
                    at: 3.0
                },
                ScriptCommand::DeleteClip(7),
                ScriptCommand::AddTitle {
                    text: "default".to_owned(),
                    at: 6.5,
                    duration: 4.0,
                    style: "lower-third".to_owned()
                },
                ScriptCommand::AddAsset {
                    asset: "a.mp4".to_owned(),
                    at: 6.5
                },
                ScriptCommand::DeleteClip(9),
                ScriptCommand::SplitClip { clip: 9, at: 4.0 },
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

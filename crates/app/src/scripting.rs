//! In-app JavaScript control via rquickjs (QuickJS).
//!
//! A persistent context backs the REPL console. JS functions do not touch
//! editor state directly. They enqueue `ScriptCommand`s that the app
//! applies on the UI thread right after evaluation. Queries read a JSON
//! snapshot (`state()`) refreshed before every eval.
//!
//! Sandboxed: no module loader, no OS access, 32 MB memory cap, and a
//! 500 ms interrupt deadline per eval so a runaway script cannot hang the UI.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use rquickjs::{Context, Function, Runtime};

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
    AddTitle {
        text: String,
        at: f64,
        duration: f64,
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
    LoadSample,
    Export(String),
    ClearTimeline,
    SetTool(String),
    SetMagnet(bool),
    SetZoom(f32),
    CloseGaps,
    Log(String),
}

type Queue = Rc<RefCell<Vec<ScriptCommand>>>;

pub struct ScriptEngine {
    context: Context,
    queue: Queue,
    deadline: Rc<RefCell<Option<Instant>>>,
}

const PRELUDE: &str = r#"
function state() { return JSON.parse(__state_json); }
function clips() { return state().clips; }
function assets() { return state().assets; }
function sweepCandidates() { return state().sweepCandidates || []; }
function transcriptSegments() { return state().transcriptSegments || []; }
function help() {
  return [
    "state() clips() assets() sweepCandidates() transcriptSegments()",
    "play() pause() toggle() seek(t) step(nFrames)",
    "selectClip(id) split(at?) splitClip(id, at) deleteClip(id?)",
    "moveClip(id, start) trimClip(id, left, right)  // -1 keeps an edge",
    "renameClip(id, name) setOpacity(id, v) setVolume(id, v)",
    "addTitle(text, at?, dur?) addAsset(nameOrPath, at?) addAssetRange(nameOrPath, at, sourceIn, dur)",
    "importMedia(path) loadSample() exportTo(path) clearTimeline()",
    "setTool('select'|'split') setMagnet(bool) setZoom(pxPerSec) closeGaps()",
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
  const s = state();
  __delete_clip(id === undefined ? (s.selected ?? -1) : id);
}
function addTitle(text, at, dur) {
  const s = state();
  __add_title(String(text), at === undefined ? s.playhead : at, dur === undefined ? 4 : dur);
}
function addAsset(nameOrPath, at) {
  __add_asset(String(nameOrPath), at === undefined ? state().playhead : at);
}
function addAssetRange(nameOrPath, at, sourceIn, dur) {
  __add_asset_range(String(nameOrPath), at, sourceIn, dur);
}
"#;

impl ScriptEngine {
    pub fn new() -> Result<Self, String> {
        let runtime = Runtime::new().map_err(|e| e.to_string())?;
        runtime.set_memory_limit(32 * 1024 * 1024);
        runtime.set_max_stack_size(256 * 1024);
        let deadline: Rc<RefCell<Option<Instant>>> = Rc::new(RefCell::new(None));
        let handler_deadline = Rc::clone(&deadline);
        runtime.set_interrupt_handler(Some(Box::new(move || {
            handler_deadline
                .borrow()
                .map(|d| Instant::now() >= d)
                .unwrap_or(false)
        })));
        let context = Context::full(&runtime).map_err(|e| e.to_string())?;
        let queue: Queue = Rc::new(RefCell::new(Vec::new()));

        context
            .with(|ctx| -> rquickjs::Result<()> {
                let globals = ctx.globals();
                macro_rules! reg {
                    ($name:expr, $f:expr) => {
                        globals.set($name, Function::new(ctx.clone(), $f)?)?
                    };
                }
                let q = Rc::clone(&queue);
                reg!("__log", move |msg: String| q
                    .borrow_mut()
                    .push(ScriptCommand::Log(msg)));
                let q = Rc::clone(&queue);
                reg!("play", move || q.borrow_mut().push(ScriptCommand::Play));
                let q = Rc::clone(&queue);
                reg!("pause", move || q.borrow_mut().push(ScriptCommand::Pause));
                let q = Rc::clone(&queue);
                reg!("toggle", move || q
                    .borrow_mut()
                    .push(ScriptCommand::TogglePlay));
                let q = Rc::clone(&queue);
                reg!("seek", move |t: f64| q
                    .borrow_mut()
                    .push(ScriptCommand::Seek(t)));
                let q = Rc::clone(&queue);
                reg!("step", move |n: i64| q
                    .borrow_mut()
                    .push(ScriptCommand::StepFrames(n)));
                let q = Rc::clone(&queue);
                reg!("selectClip", move |id: u64| q
                    .borrow_mut()
                    .push(ScriptCommand::SelectClip(id)));
                let q = Rc::clone(&queue);
                reg!("__split_clip", move |clip: u64, at: f64| q
                    .borrow_mut()
                    .push(ScriptCommand::SplitClip { clip, at }));
                let q = Rc::clone(&queue);
                reg!("__delete_clip", move |clip: i64| {
                    if clip >= 0 {
                        q.borrow_mut().push(ScriptCommand::DeleteClip(clip as u64));
                    }
                });
                let q = Rc::clone(&queue);
                reg!("moveClip", move |clip: u64, start: f64| q
                    .borrow_mut()
                    .push(ScriptCommand::MoveClip { clip, start }));
                let q = Rc::clone(&queue);
                reg!("trimClip", move |clip: u64, left: f64, right: f64| q
                    .borrow_mut()
                    .push(ScriptCommand::TrimClip {
                        clip,
                        left: (left >= 0.0).then_some(left),
                        right: (right >= 0.0).then_some(right),
                    }));
                let q = Rc::clone(&queue);
                reg!("renameClip", move |clip: u64, name: String| q
                    .borrow_mut()
                    .push(ScriptCommand::RenameClip { clip, name }));
                let q = Rc::clone(&queue);
                reg!("setOpacity", move |clip: u64, value: f32| q
                    .borrow_mut()
                    .push(ScriptCommand::SetOpacity { clip, value }));
                let q = Rc::clone(&queue);
                reg!("setVolume", move |clip: u64, value: f32| q
                    .borrow_mut()
                    .push(ScriptCommand::SetVolume { clip, value }));
                let q = Rc::clone(&queue);
                reg!(
                    "__add_title",
                    move |text: String, at: f64, duration: f64| q
                        .borrow_mut()
                        .push(ScriptCommand::AddTitle { text, at, duration })
                );
                let q = Rc::clone(&queue);
                reg!("__add_asset", move |asset: String, at: f64| q
                    .borrow_mut()
                    .push(ScriptCommand::AddAsset { asset, at }));
                let q = Rc::clone(&queue);
                reg!(
                    "__add_asset_range",
                    move |asset: String, at: f64, source_in: f64, duration: f64| q
                        .borrow_mut()
                        .push(ScriptCommand::AddAssetRange {
                            asset,
                            at,
                            source_in,
                            duration,
                        })
                );
                let q = Rc::clone(&queue);
                reg!("importMedia", move |path: String| q
                    .borrow_mut()
                    .push(ScriptCommand::Import(path)));
                let q = Rc::clone(&queue);
                reg!("loadSample", move || q
                    .borrow_mut()
                    .push(ScriptCommand::LoadSample));
                let q = Rc::clone(&queue);
                reg!("exportTo", move |path: String| q
                    .borrow_mut()
                    .push(ScriptCommand::Export(path)));
                let q = Rc::clone(&queue);
                reg!("clearTimeline", move || q
                    .borrow_mut()
                    .push(ScriptCommand::ClearTimeline));
                let q = Rc::clone(&queue);
                reg!("setTool", move |tool: String| q
                    .borrow_mut()
                    .push(ScriptCommand::SetTool(tool)));
                let q = Rc::clone(&queue);
                reg!("setMagnet", move |on: bool| q
                    .borrow_mut()
                    .push(ScriptCommand::SetMagnet(on)));
                let q = Rc::clone(&queue);
                reg!("setZoom", move |px: f32| q
                    .borrow_mut()
                    .push(ScriptCommand::SetZoom(px)));
                let q = Rc::clone(&queue);
                reg!("closeGaps", move || q
                    .borrow_mut()
                    .push(ScriptCommand::CloseGaps));

                globals.set("__state_json", "{}")?;
                ctx.eval::<(), _>(PRELUDE)?;
                Ok(())
            })
            .map_err(|e: rquickjs::Error| e.to_string())?;

        Ok(Self {
            context,
            queue,
            deadline,
        })
    }

    /// Evaluate one REPL line. Returns display text and queued commands.
    pub fn eval(&mut self, code: &str, state_json: &str) -> (String, Vec<ScriptCommand>) {
        let (output, commands, _) = self.eval_inner(code, state_json);
        (output, commands)
    }

    /// Evaluate for chat. On a JavaScript error the queued commands are
    /// discarded, so a rejected script makes no edit at all.
    pub fn eval_checked(
        &mut self,
        code: &str,
        state_json: &str,
    ) -> Result<(String, Vec<ScriptCommand>), String> {
        let (output, commands, success) = self.eval_inner(code, state_json);
        if success {
            Ok((output, commands))
        } else {
            Err(output)
        }
    }

    fn eval_inner(&mut self, code: &str, state_json: &str) -> (String, Vec<ScriptCommand>, bool) {
        *self.deadline.borrow_mut() = Some(Instant::now() + Duration::from_millis(500));
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
        *self.deadline.borrow_mut() = None;
        let commands = std::mem::take(&mut *self.queue.borrow_mut());
        (output, commands, success)
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
}

//! In-app JavaScript control via rquickjs (QuickJS).
//!
//! A persistent context backs the REPL console; checked evals use a fresh
//! runtime and context. JS functions do not touch editor state directly.
//! They enqueue `ScriptCommand`s that the app applies on the UI thread
//! right after evaluation. Queries read a JSON snapshot (`state()`)
//! refreshed before every eval.
//!
//! Sandboxed: no module loader, no OS access, and a 32 MB memory cap.
//! A 500 ms cooperative deadline is checked by the VM and command callbacks.
//! It cannot preempt a single native operation, so every VM runs on a thread
//! of its own and a caller stops waiting after `STALL_LIMIT`.
//! Each eval can queue at most 2,048 commands and 1 MiB of owned string bytes.

use vm::Evaluated;
use worker::{room_for_a_script, Worker};

mod bindings;
mod command;
mod vm;
mod worker;
pub use command::{ScriptCommand, CLIP_HANDLE_BASE};

/// The console's VM and checked evaluations in fresh ones.
pub struct ScriptEngine {
    /// A line that had to be abandoned takes the console's VM with it; the
    /// next line starts another, without the old variables.
    console: Option<Worker>,
    next_handle: u64,
    read_audio: bool,
}

impl ScriptEngine {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            console: Some(Worker::spawn(CLIP_HANDLE_BASE)?),
            next_handle: CLIP_HANDLE_BASE,
            read_audio: false,
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
        self.read_audio = false;
        match self.eval_console(code, state_json) {
            Ok(done) => {
                self.next_handle = done.next_handle;
                self.read_audio = done.read_audio;
                (done.output, done.commands)
            }
            Err(error) => (error, Vec::new()),
        }
    }

    fn eval_console(&mut self, code: &str, state_json: &str) -> Result<Evaluated, String> {
        room_for_a_script()?;
        let worker = match self.console.take() {
            Some(worker) => worker,
            None => Worker::spawn(self.next_handle).map_err(|error| format!("error: {error}"))?,
        };
        let done = worker
            .eval(code, state_json)
            .map_err(|error| format!("{error}; console variables were reset"))?;
        self.console = Some(worker);
        Ok(done)
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
        self.read_audio = false;
        room_for_a_script()?;
        let worker = Worker::spawn(CLIP_HANDLE_BASE).map_err(|error| format!("error: {error}"))?;
        let isolated = format!("(() => {{\n{code}\n}})()");
        let done = worker.eval(&isolated, state_json)?;
        self.read_audio = done.read_audio;
        if !done.success {
            return Err(done.output);
        }
        // Nothing runs a promise's continuation, so the commands after an
        // `await` or inside a `then` would be dropped and the rest applied.
        if done.asynchronous {
            return Err(
                "error: scripts run synchronously; remove async, await and promise callbacks"
                    .to_owned(),
            );
        }
        Ok((done.output, done.commands))
    }
}

#[cfg(test)]
mod tests;

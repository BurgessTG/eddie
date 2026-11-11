//! One QuickJS runtime: its limits, its globals and one evaluation.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use rquickjs::{Context, Runtime};

use super::command::{CommandQueue, Queue, EVAL_TIMEOUT};
use super::{bindings, ScriptCommand};

/// One QuickJS runtime and context. It stays on the thread that made it.
pub(super) struct Vm {
    context: Context,
    queue: Queue,
    next_handle: u64,
}

/// What one evaluation left behind.
pub(super) struct Evaluated {
    pub(super) output: String,
    pub(super) commands: Vec<ScriptCommand>,
    pub(super) success: bool,
    pub(super) next_handle: u64,
    pub(super) read_audio: bool,
    /// It returned a promise or left a job waiting.
    pub(super) asynchronous: bool,
}

/// The JavaScript half of the API: `state()`, time specs and the
/// wrappers that check arguments before a command is queued.
const PRELUDE: &str = include_str!("prelude.js");

impl Vm {
    pub(super) fn new(next_handle: u64) -> Result<Self, String> {
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
                bindings::install(&ctx, &queue)?;
                ctx.globals().set("__state_json", "{}")?;
                ctx.eval::<(), _>(PRELUDE)?;
                Ok(())
            })
            .map_err(|e: rquickjs::Error| e.to_string())?;

        Ok(Self {
            context,
            queue,
            next_handle,
        })
    }

    pub(super) fn eval(&mut self, code: &str, state_json: String) -> Evaluated {
        *self.queue.borrow_mut() = CommandQueue {
            deadline: Some(Instant::now() + EVAL_TIMEOUT),
            next_handle: self.next_handle,
            ..CommandQueue::default()
        };
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
        let (output, commands, success) = match queue.error {
            Some(error) => (format!("error: {error}"), Vec::new(), false),
            None => (output, queue.commands, success),
        };
        Evaluated {
            output,
            commands,
            success,
            next_handle: queue.next_handle,
            read_audio: queue.read_audio,
            asynchronous: returned_promise || self.context.runtime().is_job_pending(),
        }
    }
}

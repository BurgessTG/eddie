//! A VM on a thread of its own, so a caller can stop waiting for a
//! built-in call nothing can interrupt.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use super::command::EVAL_TIMEOUT;
use super::vm::{Evaluated, Vm};

/// How long a caller waits for an evaluation. The VM stops a script at
/// `EVAL_TIMEOUT` unless it is inside one built-in call, which nothing can
/// interrupt: `indexOf` of a 7 MB string in a 14 MB one was still running
/// after 10 s. The caller then stops waiting and the thread finishes alone.
const STALL_LIMIT: Duration = Duration::from_millis(1000);

/// An abandoned evaluation keeps its VM's memory and a processor until its
/// call returns, so no script starts while this many are stuck.
const MAX_STUCK: usize = 4;

static STUCK: AtomicUsize = AtomicUsize::new(0);

/// One abandoned evaluation, counted until its thread lets go of it.
struct Stuck;

impl Stuck {
    fn count() -> Self {
        STUCK.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for Stuck {
    fn drop(&mut self) {
        STUCK.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(super) fn room_for_a_script() -> Result<(), String> {
    let stuck = STUCK.load(Ordering::Relaxed);
    if stuck < MAX_STUCK {
        return Ok(());
    }
    Err(format!(
        "error: {stuck} earlier scripts are still inside built-in calls that ran past the \
         time limit; scripts run again when one returns or Eddie restarts"
    ))
}

/// Kernel thread IDs and priorities, so an abandoned evaluation only gets
/// processor time nothing else wants.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod idle {
    extern "C" {
        fn gettid() -> i32;
        fn setpriority(which: i32, who: u32, priority: i32) -> i32;
    }

    pub fn this_thread() -> i32 {
        // SAFETY: gettid has no preconditions.
        unsafe { gettid() }
    }

    pub fn demote(thread: i32) {
        // SAFETY: setpriority reads no memory. On Linux, PRIO_PROCESS (0)
        // with a thread ID sets that one thread's nice value, and raising
        // it to 19 needs no privilege.
        unsafe {
            setpriority(0, thread as u32, 19);
        }
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
mod idle {
    pub fn this_thread() -> i32 {
        0
    }

    pub fn demote(_thread: i32) {}
}

enum Job {
    Eval {
        code: String,
        state_json: String,
    },
    /// The caller stopped waiting. The thread drops this when it reaches it
    /// or ends, and the evaluation no longer counts as stuck.
    Abandoned(Stuck),
}

/// A VM on a thread of its own, so a caller can stop waiting for it.
pub(super) struct Worker {
    jobs: Sender<Job>,
    done: Receiver<Evaluated>,
    thread: i32,
}

impl Worker {
    pub(super) fn spawn(next_handle: u64) -> Result<Self, String> {
        let (jobs, inbox) = channel();
        let (outbox, done) = channel();
        let (started, ready) = channel();
        std::thread::Builder::new()
            .name("script".to_owned())
            .spawn(move || {
                let mut vm = match Vm::new(next_handle) {
                    Ok(vm) => vm,
                    Err(error) => {
                        let _ = started.send(Err(error));
                        return;
                    }
                };
                let _ = started.send(Ok(idle::this_thread()));
                for job in inbox {
                    let Job::Eval { code, state_json } = job else {
                        break;
                    };
                    if outbox.send(vm.eval(&code, state_json)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| format!("could not start a script thread: {error}"))?;
        let thread = ready
            .recv()
            .map_err(|_| "the script thread stopped".to_owned())??;
        Ok(Self { jobs, done, thread })
    }

    /// Evaluate and wait. After an error this worker is finished: the wait
    /// ran out and its thread was left behind, or the thread is gone.
    pub(super) fn eval(&self, code: &str, state_json: &str) -> Result<Evaluated, String> {
        let job = Job::Eval {
            code: code.to_owned(),
            state_json: state_json.to_owned(),
        };
        if self.jobs.send(job).is_ok() {
            match self.done.recv_timeout(STALL_LIMIT) {
                Ok(done) => return Ok(done),
                Err(RecvTimeoutError::Timeout) => {
                    idle::demote(self.thread);
                    let _ = self.jobs.send(Job::Abandoned(Stuck::count()));
                    return Err(format!(
                        "error: evaluation time limit exceeded ({} ms) inside one built-in \
                         call, which was left to finish; give it shorter strings",
                        EVAL_TIMEOUT.as_millis()
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {}
            }
        }
        Err("error: the script thread stopped".to_owned())
    }
}

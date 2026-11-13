//! The live editor's door for agents: a private Unix socket per open
//! project. Requests are one JSON object per line, `{"id":1,"method":"run",
//! "code":"..."}`, with the methods of `commands`; each gets one line back
//! with the same `id`, after zero or more `{"id":1,"progress":"..."}` lines.
//! Lines with an `event` and no `id` go to every connection: the timeline
//! changed, imports, curation, transcription, export.
//!
//! The editor's own thread answers, between frames, so a request sees and
//! changes exactly what the window shows. Socket threads only move lines.
//!
//! One editor listens per project. `eddie serve` saves every change; a
//! window saves when its user does, or on a `save` request. A window that
//! opens a served project takes over from the server (`take_over`).

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::commands::{self, Job, Note, Outcome, Reply};
use super::project::held_elsewhere;
use super::{Editor, Wake};

mod client;
mod events;
mod host;

pub(super) use client::connect_in;
#[cfg(test)]
pub(super) use client::editors_in;
pub use client::{connect, editors, Connection};
use events::Seen;

/// One request line. Scripts are far smaller; this only bounds a bad client.
const LINE_LIMIT: u64 = 4 << 20;
/// Lines waiting for a client that is not reading before it is dropped.
const OUTBOX: usize = 1024;
/// Request lines read and not yet taken in by the editor. Socket readers
/// wait when it is full, so a client that floods fills its own socket
/// instead of the editor's memory.
const INBOX: usize = 16;
/// Requests one connection may have waiting, and their bytes. Past either,
/// a request is refused instead of queued.
const QUEUE: usize = 256;
const QUEUE_BYTES: usize = 16 << 20;
/// A client that stops reading must not hold a writer thread forever.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Requests answered per frame stop here so a window keeps drawing.
const FRAME_BUDGET: Duration = Duration::from_millis(8);
/// Symlinks resolved, so the window and the command line agree on a name.
fn canonical(project: &Path) -> PathBuf {
    std::fs::canonicalize(project)
        .or_else(|_| std::path::absolute(project))
        .unwrap_or_else(|_| project.to_path_buf())
}

/// The folder editors listen in, which only this user can enter.
pub fn socket_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("eddie"))
        .or_else(|| Some(super::settings::config_path()?.parent()?.join("run")))
}

/// Where the editor with `project` open listens.
fn socket_in(dir: &Path, project: &Path) -> Result<PathBuf, String> {
    let name = super::sweep::path_hash(&canonical(project).to_string_lossy());
    let path = dir.join(format!("{name:016x}.sock"));
    // sun_path holds 108 bytes on Linux and 104 on macOS, with a NUL.
    if path.as_os_str().len() > 100 {
        return Err(format!("{} is too long for a socket", path.display()));
    }
    Ok(path)
}

enum Incoming {
    Connected(u64, SyncSender<String>, JoinHandle<()>),
    Line(u64, String),
    Closed(u64),
}

struct Request {
    id: Value,
    method: String,
    params: Value,
    /// Bytes of the line it came in.
    size: usize,
}

struct Client {
    out: SyncSender<String>,
    writer: JoinHandle<()>,
    queue: VecDeque<Request>,
    /// Bytes of the lines in `queue`.
    waiting: usize,
    /// A job of this client's is running; its next request waits, so
    /// requests on one connection finish in the order they were sent.
    busy: bool,
    /// It stopped sending. It may still be reading (`echo … | nc -U` closes
    /// its sending side first), so what it asked is answered before it goes.
    closed: bool,
}

pub struct Server {
    project: PathBuf,
    socket: PathBuf,
    /// A window keeps its project file for the user to save, and cannot be
    /// stopped from outside.
    window: bool,
    incoming: Receiver<Incoming>,
    stop: Arc<AtomicBool>,
    clients: HashMap<u64, Client>,
    jobs: Vec<(u64, Value, Job)>,
    seen: Seen,
    /// `stop` was asked: nothing new starts, and running jobs finish.
    stopping: bool,
    /// The client answered last, so the next frame starts after it.
    turn: u64,
}

impl Server {
    /// Listen in `dir` for `project`. Fails when another editor already
    /// does. `untitled` says the project is a window's untitled one.
    pub fn bind(
        dir: &Path,
        project: &Path,
        window: bool,
        untitled: bool,
        wake: Wake,
    ) -> Result<Self, String> {
        let socket = socket_in(dir, project)?;
        let failed = |error: std::io::Error| format!("Couldn't listen for agents: {error}");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(failed)?;
        // The folder is the lock: it may predate this run with other modes.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(failed)?;
        if socket.symlink_metadata().is_ok() {
            if UnixStream::connect(&socket).is_ok() {
                return Err(held_elsewhere(project));
            }
            // Left by an editor that was killed.
            let _ = std::fs::remove_file(&socket);
        }
        let listener = UnixListener::bind(&socket).map_err(failed)?;
        let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));

        let (tx, incoming) = sync_channel(INBOX);
        let stop = Arc::new(AtomicBool::new(false));
        let hello = format!(
            "{}\n",
            json!({
                "event": "hello",
                "project": canonical(project).to_string_lossy(),
                "window": window,
                "untitled": untitled,
                "pid": std::process::id(),
                "version": env!("CARGO_PKG_VERSION"),
            })
        );
        let stopping = stop.clone();
        thread::spawn(move || {
            let mut next = 0;
            for stream in listener.incoming() {
                if stopping.load(Ordering::Relaxed) {
                    break;
                }
                match stream {
                    Ok(stream) => {
                        next += 1;
                        attach(next, stream, &hello, &tx, &wake);
                    }
                    // Out of descriptors, say: do not spin on it.
                    Err(_) => thread::sleep(Duration::from_millis(100)),
                }
            }
        });
        Ok(Self {
            project: project.to_path_buf(),
            socket,
            window,
            incoming,
            stop,
            clients: HashMap::new(),
            jobs: Vec::new(),
            seen: Seen::default(),
            stopping: false,
            turn: 0,
        })
    }

    /// The project path this was bound for, as the editor holds it.
    pub fn project(&self) -> &Path {
        &self.project
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// A client asked a windowless editor to stop, and its jobs are done.
    pub fn stopped(&self) -> bool {
        self.stopping && self.jobs.is_empty()
    }

    /// Answer what arrived and report what changed. Call once per frame
    /// after the editor took in its workers' results. `hold_edits` keeps
    /// timeline edits waiting while the user is mid-drag.
    pub fn poll(&mut self, app: &mut Editor, hold_edits: bool) {
        let started = Instant::now();
        loop {
            // Parsing megabyte lines takes time a window needs for drawing.
            if started.elapsed() > FRAME_BUDGET {
                app.wake.wake();
                break;
            }
            let Ok(incoming) = self.incoming.try_recv() else {
                break;
            };
            match incoming {
                Incoming::Connected(id, out, writer) => {
                    self.clients.insert(
                        id,
                        Client {
                            out,
                            writer,
                            queue: VecDeque::new(),
                            waiting: 0,
                            busy: false,
                            closed: false,
                        },
                    );
                }
                Incoming::Line(id, line) => self.receive(id, &line),
                Incoming::Closed(id) => {
                    if let Some(state) = self.clients.get_mut(&id) {
                        state.closed = true;
                    }
                }
            }
        }

        let mut index = 0;
        while index < self.jobs.len() {
            let mut notes = Vec::new();
            let (client, id, job) = &mut self.jobs[index];
            let (client, id) = (*client, id.clone());
            let edits = job.edits_timeline();
            if hold_edits && edits {
                app.frame_within(Duration::from_millis(100));
                index += 1;
                continue;
            }
            let result = job.poll(app, &mut notes);
            if let (true, Some(Ok(outcome))) = (edits, &result) {
                self.edited(app, outcome);
            }
            for note in notes {
                match note {
                    Note::Progress(line) => {
                        self.send(client, json!({ "id": id, "progress": line }))
                    }
                    Note::Event(event) => self.broadcast(event),
                }
            }
            match result {
                Some(result) => {
                    self.jobs.remove(index);
                    self.finish(app, client, id, result);
                }
                None => index += 1,
            }
        }

        // One request per client in turn, starting after whoever was
        // answered last, so a client with thousands waiting delays another
        // by one request and not by all of them.
        let started = Instant::now();
        let mut clients: Vec<u64> = self.clients.keys().copied().collect();
        clients.sort_unstable();
        let first = clients.partition_point(|client| *client <= self.turn);
        clients.rotate_left(first);
        'rounds: loop {
            let mut answered = false;
            for &client in &clients {
                let Some(state) = self.clients.get_mut(&client) else {
                    continue;
                };
                if state.busy {
                    continue;
                }
                let Some(next) = state.queue.front() else {
                    continue;
                };
                if hold_edits && commands::edits_timeline(&next.method) {
                    app.frame_within(Duration::from_millis(100));
                    continue;
                }
                if started.elapsed() > FRAME_BUDGET {
                    app.wake.wake();
                    break 'rounds;
                }
                let request = state.queue.pop_front().expect("a request in front");
                state.waiting -= request.size;
                self.turn = client;
                self.answer(app, client, request);
                answered = true;
            }
            if !answered {
                break;
            }
        }
        // A job of a closed client's finishes for the editor's sake.
        self.clients
            .retain(|_, state| !(state.closed && state.queue.is_empty() && !state.busy));
        self.observe(app);
    }

    fn receive(&mut self, client: u64, line: &str) {
        let parsed = serde_json::from_str::<Value>(line)
            .ok()
            .filter(Value::is_object);
        let Some(mut params) = parsed else {
            self.send(
                client,
                json!({ "id": null, "ok": false, "error": "a request is one JSON object per line" }),
            );
            return;
        };
        let id = params["id"].take();
        let Some(method) = params["method"].as_str().map(str::to_owned) else {
            self.send(
                client,
                json!({ "id": id, "ok": false, "error": "missing method" }),
            );
            return;
        };
        let Some(state) = self.clients.get_mut(&client) else {
            return;
        };
        let size = line.len();
        if state.queue.len() >= QUEUE || state.waiting + size > QUEUE_BYTES {
            self.send(
                client,
                json!({
                    "id": id,
                    "ok": false,
                    "error": "too many requests are waiting: read their answers before sending more",
                }),
            );
            return;
        }
        state.waiting += size;
        state.queue.push_back(Request {
            id,
            method,
            params,
            size,
        });
    }

    fn answer(&mut self, app: &mut Editor, client: u64, request: Request) {
        let Request {
            id, method, params, ..
        } = request;
        let reply = match method.as_str() {
            // Whoever takes over reads the file as it is now.
            _ if self.stopping => Err("this editor is stopping".to_owned()),
            "save" => app
                .save_project_to(&self.project)
                .map(|()| Reply::Done(done(json!({ "saved": true })))),
            "stop" if self.window => Err("close the window to stop this editor".to_owned()),
            "stop" => {
                self.stopping = true;
                Ok(Reply::Done(done(json!({ "stopped": true }))))
            }
            _ => commands::start(app, &method, &params),
        };
        if let ("run", Ok(Reply::Done(outcome))) = (method.as_str(), &reply) {
            self.edited(app, outcome);
        }
        match reply {
            Ok(Reply::Later(job)) => {
                if let Some(state) = self.clients.get_mut(&client) {
                    state.busy = true;
                }
                self.jobs.push((client, id, job));
                // Poll it next frame even if its worker is already done.
                app.wake.wake();
            }
            Ok(Reply::Done(outcome)) => self.finish(app, client, id, Ok(outcome)),
            Err(error) => self.finish(app, client, id, Err(error)),
        }
    }

    /// The window's user did not make this edit: say where it came from.
    fn edited(&self, app: &mut Editor, outcome: &Outcome) {
        if self.window && outcome.changed {
            app.status = "Edited from the command line.".to_owned();
        }
    }

    /// Send a request's answer. A windowless editor saves what changed; a
    /// window leaves saving to its user (or a `save` request).
    fn finish(
        &mut self,
        app: &mut Editor,
        client: u64,
        id: Value,
        result: Result<Outcome, String>,
    ) {
        let result = result.map(|outcome| {
            let mut value = outcome.value;
            if outcome.changed && !self.window {
                // The change is in this editor either way, so the request
                // did not fail: sending it again would apply it twice. The
                // next change that saves writes this one too.
                match app.save_project_to(&self.project) {
                    Ok(()) => value["saved"] = json!(true),
                    Err(error) => {
                        value["saved"] = json!(false);
                        value["saveError"] = json!(error);
                    }
                }
            }
            value
        });
        let mut value = result.unwrap_or_else(|error| json!({ "ok": false, "error": error }));
        if value.get("ok").is_none() {
            value["ok"] = json!(true);
        }
        value["id"] = id;
        if let Some(state) = self.clients.get_mut(&client) {
            state.busy = false;
        }
        self.send(client, value);
    }

    fn send(&mut self, client: u64, value: Value) {
        let Some(state) = self.clients.get(&client) else {
            return;
        };
        // Full means it stopped reading; gone means it hung up.
        if state.out.try_send(format!("{value}\n")).is_err() {
            self.clients.remove(&client);
        }
    }

    fn broadcast(&mut self, event: Value) {
        let line = format!("{event}\n");
        self.clients
            .retain(|_, state| state.out.try_send(line.clone()).is_ok());
    }
}

fn done(value: Value) -> Outcome {
    Outcome {
        value,
        changed: false,
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Give the name up first. An editor that binds it from here on is
        // not the one unlinked below.
        let gone = self.socket.with_extension("gone");
        let socket = match std::fs::rename(&self.socket, &gone) {
            Ok(()) => &gone,
            Err(_) => &self.socket,
        };
        // The accept loop only looks at `stop` when someone connects.
        drop(UnixStream::connect(socket));
        let _ = std::fs::remove_file(socket);
        for (_, client) in self.clients.drain() {
            drop(client.out);
            // `eddie serve` exits next: let the answer to `stop` out first.
            // A window never waits on a client here.
            if self.stopping {
                let _ = client.writer.join();
            }
        }
    }
}

/// Two threads per connection: one reads request lines into the editor's
/// queue, one writes answers so a slow reader never blocks the editor.
fn attach(id: u64, stream: UnixStream, hello: &str, tx: &SyncSender<Incoming>, wake: &Wake) {
    let Ok(mut write_half) = stream.try_clone() else {
        return;
    };
    let _ = write_half.set_write_timeout(Some(WRITE_TIMEOUT));
    let (out, outbox) = sync_channel::<String>(OUTBOX);
    let _ = out.try_send(hello.to_owned());
    let writer = thread::spawn(move || {
        for line in outbox {
            if write_half.write_all(line.as_bytes()).is_err() {
                break;
            }
        }
        // Ends the reader too.
        let _ = write_half.shutdown(Shutdown::Both);
    });
    if tx.send(Incoming::Connected(id, out, writer)).is_err() {
        return;
    }
    let tx = tx.clone();
    let wake = wake.clone();
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            match (&mut reader).take(LINE_LIMIT).read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if !line.ends_with('\n') && line.len() as u64 >= LINE_LIMIT {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }
            if tx.send(Incoming::Line(id, line)).is_err() {
                break;
            }
            wake.wake();
        }
        let _ = tx.send(Incoming::Closed(id));
        wake.wake();
    });
}

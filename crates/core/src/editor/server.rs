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
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::commands::{self, Job, Note, Outcome, Reply};
use super::media::AssetState;
use super::project::file_name;
use super::sweep::SweepStatus;
use super::{Editor, Wake};

/// One request line. Scripts are far smaller; this only bounds a bad client.
const LINE_LIMIT: u64 = 4 << 20;
/// One answer line read by the command line. The submission project's whole
/// state is 0.2 MB; this only bounds whatever else answers on the socket.
const REPLY_LIMIT: u64 = 64 << 20;
/// Lines waiting for a client that is not reading before it is dropped.
const OUTBOX: usize = 1024;
/// A client that stops reading must not hold a writer thread forever.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Requests answered per frame stop here so a window keeps drawing.
const FRAME_BUDGET: Duration = Duration::from_millis(8);
/// How often a window asks again for a project another editor holds.
const RETRY: Duration = Duration::from_secs(2);

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
}

struct Client {
    out: SyncSender<String>,
    writer: JoinHandle<()>,
    queue: VecDeque<Request>,
    /// A job of this client's is running; its next request waits, so
    /// requests on one connection finish in the order they were sent.
    busy: bool,
}

/// What listeners were last told, to report only changes.
#[derive(Default)]
struct Seen {
    revision: u64,
    importing: bool,
    ready: usize,
    exporting: bool,
    percent: i64,
    curating: HashMap<String, (SweepStatus, String)>,
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
}

impl Server {
    /// Listen in `dir` for `project`. Fails when another editor already
    /// does.
    pub fn bind(dir: &Path, project: &Path, window: bool, wake: Wake) -> Result<Self, String> {
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
                return Err(format!(
                    "Another Eddie already has {} open.",
                    file_name(project)
                ));
            }
            // Left by an editor that was killed.
            let _ = std::fs::remove_file(&socket);
        }
        let listener = UnixListener::bind(&socket).map_err(failed)?;
        let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));

        let (tx, incoming) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let hello = format!(
            "{}\n",
            json!({
                "event": "hello",
                "project": canonical(project),
                "window": window,
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
        while let Ok(incoming) = self.incoming.try_recv() {
            match incoming {
                Incoming::Connected(id, out, writer) => {
                    self.clients.insert(
                        id,
                        Client {
                            out,
                            writer,
                            queue: VecDeque::new(),
                            busy: false,
                        },
                    );
                }
                Incoming::Line(id, line) => self.receive(id, &line),
                Incoming::Closed(id) => {
                    // Its jobs finish for the editor's sake, unanswered.
                    self.clients.remove(&id);
                }
            }
        }

        let mut index = 0;
        while index < self.jobs.len() {
            let mut notes = Vec::new();
            let (client, id, job) = &mut self.jobs[index];
            let (client, id) = (*client, id.clone());
            let result = job.poll(app, &mut notes);
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

        let started = Instant::now();
        let mut clients: Vec<u64> = self.clients.keys().copied().collect();
        clients.sort_unstable();
        'clients: for client in clients {
            while let Some(state) = self.clients.get_mut(&client) {
                if state.busy {
                    break;
                }
                let Some(next) = state.queue.front() else {
                    break;
                };
                if hold_edits && commands::edits_timeline(&next.method) {
                    app.frame_within(Duration::from_millis(100));
                    break;
                }
                if started.elapsed() > FRAME_BUDGET {
                    app.wake.wake();
                    break 'clients;
                }
                let request = state.queue.pop_front().expect("a request in front");
                self.answer(app, client, request);
            }
        }
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
        if let Some(state) = self.clients.get_mut(&client) {
            state.queue.push_back(Request { id, method, params });
        }
    }

    fn answer(&mut self, app: &mut Editor, client: u64, request: Request) {
        let Request { id, method, params } = request;
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
        // The window's user did not make this edit: say where it came from.
        if let (true, "run", Ok(Reply::Done(outcome))) = (self.window, method.as_str(), &reply) {
            if outcome.changed {
                app.status = "Edited from the command line.".to_owned();
            }
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

    /// Send a request's answer. A windowless editor saves what changed; a
    /// window leaves saving to its user (or a `save` request).
    fn finish(
        &mut self,
        app: &mut Editor,
        client: u64,
        id: Value,
        result: Result<Outcome, String>,
    ) {
        let result = result.and_then(|outcome| {
            let mut value = outcome.value;
            if outcome.changed && !self.window {
                app.save_project_to(&self.project)?;
                value["saved"] = json!(true);
            }
            Ok(value)
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

    /// Tell listeners what changed since the last frame, whoever changed it.
    fn observe(&mut self, app: &Editor) {
        let mut events = Vec::new();
        let revision = app.history.revision();
        if revision != self.seen.revision {
            self.seen.revision = revision;
            events.push(json!({
                "event": "timeline",
                "revision": revision,
                "clips": app.timeline.clips.len(),
                "duration": app.timeline.end(),
            }));
        }

        let importing = app.import_rx.is_some() || !app.import_queue.is_empty();
        let ready = app
            .assets
            .iter()
            .filter(|asset| asset.state == AssetState::Ready)
            .count();
        if (importing, ready) != (self.seen.importing, self.seen.ready) {
            self.seen.importing = importing;
            self.seen.ready = ready;
            events.push(json!({ "event": "import", "importing": importing, "assets": ready }));
        }

        if app.exporting() {
            let percent = (app.export_progress.unwrap_or(0.0) * 100.0).round() as i64;
            if !self.seen.exporting || percent != self.seen.percent {
                self.seen.exporting = true;
                self.seen.percent = percent;
                events.push(json!({ "event": "export", "progress": percent as f64 / 100.0 }));
            }
        } else if self.seen.exporting {
            self.seen.exporting = false;
            events.push(match (&app.exported, &app.export_failed) {
                (Some((file, _)), _) => json!({ "event": "export", "done": true, "output": file }),
                (None, error) => json!({ "event": "export", "done": true, "error": error }),
            });
        }

        // Only curation that ran here: a saved one restored on import is
        // not news.
        for asset in &app.assets {
            let now = (asset.sweep.status, asset.sweep.note.as_str());
            let before = self.seen.curating.get(&asset.info.path);
            let running = now.0 == SweepStatus::Running;
            if !running && before.is_none() {
                continue;
            }
            if before.is_some_and(|(status, note)| (*status, note.as_str()) == now) {
                continue;
            }
            events.push(json!({
                "event": "curation",
                "asset": asset.info.name,
                "status": match now.0 {
                    SweepStatus::Running => "running",
                    SweepStatus::Ready => "ready",
                    SweepStatus::Failed => "failed",
                    SweepStatus::NotStarted => "none",
                },
                "note": now.1,
            }));
            if running {
                self.seen
                    .curating
                    .insert(asset.info.path.clone(), (now.0, now.1.to_owned()));
            } else {
                self.seen.curating.remove(&asset.info.path);
            }
        }

        for event in events {
            self.broadcast(event);
        }
    }

    /// Start as if listeners already knew the editor's state.
    fn settle(&mut self, app: &Editor) {
        self.observe(app);
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
        // The accept loop only looks at `stop` when someone connects.
        drop(UnixStream::connect(&self.socket));
        let _ = std::fs::remove_file(&self.socket);
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
fn attach(id: u64, stream: UnixStream, hello: &str, tx: &Sender<Incoming>, wake: &Wake) {
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

impl Editor {
    /// Keep a window's socket on its open project and answer requests.
    pub(crate) fn serve_requests(&mut self) {
        if self.listen {
            let bound = self.server.as_ref().map(Server::project);
            if bound != self.project_path.as_deref() {
                self.server = None;
                self.follow_project();
            }
        }
        if let Some(mut server) = self.server.take() {
            server.poll(self, self.pointer_held);
            self.server = Some(server);
        }
    }

    fn follow_project(&mut self) {
        let Some(project) = self.project_path.clone() else {
            self.listen_refused = None;
            self.listen_notice = None;
            return;
        };
        self.bind_project(project);
        // Each finished import writes the status, so a refusal waits for
        // the project's media to load.
        if !self.importing() && self.import_queue.is_empty() {
            if let Some(notice) = self.listen_notice.take() {
                self.status = notice;
            }
        }
    }

    fn bind_project(&mut self, project: PathBuf) {
        let refused = self
            .listen_refused
            .take()
            .filter(|(refused, _)| *refused == project);
        if let Some((_, at)) = &refused {
            let since = at.elapsed();
            if since < RETRY {
                self.frame_within(RETRY - since);
                self.listen_refused = refused;
                return;
            }
        }
        let bound = self
            .socket_dir()
            .and_then(|dir| Server::bind(dir, &project, true, self.wake.clone()));
        match bound {
            Ok(mut server) => {
                server.settle(self);
                self.server = Some(server);
                self.listen_notice = None;
            }
            Err(error) => {
                // Said once; asking again until the other editor closes is
                // silent.
                if refused.is_none() {
                    self.listen_notice = Some(error);
                }
                self.listen_refused = Some((project, Instant::now()));
                self.frame_within(RETRY);
            }
        }
    }

    /// `eddie serve`: listen for this editor's open project without a
    /// window. Call `begin_frame` and `end_frame` in a loop until
    /// `serve_stopped`.
    pub fn serve(&mut self) -> Result<PathBuf, String> {
        let project = self.project_path.clone().ok_or("no project is open")?;
        let mut server = Server::bind(self.socket_dir()?, &project, false, self.wake.clone())?;
        server.settle(self);
        let socket = server.socket().to_path_buf();
        self.server = Some(server);
        Ok(socket)
    }

    pub fn serve_stopped(&self) -> bool {
        self.server.as_ref().is_none_or(Server::stopped)
    }

    fn socket_dir(&self) -> Result<&Path, String> {
        self.socket_dir
            .as_deref()
            .ok_or_else(|| "No folder for the editor's socket: set XDG_RUNTIME_DIR.".to_owned())
    }

    /// A window about to open `project` takes over from `eddie serve`,
    /// which has saved every change and only has to let go.
    pub(crate) fn take_over(&self, project: &Path) {
        if !self.listen {
            return;
        }
        let Some(mut live) = self
            .socket_dir
            .as_deref()
            .and_then(|dir| connect_in(dir, project))
        else {
            return;
        };
        if live.window {
            return;
        }
        // Usually gone within a frame. A job still running keeps it longer,
        // and a stuck one must not hold this window: after a second the
        // window opens anyway and asks again for the socket until it is free.
        live.deadline = Some(Instant::now() + Duration::from_secs(1));
        let _ = live
            .writer
            .set_read_timeout(Some(Duration::from_millis(50)));
        if live.request("stop", &json!({}), &mut |_| {}).is_ok() {
            while let Ok(Some(_)) = live.line(&mut || true) {}
        }
    }
}

/// A connection to the editor that has `project` open, for the command line.
pub struct Connection {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    hello: String,
    /// The editor is a window, which only its user stops.
    window: bool,
    /// When to stop waiting for the editor. The command line waits as long
    /// as a job takes.
    deadline: Option<Instant>,
}

/// The running editor with `project` open, if there is one.
pub fn connect(project: &Path) -> Option<Connection> {
    connect_in(&socket_dir()?, project)
}

pub(super) fn connect_in(dir: &Path, project: &Path) -> Option<Connection> {
    let stream = UnixStream::connect(socket_in(dir, project).ok()?).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut hello = String::new();
    // Whatever answers on this socket is not trusted to stop sending.
    (&mut reader).take(LINE_LIMIT).read_line(&mut hello).ok()?;
    let greeting: Value = serde_json::from_str(&hello).ok()?;
    if greeting["event"] != "hello"
        || Path::new(greeting["project"].as_str()?) != canonical(project)
    {
        return None;
    }
    Some(Connection {
        reader,
        writer: stream,
        hello,
        window: greeting["window"] == true,
        deadline: None,
    })
}

impl Connection {
    /// Read one line; `None` when the editor closed. `wait` is asked each
    /// time the read timeout passes without a line; false, or the deadline,
    /// gives up.
    fn line(&mut self, wait: &mut dyn FnMut() -> bool) -> Result<Option<String>, String> {
        let mut bytes = Vec::new();
        loop {
            // A timeout keeps what arrived, so the room left shrinks.
            let room = REPLY_LIMIT.saturating_sub(bytes.len() as u64);
            match (&mut self.reader).take(room).read_until(b'\n', &mut bytes) {
                Ok(_) if bytes.len() as u64 >= REPLY_LIMIT && !bytes.ends_with(b"\n") => {
                    return Err("the editor's answer was too long".to_owned());
                }
                Ok(0) => return Ok(None),
                Ok(_) => return Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    let late = self.deadline.is_some_and(|at| Instant::now() >= at);
                    if late || !wait() {
                        return Err("the editor did not answer".to_owned());
                    }
                }
                Err(error) => return Err(format!("the editor's socket failed: {error}")),
            }
        }
    }

    /// Ask the editor and wait for its answer; `progress` gets its progress
    /// lines meanwhile.
    pub fn request(
        &mut self,
        method: &str,
        params: &Value,
        progress: &mut dyn FnMut(&str),
    ) -> Result<Value, String> {
        let mut request = params.clone();
        request["id"] = json!(1);
        request["method"] = json!(method);
        self.writer
            .write_all(format!("{request}\n").as_bytes())
            .map_err(|error| format!("the editor's socket failed: {error}"))?;
        // Said once, and only before the editor has shown it is working: a
        // window answers between frames, so a drag or a file dialog holds
        // the request.
        let mut quiet = true;
        loop {
            let line = self.line(&mut || {
                if std::mem::take(&mut quiet) {
                    progress("waiting for the editor…");
                }
                true
            })?;
            let Some(line) = line else {
                return Err("the editor closed before it answered".to_owned());
            };
            let Ok(mut value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value["id"] != 1 {
                continue;
            }
            if let Some(line) = value["progress"].as_str() {
                quiet = false;
                progress(line);
                continue;
            }
            if let Some(object) = value.as_object_mut() {
                object.remove("id");
            }
            return Ok(value);
        }
    }

    /// Hand every event line to `each` until the editor closes or `each`
    /// returns false.
    pub fn events(&mut self, each: &mut dyn FnMut(&str) -> bool) -> Result<(), String> {
        if !each(self.hello.trim_end()) {
            return Ok(());
        }
        while let Some(line) = self.line(&mut || true)? {
            let event =
                serde_json::from_str::<Value>(&line).is_ok_and(|value| value["event"].is_string());
            if event && !each(line.trim_end()) {
                break;
            }
        }
        Ok(())
    }
}

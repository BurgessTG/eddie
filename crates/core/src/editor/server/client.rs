//! The command line's end of the socket: find the editor that has a
//! project open, ask it, and read its answer.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{canonical, socket_dir, socket_in, LINE_LIMIT};

/// One answer line read by the command line. The submission project's whole
/// state is 0.2 MB; this only bounds whatever else answers on the socket.
const REPLY_LIMIT: u64 = 64 << 20;

/// A connection to the editor that has `project` open, for the command line.
pub struct Connection {
    reader: BufReader<UnixStream>,
    pub(super) writer: UnixStream,
    hello: String,
    /// The editor is a window, which only its user stops.
    pub(super) window: bool,
    /// When to stop waiting for the editor. The command line waits as long
    /// as a job takes.
    pub(super) deadline: Option<Instant>,
}

/// The running editor with `project` open, if there is one.
pub fn connect(project: &Path) -> Option<Connection> {
    connect_in(&socket_dir()?, project)
}

pub(crate) fn connect_in(dir: &Path, project: &Path) -> Option<Connection> {
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
    pub(super) fn line(
        &mut self,
        wait: &mut dyn FnMut() -> bool,
    ) -> Result<Option<String>, String> {
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

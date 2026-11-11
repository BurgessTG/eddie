//! How an editor holds its server: a window follows its open project,
//! `eddie serve` stays on one, and a window takes a served project over.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;

use super::super::project::held_elsewhere;
use super::super::Editor;
use super::{connect_in, Server};

/// How often a window asks again for a project another editor holds.
const RETRY: Duration = Duration::from_secs(2);

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
        // The file first, then the socket: of two editors that start on a
        // socket a killed one left, only the file's holder replaces it.
        let bound = if self.hold_project() {
            self.socket_dir()
                .and_then(|dir| Server::bind(dir, &project, true, self.wake.clone()))
        } else {
            Err(held_elsewhere(&project))
        };
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
        if !self.hold_project() {
            return Err(held_elsewhere(&project));
        }
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
    /// which has saved every change and only has to let go. True when one
    /// was asked to.
    pub(crate) fn take_over(&self, project: &Path) -> bool {
        if !self.listen {
            return false;
        }
        let Some(mut live) = self
            .socket_dir
            .as_deref()
            .and_then(|dir| connect_in(dir, project))
        else {
            return false;
        };
        if live.window {
            return false;
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
        true
    }
}

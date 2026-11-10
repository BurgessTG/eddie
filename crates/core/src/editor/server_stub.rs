//! Live mode (`server.rs`) needs Unix sockets. Without them no editor
//! listens, so the command line always works on the project file.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::Editor;

const UNSUPPORTED: &str = "live mode needs Unix sockets, which this system lacks";

pub struct Server;

pub struct Connection;

pub fn socket_dir() -> Option<PathBuf> {
    None
}

pub fn connect(_project: &Path) -> Option<Connection> {
    None
}

impl Connection {
    pub fn request(
        &mut self,
        _method: &str,
        _params: &Value,
        _progress: &mut dyn FnMut(&str),
    ) -> Result<Value, String> {
        Err(UNSUPPORTED.to_owned())
    }

    pub fn events(&mut self, _each: &mut dyn FnMut(&str) -> bool) -> Result<(), String> {
        Err(UNSUPPORTED.to_owned())
    }
}

impl Editor {
    pub(crate) fn serve_requests(&mut self) {}

    pub fn serve(&mut self) -> Result<PathBuf, String> {
        Err(UNSUPPORTED.to_owned())
    }

    pub fn serve_stopped(&self) -> bool {
        true
    }

    pub(crate) fn take_over(&self, _project: &Path) -> bool {
        false
    }
}

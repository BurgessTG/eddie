//! Export from the editor: the worker, its progress and its result.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, TryRecvError};
use std::thread;

use openreel_media::MediaInfo;

use crate::Editor;

pub(super) enum ExportMsg {
    Progress(f64),
    Done(Result<PathBuf, String>),
}

impl Editor {
    /// Returns true when an export finished successfully this frame.
    pub fn poll_export(&mut self) -> bool {
        let mut done = false;
        let mut exported = false;
        loop {
            let msg = match self.export_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(msg)) => msg,
                Some(Err(TryRecvError::Empty)) => break,
                Some(Err(TryRecvError::Disconnected)) => {
                    if !done {
                        // The worker ended without an answer: it panicked.
                        let error = "Export stopped unexpectedly.".to_owned();
                        self.export_failed = Some(error.clone());
                        self.status = error;
                    }
                    done = true;
                    break;
                }
                None => break,
            };
            match msg {
                ExportMsg::Progress(p) => self.export_progress = Some(p),
                ExportMsg::Done(Ok(path)) => {
                    let name = path.file_name().unwrap_or(path.as_os_str());
                    self.status = format!("Exported {}", name.to_string_lossy());
                    self.exported = Some((path, self.status.clone()));
                    exported = true;
                    done = true;
                }
                ExportMsg::Done(Err(e)) => {
                    self.export_failed = Some(e.clone());
                    self.status = e;
                    done = true;
                }
            }
        }
        if done {
            self.export_rx = None;
            self.export_progress = None;
        }
        exported
    }

    pub fn start_export(&mut self, output: PathBuf) {
        if self.export_rx.is_some() {
            self.status = "An export is already running.".to_owned();
            return;
        }
        if self.timeline.clips.is_empty() {
            self.status = "Nothing to export.".to_owned();
            return;
        }
        if let Some(used) = self.export_would_replace(&output) {
            self.status = format!("Export cannot replace {used}, which this project uses.");
            return;
        }
        let timeline = self.timeline.clone();
        let options = openreel_media::ExportOptions {
            normalize_loudness: true,
            speech: self.speech_map(),
        };
        let infos: HashMap<String, MediaInfo> = self
            .assets
            .iter()
            .map(|a| (a.info.path.clone(), a.info.clone()))
            .collect();
        let (tx, rx) = channel::<ExportMsg>();
        let (progress_tx, progress_rx) = channel::<f64>();
        self.export_rx = Some(rx);
        self.export_progress = Some(0.0);
        self.exported = None;
        self.export_failed = None;
        let wake = self.wake.clone();
        thread::spawn(move || {
            let tx_progress = tx.clone();
            let wake_progress = wake.clone();
            let forwarder = thread::spawn(move || {
                while let Ok(p) = progress_rx.recv() {
                    let _ = tx_progress.send(ExportMsg::Progress(p));
                    wake_progress.wake();
                }
            });
            let result =
                openreel_media::export_timeline(&timeline, &infos, &output, &options, progress_tx);
            let _ = forwarder.join();
            let _ = tx.send(ExportMsg::Done(result));
            wake.wake();
        });
    }

    /// The name of the project file or imported media that `output` is.
    /// Exporting over one would destroy what the export is made from.
    pub(crate) fn export_would_replace(&self, output: &Path) -> Option<String> {
        let output = std::fs::canonicalize(output).ok()?;
        self.project_path
            .iter()
            .map(PathBuf::as_path)
            .chain(self.assets.iter().map(|asset| Path::new(&asset.info.path)))
            .find(|used| std::fs::canonicalize(used).is_ok_and(|used| used == output))
            .map(|used| {
                used.file_name()
                    .unwrap_or(used.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            })
    }

    pub fn exporting(&self) -> bool {
        self.export_rx.is_some()
    }
}

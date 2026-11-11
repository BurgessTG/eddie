//! A request answered over several frames: an import, an export, a
//! picture, a transcription, a curation, or one waiting for the mix to be
//! measured. The editor's thread polls it; workers do the waiting.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

use serde_json::{json, Value};

use super::super::sweep::transcribe::Transcript;
use super::super::sweep::SweepStatus;
use super::super::Editor;
use super::{edits_timeline, start, summary, Note, Outcome, Reply};

pub enum Job {
    Import(Vec<PathBuf>),
    Export {
        started: Instant,
        reported: f64,
    },
    Render(Receiver<Result<Value, String>>),
    Transcribe {
        rx: Receiver<TranscribeMsg>,
        results: Vec<Value>,
        failed: bool,
    },
    Curate {
        /// Each asset's path and the last note reported for it.
        assets: Vec<(String, String)>,
        /// Paths answered from a saved curation instead.
        saved: Vec<String>,
    },
    /// A request that reads the measured mix, waiting for the measurement.
    Measure {
        method: String,
        params: Value,
    },
}

impl Job {
    /// Whether this job ends in a timeline edit, which a window holds back
    /// while the user is mid-drag.
    pub fn edits_timeline(&self) -> bool {
        matches!(self, Job::Measure { method, .. } if edits_timeline(method))
    }
}

pub enum TranscribeMsg {
    Progress(String, String),
    Done {
        path: String,
        name: String,
        seconds: f64,
        result: Result<Transcript, String>,
    },
}

impl Job {
    /// Call once per frame, after the editor took in its workers' results.
    /// `Some` is the answer.
    pub fn poll(
        &mut self,
        app: &mut Editor,
        notes: &mut Vec<Note>,
    ) -> Option<Result<Outcome, String>> {
        match self {
            Job::Measure { method, params } => {
                // The timeline may have changed while this waited.
                app.measure_mix();
                if !app.mix_measured() {
                    return None;
                }
                match start(app, method, params) {
                    Ok(Reply::Done(outcome)) => Some(Ok(outcome)),
                    Ok(Reply::Later(job)) => {
                        *self = job;
                        None
                    }
                    Err(error) => Some(Err(error)),
                }
            }
            Job::Import(media) => imported(app, media),
            Job::Export { started, reported } => exported(app, *started, reported, notes),
            Job::Render(rx) => match rx.try_recv() {
                Ok(result) => Some(result.map(Outcome::read)),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("the picture worker stopped".into())),
            },
            Job::Transcribe {
                rx,
                results,
                failed,
            } => transcribed(app, rx, results, failed, notes),
            Job::Curate { assets, saved } => curated(app, assets, saved, notes),
        }
    }
}

type Polled = Option<Result<Outcome, String>>;

/// The import queue has drained: which of `media` came in and which failed.
fn imported(app: &Editor, media: &[PathBuf]) -> Polled {
    if app.import_rx.is_some() || !app.import_queue.is_empty() {
        return None;
    }
    let (imported, failed): (Vec<_>, Vec<_>) = media
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        // A file that was here and no longer reads stays in the
        // bin, and is still a failure.
        .partition(|path| app.asset(path).is_some() && !app.import_errors.contains_key(path));
    let failed: Vec<Value> = failed
        .into_iter()
        .map(|path| {
            let error = app.import_errors.get(&path).cloned();
            json!({ "path": path, "error": error })
        })
        .collect();
    Some(Ok(Outcome {
        changed: !imported.is_empty(),
        value: json!({
            "media": { "imported": imported, "failed": failed },
            "saved": false,
            "summary": summary(app),
        }),
    }))
}

/// The export's progress in tenths, then its file or its failure.
fn exported(app: &Editor, started: Instant, reported: &mut f64, notes: &mut Vec<Note>) -> Polled {
    if app.exporting() {
        let progress = app.export_progress.unwrap_or(0.0);
        if progress >= *reported + 0.1 {
            *reported = progress;
            notes.push(Note::Progress(format!("export {:.0}%", progress * 100.0)));
        }
        return None;
    }
    Some(match &app.exported {
        Some((file, _)) => Ok(Outcome::read(json!({
            "output": file.to_string_lossy(),
            "seconds": started.elapsed().as_secs_f64(),
        }))),
        None => Err(app
            .export_failed
            .clone()
            .unwrap_or_else(|| "The export stopped.".to_owned())),
    })
}

/// Take in what the transcription workers sent; the answer once all have
/// finished.
fn transcribed(
    app: &mut Editor,
    rx: &Receiver<TranscribeMsg>,
    results: &mut Vec<Value>,
    failed: &mut bool,
    notes: &mut Vec<Note>,
) -> Polled {
    loop {
        match rx.try_recv() {
            Ok(TranscribeMsg::Progress(name, message)) => {
                notes.push(Note::Event(json!({
                    "event": "transcription",
                    "asset": name,
                    "message": message,
                })));
                notes.push(Note::Progress(format!("{name}: {message}")));
            }
            Ok(TranscribeMsg::Done {
                path,
                name,
                seconds,
                result,
            }) => results.push(match result {
                Ok(transcript) => {
                    let words: usize = transcript.segments.iter().map(|s| s.words.len()).sum();
                    let entry = json!({
                        "asset": name,
                        "turns": transcript.segments.len(),
                        "words": words,
                        "source": transcript.source,
                        "reused": transcript.cached,
                        "seconds": seconds,
                        "note": transcript.note,
                    });
                    notes.push(Note::Event(json!({
                        "event": "transcription",
                        "asset": name,
                        "done": true,
                        "turns": transcript.segments.len(),
                    })));
                    // A running editor's scripts see the speech now.
                    if let Some(asset) = app.assets.iter_mut().find(|asset| asset.info.path == path)
                    {
                        asset.sweep.transcript = transcript.segments;
                    }
                    entry
                }
                Err(error) => {
                    *failed = true;
                    notes.push(Note::Event(json!({
                        "event": "transcription",
                        "asset": name,
                        "done": true,
                        "error": error,
                    })));
                    json!({ "asset": name, "error": error })
                }
            }),
            Err(TryRecvError::Empty) => break None,
            Err(TryRecvError::Disconnected) => {
                break Some(Ok(Outcome::read(json!({
                    "ok": !*failed,
                    "assets": std::mem::take(results),
                }))));
            }
        }
    }
}

/// Each asset's curation notes as they change; the answer once none is
/// running.
fn curated(
    app: &Editor,
    assets: &mut [(String, String)],
    saved: &[String],
    notes: &mut Vec<Note>,
) -> Polled {
    let mut waiting = false;
    for (path, last) in assets.iter_mut() {
        let Some(asset) = app.asset(path) else {
            continue;
        };
        if asset.sweep.status == SweepStatus::Running {
            waiting = true;
            if asset.sweep.note != *last {
                last.clone_from(&asset.sweep.note);
                notes.push(Note::Progress(format!("{}: {last}", asset.info.name)));
            }
        }
    }
    if waiting {
        return None;
    }
    let mut failed = false;
    let results: Vec<Value> = assets
        .iter()
        .map(|(path, _)| match app.asset(path) {
            Some(asset) => {
                let ready = asset.sweep.status == SweepStatus::Ready;
                failed |= !ready;
                json!({
                    "asset": asset.info.name,
                    "curated": ready,
                    "reused": saved.contains(path),
                    "candidates": asset.sweep.candidates.len(),
                    "scenes": asset.sweep.scene_clips.len(),
                    "note": asset.sweep.note,
                })
            }
            None => {
                failed = true;
                json!({ "asset": path, "error": "removed from the bin" })
            }
        })
        .collect();
    Some(Ok(Outcome::read(
        json!({ "ok": !failed, "assets": results }),
    )))
}

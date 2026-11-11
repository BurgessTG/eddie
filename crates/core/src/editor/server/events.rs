//! What listeners are told without asking: the timeline changed, imports,
//! curation, transcription and export, each reported once per change.

use std::collections::HashMap;

use serde_json::json;

use super::super::media::AssetState;
use super::super::sweep::SweepStatus;
use super::super::Editor;
use super::Server;

/// What listeners were last told, to report only changes.
#[derive(Default)]
pub(super) struct Seen {
    revision: u64,
    importing: bool,
    ready: usize,
    exporting: bool,
    percent: i64,
    curating: HashMap<String, (SweepStatus, String)>,
}

impl Server {
    /// Tell listeners what changed since the last frame, whoever changed it.
    pub(super) fn observe(&mut self, app: &Editor) {
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
                (Some((file, _)), _) => {
                    json!({ "event": "export", "done": true, "output": file.to_string_lossy() })
                }
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
    pub(super) fn settle(&mut self, app: &Editor) {
        self.observe(app);
    }
}

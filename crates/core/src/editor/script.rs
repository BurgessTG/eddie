//! Scripts against the editor: the state they read, and their commands
//! applied as one edit that rolls back whole.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use openreel_engine::{ClipId, Timeline};

use crate::scripting::ScriptCommand;
use crate::Editor;

mod apply;
mod state;

/// The editor state a script's commands can change (`Editor::edit_snapshot`).
#[derive(Clone)]
pub struct EditSnapshot {
    timeline: Timeline,
    selected: Option<ClipId>,
    playhead: f64,
    playing: bool,
    magnet: bool,
    zoom: f32,
    mute_music_beds: bool,
}

impl Editor {
    /// Apply a script's commands as one edit: every command applies, or the
    /// editor is restored (timeline, selection, transport, magnet, zoom and
    /// music-bed policy). Files go through import and export, never a
    /// script. Returns the notes and each handle's new clip ID.
    pub fn apply_edit(
        &mut self,
        commands: Vec<ScriptCommand>,
    ) -> Result<(Vec<String>, HashMap<String, ClipId>), String> {
        for command in &commands {
            if matches!(command, ScriptCommand::Import(_) | ScriptCommand::Export(_)) {
                return Err("use import and export for files, not a script".to_owned());
            }
            command.validate()?;
        }
        let before = self.edit_snapshot();
        let overlaps_before = self.timeline.lane_overlaps();
        let started = Instant::now();
        let mut created = HashMap::new();
        let result = (|| {
            let mut notes = Vec::new();
            for command in commands {
                notes.extend(self.apply_script_command(command, &mut created)?);
                // Finite input can still overflow frame-grid arithmetic, or
                // send a clip days away.
                if !self.playhead.is_finite() || !self.timeline.in_supported_range() {
                    return Err("edit exceeds the supported timeline range".to_owned());
                }
                // Cooperative bound: individual native commands are not
                // preemptible, but a long finite batch cannot run unchecked.
                if started.elapsed() > Duration::from_millis(250) {
                    return Err("edit took too long; try a smaller batch".to_owned());
                }
            }
            // Checked once the whole edit has run, so commands may pass
            // through an overlap on the way to a valid timeline.
            if let Some(&(a, b)) = self
                .timeline
                .lane_overlaps()
                .iter()
                .find(|pair| !overlaps_before.contains(pair))
            {
                let describe = |id: ClipId| match self.timeline.clip(id) {
                    Some(clip) => format!("clip {id} ({})", clip.name),
                    None => format!("clip {id}"),
                };
                let lane = self.timeline.clip(a).map_or(0, |clip| clip.track);
                return Err(format!(
                    "{} and {} would overlap on lane {lane}; trim one, or move it in time or to another lane with setClipLane",
                    describe(a),
                    describe(b)
                ));
            }
            Ok(notes)
        })();
        let changed = self.timeline != before.timeline;
        match result {
            Ok(notes) => {
                if changed {
                    self.stop_sources();
                    self.forget_stills();
                }
                let handles = created
                    .into_iter()
                    .map(|(handle, clip)| (handle.to_string(), clip))
                    .collect();
                Ok((notes, handles))
            }
            Err(error) => {
                self.restore_edit(before);
                Err(error)
            }
        }
    }

    /// Everything a script's commands can change, to put back with
    /// `restore_edit`.
    pub fn edit_snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            timeline: self.timeline.clone(),
            selected: self.selected,
            playhead: self.playhead,
            playing: self.playing,
            magnet: self.viewport.magnet,
            zoom: self.viewport.px_per_sec,
            mute_music_beds: self.mute_music_beds,
        }
    }

    /// The project file would differ from one saved at `before`.
    pub fn differs_from(&self, before: &EditSnapshot) -> bool {
        self.timeline != before.timeline || self.mute_music_beds != before.mute_music_beds
    }

    /// Undo an edit that must leave no trace: a failed one, or a dry run.
    pub fn restore_edit(&mut self, before: EditSnapshot) {
        // An edit refused before it changed anything leaves playback alone.
        let disturbed = self.timeline != before.timeline
            || self.playhead != before.playhead
            || self.playing != before.playing;
        self.timeline = before.timeline;
        self.selected = before.selected;
        self.playhead = before.playhead;
        self.playing = before.playing;
        self.viewport.magnet = before.magnet;
        self.viewport.px_per_sec = before.zoom;
        self.mute_music_beds = before.mute_music_beds;
        if disturbed && !self.rehearsing {
            self.stop_sources();
            self.last_now = None;
            self.forget_stills();
        }
    }

    pub fn apply_script_commands(&mut self, commands: Vec<ScriptCommand>) -> Vec<String> {
        let mut notes = Vec::new();
        let mut created = HashMap::new();
        for command in commands {
            match self.apply_script_command(command, &mut created) {
                Ok(Some(note)) => notes.push(note),
                Ok(None) => {}
                Err(error) => notes.push(format!("error: {error}")),
            }
        }
        notes
    }

    pub fn run_console(&mut self, code: String) {
        let code = code.trim().to_owned();
        if code.is_empty() {
            return;
        }
        self.console_output.push(format!("> {code}"));
        if self.console_history.last().map(String::as_str) != Some(code.as_str()) {
            self.console_history.push(code.clone());
        }
        self.console_history_idx = None;

        let state = self.script_state_json();
        let Some(engine) = self.script_engine.as_mut() else {
            self.console_output
                .push("error: scripting unavailable".to_owned());
            return;
        };
        let started = Instant::now();
        let (output, commands) = engine.eval(&code, &state);
        let elapsed_ms = started.elapsed().as_secs_f32() * 1000.0;
        for note in self.apply_script_commands(commands) {
            self.console_output.push(note);
        }
        if output != "undefined" {
            for line in output.lines() {
                self.console_output.push(line.to_owned());
            }
        }
        self.status = format!("JS {elapsed_ms:.2} ms");
        if self.console_output.len() > 300 {
            let excess = self.console_output.len() - 300;
            self.console_output.drain(..excess);
        }
    }

    /// Up (`older`) or Down in the console's input: the line it should now
    /// hold. Up stops at the oldest line; Down past the newest gives the
    /// empty line back. `None` when Up has nothing to recall.
    pub fn recall_console(&mut self, older: bool) -> Option<String> {
        let index = if older {
            Some(match self.console_history_idx {
                Some(index) => index.saturating_sub(1),
                None => self.console_history.len().checked_sub(1)?,
            })
        } else {
            self.console_history_idx
                .map(|index| index + 1)
                .filter(|index| *index < self.console_history.len())
        };
        self.console_history_idx = index;
        Some(index.map_or_else(String::new, |index| self.console_history[index].clone()))
    }
}

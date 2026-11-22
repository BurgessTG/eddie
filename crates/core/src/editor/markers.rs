//! Markers in the editor, as CapCut's: M drops one at the playhead in the
//! colour last used, Alt+M one in the next colour, Shift+M and Alt+Shift+M
//! jump to the next and previous one, and scripts add, change and remove
//! them. The model is `openreel_engine`'s `Marker`.

use openreel_engine::{format_timecode, MarkerColor, MarkerId, MAX_TIMELINE_SECONDS};

use super::{Action, Editor};
use crate::scripting::ScriptCommand;

impl Editor {
    pub(super) fn marker_action(&mut self, action: Action) {
        match action {
            Action::AddMarker => {
                self.add_marker_here(false);
            }
            Action::AddMarkerNewColor => {
                self.add_marker_here(true);
            }
            Action::NextMarker => self.jump_to_marker(true),
            Action::PrevMarker => self.jump_to_marker(false),
            _ => {}
        }
    }

    /// The colour M gives a new marker: the newest marker's.
    fn marker_color(&self) -> MarkerColor {
        self.timeline
            .markers
            .iter()
            .max_by_key(|marker| marker.id)
            .map_or(MarkerColor::default(), |marker| marker.color)
    }

    /// Drop a marker on the playhead's frame, in the colour last used or
    /// the next one. A marker already there is kept and returned instead.
    pub fn add_marker_here(&mut self, new_color: bool) -> MarkerId {
        if let Some(marker) = self.timeline.marker_at(self.playhead) {
            self.status = format!("Marker {} is already here.", marker.id);
            return marker.id;
        }
        let color = if new_color {
            self.timeline.next_marker_color()
        } else {
            self.marker_color()
        };
        let id = self.timeline.add_marker(self.playhead, "", color);
        self.status = format!(
            "Added marker {id} at {}.",
            format_timecode(self.playhead, self.timeline.fps)
        );
        id
    }

    /// Move the playhead to the nearest marker after it, or before it.
    pub fn jump_to_marker(&mut self, forward: bool) {
        match self.timeline.marker_beside(self.playhead, forward) {
            Some(marker) => {
                let time = marker.time;
                self.seek_and_reveal(time);
            }
            None => {
                self.status = if forward {
                    "No marker after the playhead.".to_owned()
                } else {
                    "No marker before the playhead.".to_owned()
                };
            }
        }
    }

    pub fn delete_marker(&mut self, id: MarkerId) {
        if self.timeline.remove_marker(id) {
            self.status = format!("Marker {id} deleted.");
        }
    }

    /// `addMarker`, `setMarker`, `removeMarker` and `clearMarkers`.
    pub(super) fn apply_marker_command(
        &mut self,
        command: ScriptCommand,
    ) -> Result<Option<String>, String> {
        match command {
            ScriptCommand::AddMarker { at, name, color } => {
                marker_time("addMarker", at)?;
                let color = match color.as_str() {
                    "" => self.marker_color(),
                    name => marker_color(name)?,
                };
                let id = self.timeline.add_marker(at, &name, color);
                let time = self.timeline.marker(id).map_or(at, |marker| marker.time);
                Ok(Some(format!("addMarker: marker {id} at {time} s")))
            }
            ScriptCommand::SetMarker { marker, changes } => {
                let changes: serde_json::Map<String, serde_json::Value> =
                    serde_json::from_str(&changes).map_err(|_| {
                        "setMarker needs an object such as {name: 'B-roll here', color: 'red'}"
                            .to_owned()
                    })?;
                let mut edited = self
                    .timeline
                    .marker(marker)
                    .cloned()
                    .ok_or_else(|| format!("no marker {marker}"))?;
                for (key, value) in changes {
                    match (key.as_str(), value) {
                        ("time", serde_json::Value::Number(time)) => {
                            edited.time = time.as_f64().unwrap_or(-1.0);
                            marker_time("setMarker", edited.time)?;
                        }
                        ("name", serde_json::Value::String(name)) => edited.name = name,
                        ("color", serde_json::Value::String(color)) => {
                            edited.color = marker_color(&color)?;
                        }
                        ("time" | "name" | "color", value) => {
                            return Err(format!(
                                "setMarker {key} cannot be {value}: time is a number of seconds \
                                 or a time spec, name and color are strings"
                            ));
                        }
                        _ => {
                            return Err(format!("setMarker has no '{key}'; use time, name, color"))
                        }
                    }
                }
                if edited
                    .name
                    .chars()
                    .any(|c| c.is_control() && !c.is_whitespace())
                {
                    return Err("marker names cannot hold control characters".to_owned());
                }
                let time = edited.time;
                if let Some(target) = self.timeline.marker_mut(marker) {
                    *target = edited;
                }
                self.timeline.move_marker(marker, time);
                Ok(None)
            }
            ScriptCommand::RemoveMarker(marker) => {
                if !self.timeline.remove_marker(marker) {
                    return Err(format!("no marker {marker}"));
                }
                Ok(None)
            }
            ScriptCommand::ClearMarkers => {
                self.timeline.markers.clear();
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

/// Refuse a time outside the timeline instead of moving it there.
fn marker_time(command: &str, time: f64) -> Result<(), String> {
    if (0.0..=MAX_TIMELINE_SECONDS).contains(&time) {
        Ok(())
    } else {
        Err(format!(
            "{command} needs a time from 0 to {MAX_TIMELINE_SECONDS} s, not {time}"
        ))
    }
}

/// A marker colour by name; the error names the ones that exist.
fn marker_color(name: &str) -> Result<MarkerColor, String> {
    MarkerColor::from_name(name).ok_or_else(|| {
        let known: Vec<&str> = MarkerColor::ALL.iter().map(|c| c.as_str()).collect();
        format!("unknown marker color '{name}'; use {}", known.join(", "))
    })
}

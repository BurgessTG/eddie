//! The clip edits keys, menus and buttons ask for: split, delete, ripple
//! delete, copy, cut, paste, duplicate and freeze frame. Each says in the
//! status bar what it did or why it did nothing. History makes each one an
//! undo step without hooks of its own.

use std::path::Path;

use openreel_engine::{Clip, ClipId, ClipKind};

use crate::Editor;

/// How long a freeze frame holds, as in CapCut.
pub const FREEZE_SECONDS: f64 = 3.0;

fn count(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

impl Editor {
    /// Split at the playhead: the selected clip when the playhead is
    /// inside it, otherwise the topmost clip under the playhead.
    /// Clicking the timeline parks the playhead first, so the flow
    /// is click, then split (button, S, or B).
    pub fn split_at_playhead(&mut self) {
        let playhead = self.playhead;
        let target = self
            .selected
            .filter(|id| {
                self.timeline
                    .clip(*id)
                    .map(|c| c.contains(playhead))
                    .unwrap_or(false)
            })
            .or_else(|| {
                self.timeline
                    .clips
                    .iter()
                    .filter(|c| c.contains(playhead))
                    .min_by_key(|c| self.timeline.row(c.kind, c.track))
                    .map(|c| c.id)
            });
        let Some(id) = target else {
            self.status = "No clip under the playhead.".to_owned();
            return;
        };
        match self.timeline.split_at(id, playhead) {
            Some(new_id) => {
                self.selected = Some(new_id);
                self.status = "Clip split at playhead.".to_owned();
            }
            None => {
                self.status = "Playhead is too close to a clip edge.".to_owned();
            }
        }
    }

    /// Delete a clip from a user action (menu, key, toolbar,
    /// inspector). The gap stays; the magnet only snaps while
    /// dragging and never moves clips on its own. Scripts close
    /// gaps only via the explicit CloseGaps command.
    pub fn delete_clip(&mut self, id: ClipId) {
        self.timeline.remove_clip(id);
        if self.selected == Some(id) {
            self.selected = None;
        }
    }

    /// Delete the selected clip and close its gap
    /// ([`Timeline::ripple_delete`](openreel_engine::Timeline::ripple_delete)).
    pub fn ripple_delete_selected(&mut self) {
        let Some(ripple) = self
            .selected
            .take()
            .and_then(|id| self.timeline.ripple_delete(id))
        else {
            self.status = "Select a clip to ripple delete.".to_owned();
            return;
        };
        let what = match ripple.titles {
            0 => "Clip deleted".to_owned(),
            n => format!("Clip and {} over it deleted", count(n, "title")),
        };
        let gap = match ripple.lanes_kept {
            0 => "the gap closed".to_owned(),
            n => format!(
                "{} under a clip across the gap kept {}",
                count(n, "lane"),
                if n == 1 { "its place" } else { "their place" }
            ),
        };
        self.status = format!("{what}; {gap}.");
    }

    /// Clip `id` and its linked partner, `id` first.
    fn with_partner(&self, id: ClipId) -> Option<Vec<Clip>> {
        let clip = self.timeline.clip(id)?;
        let partner = clip.linked_clip.and_then(|id| self.timeline.clip(id));
        Some(
            [Some(clip), partner]
                .into_iter()
                .flatten()
                .cloned()
                .collect(),
        )
    }

    /// The selected clip and its linked partner, the selected one first.
    fn selected_group(&self) -> Option<Vec<Clip>> {
        self.with_partner(self.selected?)
    }

    /// Copy the selected clip, with its linked sound, for Paste.
    pub fn copy_selected(&mut self) {
        let Some(group) = self.selected_group() else {
            self.status = "Select a clip to copy.".to_owned();
            return;
        };
        self.clipboard = group;
        self.status = "Clip copied.".to_owned();
    }

    /// Copy the selected clip, with its linked sound, and delete both.
    pub fn cut_selected(&mut self) {
        let Some(group) = self.selected_group() else {
            self.status = "Select a clip to cut.".to_owned();
            return;
        };
        for clip in &group {
            self.timeline.remove_clip(clip.id);
        }
        self.selected = None;
        self.clipboard = group;
        self.status = "Clip cut.".to_owned();
    }

    /// Something has been copied or cut.
    pub fn can_paste(&self) -> bool {
        !self.clipboard.is_empty()
    }

    /// Put a copy of what was copied or cut at the playhead and select it.
    pub fn paste(&mut self) {
        if self.clipboard.is_empty() {
            self.status = "Copy a clip first.".to_owned();
            return;
        }
        let gone = self
            .clipboard
            .iter()
            .filter_map(|clip| clip.source.as_deref())
            .find(|source| self.asset(source).is_none());
        if let Some(source) = gone {
            let name = Path::new(source)
                .file_name()
                .map_or(source.into(), |name| name.to_string_lossy());
            self.status = format!("{name} is no longer in the media bin.");
            return;
        }
        let ids = self.timeline.place_copies(&self.clipboard, self.playhead);
        self.selected = ids.first().copied();
        self.status = "Clip pasted at the playhead.".to_owned();
    }

    /// Put a copy of clip `id`, with its linked sound, right after it on
    /// its lane. When that time is taken, a video clip makes room by moving
    /// every later clip along, as a freeze frame does, and a title or sound
    /// takes the first lane of its kind free then. Returns the copy of `id`.
    pub fn duplicate_clip(&mut self, id: ClipId) -> Option<ClipId> {
        let group = self.with_partner(id)?;
        let clip = &group[0];
        let after = clip.end();
        if clip.kind == ClipKind::Video
            && self
                .timeline
                .lane_taken(clip.kind, clip.track, after, after + clip.duration)
        {
            self.timeline.push_later(after, clip.duration);
        }
        self.timeline.place_copies(&group, after).first().copied()
    }

    /// Duplicate the selected clip and select the copy.
    pub fn duplicate_selected(&mut self) {
        let Some(copy) = self.selected.and_then(|id| self.duplicate_clip(id)) else {
            self.status = "Select a clip to duplicate.".to_owned();
            return;
        };
        self.selected = Some(copy);
        self.status = "Clip duplicated.".to_owned();
    }

    /// Freeze the picture at the playhead for [`FREEZE_SECONDS`]: the
    /// selected video clip's when the playhead is on it, else the one
    /// showing there. Selects the freeze frame.
    pub fn freeze_at_playhead(&mut self) {
        let playhead = self.playhead;
        let target = self
            .selected
            .filter(|id| {
                self.timeline
                    .clip(*id)
                    .is_some_and(|clip| clip.kind == ClipKind::Video && clip.contains(playhead))
            })
            .or_else(|| self.timeline.active_video(playhead).map(|clip| clip.id));
        let Some(id) = target else {
            self.status = "No video under the playhead.".to_owned();
            return;
        };
        match self.freeze_frame(id, playhead, FREEZE_SECONDS) {
            Ok(freeze) => {
                self.selected = Some(freeze);
                self.status = format!("Frame frozen for {FREEZE_SECONDS} s.");
            }
            Err(why) => self.status = format!("Cannot freeze this clip: {why}."),
        }
    }

    /// [`Timeline::freeze_frame`](openreel_engine::Timeline::freeze_frame),
    /// or why it cannot: a reason a status line or script error finishes.
    pub fn freeze_frame(&mut self, id: ClipId, at: f64, seconds: f64) -> Result<ClipId, String> {
        let clip = self
            .timeline
            .clip(id)
            .ok_or_else(|| format!("there is no clip {id}"))?;
        if clip.kind != ClipKind::Video || clip.source.is_none() {
            return Err("only video from a file has frames to freeze".to_owned());
        }
        if clip.still() {
            return Err("it is already a still picture".to_owned());
        }
        if !clip.contains(at) {
            return Err(format!(
                "{at} s is not inside it ({} to {} s)",
                clip.start,
                clip.end()
            ));
        }
        if seconds.is_nan() || seconds <= 0.0 {
            return Err(format!("a hold lasts more than 0 s, not {seconds}"));
        }
        self.timeline
            .freeze_frame(id, at, seconds)
            .ok_or_else(|| format!("{at} s is under half a frame from its end"))
    }
}

//! Copies of clips, for duplicate and paste.

use super::Timeline;
use crate::{Clip, ClipId};

impl Timeline {
    /// Add copies of `clips` so the first starts at `start` and the rest
    /// keep their offsets from it, none before zero. Each copy keeps its
    /// look, sound and words, and copies of a linked pair are linked to
    /// each other. A copy lands on its own lane when that lane is free for
    /// its new time, else on the first lane of its kind free then (a new
    /// one below the others when none is). Returns the copies' IDs in the
    /// order of `clips`.
    pub fn place_copies(&mut self, clips: &[Clip], start: f64) -> Vec<ClipId> {
        let Some(first) = clips.first() else {
            return Vec::new();
        };
        let earliest = clips
            .iter()
            .map(|clip| clip.start)
            .fold(f64::INFINITY, f64::min);
        let shift = (start - first.start).max(-earliest);
        let mut ids = Vec::with_capacity(clips.len());
        for clip in clips {
            let start = clip.start + shift;
            let end = start + clip.duration;
            let own = clip.track < self.lanes.count(clip.kind)
                && !self.lane_taken(clip.kind, clip.track, start, end);
            let track = if own {
                clip.track
            } else {
                self.free_lane(clip.kind, start, end)
            };
            let id = self.next_id;
            self.next_id += 1;
            self.clips.push(Clip {
                id,
                start,
                track,
                linked_clip: None,
                ..clip.clone()
            });
            ids.push(id);
        }
        for (index, clip) in clips.iter().enumerate() {
            let partner = clips
                .iter()
                .position(|other| Some(other.id) == clip.linked_clip);
            if let Some(partner) = partner {
                self.link_pair(ids[index], ids[partner]);
            }
        }
        ids
    }
}

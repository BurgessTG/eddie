//! A picture and its detached sound, linked so they edit together.

use super::Timeline;
use crate::{ClipId, ClipKind};

impl Timeline {
    pub(super) fn linked_id(&self, id: ClipId) -> Option<ClipId> {
        self.clip(id)
            .and_then(|clip| clip.linked_clip)
            .filter(|linked| self.clip(*linked).is_some())
    }

    pub(super) fn link_pair(&mut self, left: ClipId, right: ClipId) {
        if let Some(clip) = self.clip_mut(left) {
            clip.linked_clip = Some(right);
        }
        if let Some(clip) = self.clip_mut(right) {
            clip.linked_clip = Some(left);
        }
    }

    /// Detach a video clip's embedded sound into a linked audio
    /// clip on the first lane free for the same span. The engine
    /// does not know whether the source has an audio stream; the
    /// app checks probed metadata before calling this.
    pub fn detach_audio(&mut self, id: ClipId) -> Option<ClipId> {
        let clip = self.clip(id)?.clone();
        let source = clip.source.as_deref()?;
        if clip.kind != ClipKind::Video || !clip.embedded_audio || clip.linked_clip.is_some() {
            return None;
        }
        let mut free_track = None;
        for track in 0..self.track_labels.len() {
            if track == clip.track {
                continue;
            }
            let blocked = self
                .clips
                .iter()
                .any(|c| c.track == track && c.start < clip.end() && clip.start < c.end());
            if !blocked {
                free_track = Some(track);
                break;
            }
        }
        let track = free_track.unwrap_or_else(|| self.append_track());
        let audio_name = format!("{} (audio)", clip.name);
        let audio = self.add_clip_inner(
            &audio_name,
            ClipKind::Audio,
            track,
            clip.start,
            clip.duration,
            clip.source_in,
            Some(source),
        );
        // Everything that shaped the embedded sound goes with it.
        if let Some(sound) = self.clip_mut(audio) {
            sound.volume = clip.volume;
            sound.fade_in = clip.fade_in;
            sound.fade_out = clip.fade_out;
            sound.smooth_fades = clip.smooth_fades;
            sound.clean_voice = clip.clean_voice;
            sound.duck = clip.duck;
        }
        if let Some(picture) = self.clip_mut(id) {
            picture.embedded_audio = false;
            picture.fade_in = 0.0;
            picture.fade_out = 0.0;
            picture.smooth_fades = false;
            picture.clean_voice = false;
            picture.duck = false;
        }
        self.link_pair(id, audio);
        Some(audio)
    }

    /// Break a detach link while keeping both clips.
    pub fn unlink_clip(&mut self, id: ClipId) -> bool {
        let Some(linked) = self.linked_id(id) else {
            // Clear a stale pointer too, but report no live link broke.
            if let Some(clip) = self.clip_mut(id) {
                clip.linked_clip = None;
            }
            return false;
        };
        if let Some(clip) = self.clip_mut(id) {
            clip.linked_clip = None;
        }
        if let Some(clip) = self.clip_mut(linked) {
            if clip.linked_clip == Some(id) {
                clip.linked_clip = None;
            }
        }
        true
    }

    /// Enable or disable a video clip's own embedded sound. It
    /// cannot be re-enabled while a detached partner exists, which
    /// would otherwise play the same source audio twice.
    pub fn set_embedded_audio(&mut self, id: ClipId, enabled: bool) -> bool {
        let Some(clip) = self.clip(id).cloned() else {
            return false;
        };
        if clip.kind != ClipKind::Video
            || clip.source.is_none()
            || (enabled && self.linked_id(id).is_some())
        {
            return false;
        }
        if let Some(clip) = self.clip_mut(id) {
            clip.embedded_audio = enabled;
            true
        } else {
            false
        }
    }
}

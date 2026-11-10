//! The edits the Details panel makes to one clip. A window draws the fields
//! and buttons; the clamps, a linked partner's share of each edit and the
//! status line live here, so every front end behaves alike.

use openreel_engine::{ClipId, ClipKind};

use super::Editor;

/// The shortest a timing field leaves a clip, in seconds.
pub const MIN_DURATION: f64 = 0.1;
/// The most any seconds field holds: one day.
pub const MAX_SECONDS: f64 = 86_400.0;
/// Clip gain tops out at 200%, for the Details slider and scripts'
/// `setVolume` alike. A narrower slider would rewrite a script's louder
/// clip the moment it was shown.
pub const MAX_VOLUME: f32 = 2.0;

impl Editor {
    /// The Start field: move the clip on the project frame grid. A linked
    /// partner moves by the same amount.
    pub fn set_clip_start(&mut self, id: ClipId, start: f64) {
        if start.is_finite() {
            self.timeline.move_clip(id, start.clamp(0.0, MAX_SECONDS));
        }
    }

    /// The Duration field: at least `MIN_DURATION`, and no longer than the
    /// source has left after the clip's source-in.
    pub fn set_clip_duration(&mut self, id: ClipId, duration: f64) {
        let Some(source_in) = self.timeline.clip(id).map(|clip| clip.source_in) else {
            return;
        };
        if duration.is_finite() {
            self.apply_timing(id, duration.clamp(MIN_DURATION, MAX_SECONDS), source_in);
        }
    }

    /// The Source in field: inside the source file. The clip shortens when
    /// less of the source is left; a clip already shorter than
    /// `MIN_DURATION` (a one-frame scene) keeps its length.
    pub fn set_clip_source_in(&mut self, id: ClipId, source_in: f64) {
        let Some(duration) = self.timeline.clip(id).map(|clip| clip.duration) else {
            return;
        };
        if source_in.is_finite() {
            self.apply_timing(id, duration, source_in.clamp(0.0, MAX_SECONDS));
        }
    }

    /// Keep both values inside the source file, and give a linked partner
    /// the same change so the pair keeps covering the same media.
    fn apply_timing(&mut self, id: ClipId, mut duration: f64, mut source_in: f64) {
        let Some(before) = self.timeline.clip(id).cloned() else {
            return;
        };
        let source_duration = before
            .source
            .as_deref()
            .and_then(|source| self.asset(source))
            .map(|asset| asset.info.duration);
        if let Some(longest) = source_duration {
            source_in = source_in.min(longest.max(0.0));
            duration = duration.min((longest - source_in).max(MIN_DURATION));
        }
        let linked_before = before
            .linked_clip
            .and_then(|linked| self.timeline.clip(linked).cloned());
        if let Some(clip) = self.timeline.clip_mut(id) {
            clip.duration = duration;
            clip.source_in = source_in;
        }
        let duration_delta = duration - before.duration;
        let source_delta = source_in - before.source_in;
        if duration_delta == 0.0 && source_delta == 0.0 {
            return;
        }
        if let Some(linked_before) = linked_before {
            let shortest = 1.0 / self.timeline.fps;
            if let Some(linked) = self.timeline.clip_mut(linked_before.id) {
                linked.duration = (linked_before.duration + duration_delta).max(shortest);
                linked.source_in = (linked_before.source_in + source_delta).max(0.0);
            }
        }
    }

    /// The Opacity slider and its field: 0 is black, 1 the whole picture.
    pub fn set_clip_opacity(&mut self, id: ClipId, opacity: f32) {
        if !opacity.is_finite() {
            return;
        }
        if let Some(clip) = self.timeline.clip_mut(id) {
            clip.opacity = opacity.clamp(0.0, 1.0);
        }
    }

    /// The Volume slider and its field: 1 is unity gain.
    pub fn set_clip_volume(&mut self, id: ClipId, volume: f32) {
        if !volume.is_finite() {
            return;
        }
        if let Some(clip) = self.timeline.clip_mut(id) {
            clip.volume = volume.clamp(0.0, MAX_VOLUME);
        }
    }

    /// The Fades fields, in seconds; `None` leaves that fade as it is.
    pub fn set_clip_fades(&mut self, id: ClipId, fade_in: Option<f64>, fade_out: Option<f64>) {
        let Some(clip) = self.timeline.clip_mut(id) else {
            return;
        };
        if let Some(seconds) = fade_in.filter(|seconds| seconds.is_finite()) {
            clip.fade_in = seconds.clamp(0.0, MAX_SECONDS);
        }
        if let Some(seconds) = fade_out.filter(|seconds| seconds.is_finite()) {
            clip.fade_out = seconds.clamp(0.0, MAX_SECONDS);
        }
    }

    /// Whether the clip's source file has sound.
    pub fn clip_source_has_audio(&self, id: ClipId) -> bool {
        self.timeline
            .clip(id)
            .and_then(|clip| clip.source.as_deref())
            .and_then(|source| self.asset(source))
            .is_some_and(|asset| asset.info.has_audio)
    }

    /// Detach audio: move a video clip's sound to a linked audio clip and
    /// select it.
    pub fn detach_clip_audio(&mut self, id: ClipId) {
        if !self.clip_source_has_audio(id) {
            self.status = "This clip has no detachable audio.".to_owned();
            return;
        }
        match self.timeline.detach_audio(id) {
            Some(audio) => {
                self.selected = Some(audio);
                self.status = "Audio detached.".to_owned();
            }
            None => self.status = "This clip's audio is already detached.".to_owned(),
        }
    }

    /// Unlink: let picture and detached sound move apart.
    pub fn unlink_clip(&mut self, id: ClipId) {
        self.status = if self.timeline.unlink_clip(id) {
            "Picture and audio unlinked.".to_owned()
        } else {
            "This clip is not linked.".to_owned()
        };
    }

    /// Captions → Add: caption the clip's transcript in the chosen style.
    pub fn add_clip_captions(&mut self, id: ClipId) {
        self.status = match self.add_captions_for_clip(id, self.caption_style) {
            Ok(0) => "Captions already added or none overlap.".to_owned(),
            Ok(count) => format!(
                "Added {count} caption{}.",
                if count == 1 { "" } else { "s" }
            ),
            Err(error) => error,
        };
    }

    /// Remove silence: cut the pauses from a speech clip and ripple what
    /// follows.
    pub fn remove_clip_silence(&mut self, id: ClipId) {
        self.status = match self.tighten_clip_silence(id, None, 0.35, 0.08) {
            Ok(1) => "No removable silence found.".to_owned(),
            Ok(pieces) => format!("Removed silence into {pieces} phrases."),
            Err(error) => error,
        };
    }

    /// Split at playhead, for this clip only; the right-hand piece becomes
    /// the selection.
    pub fn split_clip_at_playhead(&mut self, id: ClipId) {
        match self.timeline.split_at(id, self.playhead) {
            Some(right) => {
                self.selected = Some(right);
                self.status = "Clip split at playhead.".to_owned();
            }
            None => self.status = "Playhead is not inside the selected clip.".to_owned(),
        }
    }

    /// What Details offers for a clip beyond its fields.
    pub fn clip_details(&self, id: ClipId) -> Option<ClipDetails> {
        let clip = self.timeline.clip(id)?;
        let asset = clip.source.as_deref().and_then(|source| self.asset(source));
        let source_has_audio = asset.is_some_and(|asset| asset.info.has_audio);
        let video_audio = clip.kind == ClipKind::Video && source_has_audio;
        let linked = clip
            .linked_clip
            .and_then(|linked| self.timeline.clip(linked));
        Some(ClipDetails {
            has_source: clip.source.is_some(),
            embedded_audio_choice: video_audio && clip.linked_clip.is_none(),
            can_detach: video_audio && clip.embedded_audio && clip.linked_clip.is_none(),
            can_remove_silence: source_has_audio && clip.kind != ClipKind::Title,
            can_caption: clip.kind != ClipKind::Title
                && asset.is_some_and(|asset| !asset.sweep.transcript.is_empty()),
            linked_name: linked.map(|partner| partner.name.clone()),
        })
    }
}

/// Which of Details' controls apply to one clip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipDetails {
    pub has_source: bool,
    /// A video clip whose own sound can be switched on and off.
    pub embedded_audio_choice: bool,
    pub can_detach: bool,
    pub can_remove_silence: bool,
    pub can_caption: bool,
    /// The detached partner's name, while the link is live.
    pub linked_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::media::{AssetState, BinAsset};
    use crate::editor::sweep;

    /// An editor whose bin holds a 10 s video with sound, placed from 1 s
    /// of the source for 4 s.
    fn editor_with_video() -> (Editor, ClipId) {
        let mut editor = Editor::headless();
        editor.assets.push(BinAsset {
            info: openreel_media::MediaInfo {
                path: "/media/a.mp4".to_owned(),
                name: "a.mp4".to_owned(),
                duration: 10.0,
                width: 320,
                height: 180,
                fps: 30.0,
                has_video: true,
                has_audio: true,
            },
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumbs: Vec::new(),
            sweep: sweep::SweepData::default(),
        });
        let video = editor.timeline.add_media_clip(
            "Picture",
            ClipKind::Video,
            0,
            2.0,
            4.0,
            1.0,
            "/media/a.mp4",
        );
        (editor, video)
    }

    fn timing(editor: &Editor, id: ClipId) -> (f64, f64) {
        let clip = editor.timeline.clip(id).unwrap();
        (clip.duration, clip.source_in)
    }

    #[test]
    fn timing_stays_inside_the_source_and_a_linked_partner_follows() {
        let (mut editor, video) = editor_with_video();
        let audio = editor.timeline.detach_audio(video).unwrap();

        // Longer than the source has left from 1 s in.
        editor.set_clip_duration(video, 20.0);
        assert_eq!(timing(&editor, video), (9.0, 1.0));
        assert_eq!(timing(&editor, audio), (9.0, 1.0));

        // Starting later leaves less of the source to play.
        editor.set_clip_source_in(audio, 8.0);
        assert_eq!(timing(&editor, audio), (2.0, 8.0));
        assert_eq!(timing(&editor, video), (2.0, 8.0));

        // Past the end of the source, the shortest clip is kept.
        editor.set_clip_source_in(video, 30.0);
        assert_eq!(timing(&editor, video), (MIN_DURATION, 10.0));
        let (linked_duration, linked_source_in) = timing(&editor, audio);
        assert!((linked_duration - MIN_DURATION).abs() < 1e-9);
        assert_eq!(linked_source_in, 10.0);

        editor.set_clip_duration(video, f64::NAN);
        editor.set_clip_source_in(video, f64::INFINITY);
        assert_eq!(timing(&editor, video), (MIN_DURATION, 10.0));
    }

    #[test]
    fn a_one_frame_scene_keeps_its_length_until_its_duration_is_edited() {
        let (mut editor, _) = editor_with_video();
        let scene = editor.timeline.add_media_clip(
            "Insert",
            ClipKind::Video,
            1,
            0.0,
            0.03,
            1.0,
            "/media/a.mp4",
        );
        editor.set_clip_source_in(scene, 2.0);
        assert_eq!(timing(&editor, scene), (0.03, 2.0));
        editor.set_clip_duration(scene, 0.05);
        assert_eq!(timing(&editor, scene), (MIN_DURATION, 2.0));
    }

    #[test]
    fn a_title_is_bounded_only_by_the_shortest_duration() {
        let mut editor = Editor::headless();
        let title = editor
            .timeline
            .add_clip("Hello", ClipKind::Title, 0, 0.0, 3.0, 0.0);
        editor.set_clip_duration(title, 0.01);
        assert_eq!(timing(&editor, title), (MIN_DURATION, 0.0));
        editor.set_clip_duration(title, 500.0);
        assert_eq!(timing(&editor, title), (500.0, 0.0));
    }

    #[test]
    fn start_moves_a_linked_pair_together() {
        let (mut editor, video) = editor_with_video();
        let audio = editor.timeline.detach_audio(video).unwrap();
        editor.set_clip_start(audio, 5.0);
        assert_eq!(editor.timeline.clip(video).unwrap().start, 5.0);
        assert_eq!(editor.timeline.clip(audio).unwrap().start, 5.0);
        editor.set_clip_start(video, -3.0);
        assert_eq!(editor.timeline.clip(audio).unwrap().start, 0.0);
    }

    #[test]
    fn details_keeps_the_loudest_gain_a_script_can_set() {
        use crate::scripting::ScriptCommand;

        let (mut editor, video) = editor_with_video();
        let louder = ScriptCommand::SetVolume {
            clip: video,
            value: 9.0,
        };
        editor.apply_edit(vec![louder]).unwrap();
        let scripted = editor.timeline.clip(video).unwrap().volume;
        assert_eq!(scripted, 2.0);
        // Showing the clip and applying what the field reads back is no edit.
        editor.set_clip_volume(video, scripted);
        assert_eq!(editor.timeline.clip(video).unwrap().volume, scripted);
    }

    #[test]
    fn gain_opacity_and_fades_are_clamped() {
        let (mut editor, video) = editor_with_video();
        editor.set_clip_volume(video, 4.0);
        editor.set_clip_opacity(video, -0.5);
        editor.set_clip_fades(video, Some(-1.0), None);
        let clip = editor.timeline.clip(video).unwrap();
        assert_eq!((clip.volume, clip.opacity), (MAX_VOLUME, 0.0));
        assert_eq!((clip.fade_in, clip.fade_out), (0.0, 0.0));
        editor.set_clip_fades(video, None, Some(1.25));
        let clip = editor.timeline.clip(video).unwrap();
        assert_eq!((clip.fade_in, clip.fade_out), (0.0, 1.25));
    }

    #[test]
    fn detach_and_unlink_say_what_happened() {
        let (mut editor, video) = editor_with_video();
        let before = editor.clip_details(video).unwrap();
        assert!(before.embedded_audio_choice && before.can_detach && before.can_remove_silence);
        assert!(!before.can_caption && before.linked_name.is_none());

        editor.detach_clip_audio(video);
        assert_eq!(editor.status, "Audio detached.");
        let audio = editor.selected.expect("the new audio clip is selected");
        assert_eq!(editor.timeline.clip(audio).unwrap().kind, ClipKind::Audio);
        let linked = editor.clip_details(video).unwrap();
        assert!(!linked.embedded_audio_choice && !linked.can_detach);
        // Each side names the other: the engine calls the new clip
        // "<picture> (audio)".
        assert_eq!(linked.linked_name.as_deref(), Some("Picture (audio)"));
        assert_eq!(
            editor.clip_details(audio).unwrap().linked_name.as_deref(),
            Some("Picture")
        );

        editor.detach_clip_audio(video);
        assert_eq!(editor.status, "This clip's audio is already detached.");

        editor.unlink_clip(audio);
        assert_eq!(editor.status, "Picture and audio unlinked.");
        editor.unlink_clip(audio);
        assert_eq!(editor.status, "This clip is not linked.");

        let title = editor
            .timeline
            .add_clip("Hello", ClipKind::Title, 1, 0.0, 3.0, 0.0);
        editor.detach_clip_audio(title);
        assert_eq!(editor.status, "This clip has no detachable audio.");
        let details = editor.clip_details(title).unwrap();
        assert!(!details.has_source && !details.can_remove_silence && !details.can_caption);
    }

    #[test]
    fn split_needs_the_playhead_inside_the_clip() {
        let (mut editor, video) = editor_with_video();
        editor.selected = Some(video);
        editor.playhead = 1.0;
        editor.split_clip_at_playhead(video);
        assert_eq!(editor.status, "Playhead is not inside the selected clip.");
        assert_eq!(editor.selected, Some(video));

        editor.playhead = 3.0;
        editor.split_clip_at_playhead(video);
        assert_eq!(editor.status, "Clip split at playhead.");
        let right = editor.selected.unwrap();
        assert_ne!(right, video);
        assert_eq!(editor.timeline.clip(right).unwrap().start, 3.0);
        assert_eq!(editor.timeline.clip(video).unwrap().duration, 1.0);
    }
}

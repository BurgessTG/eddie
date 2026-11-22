//! The edits the Details panel makes to one clip. A window draws the fields
//! and buttons; the clamps, a linked partner's share of each edit and the
//! status line live here, so every front end behaves alike.

use openreel_engine::{
    fill_scale, Background, BlendMode, ChromaKey, ClipId, ClipKind, ColorAdjust, Mask, Property,
    TextLook, Transform, Transition, MAX_ROTATION, MAX_SCALE, MAX_SPEED, MIN_CROP_KEEP, MIN_SCALE,
    MIN_SPEED,
};

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
        // A still has no source range to run out of.
        let source_duration = before
            .source
            .as_deref()
            .filter(|_| !before.still())
            .and_then(|source| self.asset(source))
            .map(|asset| asset.info.duration);
        if let Some(longest) = source_duration {
            source_in = source_in.min(longest.max(0.0));
            duration = duration.min(((longest - source_in) / before.speed).max(MIN_DURATION));
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

    /// The Speed slider and its field: `MIN_SPEED` to `MAX_SPEED` times as
    /// fast. The clip keeps its source range and its length follows, so a
    /// slider dragged down and back loses nothing. Slowing stops at the
    /// slowest speed whose length still fits before the next clip on the
    /// lane, and the status bar says so.
    pub fn set_clip_speed(&mut self, id: ClipId, speed: f64) {
        let Some(clip) = self.timeline.clip(id) else {
            return;
        };
        if !speed.is_finite()
            || clip.kind == ClipKind::Title
            || clip.source.is_none()
            || clip.still()
        {
            return;
        }
        let range = clip.duration * clip.speed;
        let room = self.timeline.room_after_start(id);
        let fits = (range / room).clamp(MIN_SPEED, MAX_SPEED);
        let mut speed = speed.clamp(MIN_SPEED, MAX_SPEED);
        if speed < fits {
            // Hundredths, as the field shows, rounded up so it still fits.
            speed = ((fits * 100.0).ceil() / 100.0).min(MAX_SPEED);
            self.status = format!("Slowest that fits before the next clip: {speed:.2}×.");
        }
        self.timeline.set_speed(id, speed);
    }

    /// The Opacity slider and its field: 0 is black, 1 the whole picture.
    /// Once opacity has keys, this sets the key at the playhead.
    pub fn set_clip_opacity(&mut self, id: ClipId, opacity: f32) {
        if opacity.is_finite() {
            let value = openreel_engine::widen(opacity.clamp(0.0, 1.0));
            self.write_property(id, Property::Opacity, value);
        }
    }

    /// The Volume slider and its field: 1 is unity gain. Once volume has
    /// keys, this sets the key at the playhead.
    pub fn set_clip_volume(&mut self, id: ClipId, volume: f32) {
        if volume.is_finite() {
            let value = openreel_engine::widen(volume.clamp(0.0, MAX_VOLUME));
            self.write_property(id, Property::Volume, value);
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

    /// The Transform card: change a video clip's place on the canvas, kept
    /// inside what preview and export draw. Scale stays 5-2000%, position
    /// within ten canvases, rotation 0-360 and each crop leaves 5% of the
    /// picture. `edit` sees the transform at the playhead; a position,
    /// scale or rotation with keys gets a key there when it changes, and
    /// keyed rotation may turn past 360.
    pub fn edit_clip_transform(&mut self, id: ClipId, edit: impl FnOnce(&mut Transform)) {
        let Some(clip) = self
            .timeline
            .clip(id)
            .filter(|clip| clip.kind == ClipKind::Video)
        else {
            return;
        };
        let here = clip.transform_at(self.playhead);
        let keys = clip.keyframes.clone();
        let mut t = here;
        edit(&mut t);
        t.scale = t.scale.clamp(MIN_SCALE, MAX_SCALE);
        t.x = t.x.clamp(-10.0, 10.0);
        t.y = t.y.clamp(-10.0, 10.0);
        t.rotation = if keys.rotation.is_empty() {
            t.rotation.rem_euclid(360.0)
        } else {
            t.rotation.clamp(-MAX_ROTATION, MAX_ROTATION)
        };
        // A hair under the limit, so the sum stays inside it after rounding.
        let most = |other: f64| (1.0 - MIN_CROP_KEEP - other - 1e-9).max(0.0);
        t.crop_left = t.crop_left.clamp(0.0, most(t.crop_right));
        t.crop_right = t.crop_right.clamp(0.0, most(t.crop_left));
        t.crop_top = t.crop_top.clamp(0.0, most(t.crop_bottom));
        t.crop_bottom = t.crop_bottom.clamp(0.0, most(t.crop_top));
        if !t.is_valid() {
            return;
        }
        let at = self.clip_time(id).unwrap_or(0.0);
        let Some(clip) = self.timeline.clip_mut(id) else {
            return;
        };
        let mut plain = t;
        let moved = [
            (Property::X, here.x, t.x, &mut plain.x, clip.transform.x),
            (Property::Y, here.y, t.y, &mut plain.y, clip.transform.y),
            (
                Property::Scale,
                here.scale,
                t.scale,
                &mut plain.scale,
                clip.transform.scale,
            ),
            (
                Property::Rotation,
                here.rotation,
                t.rotation,
                &mut plain.rotation,
                clip.transform.rotation,
            ),
        ];
        for (property, before, after, field, kept) in moved {
            if keys.keys(property).is_empty() {
                continue;
            }
            // A keyed property keeps its plain value and changes by key,
            // only when this edit changed it.
            *field = kept;
            if after != before {
                clip.keyframes.set(property, at, after);
            }
        }
        clip.transform = plain;
    }

    /// Fit and Fill: centre the picture, whole inside the canvas or
    /// covering all of it.
    pub fn fit_clip(&mut self, id: ClipId, fill: bool) {
        let (w, h) = self.canvas();
        let canvas = (f64::from(w), f64::from(h));
        let source = self
            .timeline
            .clip(id)
            .and_then(|clip| self.asset(clip.source.as_deref()?))
            .map_or(canvas, |asset| {
                (f64::from(asset.info.width), f64::from(asset.info.height))
            });
        self.edit_clip_transform(id, |t| {
            t.scale = if fill {
                fill_scale(canvas, source, t)
            } else {
                1.0
            };
            t.x = 0.0;
            t.y = 0.0;
        });
    }

    /// What fills the canvas around a video clip's picture.
    pub fn set_clip_background(&mut self, id: ClipId, background: Background) {
        if let Some(clip) = self
            .timeline
            .clip_mut(id)
            .filter(|clip| clip.kind == ClipKind::Video)
        {
            clip.background = background;
        }
    }

    /// How a video clip's picture mixes with the lanes under it.
    pub fn set_clip_blend(&mut self, id: ClipId, blend: BlendMode) {
        if let Some(clip) = self
            .timeline
            .clip_mut(id)
            .filter(|clip| clip.kind == ClipKind::Video)
        {
            clip.blend = blend;
        }
    }

    /// The Chroma key card: key a video clip (`None` for no key) or change
    /// its key, each amount moved into its range.
    pub fn edit_clip_chroma(&mut self, id: ClipId, edit: impl FnOnce(&mut Option<ChromaKey>)) {
        if let Some(clip) = self
            .timeline
            .clip_mut(id)
            .filter(|clip| clip.kind == ClipKind::Video)
        {
            let mut chroma = clip.chroma;
            edit(&mut chroma);
            clip.chroma = chroma.map(ChromaKey::clamped);
        }
    }

    /// The Mask card: mask a video clip (`None` for no mask) or change its
    /// mask, each number moved into its range.
    pub fn edit_clip_mask(&mut self, id: ClipId, edit: impl FnOnce(&mut Option<Mask>)) {
        if let Some(clip) = self
            .timeline
            .clip_mut(id)
            .filter(|clip| clip.kind == ClipKind::Video)
        {
            let mut mask = clip.mask;
            edit(&mut mask);
            clip.mask = mask.map(Mask::clamped);
        }
    }

    /// The Colour card: change a video clip's adjustments and filter, each
    /// moved into its range.
    pub fn edit_clip_color(&mut self, id: ClipId, edit: impl FnOnce(&mut ColorAdjust)) {
        if let Some(clip) = self
            .timeline
            .clip_mut(id)
            .filter(|clip| clip.kind == ClipKind::Video)
        {
            let mut color = clip.color;
            edit(&mut color);
            clip.color = color.clamped();
        }
    }

    /// The Transition card: a video clip's way in from the clip right
    /// before it, or none. A clip with nothing before it gets none.
    pub fn set_clip_transition(&mut self, id: ClipId, transition: Option<Transition>) {
        let before = self.timeline.clip_before(id).is_some();
        if let Some(clip) = self
            .timeline
            .clip_mut(id)
            .filter(|clip| clip.kind == ClipKind::Video)
        {
            clip.transition = transition
                .filter(|_| before)
                .map(|t| Transition::new(t.kind, t.duration));
        }
    }

    /// The Text card: change a title's size, colours, box, outline or
    /// place, each moved into its range.
    pub fn edit_title_text(&mut self, id: ClipId, edit: impl FnOnce(&mut TextLook)) {
        if let Some(clip) = self
            .timeline
            .clip_mut(id)
            .filter(|clip| clip.kind == ClipKind::Title)
        {
            let mut text = clip.text;
            edit(&mut text);
            clip.text = text.clamped();
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
        // A freeze frame is a silent picture: its file's sound and
        // transcript are not its own.
        let asset = clip
            .source
            .as_deref()
            .filter(|_| !clip.freeze)
            .and_then(|source| self.asset(source));
        let source_has_audio = asset.is_some_and(|asset| asset.info.has_audio);
        let video_audio = clip.kind == ClipKind::Video && source_has_audio;
        let linked = clip
            .linked_clip
            .and_then(|linked| self.timeline.clip(linked));
        Some(ClipDetails {
            has_source: clip.source.is_some(),
            has_sound: source_has_audio,
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
    /// Its file has sound, so Volume does something (not for a still).
    pub has_sound: bool,
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
                photo: false,
                alpha: false,
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
    fn transform_edits_stay_drawable_and_fill_covers_the_canvas() {
        let (mut editor, video) = editor_with_video();
        let transform = |editor: &Editor| editor.timeline.clip(video).unwrap().transform;
        editor.edit_clip_transform(video, |t| {
            t.scale = 0.0;
            t.rotation = -90.0;
            t.crop_right = 0.1;
            t.crop_left = 0.9;
        });
        let t = transform(&editor);
        assert_eq!((t.scale, t.rotation), (MIN_SCALE, 270.0));
        assert!((t.crop_right - 0.1).abs() < 1e-6, "{t:?}");
        assert!((t.crop_left - 0.85).abs() < 1e-6 && t.is_valid(), "{t:?}");

        // A square canvas from 320x180: 180x180. Fill covers it; Fit
        // brings back the whole (cropped) picture, centred.
        editor.edit_clip_transform(video, |t| {
            *t = Transform {
                x: 0.3,
                ..Transform::default()
            }
        });
        editor.set_format(openreel_engine::Format::Square);
        assert_eq!(editor.canvas(), (180, 180));
        editor.fit_clip(video, true);
        let t = transform(&editor);
        assert!(
            (t.scale - 320.0 / 180.0).abs() < 1e-9 && t.x == 0.0,
            "{t:?}"
        );
        let placed = openreel_engine::place((180.0, 180.0), (320.0, 180.0), &t);
        assert!(placed.covers((180.0, 180.0)));
        editor.fit_clip(video, false);
        assert!(transform(&editor).is_identity());

        // Only pictures move.
        let audio = editor.timeline.detach_audio(video).unwrap();
        editor.edit_clip_transform(audio, |t| t.scale = 2.0);
        editor.set_clip_background(audio, Background::Blur);
        let audio = editor.timeline.clip(audio).unwrap();
        assert!(audio.transform.is_identity() && audio.background == Background::Black);
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

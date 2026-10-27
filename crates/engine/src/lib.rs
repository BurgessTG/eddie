//! UI-independent timeline model.
//!
//! All times are seconds (`f64`), always snapped to the timeline frame grid
//! at edit boundaries so clips can never land between frames.

pub type ClipId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipKind {
    Video,
    Audio,
    Title,
}

#[derive(Clone, Debug)]
pub struct Clip {
    pub id: ClipId,
    pub name: String,
    pub kind: ClipKind,
    pub track: usize,
    /// Start on the timeline, seconds.
    pub start: f64,
    pub duration: f64,
    /// In-point inside the source media, seconds.
    pub source_in: f64,
    /// Source media file path. `None` for generated clips (titles).
    pub source: Option<String>,
    /// Video/title opacity, 0..=1.
    pub opacity: f32,
    /// Audio gain, 0..=2 (1 = unity). Applies to audio and embedded audio.
    pub volume: f32,
}

impl Clip {
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }

    pub fn contains(&self, time: f64) -> bool {
        time >= self.start && time < self.end()
    }
}

#[derive(Clone, Debug)]
pub struct Timeline {
    pub fps: f64,
    pub track_labels: Vec<String>,
    pub clips: Vec<Clip>,
    next_id: ClipId,
}

impl Timeline {
    pub fn new(fps: f64) -> Self {
        Self {
            fps,
            track_labels: Vec::new(),
            clips: Vec::new(),
            next_id: 1,
        }
    }

    /// Snap a time to the nearest frame boundary.
    pub fn snap(&self, time: f64) -> f64 {
        (time * self.fps).round() / self.fps
    }

    pub fn add_track(&mut self, label: &str) -> usize {
        self.track_labels.push(label.to_owned());
        self.track_labels.len() - 1
    }

    /// Add a generated clip with no media file (e.g. a title).
    pub fn add_clip(
        &mut self,
        name: &str,
        kind: ClipKind,
        track: usize,
        start: f64,
        duration: f64,
        source_in: f64,
    ) -> ClipId {
        self.add_clip_inner(name, kind, track, start, duration, source_in, None)
    }

    /// Add a clip backed by a media file at `source_path`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_media_clip(
        &mut self,
        name: &str,
        kind: ClipKind,
        track: usize,
        start: f64,
        duration: f64,
        source_in: f64,
        source_path: &str,
    ) -> ClipId {
        self.add_clip_inner(
            name,
            kind,
            track,
            start,
            duration,
            source_in,
            Some(source_path),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn add_clip_inner(
        &mut self,
        name: &str,
        kind: ClipKind,
        track: usize,
        start: f64,
        duration: f64,
        source_in: f64,
        source: Option<&str>,
    ) -> ClipId {
        let id = self.next_id;
        self.next_id += 1;
        self.clips.push(Clip {
            id,
            name: name.to_owned(),
            kind,
            track,
            start: self.snap(start.max(0.0)),
            duration: self.snap(duration.max(1.0 / self.fps)),
            source_in: source_in.max(0.0),
            source: source.map(str::to_owned),
            opacity: 1.0,
            volume: 1.0,
        });
        id
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// End of the last clip, or 0 for an empty timeline.
    pub fn end(&self) -> f64 {
        self.clips.iter().map(Clip::end).fold(0.0, f64::max)
    }

    /// Move a clip. Start is clamped to >= 0 and snapped to a frame.
    pub fn move_clip(&mut self, id: ClipId, new_start: f64) {
        let snapped = self.snap(new_start.max(0.0));
        if let Some(clip) = self.clip_mut(id) {
            clip.start = snapped;
        }
    }

    /// Trim the left edge to `new_start`, keeping the end fixed.
    /// `source_in` moves with the edge and clamps at 0 (start of media).
    pub fn trim_left(&mut self, id: ClipId, new_start: f64) {
        let frame = 1.0 / self.fps;
        let Some(clip) = self.clip(id).cloned() else {
            return;
        };
        let min_start = (clip.start - clip.source_in).max(0.0);
        let max_start = clip.end() - frame;
        let desired = self.snap(new_start.clamp(min_start, max_start.max(min_start)));
        let delta = desired - clip.start;
        if let Some(clip) = self.clip_mut(id) {
            clip.start = desired;
            clip.source_in = (clip.source_in + delta).max(0.0);
            clip.duration = (clip.duration - delta).max(frame);
        }
    }

    /// Trim the right edge to `new_end`. The caller clamps against the
    /// source media duration; the model only enforces one frame minimum.
    pub fn trim_right(&mut self, id: ClipId, new_end: f64) {
        let frame = 1.0 / self.fps;
        let snapped = self.snap(new_end);
        if let Some(clip) = self.clip_mut(id) {
            clip.duration = (snapped - clip.start).max(frame);
        }
    }

    /// Split a clip at `time`. Returns the new (right-hand) clip id.
    /// No-op (`None`) if the point is not strictly inside the clip.
    pub fn split_at(&mut self, id: ClipId, time: f64) -> Option<ClipId> {
        let at = self.snap(time);
        let clip = self.clip(id)?.clone();
        let frame = 1.0 / self.fps;
        if at <= clip.start + frame / 2.0 || at >= clip.end() - frame / 2.0 {
            return None;
        }
        let left_duration = at - clip.start;
        if let Some(left) = self.clip_mut(id) {
            left.duration = left_duration;
        }
        let right = self.add_clip_inner(
            &clip.name,
            clip.kind,
            clip.track,
            at,
            clip.end() - at,
            clip.source_in + left_duration,
            clip.source.as_deref(),
        );
        if let Some(right_clip) = self.clip_mut(right) {
            right_clip.opacity = clip.opacity;
            right_clip.volume = clip.volume;
        }
        Some(right)
    }

    /// Magnetic main-track behavior: shift clips on `track` left so no
    /// gaps remain between them. The first clip keeps its start.
    pub fn close_gaps(&mut self, track: usize) {
        let mut ordered: Vec<(f64, ClipId)> = self
            .clips
            .iter()
            .filter(|c| c.track == track)
            .map(|c| (c.start, c.id))
            .collect();
        ordered.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut cursor: Option<f64> = None;
        for (_, id) in ordered {
            if let Some(clip) = self.clip_mut(id) {
                if let Some(expected) = cursor {
                    if clip.start > expected {
                        clip.start = expected;
                    }
                }
                cursor = Some(clip.end());
            }
        }
    }

    pub fn remove_clip(&mut self, id: ClipId) {
        self.clips.retain(|c| c.id != id);
    }

    /// Topmost video/title clip active at `time` (lowest track index wins).
    pub fn visible_clip_at(&self, time: f64) -> Option<&Clip> {
        self.clips
            .iter()
            .filter(|c| c.kind != ClipKind::Audio && c.contains(time))
            .min_by_key(|c| c.track)
    }

    /// Standard empty project: titles over video over audio.
    pub fn empty_project() -> Self {
        let mut tl = Self::new(30.0);
        tl.add_track("V2 · Titles");
        tl.add_track("V1 · Video");
        tl.add_track("A1 · Audio");
        tl
    }

    /// Fictional project kept for model tests.
    pub fn demo() -> Self {
        let mut tl = Self::new(30.0);
        let titles = tl.add_track("V2 · Titles");
        let video = tl.add_track("V1 · Video");
        let audio = tl.add_track("A1 · Audio");
        tl.add_clip(
            "A-Roll_Interview.mp4",
            ClipKind::Video,
            video,
            0.0,
            8.0,
            12.0,
        );
        tl.add_clip("B-Roll_City.mp4", ClipKind::Video, video, 8.0, 6.0, 3.0);
        tl.add_clip("Drone_Coast.mp4", ClipKind::Video, video, 14.0, 7.0, 0.0);
        tl.add_clip("Lower Third — Name", ClipKind::Title, titles, 1.0, 4.0, 0.0);
        tl.add_clip("Music_Bed.wav", ClipKind::Audio, audio, 0.0, 21.0, 0.0);
        tl
    }
}

/// Format seconds as `HH:MM:SS:FF` at the given frame rate.
pub fn format_timecode(seconds: f64, fps: f64) -> String {
    let total_frames = (seconds.max(0.0) * fps).round() as u64;
    let f = fps.round() as u64;
    let frames = total_frames % f;
    let total_seconds = total_frames / f;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        total_seconds / 3600,
        (total_seconds % 3600) / 60,
        total_seconds % 60,
        frames
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_aligns_to_frame_grid() {
        let tl = Timeline::new(30.0);
        assert!((tl.snap(1.01) - 1.0).abs() < 1e-9);
        assert!((tl.snap(1.02) - 31.0 / 30.0).abs() < 1e-9);
    }

    #[test]
    fn move_clamps_to_zero() {
        let mut tl = Timeline::demo();
        let id = tl.clips[0].id;
        tl.move_clip(id, -5.0);
        assert_eq!(tl.clip(id).unwrap().start, 0.0);
    }

    #[test]
    fn split_preserves_total_coverage() {
        let mut tl = Timeline::new(30.0);
        let track = tl.add_track("V1");
        let id = tl.add_clip("a.mp4", ClipKind::Video, track, 2.0, 10.0, 5.0);
        let right = tl.split_at(id, 6.0).expect("split inside clip");
        let left = tl.clip(id).unwrap().clone();
        let right = tl.clip(right).unwrap().clone();
        assert_eq!(left.end(), 6.0);
        assert_eq!(right.start, 6.0);
        assert_eq!(right.end(), 12.0);
        assert_eq!(right.source_in, 9.0);
    }

    #[test]
    fn split_outside_clip_is_noop() {
        let mut tl = Timeline::new(30.0);
        let track = tl.add_track("V1");
        let id = tl.add_clip("a.mp4", ClipKind::Video, track, 2.0, 4.0, 0.0);
        assert!(tl.split_at(id, 99.0).is_none());
        assert_eq!(tl.clips.len(), 1);
    }

    #[test]
    fn timecode_formats_frames() {
        assert_eq!(format_timecode(0.0, 30.0), "00:00:00:00");
        assert_eq!(format_timecode(61.5, 30.0), "00:01:01:15");
        assert_eq!(format_timecode(3661.0, 25.0), "01:01:01:00");
    }

    #[test]
    fn demo_ends_at_last_clip() {
        assert_eq!(Timeline::demo().end(), 21.0);
    }

    #[test]
    fn trim_left_moves_source_in_with_start() {
        let mut tl = Timeline::new(30.0);
        let track = tl.add_track("V1");
        let id = tl.add_clip("a.mp4", ClipKind::Video, track, 2.0, 10.0, 5.0);
        tl.trim_left(id, 4.0);
        let clip = tl.clip(id).unwrap();
        assert_eq!(clip.start, 4.0);
        assert_eq!(clip.source_in, 7.0);
        assert_eq!(clip.end(), 12.0);
    }

    #[test]
    fn trim_left_clamps_at_source_zero() {
        let mut tl = Timeline::new(30.0);
        let track = tl.add_track("V1");
        let id = tl.add_clip("a.mp4", ClipKind::Video, track, 6.0, 4.0, 1.0);
        tl.trim_left(id, 0.0);
        let clip = tl.clip(id).unwrap();
        // Can only extend 1s earlier before the source runs out.
        assert_eq!(clip.start, 5.0);
        assert_eq!(clip.source_in, 0.0);
        assert_eq!(clip.duration, 5.0);
    }

    #[test]
    fn trim_right_changes_duration_only() {
        let mut tl = Timeline::new(30.0);
        let track = tl.add_track("V1");
        let id = tl.add_clip("a.mp4", ClipKind::Video, track, 2.0, 10.0, 5.0);
        tl.trim_right(id, 8.0);
        let clip = tl.clip(id).unwrap();
        assert_eq!(clip.duration, 6.0);
        assert_eq!(clip.source_in, 5.0);
    }

    #[test]
    fn close_gaps_shifts_following_clips_left() {
        let mut tl = Timeline::new(30.0);
        let main = tl.add_track("V1");
        let other = tl.add_track("A1");
        tl.add_clip("a.mp4", ClipKind::Video, main, 0.0, 2.0, 0.0);
        let b = tl.add_clip("b.mp4", ClipKind::Video, main, 5.0, 2.0, 0.0);
        let c = tl.add_clip("c.mp4", ClipKind::Video, main, 10.0, 1.0, 0.0);
        let keep = tl.add_clip("x.wav", ClipKind::Audio, other, 7.0, 1.0, 0.0);
        tl.close_gaps(main);
        assert_eq!(tl.clip(b).unwrap().start, 2.0);
        assert_eq!(tl.clip(c).unwrap().start, 4.0);
        assert_eq!(tl.clip(keep).unwrap().start, 7.0);
    }

    #[test]
    fn split_preserves_source_and_levels() {
        let mut tl = Timeline::new(30.0);
        let track = tl.add_track("A1");
        let id = tl.add_media_clip(
            "song.mp3",
            ClipKind::Audio,
            track,
            0.0,
            10.0,
            2.0,
            "/media/song.mp3",
        );
        tl.clip_mut(id).unwrap().volume = 0.4;
        let right = tl.split_at(id, 4.0).expect("split inside clip");
        let right = tl.clip(right).unwrap();
        assert_eq!(right.source.as_deref(), Some("/media/song.mp3"));
        assert_eq!(right.source_in, 6.0);
        assert!((right.volume - 0.4).abs() < f32::EPSILON);
    }
}

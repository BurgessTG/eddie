//! UI-independent timeline model.
//!
//! Times are seconds (`f64`). Media placement preserves source intervals;
//! generated clips and explicit move/trim/split edits use the project frame grid.

pub type ClipId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ClipKind {
    Video,
    Audio,
    Title,
}

/// Presentation style for title/caption clips. Preview and export
/// interpret the same value so generated captions do not change
/// appearance when rendered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TitleStyle {
    #[default]
    LowerThird,
    Caption,
    PopCaption,
    MinimalCaption,
    /// A caption whose spoken word lights up (needs `Clip::words`).
    HighlightCaption,
}

impl TitleStyle {
    pub const ALL: [Self; 5] = [
        Self::LowerThird,
        Self::Caption,
        Self::PopCaption,
        Self::MinimalCaption,
        Self::HighlightCaption,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LowerThird => "lower-third",
            Self::Caption => "caption",
            Self::PopCaption => "pop-caption",
            Self::MinimalCaption => "minimal-caption",
            Self::HighlightCaption => "highlight-caption",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::LowerThird => "Lower third",
            Self::Caption => "Caption",
            Self::PopCaption => "Pop caption",
            Self::MinimalCaption => "Minimal caption",
            Self::HighlightCaption => "Highlight caption",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|style| style.as_str() == name.trim().to_lowercase())
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
    /// Whether a video clip still plays the audio embedded in its
    /// source. Detaching moves that sound to an `Audio` clip and
    /// clears this flag so playback/export never double it.
    pub embedded_audio: bool,
    /// Linear audio fade lengths in seconds at the clip's head and tail.
    /// `fade_lengths` fits them inside the clip; picture is unaffected.
    pub fade_in: f64,
    pub fade_out: f64,
    /// Detached picture/sound partner. Linked clips move, trim, and
    /// split together until explicitly unlinked.
    pub linked_clip: Option<ClipId>,
    pub title_style: TitleStyle,
    /// Sound that dips under speech: it drops to [`DUCK_LEVEL`] wherever
    /// another audible clip that does not itself duck is talking.
    #[serde(default)]
    pub duck: bool,
    /// Titles only: when each whitespace-separated word of `name` starts,
    /// on the clip's source clock (timeline time is
    /// `start + word - source_in`, so trims and splits keep them aligned).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<f64>,
}

impl Clip {
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }

    pub fn contains(&self, time: f64) -> bool {
        time >= self.start && time < self.end()
    }

    /// Fades that fit the clip: each at most its length, and when
    /// together they exceed it, both scale down to meet.
    pub fn fade_lengths(&self) -> (f64, f64) {
        fit_fades(self.duration, self.fade_in, self.fade_out)
    }

    /// Word start times on the timeline, when there is one per word of the
    /// title text (a renamed title loses its timing).
    pub fn word_times(&self) -> Option<Vec<f64>> {
        (!self.words.is_empty() && self.words.len() == self.name.split_whitespace().count()).then(
            || {
                self.words
                    .iter()
                    .map(|word| self.start + word - self.source_in)
                    .collect()
            },
        )
    }

    /// The word being spoken at `time`: the last one started by then.
    pub fn active_word(&self, time: f64) -> Option<usize> {
        let times = self.word_times()?;
        times.iter().rposition(|&start| start <= time)
    }
}

pub fn fit_fades(duration: f64, fade_in: f64, fade_out: f64) -> (f64, f64) {
    let duration = duration.max(0.0);
    // Scaling the requested lengths keeps their ratio; each then fits.
    let clean = |f: f64| {
        if f.is_finite() {
            f.max(0.0)
        } else {
            0.0
        }
    };
    let (fade_in, fade_out) = (clean(fade_in), clean(fade_out));
    let total = fade_in + fade_out;
    if total > duration && total > 0.0 {
        (fade_in * duration / total, fade_out * duration / total)
    } else {
        (fade_in, fade_out)
    }
}

/// Gain of a fade envelope `offset` seconds into a clip of `duration`.
pub fn fade_gain(offset: f64, duration: f64, fade_in: f64, fade_out: f64) -> f32 {
    let (fade_in, fade_out) = fit_fades(duration, fade_in, fade_out);
    let mut gain = 1.0_f64;
    if fade_in > 0.0 {
        gain = gain.min(offset / fade_in);
    }
    if fade_out > 0.0 {
        gain = gain.min((duration - offset) / fade_out);
    }
    gain.clamp(0.0, 1.0) as f32
}

/// Ducked sound plays at this gain under speech (about -12 dB).
pub const DUCK_LEVEL: f32 = 0.25;
/// Seconds to fall to [`DUCK_LEVEL`] before speech and to recover after.
pub const DUCK_RAMP: f64 = 0.3;
/// Pauses shorter than this stay ducked, so music does not pump between
/// words. Equal to two ramps, so neighbouring dips never overlap.
const DUCK_HOLD: f64 = 2.0 * DUCK_RAMP;

/// Gain of a ducking envelope `offset` seconds into a clip, given its
/// sorted, non-overlapping speech ranges from [`Timeline::duck_ranges`].
/// Unity away from speech, [`DUCK_LEVEL`] inside it, linear ramps between.
pub fn duck_gain(offset: f64, ranges: &[(f64, f64)]) -> f32 {
    let first = ranges.partition_point(|&(_, end)| end + DUCK_RAMP <= offset);
    let depth = ranges[first..]
        .iter()
        .take(2)
        .map(|&(start, end)| {
            let rise = (offset - (start - DUCK_RAMP)) / DUCK_RAMP;
            let fall = (end + DUCK_RAMP - offset) / DUCK_RAMP;
            rise.min(fall).clamp(0.0, 1.0)
        })
        .fold(0.0_f64, f64::max);
    (1.0 - (1.0 - f64::from(DUCK_LEVEL)) * depth) as f32
}

/// Project picture format. The canvas never follows the footage: video
/// of the other orientation is not added rather than pillarboxed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Orientation {
    #[default]
    Landscape,
    Portrait,
}

impl Orientation {
    /// Displayed (rotation-applied) size; square counts as landscape.
    pub fn of(width: u32, height: u32) -> Self {
        if height > width {
            Self::Portrait
        } else {
            Self::Landscape
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Landscape => "landscape",
            Self::Portrait => "portrait",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Landscape, Self::Portrait]
            .into_iter()
            .find(|orientation| orientation.as_str() == name)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Timeline {
    pub fps: f64,
    pub orientation: Orientation,
    pub track_labels: Vec<String>,
    pub clips: Vec<Clip>,
    next_id: ClipId,
}

impl Timeline {
    pub fn new(fps: f64) -> Self {
        Self {
            fps,
            orientation: Orientation::default(),
            track_labels: Vec::new(),
            clips: Vec::new(),
            next_id: 1,
        }
    }

    /// After loading a saved project: new clips never reuse a saved ID.
    pub fn ensure_fresh_ids(&mut self) {
        let next = self.clips.iter().map(|clip| clip.id + 1).max().unwrap_or(1);
        self.next_id = self.next_id.max(next);
    }

    /// Snap a time to the nearest frame boundary.
    pub fn snap(&self, time: f64) -> f64 {
        (time * self.fps).round() / self.fps
    }

    pub fn add_track(&mut self, label: &str) -> usize {
        self.track_labels.push(label.to_owned());
        self.track_labels.len() - 1
    }

    fn relabel_tracks(&mut self) {
        for (i, label) in self.track_labels.iter_mut().enumerate() {
            *label = format!("Track {}", i + 1);
        }
    }

    /// Grow the lane list until `track` exists. Lanes are
    /// kind-agnostic and appear on demand (drops, drags, adds).
    pub fn ensure_track(&mut self, track: usize) {
        if self.track_labels.len() > track {
            return;
        }
        while self.track_labels.len() <= track {
            self.track_labels.push(String::new());
        }
        self.relabel_tracks();
    }

    /// Append one lane at the bottom. Returns its index.
    pub fn append_track(&mut self) -> usize {
        let index = self.track_labels.len();
        self.ensure_track(index);
        index
    }

    /// Insert a lane at `index`, shifting clips at or below it down
    /// one lane. Returns the inserted index.
    pub fn insert_track_at(&mut self, index: usize) -> usize {
        let index = index.min(self.track_labels.len());
        self.track_labels.insert(index, String::new());
        for clip in &mut self.clips {
            if clip.track >= index {
                clip.track += 1;
            }
        }
        self.relabel_tracks();
        index
    }

    /// Move a clip to another lane, growing the lane list if needed.
    pub fn set_clip_track(&mut self, id: ClipId, track: usize) {
        self.ensure_track(track);
        if let Some(clip) = self.clip_mut(id) {
            clip.track = track;
        }
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
        let start = self.snap(start.max(0.0));
        let duration = self.snap(duration.max(1.0 / self.fps));
        self.add_clip_inner(name, kind, track, start, duration, source_in, None)
    }

    /// Add a media interval without rounding its start or positive duration.
    /// Source-frame boundaries need not align with the project frame grid.
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
        self.ensure_track(track);
        let id = self.next_id;
        self.next_id += 1;
        self.clips.push(Clip {
            id,
            name: name.to_owned(),
            kind,
            track,
            start: start.max(0.0),
            duration: if duration > 0.0 {
                duration
            } else {
                1.0 / self.fps
            },
            source_in: source_in.max(0.0),
            source: source.map(str::to_owned),
            opacity: 1.0,
            volume: 1.0,
            embedded_audio: kind == ClipKind::Video && source.is_some(),
            fade_in: 0.0,
            fade_out: 0.0,
            linked_clip: None,
            title_style: TitleStyle::default(),
            duck: false,
            words: Vec::new(),
        });
        id
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// The picture shown by both preview and export. Lower lane
    /// indexes win; insertion order breaks ties. Opacity fades this
    /// picture against black, not against pictures on other lanes.
    pub fn active_video(&self, time: f64) -> Option<&Clip> {
        self.clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Video && clip.contains(time))
            .min_by_key(|clip| clip.track)
    }

    /// Only the first active title is shown, in insertion order.
    pub fn active_title(&self, time: f64) -> Option<&Clip> {
        self.clips
            .iter()
            .find(|clip| clip.kind == ClipKind::Title && clip.contains(time))
    }

    /// End of the last clip, or 0 for an empty timeline.
    pub fn end(&self) -> f64 {
        self.clips.iter().map(Clip::end).fold(0.0, f64::max)
    }

    fn linked_id(&self, id: ClipId) -> Option<ClipId> {
        self.clip(id)
            .and_then(|clip| clip.linked_clip)
            .filter(|linked| self.clip(*linked).is_some())
    }

    fn link_pair(&mut self, left: ClipId, right: ClipId) {
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
        if let Some(sound) = self.clip_mut(audio) {
            sound.volume = clip.volume;
        }
        if let Some(picture) = self.clip_mut(id) {
            picture.embedded_audio = false;
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

    /// Move a clip. Start is clamped to >= 0 and snapped to a frame.
    /// A linked detach partner moves by the same amount.
    pub fn move_clip(&mut self, id: ClipId, new_start: f64) {
        let Some(original) = self.clip(id).cloned() else {
            return;
        };
        let snapped = self.snap(new_start.max(0.0));
        let delta = snapped - original.start;
        if let Some(clip) = self.clip_mut(id) {
            clip.start = snapped;
        }
        if delta != 0.0 {
            if let Some(linked) = self.linked_id(id) {
                if let Some(companion) = self.clip(linked).cloned() {
                    let companion_start = self.snap((companion.start + delta).max(0.0));
                    if let Some(clip) = self.clip_mut(linked) {
                        clip.start = companion_start;
                    }
                }
            }
        }
    }

    /// Swap two non-overlapping clips on one lane, including their detached
    /// partners, atomically. Adjacent clips keep their occupied outer span
    /// and gap; non-adjacent clips exchange start positions only if they fit.
    /// This reorders existing intervals without rounding source durations or
    /// their derived boundary. Ordinary free moves still use `move_clip`.
    /// Any collision on any affected lane leaves the whole timeline unchanged.
    pub fn swap_clips(&mut self, first: ClipId, second: ClipId) -> bool {
        let (Some(first), Some(second)) = (self.clip(first), self.clip(second)) else {
            return false;
        };
        if first.id == second.id || first.track != second.track {
            return false;
        }
        let (left, right) = if first.start <= second.start {
            (first, second)
        } else {
            (second, first)
        };
        const EPSILON: f64 = 1e-9;
        if left.end() > right.start + EPSILON {
            return false;
        }
        let adjacent = !self.clips.iter().any(|clip| {
            clip.id != left.id
                && clip.id != right.id
                && clip.track == left.track
                && clip.start < right.start
                && clip.end() > left.end() + EPSILON
        });
        let left_start = if adjacent {
            right.end() - left.duration
        } else {
            right.start
        };
        let mut placements = Vec::with_capacity(4);
        for (clip, new_start) in [(left, left_start), (right, left.start)] {
            placements.push((clip.id, new_start));
            if let Some(companion) = self.linked_id(clip.id).and_then(|id| self.clip(id)) {
                // Swapping members of the same linked group would move one
                // clip twice and cannot preserve their relative placement.
                if companion.id == left.id || companion.id == right.id {
                    return false;
                }
                placements.push((companion.id, companion.start + (new_start - clip.start)));
            }
        }
        for (index, &(id, start)) in placements.iter().enumerate() {
            if !start.is_finite()
                || start < 0.0
                || placements[..index].iter().any(|(other, _)| *other == id)
            {
                return false;
            }
            let clip = self.clip(id).unwrap();
            let blocked = self.clips.iter().any(|other| {
                if other.id == id || other.track != clip.track {
                    return false;
                }
                let other_start = placements
                    .iter()
                    .find(|(moved, _)| *moved == other.id)
                    .map_or(other.start, |(_, new_start)| *new_start);
                start < other_start + other.duration - EPSILON
                    && other_start < start + clip.duration - EPSILON
            });
            if blocked {
                return false;
            }
        }
        for (id, start) in placements {
            self.clip_mut(id).unwrap().start = start;
        }
        true
    }

    fn trim_left_one(&mut self, id: ClipId, new_start: f64) {
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

    /// Trim the left edge to `new_start`, keeping the end fixed.
    /// `source_in` moves with the edge and clamps at 0 (start of media).
    /// A linked detach partner receives the same edge movement.
    pub fn trim_left(&mut self, id: ClipId, new_start: f64) {
        let Some(original) = self.clip(id).cloned() else {
            return;
        };
        self.trim_left_one(id, new_start);
        let Some(updated) = self.clip(id).cloned() else {
            return;
        };
        let delta = updated.start - original.start;
        if delta != 0.0 {
            if let Some(linked) = self.linked_id(id) {
                if let Some(companion) = self.clip(linked).cloned() {
                    self.trim_left_one(linked, companion.start + delta);
                }
            }
        }
    }

    fn trim_right_one(&mut self, id: ClipId, new_end: f64) {
        let frame = 1.0 / self.fps;
        let snapped = self.snap(new_end);
        if let Some(clip) = self.clip_mut(id) {
            clip.duration = (snapped - clip.start).max(frame);
        }
    }

    /// Trim the right edge to `new_end`. The caller clamps against the
    /// source media duration; the model only enforces one frame minimum.
    /// A linked detach partner receives the same edge movement.
    pub fn trim_right(&mut self, id: ClipId, new_end: f64) {
        let Some(original) = self.clip(id).cloned() else {
            return;
        };
        self.trim_right_one(id, new_end);
        let Some(updated) = self.clip(id).cloned() else {
            return;
        };
        let delta = updated.end() - original.end();
        if delta != 0.0 {
            if let Some(linked) = self.linked_id(id) {
                if let Some(companion) = self.clip(linked).cloned() {
                    self.trim_right_one(linked, companion.end() + delta);
                }
            }
        }
    }

    fn split_one(&mut self, id: ClipId, time: f64) -> Option<ClipId> {
        let at = self.snap(time);
        let clip = self.clip(id)?.clone();
        let frame = 1.0 / self.fps;
        if at <= clip.start + frame / 2.0 || at >= clip.end() - frame / 2.0 {
            return None;
        }
        let left_duration = at - clip.start;
        if let Some(left) = self.clip_mut(id) {
            left.duration = left_duration;
            // The head fade stays left, the tail fade moves right.
            left.fade_out = 0.0;
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
            right_clip.embedded_audio = clip.embedded_audio;
            right_clip.fade_out = clip.fade_out;
            right_clip.linked_clip = None;
            right_clip.title_style = clip.title_style;
            right_clip.duck = clip.duck;
            right_clip.words = clip.words.clone();
        }
        Some(right)
    }

    /// Split a clip at `time`. Returns the new (right-hand) clip id.
    /// No-op (`None`) if the point is not strictly inside the clip.
    /// A linked detach partner splits at the same timeline point
    /// and the two right-hand pieces stay linked to each other.
    pub fn split_at(&mut self, id: ClipId, time: f64) -> Option<ClipId> {
        let linked = self.linked_id(id);
        let right = self.split_one(id, time)?;
        if let Some(companion) = linked {
            if let Some(right_companion) = self.split_one(companion, time) {
                self.link_pair(right, right_companion);
            }
        }
        Some(right)
    }

    /// Open `seconds` of empty time at `at`: every clip that crosses the
    /// point splits there (linked pairs together, titles keep their text),
    /// and everything from the point on, on every lane, moves later by the
    /// same amount, so pictures, sound and captions stay in sync.
    pub fn insert_time(&mut self, at: f64, seconds: f64) {
        let at = self.snap(at.max(0.0));
        let seconds = self.snap(seconds);
        if seconds <= 0.0 {
            return;
        }
        let crossing: Vec<ClipId> = self
            .clips
            .iter()
            .filter(|clip| clip.start < at && at < clip.end())
            .map(|clip| clip.id)
            .collect();
        for id in crossing {
            // A linked partner may already have split with its pair.
            if self.clip(id).is_some_and(|clip| at < clip.end()) {
                self.split_at(id, at);
            }
        }
        let frame = 1.0 / self.fps;
        for clip in &mut self.clips {
            if clip.start >= at - frame / 2.0 {
                clip.start += seconds;
            }
        }
    }

    /// Where `target` ducks, as sorted ranges in its own clip-local
    /// seconds: wherever another audible clip that does not itself duck
    /// is speaking. `speech` maps a source path to its audible
    /// source-time ranges. Empty unless `target.duck` is set.
    pub fn duck_ranges(
        &self,
        target: &Clip,
        speech: &std::collections::HashMap<String, Vec<(f64, f64)>>,
    ) -> Vec<(f64, f64)> {
        if !target.duck {
            return Vec::new();
        }
        let mut ranges: Vec<(f64, f64)> = Vec::new();
        for clip in &self.clips {
            let audible = clip.kind == ClipKind::Audio
                || (clip.kind == ClipKind::Video && clip.embedded_audio);
            if clip.id == target.id || clip.duck || !audible || clip.volume <= 0.0 {
                continue;
            }
            let Some(spans) = clip.source.as_ref().and_then(|source| speech.get(source)) else {
                continue;
            };
            let source_end = clip.source_in + clip.duration;
            for &(from, to) in spans {
                let (from, to) = (from.max(clip.source_in), to.min(source_end));
                let offset = clip.start - clip.source_in - target.start;
                let (start, end) = ((from + offset).max(0.0), (to + offset).min(target.duration));
                if end > start {
                    ranges.push((start, end));
                }
            }
        }
        ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for (start, end) in ranges {
            match merged.last_mut() {
                Some(last) if start - last.1 < DUCK_HOLD => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        merged
    }

    /// Magnetic main-track behavior: shift clips on `track` left so no
    /// gaps remain between them. The first clip keeps its start.
    /// Remove timeline time that no video or audio clip covers, after the
    /// first such clip. Every clip after a gap, on every lane, moves left by
    /// the same amount, so titles, captions and linked or layered media stay
    /// in sync with the pictures they sit over. A title inside a removed gap
    /// moves to the gap's start.
    pub fn close_gaps(&mut self) {
        let mut covered: Vec<(f64, f64)> = self
            .clips
            .iter()
            .filter(|c| c.kind != ClipKind::Title)
            .map(|c| (c.start, c.end()))
            .collect();
        covered.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut gaps: Vec<(f64, f64)> = Vec::new();
        let mut reach: Option<f64> = None;
        for (start, end) in covered {
            match reach {
                Some(at) if start > at => {
                    gaps.push((at, start));
                    reach = Some(end);
                }
                Some(at) => reach = Some(at.max(end)),
                None => reach = Some(end),
            }
        }
        for clip in &mut self.clips {
            let shift: f64 = gaps
                .iter()
                .map(|&(a, b)| (clip.start.min(b) - a).max(0.0))
                .sum();
            clip.start -= shift;
        }
    }

    pub fn remove_clip(&mut self, id: ClipId) {
        self.clips.retain(|c| c.id != id);
        for clip in &mut self.clips {
            if clip.linked_clip == Some(id) {
                clip.linked_clip = None;
            }
        }
    }

    /// Topmost video/title clip active at `time` (lowest track index wins).
    pub fn visible_clip_at(&self, time: f64) -> Option<&Clip> {
        self.clips
            .iter()
            .filter(|c| c.kind != ClipKind::Audio && c.contains(time))
            .min_by_key(|c| c.track)
    }

    /// Standard empty project: one lane. Lanes grow on demand as
    /// clips are dropped or dragged above or below existing ones.
    pub fn empty_project() -> Self {
        let mut tl = Self::new(30.0);
        tl.add_track("Track 1");
        tl
    }

    pub fn with_orientation(mut self, orientation: Orientation) -> Self {
        self.orientation = orientation;
        self
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
    fn media_placement_preserves_source_intervals() {
        let mut tl = Timeline::new(30.0);
        for kind in [ClipKind::Video, ClipKind::Audio] {
            for (start, duration, source_in) in [
                (49.0 / 60.0, 1.235 - 49.0 / 60.0, 49.0 / 60.0),
                (0.0, 1.0 / 120.0, 17.0 / 120.0),
                (2.157, 0.001, 0.157),
            ] {
                let id = tl.add_media_clip(
                    "scene",
                    kind,
                    0,
                    start,
                    duration,
                    source_in,
                    "/media/scene.mp4",
                );
                let clip = tl.clip(id).unwrap();
                assert_eq!(
                    (clip.start, clip.duration, clip.source_in),
                    (start, duration, source_in)
                );
                assert_eq!(clip.end(), start + duration);
            }
        }

        let title = tl.add_clip("title", ClipKind::Title, 1, 1.02, 0.01, 0.0);
        assert_eq!(tl.clip(title).unwrap().start, 31.0 / 30.0);
        assert_eq!(tl.clip(title).unwrap().duration, 1.0 / 30.0);
        tl.move_clip(tl.clips[0].id, 2.02);
        assert_eq!(tl.clips[0].start, 61.0 / 30.0);
    }

    #[test]
    fn fractional_media_keeps_coverage_and_linked_audio_through_edits() {
        let mut tl = Timeline::empty_project();
        let video = tl.add_media_clip(
            "scene",
            ClipKind::Video,
            0,
            0.817,
            0.418,
            2.135,
            "/media/scene.mp4",
        );
        let audio = tl.detach_audio(video).unwrap();
        for id in [video, audio] {
            let clip = tl.clip(id).unwrap();
            assert_eq!(
                (clip.start, clip.duration, clip.source_in),
                (0.817, 0.418, 2.135)
            );
        }

        let right_video = tl.split_at(video, 1.0).unwrap();
        let right_audio = tl.clip(right_video).unwrap().linked_clip.unwrap();
        for (left, right) in [(video, right_video), (audio, right_audio)] {
            let left = tl.clip(left).unwrap();
            let right = tl.clip(right).unwrap();
            assert_eq!(left.end(), 1.0);
            assert_eq!(right.start, left.end());
            assert!((right.end() - 1.235).abs() < 1e-12);
            assert!((left.duration + right.duration - 0.418).abs() < 1e-12);
            assert_eq!(right.source_in, left.source_in + left.duration);
        }

        let tail = tl.add_media_clip(
            "tail",
            ClipKind::Video,
            0,
            2.0,
            0.007,
            4.0,
            "/media/scene.mp4",
        );
        let tail_audio = tl.detach_audio(tail).unwrap();
        tl.close_gaps();
        for id in [tail, tail_audio] {
            let clip = tl.clip(id).unwrap();
            assert_eq!(clip.start, tl.clip(right_video).unwrap().end());
            assert_eq!(clip.duration, 0.007);
            assert_eq!(clip.source_in, 4.0);
        }
        let unchanged = tl.clone();
        tl.close_gaps();
        assert_eq!(tl, unchanged, "closing linked lanes again is a no-op");
    }

    #[test]
    fn move_clamps_to_zero() {
        let mut tl = Timeline::demo();
        let id = tl.clips[0].id;
        tl.move_clip(id, -5.0);
        assert_eq!(tl.clip(id).unwrap().start, 0.0);
    }

    #[test]
    fn adjacent_swaps_preserve_fractional_span_gap_and_linked_coverage() {
        for left_duration in [1.0 / 120.0, 0.157, 0.418] {
            for right_duration in [0.001, 0.1, 7.0 / 30.0] {
                for gap in [0.0, 0.23] {
                    for reverse in [false, true] {
                        let mut tl = Timeline::empty_project();
                        let left = tl.add_media_clip(
                            "left",
                            ClipKind::Video,
                            0,
                            0.817,
                            left_duration,
                            2.135,
                            "left",
                        );
                        let right = tl.add_media_clip(
                            "right",
                            ClipKind::Video,
                            0,
                            0.817 + left_duration + gap,
                            right_duration,
                            4.237,
                            "right",
                        );
                        let left_audio = tl.detach_audio(left).unwrap();
                        let right_audio = tl.detach_audio(right).unwrap();
                        let outer_end = tl.clip(right).unwrap().end();
                        let prefix = tl.add_media_clip(
                            "prefix",
                            ClipKind::Video,
                            0,
                            0.0,
                            0.817,
                            0.0,
                            "prefix",
                        );
                        let tail = tl.add_media_clip(
                            "tail",
                            ClipKind::Video,
                            0,
                            outer_end,
                            0.1,
                            0.0,
                            "tail",
                        );
                        let before = tl.clone();
                        let (first, second) = if reverse {
                            (right, left)
                        } else {
                            (left, right)
                        };
                        assert!(tl.swap_clips(first, second), "left={left_duration} right={right_duration} gap={gap} reverse={reverse}");
                        for (id, expected_start) in [
                            (left, outer_end - left_duration),
                            (left_audio, outer_end - left_duration),
                            (right, 0.817),
                            (right_audio, 0.817),
                        ] {
                            let clip = tl.clip(id).unwrap();
                            assert!((clip.start - expected_start).abs() < 1e-12);
                            let mut expected = before.clip(id).unwrap().clone();
                            expected.start = clip.start;
                            assert_eq!(clip, &expected, "only start may change");
                        }
                        assert!((tl.clip(left).unwrap().end() - outer_end).abs() < 1e-12);
                        assert!(
                            (tl.clip(left).unwrap().start - tl.clip(right).unwrap().end() - gap)
                                .abs()
                                < 1e-12
                        );
                        assert_eq!(tl.clip(prefix), before.clip(prefix));
                        assert_eq!(tl.clip(tail), before.clip(tail));
                        assert_eq!(tl.track_labels, before.track_labels);
                        assert!(tl.swap_clips(first, second));
                        for clip in &tl.clips {
                            assert!(
                                (clip.start - before.clip(clip.id).unwrap().start).abs() < 1e-12,
                                "swapping back restores the original order"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn swap_rejects_collisions_on_either_companion_lane_atomically() {
        for block_left in [false, true] {
            let mut tl = Timeline::empty_project();
            let left = tl.add_media_clip("left", ClipKind::Video, 0, 2.0, 0.4, 0.157, "left");
            let right = tl.add_media_clip("right", ClipKind::Video, 0, 2.4, 0.2, 1.237, "right");
            tl.detach_audio(left).unwrap();
            let right_audio = tl.detach_audio(right).unwrap();
            tl.set_clip_track(right_audio, 2);
            let (track, start) = if block_left { (1, 2.4) } else { (2, 2.0) };
            tl.add_media_clip(
                "blocker",
                ClipKind::Audio,
                track,
                start,
                0.2,
                0.0,
                "blocker",
            );
            let before = tl.clone();
            assert!(!tl.swap_clips(left, right));
            assert_eq!(tl, before);
            assert!(!tl.swap_clips(right, left));
            assert_eq!(tl, before);
        }
    }

    #[test]
    fn nonadjacent_swaps_leave_middle_clips_unchanged_and_reject_collisions() {
        for duration in [0.4, 1.0] {
            let mut tl = Timeline::empty_project();
            let left = tl.add_media_clip("left", ClipKind::Video, 0, 2.0, duration, 0.0, "left");
            let right = tl.add_media_clip("right", ClipKind::Video, 0, 5.0, 0.4, 0.0, "right");
            let middle = tl.add_media_clip("middle", ClipKind::Video, 0, 3.0, 1.0, 0.0, "middle");
            let tail = tl.add_media_clip("tail", ClipKind::Video, 0, 5.4, 1.0, 0.0, "tail");
            let before = tl.clone();
            if duration == 0.4 {
                assert!(tl.swap_clips(left, right));
                assert_eq!(tl.clip(left).unwrap().start, 5.0);
                assert_eq!(tl.clip(right).unwrap().start, 2.0);
                assert_eq!(tl.clip(middle), before.clip(middle));
                assert_eq!(tl.clip(tail), before.clip(tail));
            } else {
                assert!(!tl.swap_clips(left, right));
                assert_eq!(tl, before);
            }
        }
    }

    #[test]
    fn swap_rejects_invalid_pairs_without_mutating() {
        let mut tl = Timeline::empty_project();
        let video = tl.add_media_clip("scene", ClipKind::Video, 0, 2.0, 0.4, 0.157, "scene");
        let audio = tl.detach_audio(video).unwrap();
        let before = tl.clone();
        for (first, second) in [(video, video), (video, audio), (video, 99), (99, video)] {
            assert!(!tl.swap_clips(first, second));
            assert_eq!(tl, before);
        }
        // Even if a caller places linked members at distinct times on one
        // lane, they cannot be swapped independently of their shared group.
        tl.set_clip_track(audio, 0);
        tl.clip_mut(audio).unwrap().start = 3.0;
        let before = tl.clone();
        assert!(!tl.swap_clips(video, audio));
        assert_eq!(tl, before);
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
    fn close_gaps_removes_empty_time_on_every_lane_together() {
        let mut tl = Timeline::new(30.0);
        let main = tl.add_track("V1");
        let other = tl.add_track("A1");
        let captions = tl.add_track("Captions");
        tl.add_clip("a.mp4", ClipKind::Video, main, 1.0, 2.0, 0.0);
        let b = tl.add_clip("b.mp4", ClipKind::Video, main, 5.0, 2.0, 0.0);
        let c = tl.add_clip("c.mp4", ClipKind::Video, main, 10.0, 1.0, 0.0);
        // Audio on another lane bridges 7-8, so only 3-5 and 8-10 are empty.
        let bridge = tl.add_clip("x.wav", ClipKind::Audio, other, 7.0, 1.0, 0.0);
        // Captions over b and c, and a title alone in the 8-10 gap.
        let over_b = tl.add_clip("over b", ClipKind::Title, captions, 5.5, 1.0, 0.0);
        let over_c = tl.add_clip("over c", ClipKind::Title, captions, 10.2, 0.5, 0.0);
        let in_gap = tl.add_clip("in gap", ClipKind::Title, captions, 9.0, 0.5, 0.0);
        tl.close_gaps();
        let start = |id| tl.clip(id).unwrap().start;
        // The leading gap before the first media clip stays.
        assert_eq!(start(b), 3.0);
        assert_eq!(start(bridge), 5.0);
        assert_eq!(start(c), 6.0);
        assert_eq!(start(over_b) - start(b), 0.5);
        assert!((start(over_c) - start(c) - 0.2).abs() < 1e-9);
        assert_eq!(start(in_gap), 6.0);
    }

    #[test]
    fn split_keeps_each_fade_on_its_own_edge() {
        let mut tl = Timeline::empty_project();
        let id = tl.add_media_clip("m", ClipKind::Audio, 0, 0.0, 4.0, 0.0, "m.mp3");
        let clip = tl.clip_mut(id).unwrap();
        (clip.fade_in, clip.fade_out) = (1.0, 2.0);
        let right = tl.split_at(id, 1.5).unwrap();
        let (left, right) = (tl.clip(id).unwrap(), tl.clip(right).unwrap());
        assert_eq!((left.fade_in, left.fade_out), (1.0, 0.0));
        assert_eq!((right.fade_in, right.fade_out), (0.0, 2.0));
        assert_eq!(fit_fades(2.0, 3.0, 1.0), (1.5, 0.5));
        assert_eq!(fit_fades(2.0, f64::NAN, -1.0), (0.0, 0.0));
    }

    #[test]
    fn empty_project_starts_with_one_track() {
        let tl = Timeline::empty_project();
        assert_eq!(tl.track_labels.len(), 1);
        assert!(tl.clips.is_empty());
    }

    #[test]
    fn add_clip_grows_tracks_to_cover_its_index() {
        let mut tl = Timeline::empty_project();
        tl.add_clip("a.mp4", ClipKind::Video, 2, 0.0, 2.0, 0.0);
        assert_eq!(tl.track_labels.len(), 3);
    }

    #[test]
    fn insert_track_at_shifts_existing_clips_down() {
        let mut tl = Timeline::empty_project();
        let a = tl.add_clip("a.mp4", ClipKind::Video, 0, 0.0, 2.0, 0.0);
        let b = tl.add_clip("b.mp4", ClipKind::Audio, 0, 3.0, 2.0, 0.0);
        let inserted = tl.insert_track_at(0);
        assert_eq!(inserted, 0);
        assert_eq!(tl.track_labels.len(), 2);
        assert_eq!(tl.clip(a).unwrap().track, 1);
        assert_eq!(tl.clip(b).unwrap().track, 1);
        // The new top lane is free for the clip that asked for it.
        tl.set_clip_track(a, 0);
        assert_eq!(tl.clip(a).unwrap().track, 0);
        assert_eq!(tl.clip(b).unwrap().track, 1);
    }

    #[test]
    fn append_track_and_set_clip_track_grows() {
        let mut tl = Timeline::empty_project();
        let a = tl.add_clip("a.mp4", ClipKind::Video, 0, 0.0, 2.0, 0.0);
        let new_track = tl.append_track();
        assert_eq!(new_track, 1);
        tl.set_clip_track(a, new_track);
        assert_eq!(tl.clip(a).unwrap().track, 1);
        // Setting beyond the end grows instead of losing the clip.
        tl.set_clip_track(a, 4);
        assert_eq!(tl.clip(a).unwrap().track, 4);
        assert_eq!(tl.track_labels.len(), 5);
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

    #[test]
    fn detach_audio_creates_linked_clip_and_silences_picture() {
        let mut tl = Timeline::empty_project();
        let video = tl.add_media_clip(
            "interview.mp4",
            ClipKind::Video,
            0,
            2.0,
            8.0,
            3.0,
            "/media/interview.mp4",
        );
        tl.clip_mut(video).unwrap().volume = 0.6;

        let audio = tl.detach_audio(video).expect("detach embedded audio");
        let picture = tl.clip(video).unwrap().clone();
        let sound = tl.clip(audio).unwrap().clone();
        assert!(!picture.embedded_audio);
        assert_eq!(picture.linked_clip, Some(audio));
        assert_eq!(sound.kind, ClipKind::Audio);
        assert_eq!(sound.linked_clip, Some(video));
        assert_eq!(sound.start, picture.start);
        assert_eq!(sound.duration, picture.duration);
        assert_eq!(sound.source_in, picture.source_in);
        assert_eq!(sound.source, picture.source);
        assert!((sound.volume - 0.6).abs() < f32::EPSILON);
        assert_ne!(sound.track, picture.track);

        tl.move_clip(video, 5.0);
        assert_eq!(tl.clip(audio).unwrap().start, 5.0, "linked audio moves");
        tl.trim_left(audio, 6.0);
        assert_eq!(tl.clip(video).unwrap().start, 6.0, "linked picture trims");
        assert_eq!(tl.clip(video).unwrap().source_in, 4.0);

        let right_video = tl.split_at(video, 8.0).expect("split linked pair");
        let right_audio_id = tl
            .clip(right_video)
            .unwrap()
            .linked_clip
            .expect("right picture stays linked");
        assert_ne!(right_audio_id, audio);
        assert_eq!(tl.clip(right_audio_id).unwrap().start, 8.0);
        assert_eq!(
            tl.clip(right_audio_id).unwrap().linked_clip,
            Some(right_video)
        );

        tl.remove_clip(right_audio_id);
        assert_eq!(tl.clip(right_video).unwrap().linked_clip, None);
        assert!(tl.set_embedded_audio(right_video, true));
        assert!(tl.clip(right_video).unwrap().embedded_audio);
    }

    #[test]
    fn title_style_defaults_parses_and_survives_split() {
        let mut tl = Timeline::empty_project();
        let title = tl.add_clip("Hello", ClipKind::Title, 0, 0.0, 4.0, 0.0);
        assert_eq!(tl.clip(title).unwrap().title_style, TitleStyle::LowerThird);
        tl.clip_mut(title).unwrap().title_style =
            TitleStyle::from_name("pop-caption").expect("known style");
        let right = tl.split_at(title, 2.0).expect("split title");
        assert_eq!(tl.clip(right).unwrap().title_style, TitleStyle::PopCaption);
        assert_eq!(TitleStyle::Caption.as_str(), "caption");
        assert_eq!(
            TitleStyle::from_name("minimal-caption"),
            Some(TitleStyle::MinimalCaption)
        );
        assert_eq!(TitleStyle::from_name("comic-sans"), None);
    }

    #[test]
    fn unlinking_detached_audio_keeps_both_clips_independent() {
        let mut tl = Timeline::empty_project();
        let video = tl.add_media_clip(
            "interview.mp4",
            ClipKind::Video,
            0,
            0.0,
            6.0,
            0.0,
            "/media/interview.mp4",
        );
        let audio = tl.detach_audio(video).expect("detach embedded audio");
        assert!(tl.unlink_clip(video));
        assert_eq!(tl.clip(video).unwrap().linked_clip, None);
        assert_eq!(tl.clip(audio).unwrap().linked_clip, None);
        tl.move_clip(video, 3.0);
        assert_eq!(tl.clip(audio).unwrap().start, 0.0);
    }

    #[test]
    fn inserting_time_splits_crossing_clips_and_moves_everything_after() {
        let mut tl = Timeline::new(30.0);
        let early = tl.add_clip("Hello", ClipKind::Title, 2, 1.0, 1.0, 0.0);
        let talk = tl.add_media_clip("talk", ClipKind::Video, 1, 0.0, 10.0, 0.0, "/m/talk.mp4");
        let sound = tl.detach_audio(talk).expect("detach");
        let caption = tl.add_clip("across the point", ClipKind::Title, 2, 3.0, 3.0, 0.0);
        let broll = tl.add_media_clip("broll", ClipKind::Video, 0, 7.0, 2.0, 4.0, "/m/b.mp4");

        tl.insert_time(5.0, 4.0);

        let span = |id: ClipId| {
            let clip = tl.clip(id).unwrap();
            (clip.start, clip.end(), clip.source_in)
        };
        assert_eq!(span(early), (1.0, 2.0, 0.0), "clips before the point stay");
        assert_eq!(span(talk), (0.0, 5.0, 0.0));
        assert_eq!(span(sound), (0.0, 5.0, 0.0));
        assert_eq!(span(caption).0, 3.0);
        assert_eq!(span(caption).1, 5.0);
        assert_eq!(
            span(broll),
            (11.0, 13.0, 4.0),
            "later clips move by the gap"
        );
        let pieces = |name: &str| {
            let mut found: Vec<_> = tl
                .clips
                .iter()
                .filter(|c| c.name == name)
                .map(|c| (c.start, c.end(), c.source_in, c.linked_clip.is_some()))
                .collect();
            found.sort_by(|a, b| a.0.total_cmp(&b.0));
            found
        };
        assert_eq!(
            pieces("talk")[1],
            (9.0, 14.0, 5.0, true),
            "right piece resumes the source"
        );
        assert_eq!(
            pieces("across the point")[1].0,
            9.0,
            "titles keep their text after the gap"
        );
        let right_talk = tl
            .clips
            .iter()
            .find(|c| c.name == "talk" && c.start == 9.0)
            .unwrap();
        let partner = tl.clip(right_talk.linked_clip.unwrap()).unwrap();
        assert_eq!((partner.kind, partner.start), (ClipKind::Audio, 9.0));
        assert_eq!(tl.end(), 14.0);
        assert!(
            tl.clips.iter().all(|c| c.end() <= 5.0 || c.start >= 9.0),
            "nothing plays inside the inserted time"
        );
    }

    #[test]
    fn ducked_sound_dips_under_other_clips_speech_and_recovers_in_pauses() {
        let mut tl = Timeline::new(30.0);
        let music = tl.add_media_clip("bed", ClipKind::Audio, 2, 2.0, 20.0, 0.0, "/m/bed.mp3");
        tl.clip_mut(music).unwrap().duck = true;
        let other_bed =
            tl.add_media_clip("bed2", ClipKind::Audio, 3, 0.0, 30.0, 0.0, "/m/bed2.mp3");
        tl.clip_mut(other_bed).unwrap().duck = true;
        tl.add_media_clip("talk", ClipKind::Video, 0, 5.0, 10.0, 10.0, "/m/talk.mp4");
        let muted = tl.add_media_clip("muted", ClipKind::Audio, 4, 0.0, 30.0, 0.0, "/m/muted.wav");
        tl.clip_mut(muted).unwrap().volume = 0.0;
        let speech: std::collections::HashMap<String, Vec<(f64, f64)>> = [
            // Source time; the talk clip shows source 10..20 at timeline 5..15.
            (
                "/m/talk.mp4".to_owned(),
                vec![(11.0, 13.0), (13.4, 14.0), (18.0, 30.0)],
            ),
            ("/m/bed2.mp3".to_owned(), vec![(0.0, 30.0)]),
            ("/m/muted.wav".to_owned(), vec![(0.0, 30.0)]),
        ]
        .into();
        let bed = tl.clip(music).unwrap().clone();
        // Timeline 6..8 and 8.4..9 (a 0.4 s pause stays ducked), then 13..15,
        // in the bed's own time (it starts at 2).
        assert_eq!(
            tl.duck_ranges(&bed, &speech),
            vec![(4.0, 7.0), (11.0, 13.0)]
        );
        let mut plain = bed.clone();
        plain.duck = false;
        assert!(tl.duck_ranges(&plain, &speech).is_empty());

        let ranges = tl.duck_ranges(&bed, &speech);
        let gain = |t: f64| duck_gain(t, &ranges);
        assert_eq!(gain(2.0), 1.0);
        assert!((gain(3.85) - 0.625).abs() < 1e-6, "halfway down the ramp");
        assert_eq!(gain(5.0), DUCK_LEVEL);
        assert_eq!(gain(7.3), 1.0, "recovered after the ramp");
        assert_eq!(gain(9.0), 1.0, "a real pause brings the music back");
        assert_eq!(gain(12.0), DUCK_LEVEL);
    }
}

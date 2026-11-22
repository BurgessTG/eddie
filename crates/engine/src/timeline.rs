//! The timeline: lanes of clips on the project frame grid.

use crate::{
    time_spec, Clip, ClipId, ClipKind, Graphic, Marker, MarkerId, Reversed, Shape, TitleStyle,
};

mod copies;
mod edit;
mod lanes;
mod linked;

pub use edit::Ripple;
pub use lanes::{Lanes, LANE_KINDS};

/// Longest supported timeline, about 11.5 days. Loudness measurement and
/// export planning walk the whole timeline, so a clip sent absurdly far out
/// has to be an error instead of hours of rendered silence.
pub const MAX_TIMELINE_SECONDS: f64 = 1_000_000.0;

/// Slack for comparing an edge with the frame grid: far below a frame and
/// above the noise of adding and subtracting timeline seconds.
const EDGE_EPSILON: f64 = 1e-9;

/// Project picture format: the canvas shape. Media of any shape goes on
/// any canvas; each clip's [`Transform`](crate::Transform) fits it, with
/// bars by default. Projects saved before 0.3 say `Landscape` or `Portrait`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Format {
    /// The first picture's own shape, as export always made it.
    #[default]
    Original,
    /// 16:9.
    Landscape,
    /// 9:16.
    Portrait,
    /// 1:1.
    Square,
    /// 4:5.
    Vertical,
    /// 4:3.
    Classic,
    /// 21:9.
    Cinema,
}

impl Format {
    pub const ALL: [Self; 7] = [
        Self::Original,
        Self::Landscape,
        Self::Portrait,
        Self::Square,
        Self::Vertical,
        Self::Classic,
        Self::Cinema,
    ];

    /// Width to height; `None` for [`Format::Original`], which takes the
    /// first picture's.
    pub fn ratio(self) -> Option<(u32, u32)> {
        match self {
            Self::Original => None,
            Self::Landscape => Some((16, 9)),
            Self::Portrait => Some((9, 16)),
            Self::Square => Some((1, 1)),
            Self::Vertical => Some((4, 5)),
            Self::Classic => Some((4, 3)),
            Self::Cinema => Some((21, 9)),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Landscape => "landscape",
            Self::Portrait => "portrait",
            Self::Square => "square",
            Self::Vertical => "vertical",
            Self::Classic => "classic",
            Self::Cinema => "cinema",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Original => "Original",
            Self::Landscape => "16:9 Landscape",
            Self::Portrait => "9:16 Portrait",
            Self::Square => "1:1 Square",
            Self::Vertical => "4:5 Vertical",
            Self::Classic => "4:3 Classic",
            Self::Cinema => "21:9 Cinema",
        }
    }

    /// A name (`square`) or a ratio (`1:1`).
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_lowercase();
        Self::ALL.into_iter().find(|format| {
            format.as_str() == name
                || format
                    .ratio()
                    .is_some_and(|(w, h)| format!("{w}:{h}") == name)
        })
    }

    /// The canvas for this format whose short side is `short` pixels, both
    /// sides even. `original` is the first picture's size, which
    /// [`Format::Original`] takes its shape from.
    pub fn canvas(self, short: u32, original: (u32, u32)) -> (u32, u32) {
        let (w, h) = self
            .ratio()
            .unwrap_or((original.0.max(1), original.1.max(1)));
        let short = short.max(2) / 2 * 2;
        let long = |a: u32, b: u32| {
            ((u64::from(short) * u64::from(a)).div_ceil(u64::from(b)) as u32).div_ceil(2) * 2
        };
        if w >= h {
            (long(w, h), short)
        } else {
            (short, long(h, w))
        }
    }
}

/// `landscape` or `portrait` for a displayed picture size; square counts
/// as landscape.
pub fn orientation_name(width: u32, height: u32) -> &'static str {
    if height > width {
        "portrait"
    } else {
        "landscape"
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Timeline {
    pub fps: f64,
    /// Saved as `orientation`, its name before formats other than 16:9
    /// and 9:16 existed.
    #[serde(rename = "orientation")]
    pub format: Format,
    /// How many lanes titles, video and sound have (`lanes.rs`). A clip's
    /// `track` counts within its kind's lanes.
    #[serde(default)]
    pub lanes: Lanes,
    pub clips: Vec<Clip>,
    next_id: ClipId,
    /// Notes pinned to moments of the timeline (`marker.rs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
    #[serde(default = "first_marker", skip_serializing_if = "is_first_marker")]
    pub(crate) next_marker: MarkerId,
}

fn first_marker() -> MarkerId {
    1
}

fn is_first_marker(id: &MarkerId) -> bool {
    *id == 1
}

impl Timeline {
    pub fn new(fps: f64) -> Self {
        Self {
            fps,
            format: Format::default(),
            lanes: Lanes::default(),
            clips: Vec::new(),
            next_id: 1,
            markers: Vec::new(),
            next_marker: 1,
        }
    }

    /// What time specs (`1:05`, `clip:7@end`, …) resolve against.
    pub fn time_index(&self) -> time_spec::TimeIndex {
        time_spec::TimeIndex {
            fps: self.fps,
            end: self.end(),
            clips: self
                .clips
                .iter()
                .map(|clip| (clip.id, clip.start, clip.end()))
                .collect(),
            markers: self
                .markers
                .iter()
                .map(|marker| (marker.id, marker.time))
                .collect(),
        }
    }

    /// Whether every number is one the editor can work with: a real frame
    /// rate, finite clip times inside [`MAX_TIMELINE_SECONDS`], speeds from
    /// [`MIN_SPEED`](crate::MIN_SPEED) to [`MAX_SPEED`](crate::MAX_SPEED), and
    /// transforms, colour and transitions the renderers take ([`Transform::is_valid`](crate::Transform),
    /// [`ColorAdjust::is_valid`](crate::ColorAdjust)). A project
    /// file or a script can otherwise carry values that divide by zero or
    /// keep offline work running for hours.
    pub fn in_supported_range(&self) -> bool {
        (1.0..=1000.0).contains(&self.fps)
            && self.clips.iter().all(|clip| {
                clip.start.is_finite()
                    && clip.source_in.is_finite()
                    && (0.0..=MAX_TIMELINE_SECONDS).contains(&clip.duration)
                    && clip.start.abs() <= MAX_TIMELINE_SECONDS
                    && clip.end() <= MAX_TIMELINE_SECONDS
                    && clip.transform.is_valid()
                    && clip.color.is_valid()
                    && (crate::MIN_SPEED..=crate::MAX_SPEED).contains(&clip.speed)
                    && clip.transition.is_none_or(|t| t.is_valid())
                    && clip.keyframes.is_valid()
                    && clip.chroma.is_none_or(|key| key.is_valid())
                    && clip.mask.is_none_or(|mask| mask.is_valid())
                    && clip
                        .graphic
                        .as_ref()
                        .and_then(Graphic::shape)
                        .is_none_or(Shape::is_valid)
                    && clip.reversed.as_ref().is_none_or(Reversed::is_valid)
            })
            && self.markers_in_range()
    }

    /// Pairs of clips that share a lane for a frame or more, lower ID
    /// first. Adds, drags and swaps never leave one; a script's `moveClip`,
    /// `setClipLane` or `trimClip` can. Overlaps under a frame come from
    /// grid edges meeting off-grid media edges and do not count.
    pub fn lane_overlaps(&self) -> Vec<(ClipId, ClipId)> {
        let frame = 1.0 / self.fps;
        let mut by_lane: Vec<&Clip> = self.clips.iter().collect();
        by_lane.sort_by(|a, b| {
            let row = |clip: &Clip| self.row(clip.kind, clip.track);
            row(a).cmp(&row(b)).then(a.start.total_cmp(&b.start))
        });
        let mut pairs = Vec::new();
        for (index, clip) in by_lane.iter().enumerate() {
            for other in &by_lane[index + 1..] {
                if !other.same_lane(clip) || other.start > clip.end() - frame + EDGE_EPSILON {
                    break;
                }
                if other.end() - other.start.max(clip.start) >= frame - EDGE_EPSILON {
                    pairs.push((clip.id.min(other.id), clip.id.max(other.id)));
                }
            }
        }
        pairs.sort_unstable();
        pairs
    }

    /// After loading a saved project: new clips and markers never reuse a
    /// saved ID.
    pub fn ensure_fresh_ids(&mut self) {
        let next = self.clips.iter().map(|clip| clip.id + 1).max().unwrap_or(1);
        self.next_id = self.next_id.max(next);
        let next = self.markers.iter().map(|m| m.id + 1).max().unwrap_or(1);
        self.next_marker = self.next_marker.max(next);
    }

    /// Snap a time to the nearest frame boundary.
    pub fn snap(&self, time: f64) -> f64 {
        (time * self.fps).round() / self.fps
    }

    /// The nearest frame boundary that is not before `time`.
    fn snap_at_or_after(&self, time: f64) -> f64 {
        let snapped = self.snap(time);
        if snapped < time - EDGE_EPSILON {
            ((time * self.fps).round() + 1.0) / self.fps
        } else {
            snapped
        }
    }

    /// The nearest frame boundary that is not after `time`.
    fn snap_at_or_before(&self, time: f64) -> f64 {
        let snapped = self.snap(time);
        if snapped > time + EDGE_EPSILON {
            ((time * self.fps).round() - 1.0) / self.fps
        } else {
            snapped
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
        self.grow_lanes(kind, track);
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
            reduce_noise: false,
            clean_voice: false,
            smooth_fades: false,
            words: Vec::new(),
            transform: crate::Transform::default(),
            background: crate::Background::default(),
            color: crate::ColorAdjust::default(),
            text: crate::TextLook::default(),
            speed: 1.0,
            photo: false,
            freeze: false,
            transition: None,
            keyframes: crate::Keyframes::default(),
            blend: crate::BlendMode::Normal,
            chroma: None,
            mask: None,
            graphic: None,
            reversed: None,
        });
        id
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// The top-most video clip at `time`: lower lane numbers draw over
    /// higher ones, and insertion order breaks ties. Lanes under it may
    /// still show around or through it ([`Timeline::video_stack`]).
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

    pub fn remove_clip(&mut self, id: ClipId) {
        self.clips.retain(|c| c.id != id);
        for clip in &mut self.clips {
            if clip.linked_clip == Some(id) {
                clip.linked_clip = None;
            }
        }
    }

    /// Standard empty project: one lane each for titles, video and
    /// sound. More grow on demand as clips are added, dropped or dragged
    /// past the first or last lane of their kind.
    pub fn empty_project() -> Self {
        let mut tl = Self::new(30.0);
        tl.ensure_lanes();
        tl
    }

    pub fn with_format(mut self, format: Format) -> Self {
        self.format = format;
        self
    }

    /// Fictional project kept for model tests.
    pub fn demo() -> Self {
        let mut tl = Self::empty_project();
        let (titles, video, audio) = (0, 0, 0);
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

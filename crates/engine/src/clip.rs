//! A clip: one placed piece of video, audio or title text.

use crate::blend::BlendMode;
use crate::chroma::ChromaKey;
use crate::color::ColorAdjust;
use crate::fit_fades;
use crate::keyframe::{Keyframes, Property};
use crate::mask::Mask;
use crate::place::{Background, Transform};
use crate::text::TextLook;
use crate::transition::Transition;

pub type ClipId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ClipKind {
    Video,
    Audio,
    Title,
}

impl ClipKind {
    /// The name scripts and project files use: `video`, `audio`, `title`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Title => "title",
        }
    }
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
    /// Steady background noise (hiss, hum, fans) brought down in preview
    /// and export, from the noise measured in the source's quiet parts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reduce_noise: bool,
    /// Speech cleanup (80 Hz high-pass and a gentle compressor) in preview
    /// and export. Projects saved before it load with it off.
    #[serde(default)]
    pub clean_voice: bool,
    /// Fades follow an equal-power quarter sine (FFmpeg's `qsin`) instead
    /// of a straight line; see [`fade_shape`]. Off in older projects.
    #[serde(default)]
    pub smooth_fades: bool,
    /// Titles only: when each whitespace-separated word of `name` starts,
    /// on the clip's source clock ([`Clip::time_at`] places them, so trims
    /// and splits keep them aligned).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<f64>,
    /// Video only: where the picture sits on the canvas (`place`).
    #[serde(default, skip_serializing_if = "Transform::is_identity")]
    pub transform: Transform,
    /// Video only: what fills the canvas around the picture.
    #[serde(default, skip_serializing_if = "is_black")]
    pub background: Background,
    /// Video only: colour adjustments and filter (`ColorAdjust`).
    #[serde(default, skip_serializing_if = "ColorAdjust::is_identity")]
    pub color: ColorAdjust,
    /// Source seconds played per timeline second, [`MIN_SPEED`] to
    /// [`MAX_SPEED`]; sound keeps its pitch. Use [`Clip::source_at`] and
    /// [`Clip::time_at`] to move between the two clocks.
    #[serde(default = "normal_speed", skip_serializing_if = "is_normal_speed")]
    pub speed: f64,
    /// Video only: the source is one picture, a photo, shown for the
    /// clip's whole length. It has no source range (`source_in` stays 0),
    /// so either edge trims as far as it likes, and it has no speed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub photo: bool,
    /// Video only: a freeze frame, the source picture at `source_in` held
    /// for the clip's whole length. Like a photo it has no source range and
    /// no speed, but its file is a video, so readers seek to the picture.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub freeze: bool,
    /// Titles only: size, colour, place, box and outline over the style's.
    #[serde(default, skip_serializing_if = "TextLook::is_identity")]
    pub text: TextLook,
    /// Video only: how the picture passes into this clip from the clip
    /// that ends where it starts on its lane ([`Timeline::transitions`]).
    /// It does nothing while no clip ends there.
    ///
    /// [`Timeline::transitions`]: crate::Timeline::transitions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<Transition>,
    /// Position, scale, rotation, opacity and volume changing over the
    /// clip; a property with keys ignores its plain value. Read the values
    /// at a time with [`Clip::transform_at`], [`Clip::opacity_at`] and
    /// [`Clip::volume_at`].
    #[serde(default, skip_serializing_if = "Keyframes::is_empty")]
    pub keyframes: Keyframes,
    /// Video only: how the picture mixes with what the lanes under it show.
    #[serde(default, skip_serializing_if = "BlendMode::is_normal")]
    pub blend: BlendMode,
    /// Clears the parts of the picture near a colour, such as a green
    /// screen. Saved only when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma: Option<ChromaKey>,
    /// Keeps the part of the picture inside a shape and clears the rest.
    /// Saved only when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Mask>,
}

/// The slowest and fastest a clip plays.
pub const MIN_SPEED: f64 = 0.25;
pub const MAX_SPEED: f64 = 4.0;

fn normal_speed() -> f64 {
    1.0
}

fn is_normal_speed(speed: &f64) -> bool {
    *speed == 1.0
}

fn is_black(background: &Background) -> bool {
    *background == Background::Black
}

impl Clip {
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }

    pub fn contains(&self, time: f64) -> bool {
        time >= self.start && time < self.end()
    }

    /// One picture held for the whole clip: a photo or a freeze frame.
    pub fn still(&self) -> bool {
        self.photo || self.freeze
    }

    /// The source time shown or heard at timeline `time`. A still shows
    /// its one picture throughout.
    pub fn source_at(&self, time: f64) -> f64 {
        if self.still() {
            return self.source_in;
        }
        self.source_in + (time - self.start) * self.speed
    }

    /// The timeline time that shows or plays source time `source`.
    pub fn time_at(&self, source: f64) -> f64 {
        self.start + (source - self.source_in) / self.speed
    }

    /// Where the clip's source range ends: `source_in` plus the source
    /// seconds its duration plays.
    pub fn source_end(&self) -> f64 {
        self.source_at(self.end())
    }

    /// Fades that fit the clip: each at most its length, and when
    /// together they exceed it, both scale down to meet.
    pub fn fade_lengths(&self) -> (f64, f64) {
        fit_fades(self.duration, self.fade_in, self.fade_out)
    }

    /// The value a keyed property has at timeline `time`.
    fn keyed(&self, property: Property, time: f64) -> Option<f64> {
        self.keyframes.value(property, time - self.start)
    }

    /// Where the picture sits at timeline `time`: its transform with any
    /// keyed position, scale and rotation in place.
    pub fn transform_at(&self, time: f64) -> Transform {
        let mut t = self.transform;
        if self.keyframes.moves_transform() {
            t.x = self.keyed(Property::X, time).unwrap_or(t.x);
            t.y = self.keyed(Property::Y, time).unwrap_or(t.y);
            t.scale = self.keyed(Property::Scale, time).unwrap_or(t.scale);
            t.rotation = self.keyed(Property::Rotation, time).unwrap_or(t.rotation);
        }
        t
    }

    pub fn opacity_at(&self, time: f64) -> f32 {
        self.keyed(Property::Opacity, time)
            .map_or(self.opacity, |v| v as f32)
    }

    pub fn volume_at(&self, time: f64) -> f32 {
        self.keyed(Property::Volume, time)
            .map_or(self.volume, |v| v as f32)
    }

    /// This clip with its keyed values at timeline `time` as its plain
    /// ones: what a panel shows for the moment under the playhead.
    pub fn posed_at(&self, time: f64) -> Clip {
        let mut clip = self.clone();
        clip.transform = self.transform_at(time);
        clip.opacity = self.opacity_at(time);
        clip.volume = self.volume_at(time);
        clip
    }

    /// Never heard: its volume is 0, or every volume key is.
    pub fn muted(&self) -> bool {
        match self.keyframes.keys(Property::Volume) {
            [] => self.volume <= 0.0,
            keys => keys.iter().all(|key| key.value <= 0.0),
        }
    }

    /// A property's plain value, or its value at timeline `time` when keyed.
    pub fn property_at(&self, property: Property, time: f64) -> f64 {
        let t = self.transform_at(time);
        match property {
            Property::X => t.x,
            Property::Y => t.y,
            Property::Scale => t.scale,
            Property::Rotation => t.rotation,
            Property::Opacity => self
                .keyed(Property::Opacity, time)
                .unwrap_or_else(|| crate::widen(self.opacity)),
            Property::Volume => self
                .keyed(Property::Volume, time)
                .unwrap_or_else(|| crate::widen(self.volume)),
        }
    }

    /// Word start times on the timeline, when there is one per word of the
    /// title text (a renamed title loses its timing).
    pub fn word_times(&self) -> Option<Vec<f64>> {
        (!self.words.is_empty() && self.words.len() == self.name.split_whitespace().count())
            .then(|| self.words.iter().map(|&word| self.time_at(word)).collect())
    }

    /// The word being spoken at `time`: the last one started by then.
    pub fn active_word(&self, time: f64) -> Option<usize> {
        let times = self.word_times()?;
        times.iter().rposition(|&start| start <= time)
    }
}

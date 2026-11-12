//! A clip: one placed piece of video, audio or title text.

use crate::fit_fades;

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
    /// Speech cleanup (80 Hz high-pass and a gentle compressor) in preview
    /// and export. Projects saved before it load with it off.
    #[serde(default)]
    pub clean_voice: bool,
    /// Fades follow an equal-power quarter sine (FFmpeg's `qsin`) instead
    /// of a straight line; see [`fade_shape`]. Off in older projects.
    #[serde(default)]
    pub smooth_fades: bool,
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

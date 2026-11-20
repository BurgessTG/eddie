//! What an export makes: its picture size, frame rate and quality, as the
//! Export dialog, the `eddie export` flags and the settings file name them.

/// The finished picture's size, by its short side. The canvas keeps the
/// project format's shape at any size, and titles keep their place on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Resolution {
    /// The largest video's short side, at most 1080; 720 without video.
    #[default]
    Auto,
    P480,
    P720,
    P1080,
    P1440,
    P2160,
}

impl Resolution {
    pub const ALL: [Self; 6] = [
        Self::Auto,
        Self::P480,
        Self::P720,
        Self::P1080,
        Self::P1440,
        Self::P2160,
    ];

    /// The short side in pixels; `None` for [`Resolution::Auto`].
    pub fn short_side(self) -> Option<u32> {
        match self {
            Self::Auto => None,
            Self::P480 => Some(480),
            Self::P720 => Some(720),
            Self::P1080 => Some(1080),
            Self::P1440 => Some(1440),
            Self::P2160 => Some(2160),
        }
    }

    /// The name settings and the command line use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::P480 => "480p",
            Self::P720 => "720p",
            Self::P1080 => "1080p",
            Self::P1440 => "1440p",
            Self::P2160 => "2160p",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::P480 => "480p",
            Self::P720 => "720p",
            Self::P1080 => "1080p",
            Self::P1440 => "2K",
            Self::P2160 => "4K",
        }
    }

    /// `auto`, `1080p`, `1080`, `2k` or `4k`, in any case.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|resolution| {
            name == resolution.as_str()
                || name == resolution.label().to_ascii_lowercase()
                || resolution
                    .short_side()
                    .is_some_and(|short| name == short.to_string())
        })
    }
}

/// How much of the picture x264 keeps, as a constant rate factor: a lower
/// one keeps more detail in a larger file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    Lower,
    #[default]
    Recommended,
    Higher,
}

impl Quality {
    pub const ALL: [Self; 3] = [Self::Lower, Self::Recommended, Self::Higher];

    pub(super) fn crf(self) -> u8 {
        match self {
            Self::Lower => 26,
            Self::Recommended => 20,
            Self::Higher => 16,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lower => "lower",
            Self::Recommended => "recommended",
            Self::Higher => "higher",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Lower => "Lower",
            Self::Recommended => "Recommended",
            Self::Higher => "Higher",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|quality| name == quality.as_str())
    }
}

/// The frame rates an export offers, as CapCut does.
pub const FRAME_RATES: [u32; 5] = [24, 25, 30, 50, 60];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExportSettings {
    pub resolution: Resolution,
    /// One of [`FRAME_RATES`], or `None` for the timeline's own rate.
    pub fps: Option<u32>,
    pub quality: Quality,
}

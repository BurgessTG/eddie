//! What an export makes: a video, the sound alone or a GIF, with its picture
//! size, frame rate, quality and format, as the Export dialog, the `eddie
//! export` flags and the settings file name them.

use std::path::{Path, PathBuf};

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

/// What an export writes: a video with its sound, the sound alone, or a
/// silent looping GIF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportKind {
    #[default]
    Video,
    Audio,
    Gif,
}

impl ExportKind {
    pub const ALL: [Self; 3] = [Self::Video, Self::Audio, Self::Gif];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Gif => "gif",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Gif => "GIF",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|kind| name == kind.as_str())
    }
}

/// The file the sound alone is written as, as CapCut offers them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AudioFormat {
    /// 320 kbps.
    #[default]
    Mp3,
    /// 16-bit PCM.
    Wav,
    /// 256 kbps, in an `.m4a`.
    Aac,
    /// Lossless, 16-bit.
    Flac,
}

impl AudioFormat {
    pub const ALL: [Self; 4] = [Self::Mp3, Self::Wav, Self::Aac, Self::Flac];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Aac => "aac",
            Self::Flac => "flac",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mp3 => "MP3",
            Self::Wav => "WAV",
            Self::Aac => "AAC",
            Self::Flac => "FLAC",
        }
    }

    /// The extension a file of this format gets.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Aac => "m4a",
            format => format.as_str(),
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|format| name == format.as_str())
    }

    /// FFmpeg's encoder and its options for this format.
    pub(super) fn encoder(self) -> &'static [&'static str] {
        match self {
            Self::Mp3 => &["-c:a", "libmp3lame", "-b:a", "320k"],
            Self::Wav => &["-c:a", "pcm_s16le"],
            Self::Aac => &["-c:a", "aac", "-b:a", "256k"],
            Self::Flac => &["-c:a", "flac"],
        }
    }

    /// Whether the mix is dithered down to 16 bits for this format.
    pub(super) fn is_pcm(self) -> bool {
        matches!(self, Self::Wav | Self::Flac)
    }
}

/// A GIF's size, by its short side: CapCut Online's choices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GifSize {
    P240,
    P320,
    #[default]
    P480,
    P640,
}

impl GifSize {
    pub const ALL: [Self; 4] = [Self::P240, Self::P320, Self::P480, Self::P640];

    pub fn short_side(self) -> u32 {
        match self {
            Self::P240 => 240,
            Self::P320 => 320,
            Self::P480 => 480,
            Self::P640 => 640,
        }
    }

    /// `480p`; also the label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::P240 => "240p",
            Self::P320 => "320p",
            Self::P480 => "480p",
            Self::P640 => "640p",
        }
    }

    /// `480p` or `480`, in any case.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|size| name == size.as_str() || name == size.short_side().to_string())
    }
}

/// A GIF's frame rate, or the project's when that is lower: smooth enough
/// for motion, at a fraction of the frames.
pub const GIF_FPS: f64 = 15.0;

/// Video containers an export writes under their own extension.
const VIDEO_EXTENSIONS: [&str; 4] = ["mp4", "mov", "m4v", "mkv"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExportSettings {
    pub kind: ExportKind,
    pub resolution: Resolution,
    /// One of [`FRAME_RATES`], or `None` for the timeline's own rate.
    pub fps: Option<u32>,
    pub quality: Quality,
    /// The format of an [`ExportKind::Audio`] export.
    pub audio: AudioFormat,
    /// The size of an [`ExportKind::Gif`] export.
    pub gif: GifSize,
}

impl ExportSettings {
    /// The extension the file these settings make gets.
    pub fn extension(&self) -> &'static str {
        match self.kind {
            ExportKind::Video => "mp4",
            ExportKind::Audio => self.audio.extension(),
            ExportKind::Gif => "gif",
        }
    }

    /// What an export to `path` makes, and the path it writes. A name whose
    /// extension says what it is decides (`.mp3` writes MP3 whatever the
    /// settings say; `.mov` a video); any other name gets these settings'
    /// extension, so `cut` and `cut v1.2` become `cut.mp4` and
    /// `cut v1.2.mp4`.
    pub fn for_output(mut self, path: &Path) -> (PathBuf, Self) {
        let extension = path
            .extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase());
        let extension = extension.as_deref().unwrap_or_default();
        if extension == "gif" {
            self.kind = ExportKind::Gif;
        } else if VIDEO_EXTENSIONS.contains(&extension) {
            self.kind = ExportKind::Video;
        } else if let Some(format) = audio_for(extension) {
            self.kind = ExportKind::Audio;
            self.audio = format;
        } else {
            let mut name = path.as_os_str().to_owned();
            name.push(".");
            name.push(self.extension());
            return (name.into(), self);
        }
        (path.to_path_buf(), self)
    }

    /// The short side of the picture, or `None` for the size Auto picks.
    pub fn short_side(&self) -> Option<u32> {
        match self.kind {
            ExportKind::Gif => Some(self.gif.short_side()),
            _ => self.resolution.short_side(),
        }
    }

    /// The frame rate of the picture, for a timeline at `project` fps.
    pub fn frame_rate(&self, project: f64) -> f64 {
        match self.kind {
            ExportKind::Gif => GIF_FPS.min(project),
            _ => self.fps.map_or(project, f64::from),
        }
    }
}

fn audio_for(extension: &str) -> Option<AudioFormat> {
    match extension {
        "mp3" => Some(AudioFormat::Mp3),
        "wav" => Some(AudioFormat::Wav),
        "m4a" | "aac" => Some(AudioFormat::Aac),
        "flac" => Some(AudioFormat::Flac),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_decides_what_is_made_and_a_name_without_one_gets_the_settings_extension() {
        let audio = ExportSettings {
            kind: ExportKind::Audio,
            audio: AudioFormat::Flac,
            ..ExportSettings::default()
        };
        let cases = [
            // A name that says what it is wins over the settings.
            (
                "song.WAV",
                audio,
                "song.WAV",
                ExportKind::Audio,
                AudioFormat::Wav,
            ),
            (
                "x.aac",
                ExportSettings::default(),
                "x.aac",
                ExportKind::Audio,
                AudioFormat::Aac,
            ),
            (
                "clip.mov",
                audio,
                "clip.mov",
                ExportKind::Video,
                AudioFormat::Flac,
            ),
            (
                "loop.gif",
                audio,
                "loop.gif",
                ExportKind::Gif,
                AudioFormat::Flac,
            ),
            // Anything else keeps its whole name and gets the settings' kind.
            (
                "song",
                audio,
                "song.flac",
                ExportKind::Audio,
                AudioFormat::Flac,
            ),
            (
                "cut v1.2",
                ExportSettings::default(),
                "cut v1.2.mp4",
                ExportKind::Video,
                AudioFormat::Mp3,
            ),
        ];
        for (name, settings, path, kind, format) in cases {
            let (made, made_settings) = settings.for_output(Path::new(name));
            assert_eq!(made, Path::new(path), "{name}");
            assert_eq!(
                (made_settings.kind, made_settings.audio),
                (kind, format),
                "{name}"
            );
        }
    }
}

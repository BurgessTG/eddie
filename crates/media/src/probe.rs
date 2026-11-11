//! What a media file holds, read in process through libavformat.

use std::path::Path;

use crate::{decode, MediaResult};

#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub path: String,
    pub name: String,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_video: bool,
    pub has_audio: bool,
}

/// Probe a media file in process: what it holds, how long it runs and the
/// size its picture is displayed at.
pub fn probe(path: &str) -> MediaResult<MediaInfo> {
    use ffmpeg_next::format::stream::Disposition;
    use ffmpeg_next::media::Type;

    decode::ensure_init();
    let input = ffmpeg_next::format::input(path).map_err(|error| format!("{path}: {error}"))?;
    let rate = |rate: ffmpeg_next::Rational| match rate.denominator() {
        0 => 0.0,
        den => f64::from(rate.numerator()) / f64::from(den),
    };
    let mut info = MediaInfo {
        path: path.to_owned(),
        name: Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_owned()),
        // Unknown is a large negative number.
        duration: (input.duration() as f64 / f64::from(ffmpeg_next::ffi::AV_TIME_BASE)).max(0.0),
        width: 0,
        height: 0,
        fps: 0.0,
        has_video: false,
        has_audio: false,
    };
    for stream in input.streams() {
        let parameters = stream.parameters();
        // Cover art in a music file is one attached picture, not video.
        let cover = stream.disposition().contains(Disposition::ATTACHED_PIC);
        match parameters.medium() {
            Type::Video if !info.has_video && !cover => {
                info.has_video = true;
                // SAFETY: the parameters are a valid AVCodecParameters owned
                // by the stream, which outlives this read.
                let (width, height) =
                    unsafe { ((*parameters.as_ptr()).width, (*parameters.as_ptr()).height) };
                info.width = width.max(0) as u32;
                info.height = height.max(0) as u32;
                // Report displayed dimensions, as every decode path rotates.
                if decode::display_turns(&stream) % 2 == 1 {
                    std::mem::swap(&mut info.width, &mut info.height);
                }
                info.fps = rate(stream.avg_frame_rate());
                if info.fps <= 0.0 {
                    info.fps = rate(stream.rate());
                }
            }
            Type::Audio => info.has_audio = true,
            _ => {}
        }
        if info.duration <= 0.0 && !cover {
            let base = stream.time_base();
            info.duration = (stream.duration() as f64 * rate(base)).max(0.0);
        }
    }
    if !info.has_video && !info.has_audio {
        return Err(format!("no audio or video stream in {path}"));
    }
    Ok(info)
}

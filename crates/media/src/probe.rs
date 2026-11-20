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
    /// One picture, such as a PNG, JPEG, WebP or HEIC photo. It has no length
    /// (`duration` and `fps` are 0): a clip shows it for as long as it lasts.
    pub photo: bool,
    /// Its picture has an alpha channel (a PNG with transparency, ProRes
    /// 4444): what is under it on the timeline can show through, so it
    /// never hides the lanes under it.
    pub alpha: bool,
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
        photo: false,
        alpha: false,
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
                info.alpha = has_alpha(&stream);
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
    if info.has_video && decode::is_photo(&input) {
        info.photo = true;
        // Image demuxers give a picture a nominal 25 fps and one frame.
        (info.duration, info.fps) = (0.0, 0.0);
        // A photo's EXIF orientation reaches its decoded picture, not the
        // stream, and a HEIF photo's tiles are joined to one picture, so
        // the decoder says the size.
        (info.width, info.height) = decode::VideoDecoder::open(path, 16, true)?.photo_size();
    }
    Ok(info)
}

/// Whether `stream`'s pixels carry alpha, from its pixel format.
fn has_alpha(stream: &ffmpeg_next::format::stream::Stream) -> bool {
    use ffmpeg_next::ffi::{AVPixFmtDescriptor, AV_PIX_FMT_FLAG_ALPHA};
    use std::os::raw::c_int;
    // Declared over a plain int: a format newer than these bindings is not
    // a valid Rust enum value, and C passes the enum as an int.
    unsafe extern "C" {
        fn av_pix_fmt_desc_get(format: c_int) -> *const AVPixFmtDescriptor;
    }
    // SAFETY: the parameters belong to the open input and outlive this
    // read, and the descriptor table is static.
    unsafe {
        let format = (*stream.parameters().as_ptr()).format;
        let descriptor = av_pix_fmt_desc_get(format);
        !descriptor.is_null() && (*descriptor).flags & AV_PIX_FMT_FLAG_ALPHA as u64 != 0
    }
}

//! Photos: one picture from an image demuxer or a HEIF file, decoded once
//! and handed out again for every seek, since `image2` returns nothing
//! after any seek.

use ffmpeg_next as ffmpeg;

use super::heif::{is_heif, Heif};
use super::{matrix_orientation, Scaler, VideoDecoder};
use crate::MediaResult;

/// Whether `input` is one picture: an image demuxer (`image2`, or one of
/// the `png_pipe`, `jpeg_pipe`, `webp_pipe` family), a HEIF still image or
/// a GIF of one frame, not a container with a timeline. An animated GIF is
/// video.
pub(crate) fn is_photo(input: &ffmpeg::format::context::Input) -> bool {
    is_image_demuxer(input) || is_heif(input) || is_still_gif(input)
}

/// A GIF with one frame. The `gif` demuxer counts a seekable file's frames
/// when it opens; one frame lasts its delay, 0.04 s or so, as video.
fn is_still_gif(input: &ffmpeg::format::context::Input) -> bool {
    input.format().name() == "gif"
        && input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .is_some_and(|stream| stream.frames() == 1)
}

fn is_image_demuxer(input: &ffmpeg::format::context::Input) -> bool {
    let format = input.format();
    format.name() == "image2" || format.name().ends_with("_pipe")
}

/// Open a media file for reading. libavformat's stream probe decodes a
/// photo's whole picture to learn its size and pixel format (40 ms for a
/// 2160 px PNG, 70 ms for a WebP), and the decoder then decodes it again.
/// An image demuxer's stream skips that probe: its decoder reads both from
/// the picture, and `queue_photo` sizes the output from what it decoded.
pub(crate) fn open_input(path: &str) -> Result<ffmpeg::format::context::Input, ffmpeg::Error> {
    use ffmpeg::ffi;
    let path = std::ffi::CString::new(path).map_err(|_| ffmpeg::Error::InvalidData)?;
    let mut context = std::ptr::null_mut();
    // SAFETY: `context` starts null, as avformat_open_input requires, and
    // is owned by the `Input` from here on, which closes it when dropped.
    unsafe {
        match ffi::avformat_open_input(
            &mut context,
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
        ) {
            0 => {}
            error => return Err(ffmpeg::Error::from(error)),
        }
        let input = ffmpeg::format::context::Input::wrap(context);
        if !is_image_demuxer(&input) {
            match ffi::avformat_find_stream_info(context, std::ptr::null_mut()) {
                0.. => {}
                error => return Err(ffmpeg::Error::from(error)),
            }
        }
        Ok(input)
    }
}

impl VideoDecoder {
    /// Decode a photo's one picture now (joining a HEIF grid's tiles) and
    /// queue it as the next, turned as its own display matrix says.
    pub(super) fn queue_photo(&mut self, heif: Option<&Heif>) -> MediaResult<()> {
        let (pts, orientation) = if let Some(Heif::Grid(grid)) = heif {
            self.join_tiles(grid)?;
            (0.0, grid.orientation)
        } else {
            let pts = self
                .receive_decoded()?
                .ok_or_else(|| "the picture did not decode".to_owned())?;
            let orientation = self
                .decoded
                .side_data(ffmpeg::frame::side_data::Type::DisplayMatrix)
                .filter(|data| data.data().len() >= 36)
                // SAFETY: a display matrix is nine i32 values in an
                // av_malloc-aligned side-data buffer owned by the frame.
                .map_or(self.orientation, |data| unsafe {
                    matrix_orientation(data.data().as_ptr().cast())
                });
            (pts, orientation)
        };
        // A grid's picture is larger than the tile the decoder opened with.
        let source = (
            self.decoded.format(),
            self.decoded.width(),
            self.decoded.height(),
        );
        if (source, orientation) != (self.scaler.source, self.orientation) {
            self.orientation = orientation;
            let (scaler, target_h) =
                Scaler::new(source, orientation.turns, self.target_w, self.fast)?;
            (self.scaler, self.target_h) = (scaler, target_h);
        }
        std::mem::swap(&mut self.decoded, &mut self.lookahead);
        self.lookahead_pts = Some(pts);
        Ok(())
    }

    /// The size a photo is displayed at: its picture, turned.
    pub(crate) fn photo_size(&self) -> (u32, u32) {
        let (_, width, height) = self.scaler.source;
        if self.orientation.turns % 2 == 1 {
            (height, width)
        } else {
            (width, height)
        }
    }

    /// The pixel format a photo decoded to.
    pub(crate) fn photo_format(&self) -> ffmpeg::format::Pixel {
        self.scaler.source.0
    }

    /// Back to a photo's one picture: queue it as the next again.
    pub(super) fn rewind_photo(&mut self) {
        if let Some(pts) = self.decoded_pts.take() {
            if self.lookahead_pts.is_none() {
                std::mem::swap(&mut self.decoded, &mut self.lookahead);
                self.lookahead_pts = Some(pts);
            }
        }
        self.recent.clear();
    }
}

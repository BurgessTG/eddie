//! Photos: one picture from an image demuxer or a HEIF file, decoded once
//! and handed out again for every seek, since `image2` returns nothing
//! after any seek.

use ffmpeg_next as ffmpeg;

use super::heif::{is_heif, Heif};
use super::{clockwise_turns, Scaler, VideoDecoder};
use crate::MediaResult;

/// Whether `input` is one picture: an image demuxer (`image2`, or one of
/// the `png_pipe`, `jpeg_pipe`, `webp_pipe` family) or a HEIF still image,
/// not a container with a timeline.
pub(crate) fn is_photo(input: &ffmpeg::format::context::Input) -> bool {
    let format = input.format();
    format.name() == "image2" || format.name().ends_with("_pipe") || is_heif(input)
}

impl VideoDecoder {
    /// Decode a photo's one picture now (joining a HEIF grid's tiles) and
    /// queue it as the next, turned as its own display matrix says.
    pub(super) fn queue_photo(&mut self, heif: Option<&Heif>) -> MediaResult<()> {
        let (pts, turns) = if let Some(Heif::Grid(grid)) = heif {
            self.join_tiles(grid)?;
            (0.0, grid.turns)
        } else {
            let pts = self
                .receive_decoded()?
                .ok_or_else(|| "the picture did not decode".to_owned())?;
            let turns = self
                .decoded
                .side_data(ffmpeg::frame::side_data::Type::DisplayMatrix)
                .filter(|data| data.data().len() >= 36)
                .map_or(self.turns, |data| {
                    // SAFETY: a display matrix is nine i32 values in an
                    // av_malloc-aligned side-data buffer owned by the frame.
                    clockwise_turns(unsafe {
                        ffmpeg::ffi::av_display_rotation_get(data.data().as_ptr().cast())
                    })
                });
            (pts, turns)
        };
        // A grid's picture is larger than the tile the decoder opened with.
        let source = (
            self.decoded.format(),
            self.decoded.width(),
            self.decoded.height(),
        );
        if (source, turns) != (self.scaler.source, self.turns) {
            self.turns = turns;
            let (scaler, target_h) = Scaler::new(source, turns, self.target_w, self.fast)?;
            (self.scaler, self.target_h) = (scaler, target_h);
        }
        std::mem::swap(&mut self.decoded, &mut self.lookahead);
        self.lookahead_pts = Some(pts);
        Ok(())
    }

    /// The size a photo is displayed at: its picture, turned.
    pub(crate) fn photo_size(&self) -> (u32, u32) {
        let (_, width, height) = self.scaler.source;
        if self.turns % 2 == 1 {
            (height, width)
        } else {
            (width, height)
        }
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

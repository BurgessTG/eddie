//! Scaling and converting decoded pictures to RGBA at preview size.

use ffmpeg_next as ffmpeg;

use super::{err, raw_enum};
use crate::{scaled_height, MediaResult};

/// Source pictures to display-size RGBA in two swscale passes: resize in
/// the source format, then convert at the small size with each picture's
/// own matrix and range, as export does. One combined pass cost about
/// 2.8 ms per 4K picture against about 1.25 ms for both, and read every
/// source as BT.601, which oversaturated BT.709 HD footage.
pub(super) struct Scaler {
    resize: ffmpeg::software::scaling::Context,
    convert: ffmpeg::software::scaling::Context,
    resized: ffmpeg::frame::Video,
    rgba: ffmpeg::frame::Video,
    /// swscale matrix and full-range flag last applied to `convert`.
    colors: Option<(i32, i32)>,
    /// Input format and size this scaler reads.
    pub(super) source: (ffmpeg::format::Pixel, u32, u32),
}

impl Scaler {
    /// A scaler for `target_w` display pixels, and the display height.
    pub(super) fn new(
        (source, width, height): (ffmpeg::format::Pixel, u32, u32),
        turns: u8,
        target_w: u32,
        fast: bool,
    ) -> MediaResult<(Self, u32)> {
        use ffmpeg::format::Pixel;
        use ffmpeg::software::scaling::{Context, Flags};
        // target_w/target_h are display dimensions. A quarter-turn scales to
        // the transposed size, then rotates while copying out.
        let quarter = turns % 2 == 1;
        let target_h = if quarter {
            scaled_height(height, width, target_w)
        } else {
            scaled_height(width, height, target_w)
        };
        let (scaled_w, scaled_h) = if quarter {
            (target_h, target_w)
        } else {
            (target_w, target_h)
        };
        let flags = if fast {
            Flags::FAST_BILINEAR
        } else {
            Flags::BICUBIC
        };
        // SAFETY: a pure query on a pixel format value.
        let resizable = unsafe { ffmpeg::ffi::sws_isSupportedOutput(source.into()) } > 0;
        let middle = if resizable { source } else { Pixel::YUV420P };
        let resize = Context::get(source, width, height, middle, scaled_w, scaled_h, flags)
            .map_err(|e| err("create scaler", e))?;
        let convert = Context::get(
            middle,
            scaled_w,
            scaled_h,
            Pixel::RGBA,
            scaled_w,
            scaled_h,
            flags,
        )
        .map_err(|e| err("create converter", e))?;
        let scaler = Self {
            resize,
            convert,
            resized: ffmpeg::frame::Video::empty(),
            rgba: ffmpeg::frame::Video::empty(),
            colors: None,
            source: (source, width, height),
        };
        Ok((scaler, target_h))
    }

    pub(super) fn run(
        &mut self,
        picture: &ffmpeg::frame::Video,
    ) -> MediaResult<&ffmpeg::frame::Video> {
        use ffmpeg::ffi::{self, AVColorRange, AVColorSpace};
        const BT709: u32 = AVColorSpace::AVCOL_SPC_BT709 as u32;
        const FCC: u32 = AVColorSpace::AVCOL_SPC_FCC as u32;
        const SMPTE240M: u32 = AVColorSpace::AVCOL_SPC_SMPTE240M as u32;
        const BT2020_NCL: u32 = AVColorSpace::AVCOL_SPC_BT2020_NCL as u32;
        const BT2020_CL: u32 = AVColorSpace::AVCOL_SPC_BT2020_CL as u32;
        const JPEG: u32 = AVColorRange::AVCOL_RANGE_JPEG as u32;
        const MPEG: u32 = AVColorRange::AVCOL_RANGE_MPEG as u32;
        // SAFETY: the frame is alive for this call and both fields are C
        // enums.
        let (space, range) = unsafe {
            let frame = picture.as_ptr();
            (
                raw_enum(std::ptr::addr_of!((*frame).colorspace)),
                raw_enum(std::ptr::addr_of!((*frame).color_range)),
            )
        };
        // Untagged matrices use swscale's default (BT.601), as the FFmpeg
        // CLI and export do; yuvj formats are full range by definition.
        let matrix = match space {
            BT709 => ffi::SWS_CS_ITU709,
            FCC => ffi::SWS_CS_FCC,
            SMPTE240M => ffi::SWS_CS_SMPTE240M,
            BT2020_NCL | BT2020_CL => ffi::SWS_CS_BT2020,
            _ => ffi::SWS_CS_DEFAULT,
        };
        let full = match range {
            JPEG => 1,
            MPEG => 0,
            _ => i32::from(matches!(
                picture.format(),
                ffmpeg::format::Pixel::YUVJ420P
                    | ffmpeg::format::Pixel::YUVJ422P
                    | ffmpeg::format::Pixel::YUVJ444P
                    | ffmpeg::format::Pixel::YUVJ440P
            )),
        };
        if self.colors != Some((matrix, full)) {
            // SAFETY: the context is owned and initialized; coefficient
            // tables are static. RGB output is always full range.
            unsafe {
                ffi::sws_setColorspaceDetails(
                    self.convert.as_mut_ptr(),
                    ffi::sws_getCoefficients(matrix),
                    full,
                    ffi::sws_getCoefficients(ffi::SWS_CS_DEFAULT),
                    1,
                    0,
                    1 << 16,
                    1 << 16,
                );
            }
            self.colors = Some((matrix, full));
        }
        self.resize
            .run(picture, &mut self.resized)
            .map_err(|e| err("scale frame", e))?;
        self.convert
            .run(&self.resized, &mut self.rgba)
            .map_err(|e| err("convert frame", e))?;
        Ok(&self.rgba)
    }
}

//! How export reads each source's picture: the stream it comes from, and
//! the conversion that brings it to the export's BT.709 limited range.

use crate::decode::{Heif, TileGrid};

use super::video::{VIDEO_COLOR_RANGE, VIDEO_COLOR_SPACE};

pub(super) struct Source {
    picture: Picture,
    /// See [`colorspace_filter`].
    pub(super) convert: Option<String>,
}

enum Picture {
    /// A stream specifier: `v`, or a HEIF photo's image stream.
    Stream(String),
    /// A HEIF photo's tiles, and the filters that join them.
    Tiles(Vec<usize>, String),
}

impl Source {
    /// The start of the filter chain that reads input `index`'s picture,
    /// ready for the next filter's name.
    pub(super) fn head(&self, index: usize) -> String {
        match &self.picture {
            Picture::Stream(specifier) => format!("[{index}:{specifier}]"),
            Picture::Tiles(streams, join) => {
                let mut head: String = streams.iter().map(|s| format!("[{index}:{s}]")).collect();
                head.push_str(join);
                head.push(',');
                head
            }
        }
    }

    /// Decoder threads per stream: one for a tile, of which every one
    /// decodes at once (48 for an iPhone photo); else as many as libavcodec
    /// likes.
    pub(super) fn threads(&self) -> &'static str {
        match self.picture {
            Picture::Tiles(..) => "1",
            Picture::Stream(_) => "0",
        }
    }
}

pub(super) fn read(path: &str) -> Source {
    crate::decode::ensure_init();
    let Ok(input) = ffmpeg_next::format::input(path) else {
        return Source {
            picture: Picture::Stream("v".to_owned()),
            convert: None,
        };
    };
    let heif = crate::decode::heif_picture(&input);
    // A grid's tiles share their coding settings and colour tags.
    let stream = match &heif {
        Some(heif) => input.stream(heif.stream()),
        None => input.streams().best(ffmpeg_next::media::Type::Video),
    };
    let convert = stream.as_ref().and_then(colorspace_filter);
    let picture = match heif {
        None => Picture::Stream("v".to_owned()),
        Some(Heif::Stream(index)) => Picture::Stream(index.to_string()),
        Some(Heif::Grid(grid)) => join(&grid),
    };
    Source { picture, convert }
}

/// The tiles joined on the grid as the preview's decoder joins them (black
/// where none lands), cropped, mirrored and turned.
fn join(grid: &TileGrid) -> Picture {
    let layout: Vec<String> = grid
        .tiles
        .iter()
        .map(|(_, (x, y))| format!("{x}_{y}"))
        .collect();
    let orientation = grid.orientation;
    let mirror = if orientation.mirror { ",hflip" } else { "" };
    let turn =
        ["", ",transpose=clock", ",hflip,vflip", ",transpose=cclock"][orientation.turns as usize];
    Picture::Tiles(
        grid.tiles.iter().map(|&(stream, _)| stream).collect(),
        format!(
            "xstack=inputs={}:layout={}:fill=black,crop={}:{}:{}:{}{mirror}{turn}",
            grid.tiles.len(),
            layout.join("|"),
            grid.width,
            grid.height,
            grid.crop.0,
            grid.crop.1,
        ),
    )
}

/// Filters that bring the picture at `path` to export's 8-bit 4:2:0
/// BT.709 limited range at its own size, as export converts a source.
pub(crate) fn standard_picture(path: &str) -> String {
    match read(path).convert {
        Some(convert) => format!("{convert},format=yuv420p"),
        None => format!(
            "scale=flags=bicubic+accurate_rnd+full_chroma_int:\
             out_color_matrix={VIDEO_COLOR_SPACE}:out_range={VIDEO_COLOR_RANGE},format=yuv420p"
        ),
    }
}

/// The colorspace filter that brings `stream` to the export's BT.709
/// limited range after resizing, or `None` when it already is BT.709
/// limited range or the filter cannot read its format (swscale then
/// converts while resizing). Untagged matrices are BT.601, as the FFmpeg
/// CLI and the preview assume. Primaries and transfer stay as they are, so
/// only the matrix and range change, as before.
fn colorspace_filter(stream: &ffmpeg_next::format::stream::Stream) -> Option<String> {
    use ffmpeg_next::ffi::{AVColorRange, AVColorSpace, AVPixelFormat};
    // SAFETY: the parameters belong to the open input and outlive this read.
    let (space, range, format) = unsafe {
        let parameters = stream.parameters().as_ptr();
        (
            crate::decode::raw_enum(std::ptr::addr_of!((*parameters).color_space)),
            crate::decode::raw_enum(std::ptr::addr_of!((*parameters).color_range)),
            (*parameters).format,
        )
    };
    // Compare raw values: a pixel format newer than these bindings, or a
    // matrix the file made up, is not a valid Rust enum value.
    let is = |f: AVPixelFormat| f as i32 == format;
    use AVPixelFormat::*;
    // The colorspace filter keeps a yuvj input's pixel format while tagging
    // it limited range, and later conversions read yuvj as full range.
    // Name the plain format of the same layout as its output.
    let (output, full_range_format) = [
        (AV_PIX_FMT_YUVJ420P, "yuv420p", true),
        (AV_PIX_FMT_YUVJ422P, "yuv422p", true),
        (AV_PIX_FMT_YUVJ444P, "yuv444p", true),
        (AV_PIX_FMT_YUV420P, "yuv420p", false),
        (AV_PIX_FMT_YUV422P, "yuv422p", false),
        (AV_PIX_FMT_YUV444P, "yuv444p", false),
        (AV_PIX_FMT_YUV420P10LE, "yuv420p10", false),
        (AV_PIX_FMT_YUV422P10LE, "yuv422p10", false),
        (AV_PIX_FMT_YUV444P10LE, "yuv444p10", false),
        (AV_PIX_FMT_YUV420P12LE, "yuv420p12", false),
        (AV_PIX_FMT_YUV422P12LE, "yuv422p12", false),
        (AV_PIX_FMT_YUV444P12LE, "yuv444p12", false),
    ]
    .into_iter()
    .find(|&(f, ..)| is(f))
    .map(|(_, output, full)| (output, full))?;
    let matrix = [
        (AVColorSpace::AVCOL_SPC_BT709, "bt709"),
        (AVColorSpace::AVCOL_SPC_UNSPECIFIED, "smpte170m"),
        (AVColorSpace::AVCOL_SPC_RESERVED, "smpte170m"),
        (AVColorSpace::AVCOL_SPC_SMPTE170M, "smpte170m"),
        (AVColorSpace::AVCOL_SPC_BT470BG, "bt470bg"),
        (AVColorSpace::AVCOL_SPC_SMPTE240M, "smpte240m"),
        (AVColorSpace::AVCOL_SPC_FCC, "fcc"),
        (AVColorSpace::AVCOL_SPC_BT2020_NCL, "bt2020ncl"),
    ]
    .into_iter()
    .find(|&(named, _)| named as u32 == space)
    .map(|(_, matrix)| matrix)?;
    let range = if range == AVColorRange::AVCOL_RANGE_JPEG as u32 {
        "pc"
    } else if range == AVColorRange::AVCOL_RANGE_MPEG as u32 || !full_range_format {
        "tv"
    } else {
        "pc"
    };
    ((matrix, range) != (VIDEO_COLOR_SPACE, VIDEO_COLOR_RANGE)).then(|| {
        format!(
            "colorspace=ispace={matrix}:irange={range}:itrc=bt709:iprimaries=bt709:\
             space={VIDEO_COLOR_SPACE}:range={VIDEO_COLOR_RANGE}:trc=bt709:primaries=bt709:\
             format={output}"
        )
    })
}

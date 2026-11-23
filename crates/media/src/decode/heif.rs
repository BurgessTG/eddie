//! HEIF photos (`.heic`, `.heif`): what iPhones save. libavformat's `mov`
//! demuxer reads them as one stream per image. A large photo is a grid of
//! 512-pixel tiles, each its own stream, joined by a stream group that
//! says where each tile goes, how far to crop and how to turn the result.
//! This module joins them in process; export joins them with `xstack` in
//! its own filter graph (the CLI's `[i:g:N]` output stalled `fps`).

use ffmpeg::ffi;
use ffmpeg_next as ffmpeg;

use super::{err, matrix_orientation, raw_enum, Orientation, VideoDecoder};
use crate::MediaResult;

/// The `ftyp` brands of a HEIF still image. Image sequences (`msf1`,
/// `hevc`) have a timeline and stay video.
const STILL_BRANDS: [&str; 6] = ["heic", "heix", "heim", "heis", "mif1", "mif2"];

/// Whether `input` is a HEIF still image.
pub(crate) fn is_heif(input: &ffmpeg::format::context::Input) -> bool {
    input.format().name().starts_with("mov")
        && input
            .metadata()
            .get("major_brand")
            .is_some_and(|brand| STILL_BRANDS.contains(&brand.trim()))
}

/// Where a HEIF photo's picture is.
pub(crate) enum Heif {
    /// One coded image: the primary one, which `streams().best` picks by
    /// its default disposition.
    Stream(usize),
    Grid(TileGrid),
}

impl Heif {
    /// A stream that holds (part of) the picture, for its codec settings.
    pub(crate) fn stream(&self) -> usize {
        match self {
            Self::Stream(index) => *index,
            Self::Grid(grid) => grid.tiles[0].0,
        }
    }
}

/// A picture made of tiles.
pub(crate) struct TileGrid {
    /// Each tile's stream and where its top-left corner lands on the grid.
    pub(crate) tiles: Vec<(usize, (i32, i32))>,
    /// Where the picture starts on the grid, and its size.
    pub(crate) crop: (i32, i32),
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// From the grid's display matrix (HEIF's `irot` and `imir`;
    /// libavformat drops EXIF orientation, which HEIF says is only
    /// informative).
    pub(crate) orientation: Orientation,
}

/// The picture of a HEIF photo, or `None` when `input` is not one. Of
/// several grids the default (the file's primary image) wins, then the
/// largest, as the FFmpeg CLI chooses.
pub(crate) fn heif_picture(input: &ffmpeg::format::context::Input) -> Option<Heif> {
    if !is_heif(input) {
        return None;
    }
    // SAFETY: the groups, their tile grids, offsets and side data belong to
    // the open input and are only read while it lives.
    let grid = unsafe {
        let context = &*input.as_ptr();
        (0..context.nb_stream_groups as usize)
            .map(|index| &**context.stream_groups.add(index))
            .filter(|group| {
                raw_enum(std::ptr::addr_of!(group.type_))
                    == ffi::AVStreamGroupParamsType::AV_STREAM_GROUP_PARAMS_TILE_GRID as u32
                    // The CLI reads a one-tile grid as its stream.
                    && (*group.params.tile_grid).nb_tiles > 1
            })
            .max_by_key(|group| {
                let grid = &*group.params.tile_grid;
                (
                    group.disposition & ffi::AV_DISPOSITION_DEFAULT != 0,
                    i64::from(grid.width) * i64::from(grid.height),
                )
            })
            .map(|group| {
                let grid = &*group.params.tile_grid;
                let offsets = std::slice::from_raw_parts(grid.offsets, grid.nb_tiles as usize);
                let tiles = offsets
                    .iter()
                    .map(|tile| {
                        let stream = &**group.streams.add(tile.idx as usize);
                        (stream.index as usize, (tile.horizontal, tile.vertical))
                    })
                    .collect();
                let matrix = ffi::av_packet_side_data_get(
                    grid.coded_side_data,
                    grid.nb_coded_side_data,
                    ffi::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
                );
                let orientation = if matrix.is_null() || (*matrix).size < 36 {
                    Orientation::default()
                } else {
                    matrix_orientation((*matrix).data.cast())
                };
                TileGrid {
                    tiles,
                    crop: (grid.horizontal_offset, grid.vertical_offset),
                    width: grid.width.max(1) as u32,
                    height: grid.height.max(1) as u32,
                    orientation,
                }
            })
    };
    Some(match grid {
        Some(grid) => Heif::Grid(grid),
        None => Heif::Stream(input.streams().best(ffmpeg::media::Type::Video)?.index()),
    })
}

impl VideoDecoder {
    /// Decode every tile of `grid` and join them into `decoded`, with the
    /// grid's turns. One decoder takes them all: each tile is a picture of
    /// its own, so frame threads decode several at once. A tile's packet
    /// carries its stream index as its timestamp, which comes back on its
    /// picture whatever order the threads finish in.
    pub(super) fn join_tiles(&mut self, grid: &TileGrid) -> MediaResult<()> {
        let settings = |stream: usize| {
            self.input.stream(stream).map(|stream| {
                // SAFETY: the parameters belong to the open input.
                unsafe {
                    let parameters = &*stream.parameters().as_ptr();
                    std::slice::from_raw_parts(
                        parameters.extradata,
                        parameters.extradata_size.max(0) as usize,
                    )
                    .to_vec()
                }
            })
        };
        let first = settings(grid.tiles[0].0);
        if grid
            .tiles
            .iter()
            .any(|&(stream, _)| settings(stream) != first)
        {
            return Err("its tiles are coded with different settings".to_owned());
        }
        let mut canvas = None;
        let mut joined = 0;
        let mut packet = ffmpeg::Packet::empty();
        loop {
            match packet.read(&mut self.input) {
                Ok(()) => {
                    let stream = packet.stream();
                    if !grid.tiles.iter().any(|&(tile, _)| tile == stream) {
                        continue;
                    }
                    packet.set_pts(Some(stream as i64));
                    packet.set_dts(Some(stream as i64));
                    loop {
                        match self.decoder.send_packet(&packet) {
                            Ok(()) => break,
                            // A full decoder always has a picture to give.
                            Err(ffmpeg::Error::Other { errno })
                                if errno == ffmpeg::error::EAGAIN =>
                            {
                                joined += self.join_next(grid, &mut canvas)?.unwrap_or(0);
                            }
                            Err(e) => return Err(err("send tile", e)),
                        }
                    }
                }
                Err(ffmpeg::Error::Eof) => break,
                Err(e) => return Err(err("read tile", e)),
            }
        }
        self.decoder.send_eof().map_err(|e| err("send EOF", e))?;
        while let Some(placed) = self.join_next(grid, &mut canvas)? {
            joined += placed;
        }
        if joined < grid.tiles.len() {
            return Err(format!(
                "{} of its {} tiles did not decode",
                grid.tiles.len() - joined,
                grid.tiles.len()
            ));
        }
        self.decoded = canvas.expect("every tile was joined");
        self.decoded.set_pts(Some(0));
        Ok(())
    }

    /// Receive one decoded tile and put it everywhere the grid places its
    /// stream: how many places that filled, or `None` once every tile is out.
    fn join_next(
        &mut self,
        grid: &TileGrid,
        canvas: &mut Option<ffmpeg::frame::Video>,
    ) -> MediaResult<Option<usize>> {
        let mut tile = ffmpeg::frame::Video::empty();
        match self.decoder.receive_frame(&mut tile) {
            Ok(()) => {}
            Err(ffmpeg::Error::Eof) => return Ok(None),
            Err(e) => return Err(err("receive tile", e)),
        }
        let canvas = canvas.get_or_insert_with(|| {
            let mut canvas = ffmpeg::frame::Video::new(tile.format(), grid.width, grid.height);
            // SAFETY: both frames are allocated pictures; this copies the
            // colour tags (and side data) and fills the new picture black
            // where no tile lands.
            unsafe {
                ffi::av_frame_copy_props(canvas.as_mut_ptr(), tile.as_ptr());
                let frame = &*canvas.as_ptr();
                let linesizes = frame.linesize.map(|size| size as isize);
                ffi::av_image_fill_black(
                    frame.data.as_ptr().cast(),
                    linesizes.as_ptr(),
                    tile.format().into(),
                    frame.color_range,
                    grid.width as i32,
                    grid.height as i32,
                );
            }
            canvas
        });
        let stream = tile.pts().unwrap_or(-1);
        let mut placed = 0;
        for &(_, (x, y)) in grid.tiles.iter().filter(|(s, _)| *s as i64 == stream) {
            place(canvas, &tile, (x - grid.crop.0, y - grid.crop.1));
            placed += 1;
        }
        Ok(Some(placed))
    }
}

/// Copy `tile` onto `canvas` with its top-left corner at `(x, y)`, keeping
/// only what lands inside. Planes 1 and 2 are subsampled, as libavutil lays
/// out every planar format.
fn place(canvas: &mut ffmpeg::frame::Video, tile: &ffmpeg::frame::Video, (x, y): (i32, i32)) {
    if tile.format() != canvas.format() {
        return;
    }
    let (from_x, from_y) = ((-x).max(0), (-y).max(0));
    let (to_x, to_y) = (x.max(0), y.max(0));
    let width = (tile.width() as i32 - from_x).min(canvas.width() as i32 - to_x);
    let height = (tile.height() as i32 - from_y).min(canvas.height() as i32 - to_y);
    if width <= 0 || height <= 0 {
        return;
    }
    let format: ffi::AVPixelFormat = canvas.format().into();
    let chroma_h = canvas
        .format()
        .descriptor()
        .map_or(0, |d| d.log2_chroma_h());
    for plane in 0..canvas.planes().min(tile.planes()) {
        // SAFETY: a pure size computation on a valid pixel format.
        let bytes = |pixels: i32| unsafe {
            ffi::av_image_get_linesize(format, pixels, plane as i32).max(0) as usize
        };
        let shift = if plane == 1 || plane == 2 {
            chroma_h
        } else {
            0
        };
        let (from_byte, to_byte) = (bytes(from_x), bytes(to_x));
        let length = (bytes(from_x + width) - from_byte).min(bytes(to_x + width) - to_byte);
        let (from_row, to_row) = ((from_y >> shift) as usize, (to_y >> shift) as usize);
        let rows = ((((from_y + height) + (1 << shift) - 1) >> shift) as usize - from_row)
            .min(canvas.plane_height(plane) as usize - to_row)
            .min(tile.plane_height(plane) as usize - from_row);
        let (from_stride, to_stride) = (tile.stride(plane), canvas.stride(plane));
        let source = tile.data(plane);
        let target = canvas.data_mut(plane);
        for row in 0..rows {
            let from = (from_row + row) * from_stride + from_byte;
            let to = (to_row + row) * to_stride + to_byte;
            target[to..to + length].copy_from_slice(&source[from..from + length]);
        }
    }
}

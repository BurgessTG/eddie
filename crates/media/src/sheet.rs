//! Contact sheets of the program output, so an editing agent that cannot
//! watch video can still see the pictures and titles its edit placed.
//! Cells decode in process; one ffmpeg run draws labels and titles with
//! export's own title table and writes the JPEG.

use std::io::{Read, Write};
use std::process::Stdio;
use std::thread;

use openreel_engine::{BlendMode, ChromaKey, Mask, Placement};

use crate::composite::{Canvas, Layer};
use crate::export::{filter_value, font_file};
use crate::{scaled_height, MediaResult, RawFrame, TitleLook, VideoDecoder};

/// Strip under each cell holding its label.
const LABEL_H: usize = 26;
const GUTTER: usize = 4;
/// mjpeg `-q:v` for source strips; 7 was 32-39% smaller than the
/// sheet's 3 with labels and shots still clear.
const STRIP_QUALITY: u32 = 7;

/// Decoders a sheet keeps open, so cells that go back and forth between a
/// picture and the one under it do not reopen either.
const OPEN_DECODERS: usize = 4;

/// One cell: the pictures shown at a program time, as preview and export
/// compose them, and the label drawn under it.
pub struct SheetCell {
    /// The pictures, bottom first, over black. None leaves the cell black.
    pub layers: Vec<SheetLayer>,
    /// The active title's text, look ([`crate::clip_title_look`]) and opacity.
    pub title: Option<(String, TitleLook, f32)>,
    pub label: String,
}

/// One picture of a cell. The colour grade is not drawn; a chroma key is,
/// since it decides what shows.
pub struct SheetLayer {
    /// Source path and the source time shown.
    pub path: String,
    pub time: f64,
    /// Where it lands on the canvas ([`openreel_engine::place`]) and its
    /// file's displayed size. `None` fits the whole picture to the cell.
    pub placement: Option<(Placement, (f64, f64))>,
    /// Its opacity over what is under it.
    pub opacity: f32,
    /// Drawn over a blurred copy of itself that fills the canvas.
    pub blur: bool,
    /// How it mixes with the pictures under it.
    pub blend: BlendMode,
    pub chroma: Option<ChromaKey>,
    pub mask: Option<Mask>,
}

impl SheetLayer {
    /// The picture of `path` at `time`, fitted to the cell.
    pub fn whole(path: String, time: f64) -> Self {
        Self {
            path,
            time,
            placement: None,
            opacity: 1.0,
            blur: false,
            blend: BlendMode::Normal,
            chroma: None,
            mask: None,
        }
    }
}

/// A labelled JPEG grid of `cells` on the export `canvas` aspect, four
/// columns wide. Undecodable pictures stay black; their label says so.
pub fn contact_sheet(cells: &[SheetCell], canvas: (u32, u32)) -> MediaResult<Vec<u8>> {
    let portrait = canvas.1 > canvas.0;
    render(cells, canvas, if portrait { 216 } else { 384 }, 4, 3)
}

/// Like [`contact_sheet`] at half the cell width and eight columns: many
/// small pictures sampled through source files, for finding a shot.
pub fn source_strip(cells: &[SheetCell], canvas: (u32, u32)) -> MediaResult<Vec<u8>> {
    let portrait = canvas.1 > canvas.0;
    // Lower JPEG quality: these pictures are for finding a shot, and at
    // the sheet's quality a strip was most of a check request's bytes.
    render(
        cells,
        canvas,
        if portrait { 108 } else { 192 },
        8,
        STRIP_QUALITY,
    )
}

fn render(
    cells: &[SheetCell],
    canvas: (u32, u32),
    cell_w: usize,
    per_row: usize,
    quality: u32,
) -> MediaResult<Vec<u8>> {
    if cells.is_empty() {
        return Err("no cells".to_owned());
    }
    let grid = Grid::new(cells.len(), canvas, cell_w, per_row);
    let mut sheet = vec![0_u8; grid.width * grid.height * 4];
    for pixel in sheet.as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[38, 38, 42, 255]);
    }

    let mut labels: Vec<String> = cells.iter().map(|cell| cell.label.clone()).collect();
    let mut decoders = Decoders::default();
    let size = (f64::from(canvas.0), f64::from(canvas.1));
    for (index, cell) in cells.iter().enumerate() {
        let mut picture = Canvas::new(grid.cell_w, grid.cell_h, size);
        for layer in &cell.layers {
            let drawn = match layer.placement {
                None => decoders
                    .fitted(&layer.path, layer.time, &grid)
                    .map(|frame| fit(&mut picture, &frame, layer.opacity)),
                Some((placement, source)) => decoders
                    .placed(layer, &placement, source, &grid, size)
                    .map(|frame| {
                        picture.draw(&Layer {
                            image: (&frame).into(),
                            source,
                            placement,
                            opacity: layer.opacity,
                            blur: layer.blur,
                            blend: layer.blend,
                            mask: layer.mask,
                            shift: [0.0, 0.0],
                            window: None,
                        })
                    }),
            };
            if drawn.is_err() {
                labels[index].push_str(" (undecodable)");
                break;
            }
        }
        grid.put(&mut sheet, index, &picture);
    }
    let filters = text_filters(cells, &labels, &grid, canvas);
    encode(sheet, &grid, &filters, quality)
}

/// Where the cells sit on the sheet, in pixels.
struct Grid {
    cell_w: usize,
    cell_h: usize,
    per_row: usize,
    width: usize,
    height: usize,
}

impl Grid {
    fn new(cells: usize, canvas: (u32, u32), cell_w: usize, per_row: usize) -> Self {
        let cell_h = scaled_height(canvas.0, canvas.1, cell_w as u32) as usize;
        let columns = cells.min(per_row);
        let rows = cells.div_ceil(per_row);
        Self {
            cell_w,
            cell_h,
            per_row,
            width: columns * cell_w + (columns + 1) * GUTTER,
            height: rows * (cell_h + LABEL_H) + (rows + 1) * GUTTER,
        }
    }

    /// The top-left corner of cell `index`.
    fn origin(&self, index: usize) -> (usize, usize) {
        let (column, row) = (index % self.per_row, index / self.per_row);
        (
            GUTTER + column * (self.cell_w + GUTTER),
            GUTTER + row * (self.cell_h + LABEL_H + GUTTER),
        )
    }

    /// Copy cell `index`'s picture onto the sheet.
    fn put(&self, sheet: &mut [u8], index: usize, picture: &Canvas) {
        let (x0, y0) = self.origin(index);
        let line = self.cell_w * 4;
        for (row, src) in picture.rgba.chunks_exact(line).enumerate() {
            let start = ((y0 + row) * self.width + x0) * 4;
            sheet[start..start + line].copy_from_slice(src);
        }
    }
}

/// Centre `frame` on `picture`, faded against black by `opacity`.
fn fit(picture: &mut Canvas, frame: &RawFrame, opacity: f32) {
    let (cell_w, cell_h) = (picture.width, picture.height);
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (dx, dy) = ((cell_w - w) / 2, (cell_h - h.min(cell_h)) / 2);
    let gain = opacity.clamp(0.0, 1.0);
    for row in 0..h.min(cell_h) {
        let src = &frame.rgba[row * w * 4..(row + 1) * w * 4];
        let start = ((dy + row) * cell_w + dx) * 4;
        for (dst, src) in picture.rgba[start..start + w * 4]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(src.as_chunks::<4>().0)
        {
            for channel in 0..3 {
                dst[channel] = (f32::from(src[channel]) * gain).round() as u8;
            }
        }
    }
}

/// The decoders a sheet has open, the most recently used last.
#[derive(Default)]
struct Decoders(Vec<(String, VideoDecoder)>);

impl Decoders {
    /// `path`'s decoder, writing `width` wide pictures.
    fn get(&mut self, path: &str, width: u32) -> MediaResult<&mut VideoDecoder> {
        let decoder = match self.0.iter().position(|(open, _)| open == path) {
            Some(index) => self.0.remove(index).1,
            None => VideoDecoder::open(path, width, false)?,
        };
        if self.0.len() >= OPEN_DECODERS {
            self.0.remove(0);
        }
        self.0.push((path.to_owned(), decoder));
        let decoder = &mut self.0.last_mut().expect("just pushed").1;
        decoder.set_output(width, false)?;
        Ok(decoder)
    }

    /// The picture of `path` at `time`, sized to fit a cell.
    fn fitted(&mut self, path: &str, time: f64, grid: &Grid) -> MediaResult<RawFrame> {
        let decoder = self.get(path, grid.cell_w as u32)?;
        let (w, h) = decoder.output_size();
        if h as usize > grid.cell_h {
            // Taller than the canvas: fit the height (pillarbox).
            let fit = (w as usize * grid.cell_h / h as usize) / 2 * 2;
            decoder.set_output(fit.max(2) as u32, false)?;
        }
        decoder.still_at(time)
    }

    /// `layer`'s picture, the whole file as wide as `placement` shows it
    /// in a cell (at most 1920 pixels).
    fn placed(
        &mut self,
        layer: &SheetLayer,
        placement: &Placement,
        source: (f64, f64),
        grid: &Grid,
        canvas: (f64, f64),
    ) -> MediaResult<RawFrame> {
        let shown = source.0 * placement.size[0] / placement.crop[2].max(1.0);
        let width = shown * grid.cell_w as f64 / canvas.0;
        let width = ((width / 2.0).ceil() * 2.0).clamp(16.0, 1920.0) as u32;
        let mut frame = self.get(&layer.path, width)?.still_at(layer.time)?;
        if let Some(key) = &layer.chroma {
            key.apply_rgba(&mut frame.rgba, frame.width as usize);
        }
        Ok(frame)
    }
}

/// The drawtext chain for every label and title: export's title table,
/// scaled to the cell. `null` without a font.
fn text_filters(cells: &[SheetCell], labels: &[String], grid: &Grid, canvas: (u32, u32)) -> String {
    let Some(font) = font_file() else {
        return "null".to_owned();
    };
    let font = filter_value(&font);
    let (cell_w, cell_h) = (grid.cell_w, grid.cell_h);
    let scale = cell_h as f32 / canvas.1.max(1) as f32;
    let mut filters = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        let (x0, y0) = grid.origin(index);
        filters.push(format!(
            "drawtext=fontfile={font}:expansion=none:text={}:fontcolor=white:\
             fontsize=15:x={}:y={}",
            filter_value(&labels[index]),
            x0 + 6,
            y0 + cell_h + 6,
        ));
        let Some((text, look, opacity)) = &cell.title else {
            continue;
        };
        let mut look = look.scaled(f64::from(scale));
        look.font_size = look.font_size.max(6);
        let (dx, dy) = (
            (look.offset.0 * cell_w as f64).round() as isize,
            (look.offset.1 * cell_h as f64).round() as isize,
        );
        let x = match look.left {
            Some(left) => format!("{}", x0 as isize + left as isize + dx),
            None => format!("{x0}+({cell_w}-text_w)/2{dx:+}"),
        };
        let y = y0 as isize + cell_h as isize - look.bottom as isize + dy;
        filters.push(format!(
            "drawtext=fontfile={font}:expansion=none:text={}:{}:x={x}:y={y}",
            // One line, as export draws it.
            filter_value(&text.split_whitespace().collect::<Vec<_>>().join(" ")),
            look.drawtext_options(*opacity),
        ));
    }
    filters.join(",")
}

/// One ffmpeg run: draw `filters` over the RGBA `sheet` and return a JPEG.
fn encode(sheet: Vec<u8>, grid: &Grid, filters: &str, quality: u32) -> MediaResult<Vec<u8>> {
    let mut child = crate::cli::ffmpeg()
        .args(["-v", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s"])
        .arg(format!("{}x{}", grid.width, grid.height))
        .args(["-i", "pipe:0", "-vf", filters, "-frames:v", "1"])
        .args([
            "-q:v",
            &quality.to_string(),
            "-c:v",
            "mjpeg",
            "-f",
            "image2",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("start ffmpeg for contact sheet: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = thread::spawn(move || stdin.write_all(&sheet));
    let mut jpeg = Vec::new();
    let mut errors = String::new();
    child
        .stdout
        .take()
        .expect("piped stdout")
        .read_to_end(&mut jpeg)
        .map_err(|e| format!("read contact sheet: {e}"))?;
    let _ = child
        .stderr
        .take()
        .expect("piped stderr")
        .read_to_string(&mut errors);
    let status = child.wait().map_err(|e| format!("ffmpeg: {e}"))?;
    let written = writer.join().map_err(|_| "contact sheet writer panicked")?;
    if !status.success() || written.is_err() || !jpeg.starts_with(&[0xFF, 0xD8]) {
        return Err(crate::cli::cannot_draw_text(&errors)
            .unwrap_or_else(|| format!("contact sheet failed: {}", errors.trim())));
    }
    Ok(jpeg)
}

//! Contact sheets of the program output, so an editing agent that cannot
//! watch video can still see the pictures and titles its edit placed.
//! Cells decode in process; one ffmpeg run draws labels and titles with
//! export's own title table and writes the JPEG.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;

use openreel_engine::TitleStyle;

use crate::export::{escape_drawtext, font_file};
use crate::{scaled_height, title_look, MediaResult, VideoDecoder};

/// Strip under each cell holding its label.
const LABEL_H: usize = 26;
const GUTTER: usize = 4;
/// mjpeg `-q:v` for source strips; 7 was 32-39% smaller than the
/// sheet's 3 with labels and shots still clear.
const STRIP_QUALITY: u32 = 7;

/// One cell: the picture shown at a program time, as preview and export
/// compose it, and the label drawn under it.
pub struct SheetCell {
    /// Source path and source time of the visible video, if any.
    pub picture: Option<(String, f64)>,
    /// The picture's opacity; it fades against black.
    pub opacity: f32,
    /// The active title's text, style and opacity.
    pub title: Option<(String, TitleStyle, f32)>,
    pub label: String,
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
    let cell_h = scaled_height(canvas.0, canvas.1, cell_w as u32) as usize;
    let columns = cells.len().min(per_row);
    let rows = cells.len().div_ceil(per_row);
    let width = columns * cell_w + (columns + 1) * GUTTER;
    let height = rows * (cell_h + LABEL_H) + (rows + 1) * GUTTER;
    let mut sheet = vec![0_u8; width * height * 4];
    for pixel in sheet.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[38, 38, 42, 255]);
    }
    let origin = |index: usize| {
        let (column, row) = (index % per_row, index / per_row);
        (
            GUTTER + column * (cell_w + GUTTER),
            GUTTER + row * (cell_h + LABEL_H + GUTTER),
        )
    };

    let mut labels: Vec<String> = cells.iter().map(|cell| cell.label.clone()).collect();
    let mut decoder: Option<(String, VideoDecoder)> = None;
    for (index, cell) in cells.iter().enumerate() {
        let (x0, y0) = origin(index);
        for row in 0..cell_h {
            let start = ((y0 + row) * width + x0) * 4;
            for pixel in sheet[start..start + cell_w * 4].chunks_exact_mut(4) {
                pixel.copy_from_slice(&[0, 0, 0, 255]);
            }
        }
        let Some((path, time)) = &cell.picture else {
            continue;
        };
        let picture = (|| {
            if decoder.as_ref().is_none_or(|(open, _)| open != path) {
                decoder = Some((
                    path.clone(),
                    VideoDecoder::open(path, cell_w as u32, false)?,
                ));
            }
            let decoder = &mut decoder.as_mut().unwrap().1;
            decoder.set_output(cell_w as u32, false)?;
            let (w, h) = decoder.output_size();
            if h as usize > cell_h {
                // Taller than the canvas: fit the height (pillarbox).
                let fit = (w as usize * cell_h / h as usize) / 2 * 2;
                decoder.set_output(fit.max(2) as u32, false)?;
            }
            decoder.still_at(*time)
        })();
        let frame = match picture {
            Ok(frame) => frame,
            Err(_) => {
                labels[index].push_str(" (undecodable)");
                continue;
            }
        };
        let (w, h) = (frame.width as usize, frame.height as usize);
        let (dx, dy) = (x0 + (cell_w - w) / 2, y0 + (cell_h - h.min(cell_h)) / 2);
        let gain = cell.opacity.clamp(0.0, 1.0);
        for row in 0..h.min(cell_h) {
            let src = &frame.rgba[row * w * 4..(row + 1) * w * 4];
            let start = ((dy + row) * width + dx) * 4;
            for (dst, src) in sheet[start..start + w * 4]
                .chunks_exact_mut(4)
                .zip(src.chunks_exact(4))
            {
                for channel in 0..3 {
                    dst[channel] = (f32::from(src[channel]) * gain).round() as u8;
                }
            }
        }
    }

    // Labels and titles: export's drawtext table, scaled to the cell.
    let mut filters = Vec::new();
    if let Some(font) = font_file() {
        let scale = cell_h as f32 / canvas.1.max(1) as f32;
        for (index, cell) in cells.iter().enumerate() {
            let (x0, y0) = origin(index);
            filters.push(format!(
                "drawtext=fontfile={font}:text='{}':fontcolor=white:fontsize=15:x={}:y={}",
                escape_drawtext(&labels[index]),
                x0 + 6,
                y0 + cell_h + 6,
            ));
            let Some((text, style, opacity)) = &cell.title else {
                continue;
            };
            let look = title_look(*style);
            let (r, g, b) = look.color;
            let border = ((look.border as f32 * scale).round() as u32).max(1);
            let box_options = match look.box_alpha {
                Some(alpha) => format!("box=1:boxcolor=black@{alpha:.2}:boxborderw={border}"),
                None => format!("box=0:borderw={border}:bordercolor=black@0.8"),
            };
            let x = look.left.map_or_else(
                || format!("{x0}+({cell_w}-text_w)/2"),
                |left| format!("{}", x0 + (left as f32 * scale).round() as usize),
            );
            let y = y0 + cell_h - (look.bottom as f32 * scale).round() as usize;
            filters.push(format!(
                "drawtext=fontfile={font}:text='{}':fontcolor=0x{r:02X}{g:02X}{b:02X}@{:.2}:\
                 fontsize={}:{box_options}:x={x}:y={y}",
                escape_drawtext(text),
                opacity.clamp(0.0, 1.0),
                ((look.font_size as f32 * scale).round() as u32).max(6),
            ));
        }
    }
    let filters = if filters.is_empty() {
        "null".to_owned()
    } else {
        filters.join(",")
    };

    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s"])
        .arg(format!("{width}x{height}"))
        .args(["-i", "pipe:0", "-vf", &filters, "-frames:v", "1"])
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
        return Err(format!("contact sheet failed: {}", errors.trim()));
    }
    Ok(jpeg)
}

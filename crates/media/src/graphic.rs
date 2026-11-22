//! Shapes and stickers drawn to pictures. Each graphic becomes an SVG
//! (`shape` builds a shape's; a sticker's is built in), and resvg draws
//! it. [`write_graphic`] keeps the drawing as a PNG named by its content,
//! which the photo path plays, exports and composites like any photo.
//!
//! The stickers are Microsoft's Fluent Emoji, flat style (MIT; see
//! `graphic/stickers/LICENSE`).

mod shape;

use std::fs;
use std::path::{Path, PathBuf};

use openreel_engine::Graphic;
use resvg::{tiny_skia, usvg};

use crate::{MediaInfo, MediaResult, RawFrame};

/// The long side of a graphic's file, in pixels: a 4K picture's short
/// side, so a graphic up to that size exports sharp.
pub const GRAPHIC_SIDE: u32 = 2160;

/// One built-in sticker.
pub struct Sticker {
    /// What projects and scripts call it, such as `thumbs-up`.
    pub name: &'static str,
    svg: &'static str,
}

impl Sticker {
    /// Its name as words: `thumbs-up` is "Thumbs up".
    pub fn label(&self) -> String {
        let words = self.name.replace('-', " ");
        let mut chars = words.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    }
}

macro_rules! stickers {
    ($($name:literal),* $(,)?) => {
        &[$(Sticker {
            name: $name,
            svg: include_str!(concat!("graphic/stickers/", $name, ".svg")),
        }),*]
    };
}

/// The built-in stickers, in the order the sticker grid shows them.
pub static STICKERS: &[Sticker] = stickers![
    "grinning-face",
    "face-with-tears-of-joy",
    "smiling-face-with-heart-eyes",
    "star-struck",
    "smiling-face-with-sunglasses",
    "winking-face",
    "thinking-face",
    "partying-face",
    "face-with-rolling-eyes",
    "astonished-face",
    "face-screaming-in-fear",
    "exploding-head",
    "loudly-crying-face",
    "angry-face",
    "skull",
    "ghost",
    "thumbs-up",
    "thumbs-down",
    "clapping-hands",
    "waving-hand",
    "folded-hands",
    "victory-hand",
    "flexed-biceps",
    "raising-hands",
    "backhand-index-pointing-right",
    "backhand-index-pointing-down",
    "eyes",
    "red-heart",
    "broken-heart",
    "sparkles",
    "fire",
    "hundred-points",
    "star",
    "glowing-star",
    "collision",
    "check-mark-button",
    "cross-mark",
    "warning",
    "red-question-mark",
    "red-exclamation-mark",
    "party-popper",
    "confetti-ball",
    "wrapped-gift",
    "birthday-cake",
    "trophy",
    "light-bulb",
    "bell",
    "megaphone",
    "rocket",
    "money-bag",
    "clapper-board",
    "musical-notes",
    "speech-balloon",
];

/// The built-in sticker called `name`, in any case.
pub fn sticker(name: &str) -> Option<&'static Sticker> {
    let name = name.trim().to_lowercase();
    STICKERS.iter().find(|sticker| sticker.name == name)
}

/// What a graphic clip is called: the shape's or sticker's name in words.
pub fn graphic_label(graphic: &Graphic) -> String {
    match graphic {
        Graphic::Shape(shape) => shape.kind.label().to_owned(),
        Graphic::Sticker { name } => sticker(name).map_or_else(|| name.clone(), Sticker::label),
    }
}

/// The SVG a graphic draws.
fn svg(graphic: &Graphic) -> MediaResult<String> {
    match graphic {
        Graphic::Shape(shape) if shape.is_valid() => Ok(shape::svg(shape)),
        Graphic::Shape(_) => Err("a shape needs a fill or an outline, in range".to_owned()),
        Graphic::Sticker { name } => sticker(name)
            .map(|sticker| sticker.svg.to_owned())
            .ok_or_else(|| format!("no sticker '{name}'")),
    }
}

/// A parsed drawing and the pixel size it is drawn at.
struct Drawing {
    tree: usvg::Tree,
    width: u32,
    height: u32,
}

impl Drawing {
    /// `svg` sized so its long side is `long` pixels (both sides even, at
    /// least 2).
    fn new(svg: &str, long: u32) -> MediaResult<Self> {
        let tree = usvg::Tree::from_str(svg, &usvg::Options::default())
            .map_err(|error| format!("drawing a graphic: {error}"))?;
        let size = tree.size();
        let scale = long as f32 / size.width().max(size.height());
        let even = |side: f32| (((side * scale) / 2.0).round() as u32 * 2).max(2);
        Ok(Self {
            width: even(size.width()),
            height: even(size.height()),
            tree,
        })
    }

    /// Drawn as straight (not premultiplied) RGBA.
    fn rgba(&self) -> MediaResult<RawFrame> {
        let mut pixmap = tiny_skia::Pixmap::new(self.width, self.height)
            .ok_or_else(|| "a graphic cannot be drawn that size".to_owned())?;
        let size = self.tree.size();
        let transform = tiny_skia::Transform::from_scale(
            self.width as f32 / size.width(),
            self.height as f32 / size.height(),
        );
        resvg::render(&self.tree, transform, &mut pixmap.as_mut());
        let mut rgba = pixmap.take();
        demultiply(&mut rgba);
        Ok(RawFrame {
            width: self.width,
            height: self.height,
            rgba,
        })
    }
}

/// Premultiplied RGBA to straight, rounding to the nearest level. Most of
/// a graphic is wholly clear or wholly covered, which stays as it is
/// (tiny-skia's own per-pixel conversion took 18 to 26 ms a 2160-pixel
/// drawing).
fn demultiply(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if alpha != 0 && alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

/// A graphic drawn `long` pixels on its long side, as straight
/// (not premultiplied) RGBA, for tiles and filmstrips.
pub fn draw_graphic(graphic: &Graphic, long: u32) -> MediaResult<RawFrame> {
    Drawing::new(&svg(graphic)?, long)?.rgba()
}

/// `frame` as a PNG, compressed fast: graphics are mostly flat colour,
/// which compresses well at any level.
fn encode_png(frame: &RawFrame) -> MediaResult<Vec<u8>> {
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&frame.rgba))
        .map_err(|error| format!("encoding a graphic: {error}"))?;
    Ok(png)
}

/// A graphic's file: where it is and what the photo path needs to know.
pub struct GraphicFile {
    pub path: PathBuf,
    pub info: MediaInfo,
    /// It was drawn now, not found already drawn.
    pub drawn: bool,
}

/// Bumped when the drawing of the same SVG changes, so old files are not
/// taken for new ones.
const FILE_VERSION: u64 = 1;

/// The graphic's PNG in `folder`, drawn [`GRAPHIC_SIDE`] on its long side
/// and named by what it draws, so one drawing is one file and an existing
/// file is used as it is. The file is written beside its final name and
/// renamed in, so a reader never sees half of it.
pub fn write_graphic(graphic: &Graphic, folder: &Path) -> MediaResult<GraphicFile> {
    let svg = svg(graphic)?;
    let drawing = Drawing::new(&svg, GRAPHIC_SIDE)?;
    let slug = match graphic {
        Graphic::Shape(shape) => shape.kind.as_str(),
        Graphic::Sticker { name } => name.as_str(),
    };
    let hash = fnv1a(
        [
            FILE_VERSION.to_le_bytes().as_slice(),
            svg.as_bytes(),
            &GRAPHIC_SIDE.to_le_bytes(),
        ]
        .concat()
        .as_slice(),
    );
    let path = folder.join(format!("{slug}-{hash:016x}.png"));
    let drawn = !path.is_file();
    if !drawn {
        // In use again, so the graphics folder's pruning keeps it.
        let _ = fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_modified(std::time::SystemTime::now()));
    } else {
        fs::create_dir_all(folder)
            .map_err(|error| format!("cannot make {}: {error}", folder.display()))?;
        let png = encode_png(&drawing.rgba()?)?;
        let temporary = path.with_extension(format!("png.{}.tmp", std::process::id()));
        fs::write(&temporary, png)
            .and_then(|()| fs::rename(&temporary, &path))
            .map_err(|error| {
                let _ = fs::remove_file(&temporary);
                format!("cannot write {}: {error}", path.display())
            })?;
    }
    let info = MediaInfo {
        path: path.to_string_lossy().into_owned(),
        name: graphic_label(graphic),
        duration: 0.0,
        width: drawing.width,
        height: drawing.height,
        fps: 0.0,
        has_video: true,
        has_audio: false,
        photo: true,
        alpha: true,
    };
    Ok(GraphicFile { path, info, drawn })
}

/// 64-bit FNV-1a: a file name that stays the same across builds and Rust
/// versions, which `DefaultHasher` does not promise.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests;

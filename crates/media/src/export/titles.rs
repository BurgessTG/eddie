//! Titles and captions: one table of looks shared with the program
//! monitor, and the drawtext graph export burns in.

use std::collections::HashMap;
use std::path::PathBuf;

use openreel_engine::{Clip, Rgb, TextLook, TitleStyle, DEFAULT_BOX_OPACITY, DEFAULT_OUTLINE};

use super::frames::{enabled_frames, VideoClock};
use crate::fonts::Face;

/// `value` as one filter option, quoted for both parsers it passes: the
/// option list (`:`, `'` and `\` are special) and then the filtergraph
/// (`'`). With `expansion=none` drawtext draws the text exactly as written.
pub(crate) fn filter_value(value: &str) -> String {
    let option = value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace(':', "\\:");
    format!("'{}'", option.replace('\'', "'\\''"))
}

/// drawtext's options that place a line as Rune does: `y` is the top of
/// the font's line box, the baseline an ascender below it, and a box is
/// the line box tall whatever letters the text has. drawtext's default
/// put `y` at the tallest letter's top, so a box's height followed the
/// text, and a highlight layer of lower-case words sat higher than the
/// line it lit.
pub(crate) const LINE_BOX: &str = "y_align=font";

/// Titles keep the whole-timeline clock: each segment draws the visible
/// frames that fall inside it, shifted to its own frame numbers.
pub(super) struct Titles<'a> {
    /// Every face the titles draw in, written where drawtext reads it.
    pub(super) fonts: HashMap<Face, PathBuf>,
    pub(super) clock: VideoClock,
    /// The output's size against the canvas [`title_look`] is laid out on.
    pub(super) scale: f64,
    pub(super) clips: Vec<(&'a Clip, Vec<(i64, i64)>)>,
}

/// Width of `text` in pixels at `size` (drawtext's em size), from advances
/// and kerning: centres a highlight caption, whose layers share one left edge.
fn text_width(font: &[u8], text: &str, size: f32) -> Option<f32> {
    use ab_glyph::{Font, FontRef};
    let font = FontRef::try_from_slice_and_index(font, 0).ok()?;
    let scale = size / font.units_per_em()?;
    let mut previous = None;
    let mut width = 0.0;
    for c in text.chars() {
        let glyph = font.glyph_id(c);
        if let Some(previous) = previous {
            width += font.kern_unscaled(previous, glyph);
        }
        width += font.h_advance_unscaled(glyph);
        previous = Some(glyph);
    }
    Some(width * scale)
}

impl Titles<'_> {
    pub(super) fn graph(&self, start: i64, end: i64) -> String {
        let mut filters = vec!["[0:v]null[base]".to_owned()];
        let mut current = "base".to_owned();
        {
            for (i, (clip, ranges)) in self.clips.iter().enumerate() {
                let ranges: Vec<_> = ranges
                    .iter()
                    .filter_map(|&(a, b)| {
                        let (a, b) = (a.max(start), b.min(end));
                        (a < b).then_some((a - start, b - start))
                    })
                    .collect();
                if ranges.is_empty() {
                    continue;
                }
                let look = clip_title_look(clip).scaled(self.scale);
                let Some(font) = self.fonts.get(&look.face) else {
                    continue;
                };
                let font = filter_value(&font.to_string_lossy());
                let options = look.drawtext_options(clip.opacity);
                let x = look
                    .left
                    .map_or_else(|| "(w-text_w)/2".to_owned(), |left| left.to_string());
                let y = look.y();
                let words: Vec<&str> = clip.name.split_whitespace().collect();
                let highlight = look
                    .highlight
                    .zip(clip.word_times())
                    .filter(|_| !words.is_empty());
                // Every highlight layer starts at the same left edge, so
                // drawtext lays out each prefix exactly as in the full line.
                let x = look.x(match (&highlight, look.left) {
                    (Some(_), None) => {
                        text_width(look.face.data(), &words.join(" "), look.font_size as f32)
                            .map_or(x, |width| format!("(w-{width:.1})/2"))
                    }
                    _ => x,
                });
                filters.push(format!(
                    "[{current}]drawtext=fontfile={font}:expansion=none:text={}:\
                     {options}:x={x}:y={y}:{LINE_BOX}:enable='{}'[t{i}]",
                    filter_value(&words.join(" ")),
                    enabled_frames(&ranges),
                ));
                current = format!("t{i}");
                let Some((lit, times)) = highlight else {
                    continue;
                };
                // Word n lights while it is spoken: the words through it in
                // the highlight colour, then the words before it back in the
                // text colour on top. No box: the base layer drew it.
                let alpha = clip.opacity.clamp(0.0, 1.0);
                let outline = look.outline_options(alpha);
                for (n, &from) in times.iter().enumerate() {
                    let to = times.get(n + 1).copied().unwrap_or(clip.end());
                    let (a, b) = (self.clock.boundary(from), self.clock.boundary(to));
                    let active: Vec<_> = ranges
                        .iter()
                        .filter_map(|&(ra, rb)| {
                            let (ra, rb) = (ra.max(a - start), rb.min(b - start));
                            (ra < rb).then_some((ra, rb))
                        })
                        .collect();
                    if active.is_empty() {
                        continue;
                    }
                    let enable = enabled_frames(&active);
                    let mut layers = vec![(words[..=n].join(" "), lit)];
                    if n > 0 {
                        layers.push((words[..n].join(" "), look.color));
                    }
                    for (layer, (text, color)) in layers.into_iter().enumerate() {
                        let text = filter_value(&text);
                        filters.push(format!(
                            "[{current}]drawtext=fontfile={font}:expansion=none:text={text}:\
                             fontcolor={}@{alpha:.2}:fontsize={}:box=0{outline}:\
                             x={x}:y={y}:{LINE_BOX}:enable='{enable}'[t{i}w{n}l{layer}]",
                            ffmpeg_color(color),
                            look.font_size,
                        ));
                        current = format!("t{i}w{n}l{layer}");
                    }
                }
            }
        }
        filters.push(format!("[{current}]format=yuv420p[vout]"));
        filters.join(";")
    }
}

/// How a title is drawn, in output-canvas pixels. The program monitor and
/// contact sheets read the same look and scale it to their frame, so
/// titles preview at the size and place they export.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TitleLook {
    /// The font, weight and slant.
    pub face: Face,
    pub font_size: u32,
    pub color: Rgb,
    /// The box behind the text: its colour and opacity. `None` draws none.
    pub text_box: Option<(Rgb, f32)>,
    /// Space between the line box and the edge of the text's box.
    pub padding: u32,
    /// The outline round each letter: width, colour and opacity.
    pub outline: Option<(u32, Rgb, f32)>,
    /// Text left edge; `None` centres the text.
    pub left: Option<u32>,
    /// Distance from the canvas bottom to the top of the line box.
    pub bottom: u32,
    /// Moved from that place by shares of the canvas width and height.
    pub offset: (f64, f64),
    /// Colour of the word being spoken, for titles with word timing.
    pub highlight: Option<Rgb>,
}

impl TitleLook {
    /// The look on a canvas `scale` times as large.
    pub fn scaled(self, scale: f64) -> Self {
        let px = |value: u32| (f64::from(value) * scale).round() as u32;
        Self {
            font_size: px(self.font_size).max(1),
            padding: px(self.padding),
            outline: self
                .outline
                .map(|(width, color, alpha)| (px(width).max(1), color, alpha)),
            left: self.left.map(px),
            bottom: px(self.bottom),
            ..self
        }
    }

    /// drawtext's size, colour, box and outline options for this look on
    /// a title at `opacity`, which fades the box and outline too.
    pub(crate) fn drawtext_options(&self, opacity: f32) -> String {
        let alpha = opacity.clamp(0.0, 1.0);
        let mut options = format!(
            "fontcolor={}@{alpha:.2}:fontsize={}:{}",
            ffmpeg_color(self.color),
            self.font_size,
            self.box_options(alpha)
        );
        options.push_str(&self.outline_options(alpha));
        options
    }

    fn box_options(&self, alpha: f32) -> String {
        match self.text_box {
            Some((color, opacity)) => format!(
                "box=1:boxcolor={}@{:.2}:boxborderw={}",
                ffmpeg_color(color),
                opacity * alpha,
                self.padding
            ),
            None => "box=0".to_owned(),
        }
    }

    fn outline_options(&self, alpha: f32) -> String {
        match self.outline {
            Some((width, color, opacity)) if width > 0 => format!(
                ":borderw={width}:bordercolor={}@{:.2}",
                ffmpeg_color(color),
                opacity * alpha
            ),
            _ => String::new(),
        }
    }

    /// drawtext's `x` for text whose own left edge is `x`, moved by the
    /// look's offset.
    fn x(&self, x: String) -> String {
        match self.offset.0 {
            0.0 => x,
            share => format!("{x}{share:+.5}*w"),
        }
    }

    /// drawtext's `y`: the text top, moved by the look's offset.
    fn y(&self) -> String {
        match self.offset.1 {
            0.0 => format!("h-{}", self.bottom),
            share => format!("h-{}{share:+.5}*h", self.bottom),
        }
    }
}

/// FFmpeg's `0xRRGGBB`.
fn ffmpeg_color(Rgb(r, g, b): Rgb) -> String {
    format!("0x{r:02X}{g:02X}{b:02X}")
}

/// The look a title style gives, before a title's own changes.
pub fn title_look(style: TitleStyle) -> TitleLook {
    let white = Rgb::WHITE;
    let boxed = |opacity| Some((Rgb::BLACK, opacity));
    match style {
        TitleStyle::LowerThird => TitleLook {
            face: Face::default(),
            font_size: 44,
            color: white,
            text_box: boxed(0.55),
            padding: 14,
            outline: None,
            left: Some(42),
            bottom: 110,
            offset: (0.0, 0.0),
            highlight: None,
        },
        TitleStyle::Caption => TitleLook {
            face: Face::default(),
            font_size: 38,
            color: white,
            text_box: boxed(0.65),
            padding: 12,
            outline: None,
            left: None,
            bottom: 90,
            offset: (0.0, 0.0),
            highlight: None,
        },
        TitleStyle::PopCaption => TitleLook {
            face: Face::default(),
            font_size: 48,
            color: Rgb(255, 255, 0),
            text_box: boxed(0.75),
            padding: 14,
            outline: None,
            left: None,
            bottom: 100,
            offset: (0.0, 0.0),
            highlight: None,
        },
        TitleStyle::MinimalCaption => TitleLook {
            face: Face::default(),
            font_size: 34,
            color: white,
            text_box: None,
            padding: 2,
            outline: Some((2, Rgb::BLACK, 0.8)),
            left: None,
            bottom: 70,
            offset: (0.0, 0.0),
            highlight: None,
        },
        TitleStyle::HighlightCaption => TitleLook {
            face: Face::default(),
            font_size: 44,
            color: white,
            text_box: boxed(0.65),
            padding: 12,
            outline: None,
            left: None,
            bottom: 96,
            offset: (0.0, 0.0),
            highlight: Some(Rgb(255, 214, 0)),
        },
    }
}

/// `title`'s [`TextLook`] with every choice made: its own where it has
/// one, else its style's. A box or outline it does not draw is 0.
pub fn drawn_text(title: &Clip) -> TextLook {
    let style = title_look(title.title_style);
    let text = title.text;
    // Bold and italic only where the font has those faces.
    let face = Face::new(
        text.font.unwrap_or(style.face.font()),
        text.bold || style.face.bold(),
        text.italic || style.face.italic(),
    );
    let style_outline = style
        .outline
        .map(|(width, ..)| f64::from(width) / f64::from(style.font_size));
    TextLook {
        color: Some(text.color.unwrap_or(style.color)),
        box_opacity: Some(
            text.box_opacity
                .or(style.text_box.map(|(_, opacity)| f64::from(opacity)))
                .or(text.box_color.map(|_| DEFAULT_BOX_OPACITY))
                .unwrap_or(0.0),
        ),
        box_color: Some(
            text.box_color
                .or(style.text_box.map(|(color, _)| color))
                .unwrap_or(Rgb::BLACK),
        ),
        outline: Some(
            text.outline
                .or(style_outline)
                .or(text.outline_color.map(|_| DEFAULT_OUTLINE))
                .unwrap_or(0.0),
        ),
        outline_color: Some(
            text.outline_color
                .or(style.outline.map(|(_, color, _)| color))
                .unwrap_or(Rgb::BLACK),
        ),
        font: Some(face.font()),
        bold: face.bold(),
        italic: face.italic(),
        ..text
    }
}

/// How `title` is drawn: its style's look with the title's own size,
/// colours, box, outline and place laid over it ([`TextLook`]).
pub fn clip_title_look(title: &Clip) -> TitleLook {
    let style = title_look(title.title_style);
    let drawn = drawn_text(title);
    let font_size = (f64::from(style.font_size) * drawn.size).round().max(1.0) as u32;
    let box_opacity = drawn.box_opacity.unwrap_or(0.0);
    let outline = drawn.outline.unwrap_or(0.0);
    // The style's outline keeps its own opacity; a chosen colour is solid.
    let outline_opacity = match (title.text.outline_color, style.outline) {
        (None, Some((_, _, opacity))) => opacity,
        _ => 1.0,
    };
    let face = Face::new(drawn.font.unwrap_or_default(), drawn.bold, drawn.italic);
    // The line box's lower edge stays where the style puts it, so a larger
    // size or a taller font grows the text upward.
    let line = |face: Face, size: u32| f64::from(size) * f64::from(face.line_height());
    let lower = f64::from(style.bottom) - line(style.face, style.font_size);
    let bottom = (lower + line(face, font_size)).round().max(0.0) as u32;
    TitleLook {
        face,
        font_size,
        color: drawn.color.unwrap_or(style.color),
        text_box: (box_opacity > 0.0)
            .then(|| (drawn.box_color.unwrap_or(Rgb::BLACK), box_opacity as f32)),
        padding: (f64::from(style.padding) * drawn.size).round() as u32,
        outline: (outline > 0.0).then(|| {
            let width = (outline * f64::from(font_size)).round().max(1.0) as u32;
            (
                width,
                drawn.outline_color.unwrap_or(Rgb::BLACK),
                outline_opacity,
            )
        }),
        bottom,
        offset: (drawn.x, drawn.y),
        ..style
    }
}

#[cfg(test)]
mod title_text_tests {
    use super::super::argument_path;
    use super::{filter_value, LINE_BOX};
    use crate::fonts::Face;
    use std::path::Path;

    #[test]
    fn an_output_path_is_never_read_as_an_option() {
        for (given, passed) in [
            ("-cut.mp4", "./-cut.mp4"),
            ("-clips/out.mp4", "./-clips/out.mp4"),
            ("clips/-out.mp4", "clips/-out.mp4"),
            ("/tmp/-cut.mp4", "/tmp/-cut.mp4"),
            ("out.mp4", "out.mp4"),
        ] {
            assert_eq!(argument_path(Path::new(given)), Path::new(passed));
        }
    }

    /// One 640x80 white frame with `text` options drawn on it by drawtext.
    fn drawn(font: &str, text: &str) -> Vec<u8> {
        let out = crate::cli::ffmpeg()
            .args(["-v", "error", "-f", "lavfi", "-i"])
            .arg("color=c=white:s=640x80:r=1:d=1")
            .arg("-vf")
            .arg(format!(
                "drawtext=fontfile={}:expansion=none:{text}:fontsize=24:x=10:y=20:{LINE_BOX}",
                filter_value(font)
            ))
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .output()
            .expect("run ffmpeg");
        assert!(
            out.status.success(),
            "{text}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    #[test]
    fn titles_draw_every_character_as_typed() {
        let dir = std::env::temp_dir().join(format!("eddie-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let font = Face::default().write_into(&dir).unwrap();
        let font = font.to_string_lossy();
        // What the two parsers and drawtext's own expansion treat specially.
        let typed = r"7:05 100% it's a\b, [x]; = %{n} \'";
        let file = dir.join("typed.txt");
        std::fs::write(&file, typed).unwrap();
        // drawtext reads a text file without any parsing: the oracle.
        let expected = drawn(
            &font,
            &format!("textfile={}", filter_value(&file.to_string_lossy())),
        );
        let quoted = drawn(&font, &format!("text={}", filter_value(typed)));
        let _ = std::fs::remove_dir_all(&dir);
        let dark = expected
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|rgb| rgb[0] < 128)
            .count();
        assert!(dark > 500, "the text is drawn, dark pixels={dark}");
        assert!(
            quoted == expected,
            "the quoted text draws the same picture as the file"
        );
    }
}

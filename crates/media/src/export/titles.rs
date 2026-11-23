//! Titles and captions: one table of looks shared with the program
//! monitor, and the drawtext graph export burns in.

use std::collections::HashMap;
use std::path::PathBuf;

use openreel_engine::{
    Clip, Rgb, TextAlign, TextLook, TitleStyle, DEFAULT_BOX_OPACITY, DEFAULT_OUTLINE,
    DEFAULT_SHADOW_OPACITY,
};

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
                let lines = title_lines(clip);
                let x = look.x("text_w");
                let y = look.y(lines.len());
                let words: Vec<&str> = clip.name.split_whitespace().collect();
                let highlight = look
                    .highlight
                    .zip(clip.word_times())
                    .filter(|_| !words.is_empty());
                // Every highlight layer starts at the same left edge, so
                // drawtext lays out each prefix exactly as in the full line.
                let x = match (&highlight, look.align) {
                    (Some(_), TextAlign::Center | TextAlign::Right) => {
                        text_width(look.face.data(), &words.join(" "), look.font_size as f32)
                            .map_or(x, |width| look.x(&format!("{width:.1}")))
                    }
                    _ => x,
                };
                let enable = enabled_frames(&ranges);
                filters.extend(look.filters(
                    &TitleText {
                        font: &font,
                        text: &filter_value(&lines.join("\n")),
                        x: &x,
                        y: &y,
                        enable: Some(&enable),
                        opacity: clip.opacity,
                        tag: &format!("t{i}"),
                    },
                    &current,
                    &format!("t{i}"),
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
    /// The shadow under the letters and their outline.
    pub shadow: Option<TextShadow>,
    /// How the lines line up, and which edge of the text holds its
    /// place: the left edge `margin` in from the canvas's left, the
    /// middle at the canvas's middle, or the right edge `margin` in from
    /// its right.
    pub align: TextAlign,
    pub margin: u32,
    /// Distance from the canvas bottom to the top of the last line's box;
    /// more lines grow upward from there.
    pub bottom: u32,
    /// Moved from that place by shares of the canvas width and height.
    pub offset: (f64, f64),
    /// Colour of the word being spoken, for titles with word timing.
    pub highlight: Option<Rgb>,
}

/// A shadow under a title's letters, in output-canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextShadow {
    /// Where it falls from the letters, right and down positive.
    pub x: f32,
    pub y: f32,
    /// The blur's standard deviation; below [`SHARP_SHADOW`] it is a plain copy.
    pub blur: f32,
    pub color: Rgb,
    pub opacity: f32,
}

/// A shadow blurred less than this, in pixels, is drawn sharp.
pub const SHARP_SHADOW: f32 = 0.5;

/// What one title draws and where, for [`TitleLook::filters`].
pub(crate) struct TitleText<'a> {
    /// drawtext's `fontfile` and `text`, each quoted by [`filter_value`].
    pub(crate) font: &'a str,
    pub(crate) text: &'a str,
    /// drawtext's `x` and `y`: the first line's left and top.
    pub(crate) x: &'a str,
    pub(crate) y: &'a str,
    /// drawtext's `enable`; `None` draws on every frame.
    pub(crate) enable: Option<&'a str>,
    /// The title's opacity, which fades its box, outline and shadow too.
    pub(crate) opacity: f32,
    /// Names this title's pads apart from every other filter's.
    pub(crate) tag: &'a str,
}

impl TitleLook {
    /// The look on a canvas `scale` times as large.
    pub fn scaled(self, scale: f64) -> Self {
        let px = |value: u32| (f64::from(value) * scale).round() as u32;
        let scale_f = scale as f32;
        Self {
            font_size: px(self.font_size).max(1),
            padding: px(self.padding),
            outline: self
                .outline
                .map(|(width, color, alpha)| (px(width).max(1), color, alpha)),
            shadow: self.shadow.map(|shadow| TextShadow {
                x: shadow.x * scale_f,
                y: shadow.y * scale_f,
                blur: shadow.blur * scale_f,
                ..shadow
            }),
            margin: px(self.margin),
            bottom: px(self.bottom),
            ..self
        }
    }

    /// The filters that draw `text` from pad `input` to pad `output`: its
    /// box, then its shadow, outline and letters, as drawtext orders them.
    /// drawtext clips a shadow to the box it draws and cannot blur, so a
    /// title with a shadow draws its box in a pass of its own, and a soft
    /// shadow is drawn alone on a clear copy of the frame, blurred and
    /// laid under the letters.
    pub(crate) fn filters(&self, text: &TitleText, input: &str, output: &str) -> Vec<String> {
        let alpha = text.opacity.clamp(0.0, 1.0);
        let enable = text
            .enable
            .map(|enable| format!(":enable='{enable}'"))
            .unwrap_or_default();
        let draw = |x: &str, y: &str, options: &str| {
            format!(
                "drawtext=fontfile={}:expansion=none:text={}:fontsize={}:{options}:\
                 x={x}:y={y}:{LINE_BOX}:text_align={}{enable}",
                text.font,
                text.text,
                self.font_size,
                self.align_option(),
            )
        };
        let letters = format!(
            "fontcolor={}@{alpha:.2}{}",
            ffmpeg_color(self.color),
            self.outline_options(alpha)
        );
        let (x, y, tag) = (text.x, text.y, text.tag);
        let Some(shadow) = self.shadow else {
            let options = format!("{letters}:{}", self.box_options(alpha));
            return vec![format!("[{input}]{}[{output}]", draw(x, y, &options))];
        };
        let mut filters = Vec::new();
        let mut current = input.to_owned();
        if self.text_box.is_some() {
            let options = format!("fontcolor=black@0:{}", self.box_options(alpha));
            filters.push(format!("[{current}]{}[{tag}box]", draw(x, y, &options)));
            current = format!("{tag}box");
        }
        let color = ffmpeg_color(shadow.color);
        let shade = format!("{color}@{:.3}", shadow.opacity * alpha);
        let (dx, dy) = (shadow.x.round() as i32, shadow.y.round() as i32);
        if shadow.blur < SHARP_SHADOW {
            let options = format!("{letters}:box=0:shadowcolor={shade}:shadowx={dx}:shadowy={dy}");
            filters.push(format!("[{current}]{}[{output}]", draw(x, y, &options)));
            return filters;
        }
        // The shadow's letters, outlined as the title's are, in its colour
        // on a clear canvas of that colour, so blurring the alpha alone
        // softens it. An outline's glyph covers the letter's too.
        let glyph = match self.outline {
            Some((width, ..)) if width > 0 => {
                format!("fontcolor={color}@0:borderw={width}:bordercolor={shade}")
            }
            _ => format!("fontcolor={shade}"),
        };
        let (sx, sy) = (format!("{x}{dx:+}"), format!("{y}{dy:+}"));
        filters.push(format!("[{current}]split[{tag}main][{tag}clear]"));
        filters.push(format!(
            "[{tag}clear]format=yuva420p,\
             drawbox=x=0:y=0:w=iw:h=ih:color={color}@0:t=fill:replace=1{enable},\
             {},gblur=sigma={:.2}:steps=3:planes=8{enable}[{tag}shadow]",
            draw(&sx, &sy, &format!("{glyph}:box=0")),
            shadow.blur,
        ));
        filters.push(format!(
            "[{tag}main][{tag}shadow]overlay=format=auto{enable},{}[{output}]",
            draw(x, y, &format!("{letters}:box=0"))
        ));
        filters
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

    /// The distance in pixels from one line to the next: the font's line
    /// box, which FreeType rounds to whole pixels.
    pub fn line_height(&self) -> u32 {
        (self.font_size as f32 * self.face.line_height()).round() as u32
    }

    /// From the canvas bottom to the top of the first of `lines` lines.
    pub fn top(&self, lines: usize) -> u32 {
        self.bottom + self.line_height() * lines.saturating_sub(1) as u32
    }

    /// drawtext's `x` for text `width` wide (an expression), placed as
    /// the look aligns it and moved by its offset.
    fn x(&self, width: &str) -> String {
        let x = match self.align {
            TextAlign::Left => self.margin.to_string(),
            TextAlign::Center => format!("(w-{width})/2"),
            TextAlign::Right => format!("w-{width}-{}", self.margin),
        };
        match self.offset.0 {
            0.0 => x,
            share => format!("{x}{share:+.5}*w"),
        }
    }

    /// drawtext's `y` for `lines` lines: the first line's top, moved by
    /// the look's offset.
    fn y(&self, lines: usize) -> String {
        let top = self.top(lines);
        match self.offset.1 {
            0.0 => format!("h-{top}"),
            share => format!("h-{top}{share:+.5}*h"),
        }
    }

    /// drawtext's `text_align`, which lines several lines up.
    pub(crate) fn align_option(&self) -> &'static str {
        match self.align {
            TextAlign::Left => "L",
            TextAlign::Center => "C",
            TextAlign::Right => "R",
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
            shadow: None,
            align: TextAlign::Left,
            margin: 42,
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
            shadow: None,
            align: TextAlign::Center,
            margin: 42,
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
            shadow: None,
            align: TextAlign::Center,
            margin: 42,
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
            shadow: None,
            align: TextAlign::Center,
            margin: 42,
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
            shadow: None,
            align: TextAlign::Center,
            margin: 42,
            bottom: 96,
            offset: (0.0, 0.0),
            highlight: Some(Rgb(255, 214, 0)),
        },
    }
}

/// `title`'s [`TextLook`] with every choice made: its own where it has
/// one, else its style's. A box, outline or shadow it does not draw is 0.
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
        shadow_opacity: Some(
            text.shadow_opacity
                .or(text.shadow_color.map(|_| DEFAULT_SHADOW_OPACITY))
                .unwrap_or(0.0),
        ),
        shadow_color: Some(text.shadow_color.unwrap_or(Rgb::BLACK)),
        font: Some(face.font()),
        bold: face.bold(),
        italic: face.italic(),
        align: Some(text.align.unwrap_or(style.align)),
        ..text
    }
}

/// The lines `title` draws: one per line break, each line's words one
/// space apart, blank lines left out. A highlight caption lights its words
/// along one line, so it draws them all on one.
pub fn title_lines(title: &Clip) -> Vec<String> {
    let words = |line: &str| line.split_whitespace().collect::<Vec<_>>().join(" ");
    if title_look(title.title_style).highlight.is_some() {
        return vec![words(&title.name)];
    }
    let lines: Vec<String> = title
        .name
        .lines()
        .map(words)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// How `title` is drawn: its style's look with the title's own size,
/// colours, box, outline, shadow and place laid over it ([`TextLook`]).
pub fn clip_title_look(title: &Clip) -> TitleLook {
    let style = title_look(title.title_style);
    let drawn = drawn_text(title);
    let font_size = (f64::from(style.font_size) * drawn.size).round().max(1.0) as u32;
    let box_opacity = drawn.box_opacity.unwrap_or(0.0);
    let outline = drawn.outline.unwrap_or(0.0);
    let shadow_opacity = drawn.shadow_opacity.unwrap_or(0.0);
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
        shadow: (shadow_opacity > 0.0).then(|| {
            let size = f64::from(font_size);
            let (sin, cos) = drawn.shadow_angle.to_radians().sin_cos();
            let distance = drawn.shadow_distance * size;
            TextShadow {
                x: (distance * cos) as f32,
                y: (distance * sin) as f32,
                blur: (drawn.shadow_blur * size) as f32,
                color: drawn.shadow_color.unwrap_or(Rgb::BLACK),
                opacity: shadow_opacity as f32,
            }
        }),
        bottom,
        align: drawn.align.unwrap_or(style.align),
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

    /// One 640x120 white frame with `text` options drawn on it by drawtext.
    fn drawn(font: &str, text: &str) -> Vec<u8> {
        let out = crate::cli::ffmpeg()
            .args(["-v", "error", "-f", "lavfi", "-i"])
            .arg("color=c=white:s=640x120:r=1:d=1")
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
        let typed = r"7:05 100% it's a\b, [x]; = %{n} \'
a second line";
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

//! Titles and captions: one table of looks shared with the program
//! monitor, and the drawtext graph export burns in.

use std::path::Path;
use std::process::Command;

use openreel_engine::{Clip, TitleStyle};

use super::video::{enabled_frames, VideoClock};

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

/// The font titles are drawn with, as a path.
pub(crate) fn font_file() -> Option<String> {
    if let Ok(out) = Command::new("fc-match")
        .args(["-f", "%{file}", "sans"])
        .output()
    {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if Path::new(&path).exists() {
            return Some(path);
        }
    }
    [
        "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "C:/Windows/Fonts/arial.ttf",
    ]
    .iter()
    .find(|p| Path::new(p).exists())
    .map(|p| (*p).to_owned())
}

/// Titles keep the whole-timeline clock: each segment draws the visible
/// frames that fall inside it, shifted to its own frame numbers.
pub(super) struct Titles<'a> {
    pub(super) font: Option<String>,
    /// The font file's bytes, to measure highlight captions.
    pub(super) font_data: Option<Vec<u8>>,
    pub(super) clock: VideoClock,
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
        if let Some(font) = &self.font {
            let font = filter_value(font);
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
                let look = title_look(clip.title_style);
                let (r, g, b) = look.color;
                let font_size = look.font_size;
                let font_color = format!("0x{r:02X}{g:02X}{b:02X}");
                let box_options = match look.box_alpha {
                    Some(alpha) => {
                        format!("box=1:boxcolor=black@{alpha:.2}:boxborderw={}", look.border)
                    }
                    None => format!("box=0:borderw={}:bordercolor=black@0.8", look.border),
                };
                let x = look
                    .left
                    .map_or_else(|| "(w-text_w)/2".to_owned(), |left| left.to_string());
                let y = format!("h-{}", look.bottom);
                let alpha = clip.opacity.clamp(0.0, 1.0);
                let words: Vec<&str> = clip.name.split_whitespace().collect();
                let highlight = look
                    .highlight
                    .zip(clip.word_times())
                    .filter(|_| !words.is_empty());
                // Every highlight layer starts at the same left edge, so
                // drawtext lays out each prefix exactly as in the full line.
                let x = match (&highlight, look.left, &self.font_data) {
                    (Some(_), None, Some(data)) => {
                        text_width(data, &words.join(" "), font_size as f32)
                            .map_or(x, |width| format!("(w-{width:.1})/2"))
                    }
                    _ => x,
                };
                filters.push(format!(
                    "[{current}]drawtext=fontfile={font}:expansion=none:text={}:\
                     fontcolor={font_color}@{alpha:.2}:\
                     fontsize={font_size}:{box_options}:x={x}:y={y}:\
                     enable='{}'[t{i}]",
                    filter_value(&words.join(" ")),
                    enabled_frames(&ranges),
                ));
                current = format!("t{i}");
                let Some(((hr, hg, hb), times)) = highlight else {
                    continue;
                };
                // Word n lights while it is spoken: the words through it in
                // the highlight colour, then the words before it back in the
                // text colour on top. No box: the base layer drew it.
                let outline = match look.box_alpha {
                    Some(_) => "box=0".to_owned(),
                    None => format!("box=0:borderw={}:bordercolor=black@0.8", look.border),
                };
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
                    let mut layers =
                        vec![(words[..=n].join(" "), format!("0x{hr:02X}{hg:02X}{hb:02X}"))];
                    if n > 0 {
                        layers.push((words[..n].join(" "), font_color.clone()));
                    }
                    for (layer, (text, color)) in layers.into_iter().enumerate() {
                        let text = filter_value(&text);
                        filters.push(format!(
                            "[{current}]drawtext=fontfile={font}:expansion=none:text={text}:\
                             fontcolor={color}@{alpha:.2}:fontsize={font_size}:{outline}:\
                             x={x}:y={y}:enable='{enable}'[t{i}w{n}l{layer}]"
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

/// How export draws one title style, in output-canvas pixels. The program
/// monitor reads the same table and scales it to its frame, so titles
/// preview at the size and place they export.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TitleLook {
    pub font_size: u32,
    pub color: (u8, u8, u8),
    /// Black box opacity behind the text; `None` draws a black outline.
    pub box_alpha: Option<f32>,
    /// Box padding, or outline width when there is no box.
    pub border: u32,
    /// Text left edge; `None` centers the text.
    pub left: Option<u32>,
    /// Distance from the canvas bottom to the text top.
    pub bottom: u32,
    /// Colour of the word being spoken, for titles with word timing.
    pub highlight: Option<(u8, u8, u8)>,
}

pub fn title_look(style: TitleStyle) -> TitleLook {
    let white = (255, 255, 255);
    match style {
        TitleStyle::LowerThird => TitleLook {
            font_size: 44,
            color: white,
            box_alpha: Some(0.55),
            border: 14,
            left: Some(42),
            bottom: 110,
            highlight: None,
        },
        TitleStyle::Caption => TitleLook {
            font_size: 38,
            color: white,
            box_alpha: Some(0.65),
            border: 12,
            left: None,
            bottom: 90,
            highlight: None,
        },
        TitleStyle::PopCaption => TitleLook {
            font_size: 48,
            color: (255, 255, 0),
            box_alpha: Some(0.75),
            border: 14,
            left: None,
            bottom: 100,
            highlight: None,
        },
        TitleStyle::MinimalCaption => TitleLook {
            font_size: 34,
            color: white,
            box_alpha: None,
            border: 2,
            left: None,
            bottom: 70,
            highlight: None,
        },
        TitleStyle::HighlightCaption => TitleLook {
            font_size: 44,
            color: white,
            box_alpha: Some(0.65),
            border: 12,
            left: None,
            bottom: 96,
            highlight: Some((255, 214, 0)),
        },
    }
}

#[cfg(test)]
mod title_text_tests {
    use super::super::argument_path;
    use super::{filter_value, font_file};
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
                "drawtext=fontfile={}:expansion=none:{text}:fontsize=24:x=10:y=20",
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
        // Without a font export draws no titles at all, so there is nothing
        // to compare: say so instead of passing.
        let font = font_file().expect("a font fc-match can find (titles are drawn with it)");
        // What the two parsers and drawtext's own expansion treat specially.
        let typed = r"7:05 100% it's a\b, [x]; = %{n} \'";
        let file = std::env::temp_dir().join(format!("eddie-title-{}.txt", std::process::id()));
        std::fs::write(&file, typed).unwrap();
        // drawtext reads a text file without any parsing: the oracle.
        let expected = drawn(
            &font,
            &format!("textfile={}", filter_value(&file.to_string_lossy())),
        );
        let _ = std::fs::remove_file(&file);
        let dark = expected
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|rgb| rgb[0] < 128)
            .count();
        assert!(dark > 500, "the text is drawn, dark pixels={dark}");
        assert!(
            drawn(&font, &format!("text={}", filter_value(typed))) == expected,
            "the quoted text draws the same picture as the file"
        );
    }
}

//! The fonts titles are drawn in, built into Eddie: `fonts/`, each family
//! under the SIL Open Font License (`fonts/licenses`). The window registers
//! [`FILES`] with Rune; export and contact sheets write the faces they draw
//! into their private scratch folder, since FFmpeg's drawtext reads a file.
//! Both lay a line out in the font's own line box ([`Face::line_height`]),
//! so a title sits where it previews on every computer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use openreel_engine::TitleFont;

use crate::MediaResult;

macro_rules! files {
    ($($name:literal),* $(,)?) => {
        const NAMES: &[&str] = &[$($name),*];
        /// Every face built in, for a front end to register: each title
        /// face, and Inter Medium and SemiBold, which the window's own text
        /// uses.
        pub static FILES: &[&[u8]] = &[$(include_bytes!(concat!("fonts/", $name))),*];
    };
}

files!(
    "Inter-Regular.ttf",
    "Inter-Medium.ttf",
    "Inter-SemiBold.ttf",
    "Inter-Bold.ttf",
    "Inter-Italic.ttf",
    "Inter-BoldItalic.ttf",
    "Lora-Regular.ttf",
    "Lora-Bold.ttf",
    "Lora-Italic.ttf",
    "Lora-BoldItalic.ttf",
    "PlayfairDisplay-Regular.ttf",
    "PlayfairDisplay-Bold.ttf",
    "PlayfairDisplay-Italic.ttf",
    "PlayfairDisplay-BoldItalic.ttf",
    "Oswald-Regular.ttf",
    "Oswald-Bold.ttf",
    "BebasNeue-Regular.ttf",
    "Caveat-Regular.ttf",
    "Caveat-Bold.ttf",
    "Pacifico-Regular.ttf",
    "RobotoMono-Regular.ttf",
    "RobotoMono-Bold.ttf",
    "RobotoMono-Italic.ttf",
    "RobotoMono-BoldItalic.ttf",
);

/// One face of a [`TitleFont`]. Bold and italic hold only where the
/// family has those faces, so what is asked for and what draws agree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Face {
    font: TitleFont,
    bold: bool,
    italic: bool,
}

impl Face {
    pub fn new(font: TitleFont, bold: bool, italic: bool) -> Self {
        Self {
            font,
            bold: bold && font.has_bold(),
            italic: italic && font.has_italic(),
        }
    }

    pub fn font(self) -> TitleFont {
        self.font
    }

    pub fn bold(self) -> bool {
        self.bold
    }

    pub fn italic(self) -> bool {
        self.italic
    }

    /// The weight that picks this face from its family.
    pub fn weight(self) -> f32 {
        if self.bold {
            700.0
        } else {
            400.0
        }
    }

    /// The face's file in `fonts/`.
    fn name(self) -> &'static str {
        use TitleFont::*;
        match (self.font, self.bold, self.italic) {
            (Inter, false, false) => "Inter-Regular.ttf",
            (Inter, true, false) => "Inter-Bold.ttf",
            (Inter, false, true) => "Inter-Italic.ttf",
            (Inter, true, true) => "Inter-BoldItalic.ttf",
            (Lora, false, false) => "Lora-Regular.ttf",
            (Lora, true, false) => "Lora-Bold.ttf",
            (Lora, false, true) => "Lora-Italic.ttf",
            (Lora, true, true) => "Lora-BoldItalic.ttf",
            (PlayfairDisplay, false, false) => "PlayfairDisplay-Regular.ttf",
            (PlayfairDisplay, true, false) => "PlayfairDisplay-Bold.ttf",
            (PlayfairDisplay, false, true) => "PlayfairDisplay-Italic.ttf",
            (PlayfairDisplay, true, true) => "PlayfairDisplay-BoldItalic.ttf",
            (Oswald, false, _) => "Oswald-Regular.ttf",
            (Oswald, true, _) => "Oswald-Bold.ttf",
            (BebasNeue, ..) => "BebasNeue-Regular.ttf",
            (Caveat, false, _) => "Caveat-Regular.ttf",
            (Caveat, true, _) => "Caveat-Bold.ttf",
            (Pacifico, ..) => "Pacifico-Regular.ttf",
            (RobotoMono, false, false) => "RobotoMono-Regular.ttf",
            (RobotoMono, true, false) => "RobotoMono-Bold.ttf",
            (RobotoMono, false, true) => "RobotoMono-Italic.ttf",
            (RobotoMono, true, true) => "RobotoMono-BoldItalic.ttf",
        }
    }

    /// The face's font file.
    pub fn data(self) -> &'static [u8] {
        let name = self.name();
        let index = NAMES
            .iter()
            .position(|file| *file == name)
            .expect("every face is built in");
        FILES[index]
    }

    /// The line box in ems: ascender less descender. FreeType (drawtext
    /// with `y_align=font`) and Rune both lay a line out in it, the first
    /// baseline an ascender down; none of these fonts has a line gap, and
    /// each has the same values in `hhea` and `OS/2`, so both read the same.
    pub fn line_height(self) -> f32 {
        use ab_glyph::{Font, FontRef};
        let font = FontRef::try_from_slice(self.data()).expect("a built-in font parses");
        (font.ascent_unscaled() - font.descent_unscaled()) / font.units_per_em().unwrap_or(1000.0)
    }

    /// Write the face into `dir`, once, for drawtext's `fontfile`.
    pub(crate) fn write_into(self, dir: &Path) -> MediaResult<PathBuf> {
        let path = dir.join(self.name());
        if !path.exists() {
            std::fs::write(&path, self.data())
                .map_err(|error| format!("write the font {}: {error}", self.name()))?;
        }
        Ok(path)
    }
}

/// Write each of `faces` into `dir` once, for drawtext.
pub(crate) fn write_faces(
    faces: impl IntoIterator<Item = Face>,
    dir: &Path,
) -> MediaResult<HashMap<Face, PathBuf>> {
    let mut files = HashMap::new();
    for face in faces {
        if let std::collections::hash_map::Entry::Vacant(entry) = files.entry(face) {
            entry.insert(face.write_into(dir)?);
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each face has the weight and slant that pick it from its family, so
    /// Rune picks the file drawtext is given; a family's metrics agree in `hhea` and
    /// `OS/2`, with no line gap, or the two would place lines apart.
    #[test]
    fn every_face_is_the_file_its_name_and_weight_pick() {
        use ab_glyph::{Font, FontRef};
        assert_eq!(FILES.len(), NAMES.len());
        for font in TitleFont::ALL {
            for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
                let face = Face::new(font, bold, italic);
                assert_eq!(face.bold, bold && font.has_bold(), "{font:?}");
                assert_eq!(face.italic, italic && font.has_italic(), "{font:?}");
                let data = face.data();
                let parsed = FontRef::try_from_slice(data).unwrap();
                let table = |tag: &[u8; 4]| {
                    // The table directory: tag, checksum, offset, length.
                    let count = u16::from_be_bytes([data[4], data[5]]) as usize;
                    (0..count)
                        .map(|i| &data[12 + 16 * i..28 + 16 * i])
                        .find(|entry| &entry[..4] == tag)
                        .map(|entry| {
                            let offset = u32::from_be_bytes(entry[8..12].try_into().unwrap());
                            &data[offset as usize..]
                        })
                        .unwrap()
                };
                let short =
                    |table: &[u8], at: usize| i16::from_be_bytes([table[at], table[at + 1]]);
                let (os2, hhea) = (table(b"OS/2"), table(b"hhea"));
                let weight = short(os2, 4);
                assert_eq!(f32::from(weight), face.weight(), "{}", face.name());
                // fsSelection bit 0: italic.
                assert_eq!(short(os2, 62) & 1 == 1, face.italic, "{}", face.name());
                assert_eq!(
                    (short(hhea, 4), short(hhea, 6), short(hhea, 8)),
                    (short(os2, 68), short(os2, 70), 0),
                    "{}: hhea against OS/2 typo metrics",
                    face.name()
                );
                assert_eq!(parsed.ascent_unscaled(), f32::from(short(hhea, 4)));
            }
        }
    }
}

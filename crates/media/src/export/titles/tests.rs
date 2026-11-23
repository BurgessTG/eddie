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

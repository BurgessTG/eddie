use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use super::{first_drawing_text, places};

/// An empty folder of this test's own, with links resolved as `places`
/// resolves them (the system temp folder is a link on macOS).
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("eddie-cli-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// A stand-in `ffmpeg` that lists a drawtext filter, as one that can draw
/// titles does.
fn tool_that_draws_text(path: &Path) {
    fs::write(path, "#!/bin/sh\necho ' T. drawtext V->V Draw text'\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn the_ffmpeg_beside_the_program_comes_before_any_other() {
    let dir = folder("beside");
    let app = dir.join("Eddie.app/Contents/MacOS");
    fs::create_dir_all(&app).unwrap();
    fs::write(app.join("eddie"), "").unwrap();
    tool_that_draws_text(&app.join("ffmpeg"));
    let shipped = OsString::from(app.join("ffmpeg"));

    let found = places(Some(&app.join("eddie")));
    assert_eq!(found[0], shipped);
    assert_eq!(first_drawing_text(&found), shipped);

    // `eddie` on PATH as a link into the app still finds the app's own.
    fs::create_dir_all(dir.join("bin")).unwrap();
    symlink(app.join("eddie"), dir.join("bin/eddie")).unwrap();
    assert_eq!(places(Some(&dir.join("bin/eddie")))[0], shipped);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_program_with_no_ffmpeg_beside_it_starts_at_path() {
    let dir = folder("alone");
    fs::write(dir.join("eddie"), "").unwrap();
    assert_eq!(
        places(Some(&dir.join("eddie")))[0],
        OsString::from("ffmpeg")
    );
    assert_eq!(places(None)[0], OsString::from("ffmpeg"));
    fs::remove_dir_all(&dir).unwrap();
}

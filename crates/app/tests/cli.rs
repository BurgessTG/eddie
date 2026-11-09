//! The `eddie` command line end to end: real binary, real media, a project
//! file on disk. Needs `ffmpeg` and `ffprobe` on PATH, like the editor.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("eddie-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        Self(dir)
    }

    fn ffmpeg(&self, args: &[&str]) {
        let status = Command::new("ffmpeg")
            .current_dir(&self.0)
            .args(["-v", "error", "-y"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Run eddie from `cwd` (relative to the dir), returning exit code and JSON.
    fn eddie_in(&self, cwd: &str, args: &[&str]) -> (i32, Value) {
        let out = Command::new(env!("CARGO_BIN_EXE_eddie"))
            .current_dir(self.0.join(cwd))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("HOME", &self.0)
            .args(args)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let value = serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
            panic!(
                "{args:?}: {error}\nstdout: {stdout}\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code().unwrap(), value)
    }

    fn eddie(&self, args: &[&str]) -> (i32, Value) {
        self.eddie_in(".", args)
    }

    fn project(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.0.join("p.eddie")).unwrap()).unwrap()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn duration(path: &Path) -> f64 {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

#[test]
fn a_project_is_built_edited_and_exported_from_the_command_line() {
    let dir = Dir::new("edit");
    std::fs::create_dir(dir.0.join("media")).unwrap();
    std::fs::create_dir(dir.0.join("elsewhere")).unwrap();
    dir.ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=320x180:r=30:d=4",
        "-f",
        "lavfi",
        "-i",
        "sine=f=440:d=4",
        "-shortest",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "media/talk.mp4",
    ]);

    // Relative media paths are stored absolute; a missing file is reported.
    let (code, made) = dir.eddie(&["new", "p.eddie", "media/talk.mp4", "media/nope.mp4"]);
    assert_eq!(code, 0, "{made}");
    assert_eq!(made["media"]["failed"].as_array().unwrap().len(), 1);
    let talk = dir.0.join("media/talk.mp4").to_string_lossy().into_owned();
    assert_eq!(dir.project()["media"], serde_json::json!([talk]));

    // One edit from another folder: a handle from an add feeds a later call.
    let (code, run) = dir.eddie_in(
        "elsewhere",
        &[
            "run",
            "../p.eddie",
            "-e",
            "const h = addAssetRange('talk.mp4', 1, 0.5, 2); setFade(h, 0.25, 0); \
             console.log('placed'); return state().clips.length",
        ],
    );
    assert_eq!(code, 0, "{run}");
    assert_eq!(run["saved"], true);
    // Queries read the request-time snapshot, before the edit applied.
    assert_eq!(run["output"], "0");
    assert_eq!(run["notes"], serde_json::json!(["placed"]));
    let clip_id = run["handles"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    let clips = dir.project()["timeline"]["clips"].clone();
    assert_eq!(clips.as_array().unwrap().len(), 1);
    assert_eq!(clips[0]["id"], clip_id);
    assert_eq!(
        (
            &clips[0]["start"],
            &clips[0]["source_in"],
            &clips[0]["duration"]
        ),
        (&1.0.into(), &0.5.into(), &2.0.into())
    );
    assert_eq!(clips[0]["fade_in"], 0.25);
    // The empty second before the clip is black with no title.
    assert!(run["introducedFindings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["kind"] == "picture-gap"));

    // A failing command undoes the commands before it, and nothing saves.
    let saved = std::fs::read(dir.0.join("p.eddie")).unwrap();
    let (code, failed) = dir.eddie(&[
        "run",
        "p.eddie",
        "-e",
        "addAssetRange('talk.mp4', 5, 0, 1); deleteClip(99999)",
    ]);
    assert_eq!((code, &failed["ok"]), (1, &false.into()), "{failed}");
    assert_eq!(std::fs::read(dir.0.join("p.eddie")).unwrap(), saved);

    // A dry run reports the change without saving it; files go through
    // their own commands.
    let (code, dry) = dir.eddie(&["run", "p.eddie", "--dry-run", "-e", "clearTimeline()"]);
    assert_eq!(
        (code, &dry["changed"], &dry["saved"]),
        (0, &true.into(), &false.into())
    );
    let (code, _) = dir.eddie(&["run", "p.eddie", "-e", "exportTo('/tmp/x.mp4')"]);
    assert_eq!(code, 1);
    assert_eq!(std::fs::read(dir.0.join("p.eddie")).unwrap(), saved);

    let (code, state) = dir.eddie(&["state", "p.eddie"]);
    assert_eq!(code, 0);
    assert_eq!(state["clips"][0]["id"], clip_id);

    // The export holds the black lead-in and the 2 s clip.
    let (code, exported) = dir.eddie(&["export", "p.eddie", "out.mp4"]);
    assert_eq!(code, 0, "{exported}");
    let length = duration(&dir.0.join("out.mp4"));
    assert!((length - 3.0).abs() < 0.1, "exported {length} s");
}

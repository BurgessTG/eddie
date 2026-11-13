//! The `eddie` command line end to end: real binary, real media, a project
//! file on disk. Needs `ffmpeg` on PATH, like the editor, and `ffprobe`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

use serde_json::Value;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        // macOS keeps its temp folder under /var, a link to /private/var,
        // and so deep that a socket's name no longer fits. /private/tmp is
        // short and has no link in it.
        let temp = if cfg!(target_os = "macos") {
            PathBuf::from("/private/tmp")
        } else {
            std::env::temp_dir()
        };
        let dir = temp.join(format!("eddie-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        Self(dir)
    }

    /// The settings folder eddie uses with this dir as its home, and the
    /// one it had as OpenReel, where each system keeps them.
    fn settings(&self) -> (PathBuf, PathBuf) {
        if cfg!(target_os = "macos") {
            let home = self.0.join("Library/Application Support");
            (home.join("Eddie"), home.join("OpenReel"))
        } else {
            let home = self.0.join("config");
            (home.join("eddie"), home.join("openreel"))
        }
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

    /// eddie with this dir as its home, so it finds no settings, saved
    /// curation or running editor of the user's.
    fn command(&self, cwd: &str, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_eddie"));
        command
            .current_dir(self.0.join(cwd))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_RUNTIME_DIR", self.0.join("run"))
            .env("HOME", &self.0)
            .env_remove("EDDIE_WHISPER_DIR")
            .env_remove("EDDIE_WHISPER_MODEL")
            .args(args);
        command
    }

    /// Run eddie from `cwd` (relative to the dir), returning exit code and JSON.
    fn eddie_in(&self, cwd: &str, args: &[&str]) -> (i32, Value) {
        let out = self.command(cwd, args).output().unwrap();
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

    /// Start a command that keeps running and prints JSON lines.
    fn spawn(&self, args: &[&str]) -> Running {
        let mut child = self
            .command(".", args)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap());
        Running { child, lines }
    }
}

/// `eddie serve` or `eddie events`, killed if a test fails before it ends.
struct Running {
    child: Child,
    lines: BufReader<ChildStdout>,
}

impl Running {
    /// The next line it prints; null once it has closed its output.
    fn line(&mut self) -> Value {
        let mut line = String::new();
        self.lines.read_line(&mut line).unwrap();
        if line.is_empty() {
            return Value::Null;
        }
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("{error}: {line}"))
    }

    /// Skip to the next line with this `event`.
    fn event(&mut self, name: &str) -> Value {
        loop {
            let line = self.line();
            assert!(!line.is_null(), "closed before a {name} event");
            if line["event"] == name {
                return line;
            }
        }
    }

    fn exit_code(&mut self) -> i32 {
        self.child.wait().unwrap().code().unwrap()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    let failed = made["media"]["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1, "{made}");
    assert!(
        failed[0]["path"]
            .as_str()
            .unwrap()
            .ends_with("media/nope.mp4"),
        "{made}"
    );
    assert!(
        failed[0]["error"]
            .as_str()
            .unwrap()
            .contains("No such file"),
        "{made}"
    );
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
    assert!(
        run["introducedFindings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["kind"] == "picture-gap")
    );

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
    // It shows the timeline the edit would leave: here, an empty one.
    assert_eq!(dry["clips"], serde_json::json!([]), "{dry}");
    let (code, _) = dir.eddie(&["run", "p.eddie", "-e", "exportTo('/tmp/x.mp4')"]);
    assert_eq!(code, 1);
    assert_eq!(std::fs::read(dir.0.join("p.eddie")).unwrap(), saved);

    let (code, state) = dir.eddie(&["state", "p.eddie"]);
    assert_eq!(code, 0);
    assert_eq!(state["clips"][0]["id"], clip_id);

    // A script can come from a file; one that cannot be read is an error.
    std::fs::write(dir.0.join("count.js"), "return state().clips.length").unwrap();
    let (code, counted) = dir.eddie(&["run", "p.eddie", "count.js"]);
    assert_eq!((code, &counted["output"]), (0, &"1".into()), "{counted}");
    assert_eq!(
        counted["saved"], false,
        "a script that changes nothing saves nothing"
    );
    let (code, unread) = dir.eddie(&["run", "p.eddie", "absent.js"]);
    assert_eq!(code, 1, "{unread}");
    assert!(
        unread["error"].as_str().unwrap().contains("absent.js"),
        "{unread}"
    );

    // inspect measures the saved timeline: the same black second.
    let (code, inspected) = dir.eddie(&["inspect", "p.eddie"]);
    assert_eq!(code, 0, "{inspected}");
    assert!(
        inspected["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["kind"] == "picture-gap")
    );
    assert_eq!(std::fs::read(dir.0.join("p.eddie")).unwrap(), saved);

    // The export holds the black lead-in and the 2 s clip.
    let (code, exported) = dir.eddie(&["export", "p.eddie", "out.mp4"]);
    assert_eq!(code, 0, "{exported}");
    let length = duration(&dir.0.join("out.mp4"));
    assert!((length - 3.0).abs() < 0.1, "exported {length} s");

    // An export never replaces the project or the media it is made from.
    let source = std::fs::read(&talk).unwrap();
    for target in ["media/talk.mp4", "p.eddie"] {
        let (code, refused) = dir.eddie(&["export", "p.eddie", target]);
        assert_eq!(code, 1, "{refused}");
        assert!(
            refused["error"]
                .as_str()
                .unwrap()
                .contains("cannot replace"),
            "{refused}"
        );
    }
    assert!(
        std::fs::read(&talk).unwrap() == source,
        "media is untouched"
    );
    assert_eq!(std::fs::read(dir.0.join("p.eddie")).unwrap(), saved);

    // import adds media to the saved project and leaves the timeline alone.
    std::fs::copy(&talk, dir.0.join("media/more.mp4")).unwrap();
    let (code, imported) = dir.eddie(&["import", "p.eddie", "media/more.mp4"]);
    assert_eq!(code, 0, "{imported}");
    assert_eq!(
        (&imported["saved"], imported.get("live")),
        (&true.into(), None)
    );
    let project = dir.project();
    let more = dir.0.join("media/more.mp4").to_string_lossy().into_owned();
    assert_eq!(project["media"], serde_json::json!([talk, more]));
    assert_eq!(project["timeline"]["clips"], clips);
    let (code, nothing) = dir.eddie(&["import", "p.eddie"]);
    assert_eq!((code, &nothing["ok"]), (1, &false.into()), "{nothing}");

    // A raw stream has no timestamps to place or seek by: it is refused
    // with the remedy instead of becoming media nothing can use.
    dir.ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red:s=320x180:r=30:d=1",
        "-c:v",
        "libx264",
        "-f",
        "h264",
        "media/raw.h264",
    ]);
    let (_, raw) = dir.eddie(&["import", "p.eddie", "media/raw.h264"]);
    assert_eq!(raw["media"]["imported"], serde_json::json!([]), "{raw}");
    assert!(
        raw["media"]["failed"][0]["error"]
            .as_str()
            .unwrap()
            .contains("container"),
        "{raw}"
    );
    assert_eq!(dir.project()["media"], serde_json::json!([talk, more]));
}

#[test]
fn help_lists_the_commands_and_a_mistyped_one_is_an_error() {
    let dir = Dir::new("help");
    // Settings saved under the editor's old name move on the first start.
    let (settings, old) = dir.settings();
    std::fs::create_dir_all(old.join("whisper")).unwrap();
    std::fs::write(old.join("config.json"), "{}").unwrap();
    for flag in ["help", "--help", "-h"] {
        let out = dir.command(".", &[flag]).output().unwrap();
        assert_eq!(out.status.code(), Some(0));
        let text = String::from_utf8_lossy(&out.stdout);
        for command in ["new", "run", "sheet", "serve", "events"] {
            assert!(text.contains(&format!("\n  {command} ")), "{command}");
        }
        assert!(text.contains("addAssetRange("), "the script API follows");
    }
    assert!(settings.join("config.json").exists());
    assert!(settings.join("whisper").is_dir());
    assert!(!old.exists());
    let out = dir.command(".", &["guide"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let guide = String::from_utf8_lossy(&out.stdout);
    assert!(guide.starts_with("# Editing with Eddie from an agent"));
    assert!(guide.contains("## Rules that prevent the usual mistakes"));
    for flag in ["--version", "-V"] {
        let out = dir.command(".", &[flag]).output().unwrap();
        assert_eq!(out.status.code(), Some(0));
        let version = String::from_utf8_lossy(&out.stdout);
        let expected = concat!("eddie ", env!("CARGO_PKG_VERSION"), " (");
        assert!(version.starts_with(expected), "{version}");
    }

    // A typo must answer in JSON, never open the window: without a display
    // a regression fails here instead of hanging.
    let mut typo = dir.command(".", &["statee", "p.eddie"]);
    typo.env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY");
    let out = typo.output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["ok"], false);
    assert!(
        reply["error"]
            .as_str()
            .unwrap()
            .contains("unknown command 'statee'"),
        "{reply}"
    );
}

/// The colour of one pixel of a picture file.
fn pixel(path: &Path, x: u32, y: u32) -> [u8; 3] {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vf", &format!("format=rgb24,crop=1:1:{x}:{y}")])
        .args(["-frames:v", "1"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .unwrap();
    assert_eq!(out.stdout.len(), 3, "{}", path.display());
    [out.stdout[0], out.stdout[1], out.stdout[2]]
}

fn is_red(rgb: [u8; 3]) -> bool {
    rgb[0] > 150 && rgb[2] < 100
}

fn is_blue(rgb: [u8; 3]) -> bool {
    rgb[2] > 150 && rgb[0] < 100
}

/// An agent that cannot watch video sees the program, a whole source and
/// exact frames: 3 s of red then 3 s of blue, with the blue part placed.
#[test]
fn pictures_show_the_program_a_source_and_exact_frames() {
    let dir = Dir::new("pictures");
    dir.ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red:s=320x180:r=30:d=3",
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=320x180:r=30:d=3",
        "-filter_complex",
        "concat=n=2:v=1:a=0",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "shots.mp4",
    ]);
    let (code, _) = dir.eddie(&["new", "p.eddie", "shots.mp4"]);
    assert_eq!(code, 0);
    let (code, run) = dir.eddie(&[
        "run",
        "p.eddie",
        "-e",
        "addAssetRange('shots.mp4', 0, 3.5, 2); addTitle('Hello', 0, 1)",
    ]);
    assert_eq!(code, 0, "{run}");
    let clip = run["handles"].as_object().unwrap().values().next().unwrap();

    // The sheet: one shot in two cells, the titled second and the bare
    // one, both showing the placed (blue) source time.
    let (code, sheet) = dir.eddie(&["sheet", "p.eddie", "sheet.jpg"]);
    assert_eq!(code, 0, "{sheet}");
    assert_eq!(
        (&sheet["shots"], &sheet["sampled"]),
        (&1.into(), &false.into())
    );
    let cells = sheet["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 2, "{sheet}");
    assert_eq!(
        (
            &cells[0]["time"],
            &cells[0]["clip"],
            &cells[0]["sourceTime"]
        ),
        (&0.5.into(), clip, &4.0.into())
    );
    assert!(cells[0]["title"].is_number() && cells[1]["title"].is_null());
    let file = dir.0.join("sheet.jpg");
    // 384 px cells behind 4 px gutters.
    assert!(
        is_blue(pixel(&file, 196, 44)),
        "{:?}",
        pixel(&file, 196, 44)
    );
    assert!(
        is_blue(pixel(&file, 584, 44)),
        "{:?}",
        pixel(&file, 584, 44)
    );
    let (code, part) = dir.eddie(&["sheet", "p.eddie", "part.jpg", "1..2"]);
    assert_eq!(code, 0, "{part}");
    assert_eq!(part["cells"].as_array().unwrap().len(), 1);
    assert_eq!(part["cells"][0]["time"], 1.5);

    // The strip finds the source's two shots and marks what is placed.
    let (code, strip) = dir.eddie(&["strip", "p.eddie", "shots.mp4", "strip.jpg"]);
    assert_eq!(code, 0, "{strip}");
    assert_eq!(strip["shots"], 2, "{strip}");
    assert_eq!(strip["placed"], serde_json::json!([[3.5, 5.5]]));
    let pictures = strip["pictures"].as_array().unwrap();
    assert_eq!(pictures.len(), 16);
    for picture in pictures {
        let time = picture["time"].as_f64().unwrap();
        assert_eq!(picture["placed"], (3.5..5.5).contains(&time), "{picture}");
        let cut = picture["shot"][if time < 3.0 { 1 } else { 0 }]
            .as_f64()
            .unwrap();
        assert!((cut - 3.0).abs() < 0.05, "{picture}");
    }
    // 192 px cells, eight per row: the first row is red, the second blue.
    let file = dir.0.join("strip.jpg");
    assert!(is_red(pixel(&file, 100, 50)), "{:?}", pixel(&file, 100, 50));
    assert!(
        is_blue(pixel(&file, 100, 190)),
        "{:?}",
        pixel(&file, 100, 190)
    );

    // Frames carry their real source times: 2.9 s is red, 3.0 s is blue.
    let (code, frames) = dir.eddie(&[
        "frames",
        "p.eddie",
        "shots.mp4",
        "2.9..3.1",
        "frames",
        "--every",
        "0.1",
    ]);
    assert_eq!(code, 0, "{frames}");
    assert_eq!(
        (&frames["start"], &frames["end"]),
        (&2.9.into(), &3.1.into())
    );
    let listed = frames["frames"].as_array().unwrap();
    assert_eq!(listed.len(), 2, "{frames}");
    let at = |index: usize| listed[index]["time"].as_f64().unwrap();
    assert!(
        (at(0) - 2.9).abs() < 0.001 && (at(1) - 3.0).abs() < 0.001,
        "{frames}"
    );
    let file = |index: usize| PathBuf::from(listed[index]["file"].as_str().unwrap());
    assert!(file(0).starts_with(dir.0.join("frames")));
    assert!(is_red(pixel(&file(0), 100, 50)));
    assert!(is_blue(pixel(&file(1), 100, 50)));

    // A clip names the source range it shows; too many pictures is an error.
    let (code, shown) = dir.eddie(&[
        "frames",
        "p.eddie",
        "shots.mp4",
        &format!("clip:{clip}"),
        "shown",
    ]);
    assert_eq!(code, 0, "{shown}");
    let listed = shown["frames"].as_array().unwrap();
    assert_eq!(listed.len(), 16, "{shown}");
    assert_eq!(listed[0]["time"], 3.5);
    assert!(listed[15]["time"].as_f64().unwrap() < 5.5);
    let (code, many) = dir.eddie(&[
        "frames",
        "p.eddie",
        "shots.mp4",
        "0..6",
        "many",
        "--every",
        "0.1",
    ]);
    assert_eq!(code, 1);
    assert!(
        many["error"].as_str().unwrap().contains("at most 40"),
        "{many}"
    );
    let (code, wide) = dir.eddie(&[
        "frames",
        "p.eddie",
        "shots.mp4",
        "1",
        "wide",
        "--width",
        "4000",
    ]);
    assert_eq!(code, 1);
    assert!(
        wide["error"].as_str().unwrap().contains("64 to 1280"),
        "{wide}"
    );
    let (code, late) = dir.eddie(&["frames", "p.eddie", "shots.mp4", "9", "late"]);
    assert_eq!(code, 1);
    assert!(
        late["error"]
            .as_str()
            .unwrap()
            .contains("outside the source"),
        "{late}"
    );
    assert!(!dir.0.join("late").exists());
    let (code, sound) = dir.eddie(&["strip", "p.eddie", "nope.mp4", "x.jpg"]);
    assert_eq!((code, &sound["ok"]), (1, &false.into()));

    // A picture never replaces the project or its media: `sheet` called
    // with `strip`'s arguments once turned the source into a JPEG.
    let media = std::fs::read(dir.0.join("shots.mp4")).unwrap();
    let project = std::fs::read(dir.0.join("p.eddie")).unwrap();
    for arguments in [
        &["sheet", "p.eddie", "shots.mp4"][..],
        &["sheet", "p.eddie", "p.eddie"],
        &["strip", "p.eddie", "shots.mp4", "shots.mp4"],
        &["sheet", "p.eddie", "sheet.png"],
    ] {
        let (code, refused) = dir.eddie(arguments);
        assert_eq!(
            (code, &refused["ok"]),
            (1, &false.into()),
            "{arguments:?}: {refused}"
        );
        assert!(
            refused["error"].as_str().unwrap().contains(".jpg"),
            "{refused}"
        );
    }
    assert!(!dir.0.join("sheet.png").exists());
    // A .jpg name that is the media, through a link, is refused too.
    std::os::unix::fs::symlink(dir.0.join("shots.mp4"), dir.0.join("link.jpg")).unwrap();
    let (code, refused) = dir.eddie(&["sheet", "p.eddie", "link.jpg"]);
    assert_eq!(code, 1, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("cannot replace shots.mp4"),
        "{refused}"
    );
    assert_eq!(std::fs::read(dir.0.join("p.eddie")).unwrap(), project);
    assert_eq!(std::fs::read(dir.0.join("shots.mp4")).unwrap(), media);
}

/// An OpenAI-style transcription server: answers each upload with two
/// segments and word timings, and records every request it receives.
fn fake_transcriber() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            seen.lock()
                .unwrap()
                .push(head + &String::from_utf8_lossy(&body));
            let reply = serde_json::json!({
                "text": "Hello there. Second line.",
                "duration": 3.0,
                "segments": [
                    {"start": 0.2, "end": 1.4, "text": " Hello there."},
                    {"start": 1.6, "end": 2.8, "text": " Second line."}
                ],
                "words": [
                    {"word": "Hello", "start": 0.2, "end": 0.6},
                    {"word": "there.", "start": 0.7, "end": 1.4},
                    {"word": "Second", "start": 1.6, "end": 2.1},
                    {"word": "line.", "start": 2.2, "end": 2.8}
                ]
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        }
    });
    (base, requests)
}

#[test]
fn transcripts_come_from_the_chosen_provider_once_and_reach_scripts() {
    let dir = Dir::new("transcribe");
    dir.ffmpeg(&["-f", "lavfi", "-i", "sine=f=300:d=3", "voice.wav"]);
    let (base, requests) = fake_transcriber();
    // A loopback endpoint needs no key.
    let (settings, _) = dir.settings();
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::write(
        settings.join("config.json"),
        serde_json::json!({"transcription": {"provider": "openai", "base_url": base}}).to_string(),
    )
    .unwrap();
    let (code, _) = dir.eddie(&["new", "p.eddie", "voice.wav"]);
    assert_eq!(code, 0);

    let (code, first) = dir.eddie(&["transcribe", "p.eddie", "--language", "en"]);
    assert_eq!(code, 0, "{first}");
    let entry = &first["assets"][0];
    assert_eq!(
        (&entry["turns"], &entry["words"], &entry["reused"]),
        (&2.into(), &4.into(), &false.into()),
        "{first}"
    );
    assert_eq!(entry["source"], "openai:whisper-1");
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(
            request.starts_with("POST /v1/audio/transcriptions "),
            "{request}"
        );
        for field in [
            "name=\"model\"\r\n\r\nwhisper-1",
            "name=\"response_format\"\r\n\r\nverbose_json",
            "name=\"timestamp_granularities[]\"\r\n\r\nword",
            "name=\"timestamp_granularities[]\"\r\n\r\nsegment",
            "name=\"language\"\r\n\r\nen",
            "filename=\"audio.wav\"",
        ] {
            assert!(request.contains(field), "missing {field}");
        }
    }

    // Saved per file: the second run uploads nothing.
    let (code, again) = dir.eddie(&["transcribe", "p.eddie"]);
    assert_eq!(
        (code, &again["assets"][0]["reused"]),
        (0, &true.into()),
        "{again}"
    );
    assert_eq!(requests.lock().unwrap().len(), 1);

    // Scripts see the saved transcript with word timing after a reopen.
    let (code, run) = dir.eddie(&[
        "run",
        "p.eddie",
        "-e",
        "const s = transcriptSegments(); \
         return s.length + ' ' + s[1].timingPrecision + ' ' + s[1].words[0].text + ' ' + s[1].words[0].start",
    ]);
    assert_eq!(code, 0, "{run}");
    assert_eq!(run["output"], "2 word Second 1.6");

    // --force transcribes again; the model and vocabulary named for this
    // run reach the provider, and no language is sent when none is named.
    let (code, forced) = dir.eddie(&[
        "transcribe",
        "p.eddie",
        "--model",
        "tiny-test",
        "--vocabulary",
        "Eddie, Wonder",
        "--force",
    ]);
    assert_eq!(code, 0, "{forced}");
    assert_eq!(
        (
            &forced["assets"][0]["reused"],
            &forced["assets"][0]["source"]
        ),
        (&false.into(), &"openai:tiny-test".into()),
        "{forced}"
    );
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for field in [
            "name=\"model\"\r\n\r\ntiny-test",
            "name=\"prompt\"\r\n\r\nEddie, Wonder",
        ] {
            assert!(requests[1].contains(field), "missing {field}");
        }
        assert!(!requests[1].contains("name=\"language\""));
    }

    // --provider is for one run and does not borrow the saved provider's
    // endpoint: no local model is installed here, so nothing is uploaded.
    let (code, local) = dir.eddie(&["transcribe", "p.eddie", "--provider", "local", "--force"]);
    assert_eq!((code, &local["ok"]), (1, &false.into()), "{local}");
    // Each asset reports its own failure.
    assert!(local.to_string().contains("no whisper model"), "{local}");
    assert_eq!(requests.lock().unwrap().len(), 2);
    let (code, unknown) = dir.eddie(&["transcribe", "p.eddie", "--provider", "nope"]);
    assert_eq!((code, &unknown["ok"]), (1, &false.into()), "{unknown}");
    assert!(
        unknown["error"]
            .as_str()
            .unwrap()
            .contains("local, meta, openai"),
        "{unknown}"
    );

    // An unknown asset is an error, not an empty success.
    let (code, missing) = dir.eddie(&["transcribe", "p.eddie", "nope.wav"]);
    assert_eq!((code, &missing["ok"]), (1, &false.into()));
}

fn blue_clip(dir: &Dir, name: &str) {
    dir.ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=320x180:r=30:d=4",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        name,
    ]);
}

#[cfg(unix)]
#[test]
fn a_served_project_takes_every_command_live() {
    use std::os::unix::fs::PermissionsExt;

    let dir = Dir::new("serve");
    blue_clip(&dir, "shot.mp4");
    let (code, _) = dir.eddie(&["new", "p.eddie", "shot.mp4"]);
    assert_eq!(code, 0);
    let clips = |dir: &Dir| dir.project()["timeline"]["clips"].as_array().unwrap().len();

    // These need an editor that stays open.
    let (code, none) = dir.eddie(&["editors"]);
    assert_eq!(
        (code, &none["editors"]),
        (0, &serde_json::json!([])),
        "{none}"
    );
    for command in ["undo", "redo", "save", "stop", "events"] {
        let (code, refused) = dir.eddie(&[command, "p.eddie"]);
        assert_eq!(code, 1, "{refused}");
        assert!(
            refused["error"]
                .as_str()
                .unwrap()
                .contains("needs a running editor"),
            "{refused}"
        );
    }

    let mut serve = dir.spawn(&["serve", "p.eddie"]);
    let ready = serve.line();
    assert_eq!(ready["ok"], true, "{ready}");
    // Only this user reaches the socket.
    let socket = PathBuf::from(ready["socket"].as_str().unwrap());
    assert!(socket.starts_with(dir.0.join("run/eddie")), "{ready}");
    let mode = |path: &Path| path.metadata().unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&socket), 0o600);
    assert_eq!(mode(socket.parent().unwrap()), 0o700);

    // Agents find it without knowing the project.
    let (code, listed) = dir.eddie(&["editors"]);
    assert_eq!(code, 0, "{listed}");
    let editors = listed["editors"].as_array().unwrap();
    assert_eq!(editors.len(), 1, "{listed}");
    assert_eq!(editors[0]["socket"], ready["socket"], "{listed}");
    assert_eq!(
        (&editors[0]["window"], &editors[0]["untitled"]),
        (&false.into(), &false.into()),
        "{listed}"
    );
    assert!(
        editors[0]["project"]
            .as_str()
            .unwrap()
            .ends_with("/p.eddie"),
        "{listed}"
    );

    // One editor per project.
    let (code, second) = dir.eddie(&["serve", "p.eddie"]);
    assert_eq!(code, 1, "{second}");
    assert_eq!(second["error"], "Another Eddie already has p.eddie open.");

    let mut events = dir.spawn(&["events", "p.eddie"]);
    let hello = events.line();
    assert_eq!(
        (&hello["event"], &hello["window"]),
        (&"hello".into(), &false.into()),
        "{hello}"
    );

    // An edit goes to the running editor, which saves it and tells
    // whoever listens.
    let (code, run) = dir.eddie(&["run", "p.eddie", "-e", "addAssetRange('shot.mp4', 0, 1, 2)"]);
    assert_eq!(code, 0, "{run}");
    assert_eq!(
        (&run["live"], &run["changed"], &run["saved"]),
        (&true.into(), &true.into(), &true.into()),
        "{run}"
    );
    assert_eq!(clips(&dir), 1);
    let changed = events.event("timeline");
    assert_eq!(
        (&changed["clips"], &changed["duration"]),
        (&1.into(), &2.0.into()),
        "{changed}"
    );

    // A script that fails halfway leaves nothing behind.
    let (code, failed) = dir.eddie(&[
        "run",
        "p.eddie",
        "-e",
        "addAssetRange('shot.mp4', 2, 0, 1); setFade(999, 0.1, 0)",
    ]);
    assert_eq!((code, &failed["ok"]), (1, &false.into()), "{failed}");
    assert_eq!(failed["live"], true);
    let (_, state) = dir.eddie(&["state", "p.eddie"]);
    assert_eq!(state["clips"].as_array().unwrap().len(), 1, "{state}");
    // `state` is what scripts see, without a reply's wrapping.
    assert!(state.get("ok").is_none() && state.get("live").is_none());

    // A dry run reports the edit and keeps neither it nor an undo step.
    let (code, dry) = dir.eddie(&[
        "run",
        "p.eddie",
        "-e",
        "addTitle('Later', 0, 1)",
        "--dry-run",
    ]);
    assert_eq!(code, 0, "{dry}");
    assert_eq!(
        (&dry["changed"], &dry["saved"], &dry["summary"]["clips"]),
        (&true.into(), &false.into(), &2.into()),
        "{dry}"
    );
    assert_eq!(dry["clips"][1]["name"], "Later", "{dry}");
    assert_eq!(clips(&dir), 1);

    // Each run is one undo step, and stepping history saves too.
    let (code, undone) = dir.eddie(&["undo", "p.eddie"]);
    assert_eq!(code, 0, "{undone}");
    assert_eq!(
        (
            &undone["changed"],
            &undone["saved"],
            &undone["summary"]["clips"]
        ),
        (&true.into(), &true.into(), &0.into()),
        "{undone}"
    );
    assert_eq!(clips(&dir), 0);
    assert_eq!(events.event("timeline")["clips"], 0);
    let (_, nothing) = dir.eddie(&["undo", "p.eddie"]);
    assert_eq!(nothing["changed"], false, "{nothing}");
    let (code, redone) = dir.eddie(&["redo", "p.eddie"]);
    assert_eq!((code, &redone["summary"]["clips"]), (0, &1.into()));
    assert_eq!(clips(&dir), 1);
    assert_eq!(events.event("timeline")["clips"], 1);

    // Media the editor is asked to import arrives in its bin and the file.
    blue_clip(&dir, "more.mp4");
    let (code, imported) = dir.eddie(&["import", "p.eddie", "more.mp4"]);
    assert_eq!(code, 0, "{imported}");
    assert_eq!(
        (&imported["live"], &imported["saved"]),
        (&true.into(), &true.into())
    );
    assert_eq!(dir.project()["media"].as_array().unwrap().len(), 2);
    let arrived = loop {
        let event = events.event("import");
        if event["importing"] == false {
            break event;
        }
    };
    assert_eq!(arrived["assets"], 2, "{arrived}");

    // Pictures come from the running editor's own timeline.
    let (code, sheet) = dir.eddie(&["sheet", "p.eddie", "sheet.jpg"]);
    assert_eq!(code, 0, "{sheet}");
    assert_eq!(sheet["live"], true);
    assert!(dir.0.join("sheet.jpg").metadata().unwrap().len() > 0);
    let (code, strip) = dir.eddie(&["strip", "p.eddie", "shot.mp4", "strip.jpg"]);
    assert_eq!((code, &strip["live"]), (0, &true.into()), "{strip}");
    assert!(dir.0.join("strip.jpg").metadata().unwrap().len() > 0);
    let (code, frames) = dir.eddie(&[
        "frames", "p.eddie", "shot.mp4", "1..2", "frames", "--every", "0.5",
    ]);
    assert_eq!((code, &frames["live"]), (0, &true.into()), "{frames}");
    let listed = frames["frames"].as_array().unwrap();
    assert_eq!(listed.len(), 2, "{frames}");
    assert!(Path::new(listed[1]["file"].as_str().unwrap()).exists());

    // The long jobs run there too: curation reaches the editor's scripts
    // without a reload, and the export is the timeline it holds.
    let (code, curated) = dir.eddie(&["curate", "p.eddie", "shot.mp4"]);
    assert_eq!(code, 0, "{curated}");
    assert_eq!(
        (&curated["live"], &curated["assets"][0]["curated"]),
        (&true.into(), &true.into()),
        "{curated}"
    );
    let (_, scenes) = dir.eddie(&["run", "p.eddie", "-e", "return sceneClips().length"]);
    assert_eq!(
        (&scenes["live"], &scenes["output"]),
        (&true.into(), &"1".into()),
        "{scenes}"
    );
    // Neither file has sound, so there is nothing to transcribe.
    let (code, transcribed) = dir.eddie(&["transcribe", "p.eddie"]);
    assert_eq!(
        (code, &transcribed["live"]),
        (0, &true.into()),
        "{transcribed}"
    );
    assert_eq!(
        transcribed["assets"],
        serde_json::json!([]),
        "{transcribed}"
    );
    let (code, exported) = dir.eddie(&["export", "p.eddie", "out.mp4"]);
    assert_eq!((code, &exported["live"]), (0, &true.into()), "{exported}");
    let length = duration(&dir.0.join("out.mp4"));
    assert!((length - 2.0).abs() < 0.1, "exported {length} s");

    let (code, saved) = dir.eddie(&["save", "p.eddie"]);
    assert_eq!((code, &saved["saved"]), (0, &true.into()), "{saved}");
    let (code, unknown) = dir.eddie(&["save", "p.eddie", "extra"]);
    assert_eq!(code, 1, "{unknown}");

    // Stopping ends the server, the listeners and the socket.
    let (code, stopped) = dir.eddie(&["stop", "p.eddie"]);
    assert_eq!((code, &stopped["stopped"]), (0, &true.into()), "{stopped}");
    assert_eq!(serve.line()["stopped"], ready["serving"]);
    assert_eq!(serve.exit_code(), 0);
    loop {
        let line = events.line();
        assert!(!line.is_null(), "events ended without saying so");
        if line.get("closed").is_some() {
            break;
        }
    }
    assert_eq!(events.exit_code(), 0);
    assert!(!socket.exists());

    // Without an editor the same command works on the file again.
    let (code, run) = dir.eddie(&["run", "p.eddie", "-e", "return state().clips.length"]);
    assert_eq!(code, 0, "{run}");
    assert_eq!((&run["output"], run.get("live")), (&"1".into(), None));
}

#[cfg(unix)]
#[test]
fn a_killed_editor_leaves_a_socket_the_next_one_replaces() {
    let dir = Dir::new("stale");
    let (code, _) = dir.eddie(&["new", "p.eddie"]);
    assert_eq!(code, 0);
    let mut serve = dir.spawn(&["serve", "p.eddie"]);
    let socket = PathBuf::from(serve.line()["socket"].as_str().unwrap());
    serve.child.kill().unwrap();
    serve.child.wait().unwrap();
    assert!(socket.exists(), "a killed editor cannot tidy up");

    // Nobody answers there, so commands work on the file.
    let (code, run) = dir.eddie(&["run", "p.eddie", "-e", "addTitle('Hi', 0, 1)"]);
    assert_eq!(code, 0, "{run}");
    assert_eq!((&run["saved"], run.get("live")), (&true.into(), None));

    let mut again = dir.spawn(&["serve", "p.eddie"]);
    assert_eq!(again.line()["ok"], true);
    let (code, state) = dir.eddie(&["inspect", "p.eddie"]);
    assert_eq!((code, &state["live"]), (0, &true.into()), "{state}");
    let (code, _) = dir.eddie(&["stop", "p.eddie"]);
    assert_eq!(code, 0);
    assert_eq!(again.exit_code(), 0);
}

#[test]
fn curation_runs_once_per_file_and_reaches_scripts() {
    let dir = Dir::new("curate");
    // Two shots, so the scene list has a cut to find.
    dir.ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=320x180:r=30:d=3",
        "-f",
        "lavfi",
        "-i",
        "smptebars=s=320x180:r=30:d=3",
        "-filter_complex",
        "[0:v][1:v]concat=n=2:v=1:a=0",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "shots.mp4",
    ]);
    let (code, _) = dir.eddie(&["new", "p.eddie", "shots.mp4"]);
    assert_eq!(code, 0);

    let (code, first) = dir.eddie(&["curate", "p.eddie"]);
    assert_eq!(code, 0, "{first}");
    let entry = &first["assets"][0];
    assert_eq!(
        (
            &entry["asset"],
            &entry["curated"],
            &entry["reused"],
            &entry["scenes"]
        ),
        (&"shots.mp4".into(), &true.into(), &false.into(), &2.into()),
        "{first}"
    );
    // Curation is about the media, not the project.
    assert!(first.get("saved").is_none(), "{first}");

    // Saved beside the settings: a second run answers from that.
    let (code, again) = dir.eddie(&["curate", "p.eddie", "shots.mp4"]);
    assert_eq!(
        (
            code,
            &again["assets"][0]["reused"],
            &again["assets"][0]["scenes"]
        ),
        (0, &true.into(), &2.into()),
        "{again}"
    );
    let (code, forced) = dir.eddie(&["curate", "p.eddie", "--force"]);
    assert_eq!(
        (code, &forced["assets"][0]["reused"]),
        (0, &false.into()),
        "{forced}"
    );

    let (code, run) = dir.eddie(&[
        "run",
        "p.eddie",
        "-e",
        "const scenes = sceneClips(); \
         return scenes.length + ' ' + scenes[1].sourceIn",
    ]);
    assert_eq!(code, 0, "{run}");
    assert_eq!(run["output"], "2 3");

    let (code, missing) = dir.eddie(&["curate", "p.eddie", "nope.mp4"]);
    assert_eq!((code, &missing["ok"]), (1, &false.into()), "{missing}");
}

#[test]
fn commands_at_the_same_time_take_turns_on_a_project() {
    let dir = Dir::new("turns");
    let (code, _) = dir.eddie(&["new", "p.eddie"]);
    assert_eq!(code, 0);
    let running: Vec<_> = (0..6)
        .map(|n| {
            let script = format!("addTitle('Title {n}', {}, 1)", n * 2);
            dir.command(".", &["run", "p.eddie", "-e", script.as_str()])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in running {
        let out = child.wait_with_output().unwrap();
        let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            (out.status.code(), &reply["saved"]),
            (Some(0), &true.into()),
            "{reply}"
        );
    }
    let project = dir.project();
    let clips = project["timeline"]["clips"].as_array().unwrap();
    let mut ids: Vec<_> = clips
        .iter()
        .map(|clip| clip["id"].as_u64().unwrap())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        (clips.len(), ids.len()),
        (6, 6),
        "every edit is kept and no clip ID is given out twice"
    );
}

/// One request on its own connection to a served project's socket.
/// `closing` shuts the sending side first, as `echo … | nc -U` does.
#[cfg(unix)]
fn ask(socket: &Path, request: &str, closing: bool) -> Value {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .unwrap();
    writeln!(stream, "{request}").unwrap();
    if closing {
        stream.shutdown(std::net::Shutdown::Write).unwrap();
    }
    for line in BufReader::new(stream).lines() {
        let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if value["id"] == 1 && value.get("ok").is_some() {
            return value;
        }
    }
    panic!("closed without answering {request}");
}

#[cfg(unix)]
#[test]
fn a_socket_client_is_answered_after_it_closes_and_told_when_a_save_fails() {
    let dir = Dir::new("socket");
    // In its own folder, so the folder can go away under the editor.
    std::fs::create_dir_all(dir.0.join("work")).unwrap();
    let (code, _) = dir.eddie_in("work", &["new", "p.eddie"]);
    assert_eq!(code, 0);
    let mut serve = dir.spawn(&["serve", "work/p.eddie"]);
    let ready = serve.line();
    assert_eq!(ready["ok"], true, "{ready}");
    let socket = PathBuf::from(ready["socket"].as_str().unwrap());

    let run = |code: &str| serde_json::json!({ "id": 1, "method": "run", "code": code });
    let first = ask(&socket, &run("addTitle('One', 0, 1)").to_string(), true);
    assert_eq!(
        (&first["ok"], &first["saved"]),
        (&true.into(), &true.into()),
        "{first}"
    );

    // Only true is a dry run; anything else there must not edit for real.
    let mut dry = run("addTitle('Dry', 2, 1)");
    dry["dryRun"] = "true".into();
    let refused = ask(&socket, &dry.to_string(), false);
    assert_eq!(
        (&refused["ok"], &refused["error"]),
        (&false.into(), &"dryRun is true or false".into()),
        "{refused}"
    );

    // The edit is in the editor whether or not the file could be written,
    // so the answer is not a failure to send again.
    std::fs::rename(dir.0.join("work"), dir.0.join("moved")).unwrap();
    let unsaved = ask(&socket, &run("addTitle('Two', 4, 1)").to_string(), false);
    assert_eq!(
        (&unsaved["ok"], &unsaved["changed"], &unsaved["saved"]),
        (&true.into(), &true.into(), &false.into()),
        "{unsaved}"
    );
    assert!(
        unsaved["saveError"]
            .as_str()
            .unwrap()
            .starts_with("Couldn't save p.eddie"),
        "{unsaved}"
    );
    let count = ask(
        &socket,
        &run("return state().clips.length").to_string(),
        false,
    );
    assert_eq!(count["output"], "2", "{count}");

    let stopped = ask(&socket, r#"{"id":1,"method":"stop"}"#, false);
    assert_eq!(stopped["stopped"], true, "{stopped}");
    assert_eq!(serve.exit_code(), 0);
}

/// One built-in call that the 500 ms limit cannot interrupt: a search that
/// compares 7 MB at each of 7 million places. It queues a title first.
#[cfg(unix)]
const STUCK_SCRIPT: &str = "addTitle('Never', 0, 1); const hay = 'a'.repeat(1.4e7); \
                            const needle = 'a'.repeat(7e6) + 'b'; return hay.indexOf(needle)";

#[cfg(unix)]
#[test]
fn a_script_stuck_in_one_built_in_call_is_refused_and_the_editor_goes_on() {
    use std::time::{Duration, Instant};

    let dir = Dir::new("stuck");
    let (code, _) = dir.eddie(&["new", "p.eddie"]);
    assert_eq!(code, 0);

    // Standalone: an error and an exit, not a command that never returns.
    let mut alone = dir
        .command(".", &["run", "p.eddie", "-e", STUCK_SCRIPT])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while alone.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(10) {
            alone.kill().unwrap();
            alone.wait().unwrap();
            panic!("eddie run was still running after 10 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = alone.wait_with_output().unwrap();
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        (out.status.code(), &reply["ok"]),
        (Some(1), &false.into()),
        "{reply}"
    );
    assert!(
        reply["error"].as_str().unwrap().contains("time limit"),
        "{reply}"
    );

    // Live: the editor answers the next request and applied nothing.
    let mut serve = dir.spawn(&["serve", "p.eddie"]);
    let ready = serve.line();
    assert_eq!(ready["ok"], true, "{ready}");
    let socket = PathBuf::from(ready["socket"].as_str().unwrap());
    let run = |code: &str| serde_json::json!({ "id": 1, "method": "run", "code": code });
    let refused = ask(&socket, &run(STUCK_SCRIPT).to_string(), false);
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        refused["error"].as_str().unwrap().contains("time limit"),
        "{refused}"
    );
    let asked = Instant::now();
    let next = ask(
        &socket,
        &run("addTitle('After', 0, 1); return state().clips.length").to_string(),
        false,
    );
    assert_eq!(
        (&next["ok"], &next["output"]),
        (&true.into(), &"0".into()),
        "{next}"
    );
    assert!(
        asked.elapsed() < Duration::from_secs(5),
        "{:?}",
        asked.elapsed()
    );

    let stopped = ask(&socket, r#"{"id":1,"method":"stop"}"#, false);
    assert_eq!(stopped["stopped"], true, "{stopped}");
    assert_eq!(serve.exit_code(), 0);
    let project = dir.project();
    let clips = project["timeline"]["clips"].as_array().unwrap();
    assert_eq!(clips.len(), 1, "{clips:?}");
    assert_eq!(clips[0]["name"], "After");
}

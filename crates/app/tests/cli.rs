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

    // The sheet: one cell per visible shot, here the titled second and the
    // bare one, both showing the placed (blue) source time.
    let (code, sheet) = dir.eddie(&["sheet", "p.eddie", "sheet.jpg"]);
    assert_eq!(code, 0, "{sheet}");
    assert_eq!(
        (&sheet["shots"], &sheet["sampled"]),
        (&2.into(), &false.into())
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
    std::fs::create_dir_all(dir.0.join("config/openreel")).unwrap();
    std::fs::write(
        dir.0.join("config/openreel/config.json"),
        serde_json::json!({"transcription": {"provider": "openai", "base_url": base}}).to_string(),
    )
    .unwrap();
    let (code, _) = dir.eddie(&["new", "p.eddie", "voice.wav"]);
    assert_eq!(code, 0);

    let (code, first) = dir.eddie(&["transcribe", "p.eddie", "--language", "en"]);
    assert_eq!(code, 0, "{first}");
    let entry = &first["assets"][0];
    assert_eq!(
        (&entry["turns"], &entry["words"], &entry["saved"]),
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
        (code, &again["assets"][0]["saved"]),
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

    // An unknown asset is an error, not an empty success.
    let (code, missing) = dir.eddie(&["transcribe", "p.eddie", "nope.wav"]);
    assert_eq!((code, &missing["ok"]), (1, &false.into()));
}

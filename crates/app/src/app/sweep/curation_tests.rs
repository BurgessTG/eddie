//! Real decoding, image encoding, and curation against an HTTP fixture.
//! The provider is fake; these tests do not establish model judgment quality.

use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use super::{run_sweep, ChatSettings, ReviewStatus, SweepData};
use crate::app::chat::MODEL_SPARK;

static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct Footage {
    dir: PathBuf,
    info: openreel_media::MediaInfo,
    peaks: Vec<f32>,
}

impl Footage {
    fn new(
        seconds: f64,
        fps: impl std::fmt::Display,
        alternating: bool,
        audio: bool,
        extension: &str,
    ) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "eddie-visual-review-{}-{}",
            std::process::id(),
            FIXTURE_ID.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("source.{extension}"));
        let mut command = std::process::Command::new("ffmpeg");
        command.args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=red:s=64x36:r={fps}:d={seconds}"),
        ]);
        if audio {
            command.args([
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency=440:sample_rate=24000:duration={seconds}"),
                "-af",
                "volume=4",
                "-c:a",
                "aac",
            ]);
        }
        if alternating {
            command.args(["-vf", "geq=r='255*mod(N,2)':g='255*(1-mod(N,2))':b=0"]);
        }
        if extension == "webm" {
            command.args([
                "-c:v",
                "libvpx-vp9",
                "-deadline",
                "realtime",
                "-cpu-used",
                "8",
            ]);
        } else {
            command.args(["-c:v", "libx264", "-preset", "ultrafast"]);
        }
        let output = command
            .args([
                "-threads:v",
                "1",
                "-filter_threads",
                "1",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .output()
            .expect("generate source footage");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let info = openreel_media::probe(path.to_str().unwrap()).expect("probe source");
        let peaks = if audio {
            openreel_media::waveform_peaks(
                &info.path,
                info.duration,
                openreel_media::WAVEFORM_PEAKS_PER_SEC,
            )
            .expect("real waveform")
        } else {
            Vec::new()
        };
        Self { dir, info, peaks }
    }

    fn curate(&self, settings: &ChatSettings) -> SweepData {
        run_sweep(&self.info, &self.peaks, settings, &mut |_| {}).expect("curation result")
    }
}

impl Drop for Footage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[derive(Clone)]
struct Request {
    path: String,
    headers: String,
    body: Vec<u8>,
}

impl Request {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("request JSON")
    }
}

struct Api {
    settings: ChatSettings,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Api {
    fn new(respond: impl FnMut(&Request) -> (u16, String) + Send + 'static) -> Self {
        Self::with_timing(respond, |request| {
            timing_completion(request, |boundary| boundary["proposed"].as_f64().unwrap())
        })
    }

    fn with_timing(
        mut respond: impl FnMut(&Request) -> (u16, String) + Send + 'static,
        mut timing: impl FnMut(&Request) -> (u16, String) + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let settings = ChatSettings {
            base_url: format!(
                "http://{}/v1/chat/completions/",
                listener.local_addr().unwrap()
            ),
            api_key: "curation-fixture-key".to_owned(),
            ..ChatSettings::default()
        };
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let worker_stop = stop.clone();
        let worker_requests = requests.clone();
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("HTTP fixture accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                // The port is reachable by any local process. Only the app under
                // test sends the fixture key; anything else (a browser or dev
                // tool probing localhost) is answered 404 and never reaches the
                // test's handler or its recorded requests.
                let request = match read_fixture_request(&mut stream) {
                    Some(request)
                        if request.headers.lines().any(|line| {
                            line.split_once(':').is_some_and(|(key, value)| {
                                key.eq_ignore_ascii_case("authorization")
                                    && value.trim() == "Bearer curation-fixture-key"
                            })
                        }) =>
                    {
                        request
                    }
                    other => {
                        eprintln!(
                            "fixture ignored a foreign request: {:?}",
                            other.as_ref().and_then(|r| r.headers.lines().next())
                        );
                        let _ = write!(stream,
                            "HTTP/1.1 404 Fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        continue;
                    }
                };
                let response = if request.path == "/v1/chat/completions"
                    && request.json()["response_format"]["json_schema"]["name"] == "curation_timing"
                {
                    timing(&request)
                } else {
                    respond(&request)
                };
                let response = encode_fixture(&request, response);
                worker_requests.lock().unwrap().push(request);
                write!(stream,
                    "HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.0, response.1.len(), response.1).unwrap();
            }
        });
        Self {
            settings,
            stop,
            worker: Some(worker),
            requests,
        }
    }

    fn finish(mut self) -> Vec<Request> {
        self.stop.store(true, Ordering::Relaxed);
        self.worker
            .take()
            .unwrap()
            .join()
            .expect("HTTP fixture server");
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// One HTTP/1.1 request, or None for anything malformed or truncated.
fn read_fixture_request(stream: &mut std::net::TcpStream) -> Option<Request> {
    let mut headers = String::new();
    let mut reader = std::io::BufReader::new(stream);
    while !headers.ends_with("\r\n\r\n") {
        if reader.read_line(&mut headers).ok()? == 0 || headers.len() >= 64 * 1024 {
            return None;
        }
    }
    let header = |name: &str| {
        headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
    };
    let mut body = Vec::new();
    if header("transfer-encoding") == Some("chunked") {
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).ok()?;
            let size = usize::from_str_radix(line.trim().split(';').next()?, 16).ok()?;
            if size == 0 {
                break;
            }
            if body.len() + size >= 16 * 1024 * 1024 {
                return None;
            }
            let offset = body.len();
            body.resize(offset + size, 0);
            reader.read_exact(&mut body[offset..]).ok()?;
            let mut crlf = [0; 2];
            reader.read_exact(&mut crlf).ok()?;
            if crlf != *b"\r\n" {
                return None;
            }
        }
    } else {
        let length: usize = match header("content-length") {
            Some(n) => n.parse().ok()?,
            None => 0,
        };
        if length >= 16 * 1024 * 1024 {
            return None;
        }
        body.resize(length, 0);
        reader.read_exact(&mut body).ok()?;
    }
    let path = headers
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .to_owned();
    Some(Request {
        path,
        headers,
        body,
    })
}

fn catalog() -> (u16, String) {
    (200, json!({"data":[{"id":MODEL_SPARK}]}).to_string())
}

fn input_windows(body: &Value) -> Vec<Value> {
    body["messages"][1]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|content| serde_json::from_str::<Value>(content["text"].as_str()?).ok())
        .filter(|row| row["window"].is_u64())
        .collect()
}

fn range(start: f64, end: f64, category: &str) -> Value {
    json!({"start":start, "end":end, "category":category, "score":68.0,
        "tags":["steady"], "reason":"Fixture editorial selection.",
        "continuesBefore":false, "continuesAfter":false, "subjectCheck":false})
}

fn whole_window(window: &Value) -> Value {
    json!({"window":window["window"], "reason":"Fixture full window.", "ranges":[range(
        window["start"].as_f64().unwrap(), window["end"].as_f64().unwrap(), "action") ]})
}

fn completion(reviews: Vec<Value>) -> (u16, String) {
    (200, json!({
        "choices":[{"finish_reason":"stop", "message":{"content":json!({"reviews":reviews}).to_string()}}],
        "usage":{"prompt_tokens":100, "completion_tokens":30, "prompt_tokens_details":{"cached_tokens":20}},
    }).to_string())
}

/// Keep time-based fixture scenarios readable while speaking the host's new
/// edge-ID protocol. Explicit ID/omission fixtures pass through untouched;
/// invalid times produce invalid IDs, never a clamped successful response.
fn encode_fixture(request: &Request, response: (u16, String)) -> (u16, String) {
    if request.path != "/v1/chat/completions" || response.0 != 200 {
        return response;
    }
    let body = request.json();
    let stage = body["response_format"]["json_schema"]["name"]
        .as_str()
        .unwrap();
    if !matches!(stage, "curation" | "curation_orientation") {
        return response;
    }
    let windows = input_windows(&body);
    let mut envelope: Value = serde_json::from_str(&response.1).unwrap();
    let Some(text) = envelope["choices"][0]["message"]["content"].as_str() else {
        return response;
    };
    let mut content: Value = serde_json::from_str(text).unwrap();
    let Some(reviews) = content["reviews"].as_array_mut() else {
        return response;
    };
    for review in reviews {
        let Some(window) = windows
            .iter()
            .find(|window| window["window"] == review["window"])
        else {
            continue;
        };
        let edges = window["edgeTimes"].as_array().unwrap();
        let edge = |value: &Value| -> Value {
            let Some(time) = value.as_f64() else {
                return json!(-1);
            };
            if time < window["start"].as_f64().unwrap() - 1e-9
                || time > window["end"].as_f64().unwrap() + 1e-9
            {
                return json!(-1);
            }
            json!(
                edges
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| (a.as_f64().unwrap() - time)
                        .abs()
                        .total_cmp(&(b.as_f64().unwrap() - time).abs()))
                    .unwrap()
                    .0
            )
        };
        if stage == "curation_orientation" {
            if review.get("description").is_some() {
                continue;
            }
            let mut checks = review.get("subjectChecks").cloned().unwrap_or_else(|| {
                let speech = review["ranges"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|range| range["subjectCheck"] == true)
                    .collect::<Vec<_>>();
                json!(speech
                    .first()
                    .into_iter()
                    .flat_map(|first| ["person", "face"].map(|concept| json!({
                    "concept":concept,"start":first["start"],"end":speech.last().unwrap()["end"],
                    "reason":"Fixture subject visibility question."})))
                    .collect::<Vec<_>>())
            });
            for check in checks.as_array_mut().unwrap() {
                check["startEdge"] = edge(&check["start"]);
                check["endEdge"] = edge(&check["end"]);
                check["anchorPicture"] = Value::Null;
                check["anchorBox"] = Value::Null;
            }
            *review = json!({"window":review["window"], "description":"Fixture source orientation.","subjectChecks":checks});
        } else if review.get("omissions").is_none() {
            for range in review["ranges"].as_array_mut().unwrap() {
                if range.get("startEdge").is_none() {
                    range["startEdge"] = edge(&range["start"]);
                }
                if range.get("endEdge").is_none() {
                    range["endEdge"] = edge(&range["end"]);
                }
            }
            let mut spans: Vec<_> = review["ranges"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|range| {
                    Some((range["startEdge"].as_u64()?, range["endEdge"].as_u64()?))
                })
                .collect();
            spans.sort_unstable();
            let mut cursor = 0;
            let mut omissions = Vec::new();
            for (start, end) in spans.into_iter().chain(std::iter::once((
                edges.len() as u64 - 1,
                edges.len() as u64 - 1,
            ))) {
                if start > cursor {
                    omissions.push(
                        json!({"startEdge":cursor,"endEdge":start,"status":"rejected",
                        "reason":"Fixture excludes this gap."}),
                    );
                }
                cursor = cursor.max(end);
            }
            review["omissions"] = json!(omissions);
        }
    }
    envelope["choices"][0]["message"]["content"] = json!(content.to_string());
    (response.0, envelope.to_string())
}

fn timing_completion(request: &Request, mut choose: impl FnMut(&Value) -> f64) -> (u16, String) {
    let body = request.json();
    let mut decisions = Vec::new();
    for window in input_windows(&body) {
        for boundary in window["boundaries"].as_array().unwrap() {
            let time = choose(boundary);
            let choice = boundary["choices"]
                .as_array()
                .unwrap()
                .iter()
                .find(|choice| (choice["time"].as_f64().unwrap() - time).abs() < 1e-9)
                .unwrap_or_else(|| panic!("missing source boundary {time}: {boundary}"));
            decisions.push(
                json!({"boundary":boundary["boundary"], "choice":choice["choice"],
                "expand":"none", "reason":"Fixture source-frame decision."}),
            );
        }
    }
    (200, json!({
        "choices":[{"finish_reason":"stop", "message":{"content":json!({"boundaries":decisions}).to_string()}}],
        "usage":{"prompt_tokens":100, "completion_tokens":30, "prompt_tokens_details":{"cached_tokens":20}},
    }).to_string())
}

fn sam_completion(text: &str) -> (u16, String) {
    let mut events = Vec::new();
    for line in text.split_inclusive('\n') {
        events.push(json!({"type":"response.output_text.delta", "delta":line,
            "item_id":"fixture", "output_index":0, "content_index":0}));
    }
    events.push(json!({"type":"response.output_text.done", "text":text,
        "item_id":"fixture", "output_index":0, "content_index":0}));
    events.push(json!({"type":"response.completed", "response":{
        "status":"completed", "usage":{"video_frames_processed":50}}}));
    (
        200,
        events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect(),
    )
}

fn assert_inventory(data: &SweepData, count: usize, end: f64) {
    assert_eq!(data.scene_clips.len(), count);
    assert_eq!(data.scene_clips[0].source_in, 0.0);
    assert!((data.scene_clips.last().unwrap().end() - end).abs() < 1e-6);
    assert!(data
        .scene_clips
        .windows(2)
        .all(|pair| pair[0].end() == pair[1].source_in));
}

#[test]
fn failed_transcription_is_missing_evidence_not_silence() {
    let source = Footage::new(6.0, 4, false, true, "mp4");
    for available in [false, true] {
        let api =
            Api::new(move |request| match request.path.as_str() {
                "/v1/models" => catalog(),
                "/v1/asr/transcribe" if available => (200, json!({"turns":[]}).to_string()),
                "/v1/asr/transcribe" => (403, "{}".to_owned()),
                "/v1/chat/completions" => {
                    let windows = input_windows(&request.json());
                    assert_eq!(windows[0]["transcript"], json!([]));
                    assert_eq!(
                    windows[0]["transcriptionStatus"],
                    if available { "available" } else { "unavailable" },
                    "an unavailable transcript cannot be presented as verified absence of speech"
                );
                    completion(vec![whole_window(&windows[0])])
                }
                other => panic!("unexpected request {other}"),
            });
        let data = source.curate(&api.settings);
        api.finish();
        assert_inventory(&data, 1, 6.0);
        assert_eq!(data.note.contains("Transcription failed"), !available);
        assert_eq!(data.candidates[0].review.status, ReviewStatus::Reviewed);
    }
}

#[test]
fn speech_trims_keep_the_complete_available_turn_and_natural_handles() {
    let source = Footage::new(6.0, 4, false, true, "mp4");
    let api = Api::new(|request| {
        match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => (200, json!({"turns":[{
            "startMs":400,"endMs":4700,"transcript":"Keep this whole useful thought.","speaker":"A"
        }]}).to_string()),
        "/v1/chat/completions" => {
            let mut spoken = range(1.0, 4.0, "talking");
            spoken["tags"] = json!(["speech"]);
            completion(vec![json!({"window":0,"reason":"Useful speech with a false visual trim.",
                "ranges":[spoken],"subjectChecks":[]})])
        }
        other => panic!("unexpected request {other}"),
    }
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 6.0);
    assert_eq!(data.candidates.len(), 1, "{}", data.note);
    let clip = &data.candidates[0];
    assert_eq!(clip.review.status, ReviewStatus::Reviewed, "{}", data.note);
    assert_eq!(
        (clip.source_in, clip.end()),
        (0.25, 5.0),
        "the sampled visual proposal must not cut inside an available speech turn"
    );
}

#[test]
fn speech_handles_do_not_pull_in_or_clip_the_next_turn() {
    let source = Footage::new(6.0, 4, false, true, "mp4");
    let api =
        Api::new(|request| {
            match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => (200, json!({"turns":[
            {"startMs":400,"endMs":2150,"transcript":"The selected thought.","speaker":"A"},
            {"startMs":2250,"endMs":4700,"transcript":"A separate unwanted take.","speaker":"A"}
        ]}).to_string()),
        "/v1/chat/completions" => completion(vec![json!({"window":0,
            "reason":"Only the first thought is useful.",
            "ranges":[range(0.5,2.0,"talking")], "subjectChecks":[]})]),
        other => panic!("unexpected request {other}"),
    }
        });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 6.0);
    assert_eq!(data.candidates.len(), 1, "{}", data.note);
    let clip = &data.candidates[0];
    assert_eq!(clip.review.status, ReviewStatus::Reviewed, "{}", data.note);
    assert_eq!((clip.source_in, clip.end()), (0.25, 2.25));
    assert_eq!(clip.transcript.len(), 1);
}

#[test]
fn speech_fragments_expose_their_source_turn_and_asr_session() {
    let source = Footage::new(2.0, 4, true, true, "mp4");
    let api = Api::new(|request| {
        match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => (200, json!({"sessionId":"speech-session", "audioDurationMs":2000,
            "turns":[{"turnId":1,"startMs":100,"endMs":1900,
                "transcript":"A thought continuing across camera cuts.","speaker":"A"}]}).to_string()),
        "/v1/chat/completions" => completion(input_windows(&request.json()).iter().map(|window| {
            json!({"window":window["window"], "reason":"Useful speech picture fragment.",
                "ranges":[range(window["start"].as_f64().unwrap(),window["end"].as_f64().unwrap(),"talking")],
                "subjectChecks":[]})
        }).collect()),
        other => panic!("unexpected request {other}"),
    }
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 8, 2.0);
    let ctx = eframe::egui::Context::default();
    let mut app = crate::app::EditorApp::headless_for_test(&ctx);
    app.assets.push(crate::app::BinAsset {
        info: source.info.clone(),
        size_bytes: 0,
        state: crate::app::media::AssetState::Ready,
        peaks: source.peaks.clone(),
        thumb_textures: Vec::new(),
        sweep: data,
    });
    let state: Value = serde_json::from_str(&app.script_state_json()).unwrap();
    assert_eq!(
        state["transcriptSegments"][0]["asrSession"]["id"],
        "speech-session"
    );
    for clip in state["sweepCandidates"].as_array().unwrap() {
        let speech = &clip["review"]["speech"][0];
        assert_eq!(speech["turnStart"], 0.1);
        assert_eq!(speech["turnEnd"], 1.9);
        assert_eq!(speech["sessionId"], "speech-session");
        assert_eq!(
            speech["completeAvailableTurn"], false,
            "a camera-cut fragment must not claim to be the whole thought"
        );
    }
}

#[test]
fn adjacent_speech_keeps_complete_turns_without_overlapping_handles() {
    let source = Footage::new(8.0, 10, false, true, "mp4");
    for rejected_gap in [false, true] {
        let api = Api::new(move |request| {
            match request.path.as_str() {
            "/v1/models" => catalog(),
            "/v1/asr/transcribe" => (200, json!({"turns":[
                {"startMs":500,"endMs":1800,"transcript":"First connected turn.","speaker":"A"},
                {"startMs":1800,"endMs":3250,"transcript":"The reply.","speaker":"B"},
                {"startMs":4000,"endMs":4700,"transcript":"A separate beat.","speaker":"A"},
                {"startMs":5100,"endMs":6000,"transcript":"Another beat after silence.","speaker":"B"}
            ]}).to_string()),
            "/v1/chat/completions" => completion(vec![json!({"window":0,
                "reason":"Adjacent proposed speech beats, including a split inside a turn.",
                "ranges":[range(0.5,1.5,"talking"),
                    range(if rejected_gap { 1.7 } else { 1.5 },3.3,"talking"),
                    range(3.9,4.8,"talking"),range(4.8,6.1,"talking")],
                "subjectChecks":[]})]),
            other => panic!("unexpected request {other}"),
        }
        });
        let data = source.curate(&api.settings);
        api.finish();
        assert_inventory(&data, 1, 8.0);
        let mut clips: Vec<_> = data.candidates.iter().collect();
        clips.sort_by(|a, b| a.source_in.total_cmp(&b.source_in));
        if rejected_gap {
            // Rejected speech cannot be silently joined, and its edges sit inside
            // one utterance. Those two ranges need review; the others keep
            // silence handles and stay on the normal (verified) timing path.
            assert_eq!(
                clips
                    .iter()
                    .map(|c| (c.source_in, c.end(), c.review.status))
                    .collect::<Vec<_>>(),
                [
                    (0.3, 1.5, ReviewStatus::NeedsReview),
                    (1.7, 3.4, ReviewStatus::NeedsReview),
                    (3.8, 4.8, ReviewStatus::Reviewed),
                    (4.8, 6.2, ReviewStatus::Reviewed),
                ],
                "{}",
                data.note
            );
            assert!(clips[2].review_json()["edgeIssue"].is_null());
            continue;
        }
        assert_eq!(clips.len(), 3, "{}", data.note);
        assert!(clips
            .iter()
            .all(|clip| clip.review.status == ReviewStatus::Reviewed));
        assert!((clips[0].source_in - 0.3).abs() < 1e-9);
        assert!((clips[0].end() - 3.4).abs() < 1e-9);
        assert!((clips[1].source_in - 3.8).abs() < 1e-9);
        assert!((clips[2].end() - 6.2).abs() < 1e-9);
        assert!((clips[1].end() - clips[2].source_in).abs() < 1e-9);
        assert!(
            (4.7..=5.1).contains(&clips[1].end()),
            "split must remain in silence"
        );
        assert!(clips
            .windows(2)
            .all(|pair| pair[0].end() <= pair[1].source_in));
        for clip in clips {
            for turn in &clip.transcript {
                assert!(clip.source_in <= turn.start && clip.end() >= turn.end);
            }
        }
    }
}

#[test]
fn speech_split_without_a_pause_keeps_the_trailing_thought_with_its_continuation() {
    // Host-longtake regression: a turn ends on "…not sure why but" and the
    // next speaker continues 80 ms later. The review split there, and the host
    // cut 16 ms after the turn end, ending the clip mid-thought.
    let source = Footage::new(8.0, 24, false, true, "mp4");
    let api = Api::new(|request| {
        match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => (200, json!({"turns":[
            {"startMs":500,"endMs":3860,"speaker":"B",
                "transcript":"they said all the different Doctors were represented not sure why but"},
            {"startMs":3940,"endMs":5500,"speaker":"A",
                "transcript":"so so our theme this weekend it's Vikings weekend then"}
        ]}).to_string()),
        "/v1/chat/completions" => {
            let mut topic = range(0.25, 3.916_666_7, "talking");
            topic["tags"] = json!(["speech"]);
            let mut recap = range(3.916_666_7, 6.0, "talking");
            recap["tags"] = json!(["speech"]);
            completion(vec![json!({"window":0, "reason":"Topic, then a recap.",
                "ranges":[topic, recap], "subjectChecks":[]})])
        }
        other => panic!("unexpected request {other}"),
    }
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 8.0);
    let speech: Vec<_> = data
        .candidates
        .iter()
        .filter(|clip| clip.review.status == ReviewStatus::Reviewed)
        .collect();
    assert_eq!(
        speech
            .iter()
            .map(|clip| (clip.source_in, clip.end()))
            .collect::<Vec<_>>(),
        [(0.25, 6.0)],
        "an 80 ms speaker change is not a pause; do not cut the trailing thought: {}",
        data.note
    );
}

#[test]
fn unpunctuated_handover_after_a_finished_thought_is_a_cut_point() {
    // Host live run: ASR stopped punctuating mid-file, and every touching
    // speaker handover chained 19 s to 63 s into one utterance and one clip.
    // The last answer's dangling "and" still keeps its continuation.
    let source = Footage::new(8.0, 24, false, true, "mp4");
    let api = Api::new(|request| {
        match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => (
            200,
            json!({"turns":[
            {"startMs":500,"endMs":2500,"speaker":"B","transcript":"just look at how big that is"},
            {"startMs":2500,"endMs":4000,"speaker":"A","transcript":"what do you see there"},
            {"startMs":4000,"endMs":5000,"speaker":"B","transcript":"the depth of it and"},
            {"startMs":5000,"endMs":6000,"speaker":"A","transcript":"yeah all of that"}
        ]})
            .to_string(),
        ),
        "/v1/chat/completions" => completion(vec![json!({"window":0, "reason":"Exchange.",
            "ranges":[range(0.25, 2.5, "talking"), range(2.5, 4.5, "talking"),
                range(4.5, 7.0, "talking")],
            "subjectChecks":[]})]),
        other => panic!("unexpected request {other}"),
    }
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 8.0);
    let ranges: Vec<_> = reviewed_ranges(&data)
        .into_iter()
        .map(|(start, end, _)| (start, end))
        .collect();
    assert_eq!(ranges.len(), 2, "{ranges:?} {}", data.note);
    assert_eq!(ranges[0], (0.25, 2.5), "{}", data.note);
    assert_eq!(ranges[1].0, 2.5);
    assert!(
        ranges[1].1 >= 6.0,
        "the dangling \"and\" continues: {ranges:?}"
    );
}

#[test]
fn edge_a_frame_before_a_touching_handover_is_on_the_turn_boundary() {
    // Host replay: the talking range ended 10 ms before the next speaker's
    // turn, no picture edge sits exactly on that boundary, and the range fell
    // to Needs review. Turn-level ASR times are not that precise.
    let source = Footage::new(8.0, 24, false, true, "mp4");
    let api = Api::new(|request| {
        match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => (
            200,
            json!({"turns":[
            {"startMs":500,"endMs":3020,"speaker":"A","transcript":"Look at the size of this place."},
            {"startMs":3020,"endMs":5000,"speaker":"B","transcript":"It is the biggest one yet."}
        ]})
            .to_string(),
        ),
        "/v1/chat/completions" => completion(vec![json!({"window":0, "reason":"Talk, then pan.",
            "ranges":[range(0.25, 3.0, "talking"), range(3.0, 7.0, "establishing")],
            "subjectChecks":[]})]),
        other => panic!("unexpected request {other}"),
    }
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 8.0);
    assert_eq!(
        reviewed_ranges(&data),
        [
            (0.25, 3.0, "talking".to_owned()),
            (3.0, 7.0, "establishing".to_owned())
        ],
        "{}",
        data.note
    );
}

#[test]
fn transcribed_crowd_ambience_does_not_block_picture_cuts() {
    // Expo live run: ASR returned crowd noise as one 16 s turn reading "LT",
    // ending off the frame grid. The talking range starting inside it could
    // neither pull back nor move past it and fell to Needs review.
    let source = Footage::new(8.0, 24, false, true, "mp4");
    let api = Api::new(|request| {
        match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => (
            200,
            json!({"turns":[
            {"startMs":500,"endMs":6010,"speaker":"A","transcript":"LT"},
            {"startMs":6010,"endMs":7210,"speaker":"B","transcript":"Oh, you guys got a lot of merch."}
        ]})
            .to_string(),
        ),
        "/v1/chat/completions" => completion(vec![json!({"window":0, "reason":"Demo, then a remark.",
            "ranges":[range(0.25, 5.0, "action"), range(5.0, 8.0, "talking")],
            "subjectChecks":[]})]),
        other => panic!("unexpected request {other}"),
    }
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 8.0);
    assert_eq!(
        reviewed_ranges(&data),
        [
            (0.25, 5.0, "action".to_owned()),
            (5.0, 8.0, "talking".to_owned())
        ],
        "{}",
        data.note
    );
}

#[test]
fn a_heard_sentence_end_inside_a_turn_is_a_cut_point() {
    // Expo live run: one 8.5 s turn held several sentences, and the model
    // started a talking range at the end of one ("...that money.") inside it.
    // With turn timing alone the edge sat inside an utterance and the range
    // fell to Needs review; heard word timing makes the sentence end a cut.
    let source = Footage::new(8.0, 24, false, true, "mp4");
    let api = Api::new(|request| match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => {
            let words: Vec<_> = [
                ("You", 500, 900),
                ("got", 900, 1300),
                ("that", 1300, 1800),
                ("money.", 1800, 2950),
                ("And", 3050, 3500),
                ("you", 3500, 4000),
                ("can", 4000, 4600),
                ("learn", 4600, 5400),
                ("it.", 5400, 7000),
            ]
            .iter()
            .map(|(word, start, end)| json!({"word":word, "startMs":start, "endMs":end}))
            .collect();
            (
                200,
                json!({"turns":[{"startMs":500,"endMs":7000,"speaker":"A",
                "transcript":"You got that money. And you can learn it.", "words":words}]})
                .to_string(),
            )
        }
        "/v1/chat/completions" => completion(vec![json!({"window":0, "reason":"Play, then talk.",
            "ranges":[range(0.25, 3.0, "action"), range(3.0, 8.0, "talking")],
            "subjectChecks":[]})]),
        other => panic!("unexpected request {other}"),
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 8.0);
    assert_eq!(
        reviewed_ranges(&data),
        [
            (0.25, 3.0, "action".to_owned()),
            (3.0, 8.0, "talking".to_owned())
        ],
        "{}",
        data.note
    );
}

#[test]
fn fixture_server_ignores_foreign_local_clients() {
    // Some local process probes localhost ports. A stray GET / or an empty
    // connection used to panic the fixture and fail an unrelated test.
    let source = Footage::new(4.0, 24, false, false, "mp4");
    let api = Api::new(|request| match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/chat/completions" => completion(
            input_windows(&request.json())
                .iter()
                .map(whole_window)
                .collect(),
        ),
        other => panic!("unexpected request {other}"),
    });
    let address = api
        .settings
        .base_url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap()
        .to_owned();
    drop(std::net::TcpStream::connect(&address).unwrap());
    let mut probe = std::net::TcpStream::connect(&address).unwrap();
    write!(probe, "GET / HTTP/1.1\r\nHost: {address}\r\n\r\n").unwrap();
    let mut reply = String::new();
    std::io::Read::read_to_string(&mut probe, &mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 404"), "{reply}");
    let data = source.curate(&api.settings);
    let requests = api.finish();
    assert_inventory(&data, 1, 4.0);
    assert!(requests
        .iter()
        .all(|request| request.path.starts_with("/v1/")));
}

/// Reviewed (start, end, category) ranges, in source order.
fn reviewed_ranges(data: &SweepData) -> Vec<(f64, f64, String)> {
    let mut clips: Vec<_> = data
        .candidates
        .iter()
        .filter(|clip| clip.review.status == ReviewStatus::Reviewed)
        .map(|clip| (clip.source_in, clip.end(), clip.category.clone()))
        .collect();
    clips.sort_by(|a, b| a.0.total_cmp(&b.0));
    clips
}

#[test]
fn backchannel_does_not_finish_the_speakers_thought() {
    // Interviews live run: "So we had all these nobles with swords" then the
    // interviewer's "Uh-huh." made the thought look finished, and the clip
    // ended mid-story. The story resumes after a 0.5 s pause.
    let source = Footage::new(8.0, 24, false, true, "mp4");
    let api = Api::new(|request| {
        match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => (200, json!({"turns":[
            {"startMs":500,"endMs":3000,"speaker":"A","transcript":"That is what the nobles did. So we had all these nobles with swords"},
            {"startMs":3000,"endMs":3400,"speaker":"B","transcript":"Uh-huh."},
            {"startMs":3900,"endMs":5500,"speaker":"A","transcript":"and they lined up for the guard."}
        ]}).to_string()),
        "/v1/chat/completions" => completion(vec![json!({"window":0, "reason":"Story.",
            "ranges":[range(0.25, 3.5, "talking"), range(3.5, 6.0, "talking")],
            "subjectChecks":[]})]),
        other => panic!("unexpected request {other}"),
    }
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 8.0);
    assert_eq!(
        reviewed_ranges(&data),
        [(0.25, 6.0, "talking".to_owned())],
        "{}",
        data.note
    );
}

#[test]
fn narration_under_cutaways_keeps_their_picture_cuts() {
    // Mfs2 live run: one 28 s voiceover runs under several b-roll beats. The
    // model tags them `speech`, but only on-camera talking owns its turns.
    let source = Footage::new(8.0, 24, false, true, "mp4");
    let api = Api::new(|request| {
        match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => (200, json!({"turns":[{"startMs":300,"endMs":7700,"speaker":"A",
            "transcript":"The program has grown every year and students now compete across the state."}
        ]}).to_string()),
        "/v1/chat/completions" => {
            let mut first = range(0.25, 3.0, "broll");
            first["tags"] = json!(["speech"]);
            let mut second = range(3.0, 6.0, "broll");
            second["tags"] = json!(["speech"]);
            completion(vec![json!({"window":0, "reason":"Two cutaways.",
                "ranges":[first, second], "subjectChecks":[]})])
        }
        other => panic!("unexpected request {other}"),
    }
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 8.0);
    assert_eq!(
        reviewed_ranges(&data),
        [
            (0.25, 3.0, "broll".to_owned()),
            (3.0, 6.0, "broll".to_owned())
        ],
        "{}",
        data.note
    );
}

#[test]
fn talking_sentence_finishes_over_a_touching_cutaway() {
    // Host live run: the speaker keeps talking as the camera pans away, and
    // the model starts an establishing range mid-sentence. The talking range
    // finishes the sentence; the cutaway gives up its first frames.
    let source = Footage::new(8.0, 24, false, true, "mp4");
    let api = Api::new(|request| match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => (
            200,
            json!({"turns":[{"startMs":500,"endMs":4200,"speaker":"A",
            "transcript":"Just look at how big that whole place is."}]})
            .to_string(),
        ),
        "/v1/chat/completions" => completion(vec![json!({"window":0, "reason":"Talk, then pan.",
            "ranges":[range(0.25, 3.0, "talking"), range(3.0, 7.0, "establishing")],
            "subjectChecks":[]})]),
        other => panic!("unexpected request {other}"),
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 8.0);
    let ranges = reviewed_ranges(&data);
    assert_eq!(ranges.len(), 2, "{}", data.note);
    let (talk, pan) = (&ranges[0], &ranges[1]);
    assert_eq!((talk.0, talk.2.as_str()), (0.25, "talking"));
    assert!(
        (4.2..4.5).contains(&talk.1),
        "talking must end after the sentence, with a short handle: {ranges:?}"
    );
    assert_eq!(
        (pan.0, pan.1, pan.2.as_str()),
        (talk.1, 7.0, "establishing")
    );
}

#[test]
fn failed_timing_keeps_speech_edges_outside_available_turns() {
    let source = Footage::new(6.0, 4, false, true, "mp4");
    let api = Api::with_timing(
        |request| match request.path.as_str() {
            "/v1/models" => catalog(),
            "/v1/asr/transcribe" => (
                200,
                json!({"turns":[{"startMs":400,"endMs":4700,
                "transcript":"Keep this whole useful thought.","speaker":"A"}]})
                .to_string(),
            ),
            "/v1/chat/completions" => {
                let mut spoken = range(1.0, 4.0, "talking");
                spoken["tags"] = json!(["speech"]);
                completion(vec![json!({"window":0,"reason":"Useful speech.",
                    "ranges":[spoken, range(5.25, 5.75, "broll")],"subjectChecks":[]})])
            }
            other => panic!("unexpected request {other}"),
        },
        |_| (503, "{}".to_owned()),
    );
    let data = source.curate(&api.settings);
    let requests = api.finish();
    assert!(requests
        .iter()
        .any(|r| r.path.ends_with("/chat/completions")
            && r.json()["response_format"]["json_schema"]["name"] == "curation_timing"));
    assert_inventory(&data, 1, 6.0);
    let mut clips: Vec<_> = data.candidates.iter().collect();
    clips.sort_by(|a, b| a.source_in.total_cmp(&b.source_in));
    assert_eq!(clips.len(), 2, "{}", data.note);
    assert_eq!((clips[1].source_in, clips[1].end()), (5.25, 5.75));
    let clip = clips[0];
    assert_eq!(
        (clip.source_in, clip.end(), clip.review.status),
        (0.25, 5.0, ReviewStatus::Reviewed),
        "unverified edges still keep the complete turn with handles: {}",
        data.note
    );
    assert!(clip.review_json()["edgeIssue"].is_string());
}

#[test]
fn invalid_orientation_checks_are_skipped_without_losing_the_review() {
    let source = Footage::new(2.0, 4, false, false, "mp4");
    let api = Api::new(|request| {
        if request.path == "/v1/models" {
            return catalog();
        }
        if request.json()["response_format"]["json_schema"]["name"] == "curation_orientation" {
            // Recorded broll failure: endEdge equal to the edge count.
            return completion(vec![json!({"window":0,"description":"An arena.",
                "subjectChecks":[{"concept":"the object","reason":"Small object.",
                    "startEdge":0,"endEdge":9,"anchorPicture":null,"anchorBox":null}]})]);
        }
        completion(vec![json!({"window":0,"reason":"Useful.",
            "ranges":[range(0.0,2.0,"broll")],"subjectChecks":[]})])
    });
    let data = source.curate(&api.settings);
    let requests = api.finish();
    assert_inventory(&data, 1, 2.0);
    assert!(!requests.iter().any(|r| r.path.ends_with("/responses")));
    assert_eq!(
        data.candidates
            .iter()
            .map(|c| (c.source_in, c.end(), c.review.status))
            .collect::<Vec<_>>(),
        [(0.0, 2.0, ReviewStatus::Reviewed)],
        "{}",
        data.note
    );
}

#[test]
fn truncated_asr_turns_remain_uncertain_instead_of_becoming_complete_speech() {
    let source = Footage::new(6.0, 4, false, true, "mp4");
    let api = Api::new(|request| {
        match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => (200, json!({"sessionId":"partial-audio", "audioDurationMs":2500,
            "turns":[{"startMs":1000,"endMs":2500,"transcript":"An unfinished thought", "speaker":"A"}]}).to_string()),
        "/v1/chat/completions" => completion(vec![json!({"window":0,
            "reason":"Speech ending at a truncated transcription boundary.",
            "ranges":[range(1.0,2.0,"talking")],"subjectChecks":[]})]),
        other => panic!("unexpected request {other}"),
    }
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 6.0);
    assert_eq!(data.candidates.len(), 1);
    assert_eq!(data.candidates[0].review.status, ReviewStatus::NeedsReview);
    assert_eq!(
        (data.candidates[0].source_in, data.candidates[0].end()),
        (1.0, 2.0),
        "the reviewed range survives as Needs review, not a whole-shot fallback"
    );
}

#[test]
fn edge_accounting_preserves_uncertainty_and_rejects_incomplete_decisions() {
    let source = Footage::new(2.0, 4, false, false, "mp4");
    for case in ["valid", "hole", "overlap", "unknown-edge", "unknown-window"] {
        let api = Api::new(move |request| {
            if request.path == "/v1/models" {
                return catalog();
            }
            if request.json()["response_format"]["json_schema"]["name"] == "curation_orientation" {
                return completion(vec![json!({"window":0,
                    "description":"A useful middle with an uncertain tail.","subjectChecks":[]})]);
            }
            let mut review = json!({"window":0,"reason":"Complete source accounting.",
            "ranges":[{"startEdge":1,"endEdge":5,"score":70,"category":"broll",
                "tags":["detail"],"reason":"Useful detail.",
                "continuesBefore":false,"continuesAfter":false}],
            "omissions":[
                {"startEdge":0,"endEdge":1,"status":"rejected","reason":"Repositioning."},
                {"startEdge":5,"endEdge":8,"status":"needs-review","reason":"The ending is unclear."}
            ]});
            match case {
                "hole" => review["omissions"][1]["startEdge"] = json!(6),
                "overlap" => review["omissions"][1]["startEdge"] = json!(4),
                "unknown-edge" => review["ranges"][0]["endEdge"] = json!(9999),
                "unknown-window" => {
                    return completion(vec![review.clone(), json!({"window":9999})])
                }
                _ => {}
            }
            completion(vec![review])
        });
        let data = source.curate(&api.settings);
        api.finish();
        assert_inventory(&data, 1, 2.0);
        let mut clips: Vec<_> = data.candidates.iter().collect();
        clips.sort_by(|a, b| a.source_in.total_cmp(&b.source_in));
        if case == "valid" {
            assert_eq!(
                clips
                    .iter()
                    .map(|c| (c.source_in, c.end(), c.review.status))
                    .collect::<Vec<_>>(),
                [
                    (0.25, 1.25, ReviewStatus::Reviewed),
                    (1.25, 2.0, ReviewStatus::NeedsReview)
                ]
            );
        } else {
            assert_eq!(clips.len(), 1, "{case}: {}", data.note);
            assert_eq!(
                (clips[0].source_in, clips[0].end(), clips[0].review.status),
                (0.0, 2.0, ReviewStatus::NeedsReview),
                "{case}"
            );
        }
        let coverage = data.scene_clips[0].review.json(0.0, 2.0)["sourceCoverage"].clone();
        let spans = coverage.as_array().unwrap();
        assert_eq!(spans[0]["start"], 0.0);
        assert_eq!(spans.last().unwrap()["end"], 2.0);
        assert!(spans.windows(2).all(|p| p[0]["end"] == p[1]["start"]));
    }
}

#[test]
fn anchored_subjects_use_the_shown_picture_and_preserve_tracking_identity() {
    let source = Footage::new(2.0, 4, false, false, "mp4");
    for foreign_object in [false, true] {
        let api = Api::new(move |request| match request.path.as_str() {
            "/v1/models" => catalog(),
            "/v1/chat/completions" => {
                if request.json()["response_format"]["json_schema"]["name"]
                    == "curation_orientation"
                {
                    return completion(vec![json!({"window":0,
                        "description":"An identified small subject beside distractions.",
                        "subjectChecks":[{"concept":"face","reason":"Check visibility around the obstruction.",
                            "startEdge":0,"endEdge":8,"anchorPicture":4,
                            "anchorBox":[0.125,0.25,0.5,0.75]}]})]);
                }
                assert!(
                    !foreign_object,
                    "a different tracked object cannot inform the final review"
                );
                let windows = input_windows(&request.json());
                let evidence = &windows[0]["subjectEvidence"][0];
                assert_eq!(evidence["promptKind"], "object-anchor");
                let details = evidence["details"].as_array().unwrap();
                assert_eq!(
                    details
                        .iter()
                        .map(|d| d["sourceTime"].as_f64().unwrap())
                        .collect::<Vec<_>>(),
                    [0.0, 1.0, 1.75]
                );
                assert!(details.iter().all(|d| d["requestLocalId"] == "1"));
                completion(vec![whole_window(&windows[0])])
            }
            "/v1/responses" => {
                let body = request.json();
                assert_eq!(
                    body["input"][0]["content"][0]["text"],
                    "<4f>1<|box;x1=8;y1=9;x2=31;y2=26;w=64;h=36|>"
                );
                assert_eq!(body["metadata"]["propagation_direction"], "both");
                let id = if foreign_object { 2 } else { 1 };
                let text: String = [7,0,4].iter().map(|frame| format!(
                    "<{frame}f>{id}<|box;x1=8;y1=9;x2=31;y2=26;w=64;h=36|><|mask;x=0;y=0;data=5,5,!!!!!(QO(0lu8?|>\n")).collect();
                sam_completion(&text)
            }
            other => panic!("unexpected request {other}"),
        });
        let data = source.curate(&api.settings);
        api.finish();
        assert_inventory(&data, 1, 2.0);
        assert_eq!(data.candidates.len(), 1);
        let clip = &data.candidates[0];
        assert_eq!(
            clip.review.status,
            if foreign_object {
                ReviewStatus::NeedsReview
            } else {
                ReviewStatus::Reviewed
            },
            "{}",
            data.note
        );
        assert!(
            !clip.tags.iter().any(|tag| tag == "face-visible"),
            "an anchored box is not face classification"
        );
        assert_eq!(data.usage.sam.requests, 1);
        assert_eq!(data.usage.sam.failed, usize::from(foreign_object));
        assert_eq!(data.usage.sam.completed, usize::from(!foreign_object));
    }
}

#[test]
fn orientation_cannot_reject_source_and_final_review_uses_exact_edge_ids() {
    let source = Footage::new(2.25, 4, false, false, "mp4");
    let api = Api::new(|request| {
        if request.path == "/v1/models" {
            return catalog();
        }
        let body = request.json();
        let window = &input_windows(&body)[0];
        match body["response_format"]["json_schema"]["name"]
            .as_str()
            .unwrap()
        {
            "curation_orientation" => {
                let images = body["messages"][1]["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|part| part["type"] == "image_url")
                    .count();
                assert!(images <= 4);
                completion(vec![json!({"window":window["window"],
                    "description":"A steady view without a localization question.",
                    "subjectChecks":[]})])
            }
            "curation" => {
                assert!(window.get("draftReview").is_none());
                let edges = window["edgeTimes"]
                    .as_array()
                    .expect("host-issued source edges");
                assert_eq!(edges.len(), 10);
                completion(vec![json!({"window":window["window"],
                    "reason":"Retain the complete useful view.",
                    "ranges":[{"startEdge":0,"endEdge":9,"category":"broll",
                        "score":70,"tags":["detail"],"reason":"The object detail is useful.",
                        "continuesBefore":false,"continuesAfter":false}],
                    "omissions":[]})])
            }
            other => panic!("unexpected review stage {other}"),
        }
    });
    let data = source.curate(&api.settings);
    let requests = api.finish();
    assert_inventory(&data, 1, 2.25);
    assert_eq!(data.candidates.len(), 1);
    let clip = &data.candidates[0];
    assert_eq!((clip.source_in, clip.end()), (0.0, 2.25));
    assert_eq!(clip.review.status, ReviewStatus::Reviewed, "{}", data.note);
    assert!(clip.tags.iter().any(|tag| tag == "detail"));
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.path.ends_with("/chat/completions"))
            .count(),
        2
    );
}

#[test]
fn justified_ranges_do_not_require_a_duplicate_window_reason() {
    let source = Footage::new(1.25, 4, true, false, "mp4");
    let api = Api::new(|request| {
        if request.path == "/v1/models" {
            return catalog();
        }
        completion(
            input_windows(&request.json())
                .iter()
                .map(|window| {
                    let mut review = whole_window(window);
                    match window["window"].as_u64().unwrap() {
                        0 => {
                            review.as_object_mut().unwrap().remove("reason");
                        }
                        1 => review["reason"] = json!("  "),
                        2 => {
                            review["ranges"] = json!([]);
                            review.as_object_mut().unwrap().remove("reason");
                        }
                        3 => review["ranges"] = json!([]),
                        4 => review["ranges"][0]["reason"] = json!(""),
                        _ => unreachable!(),
                    }
                    review
                })
                .collect(),
        )
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 5, 1.25);
    assert_eq!(data.candidates.len(), 4);
    assert_eq!(
        data.scene_clips
            .iter()
            .map(|scene| scene.review.status)
            .collect::<Vec<_>>(),
        [
            ReviewStatus::Reviewed,
            ReviewStatus::Reviewed,
            ReviewStatus::NeedsReview,
            ReviewStatus::Rejected,
            ReviewStatus::NeedsReview
        ],
        "a justified selection survives; an unexplained rejection does not erase footage"
    );
}

#[test]
fn sam_video_evidence_refines_ranges_without_changing_scene_inventory() {
    let source = Footage::new(6.0, 4, false, false, "mp4");
    let api = Api::new(|request| match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/chat/completions" => {
            let windows = input_windows(&request.json());
            let measured = windows[0]["subjectEvidence"]
                .as_array()
                .is_some_and(|v| !v.is_empty());
            if measured {
                let evidence = &windows[0]["subjectEvidence"][0];
                assert_eq!(evidence["status"], "ready");
                assert_eq!(evidence["concept"], "person");
                completion(vec![
                    json!({"window":0, "reason":"Keep the visible subject.",
                    "ranges":[range(1.0, 5.0, "action")], "subjectChecks":[]}),
                ])
            } else {
                completion(vec![
                    json!({"window":0, "reason":"Check the subject's usable span.",
                    "ranges":[range(0.0, 6.0, "action")],
                    "subjectChecks":[{"concept":"person", "start":0.0, "end":6.0,
                        "reason":"Locate the subject before choosing the in and out points."}]}),
                ])
            }
        }
        "/v1/responses" => {
            let body = request.json();
            assert_eq!(body["model"], "sam-3.1");
            assert!(body["input"][0]["content"][1]["video_url"]
                .as_str()
                .unwrap()
                .starts_with("data:video/mp4;base64,"));
            let text: String = (4..20).map(|frame| format!("<{frame}f>0<|box;x1=16;y1=4;x2=47;y2=31;w=64;h=36;c=0.9|><|mask;x=0;y=0;data=5,5,!!!!!(QO(0lu8?|>\n")).collect();
            sam_completion(&text)
        }
        other => panic!("unexpected request {other}"),
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 6.0);
    assert_eq!(data.candidates.len(), 1);
    let clip = &data.candidates[0];
    assert_eq!((clip.source_in, clip.end()), (1.0, 5.0));
    assert_eq!(clip.review.status, ReviewStatus::Reviewed);
    assert_eq!(
        clip.score, 68.0,
        "SAM geometry must not add arbitrary quality points"
    );
    assert_eq!(
        clip.review.json(clip.source_in, clip.end())["subjectEvidence"][0]["concept"],
        "person"
    );
    assert_eq!(data.usage.json()["sam"]["reportedBilledFrames"], 50);
}

#[test]
fn subject_failures_preserve_material_and_empty_detections_do_not_reject_it() {
    let source = Footage::new(2.0, 4, false, false, "mp4");
    for case in [
        "empty",
        "vehicle",
        "partial",
        "conflict",
        "bad-record",
        "bad-frame",
        "duplicate-frame",
        "http-error",
        "bad-refinement",
        "another-pass",
        "check-budget",
        "partial-then-face",
        "face-then-partial",
    ] {
        let api = Api::new(move |request| match request.path.as_str() {
            "/v1/models" => catalog(),
            "/v1/chat/completions" => {
                let windows = input_windows(&request.json());
                let window = &windows[0];
                let mut review = whole_window(window);
                if window.get("subjectEvidence").is_none() {
                    review["subjectChecks"] = json!([{"concept":"vehicle", "start":0.0, "end":2.0,
                        "reason":"Check whether the vehicle reveal is complete."}]);
                    if case == "check-budget" {
                        let check = review["subjectChecks"][0].clone();
                        review["subjectChecks"] = json!([check, check, check]);
                    } else if matches!(case, "partial-then-face" | "face-then-partial") {
                        let mut face = review["subjectChecks"][0].clone();
                        face["concept"] = json!("face");
                        let checks = review["subjectChecks"].as_array_mut().unwrap();
                        if case == "partial-then-face" {
                            checks.push(face);
                        } else {
                            checks.insert(0, face);
                        }
                    }
                } else {
                    let evidence = &window["subjectEvidence"][0];
                    assert_eq!(evidence["concept"], "vehicle");
                    assert_eq!(evidence["status"], "ready");
                    if case == "empty" {
                        assert_eq!(evidence["observedFrames"], 0);
                    }
                    review["subjectChecks"] = json!([]);
                    if case == "bad-refinement" {
                        review["ranges"][0]["end"] = json!(3.0);
                    }
                    if case == "another-pass" {
                        review["subjectChecks"] = json!([{"concept":"person"}]);
                    }
                }
                completion(vec![review])
            }
            "/v1/responses" => {
                let body = request.json();
                let concept = body["input"][0]["content"][0]["text"].as_str().unwrap();
                assert!(
                    concept == "vehicle"
                        || (concept == "face"
                            && matches!(case, "partial-then-face" | "face-then-partial"))
                );
                let record = "12<|mask;x=0;y=0;data=5,5,!!!!!(QO(0lu8?|><|box;x1=1;y1=2;x2=30;y2=32;w=64;h=36|>";
                let text = match case {
                    "empty" => String::new(),
                    "bad-record" => "<0f>garbage".to_owned(),
                    "bad-frame" => format!("<8f>{record}\n"),
                    "duplicate-frame" => format!("<1f>{record}\n<1f>{record}\n"),
                    _ if concept == "face" => format!("<4f>{record}\n"),
                    _ => format!("<7f>{record}\n<0f>{record}\n"),
                };
                let (status, mut body) = sam_completion(&text);
                match case {
                    "partial" => body.truncate(body.find("data: {\"response\"").unwrap()),
                    "partial-then-face" | "face-then-partial" if concept == "vehicle" => {
                        body.truncate(body.find("data: {\"response\"").unwrap())
                    }
                    "conflict" => {
                        body = body.replace(
                            &format!("\"text\":{}", json!(text)),
                            "\"text\":\"conflicting final text\"",
                        )
                    }
                    "http-error" => return (401, "private provider detail".to_owned()),
                    "check-budget" => panic!("over-budget checks must fail before a paid request"),
                    _ => {}
                }
                (status, body)
            }
            other => panic!("unexpected request {other}"),
        });
        let data = source.curate(&api.settings);
        api.finish();
        assert_inventory(&data, 1, 2.0);
        assert_eq!(data.candidates.len(), 1, "{case}");
        let clip = &data.candidates[0];
        assert_eq!((clip.source_in, clip.end()), (0.0, 2.0), "{case}");
        let expected = if matches!(case, "empty" | "vehicle") {
            ReviewStatus::Reviewed
        } else {
            ReviewStatus::NeedsReview
        };
        assert_eq!(clip.review.status, expected, "{case}: {}", data.note);
        assert_eq!(data.scene_clips[0].review.status, expected, "{case}");
        assert!(!clip.tags.iter().any(|tag| tag == "prime"), "{case}");
        assert!(!data.note.contains("private provider detail"));
        if matches!(case, "partial-then-face" | "face-then-partial") {
            for clip in [&data.candidates[0], &data.scene_clips[0]] {
                let visual = clip.visual_json();
                assert_eq!(visual["samStatus"], "error", "{case}");
                assert!(visual["samError"].as_str().is_some(), "{case}");
                assert_eq!(
                    visual["facePresent"], true,
                    "valid face evidence is retained"
                );
                let evidence = clip.review.json(clip.source_in, clip.end());
                assert_eq!(evidence["subjectEvidence"].as_array().unwrap().len(), 2);
            }
        }
        if case == "vehicle" {
            let evidence = clip.review.json(clip.source_in, clip.end());
            assert_eq!(
                evidence["subjectEvidence"][0]["tracks"][0]["requestLocalId"],
                "12"
            );
            assert_eq!(evidence["subjectEvidence"][0]["firstObserved"], 0.0);
            assert_eq!(evidence["subjectEvidence"][0]["lastObserved"], 1.75);
        }
    }
}

#[test]
fn comparison_mode_uses_the_same_orientation_without_subject_requests() {
    let source = Footage::new(2.0, 4, false, false, "mp4");
    let api = Api::new(|request| {
        if request.path == "/v1/models" {
            return catalog();
        }
        assert_eq!(request.path, "/v1/chat/completions");
        let windows = input_windows(&request.json());
        let mut review = whole_window(&windows[0]);
        if request.json()["response_format"]["json_schema"]["name"] == "curation_orientation" {
            review["subjectChecks"] = json!([{"concept":"person", "start":0.0, "end":2.0,
                "reason":"Check visibility."}]);
        } else {
            assert_eq!(windows[0]["subjectEvidence"], json!([]));
            review["subjectChecks"] = json!([]);
        }
        completion(vec![review])
    });
    let data = super::run_sweep_with_mode(
        &source.info,
        &source.peaks,
        &api.settings,
        &mut |_| {},
        super::subjects::Mode::Disabled,
    )
    .unwrap();
    api.finish();
    assert_inventory(&data, 1, 2.0);
    assert_eq!(data.candidates[0].review.status, ReviewStatus::Reviewed);
    assert_eq!(data.usage.sam.requests, 0);
    assert_eq!(data.usage.refinements, 0);
}

#[test]
fn timing_review_refines_sam_ranges_and_moves_a_shared_boundary_once() {
    let source = Footage::new(6.0, 4, false, false, "mp4");
    let api = Api::with_timing(
        |request| {
            match request.path.as_str() {
            "/v1/models" => catalog(),
            "/v1/chat/completions" => {
                let windows = input_windows(&request.json());
                if windows[0].get("subjectEvidence").is_some() {
                    completion(vec![json!({"window":0, "reason":"Two distinct visual beats.",
                        "ranges":[range(1.0,3.0,"broll"),range(3.0,5.0,"action")],
                        "subjectChecks":[]})])
                } else {
                    completion(vec![json!({"window":0, "reason":"Check the visible subject.",
                        "ranges":[range(0.0,6.0,"broll")], "subjectChecks":[{
                            "concept":"person", "start":0.0, "end":6.0,
                            "reason":"Resolve two useful beats before choosing trims."}]})])
                }
            }
            "/v1/responses" => sam_completion("<0f>0<|box;x1=8;y1=4;x2=55;y2=31;w=64;h=36|><|mask;x=0;y=0;data=5,5,!!!!!(QO(0lu8?|>\n"),
            other => panic!("unexpected request {other}"),
        }
        },
        |request| {
            timing_completion(request, |boundary| {
                match boundary["kind"].as_str().unwrap() {
                    "in" => 0.5,
                    "split" => 3.25,
                    "out" => 5.5,
                    other => panic!("unexpected boundary {other}"),
                }
            })
        },
    );
    let data = source.curate(&api.settings);
    let requests = api.finish();
    assert_inventory(&data, 1, 6.0);
    let mut clips: Vec<_> = data.candidates.iter().collect();
    clips.sort_by(|a, b| a.source_in.total_cmp(&b.source_in));
    assert_eq!(
        clips
            .iter()
            .map(|c| (c.source_in, c.end(), c.category.as_str()))
            .collect::<Vec<_>>(),
        [(0.5, 3.25, "broll"), (3.25, 5.5, "action")],
        "timing follows SAM; a shared split has no gap and the last chosen frame is included: {}",
        data.note
    );
    for clip in clips {
        assert_eq!(clip.review.status, ReviewStatus::Reviewed, "{}", data.note);
        assert_eq!(clip.score, 68.0);
        assert!(clip.review.subject_check);
        assert_eq!(clip.review.subject_evidence.len(), 1);
    }
    for request in requests
        .iter()
        .filter(|r| r.path.ends_with("/chat/completions"))
    {
        let body = request.json();
        let images = body["messages"][1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|part| part["type"] == "image_url")
            .count();
        assert!((1..=40).contains(&images));
    }
}

#[test]
fn timing_review_failure_keeps_validated_ranges_with_unverified_edges() {
    let source = Footage::new(2.0, 4, false, false, "mp4");
    for case in [
        "missing",
        "duplicate",
        "outside",
        "foreign-boundary",
        "length",
        "http-error",
    ] {
        let api = Api::with_timing(
            |request| {
                if request.path == "/v1/models" {
                    return catalog();
                }
                completion(vec![json!({"window":0, "reason":"Fixture interior range.",
                    "ranges":[range(0.75,1.25,"broll")], "subjectChecks":[]})])
            },
            move |request| {
                if case == "http-error" {
                    return (401, "private provider detail".to_owned());
                }
                let (status, body) =
                    timing_completion(request, |boundary| boundary["proposed"].as_f64().unwrap());
                let mut response: Value = serde_json::from_str(&body).unwrap();
                let mut content: Value = serde_json::from_str(
                    response["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                let decisions = content["boundaries"].as_array_mut().unwrap();
                match case {
                    "missing" => {
                        decisions.pop();
                    }
                    "duplicate" => decisions[1] = decisions[0].clone(),
                    "outside" => decisions.last_mut().unwrap()["choice"] = json!(999_999),
                    "foreign-boundary" => decisions[0]["boundary"] = json!(999_999),
                    _ => {}
                }
                response["choices"][0]["message"]["content"] = json!(content.to_string());
                if case == "length" {
                    response["choices"][0]["finish_reason"] = json!("length");
                    response["choices"][0]["message"]["content"] = Value::Null;
                }
                (status, response.to_string())
            },
        );
        let data = source.curate(&api.settings);
        api.finish();
        assert_inventory(&data, 1, 2.0);
        assert_eq!(data.candidates.len(), 1, "{case}");
        let clip = &data.candidates[0];
        assert_eq!(
            (clip.source_in, clip.end(), clip.review.status),
            (0.75, 1.25, ReviewStatus::Reviewed),
            "{case}: a valid review keeps its proposed edges: {}",
            data.note
        );
        let review = clip.review_json();
        assert!(
            review["edgeIssue"]
                .as_str()
                .is_some_and(|issue| issue.contains("not frame-verified")),
            "{case}: {review}"
        );
        // Request-level failures report their own error; single bad decisions
        // report the unverified edge.
        assert!(
            data.note.contains("not frame-verified") || matches!(case, "length" | "http-error"),
            "{case}: {}",
            data.note
        );
        let coverage = data.scene_clips[0].review.json(0.0, 2.0)["sourceCoverage"].clone();
        let spans = coverage.as_array().unwrap();
        assert_eq!(spans[0]["start"], 0.0);
        assert_eq!(spans.last().unwrap()["end"], 2.0);
        assert!(spans.windows(2).all(|p| p[0]["end"] == p[1]["start"]));
        assert!(!data.note.contains("private provider detail"));
    }
}

#[test]
fn timing_resolves_adjacent_pictures_and_allows_one_wider_inspection() {
    for case in ["adjacent", "wider", "still-outside"] {
        let source = Footage::new(
            5.0,
            if case == "adjacent" { 30 } else { 4 },
            false,
            false,
            "mp4",
        );
        let api = Api::with_timing(
            |request| {
                if request.path == "/v1/models" {
                    return catalog();
                }
                completion(vec![
                    json!({"window":0,"ranges":[range(1.0,2.0,"broll")],"subjectChecks":[]}),
                ])
            },
            move |request| {
                let phase = input_windows(&request.json())[0]["phase"]
                    .as_str()
                    .unwrap_or("coarse")
                    .to_owned();
                let (status, text) = timing_completion(request, |boundary| {
                    if case == "adjacent" && phase == "adjacent" {
                        if boundary["kind"] == "in" {
                            31.0 / 30.0
                        } else {
                            61.0 / 30.0
                        }
                    } else if case != "adjacent" && phase == "wider" && boundary["kind"] == "out" {
                        3.25
                    } else {
                        boundary["proposed"].as_f64().unwrap()
                    }
                });
                let mut envelope: Value = serde_json::from_str(&text).unwrap();
                let mut answer: Value = serde_json::from_str(
                    envelope["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                if case != "adjacent" {
                    let metadata = input_windows(&request.json());
                    for decision in answer["boundaries"].as_array_mut().unwrap() {
                        let boundary = metadata[0]["boundaries"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|b| b["boundary"] == decision["boundary"])
                            .unwrap();
                        decision["expand"] = json!(if boundary["kind"] == "out"
                            && (phase == "coarse" || case == "still-outside")
                        {
                            "after"
                        } else {
                            "none"
                        });
                    }
                }
                envelope["choices"][0]["message"]["content"] = json!(answer.to_string());
                (status, envelope.to_string())
            },
        );
        let data = source.curate(&api.settings);
        let requests = api.finish();
        assert_inventory(&data, 1, 5.0);
        assert_eq!(data.candidates.len(), 1, "{case}: {}", data.note);
        let clip = &data.candidates[0];
        let (start, end, status) = match case {
            "adjacent" => (31.0 / 30.0, 61.0 / 30.0, ReviewStatus::Reviewed),
            "wider" => (1.0, 3.25, ReviewStatus::Reviewed),
            // The coarse choices stand; the unsupported wider edge stays unverified.
            _ => (1.0, 2.0, ReviewStatus::Reviewed),
        };
        assert_eq!(clip.review.status, status, "{case}: {}", data.note);
        assert_eq!(
            clip.review_json()["edgeIssue"].is_string(),
            case == "still-outside",
            "{case}"
        );
        assert!(
            (clip.source_in - start).abs() < 1e-9 && (clip.end() - end).abs() < 1e-9,
            "{case}: got [{},{}), expected [{start},{end})",
            clip.source_in,
            clip.end()
        );
        let phases: Vec<_> = requests
            .iter()
            .filter(|r| r.path.ends_with("/chat/completions"))
            .flat_map(|r| input_windows(&r.json()))
            .filter_map(|w| w["phase"].as_str().map(str::to_owned))
            .collect();
        assert!(phases.iter().any(|phase| phase
            == if case == "adjacent" {
                "adjacent"
            } else {
                "wider"
            }));
        assert_eq!(
            phases.iter().filter(|phase| *phase == "wider").count(),
            usize::from(case != "adjacent")
        );
    }
}

#[test]
fn timing_choices_keep_neighboring_ranges_ordered_and_preserve_short_moments() {
    let source = Footage::new(6.0, 4, false, false, "mp4");
    for narrow in [false, true] {
        let api = Api::with_timing(
            move |request| {
                if request.path == "/v1/models" {
                    return catalog();
                }
                let ranges = if narrow {
                    vec![range(2.75, 3.0, "action")]
                } else {
                    vec![range(0.5, 2.0, "broll"), range(3.0, 5.5, "action")]
                };
                completion(vec![
                    json!({"window":0, "ranges":ranges, "subjectChecks":[]}),
                ])
            },
            move |request| {
                timing_completion(request, |boundary| {
                    let choices = boundary["choices"].as_array().unwrap();
                    let proposed = boundary["proposed"].as_f64().unwrap();
                    if (narrow && boundary["kind"] == "in") || (!narrow && proposed == 2.0) {
                        choices.last().unwrap()["time"].as_f64().unwrap()
                    } else if (narrow && boundary["kind"] == "out") || (!narrow && proposed == 3.0)
                    {
                        choices[0]["time"].as_f64().unwrap()
                    } else {
                        proposed
                    }
                })
            },
        );
        let data = source.curate(&api.settings);
        api.finish();
        assert_inventory(&data, 1, 6.0);
        let mut clips: Vec<_> = data.candidates.iter().collect();
        clips.sort_by(|a, b| a.source_in.total_cmp(&b.source_in));
        let expected = if narrow {
            vec![(2.75, 3.0)]
        } else {
            vec![(0.5, 2.5), (2.5, 5.5)]
        };
        assert_eq!(
            clips
                .iter()
                .map(|clip| (clip.source_in, clip.end()))
                .collect::<Vec<_>>(),
            expected,
            "valid frame choices cannot cross or erase an accepted moment: {}",
            data.note
        );
        assert!(clips
            .iter()
            .all(|clip| clip.review.status == ReviewStatus::Reviewed));
    }
}

#[test]
fn timing_review_retains_every_interior_moment_across_bounded_batches() {
    let source = Footage::new(12.0, 8, false, false, "mp4");
    let api = Api::new(|request| {
        if request.path == "/v1/models" {
            return catalog();
        }
        let ranges: Vec<_> = (0..18)
            .map(|index| {
                range(
                    index as f64 * 0.5 + 0.125,
                    index as f64 * 0.5 + 0.375,
                    "broll",
                )
            })
            .collect();
        completion(vec![
            json!({"window":0, "ranges":ranges, "subjectChecks":[]}),
        ])
    });
    let data = source.curate(&api.settings);
    let requests = api.finish();
    assert_inventory(&data, 1, 12.0);
    let mut clips: Vec<_> = data.candidates.iter().collect();
    clips.sort_by(|a, b| a.source_in.total_cmp(&b.source_in));
    assert_eq!(clips.len(), 18, "{}", data.note);
    for (index, clip) in clips.into_iter().enumerate() {
        assert_eq!(clip.source_in, index as f64 * 0.5 + 0.125);
        assert_eq!(clip.duration, 0.25);
        assert_eq!(clip.review.status, ReviewStatus::Reviewed);
    }
    assert_eq!(data.usage.json()["timingBoundaries"], 36);
    for request in requests
        .iter()
        .filter(|r| r.path.ends_with("/chat/completions"))
    {
        let body = request.json();
        let images = body["messages"][1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|part| part["type"] == "image_url")
            .count();
        assert!((1..=40).contains(&images));
    }
}

#[test]
fn timing_followups_keep_distant_moments_in_separate_native_batches() {
    let source = Footage::new(65.0, 30, false, false, "mp4");
    let api =
        Api::with_timing(
            |request| {
                if request.path == "/v1/models" {
                    return catalog();
                }
                completion(input_windows(&request.json()).iter().map(|window| {
                let ranges = match window["window"].as_u64().unwrap() {
                    0 => vec![range(1.0, 3.0, "broll")],
                    1 => Vec::new(),
                    2 => vec![range(60.0, 63.0, "action")],
                    other => panic!("unexpected window {other}"),
                };
                json!({"window":window["window"], "reason":"Two distant useful moments.",
                    "ranges":ranges, "subjectChecks":[]})
            }).collect())
            },
            |request| {
                let (status, text) =
                    timing_completion(request, |boundary| boundary["proposed"].as_f64().unwrap());
                let mut response: Value = serde_json::from_str(&text).unwrap();
                let mut answer: Value = serde_json::from_str(
                    response["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                if input_windows(&request.json())[0]["phase"] == "coarse" {
                    for decision in answer["boundaries"].as_array_mut().unwrap() {
                        if decision["boundary"] == 0 {
                            decision["expand"] = json!("before");
                        }
                    }
                }
                response["choices"][0]["message"]["content"] = json!(answer.to_string());
                (status, response.to_string())
            },
        );
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 1, 65.0);
    let mut clips: Vec<_> = data.candidates.iter().collect();
    clips.sort_by(|a, b| a.source_in.total_cmp(&b.source_in));
    assert_eq!(clips.len(), 2, "{}", data.note);
    assert_eq!((clips[0].source_in, clips[0].end()), (1.0, 3.0));
    assert_eq!((clips[1].source_in, clips[1].end()), (60.0, 63.0));
    assert!(clips
        .iter()
        .all(|clip| clip.review.status == ReviewStatus::Reviewed));
}

#[test]
fn fractional_rate_review_windows_do_not_accumulate_source_grid_drift() {
    let source = Footage::new(71.98, "24000/1001", false, false, "mp4");
    let api = Api::new(|request| {
        if request.path == "/v1/models" {
            return catalog();
        }
        completion(
            input_windows(&request.json())
                .iter()
                .map(|window| {
                    let mut review = whole_window(window);
                    review["ranges"][0]["continuesBefore"] = json!(true);
                    review["ranges"][0]["continuesAfter"] = json!(true);
                    review
                })
                .collect(),
        )
    });
    let data = source.curate(&api.settings);
    let requests = api.finish();
    assert_inventory(&data, 1, source.info.duration);
    let reviews: Vec<_> = requests
        .iter()
        .filter(|request| {
            request.path == "/v1/chat/completions"
                && request.json()["response_format"]["json_schema"]["name"] == "curation"
        })
        .map(|request| request.json())
        .collect();
    for review in &reviews {
        // A live three-window review spent 7,781 of 8,192 tokens reasoning
        // and was truncated. The allowance must grow with the batch.
        let windows = input_windows(review).len() as u64;
        assert!(
            review["max_completion_tokens"].as_u64().unwrap() >= (8192 * (windows + 1)).min(32768),
            "{windows} windows: {}",
            review["max_completion_tokens"]
        );
    }
    let reviewed: Vec<_> = reviews.iter().flat_map(input_windows).collect();
    assert_eq!(
        reviewed.len(),
        3,
        "rounding must not create a fourth one-frame review"
    );
    assert_eq!(data.candidates.len(), 1);
    assert_eq!(data.candidates[0].review.status, ReviewStatus::Reviewed);
    assert!((data.candidates[0].duration - source.info.duration).abs() < 1e-9);
}

#[test]
fn visual_review_selects_interior_ranges_and_joins_a_long_usable_moment() {
    let source = Footage::new(65.0, 4, false, false, "mp4");
    let api = Api::new(|request| {
        if request.path == "/v1/models" {
            return catalog();
        }
        assert_eq!(request.path, "/v1/chat/completions");
        completion(input_windows(&request.json()).iter().map(|window| {
            let ranges = match window["window"].as_u64().unwrap() {
                0 => {
                    let mut ongoing = range(18.0, 24.0, "action");
                    ongoing["continuesAfter"] = json!(true);
                    vec![range(2.13, 6.87, "broll"), ongoing]
                }
                1 => {
                    let mut ongoing = range(24.0, 48.0, "action");
                    ongoing["continuesBefore"] = json!(true);
                    ongoing["continuesAfter"] = json!(true);
                    vec![ongoing]
                }
                2 => {
                    let mut ongoing = range(48.0, 57.5, "action");
                    ongoing["continuesBefore"] = json!(true);
                    vec![ongoing, range(60.0, 63.0, "broll")]
                }
                other => panic!("unexpected window {other}"),
            };
            json!({"window":window["window"], "reason":"Fixture trimmed lead-in and tail.", "ranges":ranges})
        }).collect())
    });
    let data = source.curate(&api.settings);
    let requests = api.finish();
    assert_inventory(&data, 1, 65.0);
    let mut actual: Vec<_> = data
        .candidates
        .iter()
        .map(|clip| (clip.source_in, clip.end(), clip.category.as_str()))
        .collect();
    actual.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert_eq!(
        actual,
        [
            (2.25, 6.75, "broll"),
            (18.0, 57.5, "action"),
            (60.0, 63.0, "broll")
        ]
    );
    assert!(data
        .candidates
        .iter()
        .all(|clip| clip.review.status == ReviewStatus::Reviewed));
    let joined = data
        .candidates
        .iter()
        .find(|clip| clip.source_in == 18.0)
        .unwrap();
    assert!(joined.review.frame_times.iter().any(|time| *time >= 48.0));
    assert!(joined
        .review
        .frame_times
        .iter()
        .all(|time| (time * 4.0).fract().abs() < 1e-8));
    let posted: Vec<_> = requests
        .iter()
        .filter(|request| request.path.ends_with("/chat/completions"))
        .collect();
    assert_eq!(data.usage.batches, posted.len());
    assert_eq!(data.usage.input_tokens, posted.len() as u64 * 100);
    assert_eq!(data.usage.output_tokens, posted.len() as u64 * 30);
    assert_eq!(data.usage.cached_tokens, posted.len() as u64 * 20);
    for request in posted {
        let body = request.json();
        assert_eq!(body["model"], MODEL_SPARK);
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        let content = body["messages"][1]["content"].as_array().unwrap();
        let images = content
            .iter()
            .filter(|part| part["type"] == "image_url")
            .count();
        assert!((1..=50).contains(&images));
        assert!(content
            .iter()
            .filter(|part| part["type"] == "image_url")
            .all(|part| part["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,/9j/")));
        assert!(request
            .headers
            .to_ascii_lowercase()
            .contains("authorization: bearer curation-fixture-key"));
    }
}

#[test]
fn conflicting_window_seams_are_resolved_before_source_frame_timing() {
    let source = Footage::new(52.0, 4, false, false, "mp4");
    for decision in ["join", "separate", "invalid"] {
        let api = Api::with_timing(
            move |request| {
                if request.path == "/v1/models" {
                    return catalog();
                }
                let body = request.json();
                let windows = input_windows(&body);
                if body["response_format"]["json_schema"]["name"] == "curation_seam" {
                    return (200,json!({"choices":[{"finish_reason":"stop","message":{"content":json!({
                    "decision":decision,"category":"action","reason":"Fixture judges the shared-context beat."
                }).to_string()}}],"usage":{"prompt_tokens":100,"completion_tokens":30}}).to_string());
                }
                completion(windows.iter().map(|window| {
                let (start,end,category) = match window["window"].as_u64().unwrap() {
                    0 => (1.0,24.0,"action"),
                    1 => (24.0,48.0,"broll"),
                    2 => (48.0,51.0,"action"),
                    _ => unreachable!(),
                };
                json!({"window":window["window"],"ranges":[range(start,end,category)],"subjectChecks":[]})
            }).collect())
            },
            move |request| {
                timing_completion(request, |boundary| {
                    let proposed = boundary["proposed"].as_f64().unwrap();
                    if decision == "separate" && boundary["kind"] == "split" {
                        proposed + 0.25
                    } else {
                        proposed
                    }
                })
            },
        );
        let data = source.curate(&api.settings);
        let requests = api.finish();
        assert_inventory(&data, 1, 52.0);
        let mut actual: Vec<_> = data
            .candidates
            .iter()
            .map(|c| (c.source_in, c.end(), c.review.status))
            .collect();
        actual.sort_by(|a, b| a.0.total_cmp(&b.0));
        let expected = match decision {
            "join" => vec![(1.0, 51.0, ReviewStatus::Reviewed)],
            "separate" => vec![
                (1.0, 24.25, ReviewStatus::Reviewed),
                (24.25, 48.25, ReviewStatus::Reviewed),
                (48.25, 51.0, ReviewStatus::Reviewed),
            ],
            // An invalid seam keeps both validated beats unreconciled.
            _ => vec![
                (1.0, 24.0, ReviewStatus::Reviewed),
                (24.0, 48.0, ReviewStatus::Reviewed),
                (48.0, 51.0, ReviewStatus::Reviewed),
            ],
        };
        assert_eq!(actual, expected, "{decision}: {}", data.note);
        let flagged = data
            .candidates
            .iter()
            .filter(|c| {
                c.review_json()["edgeIssue"]
                    .as_str()
                    .is_some_and(|issue| issue.contains("analysis seam"))
            })
            .count();
        assert_eq!(flagged, if decision == "invalid" { 3 } else { 0 });
        let stages: Vec<_> = requests
            .iter()
            .filter(|r| r.path.ends_with("/chat/completions"))
            .map(|r| {
                r.json()["response_format"]["json_schema"]["name"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert!(stages.iter().any(|s| s == "curation_seam"));
        if decision != "invalid" {
            assert!(
                stages.iter().rposition(|s| s == "curation_seam").unwrap()
                    < stages.iter().position(|s| s == "curation_timing").unwrap()
            );
        }
    }
}

#[test]
fn visual_review_keeps_every_short_shot_and_batches_without_merging_shots() {
    for (seconds, fps, frames) in [(4.25, 4, 17), (1.0, 30, 30), (1.0, 60, 60)] {
        let source = Footage::new(seconds, fps, true, false, "mp4");
        let api = Api::new(|request| {
            if request.path == "/v1/models" {
                return catalog();
            }
            assert_eq!(request.path, "/v1/chat/completions");
            completion(
                input_windows(&request.json())
                    .iter()
                    .map(|window| {
                        let mut review = whole_window(window);
                        review["ranges"][0]["continuesBefore"] = json!(true);
                        review["ranges"][0]["continuesAfter"] = json!(true);
                        review
                    })
                    .collect(),
            )
        });
        let data = source.curate(&api.settings);
        let requests = api.finish();
        assert_inventory(&data, frames, seconds);
        assert_eq!(data.candidates.len(), frames);
        assert!(data.usage.batches > 1);
        let mut starts: Vec<_> = data.candidates.iter().map(|clip| clip.source_in).collect();
        starts.sort_by(f64::total_cmp);
        for (frame, start) in starts.iter().enumerate() {
            assert!((start - frame as f64 / fps as f64).abs() < 1e-9);
        }
        for clip in &data.candidates {
            assert_eq!(
                clip.review.status,
                ReviewStatus::Reviewed,
                "{fps} fps, source {}: {}",
                clip.source_in,
                data.note,
            );
            assert!((clip.duration - 1.0 / fps as f64).abs() < 1e-9);
            assert_eq!(clip.category, "action");
            assert_eq!(clip.review.frame_times, vec![clip.source_in]);
        }
        assert_eq!(
            data.usage.images,
            frames * 2,
            "orientation and review each see every one-frame shot"
        );
        assert!(requests
            .iter()
            .all(|request| !request.path.ends_with("/responses")));
    }
}

#[test]
fn invalid_or_missing_reviews_preserve_material_but_explicit_rejections_do_not() {
    let source = Footage::new(3.75, 4, true, false, "mp4");
    let api = Api::new(|request| {
        if request.path == "/v1/models" {
            return catalog();
        }
        let mut reviews = Vec::new();
        for window in input_windows(&request.json()) {
            let id = window["window"].as_u64().unwrap();
            let start = window["start"].as_f64().unwrap();
            let end = window["end"].as_f64().unwrap();
            let mut review = whole_window(&window);
            match id {
                1 => continue,
                2 => reviews.push(review.clone()),
                3 => review["ranges"][0]["start"] = json!(start - 0.25),
                4 => {
                    review["ranges"] = json!([
                        range(start, start + 0.2, "action"),
                        range(start + 0.1, end, "action")
                    ])
                }
                5 => review["ranges"] = json!([]),
                6 => review["ranges"][0]["category"] = json!("prime"),
                7 => review["ranges"][0]["score"] = json!(101),
                8 => review["ranges"][0]["end"] = json!(start - 1.0),
                9 => {
                    review["ranges"][0]["tags"] =
                        json!(["unknown", "prime", "face-visible", "steady"])
                }
                10 => review["ranges"] = json!([range(start + 0.01, start + 0.02, "action")]),
                11 => review["ranges"][0]["reason"] = json!(""),
                12 => {
                    review["ranges"][0]["continuesBefore"] = json!("true");
                }
                13 => review["ranges"][0]["start"] = json!(start - 0.00001),
                14 => review["ranges"][0]["end"] = json!(end + 0.00001),
                _ => {}
            }
            reviews.push(review);
        }
        completion(reviews)
    });
    let data = source.curate(&api.settings);
    api.finish();
    assert_inventory(&data, 15, 3.75);
    assert_eq!(data.candidates.len(), 14);
    for (frame, scene) in data.scene_clips.iter().enumerate() {
        let expected = match frame {
            0 | 9 => ReviewStatus::Reviewed,
            5 => ReviewStatus::Rejected,
            _ => ReviewStatus::NeedsReview,
        };
        assert_eq!(scene.review.status, expected, "source frame {frame}");
        let candidate = data
            .candidates
            .iter()
            .find(|clip| clip.source_in == frame as f64 / 4.0);
        if frame == 5 {
            assert!(
                candidate.is_none(),
                "explicitly rejected material stays only in All scenes"
            );
        } else {
            let candidate = candidate.unwrap();
            assert_eq!(candidate.review.status, expected);
            assert_eq!(candidate.duration, 0.25);
        }
    }
    let valid = data
        .candidates
        .iter()
        .find(|clip| clip.source_in == 2.25)
        .unwrap();
    assert_eq!(valid.tags, ["steady"]);
}

#[test]
fn provider_failure_stops_new_batches_without_discarding_completed_reviews() {
    let source = Footage::new(20.0, 4, true, false, "mp4");
    let mut calls = 0;
    let api = Api::new(move |request| {
        if request.path == "/v1/models" {
            return catalog();
        }
        if request.json()["response_format"]["json_schema"]["name"] == "curation_orientation" {
            return completion(
                input_windows(&request.json())
                    .iter()
                    .map(whole_window)
                    .collect(),
            );
        }
        calls += 1;
        if calls == 2 {
            return (
                401,
                json!({"error":{"message":"private provider detail"}}).to_string(),
            );
        }
        assert_eq!(calls, 1, "stop after a provider failure");
        completion(
            input_windows(&request.json())
                .iter()
                .map(whole_window)
                .collect(),
        )
    });
    let data = source.curate(&api.settings);
    let requests = api.finish();
    assert_inventory(&data, 80, 20.0);
    assert_eq!(data.candidates.len(), 80);
    let posted: Vec<_> = requests
        .iter()
        .filter(|request| {
            request.path.ends_with("/chat/completions")
                && request.json()["response_format"]["json_schema"]["name"] == "curation"
        })
        .collect();
    assert_eq!(posted.len(), 2);
    let accepted = input_windows(&posted[0].json()).len();
    assert_eq!(
        data.candidates
            .iter()
            .filter(|clip| clip.review.status == ReviewStatus::Reviewed)
            .count(),
        accepted
    );
    assert_eq!(
        data.candidates
            .iter()
            .filter(|clip| clip.review.status == ReviewStatus::NeedsReview)
            .count(),
        80 - accepted
    );
    assert!(!data.note.contains("private provider detail"));

    let api = Api::new(|request| {
        assert_eq!(
            request.path, "/v1/models",
            "bad authentication must stop before paid requests"
        );
        (401, "{}".to_owned())
    });
    let data = source.curate(&api.settings);
    assert_eq!(api.finish().len(), 1);
    assert_eq!(data.usage.batches, 0);
    assert_inventory(&data, 80, 20.0);
    assert_eq!(data.candidates.len(), 80);
    assert!(data
        .candidates
        .iter()
        .all(|clip| clip.review.status == ReviewStatus::NeedsReview));
}

#[test]
fn audio_transcription_and_requested_subject_checks_use_the_same_model_configuration() {
    let source = Footage::new(6.0, 4, false, true, "mp4");
    let api = Api::new(|request| match request.path.as_str() {
        "/v1/models" => catalog(),
        "/v1/asr/transcribe" => {
            let body = String::from_utf8_lossy(&request.body);
            assert!(body.contains("muse-voice-transcribe-1.0"));
            assert!(body.contains("WAVE"), "real extracted audio is uploaded");
            (200, json!({"turns":[{"startMs":500,"endMs":2500,"transcript":"A complete spoken thought.","speaker":"A"}]}).to_string())
        }
        "/v1/chat/completions" => {
            let windows = input_windows(&request.json());
            assert_eq!(windows.len(), 1);
            assert_eq!(
                windows[0]["transcript"][0]["text"],
                "A complete spoken thought."
            );
            let mut talking = range(0.0, 3.0, "talking");
            talking["tags"] = json!(["speech", "music-bed"]);
            talking["subjectCheck"] = json!(true);
            let mut bed = range(3.0, 6.0, "broll");
            bed["tags"] = json!(["music-bed"]);
            completion(vec![
                json!({"window":0, "reason":"Fixture speech and soundtrack ranges.", "ranges":[talking,bed]}),
            ])
        }
        "/v1/responses" => {
            let body = request.json();
            assert_eq!(body["model"], "sam-3.1");
            assert!(body["input"][0]["content"][1]["video_url"]
                .as_str()
                .unwrap()
                .starts_with("data:video/mp4;base64,"));
            let concept = body["input"][0]["content"][0]["text"].as_str().unwrap();
            assert!(["person", "face"].contains(&concept));
            let record = if concept == "person" {
                "0<|box;x1=16;y1=3;x2=47;y2=32;w=64;h=36;c=0.9|>"
            } else {
                "0<|box;x1=28;y1=6;x2=35;y2=13;w=64;h=36;c=0.95|>"
            };
            let box_text: String = (0..12)
                .map(|frame| {
                    format!("<{frame}f>{record}<|mask;x=0;y=0;data=5,5,!!!!!(QO(0lu8?|>\n")
                })
                .collect();
            sam_completion(&box_text)
        }
        other => panic!("unexpected request {other}"),
    });
    let data = source.curate(&api.settings);
    let requests = api.finish();
    assert_inventory(&data, 1, 6.0);
    assert_eq!(data.candidates.len(), 2);
    assert_eq!(data.transcript.len(), 1);
    let speech = data
        .candidates
        .iter()
        .find(|clip| clip.source_in == 0.0)
        .unwrap();
    let bed = data
        .candidates
        .iter()
        .find(|clip| clip.source_in == 3.0)
        .unwrap();
    assert!(
        !speech.music_bed,
        "transcribed speech must not be muted by a coverage heuristic"
    );
    assert!(bed.music_bed);
    assert!(
        !data.scene_clips[0].music_bed,
        "the whole shot contains speech"
    );
    assert_eq!(speech.visual.as_ref().unwrap().face_present, Some(true));
    assert!(speech.tags.iter().any(|tag| tag == "face-visible"));
    assert_eq!(bed.visual.as_ref().unwrap().face_present, None);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.path.ends_with("/responses"))
            .count(),
        2
    );
    assert!(requests.iter().all(|request| request
        .headers
        .to_ascii_lowercase()
        .contains("authorization: bearer curation-fixture-key")));
}

#[test]
fn visual_curation_decodes_mov_mkv_and_webm_without_changing_source_times() {
    for extension in ["mov", "mkv", "webm"] {
        let source = Footage::new(2.25, 4, true, false, extension);
        let api = Api::new(|request| {
            if request.path == "/v1/models" {
                return catalog();
            }
            completion(
                input_windows(&request.json())
                    .iter()
                    .map(whole_window)
                    .collect(),
            )
        });
        let data = source.curate(&api.settings);
        api.finish();
        assert_inventory(&data, 9, 2.25);
        assert_eq!(data.candidates.len(), 9, "{extension}");
        assert!(
            data.candidates
                .iter()
                .all(|clip| clip.duration == 0.25 && clip.review.status == ReviewStatus::Reviewed),
            "{extension}"
        );
    }
}

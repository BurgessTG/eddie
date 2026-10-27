//! Timeline export: builds one `filter_complex` graph so the exported file
//! is composed exactly like the timeline — chopped segments in place,
//! titles burned in, audio mixed at clip offsets.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use openreel_engine::{Clip, ClipKind, Timeline};

use crate::{scaled_height, MediaInfo, MediaResult};

fn f(value: f64) -> String {
    format!("{value:.3}")
}

/// Escape user text for drawtext's `text=` value.
fn escape_drawtext(text: &str) -> String {
    text.replace('\\', "/")
        .replace('\'', "’")
        .replace(':', " -")
        .replace('%', "pct")
        .replace('\n', " ")
}

fn font_file() -> Option<String> {
    if let Ok(out) = Command::new("fc-match")
        .args(["-f", "%{file}", "sans"])
        .output()
    {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if Path::new(&path).exists() {
            return Some(path.replace(':', "\\:"));
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
    .map(|p| p.replace(':', "\\:"))
}

/// Export `timeline` to `output` (H.264/AAC MP4). Sends 0..=1 progress on
/// `progress` as ffmpeg reports it. Blocks — call from a worker thread.
pub fn export_timeline(
    timeline: &Timeline,
    infos: &HashMap<String, MediaInfo>,
    output: &Path,
    progress: Sender<f64>,
) -> MediaResult<PathBuf> {
    let total = timeline.end();
    if total <= 0.0 {
        return Err("timeline is empty".to_owned());
    }
    let fps = timeline.fps;

    let mut video_clips: Vec<&Clip> = timeline
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Video && c.source.is_some())
        .collect();
    video_clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    let title_clips: Vec<&Clip> = timeline
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Title)
        .collect();

    // Canvas from the largest video source, capped at 1080p.
    let (canvas_w, canvas_h) = video_clips
        .iter()
        .filter_map(|c| infos.get(c.source.as_ref()?))
        .max_by_key(|i| i.width * i.height)
        .map(|i| {
            let w = i.width.min(1920);
            let w = w / 2 * 2;
            (w, scaled_height(i.width, i.height, w))
        })
        .unwrap_or((1280, 720));

    // Unique inputs in first-use order.
    let mut inputs: Vec<String> = Vec::new();
    let input_index = |path: &str, inputs: &mut Vec<String>| -> usize {
        if let Some(i) = inputs.iter().position(|p| p == path) {
            i
        } else {
            inputs.push(path.to_owned());
            inputs.len() - 1
        }
    };

    let mut filters: Vec<String> = vec![format!(
        "color=c=black:s={canvas_w}x{canvas_h}:r={}:d={}[base]",
        f(fps),
        f(total)
    )];
    let mut current = "base".to_owned();

    for (i, clip) in video_clips.iter().enumerate() {
        let src = clip.source.as_ref().expect("filtered to source clips");
        let idx = input_index(src, &mut inputs);
        let alpha = if clip.opacity < 0.999 {
            format!(",format=rgba,colorchannelmixer=aa={:.3}", clip.opacity)
        } else {
            String::new()
        };
        filters.push(format!(
            "[{idx}:v]trim=start={}:end={},setpts=PTS-STARTPTS,\
             scale={canvas_w}:{canvas_h}:force_original_aspect_ratio=decrease,\
             pad={canvas_w}:{canvas_h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={}{},\
             setpts=PTS+{}/TB[seg{i}]",
            f(clip.source_in),
            f(clip.source_in + clip.duration),
            f(fps),
            alpha,
            f(clip.start),
        ));
        filters.push(format!(
            "[{current}][seg{i}]overlay=0:0:eof_action=pass[v{i}]"
        ));
        current = format!("v{i}");
    }

    if let Some(font) = font_file() {
        for (i, clip) in title_clips.iter().enumerate() {
            filters.push(format!(
                "[{current}]drawtext=fontfile={font}:text='{}':fontcolor=white@{:.2}:\
                 fontsize=44:box=1:boxcolor=black@0.55:boxborderw=14:x=42:y=h-110:\
                 enable='between(t,{},{})'[t{i}]",
                escape_drawtext(&clip.name),
                clip.opacity.clamp(0.0, 1.0),
                f(clip.start),
                f(clip.end()),
            ));
            current = format!("t{i}");
        }
    }
    filters.push(format!("[{current}]format=yuv420p[vout]"));

    // Audio: standalone audio clips plus embedded audio of video clips.
    let mut audio_labels: Vec<String> = Vec::new();
    let add_audio = |clip: &Clip,
                     inputs: &mut Vec<String>,
                     filters: &mut Vec<String>,
                     labels: &mut Vec<String>| {
        let Some(src) = clip.source.as_ref() else {
            return;
        };
        let idx = if let Some(i) = inputs.iter().position(|p| p == src) {
            i
        } else {
            inputs.push(src.clone());
            inputs.len() - 1
        };
        let label = format!("a{}", labels.len());
        let delay_ms = (clip.start * 1000.0).round() as i64;
        filters.push(format!(
            "[{idx}:a]atrim=start={}:end={},asetpts=PTS-STARTPTS,aresample=48000,\
             volume={:.3},adelay={delay_ms}|{delay_ms}[{label}]",
            f(clip.source_in),
            f(clip.source_in + clip.duration),
            clip.volume,
        ));
        labels.push(label);
    };
    for clip in timeline.clips.iter().filter(|c| c.kind == ClipKind::Audio) {
        add_audio(clip, &mut inputs, &mut filters, &mut audio_labels);
    }
    for clip in &video_clips {
        let has_audio = clip
            .source
            .as_ref()
            .and_then(|s| infos.get(s))
            .map(|i| i.has_audio)
            .unwrap_or(false);
        if has_audio {
            add_audio(clip, &mut inputs, &mut filters, &mut audio_labels);
        }
    }
    match audio_labels.len() {
        0 => filters.push(format!(
            "anullsrc=r=48000:cl=stereo,atrim=0:{}[aout]",
            f(total)
        )),
        1 => filters.push(format!("[{}]anull[aout]", audio_labels[0])),
        n => filters.push(format!(
            "{}amix=inputs={n}:duration=longest:normalize=0[aout]",
            audio_labels
                .iter()
                .map(|l| format!("[{l}]"))
                .collect::<String>()
        )),
    }

    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-v", "error", "-progress", "pipe:1"]);
    for input in &inputs {
        cmd.args(["-i", input]);
    }
    cmd.args([
        "-filter_complex",
        &filters.join(";"),
        "-map",
        "[vout]",
        "-map",
        "[aout]",
        "-t",
        &f(total),
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "20",
        "-c:a",
        "aac",
        "-b:a",
        "160k",
        "-movflags",
        "+faststart",
    ])
    .arg(output)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let stderr = child.stderr.take();
    let stderr_thread = thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut err) = stderr {
            let _ = err.read_to_string(&mut text);
        }
        text
    });

    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(value) = line.strip_prefix("out_time_ms=") {
                if let Ok(us) = value.trim().parse::<f64>() {
                    let _ = progress.send((us / 1_000_000.0 / total).clamp(0.0, 1.0));
                }
            }
        }
    }
    let status = child.wait().map_err(|e| format!("ffmpeg wait: {e}"))?;
    let stderr = stderr_thread.join().unwrap_or_default();
    if status.success() {
        let _ = progress.send(1.0);
        Ok(output.to_path_buf())
    } else {
        let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
        Err(format!(
            "export failed: {}",
            tail.into_iter().rev().collect::<Vec<_>>().join(" ")
        ))
    }
}

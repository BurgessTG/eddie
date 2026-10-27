//! Media engine built on the FFmpeg command-line tools.
//!
//! Rationale: `ffmpeg/ffprobe/ffplay` ship on every target platform and
//! track codec support for us. Frames move as raw RGBA over pipes, which is
//! plenty for proxy-resolution preview. The module boundary is kept narrow
//! so a future `ffmpeg-next`/hardware-decode backend can replace it.

mod decode;
mod export;
mod playback;
mod samples;
mod sweep;

pub use decode::StillDecoder;
pub(crate) use decode::VideoDecoder;
pub use export::export_timeline;
pub use playback::{AudioPlayer, FrameStream};
pub use samples::generate_samples;
pub use sweep::{build_segments, motion_samples, write_wav_24k, MotionSample, SweepSegment};

use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};

#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub path: String,
    pub name: String,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_video: bool,
    pub has_audio: bool,
}

#[derive(Clone, Debug)]
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub type MediaResult<T> = Result<T, String>;

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

fn run(command: &mut Command) -> MediaResult<Output> {
    let output = command
        .output()
        .map_err(|e| format!("failed to launch tool: {e}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(4).collect();
        Err(format!(
            "tool exited with {}: {}",
            output.status,
            tail.into_iter().rev().collect::<Vec<_>>().join(" ")
        ))
    }
}

fn parse_rate(rate: &str) -> f64 {
    let mut parts = rate.split('/');
    let num: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let den: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    if den == 0.0 {
        0.0
    } else {
        num / den
    }
}

/// Probe a media file with ffprobe.
pub fn probe(path: &str) -> MediaResult<MediaInfo> {
    let output = run(Command::new("ffprobe").args([
        "-v",
        "error",
        "-show_entries",
        "stream=codec_type,width,height,avg_frame_rate,r_frame_rate,duration",
        "-show_entries",
        "format=duration",
        "-of",
        "json",
        path,
    ]))?;
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("ffprobe JSON: {e}"))?;

    let mut info = MediaInfo {
        path: path.to_owned(),
        name: Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_owned()),
        duration: json["format"]["duration"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0),
        width: 0,
        height: 0,
        fps: 0.0,
        has_video: false,
        has_audio: false,
    };

    if let Some(streams) = json["streams"].as_array() {
        for stream in streams {
            match stream["codec_type"].as_str() {
                Some("video") if !info.has_video => {
                    info.has_video = true;
                    info.width = stream["width"].as_u64().unwrap_or(0) as u32;
                    info.height = stream["height"].as_u64().unwrap_or(0) as u32;
                    info.fps = parse_rate(stream["avg_frame_rate"].as_str().unwrap_or("0/1"));
                    if info.fps <= 0.0 {
                        info.fps = parse_rate(stream["r_frame_rate"].as_str().unwrap_or("0/1"));
                    }
                }
                Some("audio") => info.has_audio = true,
                _ => {}
            }
            if info.duration <= 0.0 {
                info.duration = stream["duration"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);
            }
        }
    }
    if !info.has_video && !info.has_audio {
        return Err(format!("no audio or video stream in {path}"));
    }
    Ok(info)
}

/// Height for a target width, preserving aspect, always even (encoders and
/// many scalers require even dimensions).
pub fn scaled_height(src_w: u32, src_h: u32, target_w: u32) -> u32 {
    if src_w == 0 || src_h == 0 {
        return target_w * 9 / 16;
    }
    let h = (src_h as f64 * target_w as f64 / src_w as f64).round() as u32;
    (h / 2 * 2).max(2)
}

/// Decode one frame at `time` (seconds), scaled to `target_w`.
/// `fast` trades scaler quality for speed (used while scrubbing).
/// Uses the in-process decoder first, falling back to the FFmpeg CLI for
/// anything libavcodec rejects in this build.
pub fn frame_at(
    path: &str,
    src_w: u32,
    src_h: u32,
    time: f64,
    target_w: u32,
    fast: bool,
) -> MediaResult<RawFrame> {
    match decode::frame_at(path, time, target_w, fast) {
        Ok(frame) if frame.height == scaled_height(src_w, src_h, target_w) => Ok(frame),
        _ => frame_at_cli(path, src_w, src_h, time, target_w, fast),
    }
}

fn frame_at_cli(
    path: &str,
    src_w: u32,
    src_h: u32,
    time: f64,
    target_w: u32,
    fast: bool,
) -> MediaResult<RawFrame> {
    let target_h = scaled_height(src_w, src_h, target_w);
    let flags = if fast { "fast_bilinear" } else { "bicubic" };
    let output = run(Command::new("ffmpeg").args([
        "-v",
        "error",
        "-ss",
        &format!("{time:.3}"),
        "-i",
        path,
        "-frames:v",
        "1",
        "-vf",
        &format!("scale={target_w}:{target_h}:flags={flags}"),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "-",
    ]))?;
    let expected = (target_w * target_h * 4) as usize;
    if output.stdout.len() < expected {
        return Err(format!(
            "short frame from ffmpeg: {} of {expected} bytes",
            output.stdout.len()
        ));
    }
    Ok(RawFrame {
        width: target_w,
        height: target_h,
        rgba: output.stdout[..expected].to_vec(),
    })
}

/// Filmstrip thumbnails: one frame every `interval` seconds (widened so the
/// count stays bounded on long files). Returns `(source_time, frame)`.
pub fn thumbnails(
    path: &str,
    src_w: u32,
    src_h: u32,
    duration: f64,
) -> MediaResult<Vec<(f64, RawFrame)>> {
    let target_w = 144;
    let target_h = scaled_height(src_w, src_h, target_w);
    let interval = (duration / 90.0).max(2.0);
    let frame_bytes = (target_w * target_h * 4) as usize;
    if frame_bytes == 0 {
        return Ok(Vec::new());
    }
    // Stream frames off the pipe instead of buffering the whole strip.
    let mut child = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            path,
            "-vf",
            &format!("fps=1/{interval:.3},scale={target_w}:{target_h}:flags=fast_bilinear"),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ffmpeg stdout unavailable".to_owned())?;
    let mut frames = Vec::new();
    loop {
        let mut rgba = vec![0_u8; frame_bytes];
        if stdout.read_exact(&mut rgba).is_err() {
            break;
        }
        frames.push((
            frames.len() as f64 * interval,
            RawFrame {
                width: target_w,
                height: target_h,
                rgba,
            },
        ));
    }
    let status = child.wait().map_err(|e| format!("ffmpeg wait: {e}"))?;
    if frames.is_empty() && !status.success() {
        return Err("thumbnail decode failed".to_owned());
    }
    Ok(frames)
}

fn accumulate(
    sample: i16,
    per_peak: usize,
    peaks: &mut Vec<f32>,
    current: &mut f32,
    count: &mut usize,
) {
    *current = current.max(sample.abs() as f32 / 32768.0);
    *count += 1;
    if *count >= per_peak {
        peaks.push(*current);
        *current = 0.0;
        *count = 0;
    }
}

/// Peak waveform: one max-amplitude value per `1/peaks_per_sec` seconds.
pub fn waveform_peaks(path: &str, duration: f64, peaks_per_sec: u32) -> MediaResult<Vec<f32>> {
    let sample_rate = 8000_u32;
    // Stream PCM in fixed chunks: peak memory stays constant regardless of
    // file length (the old version buffered the entire decode).
    let mut child = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            path,
            "-ac",
            "1",
            "-ar",
            &sample_rate.to_string(),
            "-f",
            "s16le",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ffmpeg stdout unavailable".to_owned())?;
    let per_peak = (sample_rate / peaks_per_sec.max(1)).max(1) as usize;
    let mut peaks = Vec::with_capacity((duration * peaks_per_sec as f64) as usize + 1);
    let mut current = 0.0_f32;
    let mut count = 0_usize;
    let mut buffer = [0_u8; 64 * 1024];
    let mut carry: Option<u8> = None;
    loop {
        let read = stdout
            .read(&mut buffer)
            .map_err(|e| format!("read ffmpeg stdout: {e}"))?;
        if read == 0 {
            break;
        }
        let mut bytes = &buffer[..read];
        if let Some(first) = carry.take() {
            if !bytes.is_empty() {
                accumulate(
                    i16::from_le_bytes([first, bytes[0]]),
                    per_peak,
                    &mut peaks,
                    &mut current,
                    &mut count,
                );
                bytes = &bytes[1..];
            }
        }
        let mut pairs = bytes.chunks_exact(2);
        for pair in &mut pairs {
            accumulate(
                i16::from_le_bytes([pair[0], pair[1]]),
                per_peak,
                &mut peaks,
                &mut current,
                &mut count,
            );
        }
        carry = pairs.remainder().first().copied();
    }
    if count > 0 {
        peaks.push(current);
    }
    let status = child.wait().map_err(|e| format!("ffmpeg wait: {e}"))?;
    if peaks.is_empty() && !status.success() {
        return Err("waveform decode failed".to_owned());
    }
    Ok(peaks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openreel_engine::{ClipKind, Timeline};
    use std::collections::HashMap;
    use std::sync::mpsc::channel;

    #[test]
    fn samples_probe_decode_and_export_end_to_end() {
        if !ffmpeg_available() {
            panic!("ffmpeg is required for OpenReel media tests");
        }
        let dir = std::env::temp_dir().join(format!("openreel-media-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = generate_samples(&dir).expect("generate samples");
        let video_path = paths[0].to_string_lossy().into_owned();
        let music_path = paths[1].to_string_lossy().into_owned();

        let video = probe(&video_path).expect("probe video");
        assert!(video.has_video && video.has_audio);
        assert_eq!((video.width, video.height), (1280, 720));
        assert!((19.0..21.0).contains(&video.duration), "{}", video.duration);

        let music = probe(&music_path).expect("probe music");
        assert!(music.has_audio && !music.has_video);
        assert!(music.duration > 10.0, "{}", music.duration);

        let peaks = waveform_peaks(&music_path, music.duration, 20).expect("peaks");
        assert!(peaks.len() > 100);
        assert!(peaks.iter().copied().fold(0.0_f32, f32::max) > 0.2);

        let frame =
            frame_at(&video_path, video.width, video.height, 2.0, 320, false).expect("frame");
        assert_eq!((frame.width, frame.height), (320, 180));
        assert_eq!(frame.rgba.len(), 320 * 180 * 4);
        // The public API falls back to the CLI, so exercise the in-process
        // decoder directly to prove that path really works.
        let in_process =
            crate::decode::frame_at(&video_path, 3.0, 320, true).expect("in-process decoded frame");
        assert_eq!((in_process.width, in_process.height), (320, 180));

        let thumbs =
            thumbnails(&video_path, video.width, video.height, video.duration).expect("thumbs");
        assert!(thumbs.len() >= 5, "{}", thumbs.len());

        // Realtime playback pieces: the paced frame stream must deliver
        // correctly sized frames, and the audio player must start (silently).
        let stream = crate::FrameStream::spawn(&video_path, 1280, 720, 1.0, 320, 30.0)
            .expect("spawn frame stream");
        let streamed = stream
            .frames
            .recv_timeout(std::time::Duration::from_secs(15))
            .expect("first streamed frame");
        assert_eq!((streamed.width, streamed.height), (320, 180));
        stream.stop();

        let mut player =
            crate::AudioPlayer::play_segment(&music_path, 0.0, 5.0, 0.0).expect("audio player");
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!player.is_finished());
        drop(player);

        // Chop two segments out of the sample video, lay the MP3 under the
        // whole thing, burn a title — then export and re-probe the result.
        let mut tl = Timeline::empty_project();
        tl.add_media_clip("v1", ClipKind::Video, 1, 0.0, 3.0, 2.0, &video_path);
        tl.add_media_clip("v2", ClipKind::Video, 1, 3.0, 2.0, 10.0, &video_path);
        let music_clip = tl.add_media_clip("music", ClipKind::Audio, 2, 0.0, 5.0, 1.0, &music_path);
        tl.clip_mut(music_clip).unwrap().volume = 0.6;
        tl.add_clip("Chop test", ClipKind::Title, 0, 0.5, 2.0, 0.0);

        let infos: HashMap<String, MediaInfo> =
            [(video_path.clone(), video), (music_path.clone(), music)].into();
        let out = dir.join("export.mp4");
        let (tx, rx) = channel();
        export_timeline(&tl, &infos, &out, tx).expect("export");
        let progress: Vec<f64> = rx.try_iter().collect();
        assert!(progress.iter().any(|p| *p >= 0.99), "{progress:?}");

        let exported = probe(&out.to_string_lossy()).expect("probe export");
        assert!(exported.has_video && exported.has_audio);
        assert!(
            (4.6..5.4).contains(&exported.duration),
            "{}",
            exported.duration
        );
    }
}

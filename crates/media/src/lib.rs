//! Media engine built on FFmpeg: libav in process for probing, preview
//! decode and audio playback, the `ffmpeg` command-line tool for export,
//! waveforms and pictures.
//!
//! Rationale: `ffmpeg` ships on every target platform and tracks codec
//! support for us. The module boundary is kept narrow so export can move
//! in process too.

mod audio;
mod beats;
mod decode;
mod evidence;
mod export;
mod loudness;
mod playback;
mod samples;
mod sheet;
mod sweep;
mod voice;

pub use audio::{AudioEngine, AudioSegment, AUDIO_RATE};
pub use beats::{beat_grid, BeatGrid};
pub(crate) use decode::VideoDecoder;
pub use decode::{
    inject_hardware_fault, set_hardware_decode, take_hardware_failure, warm_hardware_decode,
    StillDecoder,
};
pub use evidence::{EvidenceDetail, EvidenceFrame, EvidenceSampler, EvidenceVideo};
pub use export::{
    export_canvas, export_timeline, title_look, ExportOptions, TitleLook, LOUDNESS_TARGET,
};
pub use loudness::{measure_mix, Loudness, LoudnessMeter};
pub use playback::FrameStream;
pub use samples::generate_samples;
pub use sheet::{contact_sheet, source_strip, SheetCell};
pub use sweep::{
    analyze_segment, build_scene_segments, build_segments, detect_scene_cuts, frame_jpeg_at,
    frame_visual_evidence, motion_samples, pcm_mono_16k, write_wav_24k, FrameVisualEvidence,
    MotionSample, SegmentVisualEvidence, SweepSegment,
};

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

/// Probe a media file in process: what it holds, how long it runs and the
/// size its picture is displayed at.
pub fn probe(path: &str) -> MediaResult<MediaInfo> {
    use ffmpeg_next::format::stream::Disposition;
    use ffmpeg_next::media::Type;

    decode::ensure_init();
    let input = ffmpeg_next::format::input(path).map_err(|error| format!("{path}: {error}"))?;
    let rate = |rate: ffmpeg_next::Rational| match rate.denominator() {
        0 => 0.0,
        den => f64::from(rate.numerator()) / f64::from(den),
    };
    let mut info = MediaInfo {
        path: path.to_owned(),
        name: Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_owned()),
        // Unknown is a large negative number.
        duration: (input.duration() as f64 / f64::from(ffmpeg_next::ffi::AV_TIME_BASE)).max(0.0),
        width: 0,
        height: 0,
        fps: 0.0,
        has_video: false,
        has_audio: false,
    };
    for stream in input.streams() {
        let parameters = stream.parameters();
        // Cover art in a music file is one attached picture, not video.
        let cover = stream.disposition().contains(Disposition::ATTACHED_PIC);
        match parameters.medium() {
            Type::Video if !info.has_video && !cover => {
                info.has_video = true;
                // SAFETY: the parameters are a valid AVCodecParameters owned
                // by the stream, which outlives this read.
                let (width, height) =
                    unsafe { ((*parameters.as_ptr()).width, (*parameters.as_ptr()).height) };
                info.width = width.max(0) as u32;
                info.height = height.max(0) as u32;
                // Report displayed dimensions, as every decode path rotates.
                if decode::display_turns(&stream) % 2 == 1 {
                    std::mem::swap(&mut info.width, &mut info.height);
                }
                info.fps = rate(stream.avg_frame_rate());
                if info.fps <= 0.0 {
                    info.fps = rate(stream.rate());
                }
            }
            Type::Audio => info.has_audio = true,
            _ => {}
        }
        if info.duration <= 0.0 && !cover {
            let base = stream.time_base();
            info.duration = (stream.duration() as f64 * rate(base)).max(0.0);
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
    if scaled_height(src_w, src_h, 144) == 0 {
        return Ok(Vec::new());
    }
    let interval = (duration / 90.0).max(2.0);
    let count = (duration / interval).ceil().max(1.0) as usize;
    let times: Vec<f64> = (0..count).map(|i| i as f64 * interval).collect();
    let frames = decode::filmstrip_stills(path, &times, 144, interval)?;
    if frames.is_empty() {
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
    *current = current.max(sample.unsigned_abs() as f32 / 32768.0);
    *count += 1;
    if *count >= per_peak {
        peaks.push(*current);
        *current = 0.0;
        *count = 0;
    }
}

/// Import waveform resolution. Fifty buckets per second is fine
/// enough for phrase-level silence cuts without retaining audio.
pub const WAVEFORM_PEAKS_PER_SEC: u32 = 50;

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

/// A half-open source-time range, in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

impl TimeRange {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

fn valid_peak_rate(peaks_per_sec: f64) -> bool {
    peaks_per_sec.is_finite() && peaks_per_sec > 0.0
}

/// Peak level at or under which speech recorded near full scale is silent.
pub const SILENCE_PEAK: f32 = 0.03;

/// The peak level at or under which this recording counts as silent. It
/// follows the recording: 4% of its loud level (the 99th-percentile peak),
/// which is `SILENCE_PEAK` near full scale and never more. A fixed level
/// called a quiet take's soft syllables silence. It is never under 0.002
/// either, so a file without sound stays silent.
pub fn silence_threshold(peaks: &[f32]) -> f32 {
    const SHARE_OF_LOUD: f32 = 0.04;
    const FLOOR: f32 = 0.002;
    let mut levels: Vec<f32> = peaks
        .iter()
        .copied()
        .filter(|peak| peak.is_finite())
        .collect();
    if levels.is_empty() {
        return SILENCE_PEAK;
    }
    let index = (levels.len() - 1) * 99 / 100;
    let (_, loud, _) = levels.select_nth_unstable_by(index, f32::total_cmp);
    (*loud * SHARE_OF_LOUD).clamp(FLOOR, SILENCE_PEAK)
}

/// Runs of waveform buckets at or below `threshold`, keeping only
/// runs at least `min_duration` long. This is deliberately DSP,
/// not speech recognition: a quiet room tone and true silence are
/// separated by the caller's threshold, while meaning/filler words
/// still come from transcription.
pub fn silence_ranges(
    peaks: &[f32],
    peaks_per_sec: f64,
    threshold: f32,
    min_duration: f64,
) -> Vec<TimeRange> {
    if peaks.is_empty() || !valid_peak_rate(peaks_per_sec) {
        return Vec::new();
    }
    let threshold = threshold.clamp(0.0, 1.0);
    let min_duration = min_duration.max(0.0);
    let mut ranges = Vec::new();
    let mut run_start: Option<usize> = None;
    for (index, peak) in peaks.iter().copied().enumerate() {
        let silent = peak.is_finite() && peak <= threshold;
        match (silent, run_start) {
            (true, None) => run_start = Some(index),
            (false, Some(start)) => {
                let range = TimeRange {
                    start: start as f64 / peaks_per_sec,
                    end: index as f64 / peaks_per_sec,
                };
                if range.duration() + f64::EPSILON >= min_duration {
                    ranges.push(range);
                }
                run_start = None;
            }
            _ => {}
        }
    }
    if let Some(start) = run_start {
        let range = TimeRange {
            start: start as f64 / peaks_per_sec,
            end: peaks.len() as f64 / peaks_per_sec,
        };
        if range.duration() + f64::EPSILON >= min_duration {
            ranges.push(range);
        }
    }
    ranges
}

/// Audible source ranges inside `[range_start, range_end)`.
/// Silences shorter than `min_silence` remain inside a phrase;
/// each kept phrase is expanded by `padding` on both sides so
/// tightening does not clip breaths or consonant attacks.
#[allow(clippy::too_many_arguments)]
pub fn audible_ranges(
    peaks: &[f32],
    peaks_per_sec: f64,
    range_start: f64,
    range_end: f64,
    threshold: f32,
    min_silence: f64,
    padding: f64,
) -> Vec<TimeRange> {
    if peaks.is_empty() || !valid_peak_rate(peaks_per_sec) {
        return Vec::new();
    }
    let media_end = peaks.len() as f64 / peaks_per_sec;
    let range_start = range_start.clamp(0.0, media_end);
    let range_end = range_end.clamp(range_start, media_end);
    if range_end <= range_start {
        return Vec::new();
    }
    let threshold = threshold.clamp(0.0, 1.0);
    let min_silence = min_silence.max(0.0);
    let padding = padding.max(0.0);
    let start_index = (range_start * peaks_per_sec).floor() as usize;
    let end_index = ((range_end * peaks_per_sec).ceil() as usize).min(peaks.len());

    let mut groups: Vec<TimeRange> = Vec::new();
    let mut group_start: Option<usize> = None;
    let mut previous_audible: Option<usize> = None;
    for index in start_index..end_index {
        let peak = peaks[index];
        if !peak.is_finite() || peak <= threshold {
            continue;
        }
        match (group_start, previous_audible) {
            (None, _) => group_start = Some(index),
            (Some(start), Some(previous)) => {
                let silent_buckets = index.saturating_sub(previous + 1);
                let silence = silent_buckets as f64 / peaks_per_sec;
                if silent_buckets > 0 && silence + f64::EPSILON >= min_silence {
                    groups.push(TimeRange {
                        start: start as f64 / peaks_per_sec,
                        end: (previous + 1) as f64 / peaks_per_sec,
                    });
                    group_start = Some(index);
                }
            }
            _ => {}
        }
        previous_audible = Some(index);
    }
    if let (Some(start), Some(previous)) = (group_start, previous_audible) {
        groups.push(TimeRange {
            start: start as f64 / peaks_per_sec,
            end: (previous + 1) as f64 / peaks_per_sec,
        });
    }

    let mut merged: Vec<TimeRange> = Vec::new();
    for group in groups {
        let range = TimeRange {
            start: (group.start - padding).clamp(range_start, range_end),
            end: (group.end + padding).clamp(range_start, range_end),
        };
        if range.duration() <= f64::EPSILON {
            continue;
        }
        if let Some(last) = merged.last_mut() {
            if range.start <= last.end {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use openreel_engine::{ClipKind, Timeline};
    use std::collections::HashMap;
    use std::sync::mpsc::channel;

    #[test]
    fn waveform_peak_handles_full_scale_negative_sample() {
        let mut peaks = Vec::new();
        let mut current = 0.0;
        let mut count = 0;
        accumulate(i16::MIN, 1, &mut peaks, &mut current, &mut count);
        assert_eq!(peaks, vec![1.0]);
    }

    #[test]
    fn waveform_silence_and_audible_ranges_are_source_timed() {
        let mut peaks = vec![0.0; 5];
        peaks.extend([0.8; 5]);
        peaks.extend([0.0; 5]);
        peaks.extend([0.7; 5]);

        let silences = silence_ranges(&peaks, 10.0, 0.03, 0.35);
        assert_eq!(
            silences,
            vec![
                TimeRange {
                    start: 0.0,
                    end: 0.5,
                },
                TimeRange {
                    start: 1.0,
                    end: 1.5,
                },
            ]
        );

        let audible = audible_ranges(&peaks, 10.0, 0.0, 2.0, 0.03, 0.35, 0.1);
        assert_eq!(
            audible,
            vec![
                TimeRange {
                    start: 0.4,
                    end: 1.1,
                },
                TimeRange {
                    start: 1.4,
                    end: 2.0,
                },
            ]
        );
    }

    #[test]
    fn a_quiet_recording_has_the_same_pauses_as_a_loud_one() {
        // A take peaking near 0.08 had its soft trailing syllables under the
        // fixed 0.03: 18 s of its 45 s read as silence, and about 5 s was.
        let mut loud = Vec::new();
        for _ in 0..4 {
            loud.extend([0.6_f32; 40]);
            loud.extend([0.08; 20]); // a soft trailing syllable
            loud.extend([0.004; 30]); // the pause, with room tone
        }
        let quiet: Vec<f32> = loud.iter().map(|peak| peak * 0.1).collect();
        let pauses = |peaks: &[f32]| silence_ranges(peaks, 50.0, silence_threshold(peaks), 0.35);
        assert_eq!(pauses(&loud).len(), 4);
        assert_eq!(pauses(&quiet), pauses(&loud));
        assert!((pauses(&loud)[0].duration() - 0.6).abs() < 1e-9);
        // Speech near full scale keeps the level it always had, and a file
        // without sound is silent throughout.
        assert_eq!(silence_threshold(&[0.9; 100]), SILENCE_PEAK);
        assert_eq!(pauses(&[0.0004; 200]).len(), 1);
        assert_eq!(silence_threshold(&[]), SILENCE_PEAK);
    }

    #[test]
    fn short_waveform_pauses_stay_inside_one_audible_range() {
        let peaks = [0.8, 0.8, 0.0, 0.0, 0.7, 0.7];
        let audible = audible_ranges(&peaks, 10.0, 0.0, 0.6, 0.03, 0.35, 0.0);
        assert_eq!(
            audible,
            vec![TimeRange {
                start: 0.0,
                end: 0.6,
            }]
        );
    }

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
        let jpeg = frame_jpeg_at(&video_path, 2.0, 320).expect("representative JPEG");
        assert_eq!(&jpeg[..2], &[0xff, 0xd8], "JPEG start marker");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xff, 0xd9], "JPEG end marker");
        // The public API falls back to the CLI, so exercise the in-process
        // decoder directly to prove that path really works.
        let in_process =
            crate::decode::frame_at(&video_path, 3.0, 320, true).expect("in-process decoded frame");
        assert_eq!((in_process.width, in_process.height), (320, 180));

        let thumbs =
            thumbnails(&video_path, video.width, video.height, video.duration).expect("thumbs");
        assert!(thumbs.len() >= 5, "{}", thumbs.len());
        // Keyed by each still's own PTS: ascending, inside the source.
        assert!(thumbs.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(thumbs
            .iter()
            .all(|(t, _)| (0.0..video.duration).contains(t)));

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

        // Decode the MP3 in process: one second from source 4 s, placed at
        // timeline 1 s, renders exactly that span and is not silent.
        let mut audio = crate::AudioEngine::manual();
        audio.seek(0.0);
        audio.set_limit(3.0);
        audio.set_running(true);
        audio.set_voices(&[(
            1,
            crate::AudioSegment {
                path: music_path.clone(),
                start: 1.0,
                duration: 1.0,
                source_in: 4.0,
                volume: 1.0,
                fade_in: 0.0,
                fade_out: 0.0,
                duck: Vec::new(),
                clean_voice: false,
                smooth_fades: false,
            },
        )]);
        let mut mix = Vec::new();
        while mix.len() < 3 * crate::AUDIO_RATE as usize * 2 {
            let block = audio.render(4096);
            assert!(!block.is_empty(), "audio clock stopped");
            mix.extend_from_slice(block);
        }
        let rate = crate::AUDIO_RATE as usize;
        let loud = |a: usize, b: usize| mix[a * 2..b * 2].iter().any(|s| s.abs() > 0.01);
        assert!(
            !loud(0, rate) && !loud(2 * rate, 3 * rate),
            "audio outside clip"
        );
        assert!(loud(rate, 2 * rate), "decoded music is silent");
        assert!(audio.take_error().is_none());

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
        export_timeline(
            &tl,
            &infos,
            &out,
            &crate::ExportOptions {
                normalize_loudness: true,
                ..Default::default()
            },
            tx,
        )
        .expect("export");
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

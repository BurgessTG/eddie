//! Native image evidence and bounded analysis videos for visual model requests.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use ffmpeg_next as ffmpeg;

use crate::{MediaResult, RawFrame, VideoDecoder};

const MAX_JPEG_FRAMES: usize = 40;
const MAX_VIDEO_FRAMES: usize = 256;
const MAX_EVIDENCE_SECONDS: f64 = 30.0;
const MAX_EVIDENCE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct EvidenceFrame {
    /// Actual normalized presentation timestamp, not the requested seek time.
    pub time: f64,
    pub jpeg: Vec<u8>,
}

/// A silent H.264/MP4 analysis copy, not a source-timed edit or export.
#[derive(Clone, Debug)]
pub struct EvidenceVideo {
    /// At most 16 MiB. Frames play at the requested constant proxy frame rate.
    pub mp4: Vec<u8>,
    /// One actual normalized source PTS per encoded frame, in presentation order.
    /// These strictly increasing times are authoritative, not the MP4 clock.
    pub frame_times: Vec<f64>,
    pub width: u32,
    pub height: u32,
}

/// Rectangular pixel measurements before JPEG compression at sampler resolution.
/// These are neither mask measurements nor semantic quality scores.
#[derive(Clone, Debug)]
pub struct EvidenceDetail {
    pub frame: EvidenceFrame,
    /// Population variance of the four-neighbor Laplacian (4Y minus the left,
    /// right, up and down neighbors) over interior crop pixels, with
    /// Y = (0.2126R + 0.7152G + 0.0722B) / 255. Units are normalized luma
    /// squared, in [0, 16]. A crop without interior pixels returns zero. This
    /// proxy depends on sampler resolution.
    pub sharpness: f32,
    /// Mean Y in [0, 1], using the same gamma-encoded RGB luma as sharpness.
    pub luminance: f32,
    /// Fraction of crop pixels with any RGB channel exactly 0 or 255.
    /// Digital channel rails are not proof of underexposure or overexposure.
    pub clipped_fraction: f32,
}

/// One native decoder per source, with reusable full-frame and crop JPEG encoders.
/// Video windows stream into one single-threaded FFmpeg encoder at a time.
pub struct EvidenceSampler {
    decoder: VideoDecoder,
    jpeg: JpegEncoder,
    detail_jpeg: Option<JpegEncoder>,
    spare: Vec<u8>,
}

impl EvidenceSampler {
    pub fn open(path: &str, width: u32) -> MediaResult<Self> {
        let decoder = VideoDecoder::open(path, width.clamp(64, 1280) / 2 * 2, false)?;
        let (width, height) = decoder.output_size();
        Ok(Self {
            decoder,
            jpeg: JpegEncoder::new(width, height)?,
            detail_jpeg: None,
            spare: Vec::new(),
        })
    }

    pub fn frame_at(&mut self, time: f64) -> MediaResult<EvidenceFrame> {
        if !time.is_finite() {
            return Err("Evidence frame time must be finite".to_owned());
        }
        let (time, frame) = self
            .decoder
            .frame_at_into(time, std::mem::take(&mut self.spare))?;
        let jpeg = self.jpeg.encode(&frame, 0, 0);
        self.spare = frame.rgba;
        Ok(EvidenceFrame { time, jpeg: jpeg? })
    }

    /// Sample a bounded JPEG sequence with one seek followed by sequential
    /// native decoding. Requests must be finite, nonnegative, nondecreasing,
    /// nonempty and at most 40 entries. Both the requested span and emitted
    /// source PTS span must be at most 30 s; combined JPEG bytes are at most
    /// 16 MiB. Invalid or oversized sequences return an error, not a partial batch.
    ///
    /// Frames cover [PTS, next PTS), clamping to the first/last frame outside
    /// the video. Repeated requests in one source interval yield one frame,
    /// with its actual normalized source PTS. Zero-length intervals are skipped.
    /// Two RGBA buffers and the sampler's JPEG encoder buffers are reused; only
    /// the selected JPEGs are retained, never the full decoded window.
    pub fn frames(&mut self, times: &[f64]) -> MediaResult<Vec<EvidenceFrame>> {
        if times.is_empty() || times.len() > MAX_JPEG_FRAMES {
            return Err("Evidence frames need 1 to 40 requested times".to_owned());
        }
        validate_times(times, "frames")?;
        let (time, frame) = self
            .decoder
            .frame_at_into(times[0], std::mem::take(&mut self.spare))?;
        if !time.is_finite() {
            return Err("Evidence frames have a nonfinite source timestamp".to_owned());
        }
        let mut frames = Vec::with_capacity(times.len());
        let mut bytes = 0;
        sample_frames(
            &mut self.decoder,
            &mut self.spare,
            times,
            (time, frame),
            "frames",
            |time, frame| {
                let jpeg = self.jpeg.encode(frame, 0, 0)?;
                if jpeg.len() > MAX_EVIDENCE_BYTES - bytes {
                    return Err("Evidence frames exceed 16 MiB".to_owned());
                }
                bytes += jpeg.len();
                frames.push(EvidenceFrame { time, jpeg });
                Ok(())
            },
        )?;
        Ok(frames)
    }

    /// Sample the frame covering each request in [PTS, next PTS), clamping to
    /// the first/last frame outside the video, as `frame_at` does. Requests must
    /// be finite, nonnegative, nondecreasing, nonempty and at most 256 entries.
    /// Both the requested span and the returned PTS span must be at most 30 s.
    /// Repeated requests and requests in the same source interval yield one
    /// frame. Zero-length source intervals are skipped. `fps` is 1..=30 and
    /// controls only the analysis copy's playback.
    ///
    /// One seek precedes a sequential native decode through the last requested
    /// interval. The window uses two reusable decoded RGBA buffers rather than
    /// retaining all its frames.
    pub fn video(&mut self, times: &[f64], fps: u32) -> MediaResult<EvidenceVideo> {
        if times.is_empty() || times.len() > MAX_VIDEO_FRAMES {
            return Err("Evidence video needs 1 to 256 requested times".to_owned());
        }
        if !(1..=30).contains(&fps) {
            return Err("Evidence video frame rate must be 1 to 30".to_owned());
        }
        validate_times(times, "video")?;

        let (time, frame) = self
            .decoder
            .frame_at_into(times[0], std::mem::take(&mut self.spare))?;
        if !time.is_finite() {
            return Err("Evidence video has a nonfinite source timestamp".to_owned());
        }
        let (width, height) = self.decoder.output_size();
        let mut encoder = VideoEncoder::new(width, height, fps)?;
        let mut frame_times = Vec::with_capacity(times.len());
        sample_frames(
            &mut self.decoder,
            &mut self.spare,
            times,
            (time, frame),
            "video",
            |time, frame| {
                encoder.write_frame(&frame.rgba)?;
                frame_times.push(time);
                Ok(())
            },
        )?;
        Ok(EvidenceVideo {
            mp4: encoder.finish()?,
            frame_times,
            width,
            height,
        })
    }

    /// Crop a finite, nonempty normalized half-open [left, top, right, bottom]
    /// rectangle within [0, 1]. Fractional pixel edges round outward, retaining
    /// every intersected pixel. The JPEG keeps those exact pixel dimensions,
    /// including odd sizes. Time selection and clamping match `frame_at`.
    pub fn detail_at(&mut self, time: f64, bounds: [f32; 4]) -> MediaResult<EvidenceDetail> {
        if !time.is_finite() {
            return Err("Evidence frame time must be finite".to_owned());
        }
        let (width, height) = self.decoder.output_size();
        let region = PixelRegion::from_bounds(bounds, width, height)?;
        if self.detail_jpeg.as_ref().map(|jpeg| jpeg.size()) != Some((region.width, region.height))
        {
            self.detail_jpeg = Some(JpegEncoder::new(region.width, region.height)?);
        }
        let (time, frame) = self
            .decoder
            .frame_at_into(time, std::mem::take(&mut self.spare))?;
        let (sharpness, luminance, clipped_fraction) = region.measure(&frame);
        let jpeg = self
            .detail_jpeg
            .as_mut()
            .expect("crop encoder initialized")
            .encode(&frame, region.left, region.top);
        self.spare = frame.rgba;
        Ok(EvidenceDetail {
            frame: EvidenceFrame { time, jpeg: jpeg? },
            sharpness,
            luminance,
            clipped_fraction,
        })
    }
}

// Callers check their own nonempty/count limits before shared time validation.
fn validate_times(times: &[f64], kind: &str) -> MediaResult<()> {
    if times.iter().any(|time| !time.is_finite() || *time < 0.0)
        || times.windows(2).any(|pair| pair[0] > pair[1])
    {
        return Err(format!(
            "Evidence {kind} times must be finite, nonnegative and source-ordered"
        ));
    }
    if times[times.len() - 1] > times[0] + MAX_EVIDENCE_SECONDS {
        return Err(format!("Evidence {kind} requests exceed 30 source seconds"));
    }
    Ok(())
}

// The initial frame comes from the caller's single seek. Keep interval
// selection, duplicate-PTS handling and EOF draining identical for JPEG/video.
fn sample_frames(
    decoder: &mut VideoDecoder,
    buffer: &mut Vec<u8>,
    times: &[f64],
    (mut time, mut frame): (f64, RawFrame),
    kind: &str,
    mut emit: impl FnMut(f64, &RawFrame) -> MediaResult<()>,
) -> MediaResult<()> {
    let mut first_time: Option<f64> = None;
    let mut spare = Vec::new();
    let mut requested = 0;
    while requested < times.len() {
        let next = decoder.try_next_frame(std::mem::take(&mut spare))?;
        let interval_end = if let Some((next_time, _)) = &next {
            if !next_time.is_finite() || *next_time < time {
                return Err(format!("Evidence {kind} source timestamps are not ordered"));
            }
            *next_time
        } else {
            f64::INFINITY
        };
        if time < interval_end && times[requested] < interval_end {
            if first_time.is_some_and(|first| time > first + MAX_EVIDENCE_SECONDS) {
                return Err(format!("Evidence {kind} frames exceed 30 source seconds"));
            }
            emit(time, &frame)?;
            first_time.get_or_insert(time);
            while requested < times.len() && times[requested] < interval_end {
                requested += 1;
            }
        }
        if let Some((next_time, next_frame)) = next {
            spare = std::mem::replace(&mut frame, next_frame).rgba;
            time = next_time;
        } else {
            break;
        }
    }
    *buffer = frame.rgba;
    Ok(())
}

struct PixelRegion {
    left: usize,
    top: usize,
    width: u32,
    height: u32,
}

impl PixelRegion {
    fn from_bounds(bounds: [f32; 4], width: u32, height: u32) -> MediaResult<Self> {
        let [left, top, right, bottom] = bounds;
        if bounds
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
            || left >= right
            || top >= bottom
        {
            return Err("Evidence detail needs a nonempty rectangle within [0, 1]".to_owned());
        }
        let left = (left as f64 * width as f64).floor() as u32;
        let top = (top as f64 * height as f64).floor() as u32;
        let right = (right as f64 * width as f64).ceil() as u32;
        let bottom = (bottom as f64 * height as f64).ceil() as u32;
        Ok(Self {
            left: left as usize,
            top: top as usize,
            width: right - left,
            height: bottom - top,
        })
    }

    fn measure(&self, frame: &RawFrame) -> (f32, f32, f32) {
        let pixel = |x: usize, y: usize| {
            let start = ((self.top + y) * frame.width as usize + self.left + x) * 4;
            &frame.rgba[start..start + 3]
        };
        let luma = |x, y| {
            let rgb = pixel(x, y);
            (0.2126 * rgb[0] as f64 + 0.7152 * rgb[1] as f64 + 0.0722 * rgb[2] as f64) / 255.0
        };
        let mut sum = 0.0;
        let mut clipped = 0;
        let mut laplacian_sum = 0.0;
        let mut laplacian_squared = 0.0;
        let mut interior = 0;
        for y in 0..self.height as usize {
            for x in 0..self.width as usize {
                let value = luma(x, y);
                sum += value;
                if pixel(x, y)
                    .iter()
                    .any(|channel| *channel == 0 || *channel == 255)
                {
                    clipped += 1;
                }
                if x > 0 && x + 1 < self.width as usize && y > 0 && y + 1 < self.height as usize {
                    let laplacian = 4.0 * value
                        - luma(x - 1, y)
                        - luma(x + 1, y)
                        - luma(x, y - 1)
                        - luma(x, y + 1);
                    laplacian_sum += laplacian;
                    laplacian_squared += laplacian * laplacian;
                    interior += 1;
                }
            }
        }
        let variance = if interior > 0 {
            (laplacian_squared / interior as f64 - (laplacian_sum / interior as f64).powi(2))
                .max(0.0)
        } else {
            0.0
        };
        let pixels = self.width as f64 * self.height as f64;
        (
            variance as f32,
            (sum / pixels) as f32,
            (clipped as f64 / pixels) as f32,
        )
    }
}

struct JpegEncoder {
    encoder: ffmpeg::encoder::Video,
    scaler: ffmpeg::software::scaling::Context,
    rgba: ffmpeg::frame::Video,
    yuv: ffmpeg::frame::Video,
    sequence: i64,
}

impl JpegEncoder {
    fn new(width: u32, height: u32) -> MediaResult<Self> {
        let codec = ffmpeg::encoder::find(ffmpeg::codec::Id::MJPEG)
            .ok_or_else(|| "JPEG encoder is unavailable".to_owned())?;
        let mut encoder = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|error| format!("JPEG encoder: {error}"))?;
        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_format(ffmpeg::format::Pixel::YUVJ420P);
        encoder.set_time_base((1, 1));
        encoder.set_threading(ffmpeg::codec::threading::Config {
            kind: ffmpeg::codec::threading::Type::Slice,
            count: 1,
        });
        let encoder = encoder
            .open_as(codec)
            .map_err(|error| format!("open JPEG encoder: {error}"))?;
        let scaler = ffmpeg::software::scaling::Context::get(
            ffmpeg::format::Pixel::RGBA,
            width,
            height,
            ffmpeg::format::Pixel::YUVJ420P,
            width,
            height,
            ffmpeg::software::scaling::Flags::BILINEAR,
        )
        .map_err(|error| format!("JPEG colorspace: {error}"))?;
        Ok(Self {
            encoder,
            scaler,
            rgba: ffmpeg::frame::Video::new(ffmpeg::format::Pixel::RGBA, width, height),
            yuv: ffmpeg::frame::Video::empty(),
            sequence: 0,
        })
    }

    fn size(&self) -> (u32, u32) {
        (self.rgba.width(), self.rgba.height())
    }

    fn encode(&mut self, frame: &RawFrame, left: usize, top: usize) -> MediaResult<Vec<u8>> {
        let width = self.rgba.width() as usize * 4;
        let stride = self.rgba.stride(0);
        for row in 0..self.rgba.height() as usize {
            let start = ((top + row) * frame.width as usize + left) * 4;
            self.rgba.data_mut(0)[row * stride..row * stride + width]
                .copy_from_slice(&frame.rgba[start..start + width]);
        }
        // An encoder may still own a reference to the preceding picture.
        if self.yuv.width() > 0 {
            let result = unsafe { ffmpeg::ffi::av_frame_make_writable(self.yuv.as_mut_ptr()) };
            if result < 0 {
                return Err(format!(
                    "JPEG image buffer: {}",
                    ffmpeg::Error::from(result)
                ));
            }
        }
        self.scaler
            .run(&self.rgba, &mut self.yuv)
            .map_err(|error| format!("JPEG colorspace conversion: {error}"))?;
        self.yuv.set_pts(Some(self.sequence));
        self.sequence += 1;
        self.encoder
            .send_frame(&self.yuv)
            .map_err(|error| format!("encode JPEG: {error}"))?;
        let mut packet = ffmpeg::Packet::empty();
        self.encoder
            .receive_packet(&mut packet)
            .map_err(|error| format!("receive JPEG: {error}"))?;
        packet
            .data()
            .filter(|bytes| bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9]))
            .map(|bytes| bytes.to_vec())
            .ok_or_else(|| "Encoder returned an invalid JPEG".to_owned())
    }
}

struct VideoEncoder {
    directory: PathBuf,
    child: Option<Child>,
}

impl VideoEncoder {
    fn new(width: u32, height: u32, fps: u32) -> MediaResult<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let directory = loop {
            let path = std::env::temp_dir().join(format!(
                "openreel-evidence-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("create evidence directory: {error}")),
            }
        };
        let mut encoder = Self {
            directory,
            child: None,
        };
        // A file avoids blocking the raw-video writer on a full stderr pipe.
        let stderr = File::create(encoder.directory.join("stderr"))
            .map_err(|error| format!("create evidence encoder log: {error}"))?;
        encoder.child = Some(
            crate::cli::ffmpeg()
                .args([
                    "-v",
                    "error",
                    "-nostdin",
                    "-y",
                    "-filter_threads",
                    "1",
                    "-threads",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pixel_format",
                    "rgba",
                    "-video_size",
                    &format!("{width}x{height}"),
                    "-framerate",
                    &fps.to_string(),
                    "-i",
                    "pipe:0",
                    "-an",
                    "-c:v",
                    "libx264",
                    "-threads:v",
                    "1",
                    "-preset",
                    "veryfast",
                    "-crf",
                    "23",
                    "-bf",
                    "0",
                    "-pix_fmt",
                    "yuv420p",
                    "-movflags",
                    "+faststart",
                    "-fs",
                    &(MAX_EVIDENCE_BYTES + 1).to_string(),
                    "-f",
                    "mp4",
                ])
                .arg(encoder.directory.join("video.mp4"))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::from(stderr))
                .spawn()
                .map_err(|error| format!("start evidence video encoder: {error}"))?,
        );
        Ok(encoder)
    }

    fn write_frame(&mut self, rgba: &[u8]) -> MediaResult<()> {
        self.child
            .as_mut()
            .and_then(|child| child.stdin.as_mut())
            .expect("running evidence encoder")
            .write_all(rgba)
            .map_err(|error| {
                if fs::metadata(self.directory.join("video.mp4"))
                    .is_ok_and(|metadata| metadata.len() > MAX_EVIDENCE_BYTES as u64)
                {
                    "Evidence video exceeds 16 MiB".to_owned()
                } else {
                    format!("write evidence video frame: {error}")
                }
            })
    }

    fn finish(mut self) -> MediaResult<Vec<u8>> {
        let child = self.child.as_mut().expect("running evidence encoder");
        drop(child.stdin.take());
        let status = child
            .wait()
            .map_err(|error| format!("wait for evidence video: {error}"))?;
        self.child = None;
        if !status.success() {
            let mut log = String::new();
            if let Ok(file) = File::open(self.directory.join("stderr")) {
                let _ = file.take(4096).read_to_string(&mut log);
            }
            return Err(format!(
                "Evidence video encoder exited with {status}: {}",
                log.trim()
            ));
        }
        let file = File::open(self.directory.join("video.mp4"))
            .map_err(|error| format!("open evidence video: {error}"))?;
        let mut mp4 = Vec::new();
        file.take((MAX_EVIDENCE_BYTES + 1) as u64)
            .read_to_end(&mut mp4)
            .map_err(|error| format!("read evidence video: {error}"))?;
        if mp4.is_empty() || mp4.len() > MAX_EVIDENCE_BYTES {
            return Err("Evidence video is empty or exceeds 16 MiB".to_owned());
        }
        Ok(mp4)
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            drop(child.stdin.take());
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

//! The H.264 encoder each picture segment goes through: x264 on the CPU,
//! or NVIDIA's NVENC when the user turned hardware encoding on and the GPU
//! takes this export's size.

use std::process::Stdio;

/// How a segment's H.264 is made. Every segment of one export uses the
/// same, so they join by stream copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Encoder {
    X264 { crf: u8, threads: usize },
    Nvenc { cq: u8 },
}

impl Encoder {
    /// The output options after `-map`.
    pub(super) fn args(self) -> Vec<String> {
        let args: &[&str] = match self {
            Self::X264 { .. } => &["-c:v", "libx264", "-preset", "veryfast"],
            // vbr with a constant quality and no bitrate is NVENC's
            // nearest to x264's CRF.
            Self::Nvenc { .. } => &[
                "-c:v",
                "h264_nvenc",
                "-preset",
                "p4",
                "-tune",
                "hq",
                "-rc",
                "vbr",
                "-b:v",
                "0",
            ],
        };
        let mut args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        match self {
            Self::X264 { crf, threads } => args.extend([
                "-threads:v".into(),
                threads.to_string(),
                "-crf".into(),
                crf.to_string(),
            ]),
            Self::Nvenc { cq } => args.extend(["-cq".into(), cq.to_string()]),
        }
        args
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::X264 { .. } => "x264",
            Self::Nvenc { .. } => "nvenc",
        }
    }

    pub(super) fn is_hardware(self) -> bool {
        matches!(self, Self::Nvenc { .. })
    }
}

/// Whether NVENC opens at `canvas` and `rate` with `encoder`'s options:
/// the FFmpeg in use may lack it, the machine may have no NVIDIA GPU, the
/// driver may refuse a size (under 160×90 or so), or every session may be
/// taken. Two frames, about 0.3 s.
pub(super) fn opens(encoder: Encoder, canvas: (u32, u32), rate: ffmpeg_next::Rational) -> bool {
    if cfg!(target_os = "macos") {
        return false;
    }
    let (width, height) = canvas;
    crate::cli::ffmpeg()
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=s={width}x{height}:r={rate}"))
        .args(["-frames:v", "2", "-pix_fmt", "yuv420p"])
        .args(encoder.args())
        .args(["-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

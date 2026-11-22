//! A clip's sound filters run in process for the preview, as export runs
//! them in its graph: noise reduction (`denoise_filter`), then speed
//! (`tempo_filter`).

use ffmpeg_next as ffmpeg;

use crate::denoise::{denoise_filter, NoiseProfile};
use crate::tempo::tempo_filter;
use crate::MediaResult;

/// A clip's filters over interleaved stereo `f32` at one rate.
pub(crate) struct Chain {
    graph: ffmpeg::filter::Graph,
    rate: u32,
    /// Input samples sent so far: the next input frame's timestamp.
    sent: i64,
    stretched: ffmpeg::frame::Audio,
    scratch: Vec<f32>,
}

fn fail(error: ffmpeg::Error) -> String {
    format!("audio filters: {error}")
}

impl Chain {
    /// The filters a clip's sound plays through, noise reduction first,
    /// or `None` when it plays as recorded. A `mono` source reaches the
    /// mix format 3 dB down on each channel, so it is raised back to its
    /// own level around `afftdn`, which export runs on the mono source.
    pub(crate) fn for_clip(
        speed: f64,
        noise: Option<&NoiseProfile>,
        mono: bool,
        rate: u32,
    ) -> MediaResult<Option<Self>> {
        let mut filters = Vec::new();
        if let Some(profile) = noise {
            let denoise = denoise_filter(profile);
            filters.push(if mono {
                format!(
                    "volume={}:precision=float,{denoise},volume={}:precision=float",
                    std::f64::consts::SQRT_2,
                    std::f64::consts::FRAC_1_SQRT_2
                )
            } else {
                denoise
            });
        }
        filters.extend(tempo_filter(speed));
        if filters.is_empty() {
            return Ok(None);
        }
        let chain = filters.join(",");
        crate::decode::ensure_init();
        let find = |name: &str| {
            ffmpeg::filter::find(name).ok_or_else(|| format!("FFmpeg has no {name} filter"))
        };
        let mut graph = ffmpeg::filter::Graph::new();
        graph
            .add(
                &find("abuffer")?,
                "in",
                &format!(
                    "time_base=1/{rate}:sample_rate={rate}:sample_fmt=flt:channel_layout=stereo"
                ),
            )
            .map_err(fail)?;
        graph.add(&find("abuffersink")?, "out", "").map_err(fail)?;
        graph
            .output("in", 0)
            .and_then(|parser| parser.input("out", 0))
            .and_then(|parser| {
                parser.parse(&format!(
                    "{chain},aformat=sample_fmts=flt:channel_layouts=stereo"
                ))
            })
            .map_err(fail)?;
        graph.validate().map_err(fail)?;
        Ok(Some(Self {
            graph,
            rate,
            sent: 0,
            stretched: ffmpeg::frame::Audio::empty(),
            scratch: Vec::new(),
        }))
    }

    /// Feed interleaved stereo samples, in order.
    pub(crate) fn push(&mut self, samples: &[f32]) -> MediaResult<()> {
        let count = samples.len() / 2;
        if count == 0 {
            return Ok(());
        }
        let mut frame = ffmpeg::frame::Audio::new(
            ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
            count,
            ffmpeg::ChannelLayout::STEREO,
        );
        frame.set_rate(self.rate);
        frame.set_pts(Some(self.sent));
        self.sent += count as i64;
        let (bytes, _) = frame.data_mut(0).as_chunks_mut::<4>();
        for (out, sample) in bytes.iter_mut().zip(&samples[..count * 2]) {
            *out = sample.to_ne_bytes();
        }
        let mut input = self.graph.get("in").expect("graph has its source");
        input.source().add(&frame).map_err(fail)
    }

    /// Mark the end of the input, so [`Chain::drain`] returns the rest.
    pub(crate) fn finish(&mut self) -> MediaResult<()> {
        let mut input = self.graph.get("in").expect("graph has its source");
        input.source().flush().map_err(fail)
    }

    /// Pass each block of filtered samples to `out`, in order, until none
    /// is ready (true) or `out` returns false (false).
    pub(crate) fn drain(&mut self, mut out: impl FnMut(&[f32]) -> bool) -> MediaResult<bool> {
        let mut output = self.graph.get("out").expect("graph has its sink");
        loop {
            match output.sink().frame(&mut self.stretched) {
                Ok(()) => {}
                Err(ffmpeg::Error::Eof) => return Ok(true),
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
                    return Ok(true)
                }
                Err(error) => return Err(fail(error)),
            }
            let count = self.stretched.samples() * 2;
            let (floats, _) = self.stretched.data(0).as_chunks::<4>();
            self.scratch.clear();
            self.scratch.extend(
                floats[..count]
                    .iter()
                    .map(|bytes| f32::from_ne_bytes(*bytes)),
            );
            if !out(&self.scratch) {
                return Ok(false);
            }
        }
    }
}

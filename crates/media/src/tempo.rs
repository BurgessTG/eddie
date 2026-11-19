//! Clip speed with the pitch kept: FFmpeg's `atempo` (WSOLA). Export puts
//! [`tempo_filter`] in its graph and the preview runs the same chain in
//! process ([`Tempo`]), so both stretch a clip's sound the same way.

use ffmpeg_next as ffmpeg;

use crate::MediaResult;

/// Source seconds read past a sped-up clip's end. `atempo` holds up to half
/// its 43 ms window back until more input comes, so without this the last
/// 50-100 ms of the clip would be silent.
pub(crate) const TEMPO_TAIL: f64 = 0.1;

/// The `atempo` chain that plays `speed` source seconds per second, or
/// `None` at normal speed. One stage takes 0.5x to 100x; slower speeds
/// chain two equal stages, as FFmpeg's documentation advises.
pub(crate) fn tempo_filter(speed: f64) -> Option<String> {
    if speed == 1.0 {
        return None;
    }
    Some(if speed >= 0.5 {
        format!("atempo={speed}")
    } else {
        let stage = speed.sqrt();
        format!("atempo={stage},atempo={stage}")
    })
}

/// [`tempo_filter`] over interleaved stereo `f32` at one rate.
pub(crate) struct Tempo {
    graph: ffmpeg::filter::Graph,
    rate: u32,
    /// Input samples sent so far: the next input frame's timestamp.
    sent: i64,
    stretched: ffmpeg::frame::Audio,
    scratch: Vec<f32>,
}

fn fail(error: ffmpeg::Error) -> String {
    format!("audio speed: {error}")
}

impl Tempo {
    /// The filter for `speed`, or `None` when it plays as recorded.
    pub(crate) fn for_speed(speed: f64, rate: u32) -> MediaResult<Option<Self>> {
        let Some(chain) = tempo_filter(speed) else {
            return Ok(None);
        };
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

    /// Mark the end of the input, so [`Tempo::drain`] returns the rest.
    pub(crate) fn finish(&mut self) -> MediaResult<()> {
        let mut input = self.graph.get("in").expect("graph has its source");
        input.source().flush().map_err(fail)
    }

    /// Pass each block of stretched samples to `out`, in order, until none
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

//! Media engine built on FFmpeg: libav in process for probing, preview
//! decode and audio playback, the `ffmpeg` command-line tool for export,
//! waveforms and pictures.
//!
//! Rationale: `ffmpeg` ships on every target platform and tracks codec
//! support for us. The module boundary is kept narrow so export can move
//! in process too.

mod audio;
mod beats;
mod cli;
mod decode;
mod evidence;
mod export;
mod frame;
mod loudness;
mod playback;
mod probe;
mod samples;
mod sheet;
mod stills;
mod sweep;
mod voice;
mod waveform;

#[cfg(test)]
mod tests;

pub use audio::{AudioEngine, AudioSegment, AUDIO_RATE};
pub use beats::{beat_grid, BeatGrid};
pub(crate) use cli::run;
pub use cli::{ffmpeg, ffmpeg_available};
/// The decoder itself and its filmstrip, for the tests in `tests/`.
#[doc(hidden)]
pub use decode::{filmstrip_stills, VideoDecoder};
pub use decode::{
    inject_hardware_fault, set_hardware_decode, take_hardware_failure, warm_hardware_decode,
    StillDecoder,
};
pub use evidence::{EvidenceDetail, EvidenceFrame, EvidenceSampler, EvidenceVideo};
pub use export::{
    export_canvas, export_timeline, title_look, ExportOptions, TitleLook, LOUDNESS_TARGET,
};
pub use frame::{scaled_height, RawFrame};
pub use loudness::{measure_mix, Loudness, LoudnessMeter};
pub use playback::FrameStream;
pub use probe::{probe, MediaInfo};
pub use samples::generate_samples;
pub use sheet::{contact_sheet, source_strip, SheetCell};
pub use stills::{frame_at, thumbnails};
pub use sweep::{
    analyze_segment, build_scene_segments, build_segments, detect_scene_cuts, frame_jpeg_at,
    frame_visual_evidence, motion_samples, pcm_mono_16k, write_wav_24k, FrameVisualEvidence,
    MotionSample, SegmentVisualEvidence, SweepSegment,
};
pub use waveform::{
    audible_ranges, silence_ranges, silence_threshold, waveform_peaks, TimeRange, SILENCE_PEAK,
    WAVEFORM_PEAKS_PER_SEC,
};

pub type MediaResult<T> = Result<T, String>;

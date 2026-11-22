//! A 16-bit mono WAV written as it is recorded. The header's sizes are
//! brought up to date every second and at the end, so a take cut short by
//! a crash still opens with what was written before it.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

/// Bytes before the samples.
const HEADER: u64 = 44;

pub(super) struct Wav {
    file: BufWriter<File>,
    rate: u32,
    /// Samples written so far.
    frames: u64,
    /// Samples written when the header last had their count.
    counted: u64,
}

impl Wav {
    pub(super) fn create(path: &Path, rate: u32) -> std::io::Result<Self> {
        let mut wav = Self {
            file: BufWriter::new(File::create(path)?),
            rate,
            frames: 0,
            counted: 0,
        };
        wav.header()?;
        Ok(wav)
    }

    /// Append `samples` from -1 to 1; louder ones are clipped.
    pub(super) fn write(&mut self, samples: &[f32]) -> std::io::Result<()> {
        for &sample in samples {
            let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
            self.file.write_all(&value.to_le_bytes())?;
        }
        self.frames += samples.len() as u64;
        if self.frames - self.counted >= u64::from(self.rate) {
            self.header()?;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> std::io::Result<u64> {
        self.header()?;
        self.file.into_inner()?.sync_all()?;
        Ok(self.frames)
    }

    /// Write the header for the samples so far, and go back to the end.
    fn header(&mut self) -> std::io::Result<()> {
        let data = u32::try_from(self.frames * 2).unwrap_or(u32::MAX);
        let mut header = Vec::with_capacity(HEADER as usize);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&data.saturating_add(36).to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16_u32.to_le_bytes());
        // PCM, one channel.
        header.extend_from_slice(&1_u16.to_le_bytes());
        header.extend_from_slice(&1_u16.to_le_bytes());
        header.extend_from_slice(&self.rate.to_le_bytes());
        header.extend_from_slice(&(self.rate * 2).to_le_bytes());
        header.extend_from_slice(&2_u16.to_le_bytes());
        header.extend_from_slice(&16_u16.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&data.to_le_bytes());
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header)?;
        self.file.seek(SeekFrom::Start(HEADER + self.frames * 2))?;
        self.file.flush()?;
        self.counted = self.frames;
        Ok(())
    }
}

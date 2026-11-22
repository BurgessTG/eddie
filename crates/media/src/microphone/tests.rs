use std::time::{Duration, Instant};

use super::{Microphone, TONE_LEVEL};

/// The samples of a 16-bit mono WAV, after checking its header says so.
fn samples(bytes: &[u8], rate: u32) -> Vec<f32> {
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let u16_at = |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(u32_at(4) as usize, bytes.len() - 8, "RIFF size");
    assert_eq!((u16_at(20), u16_at(22)), (1, 1), "PCM, mono");
    assert_eq!((u32_at(24), u16_at(34)), (rate, 16));
    assert_eq!(u32_at(40) as usize, bytes.len() - 44, "data size");
    bytes[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| f32::from(i16::from_le_bytes(pair)) / f32::from(i16::MAX))
        .collect()
}

/// A folder of the test's own, removed when it ends.
struct Folder(std::path::PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("eddie-mic-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(10), "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_take_is_the_wav_of_what_the_microphone_heard() {
    let dir = Folder::new("take");
    let path = dir.path().join("takes").join("Voiceover 1.wav");
    let mut mic = Microphone::manual();
    // Heard before the take: metered, not written.
    mic.advance(0.5);
    assert!((mic.level() - TONE_LEVEL).abs() < 0.01);
    assert_eq!(mic.level(), 0.0, "reading the level resets it");

    let take = mic.record(&path).unwrap();
    assert!(
        mic.record(&dir.path().join("b.wav")).is_err(),
        "one take at a time"
    );
    // Uneven steps, as frames come: no sample is lost or doubled.
    for _ in 0..30 {
        mic.advance(0.05 + 1.0 / 3.0 / 48_000.0 * 7.0);
    }
    wait_for("the writer", || take.seconds() >= 1.5);
    // Past a second the header counts what is written, so a crash keeps it.
    let early = std::fs::read(&path).unwrap();
    let counted = u32::from_le_bytes(early[40..44].try_into().unwrap()) as usize;
    assert!(
        (2 * 48_000..=early.len() - 44).contains(&counted),
        "{counted} bytes counted of {}",
        early.len() - 44
    );
    assert!(take.peaks().len() >= 70);

    let recorded = take.finish().unwrap();
    mic.advance(1.0);
    // 30 steps of 2,402⅓ samples.
    let wanted = recorded.seconds * 48_000.0;
    assert!((wanted - 72_070.0).abs() <= 1.0, "{wanted} samples");
    assert_eq!(recorded.first_sample, None, "a tone has no capture time");
    assert_eq!(recorded.dropped, 0.0);

    let pcm = samples(&std::fs::read(&path).unwrap(), 48_000);
    assert_eq!(pcm.len() as f64, wanted, "nothing after the take");
    // The tone, continuing across the steps: its own sine, sample by sample.
    let step = std::f64::consts::TAU * 440.0 / 48_000.0;
    let first = (0.5 * 48_000.0) as usize;
    let worst = pcm
        .iter()
        .enumerate()
        .map(|(i, s)| (s - ((first + i) as f64 * step).sin() as f32 * TONE_LEVEL).abs())
        .fold(0.0_f32, f32::max);
    assert!(worst < 1e-4, "off by {worst}");

    let info = crate::probe(path.to_str().unwrap()).unwrap();
    assert!(info.has_audio && !info.has_video);
    assert!(
        (info.duration - recorded.seconds).abs() < 0.01,
        "{}",
        info.duration
    );
}

#[test]
fn a_dropped_take_stops_and_its_file_still_opens() {
    let dir = Folder::new("dropped");
    let path = dir.path().join("cut short.wav");
    let mut mic = Microphone::manual();
    let take = mic.record(&path).unwrap();
    mic.advance(0.25);
    drop(take);
    mic.advance(0.25);
    // The writer finishes alone; then the microphone records again.
    wait_for("the file", || {
        std::fs::read(&path).is_ok_and(|bytes| bytes.len() == 44 + 2 * 12_000)
    });
    assert_eq!(
        samples(&std::fs::read(&path).unwrap(), 48_000).len(),
        12_000
    );
    assert!(mic.record(&dir.path().join("again.wav")).is_ok());
}

//! Card pictures for the Gallery: one still worker, asked for one source
//! picture at a time, with a bounded cache of what it returned.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, TryRecvError};

use openreel_media::{MediaInfo, RawFrame, StillDecoder, scaled_height};
use rune::Pixels;

const CACHE_CAPACITY: usize = 256;
const MAX_EDGE: u32 = 224;
const WORKER_STOPPED: &str = "Thumbnail decoder stopped";

#[derive(PartialEq, Eq)]
struct ThumbnailKey {
    path: String,
    time_bits: u64,
}

struct CachedThumbnail {
    key: ThumbnailKey,
    result: Result<Pixels, String>,
}

enum DecoderState {
    Idle,
    Pending { generation: u64, key: ThumbnailKey },
    Disconnected,
}

pub(crate) struct GalleryThumbnails {
    decoder: StillDecoder,
    results: Receiver<(u64, Result<RawFrame, String>)>,
    state: DecoderState,
    generation: u64,
    cache: VecDeque<CachedThumbnail>,
    /// The pixels of each decoded picture, which `Pixels` does not give back.
    #[cfg(test)]
    decoded: Vec<(String, u64, Vec<u8>)>,
}

impl GalleryThumbnails {
    /// `wake` runs on the worker thread when a picture is ready.
    pub(crate) fn new(wake: impl Fn() + Send + 'static) -> Self {
        let (decoder, results) = StillDecoder::spawn(wake);
        Self {
            decoder,
            results,
            state: DecoderState::Idle,
            generation: 0,
            cache: VecDeque::with_capacity(CACHE_CAPACITY),
            #[cfg(test)]
            decoded: Vec::new(),
        }
    }

    /// A worker thread holds a request, so its card is still spinning.
    pub(crate) fn pending(&self) -> bool {
        matches!(self.state, DecoderState::Pending { .. })
    }

    /// Takes finished pictures into the cache. Called every frame, so a
    /// request whose card scrolled away still completes, and a stopped
    /// worker is noticed without its wake.
    pub(crate) fn collect(&mut self) {
        loop {
            match self.results.try_recv() {
                Ok((completed_generation, result)) => {
                    match std::mem::replace(&mut self.state, DecoderState::Idle) {
                        DecoderState::Pending { generation, key }
                            if generation == completed_generation =>
                        {
                            #[cfg(test)]
                            if let Ok(frame) = &result {
                                self.decoded.push((
                                    key.path.clone(),
                                    key.time_bits,
                                    frame.rgba.clone(),
                                ));
                            }
                            self.cache_result(key, result.and_then(picture));
                        }
                        other => self.state = other,
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if let DecoderState::Pending { key, .. } =
                        std::mem::replace(&mut self.state, DecoderState::Disconnected)
                    {
                        self.cache_result(key, Err(WORKER_STOPPED.to_owned()));
                    }
                    break;
                }
            }
        }
    }

    /// The picture at `time` in `info`'s file: ready, on its way (`None`),
    /// or why there is none. Asking starts the decode when the worker is
    /// free; a card still without its picture asks again on a later frame.
    pub(crate) fn picture(
        &mut self,
        info: &MediaInfo,
        time: f64,
    ) -> Result<Option<Pixels>, String> {
        self.collect();
        let time_bits = time.to_bits();
        if let Some(index) = self
            .cache
            .iter()
            .position(|entry| entry.key.path == info.path && entry.key.time_bits == time_bits)
        {
            let entry = self.cache.remove(index).unwrap();
            let result = entry.result.clone().map(Some);
            self.cache.push_back(entry);
            return result;
        }

        let target_w = (u64::from(info.width) * u64::from(MAX_EDGE)
            / u64::from(info.width.max(info.height).max(MAX_EDGE)))
        .max(1) as u32;
        let error = if !time.is_finite() || time < 0.0 {
            Some("Invalid thumbnail source time")
        } else if !info.has_video {
            Some("Source has no video stream")
        } else if info.width == 0 || info.height == 0 {
            Some("Invalid source dimensions")
        } else if scaled_height(info.width, info.height, target_w) > MAX_EDGE {
            Some("Source aspect ratio exceeds the thumbnail size limit")
        } else if matches!(self.state, DecoderState::Disconnected) {
            Some(WORKER_STOPPED)
        } else {
            None
        };
        if let Some(error) = error {
            let result = Err(error.to_owned());
            self.cache_result(
                ThumbnailKey {
                    path: info.path.clone(),
                    time_bits,
                },
                result.clone(),
            );
            return result.map(Some);
        }

        if matches!(self.state, DecoderState::Idle) {
            self.generation = self.generation.wrapping_add(1);
            self.state = DecoderState::Pending {
                generation: self.generation,
                key: ThumbnailKey {
                    path: info.path.clone(),
                    time_bits,
                },
            };
            self.decoder.request(
                self.generation,
                info.path.clone(),
                info.width,
                info.height,
                time,
                target_w,
                true,
            );
        }
        Ok(None)
    }

    fn cache_result(&mut self, key: ThumbnailKey, result: Result<Pixels, String>) {
        if let Some(index) = self.cache.iter().position(|entry| entry.key == key) {
            self.cache.remove(index);
        }
        if self.cache.len() == CACHE_CAPACITY {
            self.cache.pop_front();
        }
        self.cache.push_back(CachedThumbnail { key, result });
    }
}

/// A decoded still as a picture the window can draw, refusing sizes the
/// worker was never asked for.
fn picture(frame: RawFrame) -> Result<Pixels, String> {
    check(&frame)?;
    let pixels = Pixels::new();
    pixels.set(frame.width, frame.height, frame.rgba);
    Ok(pixels)
}

fn check(frame: &RawFrame) -> Result<(), String> {
    if frame.width == 0 || frame.height == 0 || frame.width > MAX_EDGE || frame.height > MAX_EDGE {
        return Err("Invalid thumbnail dimensions".to_owned());
    }
    if frame.rgba.len() != (frame.width * frame.height * 4) as usize {
        return Err("Invalid thumbnail pixel data".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::*;

    struct Footage(PathBuf);

    impl Drop for Footage {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn gallery_thumbnails_show_the_requested_source_and_range() {
        let footage = Footage(std::env::temp_dir().join(format!(
            "eddie-rune-gallery-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        )));
        std::fs::create_dir(&footage.0).unwrap();
        let mut sources = Vec::new();
        for (name, input) in [
            (
                "landscape",
                "color=c=red:s=320x180:r=30:d=1[a];color=c=lime:s=320x180:r=30:d=1[b];[a][b]concat=n=2:v=1:a=0",
            ),
            ("portrait", "color=c=blue:s=60x90:r=30:d=2"),
        ] {
            let path = footage.0.join(format!("{name}.mp4"));
            let output = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    input,
                    "-c:v",
                    "libx264",
                    "-threads:v",
                    "1",
                    "-filter_threads",
                    "1",
                    "-pix_fmt",
                    "yuv420p",
                ])
                .arg(&path)
                .output()
                .expect("generate source footage");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            sources.push(openreel_media::probe(path.to_str().unwrap()).unwrap());
        }

        let mut thumbnails = GalleryThumbnails::new(|| {});
        let requests = [
            (&sources[0], 0.9999, [255, 0, 0], (224, 126)),
            (&sources[1], 0.9999, [0, 0, 255], (60, 90)),
            (&sources[0], 1.0, [0, 255, 0], (224, 126)),
        ];
        let mut pictures: Vec<Option<Pixels>> = vec![None; requests.len()];
        let deadline = Instant::now() + Duration::from_secs(10);
        while pictures.iter().any(Option::is_none) {
            for (index, (source, time, _, _)) in requests.iter().enumerate() {
                pictures[index] = thumbnails.picture(source, *time).unwrap();
            }
            assert!(Instant::now() < deadline, "thumbnails must finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!thumbnails.pending(), "nothing is left to decode");
        for ((source, time, expected, size), picture) in requests.iter().zip(&pictures) {
            let picture = picture.as_ref().unwrap();
            assert_eq!(picture.size(), *size, "bounded without upscaling");
            let (_, _, rgba) = thumbnails
                .decoded
                .iter()
                .find(|(path, bits, _)| *path == source.path && *bits == time.to_bits())
                .expect("each request decoded once");
            for pixel in rgba.chunks_exact(4) {
                let distance: i32 = pixel[..3]
                    .iter()
                    .zip(expected)
                    .map(|(actual, expected)| (i32::from(*actual) - expected).abs())
                    .sum();
                assert!(distance < 24, "{} at {time}: {pixel:?}", source.name);
            }
            let cached = thumbnails.picture(source, *time).unwrap().unwrap();
            // `Pixels` prints its identity; a second ask must not decode again.
            assert_eq!(
                format!("{cached:?}"),
                format!("{picture:?}"),
                "a rebuilt card reuses its picture"
            );
        }
        assert_eq!(thumbnails.decoded.len(), requests.len());

        let mut missing = sources[0].clone();
        missing.path = footage.0.join("missing.mp4").to_str().unwrap().to_owned();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match thumbnails.picture(&missing, 0.5) {
                Err(_) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Ok(Some(_)) => panic!("a missing source must not borrow another card's image"),
            }
            assert!(Instant::now() < deadline, "failed thumbnails must finish");
        }
        assert!(thumbnails.picture(&sources[0], 1.0).unwrap().is_some());
    }
}

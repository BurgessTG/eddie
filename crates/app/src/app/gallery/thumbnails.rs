use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use openreel_media::{scaled_height, MediaInfo, RawFrame, StillDecoder};

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
    result: Result<TextureHandle, String>,
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
}

impl GalleryThumbnails {
    pub(crate) fn new(ctx: &egui::Context) -> Self {
        let wake_ctx = ctx.clone();
        let (decoder, results) = StillDecoder::spawn(move || wake_ctx.request_repaint());
        Self {
            decoder,
            results,
            state: DecoderState::Idle,
            generation: 0,
            cache: VecDeque::with_capacity(CACHE_CAPACITY),
        }
    }

    pub(crate) fn texture(
        &mut self,
        ctx: &egui::Context,
        info: &MediaInfo,
        time: f64,
    ) -> Result<Option<TextureHandle>, String> {
        loop {
            match self.results.try_recv() {
                Ok((completed_generation, result)) => {
                    match std::mem::replace(&mut self.state, DecoderState::Idle) {
                        DecoderState::Pending { generation, key }
                            if generation == completed_generation =>
                        {
                            let texture = result.and_then(|frame| {
                                if frame.width == 0
                                    || frame.height == 0
                                    || frame.width > MAX_EDGE
                                    || frame.height > MAX_EDGE
                                {
                                    return Err("Invalid thumbnail dimensions".to_owned());
                                }
                                if frame.rgba.len() != (frame.width * frame.height * 4) as usize {
                                    return Err("Invalid thumbnail pixel data".to_owned());
                                }
                                let image = ColorImage::from_rgba_unmultiplied(
                                    [frame.width as usize, frame.height as usize],
                                    &frame.rgba,
                                );
                                Ok(ctx.load_texture(
                                    format!("gallery-thumbnail-{generation}"),
                                    image,
                                    TextureOptions::LINEAR,
                                ))
                            });
                            self.cache_result(key, texture);
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
        // A worker panic cannot call wake, so pending cards also poll for disconnection.
        ctx.request_repaint_after(Duration::from_millis(100));
        Ok(None)
    }

    fn cache_result(&mut self, key: ThumbnailKey, result: Result<TextureHandle, String>) {
        if let Some(index) = self.cache.iter().position(|entry| entry.key == key) {
            self.cache.remove(index);
        }
        if self.cache.len() == CACHE_CAPACITY {
            self.cache.pop_front();
        }
        self.cache.push_back(CachedThumbnail { key, result });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
            "eddie-gallery-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
        )));
        std::fs::create_dir(&footage.0).unwrap();
        let mut sources = Vec::new();
        for (name, input) in [
            ("landscape", "color=c=red:s=320x180:r=30:d=1[a];color=c=lime:s=320x180:r=30:d=1[b];[a][b]concat=n=2:v=1:a=0"),
            ("portrait", "color=c=blue:s=60x90:r=30:d=2"),
        ] {
            let path = footage.0.join(format!("{name}.mp4"));
            let output = Command::new("ffmpeg")
                .args(["-v", "error", "-f", "lavfi", "-i", input, "-c:v", "libx264",
                    "-threads:v", "1", "-filter_threads", "1", "-pix_fmt", "yuv420p"])
                .arg(&path).output().expect("generate source footage");
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            sources.push(openreel_media::probe(path.to_str().unwrap()).unwrap());
        }

        let ctx = egui::Context::default();
        let mut thumbnails = GalleryThumbnails::new(&ctx);
        let requests = [
            (&sources[0], 0.9999, [255, 0, 0], [224, 126]),
            (&sources[1], 0.9999, [0, 0, 255], [60, 90]),
            (&sources[0], 1.0, [0, 255, 0], [224, 126]),
        ];
        let mut handles: Vec<Option<TextureHandle>> = vec![None; requests.len()];
        let mut images = HashMap::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while handles.iter().any(Option::is_none) {
            for (index, (source, time, _, _)) in requests.iter().enumerate() {
                handles[index] = thumbnails.texture(&ctx, source, *time).unwrap();
            }
            for (id, delta) in ctx.tex_manager().write().take_delta().set {
                let egui::ImageData::Color(image) = delta.image;
                images.insert(id, image);
            }
            assert!(Instant::now() < deadline, "thumbnails must finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        for ((source, time, expected, size), handle) in requests.iter().zip(&handles) {
            let handle = handle.as_ref().unwrap();
            assert_eq!(handle.size(), *size, "bounded without upscaling");
            let image = &images[&handle.id()];
            for pixel in &image.pixels {
                let distance: i32 = pixel.to_array()[..3]
                    .iter()
                    .zip(expected)
                    .map(|(actual, expected)| (i32::from(*actual) - expected).abs())
                    .sum();
                assert!(distance < 24, "{} at {time}: {pixel:?}", source.name);
            }
            let cached = thumbnails.texture(&ctx, source, *time).unwrap().unwrap();
            assert_eq!(cached.id(), handle.id(), "repaint reuses its texture");
        }

        let mut missing = sources[0].clone();
        missing.path = footage.0.join("missing.mp4").to_str().unwrap().to_owned();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match thumbnails.texture(&ctx, &missing, 0.5) {
                Err(_) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Ok(Some(_)) => panic!("a missing source must not borrow another card's image"),
            }
            assert!(Instant::now() < deadline, "failed thumbnails must finish");
        }
        assert!(thumbnails
            .texture(&ctx, &sources[0], 1.0)
            .unwrap()
            .is_some());
    }
}

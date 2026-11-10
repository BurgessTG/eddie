//! Media bin and import pipeline: placeholders, background probe and
//! thumbnail/waveform processing, and adding media to the timeline.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, TryRecvError};
use std::thread;

use openreel_engine::{format_timecode, ClipKind, Orientation};
use openreel_media::{BeatGrid, MediaInfo, RawFrame};

use super::sweep::{load_curation, SweepData, SweepStatus};
use super::Editor;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AssetState {
    Probing,
    Processing,
    Ready,
}

pub struct BinAsset {
    pub info: MediaInfo,
    pub size_bytes: u64,
    pub state: AssetState,
    pub peaks: Vec<f32>,
    /// Music only: the steady beat of an audio-only file, if it has one.
    pub beats: Option<BeatGrid>,
    /// Filmstrip stills by source time, from import. A front end takes
    /// them into its own textures, so they are held once.
    pub thumbs: Vec<(f64, RawFrame)>,
    pub sweep: SweepData,
}

pub struct ImportedAsset {
    pub info: MediaInfo,
    pub peaks: Vec<f32>,
    beats: Option<BeatGrid>,
    thumbs: Vec<(f64, RawFrame)>,
    /// A saved curation of this exact file. Restoring it is not curating.
    sweep: Option<SweepData>,
}

pub enum ImportMsg {
    Probed(Box<MediaInfo>),
    Ready(Box<ImportedAsset>),
    Failed(String, String),
    Finished,
}

impl Editor {
    pub fn asset(&self, path: &str) -> Option<&BinAsset> {
        self.assets.iter().find(|a| a.info.path == path)
    }

    /// The asset a script or command names, by path or by file name.
    pub fn asset_named(&self, name: &str) -> Option<&BinAsset> {
        self.assets
            .iter()
            .find(|a| a.info.path == name || a.info.name == name)
    }

    /// The picture size preview and export share: the largest source of the
    /// project's orientation on the timeline.
    pub fn canvas(&self) -> (u32, u32) {
        openreel_media::export_canvas(
            self.timeline.orientation,
            self.timeline
                .clips
                .iter()
                .filter(|clip| clip.kind == openreel_engine::ClipKind::Video)
                .filter_map(|clip| self.asset(clip.source.as_deref()?))
                .map(|asset| (asset.info.width, asset.info.height)),
        )
    }

    // -- import pipeline ------------------------------------------------------

    pub fn insert_placeholder(&mut self, path: &std::path::Path) {
        let path_str = path.to_string_lossy().into_owned();
        let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if let Some(existing) = self.assets.iter_mut().find(|a| a.info.path == path_str) {
            existing.size_bytes = size_bytes;
            existing.state = AssetState::Processing;
            return;
        }
        self.assets.push(BinAsset {
            info: MediaInfo {
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path_str.clone()),
                path: path_str,
                duration: 0.0,
                width: 0,
                height: 0,
                fps: 0.0,
                has_video: false,
                has_audio: false,
            },
            size_bytes,
            state: AssetState::Probing,
            peaks: Vec::new(),
            beats: None,
            thumbs: Vec::new(),
            sweep: SweepData::default(),
        });
    }

    /// Import files into the bin. Batches queue: asking for more
    /// files while a batch runs adds placeholders now and processes
    /// them when the current batch finishes, instead of dropping
    /// the request.
    pub fn spawn_import(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        for path in &paths {
            self.import_errors.remove(&*path.to_string_lossy());
            self.insert_placeholder(path);
        }
        if self.import_rx.is_some() {
            self.status = format!("Queued {} file(s) to import.", paths.len());
            self.import_queue.extend(paths);
            return;
        }
        self.start_import_batch(paths);
    }

    pub fn start_import_batch(&mut self, paths: Vec<PathBuf>) {
        let (tx, rx) = channel::<ImportMsg>();
        self.import_rx = Some(rx);
        let wake = self.wake.clone();
        let store_dir = self.curation_dir.clone();
        let thumbnails = self.make_thumbnails;
        thread::spawn(move || {
            // Files import concurrently; placeholders already hold bin
            // order. Each 4K thumbnail pass uses several slice threads.
            let next = AtomicUsize::new(0);
            let workers = paths.len().min(4);
            thread::scope(|scope| {
                for _ in 0..workers {
                    scope.spawn(|| {
                        while let Some(path) = paths.get(next.fetch_add(1, Ordering::Relaxed)) {
                            if !import_one(path, &tx, &wake, store_dir.as_deref(), thumbnails) {
                                return;
                            }
                        }
                    });
                }
            });
            let _ = tx.send(ImportMsg::Finished);
            wake.wake();
        });
    }

    pub fn poll_imports(&mut self) {
        let mut finished = false;
        loop {
            let msg = match self.import_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(msg)) => msg,
                Some(Err(TryRecvError::Empty)) => break,
                Some(Err(TryRecvError::Disconnected)) => {
                    finished = true;
                    break;
                }
                None => break,
            };
            match msg {
                ImportMsg::Probed(info) => {
                    let info = *info;
                    if let Some(existing) =
                        self.assets.iter_mut().find(|a| a.info.path == info.path)
                    {
                        existing.info = info;
                        existing.state = AssetState::Processing;
                    } else {
                        let size_bytes =
                            std::fs::metadata(&info.path).map(|m| m.len()).unwrap_or(0);
                        self.assets.push(BinAsset {
                            info,
                            size_bytes,
                            state: AssetState::Processing,
                            peaks: Vec::new(),
                            beats: None,
                            thumbs: Vec::new(),
                            sweep: SweepData::default(),
                        });
                    }
                }
                ImportMsg::Ready(imported) => {
                    let ImportedAsset {
                        info,
                        peaks,
                        beats,
                        thumbs,
                        sweep,
                    } = *imported;
                    let restored = sweep
                        .as_ref()
                        .is_some_and(|sweep| sweep.status == SweepStatus::Ready);
                    let sweep = sweep.unwrap_or_default();
                    self.status = if restored {
                        format!("Imported {} with its saved curation.", info.name)
                    } else {
                        format!("Imported {}", info.name)
                    };
                    let ready_path = info.path.clone();
                    // Fill an empty selection only; a background batch
                    // must not take the curator card from the user.
                    let keep_selection = self
                        .selected_asset
                        .as_deref()
                        .is_some_and(|path| self.assets.iter().any(|a| a.info.path == path));
                    if !keep_selection {
                        self.selected_asset = Some(ready_path.clone());
                    }
                    let size_bytes = std::fs::metadata(&info.path).map(|m| m.len()).unwrap_or(0);
                    if let Some(existing) =
                        self.assets.iter_mut().find(|a| a.info.path == info.path)
                    {
                        *existing = BinAsset {
                            info,
                            size_bytes,
                            state: AssetState::Ready,
                            peaks,
                            beats,
                            thumbs,
                            sweep,
                        };
                    } else {
                        self.assets.push(BinAsset {
                            info,
                            size_bytes,
                            state: AssetState::Ready,
                            peaks,
                            beats,
                            thumbs,
                            sweep,
                        });
                    }
                    // Import only makes the asset ready. Curation is
                    // an explicit Curate action in the media bin.
                    let _ = ready_path;
                }
                ImportMsg::Failed(path, err) => {
                    self.assets
                        .retain(|a| !(a.info.path == path && a.state != AssetState::Ready));
                    self.status = format!("Could not import {path}: {err}");
                    self.import_errors.insert(path, err);
                }
                ImportMsg::Finished => finished = true,
            }
        }
        if finished {
            self.assets.retain(|a| a.state != AssetState::Probing);
            self.import_rx = None;
            // Imports overwrite the open message; keep the missing count seen.
            if !self.missing_media.is_empty() {
                let count = self.missing_media.len();
                self.status = format!(
                    "{count} project media file{} missing. Use File → Locate missing media.",
                    if count == 1 { " is" } else { "s are" }
                );
            }
            if !self.import_queue.is_empty() {
                let next = std::mem::take(&mut self.import_queue);
                self.start_import_batch(next);
            }
        }
    }

    /// First lane with no clip overlapping [at, at+duration), or a
    /// freshly appended lane. Lanes are kind-agnostic.
    /// Take a file out of the bin. The file and its saved curation stay on
    /// disk, so importing it again restores both.
    pub fn remove_asset(&mut self, path: &str) {
        let Some(index) = self.assets.iter().position(|a| a.info.path == path) else {
            return;
        };
        let name = self.assets[index].info.name.clone();
        let used = self
            .timeline
            .clips
            .iter()
            .filter(|clip| clip.source.as_deref() == Some(path))
            .count();
        if used > 0 {
            self.status = format!(
                "{name} is in {used} timeline clip{}; delete {} first.",
                if used == 1 { "" } else { "s" },
                if used == 1 { "it" } else { "them" }
            );
            return;
        }
        if self.assets[index].state != AssetState::Ready {
            self.status = format!("{name} is still importing.");
            return;
        }
        if self.sweep_running.iter().any(|running| running == path) {
            self.status = format!("{name} is being curated; wait for it to finish.");
            return;
        }
        self.sweep_queue.retain(|queued| queued != path);
        self.assets.remove(index);
        if self.selected_asset.as_deref() == Some(path) {
            self.selected_asset = None;
        }
        self.status = format!("Removed {name} from the bin.");
    }

    pub fn first_free_track(&mut self, at: f64, duration: f64) -> usize {
        let end = at + duration;
        for track in 0..self.timeline.track_labels.len() {
            let blocked = self
                .timeline
                .clips
                .iter()
                .any(|c| c.track == track && c.start < end && at < c.end());
            if !blocked {
                return track;
            }
        }
        self.timeline.append_track()
    }

    /// Video of the other orientation never mixes into the project.
    /// Audio-only media fits any format.
    pub fn clashes_with_format(&self, info: &MediaInfo) -> bool {
        info.has_video && Orientation::of(info.width, info.height) != self.timeline.orientation
    }

    pub fn set_format(&mut self, format: Orientation) {
        self.timeline.orientation = format;
        let clashing = self
            .timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Video)
            .filter_map(|clip| self.asset(clip.source.as_deref()?))
            .filter(|asset| self.clashes_with_format(&asset.info))
            .count();
        self.status = match clashing {
            0 => format!("Format set to {}.", format.as_str()),
            1 => "1 clip doesn't match the new format.".to_owned(),
            n => format!("{n} clips don't match the new format."),
        };
    }

    pub fn add_asset_to_timeline(&mut self, path: &str, track: Option<usize>, at: f64) {
        let Some(asset) = self.asset(path) else {
            return;
        };
        let (name, duration) = (asset.info.name.clone(), asset.info.duration);
        self.add_asset_range_to_timeline(path, &name, track, at, 0.0, duration);
    }

    pub fn add_asset_range_to_timeline(
        &mut self,
        path: &str,
        title: &str,
        track: Option<usize>,
        at: f64,
        source_in: f64,
        duration: f64,
    ) {
        if let Err(error) =
            self.try_add_asset_range_to_timeline(path, title, track, at, source_in, duration)
        {
            self.status = error;
        }
    }

    /// The command path needs a typed failure, not a status that
    /// its caller can accidentally replace with a success message.
    pub fn try_add_asset_range_to_timeline(
        &mut self,
        path: &str,
        title: &str,
        track: Option<usize>,
        at: f64,
        source_in: f64,
        duration: f64,
    ) -> Result<openreel_engine::ClipId, String> {
        let Some(asset) = self.asset(path) else {
            return Err("That media is no longer in the bin.".to_owned());
        };
        if asset.state != AssetState::Ready {
            return Err(format!("Still importing {}", asset.info.name));
        }
        if self.clashes_with_format(&asset.info) {
            let other = Orientation::of(asset.info.width, asset.info.height);
            return Err(format!(
                "{} is {}; the project is {}.",
                asset.info.name,
                other.as_str(),
                self.timeline.orientation.as_str()
            ));
        }
        let asset_name = asset.info.name.clone();
        let source_duration = asset.info.duration;
        let has_video = asset.info.has_video;
        // Baked-in montage music must not ride into a new edit, so the
        // add path mutes video whose added range is mostly curator-flagged
        // music bed. Audio-only media is the new soundtrack itself and is
        // never muted. The volume slider in Details brings it back.
        let range_end = source_in + duration;
        let bed_overlap = asset.sweep.music_bed_overlap(source_in, range_end);
        let starts_muted =
            self.mute_music_beds && has_video && duration > 0.0 && bed_overlap >= duration * 0.5;
        // Audio-only music ducks under speech by default. A steady beat or a
        // curator music-bed flag marks it as music; a voiceover has neither
        // and must stay unducked, or it would stop counting as speech.
        let ducks = !has_video
            && (asset.beats.is_some() || (duration > 0.0 && bed_overlap >= duration * 0.5));
        let source_in = source_in.clamp(0.0, source_duration.max(0.0));
        let duration = duration.min((source_duration - source_in).max(0.0));
        if duration <= 0.0 {
            return Err("That clip range is outside the media.".to_owned());
        }
        let name = if title.trim().is_empty() {
            asset_name
        } else {
            title.to_owned()
        };
        let kind = if has_video {
            ClipKind::Video
        } else {
            ClipKind::Audio
        };
        // An explicit drop lane wins (one past the end appends a
        // lane); otherwise the first lane free at that span.
        let track = match track {
            Some(requested) => {
                let track = requested.min(self.timeline.track_labels.len());
                self.timeline.ensure_track(track);
                track
            }
            None => self.first_free_track(at.max(0.0), duration),
        };
        let id = self.timeline.add_media_clip(
            &name,
            kind,
            track,
            at.max(0.0),
            duration,
            source_in,
            path,
        );
        if let Some(clip) = self.timeline.clip_mut(id) {
            if starts_muted {
                clip.volume = 0.0;
            }
            clip.duck = ducks;
        }
        self.selected = Some(id);
        self.status = format!(
            "Added {name} at {}{}",
            format_timecode(at, self.timeline.fps),
            if starts_muted {
                " (muted: music bed)"
            } else {
                ""
            }
        );
        Ok(id)
    }

    // -- playback --------------------------------------------------------------
}

pub fn asset_meta(asset: &BinAsset) -> String {
    let info = &asset.info;
    let size = format_file_size(asset.size_bytes);
    match asset.state {
        AssetState::Probing => format!("{} · {size}", file_extension(&info.name)),
        AssetState::Processing | AssetState::Ready => {
            let dur = format_duration(info.duration);
            if info.has_video {
                format!("{}×{} · {dur} · {size}", info.width, info.height)
            } else {
                format!("Audio · {dur} · {size}")
            }
        }
    }
}

pub fn file_extension(name: &str) -> String {
    name.rsplit('.')
        .next()
        .filter(|ext| ext.len() <= 5 && !ext.is_empty())
        .map(|ext| ext.to_uppercase())
        .unwrap_or_else(|| "FILE".to_owned())
}

pub fn format_file_size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{bytes} B")
    }
}

pub fn format_duration(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

pub fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// Probe, waveform and thumbnails for one file. False once the app has
/// dropped the receiver.
fn import_one(
    path: &std::path::Path,
    tx: &std::sync::mpsc::Sender<ImportMsg>,
    wake: &super::Wake,
    store_dir: Option<&std::path::Path>,
    thumbnails: bool,
) -> bool {
    let path_str = path.to_string_lossy().into_owned();
    // A raw stream has no timestamps to seek or place by.
    let probed = openreel_media::probe(&path_str).and_then(|info| match info.duration > 0.0 {
        true => Ok(info),
        false => Err(
            "no duration; put a raw stream in a container first (ffmpeg -i in -c copy out.mp4)"
                .to_owned(),
        ),
    });
    let msg = match probed {
        Ok(info) => {
            if tx.send(ImportMsg::Probed(Box::new(info.clone()))).is_err() {
                return false;
            }
            wake.wake();
            // Decoding a file's whole sound is the slow part of an import:
            // do it once per file, not on every open or command.
            let rate = openreel_media::WAVEFORM_PEAKS_PER_SEC;
            let saved = store_dir.and_then(|dir| super::sweep::load_peaks(dir, &path_str, rate));
            let peaks = match saved {
                Some(peaks) => peaks,
                None if info.has_audio => {
                    let peaks = openreel_media::waveform_peaks(&path_str, info.duration, rate)
                        .unwrap_or_default();
                    if let Some(dir) = store_dir.filter(|_| !peaks.is_empty()) {
                        let _ = super::sweep::save_peaks(dir, &path_str, rate, &peaks);
                    }
                    peaks
                }
                None => Vec::new(),
            };
            let thumbs = if thumbnails && info.has_video {
                openreel_media::thumbnails(&path_str, info.width, info.height, info.duration)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let beats = (!info.has_video)
                .then(|| {
                    openreel_media::beat_grid(
                        &peaks,
                        f64::from(openreel_media::WAVEFORM_PEAKS_PER_SEC),
                    )
                })
                .flatten();
            // A saved transcript is the latest one (curation saves its own
            // there too), and gives scripts the speech without a curation.
            let sweep = store_dir.and_then(|dir| {
                let transcript = super::sweep::load_transcript(dir, &path_str);
                let mut sweep = load_curation(dir, &path_str);
                if let Some((segments, _)) = transcript {
                    sweep.get_or_insert_with(SweepData::default).transcript = segments;
                }
                sweep
            });
            ImportMsg::Ready(Box::new(ImportedAsset {
                info,
                peaks,
                beats,
                thumbs,
                sweep,
            }))
        }
        Err(e) => ImportMsg::Failed(path_str, e),
    };
    let sent = tx.send(msg).is_ok();
    wake.wake();
    sent
}

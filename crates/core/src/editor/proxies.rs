//! Proxies (Settings → Proxies, as CapCut's Proxy setting). While it is
//! on, each video wider than the preview gets a light copy at the
//! preview's width (`openreel_media::make_proxy`), made one at a time on a
//! worker thread in `proxies/` beside the settings, the timeline's files
//! first. The preview's players and stills then decode the copy instead of
//! the file (`preview_file`); the timeline, sound, export and every picture
//! command still read the file itself. A command-line editor has no
//! `proxy_dir`, so it makes none.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use openreel_engine::ClipKind;
use openreel_media::MediaInfo;

use super::media::AssetState;
use super::sweep::path_hash;
use super::{Editor, PREVIEW_W};

/// `proxies/` beside the settings file.
pub fn default_dir() -> Option<PathBuf> {
    Some(super::settings::config_path()?.parent()?.join("proxies"))
}

/// Whether the preview gains from a copy of `info`'s file: footage wider
/// than the preview decodes, without an alpha channel the copy would lose.
fn wants_proxy(info: &MediaInfo) -> bool {
    info.has_video && !info.photo && !info.alpha && info.width > PREVIEW_W
}

/// A file's copy and its picture's size.
pub(crate) struct Proxy {
    pub(crate) path: String,
    pub(crate) size: (u32, u32),
}

enum Msg {
    Progress(String, f64),
    Done(String, Result<Proxy, String>),
}

struct Running {
    source: String,
    progress: f64,
    cancel: Arc<AtomicBool>,
}

/// Each file's copy, made or being made.
pub(crate) struct Proxies {
    ready: HashMap<String, Proxy>,
    failed: HashMap<String, String>,
    running: Option<Running>,
    /// Whether the preview was last asked for pictures with proxies in
    /// use; a change sends playing lanes to the other file.
    in_use: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl Default for Proxies {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            ready: HashMap::new(),
            failed: HashMap::new(),
            running: None,
            in_use: false,
            tx,
            rx,
        }
    }
}

impl Drop for Proxies {
    /// A copy under way stops with its editor, so FFmpeg does not go on
    /// decoding the whole file after the window closed. It stops within a
    /// tenth of a second; this waits at most a second.
    fn drop(&mut self) {
        let Some(running) = &self.running else {
            return;
        };
        running.cancel.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(1);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.rx.recv_timeout(left) {
                Ok(Msg::Progress(..)) => {}
                Ok(Msg::Done(..)) | Err(_) => break,
            }
        }
    }
}

impl Editor {
    fn proxies_on(&self) -> bool {
        self.settings.proxies && self.proxy_dir.is_some()
    }

    /// The copy the preview decodes for `source`, while proxies are on
    /// and it is made.
    pub(crate) fn proxy(&self, source: &str) -> Option<&Proxy> {
        self.proxies_on()
            .then(|| self.proxies.ready.get(source))
            .flatten()
    }

    /// The file the preview decodes for `source`: its proxy, or itself.
    pub(crate) fn preview_file<'a>(&'a self, source: &'a str) -> &'a str {
        self.proxy(source).map_or(source, |proxy| &proxy.path)
    }

    /// Whether the preview plays `source` from its proxy.
    pub fn has_proxy(&self, source: &str) -> bool {
        self.proxy(source).is_some()
    }

    /// While a proxy is made: how far along it is, 0 to 1, and how many
    /// files wait for one after it.
    pub fn proxy_progress(&self) -> Option<(f64, usize)> {
        let running = self.proxies.running.as_ref()?;
        let waiting = self
            .proxy_wanted()
            .filter(|source| *source != running.source)
            .count();
        Some((running.progress, waiting))
    }

    /// A file imported again may have changed: its copy is made again.
    pub(super) fn forget_proxy(&mut self, source: &str) {
        self.proxies.ready.remove(source);
        self.proxies.failed.remove(source);
    }

    /// Files that want a proxy and have none: the timeline's footage in
    /// time order, then the rest of the bin and Eddie's own files.
    fn proxy_wanted(&self) -> impl Iterator<Item = &str> {
        let mut used: Vec<_> = self
            .timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Video)
            .filter_map(|clip| Some((clip.start, clip.source.as_deref()?)))
            .collect();
        used.sort_by(|a, b| a.0.total_cmp(&b.0));
        let bin = self.assets.iter().chain(&self.made);
        let mut seen = std::collections::HashSet::new();
        used.into_iter()
            .map(|(_, source)| source)
            .chain(bin.map(|asset| asset.info.path.as_str()))
            .filter(move |source| seen.insert(*source))
            .filter(|source| {
                self.asset(source).is_some_and(|asset| {
                    asset.state == AssetState::Ready && wants_proxy(&asset.info)
                })
            })
            .filter(|source| {
                !self.proxies.ready.contains_key(*source)
                    && !self.proxies.failed.contains_key(*source)
            })
    }

    /// Once per frame: take in made copies, start the next, and stop while
    /// proxies are off.
    pub(super) fn poll_proxies(&mut self) {
        let mut made = false;
        while let Ok(msg) = self.proxies.rx.try_recv() {
            match msg {
                Msg::Progress(source, share) => {
                    if let Some(running) = &mut self.proxies.running {
                        if running.source == source {
                            running.progress = share;
                        }
                    }
                }
                Msg::Done(source, result) => {
                    let Some(running) = self.proxies.running.take() else {
                        continue;
                    };
                    match result {
                        Ok(proxy) => {
                            self.proxies.ready.insert(source, proxy);
                            made = true;
                        }
                        // Stopped by turning proxies off: try again later.
                        Err(_) if running.cancel.load(Ordering::Relaxed) => {}
                        Err(error) => {
                            self.proxies.failed.insert(source, error);
                        }
                    }
                }
            }
        }
        let on = self.proxies_on();
        if !on {
            if let Some(running) = &self.proxies.running {
                running.cancel.store(true, Ordering::Relaxed);
            }
        } else if self.proxies.running.is_none() {
            let next = self.proxy_wanted().next().map(str::to_owned);
            if let Some(source) = next {
                self.start_proxy(source);
            }
        }
        if made || on != self.proxies.in_use {
            self.proxies.in_use = on;
            self.replay_previews();
        }
        if self.proxies.running.is_some() {
            self.frame_within(Duration::from_millis(250));
        }
    }

    fn start_proxy(&mut self, source: String) {
        let Some(folder) = self.proxy_dir.clone() else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.proxies.running = Some(Running {
            source: source.clone(),
            progress: 0.0,
            cancel: Arc::clone(&cancel),
        });
        let tx = self.proxies.tx.clone();
        let wake = self.wake.clone();
        let spawned = thread::Builder::new().name("proxy".into()).spawn(move || {
            let mut progress = |share: f64| {
                let _ = tx.send(Msg::Progress(source.clone(), share));
                wake.wake();
            };
            let made = make(&source, &folder, &cancel, &mut progress);
            let _ = tx.send(Msg::Done(source, made));
            wake.wake();
        });
        if spawned.is_err() {
            self.proxies.running = None;
        }
    }

    /// Send the preview to the file it now decodes: playing lanes ask again
    /// from the playhead on their next sync, and the paused picture is
    /// decoded again.
    fn replay_previews(&mut self) {
        self.forget_stills();
        for lane in 0..self.players.len() {
            let player = &mut self.players[lane];
            player.key = None;
            player.ready = false;
            self.stop_incoming(lane);
        }
    }
}

/// On a worker: `source`'s copy in `folder`, named by its name and a hash
/// of its path, size, modification time and width, written unless it is
/// there.
fn make(
    source: &str,
    folder: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<Proxy, String> {
    let meta = std::fs::metadata(source).map_err(|e| format!("{source}: {e}"))?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());
    let key = format!("{source}\n{}\n{modified}\n{PREVIEW_W}", meta.len());
    let stem = Path::new(source)
        .file_stem()
        .map_or("clip".into(), |stem| stem.to_string_lossy());
    let file = folder.join(format!("{stem}-{:016x}.mov", path_hash(&key)));
    if file.exists() {
        // Kept from `graphics::prune` while it is used.
        let _ = std::fs::File::options()
            .append(true)
            .open(&file)
            .and_then(|f| f.set_modified(SystemTime::now()));
    } else {
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
        openreel_media::make_proxy(source, PREVIEW_W, &file, progress, cancel)?;
    }
    let path = file.to_string_lossy().into_owned();
    let info = openreel_media::probe(&path)?;
    Ok(Proxy {
        path,
        size: (info.width, info.height),
    })
}

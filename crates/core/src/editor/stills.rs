//! Paused pictures: on every lane that shows, the still under the playhead,
//! its neighbours, and during a transition the picture it goes to.

use openreel_engine::Clip;
use openreel_media::RawFrame;

use super::playback::video_key;
use super::{StillKey, PREVIEW_W, SCRUB_W, STILL_RETRY};
use crate::Editor;

/// What a still request is for, on a lane: its picture, a neighbour an
/// arrow step reaches, or the picture a transition at the playhead goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StillUse {
    Program(usize),
    Prefetch(usize),
    Incoming(usize),
}

/// What a still request decodes: its key, source path and size, and time.
pub struct StillTarget {
    pub key: StillKey,
    source: String,
    size: (u32, u32),
    time: f64,
}

impl Editor {
    /// Drop cached stills and the answer in flight after the timeline is
    /// replaced, so no picture from before can show.
    pub fn forget_stills(&mut self) {
        if self.rehearsing {
            return;
        }
        for player in &mut self.players {
            player.still = None;
            player.incoming.still = None;
            player.incoming.shown = None;
        }
        self.still_cache.clear();
        self.still_failed.clear();
        if let Some((_, key, _)) = &mut self.still_inflight {
            *key = None;
        }
    }

    /// The still of the top picture at timeline time `at`, decoded at
    /// `width`.
    pub fn still_target(&self, at: f64, width: u32) -> Option<StillTarget> {
        self.clip_still(self.stack().at(at).last()?.clip, at, width)
    }

    /// The still of `clip` at timeline time `at`. A transition shows a clip
    /// past its edges, where a file without pictures holds its first or
    /// last.
    fn clip_still(&self, clip: &Clip, at: f64, width: u32) -> Option<StillTarget> {
        let source = clip.source.as_deref()?;
        let info = &self.asset(source)?.info;
        let time = clip.source_at(at).clamp(0.0, info.duration.max(0.0));
        let (source, size) = match self.proxy(source) {
            Some(proxy) => (proxy.path.clone(), proxy.size),
            None => (source.to_owned(), (info.width, info.height)),
        };
        Some(StillTarget {
            key: (clip.id, (time * self.timeline.fps).round() as i64, width),
            size,
            source,
            time,
        })
    }

    pub(super) fn request_still(&mut self, target: StillTarget, used: StillUse) {
        self.still_gen += 1;
        self.still_inflight = Some((self.still_gen, Some(target.key), used));
        let width = target.key.2;
        self.still_decoder.request(
            self.still_gen,
            target.source,
            target.size.0,
            target.size.1,
            target.time,
            width,
            width <= SCRUB_W,
        );
    }

    /// Show a decoded still where it belongs now: the newest finished
    /// picture of a lane's clip shows even when the pointer has moved on
    /// (the next request chases the pointer), and a prefetched neighbour
    /// only once the playhead is on it.
    pub(super) fn show_still(&mut self, used: StillUse, key: StillKey, frame: &RawFrame) {
        match used {
            StillUse::Program(lane) | StillUse::Prefetch(lane) => {
                let Some(clip) = self.lane_clip(lane, self.playhead) else {
                    return;
                };
                let shows = match used {
                    StillUse::Program(_) => clip.id == key.0,
                    _ => self
                        .clip_still(&clip, self.playhead, PREVIEW_W)
                        .is_some_and(|target| target.key == key),
                };
                if shows {
                    self.show_lane(lane, video_key(&clip), frame);
                    self.player(lane).still = Some(key);
                }
            }
            StillUse::Incoming(lane) => {
                let Some(to) = self.incoming_clip(lane, self.playhead) else {
                    return;
                };
                if to.id == key.0 {
                    let to = video_key(to);
                    self.preview.show_incoming(lane, frame);
                    let incoming = &mut self.player(lane).incoming;
                    incoming.shown = to;
                    incoming.still = Some(key);
                }
            }
        }
    }

    /// Show the cached still `key` with `show`, and make it the most
    /// recently used. False when it is not cached.
    fn show_cached(&mut self, key: StillKey, show: impl FnOnce(&mut Self, &RawFrame)) -> bool {
        let Some(index) = self
            .still_cache
            .iter()
            .position(|(cached, _)| *cached == key)
        else {
            return false;
        };
        let (key, frame) = self.still_cache.remove(index).expect("found above");
        show(self, &frame);
        self.still_cache.push_back((key, frame));
        true
    }

    /// A relinked or repaired file gets another try, but not a busy loop.
    fn still_failed_recently(&self, key: StillKey) -> bool {
        self.still_failed
            .iter()
            .any(|(failed, at)| *failed == key && at.elapsed() < STILL_RETRY)
    }

    pub fn request_still_if_needed(&mut self) {
        // Two tiers: fast small frames while interacting, full quality when idle.
        let width = if self.scrubbing() { SCRUB_W } else { PREVIEW_W };
        let playhead = self.playhead;
        let pictures: Vec<(usize, Clip, Option<Clip>)> = self
            .stack()
            .at(playhead)
            .iter()
            .map(|picture| {
                let to = picture
                    .transition
                    .and_then(|span| self.timeline.clip(span.to));
                (picture.lane, picture.clip.clone(), to.cloned())
            })
            .collect();
        for (lane, clip, to) in &pictures {
            let lane = *lane;
            if let Some(target) = self.clip_still(clip, playhead, width) {
                let key = target.key;
                let shown = video_key(clip);
                let player = self.player(lane);
                if player.still != Some(key) || player.shown != shown {
                    let cached = self.show_cached(key, |editor, frame| {
                        editor.show_lane(lane, shown, frame);
                        editor.player(lane).still = Some(key);
                    });
                    if !cached {
                        // One request at a time: while it decodes, the
                        // pointer can move on; its answer shows, then this
                        // asks for wherever the pointer is.
                        if self.still_inflight.is_none() && !self.still_failed_recently(key) {
                            self.request_still(target, StillUse::Program(lane));
                        }
                        return;
                    }
                }
            }
            // During a transition, the picture it goes to.
            let Some(to) = to else {
                continue;
            };
            let Some(target) = self.clip_still(to, playhead, width) else {
                continue;
            };
            let key = target.key;
            let shown = video_key(to);
            let incoming = &self.player(lane).incoming;
            if incoming.still != Some(key) || incoming.shown != shown {
                let cached = self.show_cached(key, |editor, frame| {
                    editor.preview.show_incoming(lane, frame);
                    let incoming = &mut editor.player(lane).incoming;
                    incoming.shown = shown;
                    incoming.still = Some(key);
                });
                if !cached {
                    if self.still_inflight.is_none() && !self.still_failed_recently(key) {
                        self.request_still(target, StillUse::Incoming(lane));
                    }
                    return;
                }
            }
        }
        // Paused on a picture: decode the frames an arrow step reaches next,
        // so the step shows from the cache in the same UI frame.
        if width != PREVIEW_W || self.still_inflight.is_some() {
            return;
        }
        let frame = 1.0 / self.timeline.fps;
        for (lane, ..) in &pictures {
            for at in [playhead + frame, playhead - frame] {
                let at = self.timeline.snap(at);
                if at < 0.0 || at > self.timeline.end() {
                    continue;
                }
                let neighbour = self
                    .lane_clip(*lane, at)
                    .and_then(|clip| self.clip_still(&clip, at, PREVIEW_W));
                if let Some(neighbour) = neighbour {
                    let cached = self
                        .still_cache
                        .iter()
                        .any(|(key, _)| *key == neighbour.key);
                    if !cached && !self.still_failed_recently(neighbour.key) {
                        self.request_still(neighbour, StillUse::Prefetch(*lane));
                        return;
                    }
                }
            }
        }
    }
}

//! Paused pictures: the still under the playhead, its neighbours, and
//! during a transition the picture it goes to.

use openreel_engine::Clip;

use super::playback::video_key;
use super::{StillKey, PREVIEW_W, SCRUB_W, STILL_RETRY};
use crate::Editor;

/// What a still request is for: the program picture, a neighbour an arrow
/// step reaches, or the picture a transition at the playhead goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StillUse {
    Program,
    Prefetch,
    Incoming,
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
        self.still_shown = None;
        self.incoming.still = None;
        self.incoming.shown = None;
        self.still_cache.clear();
        self.still_failed.clear();
        if let Some((_, key, _)) = &mut self.still_inflight {
            *key = None;
        }
    }

    /// The still for the program picture at timeline time `at`, decoded at
    /// `width`.
    pub fn still_target(&self, at: f64, width: u32) -> Option<StillTarget> {
        self.clip_still(self.timeline.program_video(at)?, at, width)
    }

    /// The still of `clip` at timeline time `at`. A transition shows a clip
    /// past its edges, where a file without pictures holds its first or
    /// last.
    fn clip_still(&self, clip: &Clip, at: f64, width: u32) -> Option<StillTarget> {
        let source = clip.source.clone()?;
        let info = &self.asset(&source)?.info;
        let time = clip.source_at(at).clamp(0.0, info.duration.max(0.0));
        Some(StillTarget {
            key: (clip.id, (time * self.timeline.fps).round() as i64, width),
            size: (info.width, info.height),
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

    pub fn request_still_if_needed(&mut self) {
        // Two tiers: fast small frames while interacting, full quality when idle.
        let width = if self.scrubbing() { SCRUB_W } else { PREVIEW_W };
        let Some(target) = self.still_target(self.playhead, width) else {
            return;
        };
        let key = target.key;
        let cached =
            |app: &Self, key: StillKey| app.still_cache.iter().position(|(c, _)| *c == key);
        // A relinked or repaired file gets another try, but not a busy loop.
        let failed = |app: &Self, key: StillKey| {
            app.still_failed
                .iter()
                .any(|(failed, at)| *failed == key && at.elapsed() < STILL_RETRY)
        };
        if self.still_shown != Some(key) || !self.preview_shown {
            if let Some(index) = cached(self, key) {
                // Most recently used moves to the back.
                let (key, frame) = self.still_cache.remove(index).unwrap();
                self.show_preview(&frame);
                self.still_shown = Some(key);
                self.still_cache.push_back((key, frame));
            } else {
                // One request at a time: while it decodes, the pointer can
                // move on; its answer shows, then this asks for wherever the
                // pointer is.
                if self.still_inflight.is_none() && !failed(self, key) {
                    self.request_still(target, StillUse::Program);
                }
                return;
            }
        }
        // During a transition, the picture it goes to.
        let incoming = self.incoming_clip(self.playhead).and_then(|clip| {
            Some((
                self.clip_still(clip, self.playhead, width)?,
                video_key(clip),
            ))
        });
        if let Some((target, clip)) = incoming {
            let key = target.key;
            if self.incoming.still != Some(key) || self.incoming.shown != clip {
                if let Some(index) = cached(self, key) {
                    let (key, frame) = self.still_cache.remove(index).unwrap();
                    self.preview.show_incoming(&frame);
                    self.incoming.shown = clip;
                    self.incoming.still = Some(key);
                    self.still_cache.push_back((key, frame));
                } else {
                    if self.still_inflight.is_none() && !failed(self, key) {
                        self.request_still(target, StillUse::Incoming);
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
        for at in [self.playhead + frame, self.playhead - frame] {
            let at = self.timeline.snap(at);
            if at < 0.0 || at > self.timeline.end() {
                continue;
            }
            if let Some(neighbor) = self.still_target(at, PREVIEW_W) {
                if cached(self, neighbor.key).is_none() && !failed(self, neighbor.key) {
                    self.request_still(neighbor, StillUse::Prefetch);
                    return;
                }
            }
        }
    }
}

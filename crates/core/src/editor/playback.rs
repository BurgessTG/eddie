//! The transport and the program pictures: play, seek, shuttle, and each
//! lane's video following the audio clock.

use std::time::Instant;

use openreel_engine::{Clip, ClipKind};
use openreel_media::FrameStream;

use super::{VideoKey, PREVIEW_W, SHUTTLE_MAX, STILL_CACHE_CAP};
use crate::Editor;

/// What a video stream is asked for: a clip's identity, source and
/// mapping, so editing the mapping restarts it.
pub(super) fn video_key(clip: &Clip) -> Option<VideoKey> {
    Some((
        clip.id,
        clip.source.clone()?,
        clip.start.to_bits(),
        clip.source_in.to_bits(),
        clip.speed.to_bits(),
    ))
}

impl Editor {
    pub fn stop_sources(&mut self) {
        if self.rehearsing {
            return;
        }
        for lane in 0..self.players.len() {
            let player = &mut self.players[lane];
            if let Some(stream) = &player.stream {
                stream.cancel_preview();
            }
            player.key = None;
            player.ready = false;
            // A still decoding now still answers its own frame; let it land.
            player.still = None;
            self.stop_incoming(lane);
        }
        self.stream_failed = false;
        self.audio_failed = false;
        self.clock_playhead = None;
        if let Some(audio) = &mut self.audio {
            audio.seek(self.playhead);
            audio.set_running(false);
        }
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.shuttle = 0.0;
        if playing {
            self.curator.open = false;
            if self.stream_failed {
                self.stop_sources();
            }
        }
        if playing && self.playhead >= self.timeline.end() {
            self.playhead = 0.0;
        }
        self.playing = playing;
        self.forget_shown_rates();
        self.playing_since = playing.then(Instant::now);
        if !playing {
            self.stop_sources();
            if self.status == "Buffering video." || self.status == "Playing." {
                self.status = "Paused.".to_owned();
            }
        }
    }

    pub fn seek(&mut self, time: f64) {
        self.playhead = self
            .timeline
            .snap(time.clamp(0.0, self.timeline.end().max(0.0)));
        // The current picture stays up until the new one arrives; a different
        // clip's picture is cleared in poll_frames.
        self.stop_sources();
    }

    /// Ruler, clip drag or monitor scrubber held: preview from low-res
    /// stills instead of restarting playback on every move.
    pub fn scrubbing(&self) -> bool {
        self.viewport.interacting || self.transport_scrubbing || self.shuttle != 0.0
    }

    /// J and L: the first L plays normally; each repeat in the same
    /// direction doubles the speed up to 8x. Reverse and faster-than-1x
    /// runs are silent and preview low-res stills.
    pub fn shuttle(&mut self, forward: bool) {
        let sign = if forward { 1.0 } else { -1.0 };
        let speed = if self.shuttle * sign > 0.0 {
            (self.shuttle.abs() * 2.0).min(SHUTTLE_MAX)
        } else if forward && self.playing {
            2.0
        } else {
            1.0
        };
        if forward && speed == 1.0 {
            self.curator.open = false;
            self.set_playing(true);
            self.status = "Playing.".to_owned();
            return;
        }
        self.set_playing(false);
        self.curator.open = false;
        self.shuttle = sign * speed;
        self.status = format!("{} {speed}x.", if forward { "Forward" } else { "Reverse" });
    }

    /// The next edge after `after` where lane `lane` shows a different
    /// clip, with its source and source time there, as `sync_playback`
    /// computes them at that edge. A transition's clip takes over at its
    /// end, and a lane that a lane over it stops hiding shows again. Edges
    /// where the lane stops showing need no picture.
    pub fn next_video_change(&self, lane: usize, after: f64) -> Option<(f64, Option<(&str, f64)>)> {
        let stack = self.stack();
        let shown = |time| stack.lane(lane, time).map(|picture| picture.clip.id);
        let mut edges: Vec<f64> = self
            .timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Video)
            .flat_map(|clip| [clip.start, clip.end()])
            .chain(stack.spans.iter().flat_map(|span| [span.start, span.end]))
            .filter(|edge| *edge > after)
            .collect();
        edges.sort_by(f64::total_cmp);
        edges.dedup();
        // A clip shown again after a lane over it stops hiding it counts:
        // its lane let its picture go meanwhile.
        let mut before = shown(after);
        let (edge, clip) = edges.into_iter().find_map(|edge| {
            let now = shown(edge);
            let change = now.filter(|id| Some(*id) != before);
            before = now;
            Some((edge, self.timeline.clip(change?)?))
        })?;
        let source = clip
            .source
            .as_deref()
            .map(|source| (source, clip.source_at(edge)));
        Some((edge, source))
    }

    /// How far the audio clock may run before a picture must be ready: on
    /// every lane, the next cut it shows, or the one after when its worker
    /// has already prerolled that cut's first picture, and at most the start
    /// of a transition whose incoming picture has not arrived.
    pub fn clock_limit(&self) -> f64 {
        let end = self.timeline.end();
        if self.waiting_for_video() {
            return self.playhead;
        }
        let mut limit = end;
        for lane in self.playing_lanes() {
            let lane_limit = match self.next_video_change(lane, self.playhead) {
                None => end,
                Some((edge, source)) => {
                    let stream = self.players.get(lane).and_then(|p| p.stream.as_ref());
                    let prerolled = source.zip(stream).is_some_and(|((path, time), stream)| {
                        stream.prefetch_ready(path, time, PREVIEW_W)
                    });
                    if prerolled {
                        self.next_video_change(lane, edge)
                            .map_or(end, |(after, _)| after)
                    } else {
                        edge
                    }
                }
            };
            limit = limit.min(lane_limit);
            if let Some(start) = self.incoming_limit(lane, limit) {
                limit = limit.min(start);
            }
        }
        limit.min(end)
    }

    pub fn waiting_for_video(&self) -> bool {
        self.playing
            && self.playing_lanes().any(|lane| {
                let wanted = self.lane_clip(lane, self.playhead);
                let player = self.players.get(lane);
                wanted.as_ref().and_then(video_key).is_some_and(|key| {
                    player.is_none_or(|player| player.key.as_ref() != Some(&key) || !player.ready)
                }) || self
                    .incoming_limit(lane, self.playhead + 1e-9)
                    .is_some_and(|start| start <= self.playhead)
            })
    }

    pub fn video_playback_failed(&mut self, error: String) {
        self.playing = false;
        self.stream_failed = true;
        if let Some(audio) = &self.audio {
            audio.set_running(false);
        }
        self.status = format!("Video playback failed: {error}");
    }

    pub fn poll_frames(&mut self) {
        let stack: Vec<(usize, Clip)> = self
            .stack()
            .at(self.playhead)
            .iter()
            .map(|picture| (picture.lane, picture.clip.clone()))
            .collect();
        for lane in self.playing_lanes() {
            let wanted = stack
                .iter()
                .find(|(shown, _)| *shown == lane)
                .and_then(|(_, clip)| video_key(clip));
            self.drop_stale_picture(lane, wanted.as_ref());
        }
        let wants_still = !self.playing || self.scrubbing();
        while let Ok((gen, result)) = self.still_rx.try_recv() {
            let Some((pending, key, used)) = self.still_inflight else {
                continue;
            };
            if gen != pending {
                continue;
            }
            self.still_inflight = None;
            let Some(key) = key else {
                continue;
            };
            let Ok(frame) = result else {
                self.still_failed.retain(|(failed, _)| *failed != key);
                if self.still_failed.len() >= STILL_CACHE_CAP {
                    self.still_failed.pop_front();
                }
                self.still_failed.push_back((key, Instant::now()));
                continue;
            };
            if wants_still {
                self.show_still(used, key, &frame);
            }
            self.still_cache.retain(|(cached, _)| *cached != key);
            if self.still_cache.len() >= STILL_CACHE_CAP {
                self.still_cache.pop_front();
            }
            self.still_cache.push_back((key, frame));
        }
        if wants_still {
            return;
        }
        for (lane, clip) in stack {
            self.poll_incoming(lane);
            self.poll_lane(lane, &clip);
        }
    }

    /// Take lane `lane`'s newest picture of `clip` from its worker.
    fn poll_lane(&mut self, lane: usize, clip: &Clip) {
        let key = video_key(clip);
        let Some(player) = self.players.get(lane) else {
            return;
        };
        // A boundary or seek may precede sync_playback in this UI frame.
        // Never upload the previous generation as this clip's picture.
        if key.is_none() || player.key != key {
            return;
        }
        let result = player.stream.as_ref().and_then(|stream| {
            stream.preview_frame(player.generation, clip.source_at(self.playhead))
        });
        match result {
            Some(Ok(frame)) => {
                self.show_lane(lane, key, &frame);
                let player = self.player(lane);
                player.still = None;
                player.ready = true;
                if let Some(stream) = &player.stream {
                    stream.recycle(frame.rgba);
                }
                if self.status == "Buffering video." {
                    self.status = "Playing.".to_owned();
                }
            }
            Some(Err(error)) => self.video_playback_failed(error),
            None => {}
        }
    }

    /// Keep the video streams and audio players aligned with the playhead.
    /// Restarts sources on seeks and clip-boundary crossings.
    pub fn sync_playback(&mut self) {
        let now = self.last_now.unwrap_or(0.0);
        if self.last_sync_time > 0.0 && self.shuttle == 0.0 {
            let expected = if self.playing && !self.waiting_for_video() {
                self.last_sync_playhead + (now - self.last_sync_time)
            } else {
                self.last_sync_playhead
            };
            if (self.playhead - expected).abs() > 0.35 {
                self.stop_sources();
            }
        }
        self.last_sync_playhead = self.playhead;
        self.last_sync_time = now;

        // While scrubbing/dragging, never (re)spawn playback processes —
        // still frames carry the picture until the pointer settles.
        if self.scrubbing() {
            if self.players.iter().any(|player| player.key.is_some()) || self.audio_running() {
                self.stop_sources();
            }
            self.request_still_if_needed();
            return;
        }

        if !self.playing {
            // A terminal decode failure pauses visibly instead of consuming
            // an unseen clip or automatically retrying it through stills.
            if self.stream_failed {
                return;
            }
            if self.players.iter().any(|player| player.key.is_some()) || self.audio_running() {
                self.stop_sources();
            }
            self.request_still_if_needed();
            return;
        }

        for lane in self.playing_lanes() {
            self.sync_lane(lane);
            self.sync_incoming(lane);
            if self.stream_failed {
                return;
            }
        }
        self.sync_audio();
        if self.waiting_for_video() {
            // Preroll can exceed a short excerpt. The clock limit holds the
            // shared transport and its audio until the first picture.
            // A hardware failure during buffering keeps its notice visible.
            if !self.status.starts_with("Hardware decode failed") {
                self.status = "Buffering video.".to_owned();
            }
            // A held clock is not a seek, even if the next UI tick is late.
            self.last_sync_time = 0.0;
        }
    }

    /// Ask lane `lane`'s worker for the clip it shows at the playhead, and
    /// preroll the next picture it shows.
    fn sync_lane(&mut self, lane: usize) {
        let clip = self.lane_clip(lane, self.playhead);
        let key = clip.as_ref().and_then(video_key);
        if self
            .players
            .get(lane)
            .is_none_or(|player| player.key != key)
        {
            // A following request replaces the generation itself;
            // cancelling first would also discard the prerolled next clip.
            let source = clip.as_ref().and_then(|clip| clip.source.clone());
            let imported = source
                .as_deref()
                .is_some_and(|source| self.asset(source).is_some());
            let player = self.player(lane);
            if !imported {
                if let Some(stream) = &player.stream {
                    stream.cancel_preview();
                }
            }
            player.key = key;
            player.ready = false;
            player.still = None;
            if let (Some(clip), Some(source)) = (&clip, source) {
                if !imported {
                    return self.video_playback_failed("source is not imported".to_owned());
                }
                let start = clip.source_at(self.playhead);
                if let Err(error) = self.lane_stream(lane) {
                    return self.video_playback_failed(error);
                }
                let fps = self.timeline.fps;
                let player = self.player(lane);
                let stream = player.stream.as_ref().expect("started above");
                // A prerolled first picture is handed over, not decoded.
                // Holding the clock for its upload would stall audio
                // already rendered past the cut.
                let prerolled = stream.prefetch_ready(&source, start, PREVIEW_W);
                match stream.request_preview(&source, start, PREVIEW_W, fps) {
                    Ok(generation) => {
                        player.generation = generation;
                        player.ready = prerolled;
                    }
                    Err(error) => return self.video_playback_failed(error),
                }
            }
        }
        // Preroll the next clip the lane shows while this one plays or
        // buffers, so its first picture is ready when the playhead lands on
        // the cut. A lane showing nothing prerolls only shortly before.
        let Some((edge, Some((source, time)))) = self.next_video_change(lane, self.playhead) else {
            return;
        };
        let source = source.to_owned();
        if clip.is_none() && edge - self.playhead > super::incoming::LEAD {
            return;
        }
        // Where the playing clip's decoder will be when the cut comes.
        let active_until = clip.as_ref().map(|clip| clip.source_at(edge));
        if self.lane_stream(lane).is_ok() {
            if let Some(stream) = &self.players[lane].stream {
                let _ = stream.prefetch_preview(&source, time, PREVIEW_W, active_until);
            }
        }
    }

    /// Start lane `lane`'s preview worker if it has none.
    fn lane_stream(&mut self, lane: usize) -> Result<(), String> {
        if self.player(lane).stream.is_some() {
            return Ok(());
        }
        let wake = self.wake.clone();
        let stream = FrameStream::spawn_preview(move || wake.wake())?;
        self.player(lane).stream = Some(stream);
        Ok(())
    }

    pub fn advance_playback(&mut self, now: f64) {
        let dt = self.last_now.map_or(0.0, |last| now - last);
        self.last_now = Some(now);
        if self.playing {
            // The audio clock is the transport. It may run to the next cut
            // whose picture is not ready, so a late UI tick cannot jump over
            // a short clip and audio is never heard ahead of its picture.
            let limit = self.clock_limit();
            let playhead = self.playhead;
            let moved = self.clock_playhead != Some(playhead);
            let audio = self.audio_engine();
            if moved {
                audio.seek(playhead);
            }
            audio.set_limit(limit);
            audio.set_running(true);
            // Only the manual (test) output renders here; devices pace themselves.
            audio.advance(dt.max(0.0));
            let clock = audio.clock().min(limit);
            // The playhead still lands on each cut for one tick, so the clip
            // is requested at its first picture, which may be prerolled.
            let cut = self
                .playing_lanes()
                .filter_map(|lane| self.next_video_change(lane, playhead))
                .map(|(edge, _)| edge)
                .fold(f64::INFINITY, f64::min);
            self.playhead = playhead.max(clock.min(cut));
            self.clock_playhead = Some(self.playhead);
            if !self.scrubbing() {
                self.viewport.follow(playhead, self.playhead);
            }
            if self.playhead >= self.timeline.end() {
                self.playhead = self.timeline.end();
                self.set_playing(false);
            } else {
                self.wake.wake();
            }
        } else if self.shuttle != 0.0 {
            // A hand scrub takes the playhead back.
            if self.viewport.interacting || self.transport_scrubbing {
                self.shuttle = 0.0;
                return;
            }
            let end = self.timeline.end();
            // A UI hitch must not fling the playhead.
            let next = self.playhead + self.shuttle * dt.clamp(0.0, 0.1);
            self.playhead = next.clamp(0.0, end);
            self.viewport.reveal(self.playhead);
            if next <= 0.0 || next >= end {
                self.shuttle = 0.0;
                self.status = "Paused.".to_owned();
            } else {
                self.wake.wake();
            }
        }
    }

    /// Keyboard and transport seeks keep the playhead on screen.
    pub fn seek_and_reveal(&mut self, time: f64) {
        self.seek(time);
        self.viewport.reveal(self.playhead);
    }

    /// The nearest clip edge (or timeline start/end) after or before the
    /// playhead. Half a frame of slack steps past an edge the grid moved.
    pub fn edit_point(&self, forward: bool) -> Option<f64> {
        let slack = 0.5 / self.timeline.fps;
        let points = self
            .timeline
            .clips
            .iter()
            .flat_map(|clip| [clip.start, clip.end()])
            .chain([0.0, self.timeline.end()]);
        if forward {
            points
                .filter(|time| *time > self.playhead + slack)
                .min_by(f64::total_cmp)
        } else {
            points
                .filter(|time| *time < self.playhead - slack)
                .max_by(f64::total_cmp)
        }
    }
}

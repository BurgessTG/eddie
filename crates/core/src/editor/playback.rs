//! The transport and the program picture: play, seek, shuttle, and the
//! video that follows the audio clock.

use std::time::{Duration, Instant};

use openreel_engine::ClipKind;
use openreel_media::{FrameStream, RawFrame};

use super::{VideoKey, PREVIEW_W, SHUTTLE_MAX, STILL_CACHE_CAP};
use crate::Editor;

impl Editor {
    pub fn stop_sources(&mut self) {
        if self.rehearsing {
            return;
        }
        if let Some(stream) = &self.stream {
            stream.cancel_preview();
        }
        self.stream_key = None;
        self.stream_ready = false;
        self.stream_failed = false;
        // A still decoding now still answers its own frame; let it land.
        self.still_shown = None;
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
        self.shown_at.clear();
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

    pub fn active_video_clip(&self) -> Option<openreel_engine::Clip> {
        self.timeline.active_video(self.playhead).cloned()
    }

    pub fn active_video_key(&self) -> Option<VideoKey> {
        let clip = self.timeline.active_video(self.playhead)?;
        Some((
            clip.id,
            clip.source.clone()?,
            clip.start.to_bits(),
            clip.source_in.to_bits(),
        ))
    }

    /// The next edge after `after` where a different clip becomes visible,
    /// with its source and source time there, as `sync_playback` computes
    /// them at that edge. Edges into a video gap need no picture.
    pub fn next_video_change(&self, after: f64) -> Option<(f64, Option<(&str, f64)>)> {
        let current = self.timeline.active_video(after).map(|clip| clip.id);
        let mut next: Option<(f64, &openreel_engine::Clip)> = None;
        for clip in &self.timeline.clips {
            if clip.kind != ClipKind::Video {
                continue;
            }
            for edge in [clip.start, clip.end()] {
                if edge <= after || next.is_some_and(|(best, _)| edge >= best) {
                    continue;
                }
                if let Some(visible) = self.timeline.active_video(edge) {
                    if Some(visible.id) != current {
                        next = Some((edge, visible));
                    }
                }
            }
        }
        let (edge, clip) = next?;
        let source = clip
            .source
            .as_deref()
            .map(|source| (source, clip.source_in + (edge - clip.start)));
        Some((edge, source))
    }

    /// How far the audio clock may run before a picture must be ready: the
    /// next visible cut, or the one after it when the preview worker has
    /// already prerolled that cut's first picture.
    pub fn clock_limit(&self) -> f64 {
        let end = self.timeline.end();
        if self.waiting_for_video() {
            return self.playhead;
        }
        let Some((edge, source)) = self.next_video_change(self.playhead) else {
            return end;
        };
        let prerolled = source
            .zip(self.stream.as_ref())
            .is_some_and(|((path, time), stream)| stream.prefetch_ready(path, time, PREVIEW_W));
        if !prerolled {
            return edge.min(end);
        }
        self.next_video_change(edge)
            .map_or(end, |(after, _)| after)
            .min(end)
    }

    pub fn waiting_for_video(&self) -> bool {
        self.playing
            && self
                .active_video_key()
                .is_some_and(|key| self.stream_key.as_ref() != Some(&key) || !self.stream_ready)
    }

    pub fn video_playback_failed(&mut self, error: String) {
        self.playing = false;
        self.stream_failed = true;
        if let Some(audio) = &self.audio {
            audio.set_running(false);
        }
        self.status = format!("Video playback failed: {error}");
    }

    /// The monitor holds the active clip's picture. False while a newer
    /// clip's first picture is still decoding: draw nothing, not the old one.
    pub fn preview_is_current(&self) -> bool {
        self.preview_shown && self.preview_key == self.active_video_key()
    }

    /// Forget the shown picture so the next still is asked for again, such
    /// as at full size once a low-resolution scrub lets go.
    pub fn drop_preview(&mut self) {
        self.preview.clear();
        self.preview_shown = false;
        self.preview_key = None;
    }

    pub(super) fn show_preview(&mut self, frame: &RawFrame) {
        self.preview.show(frame);
        self.preview_shown = true;
        self.preview_key = self.active_video_key();
        if self.playing {
            let now = Instant::now();
            while self
                .shown_at
                .front()
                .is_some_and(|at| now.duration_since(*at) > Duration::from_secs(1))
            {
                self.shown_at.pop_front();
            }
            self.shown_at.push_back(now);
        }
    }

    /// Pictures shown over the last second of playback and the rate the
    /// active picture should reach (its source rate, at most the project's).
    /// `None` until playback has run a full second over video.
    pub fn playback_rate(&self) -> Option<(usize, f64)> {
        let since = self.playing_since?;
        if !self.playing || since.elapsed() < Duration::from_secs(1) {
            return None;
        }
        let clip = self.timeline.active_video(self.playhead)?;
        let source_fps = self
            .asset(clip.source.as_deref()?)
            .map_or(0.0, |asset| asset.info.fps);
        let target = if source_fps > 0.0 {
            source_fps.min(self.timeline.fps)
        } else {
            self.timeline.fps
        };
        let now = Instant::now();
        let shown = self
            .shown_at
            .iter()
            .filter(|at| now.duration_since(**at) <= Duration::from_secs(1))
            .count();
        Some((shown, target))
    }

    pub fn poll_frames(&mut self) {
        let active_key = self.active_video_key();
        if self.preview_key != active_key {
            if self.preview_shown {
                self.preview.clear();
                self.preview_shown = false;
            }
            self.preview_key = None;
        }
        let active = self.active_video_clip();
        let source_time = active
            .as_ref()
            .map(|clip| clip.source_in + (self.playhead - clip.start));
        let wants_still = !self.playing || self.scrubbing();
        while let Ok((gen, result)) = self.still_rx.try_recv() {
            let Some((pending, key, prefetch)) = self.still_inflight else {
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
            // The newest finished picture of this clip shows even when the
            // pointer has moved on; the next request chases the pointer.
            // A prefetched neighbor shows only once the playhead is on it.
            let shows = if prefetch {
                self.still_target(self.playhead, PREVIEW_W)
                    .is_some_and(|target| target.key == key)
            } else {
                active.as_ref().is_some_and(|clip| clip.id == key.0)
            };
            if wants_still && shows {
                self.show_preview(&frame);
                self.still_shown = Some(key);
            }
            self.still_cache.retain(|(cached, _)| *cached != key);
            if self.still_cache.len() >= STILL_CACHE_CAP {
                self.still_cache.pop_front();
            }
            self.still_cache.push_back((key, frame));
        }

        // A boundary/seek may precede sync_playback in this UI frame. Never
        // upload the previous generation as the active clip's picture.
        if wants_still || self.stream_key != active_key {
            return;
        }
        let result = self
            .stream
            .as_ref()
            .and_then(|stream| stream.preview_frame(self.stream_generation, source_time?));
        match result {
            Some(Ok(frame)) => {
                self.show_preview(&frame);
                self.still_shown = None;
                self.stream_ready = true;
                if self.status == "Buffering video." {
                    self.status = "Playing.".to_owned();
                }
                if let Some(stream) = &self.stream {
                    stream.recycle(frame.rgba);
                }
            }
            Some(Err(error)) => self.video_playback_failed(error),
            None => {}
        }
    }

    /// Keep the video stream and audio players aligned with the playhead.
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
            if self.stream_key.is_some() || self.audio_running() {
                self.stop_sources();
            }
            self.request_still_if_needed();
            return;
        }

        if !self.playing {
            // A terminal decode failure pauses visibly instead of consuming
            // an unseen clip or automatically retrying it through stills.
            if self.stream_failed && self.stream_key == self.active_video_key() {
                return;
            }
            if self.stream_key.is_some() || self.audio_running() {
                self.stop_sources();
            }
            self.request_still_if_needed();
            return;
        }

        // Video stream follows the active video clip.
        let active_video = self.active_video_clip();
        let active_key = self.active_video_key();
        if self.stream_key != active_key {
            // A following request replaces the generation itself; cancelling
            // first would also discard the prerolled next clip.
            let requests = active_key
                .as_ref()
                .is_some_and(|(_, source, _, _)| self.asset(source).is_some());
            if !requests {
                if let Some(stream) = &self.stream {
                    stream.cancel_preview();
                }
            }
            self.stream_key = active_key;
            self.stream_ready = false;
            self.stream_failed = false;
            self.still_shown = None;
            if let Some(clip) = &active_video {
                if let Some(source) = &clip.source {
                    if self.asset(source).is_some() {
                        let start = clip.source_in + (self.playhead - clip.start);
                        if self.stream.is_none() {
                            let wake = self.wake.clone();
                            match FrameStream::spawn_preview(move || wake.wake()) {
                                Ok(stream) => self.stream = Some(stream),
                                Err(error) => self.video_playback_failed(error),
                            }
                        }
                        if let Some(stream) = &self.stream {
                            // A prerolled first picture is handed over, not
                            // decoded. Holding the clock for its upload would
                            // stall audio already rendered past the cut.
                            let prerolled = stream.prefetch_ready(source, start, PREVIEW_W);
                            match stream.request_preview(
                                source,
                                start,
                                PREVIEW_W,
                                self.timeline.fps,
                            ) {
                                Ok(generation) => {
                                    self.stream_generation = generation;
                                    self.stream_ready = prerolled;
                                }
                                Err(error) => self.video_playback_failed(error),
                            }
                        }
                    } else {
                        self.video_playback_failed("source is not imported".to_owned());
                    }
                }
            }
        }

        if self.stream_failed {
            return;
        }
        // Preroll the next visible clip while this one plays or buffers, so
        // its first picture is ready when the playhead lands on the cut.
        if let (Some(stream), Some((edge, Some((source, time))))) =
            (&self.stream, self.next_video_change(self.playhead))
        {
            // Where the playing clip's decoder will be when the cut comes.
            let active_until = self
                .timeline
                .active_video(self.playhead)
                .map(|clip| clip.source_in + (edge - clip.start));
            let _ = stream.prefetch_preview(source, time, PREVIEW_W, active_until);
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
                .next_video_change(playhead)
                .map_or(f64::INFINITY, |(edge, _)| edge);
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

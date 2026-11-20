//! The picture a transition passes to. While a transition on a lane
//! shows, the lane's worker plays the clip it comes from past its out
//! point (the lane's picture, [`openreel_engine::LanePicture`]) and a
//! second preview worker plays the clip it goes to, from before its in
//! point; the front end draws the two as
//! [`openreel_engine::TransitionKind::frame`] says. Paused, both are stills.

use openreel_engine::{Clip, TransitionSpan};
use openreel_media::FrameStream;

use super::playback::video_key;
use super::{StillKey, VideoKey, PREVIEW_W};
use crate::Editor;

/// How long before a transition its incoming clip is asked for, so its
/// first picture is ready when the transition starts (a 4K keyframe seek
/// takes about 0.4 s). A lane that shows nothing prerolls its next clip as
/// late.
pub(super) const LEAD: f64 = 2.0;

#[derive(Default)]
pub(super) struct Incoming {
    /// A second preview worker, started at the first transition played.
    stream: Option<FrameStream>,
    /// The clip asked for and the start of its transition.
    key: Option<(VideoKey, u64)>,
    generation: u64,
    /// The source time asked for: the clip's picture there holds until
    /// the playhead reaches it.
    from: f64,
    /// The stream has delivered the asked-for clip's first picture.
    ready: bool,
    /// The clip whose picture the front end holds.
    pub(super) shown: Option<VideoKey>,
    /// The still that picture is, if it is one.
    pub(super) still: Option<StillKey>,
}

impl Editor {
    /// The transition lane `lane`'s incoming worker serves: the one showing
    /// on it at the playhead, else the next to start within `LEAD`.
    fn incoming_span(&self, lane: usize) -> Option<TransitionSpan> {
        let stack = self.stack();
        stack.spans.iter().copied().find(|span| {
            span.end > self.playhead
                && span.start <= self.playhead + LEAD
                && stack
                    .lane(lane, span.start.max(self.playhead))
                    .and_then(|picture| picture.transition)
                    == Some(*span)
        })
    }

    /// Whether the clock must wait for lane `lane`'s incoming picture
    /// before `until`: a transition showing by then has no picture of its
    /// clip yet.
    pub(super) fn incoming_limit(&self, lane: usize, until: f64) -> Option<f64> {
        let span = self.incoming_span(lane).filter(|span| span.start < until)?;
        let to = video_key(self.timeline.clip(span.to)?)?;
        let incoming = self.players.get(lane).map(|player| &player.incoming);
        let ready = incoming.is_some_and(|incoming| {
            incoming.ready && incoming.key == Some((to, span.start.to_bits()))
        });
        (!ready).then_some(span.start.max(self.playhead))
    }

    pub(super) fn stop_incoming(&mut self, lane: usize) {
        let Some(player) = self.players.get_mut(lane) else {
            return;
        };
        if let Some(stream) = &player.incoming.stream {
            stream.cancel_preview();
        }
        player.incoming.key = None;
        player.incoming.ready = false;
    }

    /// Ask lane `lane`'s incoming worker for the clip its next transition
    /// goes to, at the source time the playhead (or the transition's start)
    /// shows.
    pub(super) fn sync_incoming(&mut self, lane: usize) {
        let wanted = self.incoming_span(lane).and_then(|span| {
            let clip = self.timeline.clip(span.to)?;
            Some((span, clip.clone(), video_key(clip)?))
        });
        let wanted_key = wanted
            .as_ref()
            .map(|(span, _, key)| (key.clone(), span.start.to_bits()));
        if self.players.get(lane).map(|player| &player.incoming.key) == Some(&wanted_key)
            || (wanted_key.is_none() && self.players.len() <= lane)
        {
            return;
        }
        self.stop_incoming(lane);
        let Some((span, clip, _)) = wanted else {
            return;
        };
        let source = clip.source.as_deref().unwrap_or_default();
        if self.asset(source).is_none() {
            self.video_playback_failed("source is not imported".to_owned());
            return;
        }
        if self.player(lane).incoming.stream.is_none() {
            let wake = self.wake.clone();
            match FrameStream::spawn_preview(move || wake.wake()) {
                Ok(stream) => self.player(lane).incoming.stream = Some(stream),
                Err(error) => return self.video_playback_failed(error),
            }
        }
        let from = clip.source_at(self.playhead.max(span.start)).max(0.0);
        let fps = self.timeline.fps;
        let incoming = &mut self.player(lane).incoming;
        let Some(stream) = &incoming.stream else {
            return;
        };
        match stream.request_preview(source, from, PREVIEW_W, fps) {
            Ok(generation) => {
                incoming.generation = generation;
                incoming.from = from;
                incoming.key = wanted_key;
            }
            Err(error) => self.video_playback_failed(error),
        }
    }

    /// Take lane `lane`'s incoming worker's newest picture. Before the
    /// transition its first picture is taken and held, so the clock need
    /// not wait at the transition's start.
    pub(super) fn poll_incoming(&mut self, lane: usize) {
        let Some(incoming) = self.players.get(lane).map(|player| &player.incoming) else {
            return;
        };
        let (Some(stream), Some((key, _))) = (&incoming.stream, &incoming.key) else {
            return;
        };
        let Some(clip) = self.timeline.clip(key.0) else {
            return;
        };
        let time = clip.source_at(self.playhead).max(incoming.from);
        match stream.preview_frame(incoming.generation, time) {
            Some(Ok(frame)) => {
                let key = key.clone();
                self.preview.show_incoming(lane, &frame);
                let incoming = &mut self.player(lane).incoming;
                incoming.shown = Some(key);
                incoming.still = None;
                incoming.ready = true;
                if let Some(stream) = &incoming.stream {
                    stream.recycle(frame.rgba);
                }
            }
            Some(Err(error)) => self.video_playback_failed(error),
            None => {}
        }
    }

    /// The clip a transition on lane `lane` at `at` goes to, for its still.
    pub(super) fn incoming_clip(&self, lane: usize, at: f64) -> Option<&Clip> {
        let span = self.stack().lane(lane, at)?.transition?;
        self.timeline.clip(span.to)
    }
}

//! The picture a transition passes to. While a transition shows, the
//! program stream plays the clip it comes from past its out point
//! ([`openreel_engine::Timeline::program_video`]) and a second preview
//! worker plays the clip it goes to, from before its in point; the front
//! end draws the two as [`openreel_engine::TransitionKind::frame`] says.
//! Paused, both are stills.

use openreel_engine::{Clip, TransitionSpan};
use openreel_media::FrameStream;

use super::playback::video_key;
use super::{StillKey, VideoKey, PREVIEW_W};
use crate::Editor;

/// How long before a transition its incoming clip is asked for, so its
/// first picture is ready when the transition starts (a 4K keyframe seek
/// takes about 0.4 s).
const LEAD: f64 = 2.0;

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
    /// The transition at the playhead when the monitor holds both its
    /// pictures: the program picture of the clip it comes from and the
    /// incoming picture of the clip it goes to.
    pub fn transition_shown(&self) -> Option<TransitionSpan> {
        let span = self.timeline.transition_at(self.playhead)?;
        let to = video_key(self.timeline.clip(span.to)?)?;
        (self.preview_is_current() && self.incoming.shown.as_ref() == Some(&to)).then_some(span)
    }

    /// The transition the incoming worker serves: the one at the playhead,
    /// else the next to start within `LEAD`.
    fn incoming_span(&self) -> Option<TransitionSpan> {
        let spans = self.timeline.transitions();
        spans.iter().copied().find(|span| {
            span.end > self.playhead
                && span.start <= self.playhead + LEAD
                && self
                    .timeline
                    .transition_among(&spans, span.start.max(self.playhead))
                    == Some(*span)
        })
    }

    /// Whether the clock must wait for the incoming picture before `until`:
    /// a transition showing by then has no picture of its clip yet.
    pub(super) fn incoming_limit(&self, until: f64) -> Option<f64> {
        let span = self.incoming_span().filter(|span| span.start < until)?;
        let to = video_key(self.timeline.clip(span.to)?)?;
        let ready = self.incoming.ready && self.incoming.key == Some((to, span.start.to_bits()));
        (!ready).then_some(span.start.max(self.playhead))
    }

    pub(super) fn stop_incoming(&mut self) {
        if let Some(stream) = &self.incoming.stream {
            stream.cancel_preview();
        }
        self.incoming.key = None;
        self.incoming.ready = false;
    }

    /// Ask the incoming worker for the clip the next transition goes to,
    /// at the source time the playhead (or the transition's start) shows.
    pub(super) fn sync_incoming(&mut self) {
        let wanted = self.incoming_span().and_then(|span| {
            let clip = self.timeline.clip(span.to)?;
            Some((span, clip.clone(), video_key(clip)?))
        });
        let wanted_key = wanted
            .as_ref()
            .map(|(span, _, key)| (key.clone(), span.start.to_bits()));
        if self.incoming.key == wanted_key {
            return;
        }
        self.stop_incoming();
        let Some((span, clip, _)) = wanted else {
            return;
        };
        let source = clip.source.as_deref().unwrap_or_default();
        if self.asset(source).is_none() {
            self.video_playback_failed("source is not imported".to_owned());
            return;
        }
        if self.incoming.stream.is_none() {
            let wake = self.wake.clone();
            match FrameStream::spawn_preview(move || wake.wake()) {
                Ok(stream) => self.incoming.stream = Some(stream),
                Err(error) => return self.video_playback_failed(error),
            }
        }
        let from = clip.source_at(self.playhead.max(span.start)).max(0.0);
        let Some(stream) = &self.incoming.stream else {
            return;
        };
        match stream.request_preview(source, from, PREVIEW_W, self.timeline.fps) {
            Ok(generation) => {
                self.incoming.generation = generation;
                self.incoming.from = from;
                self.incoming.key = wanted_key;
            }
            Err(error) => self.video_playback_failed(error),
        }
    }

    /// Take the incoming worker's newest picture. Before the transition its
    /// first picture is taken and held, so the clock need not wait at the
    /// transition's start.
    pub(super) fn poll_incoming(&mut self) {
        let (Some(stream), Some((key, _))) = (&self.incoming.stream, &self.incoming.key) else {
            return;
        };
        let Some(clip) = self.timeline.clip(key.0) else {
            return;
        };
        let time = clip.source_at(self.playhead).max(self.incoming.from);
        match stream.preview_frame(self.incoming.generation, time) {
            Some(Ok(frame)) => {
                self.preview.show_incoming(&frame);
                self.incoming.shown = Some(key.clone());
                self.incoming.still = None;
                self.incoming.ready = true;
                stream.recycle(frame.rgba);
            }
            Some(Err(error)) => self.video_playback_failed(error),
            None => {}
        }
    }

    /// The clip a transition at `at` goes to, for its still.
    pub(super) fn incoming_clip(&self, at: f64) -> Option<&Clip> {
        let span = self.timeline.transition_at(at)?;
        self.timeline.clip(span.to)
    }
}

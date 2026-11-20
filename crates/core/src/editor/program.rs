//! The program picture in layers. Every video lane that shows at the
//! playhead ([`Timeline::video_stack`]) plays its own picture on its own
//! preview worker, so a picture in picture plays with the clip under it,
//! and the front end draws the lanes bottom first.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use openreel_engine::{
    hides_below, place, Clip, ClipId, LanePicture, Placement, Timeline, TransitionSpan,
};
use openreel_media::{FrameStream, RawFrame};

use super::incoming::Incoming;
use super::playback::video_key;
use super::{StillKey, VideoKey};
use crate::Editor;

/// One video lane's picture: its preview worker and what the front end
/// holds for it.
#[derive(Default)]
pub(super) struct Player {
    /// The lane's preview worker, started the first time the lane plays.
    pub(super) stream: Option<FrameStream>,
    /// The clip the worker is asked for.
    pub(super) key: Option<VideoKey>,
    pub(super) generation: u64,
    /// The worker has delivered the asked-for clip's first picture.
    pub(super) ready: bool,
    /// The clip whose picture the front end holds for this lane.
    pub(super) shown: Option<VideoKey>,
    /// The still that picture is, if it is one.
    pub(super) still: Option<StillKey>,
    /// When this lane's pictures were shown during the last second of
    /// playback.
    shown_at: VecDeque<Instant>,
    /// The picture a transition on this lane goes to.
    pub(super) incoming: Incoming,
}

/// The timeline's lanes as the program shows them, sized by the bin's
/// media: which hide the lanes under them ([`hides_below`]).
pub(crate) struct Stack<'a> {
    timeline: &'a Timeline,
    pub(super) spans: Vec<TransitionSpan>,
    canvas: (f64, f64),
    /// Each file's displayed size and whether it has alpha.
    files: HashMap<&'a str, ((f64, f64), bool)>,
}

impl<'a> Stack<'a> {
    /// `timeline`'s lanes on a `canvas` sized frame, `files` giving each
    /// file's displayed size and whether it has alpha.
    pub(crate) fn new(
        timeline: &'a Timeline,
        canvas: (f64, f64),
        files: HashMap<&'a str, ((f64, f64), bool)>,
    ) -> Self {
        Self {
            timeline,
            spans: timeline.transitions(),
            canvas,
            files,
        }
    }

    /// `clip`'s file: its displayed size and whether it has alpha. A file
    /// not in the bin counts as an opaque picture of the canvas's shape.
    pub(crate) fn file(&self, clip: &Clip) -> ((f64, f64), bool) {
        clip.source
            .as_deref()
            .and_then(|source| self.files.get(source))
            .copied()
            .unwrap_or((self.canvas, false))
    }

    fn hides(&self, clip: &Clip) -> bool {
        let (size, alpha) = self.file(clip);
        clip.source.is_some() && hides_below(clip, self.canvas, size, alpha)
    }

    /// Where `clip`'s picture lands on the canvas.
    pub(crate) fn placement(&self, clip: &Clip) -> Placement {
        place(self.canvas, self.file(clip).0, &clip.transform)
    }

    /// The pictures showing at `time`, bottom first.
    pub(crate) fn at(&self, time: f64) -> Vec<LanePicture<'a>> {
        self.timeline
            .video_stack(&self.spans, time, &|clip| self.hides(clip))
    }

    /// Lane `lane`'s picture at `time`, when it shows.
    pub(crate) fn lane(&self, lane: usize, time: f64) -> Option<LanePicture<'a>> {
        self.at(time)
            .into_iter()
            .find(|picture| picture.lane == lane)
    }
}

/// One lane's picture at the playhead, for a front end to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProgramLayer {
    pub lane: usize,
    /// The clip whose picture the lane shows; during a transition, the clip
    /// it comes from.
    pub clip: ClipId,
    pub transition: Option<TransitionSpan>,
    /// The front end holds this clip's picture for the lane.
    pub shown: bool,
    /// During a transition, it holds the picture it goes to as well.
    pub incoming_shown: bool,
}

impl Editor {
    pub(crate) fn stack(&self) -> Stack<'_> {
        let canvas = self.canvas();
        let files = self
            .assets
            .iter()
            .map(|asset| {
                let info = &asset.info;
                let size = (f64::from(info.width), f64::from(info.height));
                (info.path.as_str(), (size, info.alpha))
            })
            .collect();
        Stack::new(
            &self.timeline,
            (f64::from(canvas.0), f64::from(canvas.1)),
            files,
        )
    }

    /// The pictures at the playhead, bottom first, and whether the front
    /// end holds each: what the monitor draws.
    pub fn program_layers(&self) -> Vec<ProgramLayer> {
        self.stack()
            .at(self.playhead)
            .iter()
            .map(|picture| {
                let player = self.players.get(picture.lane);
                let held = |clip: Option<&Clip>, shown: Option<&VideoKey>| {
                    clip.and_then(video_key).as_ref() == shown && shown.is_some()
                };
                let to = picture
                    .transition
                    .and_then(|span| self.timeline.clip(span.to));
                ProgramLayer {
                    lane: picture.lane,
                    clip: picture.clip.id,
                    transition: picture.transition,
                    shown: held(Some(picture.clip), player.and_then(|p| p.shown.as_ref())),
                    incoming_shown: held(to, player.and_then(|p| p.incoming.shown.as_ref())),
                }
            })
            .collect()
    }

    /// The monitor holds the picture of every lane that shows. False while
    /// a newer clip's first picture is still decoding.
    pub fn preview_is_current(&self) -> bool {
        self.program_layers().iter().all(|layer| layer.shown)
    }

    /// A transition at the playhead whose two pictures the monitor holds.
    pub fn transition_shown(&self) -> Option<TransitionSpan> {
        self.program_layers()
            .iter()
            .find(|layer| layer.shown && layer.incoming_shown)
            .and_then(|layer| layer.transition)
    }

    /// The top picture at the playhead under canvas point `point`, else
    /// the top picture: what a press on the monitor takes hold of.
    pub fn picture_at(&self, point: (f64, f64)) -> Option<ClipId> {
        let stack = self.stack();
        let pictures = stack.at(self.playhead);
        pictures
            .iter()
            .rev()
            .find(|picture| stack.placement(picture.clip).contains(point))
            .or(pictures.last())
            .map(|picture| picture.clip.id)
    }

    /// The clip lane `lane` shows at `at`, if it shows.
    pub(super) fn lane_clip(&self, lane: usize, at: f64) -> Option<Clip> {
        self.stack()
            .lane(lane, at)
            .map(|picture| picture.clip.clone())
    }

    /// The player of lane `lane`, made when first needed.
    pub(super) fn player(&mut self, lane: usize) -> &mut Player {
        if self.players.len() <= lane {
            self.players.resize_with(lane + 1, Player::default);
        }
        &mut self.players[lane]
    }

    /// Lanes that have or had a player, or have clips: every lane playback
    /// looks after.
    pub(super) fn playing_lanes(&self) -> std::ops::Range<usize> {
        let used = self
            .timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == openreel_engine::ClipKind::Video)
            .map(|clip| clip.track + 1)
            .max()
            .unwrap_or(0);
        0..self.players.len().max(self.timeline.lanes.video).max(used)
    }

    /// Forget the shown pictures so the next stills are asked for again,
    /// such as at full size once a low-resolution scrub lets go.
    pub fn drop_preview(&mut self) {
        for lane in 0..self.players.len() {
            self.preview.clear(lane);
            self.players[lane].shown = None;
        }
    }

    /// Forget lane `lane`'s picture when it is not of the clip it shows
    /// now: a different clip's picture never stands in for it.
    pub(super) fn drop_stale_picture(&mut self, lane: usize, wanted: Option<&VideoKey>) {
        let Some(player) = self.players.get_mut(lane) else {
            return;
        };
        if player.shown.is_some() && player.shown.as_ref() != wanted {
            player.shown = None;
            self.preview.clear(lane);
        }
    }

    /// Show `frame` as lane `lane`'s picture of `clip`.
    pub(super) fn show_lane(&mut self, lane: usize, clip: Option<VideoKey>, frame: &RawFrame) {
        self.preview.show(lane, frame);
        let playing = self.playing;
        let player = self.player(lane);
        player.shown = clip;
        if playing {
            let now = Instant::now();
            while player
                .shown_at
                .front()
                .is_some_and(|at| now.duration_since(*at) > Duration::from_secs(1))
            {
                player.shown_at.pop_front();
            }
            player.shown_at.push_back(now);
        }
    }

    pub(super) fn forget_shown_rates(&mut self) {
        for player in &mut self.players {
            player.shown_at.clear();
        }
    }

    /// Pictures shown over the last second of playback and the rate they
    /// should reach (the source rate at the clip's speed, at most the
    /// project's), for the lane furthest behind. `None` until playback has
    /// run a full second over video, and over stills.
    pub fn playback_rate(&self) -> Option<(usize, f64)> {
        let since = self.playing_since?;
        if !self.playing || since.elapsed() < Duration::from_secs(1) {
            return None;
        }
        let now = Instant::now();
        self.stack()
            .at(self.playhead)
            .iter()
            .filter(|picture| !picture.clip.still())
            .filter_map(|picture| {
                let source_fps = self
                    .asset(picture.clip.source.as_deref()?)
                    .map_or(0.0, |asset| asset.info.fps);
                let target = if source_fps > 0.0 {
                    (source_fps * picture.clip.speed).min(self.timeline.fps)
                } else {
                    self.timeline.fps
                };
                let shown = self.players.get(picture.lane).map_or(0, |player| {
                    player
                        .shown_at
                        .iter()
                        .filter(|at| now.duration_since(**at) <= Duration::from_secs(1))
                        .count()
                });
                Some((shown, target))
            })
            .min_by(|a, b| (a.0 as f64 / a.1).total_cmp(&(b.0 as f64 / b.1)))
    }
}

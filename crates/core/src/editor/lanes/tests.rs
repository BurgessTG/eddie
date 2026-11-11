//! Pointer gestures on the lanes, driven as a window reports them.

use openreel_engine::ClipKind;

use super::*;

mod moves;
mod pointer;
mod swaps;
mod trim;

const VIEW: ViewRect = ViewRect {
    left: 0.0,
    top: 0.0,
    right: 1200.0,
    bottom: 400.0,
};

struct Stage {
    timeline: Timeline,
    playhead: f64,
    selected: Option<ClipId>,
    port: TimelineViewport,
    gesture: Gesture,
}

impl Stage {
    fn new(timeline: Timeline) -> Self {
        Self {
            timeline,
            playhead: 0.0,
            selected: None,
            port: TimelineViewport::default(),
            gesture: Gesture::default(),
        }
    }

    fn scene(&mut self) -> (Scene<'_>, &mut Gesture) {
        (
            Scene {
                timeline: &mut self.timeline,
                playhead: &mut self.playhead,
                selected: &mut self.selected,
                port: &mut self.port,
                rect: VIEW,
            },
            &mut self.gesture,
        )
    }

    fn point(&self, time: f64, track: usize) -> Point {
        [
            x_at(&self.port, VIEW.content_left(), time),
            VIEW.lanes_top() + track as f32 * (TRACK_H + TRACK_GAP) + TRACK_H / 2.0,
        ]
    }

    fn x(&self, time: f64) -> f32 {
        x_at(&self.port, VIEW.content_left(), time)
    }

    fn center(&self, id: ClipId) -> Point {
        let clip = self.timeline.clip(id).unwrap();
        self.point(clip.start + clip.duration / 2.0, clip.track)
    }

    /// A press at `from` released at `to`; the selection it leaves.
    fn drag(&mut self, from: Point, to: Point) -> Option<ClipId> {
        self.drag_through(from, &[to]);
        self.selected
    }

    /// A press that visits each point of `path` and is released at
    /// the last, as a window reports it: the gesture begins at the
    /// first move, and the release repeats the final position.
    fn drag_through(&mut self, press: Point, path: &[Point]) {
        let (mut scene, gesture) = self.scene();
        gesture.begin(&mut scene, press, path[0]);
        for &pos in path {
            gesture.drag(&mut scene, pos, &|_| None);
        }
        let last = path[path.len() - 1];
        gesture.drag(&mut scene, last, &|_| None);
        gesture.finish(&mut scene, Some(last));
        assert!(!gesture.is_interacting());
    }

    fn click(&mut self, at: Point) {
        let (mut scene, gesture) = self.scene();
        gesture.click(&mut scene, at, at);
    }
}

/// One clip on the second lane, 2 s to 8 s, from 3 s into a 20 s source.
fn clip_fixture() -> (Stage, ClipId) {
    let mut timeline = Timeline::empty_project();
    let id = timeline.add_media_clip(
        "clip.mp4",
        ClipKind::Video,
        1,
        2.0,
        6.0,
        3.0,
        "/media/full.mp4",
    );
    (Stage::new(timeline), id)
}

/// Two clips on the second lane that meet at 8 s.
fn touching_fixture() -> (Stage, ClipId, ClipId) {
    let mut timeline = Timeline::empty_project();
    let a = timeline.add_media_clip("a.mp4", ClipKind::Video, 1, 2.0, 6.0, 0.0, "/media/a.mp4");
    let b = timeline.add_media_clip("b.mp4", ClipKind::Video, 1, 8.0, 6.0, 0.0, "/media/a.mp4");
    let mut stage = Stage::new(timeline);
    // Keep the snap out of a test of which clip a press takes.
    stage.port.magnet = false;
    (stage, a, b)
}

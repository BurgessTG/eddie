//! What a front end hands the editor: a way to wake it ([`Wake`]) and a
//! place for the program monitor's pictures ([`PreviewSink`]).

use std::sync::Arc;

use openreel_media::RawFrame;

/// Asks the front end for another frame. Worker threads call it when their
/// result is ready, and playback calls it to keep the clock moving.
#[derive(Clone)]
pub struct Wake(Arc<dyn Fn() + Send + Sync>);

impl Wake {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(wake))
    }

    /// For an editor nothing draws: the caller polls on its own schedule.
    pub fn none() -> Self {
        Self::new(|| {})
    }

    pub fn wake(&self) {
        (self.0)()
    }
}

/// Where the program monitor's picture goes. The editor picks the frame;
/// the front end owns the pixels it is copied into.
pub trait PreviewSink {
    /// The picture of video lane `lane`
    /// ([`Editor::program_layers`](super::Editor::program_layers)).
    fn show(&mut self, lane: usize, frame: &RawFrame);
    /// Lane `lane` no longer holds a picture of the clip it shows.
    fn clear(&mut self, lane: usize);
    /// The picture of the clip a transition on lane `lane` goes to.
    fn show_incoming(&mut self, _lane: usize, _frame: &RawFrame) {}
}

/// The monitor of an editor nothing draws.
pub struct NoPreview;

impl PreviewSink for NoPreview {
    fn show(&mut self, _lane: usize, _frame: &RawFrame) {}
    fn clear(&mut self, _lane: usize) {}
}

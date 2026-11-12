//! UI-independent timeline model.
//!
//! Times are seconds (`f64`). Media placement preserves source intervals;
//! generated clips and explicit move/trim/split edits use the project frame grid.

mod clip;
mod duck;
mod fade;
pub mod time_spec;
mod timeline;

#[cfg(test)]
mod tests;

pub use clip::{Clip, ClipId, ClipKind, TitleStyle};
pub use duck::{duck_amount, duck_gain, DUCK_ATTACK, DUCK_HOLD, DUCK_LEVEL, DUCK_RELEASE};
pub use fade::{fade_gain, fade_shape, fit_fades};
pub use time_spec::format_timecode;
pub use timeline::{Lanes, Orientation, Timeline, LANE_KINDS, MAX_TIMELINE_SECONDS};

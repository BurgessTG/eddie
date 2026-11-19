//! UI-independent timeline model.
//!
//! Times are seconds (`f64`). Media placement preserves source intervals;
//! generated clips and explicit move/trim/split edits use the project frame grid.

mod clip;
mod color;
mod duck;
mod fade;
mod place;
pub mod time_spec;
mod timeline;

#[cfg(test)]
mod tests;

pub use clip::{Clip, ClipId, ClipKind, TitleStyle, MAX_SPEED, MIN_SPEED};
pub use color::{ColorAdjust, Filter, MAX_EXPOSURE};
pub use duck::{duck_amount, duck_gain, DUCK_ATTACK, DUCK_HOLD, DUCK_LEVEL, DUCK_RELEASE};
pub use fade::{fade_gain, fade_shape, fit_fades};
pub use place::{
    fill_scale, place, Background, Placement, Transform, MAX_SCALE, MIN_CROP_KEEP, MIN_SCALE,
};
pub use time_spec::format_timecode;
pub use timeline::{orientation_name, Format, Lanes, Timeline, LANE_KINDS, MAX_TIMELINE_SECONDS};

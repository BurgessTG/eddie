//! UI-independent timeline model.
//!
//! Times are seconds (`f64`). Media placement preserves source intervals;
//! generated clips and explicit move/trim/split edits use the project frame grid.

mod clip;
mod color;
mod duck;
mod fade;
mod place;
mod stack;
mod text;
pub mod time_spec;
mod timeline;
mod transition;

#[cfg(test)]
mod tests;

pub use clip::{Clip, ClipId, ClipKind, TitleStyle, MAX_SPEED, MIN_SPEED};
pub use color::{ColorAdjust, Filter, MAX_EXPOSURE};
pub use duck::{duck_amount, duck_gain, DUCK_ATTACK, DUCK_HOLD, DUCK_LEVEL, DUCK_RELEASE};
pub use fade::{fade_gain, fade_shape, fit_fades};
pub use place::{
    fill_scale, place, Background, Placement, Transform, MAX_SCALE, MIN_CROP_KEEP, MIN_SCALE,
};
pub use stack::{hides_below, LanePicture};
pub use text::{
    Rgb, TextLook, DEFAULT_BOX_OPACITY, DEFAULT_OUTLINE, MAX_OUTLINE, MAX_TEXT_OFFSET,
    MAX_TEXT_SIZE, MIN_TEXT_SIZE,
};
pub use time_spec::format_timecode;
pub use timeline::{
    orientation_name, Format, Lanes, Ripple, Timeline, LANE_KINDS, MAX_TIMELINE_SECONDS,
};
pub use transition::{
    Crossfade, Layer, Transition, TransitionFrame, TransitionKind, TransitionSpan,
    DEFAULT_TRANSITION, MAX_TRANSITION, MIN_TRANSITION,
};

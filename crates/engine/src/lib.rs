//! UI-independent timeline model.
//!
//! Times are seconds (`f64`). Media placement preserves source intervals;
//! generated clips and explicit move/trim/split edits use the project frame grid.

mod blend;
mod chroma;
mod clip;
mod color;
mod duck;
mod fade;
mod keyframe;
mod marker;
mod mask;
mod place;
mod stack;
mod text;
pub mod time_spec;
mod timeline;
mod transition;

#[cfg(test)]
mod tests;

pub use blend::BlendMode;
pub use chroma::{yuv, ChromaKey, Spill, KEY_BLUE, KEY_GREEN};
pub use clip::{Clip, ClipId, ClipKind, TitleStyle, MAX_SPEED, MIN_SPEED};
pub use color::{ColorAdjust, Filter, MAX_EXPOSURE};
pub use duck::{duck_amount, duck_gain, DUCK_ATTACK, DUCK_HOLD, DUCK_LEVEL, DUCK_RELEASE};
pub use fade::{fade_gain, fade_shape, fit_fades};
pub use keyframe::{
    key_value, widen, Key, Keyframes, Property, KEY_TOLERANCE, MAX_KEYS, MAX_ROTATION,
};
pub use marker::{Marker, MarkerColor, MarkerId};
pub use mask::{
    mask_alpha, multiply, Mask, MaskShape, Matte, MASK_MAX_OFFSET, MASK_MAX_SIZE, MASK_MIN_SIZE,
};
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

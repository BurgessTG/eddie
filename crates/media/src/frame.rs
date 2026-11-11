//! A decoded picture and the size it scales to.

#[derive(Clone, Debug)]
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Height for a target width, preserving aspect, always even (encoders and
/// many scalers require even dimensions).
pub fn scaled_height(src_w: u32, src_h: u32, target_w: u32) -> u32 {
    if src_w == 0 || src_h == 0 {
        return target_w * 9 / 16;
    }
    let h = (src_h as f64 * target_w as f64 / src_w as f64).round() as u32;
    (h / 2 * 2).max(2)
}

//! Clip speed with the pitch kept: FFmpeg's `atempo` (WSOLA). Export puts
//! [`tempo_filter`] in its graph and the preview runs the same chain in
//! process (`chain::Chain`), so both stretch a clip's sound the same way.

/// Source seconds read past a sped-up clip's end. `atempo` holds up to half
/// its 43 ms window back until more input comes, so without this the last
/// 50-100 ms of the clip would be silent.
pub(crate) const TEMPO_TAIL: f64 = 0.1;

/// The `atempo` chain that plays `speed` source seconds per second, or
/// `None` at normal speed. One stage takes 0.5x to 100x; slower speeds
/// chain two equal stages, as FFmpeg's documentation advises.
pub(crate) fn tempo_filter(speed: f64) -> Option<String> {
    if speed == 1.0 {
        return None;
    }
    Some(if speed >= 0.5 {
        format!("atempo={speed}")
    } else {
        let stage = speed.sqrt();
        format!("atempo={stage},atempo={stage}")
    })
}

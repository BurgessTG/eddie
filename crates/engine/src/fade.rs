//! Fade gain, shared by the preview mixer and export.

pub fn fit_fades(duration: f64, fade_in: f64, fade_out: f64) -> (f64, f64) {
    let duration = duration.max(0.0);
    // Scaling the requested lengths keeps their ratio; each then fits.
    // Capped so two huge requests cannot sum to infinity and scale to NaN.
    let clean = |f: f64| {
        if f.is_finite() {
            f.clamp(0.0, 1e9)
        } else {
            0.0
        }
    };
    let (fade_in, fade_out) = (clean(fade_in), clean(fade_out));
    let total = fade_in + fade_out;
    if total > duration && total > 0.0 {
        (fade_in * duration / total, fade_out * duration / total)
    } else {
        (fade_in, fade_out)
    }
}

/// Gain of a fade envelope `offset` seconds into a clip of `duration`.
pub fn fade_gain(offset: f64, duration: f64, fade_in: f64, fade_out: f64) -> f32 {
    let (fade_in, fade_out) = fit_fades(duration, fade_in, fade_out);
    let mut gain = 1.0_f64;
    if fade_in > 0.0 {
        gain = gain.min(offset / fade_in);
    }
    if fade_out > 0.0 {
        gain = gain.min((duration - offset) / fade_out);
    }
    gain.clamp(0.0, 1.0) as f32
}

/// Gain at `progress` (0..1) through a fade: linear, or the quarter sine
/// that holds loudness up through a crossfade. Preview and export share it.
pub fn fade_shape(progress: f32, smooth: bool) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    if smooth {
        (progress * std::f32::consts::FRAC_PI_2).sin()
    } else {
        progress
    }
}

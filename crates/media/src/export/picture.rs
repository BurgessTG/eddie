//! One clip's picture in export: cropped, scaled, graded, flipped, turned
//! and moved where [`place`] puts it on the canvas, over black or its own
//! blurred copy, as the preview draws it.

use openreel_engine::{place, Background, ColorAdjust};

use super::video::VideoLayer;

/// Even and at least 2, as yuv420p and the colorspace filter need.
fn even(value: f64) -> u32 {
    ((value / 2.0).round() as u32).max(1) * 2
}

/// The filters that grade a picture as [`ColorAdjust::apply_rgba`] does
/// for the preview, starting with a comma, or nothing for no change. One
/// `colorchannelmixer` on 8-bit RGBA clamps once, at the end, as the
/// preview does. A matrix past the mixer's ±2 runs halved, then doubled
/// back, on float RGBA, which does not clamp between filters (3 ms more a
/// 1080p frame), and converting back to 8 bits clamps. `lutrgb` then bends
/// brightness with the preview's own formula.
fn color_filters(color: &ColorAdjust) -> String {
    if color.is_identity() {
        return String::new();
    }
    let mut m = color.matrix();
    let largest = m.iter().flatten().fold(0f64, |a, v| a.max(v.abs()));
    let mut doublings = 0;
    while largest / f64::from(1u32 << doublings) > 2.0 {
        doublings += 1;
    }
    let k = f64::from(1u32 << doublings);
    m.iter_mut().flatten().for_each(|v| *v /= k);
    let format = if doublings == 0 { "rgba" } else { "gbrapf32le" };
    let mut out = format!(",format={format},colorchannelmixer=");
    let names = [
        ["rr", "rg", "rb", "ra"],
        ["gr", "gg", "gb", "ga"],
        ["br", "bg", "bb", "ba"],
    ];
    let terms: Vec<String> = names
        .iter()
        .flatten()
        .zip(m.iter().flatten())
        .map(|(name, v)| format!("{name}={v}"))
        .collect();
    out.push_str(&terms.join(":"));
    if doublings > 0 {
        for _ in 0..doublings {
            out.push_str(",colorchannelmixer=rr=2:gg=2:bb=2");
        }
        out.push_str(",format=rgba");
    }
    if color.brightness != 0.0 {
        let curve = format!("round(255*exp({}*log(val/255)))", color.gamma());
        out.push_str(&format!(",lutrgb=r={curve}:g={curve}:b={curve}"));
    }
    out
}

/// The filters that turn one layer's frames into its picture on the canvas,
/// ending in `[seg{index}]`, and where that picture overlays the canvas.
/// [`place`] decides the geometry, as it does for the preview. An untouched
/// clip on black keeps the resize-and-pad chain export always had, unless
/// it is drawn over another lane, which must show around it.
pub(super) fn picture(
    index: usize,
    layer: &VideoLayer<'_>,
    canvas: (u32, u32),
    source: (u32, u32),
    (resize, convert, alpha): (&str, &str, &str),
) -> (String, i64, i64) {
    let (width, height) = canvas;
    let color = color_filters(&layer.color);
    if layer.transform.is_identity()
        && layer.background == Background::Black
        && color.is_empty()
        && !layer.over
    {
        return (
            format!(
                ",scale={width}:{height}:force_original_aspect_ratio=decrease:flags={resize},\
                 pad={width}:{height}:(ow-iw)/2:(oh-ih)/2{convert},setsar=1{alpha}[seg{index}]"
            ),
            0,
            0,
        );
    }
    let canvas_f = (f64::from(width), f64::from(height));
    let t = &layer.transform;
    let p = place(canvas_f, (f64::from(source.0), f64::from(source.1)), t);
    let mut chain = String::new();
    if t.has_crop() {
        let [x, y, w, h] = p.crop.map(|v| v.round() as i64);
        chain.push_str(&format!("crop={}:{}:{x}:{y},", w.max(2), h.max(2)));
    }
    let (pw, ph) = (even(p.size[0]), even(p.size[1]));
    chain.push_str(&format!(
        "scale={pw}:{ph}:flags={resize}{convert}{color},setsar=1"
    ));
    if t.flip_h {
        chain.push_str(",hflip");
    }
    if t.flip_v {
        chain.push_str(",vflip");
    }
    let (mut out_w, mut out_h) = (f64::from(pw), f64::from(ph));
    if t.rotation.rem_euclid(360.0) != 0.0 {
        let [rw, rh] = p.turned_size();
        let (rw, rh) = (rw.round().max(1.0), rh.round().max(1.0));
        chain.push_str(&format!(
            ",format=rgba,rotate=a={:.9}:ow={rw}:oh={rh}:c=none",
            t.rotation.to_radians()
        ));
        (out_w, out_h) = (rw, rh);
    }
    let x = (p.center[0] - out_w / 2.0).round() as i64;
    let y = (p.center[1] - out_h / 2.0).round() as i64;
    if layer.background == Background::Blur && !p.covers(canvas_f) {
        // The same frames, filling the canvas and blurred: scaled to an
        // eighth first, so the blur costs little at any canvas size. They
        // flip with the picture, as the preview's do.
        let (bw, bh) = (even(canvas_f.0 / 8.0).max(8), even(canvas_f.1 / 8.0).max(8));
        let radius = ((f64::from(bw) * 0.025).round() as u32).clamp(1, (bw.min(bh) / 4).max(1));
        let flips = match (t.flip_h, t.flip_v) {
            (true, true) => ",hflip,vflip",
            (true, false) => ",hflip",
            (false, true) => ",vflip",
            (false, false) => "",
        };
        return (
            format!(
                ",split[pic{index}][bgin{index}];\
                 [bgin{index}]scale={bw}:{bh}:force_original_aspect_ratio=increase:flags={resize},\
                 crop={bw}:{bh}{convert}{color}{flips},boxblur={radius}:2,scale={width}:{height}:flags=bilinear,\
                 setsar=1[bg{index}];\
                 [pic{index}]{chain}[fg{index}];\
                 [bg{index}][fg{index}]overlay={x}:{y}{alpha}[seg{index}]"
            ),
            0,
            0,
        );
    }
    (format!(",{chain}{alpha}[seg{index}]"), x, y)
}

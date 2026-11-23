//! One clip's picture in export: cropped, scaled, graded, flipped, turned
//! and moved where [`place`] puts it on the canvas, over black or its own
//! blurred copy, as the preview draws it. A clip with keys lays its picture
//! out once at a fixed size, then fades, resizes, turns and moves it every
//! frame along its keys. A mask clears the picture outside its shape
//! before it turns, by a matte written once per batch.

use openreel_engine::{
    place, yuv, Background, ChromaKey, ColorAdjust, Ease, Key, Keyframes, Mask, Property, Spill,
    Transform,
};

use super::animate::{commands, curve};
use super::scratch::FilterGraphFile;
use super::titles::filter_value;
use super::video::VideoLayer;
use crate::MediaResult;

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

/// The filters that key a picture as [`ChromaKey::apply_rgba`] does for
/// the preview, starting with a comma, or nothing without a key:
/// `chromakey` on 4:4:4 chroma, given the key colour in the BT.709
/// limited-range chroma the frames carry, then `despill` on RGBA. Red spill
/// is despill's green with the two channels swapped around it.
fn chroma_filters(key: Option<&ChromaKey>) -> String {
    let Some(key) = key else {
        return String::new();
    };
    let (y, u, v) = yuv(key.color);
    let mut out = format!(
        ",format=yuva444p,chromakey=color=0x{y:02x}{u:02x}{v:02x}:similarity={}:blend={}:yuv=1,\
         format=rgba",
        key.similarity(),
        key.blend()
    );
    if key.spill > 0.0 {
        let amount = -(key.spill as f32);
        let swap = ",colorchannelmixer=rr=0:rg=1:gr=1:gg=0";
        let (kind, scales) = match key.spill_kind() {
            Spill::Green | Spill::Red => ("green", format!("red=0:green={amount}:blue=0")),
            Spill::Blue => ("blue", format!("red=0:green=0:blue={amount}")),
        };
        let red = key.spill_kind() == Spill::Red;
        if red {
            out.push_str(swap);
        }
        out.push_str(&format!(
            ",despill=type={kind}:mix=0.5:expand=0:{scales}:brightness=0:alpha=0"
        ));
        if red {
            out.push_str(swap);
        }
    }
    out
}

/// What a picture's own pixels go through after it is sized: its key, then
/// its grade, so the key colour is the file's own.
fn look_filters(layer: &VideoLayer<'_>) -> String {
    format!(
        "{}{}",
        chroma_filters(layer.chroma.as_ref()),
        color_filters(&layer.color)
    )
}

/// The mattes a batch's masks read, written while its graph is built and
/// kept until its FFmpeg is done with them.
pub(super) struct Mattes {
    /// The batch's frame rate. A layer's frames run on its inverse after
    /// `fps`, and a matte is timed on it too, so `lut2` keeps the layer's
    /// timebase.
    rate: ffmpeg_next::Rational,
    pub(super) files: Vec<FilterGraphFile>,
}

impl Mattes {
    pub(super) fn new(rate: ffmpeg_next::Rational) -> Self {
        Self {
            rate,
            files: Vec::new(),
        }
    }

    /// The filters, starting with a comma, that clear a `size` picture
    /// outside `mask`: its alpha times the mask's coverage, written as a
    /// grey picture of [`openreel_engine::Matte::levels`], so export
    /// clears the pixels the preview does. `lut2` rounds as
    /// [`openreel_engine::multiply`] does.
    fn filters(&mut self, index: usize, mask: &Mask, (w, h): (u32, u32)) -> MediaResult<String> {
        let levels = mask
            .on(f64::from(w), f64::from(h))
            .levels(w as usize, h as usize, (0.0, 0.0));
        let mut pgm = format!("P5\n{w} {h}\n255\n").into_bytes();
        pgm.extend_from_slice(&levels);
        let file = FilterGraphFile::write("pgm", &pgm)?;
        let path = filter_value(&file.0.to_string_lossy());
        self.files.push(file);
        let base = format!("{}/{}", self.rate.denominator(), self.rate.numerator());
        let m = format!("mk{index}");
        Ok(format!(
            ",format=rgba,split[{m}p][{m}a];[{m}a]alphaextract[{m}l];\
             movie=filename={path},format=gray,settb={base}[{m}g];\
             [{m}l][{m}g]lut2=c0='round(x*y/255)'[{m}x];[{m}p][{m}x]alphamerge"
        ))
    }
}

/// A layer's keys on its batch's clock.
pub(super) struct Motion<'a> {
    pub(super) keys: &'a Keyframes,
    /// The clip's start, in seconds after the batch's first frame.
    pub(super) offset: f64,
}

impl Motion<'_> {
    /// `property`'s keys, at seconds on the batch's clock.
    fn keys(&self, property: Property) -> Vec<Key> {
        self.keys
            .keys(property)
            .iter()
            .map(|key| Key {
                at: key.at + self.offset,
                ..*key
            })
            .collect()
    }
}

/// Where a layer's picture goes on the canvas: a fixed top-left corner, or
/// a centre that moves along keys (seconds on the batch's clock, canvas
/// pixels), which overlay reads every frame.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Spot {
    At(i64, i64),
    Moving { x: Vec<Key>, y: Vec<Key> },
}

impl Spot {
    /// overlay's options that put the picture there, for frames whose time
    /// is `offset` seconds before the batch's clock.
    pub(super) fn overlay(&self, offset: f64) -> String {
        match self {
            Self::At(x, y) => format!("{x}:{y}"),
            Self::Moving { x, y } => {
                let t = format!("(t+{offset:.6})");
                format!(
                    "x='floor({}-overlay_w/2+0.5)':y='floor({}-overlay_h/2+0.5)':eval=frame",
                    curve(x, &t),
                    curve(y, &t)
                )
            }
        }
    }
}

/// One layer's picture as filters, each starting with a comma.
pub(super) struct Picture {
    /// Lays out what does not change: run once on a still's one picture.
    fixed: String,
    /// Its opacity, after `fixed`, or over the whole canvas with a
    /// `background`.
    alpha: String,
    /// Resizes and turns each frame as its keys say.
    moving: String,
    /// For a keyed picture over its blurred copy: the filters from the
    /// frames to that copy filling the canvas.
    background: Option<String>,
    spot: Spot,
}

impl Picture {
    /// The filters from `lead`, a chain that ends on the layer's frames
    /// (for a still, its one picture, which `hold` then holds), to
    /// `[seg{index}]`.
    pub(super) fn filters(&self, index: usize, lead: &str, hold: Option<&str>) -> Vec<String> {
        let Self {
            fixed,
            alpha,
            moving,
            background,
            spot,
        } = self;
        let Some(background) = background else {
            return match hold {
                None => vec![format!("{lead}{fixed}{alpha}{moving}[seg{index}]")],
                Some(hold) => vec![
                    format!("{lead}{fixed}[photo{index}]"),
                    format!("[photo{index}]{hold}{alpha}{moving}[seg{index}]"),
                ],
            };
        };
        let held = hold.map(|hold| format!(",{hold}")).unwrap_or_default();
        vec![
            format!("{lead},split[pic{index}][bgin{index}]"),
            format!("[bgin{index}]{background}{held}[bg{index}]"),
            format!("[pic{index}]null{fixed}{held}{moving}[fg{index}]"),
            format!(
                "[bg{index}][fg{index}]overlay={}{alpha}[seg{index}]",
                spot.overlay(0.0)
            ),
        ]
    }

    /// Where `[seg{index}]` goes on the canvas.
    pub(super) fn spot(&self) -> Spot {
        if self.background.is_some() {
            Spot::At(0, 0)
        } else {
            self.spot.clone()
        }
    }
}

/// The same frames, filling the canvas and blurred: scaled to an eighth
/// first, so the blur costs little at any canvas size. They flip with the
/// picture, as the preview's do.
fn blurred(canvas: (u32, u32), t: &Transform, resize: &str, convert: &str, color: &str) -> String {
    let (width, height) = canvas;
    let (bw, bh) = (
        even(f64::from(width) / 8.0).max(8),
        even(f64::from(height) / 8.0).max(8),
    );
    let radius = ((f64::from(bw) * 0.025).round() as u32).clamp(1, (bw.min(bh) / 4).max(1));
    let flips = match (t.flip_h, t.flip_v) {
        (true, true) => ",hflip,vflip",
        (true, false) => ",hflip",
        (false, true) => ",vflip",
        (false, false) => "",
    };
    format!(
        "scale={bw}:{bh}:force_original_aspect_ratio=increase:flags={resize},\
         crop={bw}:{bh}{convert}{color}{flips},boxblur={radius}:2,\
         scale={width}:{height}:flags=bilinear,setsar=1"
    )
}

fn flips(t: &Transform) -> String {
    let mut out = String::new();
    if t.flip_h {
        out.push_str(",hflip");
    }
    if t.flip_v {
        out.push_str(",vflip");
    }
    out
}

/// The filters that turn one layer's frames into its picture on the canvas,
/// and where that picture goes. [`place`] decides the geometry, as it does
/// for the preview. An untouched clip on black keeps the resize-and-pad
/// chain export always had, unless it is drawn over another lane, which
/// must show around it. `motion` is the layer's keys, when it has any.
pub(super) fn picture(
    index: usize,
    layer: &VideoLayer<'_>,
    canvas: (u32, u32),
    source: (u32, u32),
    (resize, convert): (&str, &str),
    motion: Option<&Motion<'_>>,
    mattes: &mut Mattes,
) -> MediaResult<Picture> {
    let (width, height) = canvas;
    let color = look_filters(layer);
    let alpha = if layer.opacity < 0.999 {
        format!(",format=rgba,colorchannelmixer=aa={:.3}", layer.opacity)
    } else {
        String::new()
    };
    if let Some(motion) = motion {
        return moving(
            index,
            layer,
            canvas,
            source,
            (resize, convert),
            &color,
            motion,
            mattes,
        );
    }
    let still = |fixed: String, spot: Spot| Picture {
        fixed,
        alpha: String::new(),
        moving: String::new(),
        background: None,
        spot,
    };
    if layer.transform.is_identity()
        && layer.background == Background::Black
        && color.is_empty()
        && layer.mask.is_none()
        && !layer.over
    {
        return Ok(still(
            format!(
                ",scale={width}:{height}:force_original_aspect_ratio=decrease:flags={resize},\
                 pad={width}:{height}:(ow-iw)/2:(oh-ih)/2{convert},setsar=1{alpha}"
            ),
            Spot::At(0, 0),
        ));
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
        "scale={pw}:{ph}:flags={resize}{convert}{color},setsar=1{}",
        flips(t)
    ));
    if let Some(mask) = &layer.mask {
        chain.push_str(&mattes.filters(index, mask, (pw, ph))?);
    }
    let (mut out_w, mut out_h) = (f64::from(pw), f64::from(ph));
    if t.rotation.rem_euclid(360.0) != 0.0 {
        let [rw, rh] = p.turned_size();
        let (rw, rh) = (rw.round().max(1.0), rh.round().max(1.0));
        chain.push_str(&format!(
            ",format=rgba,rotate=a={:.9}:ow={rw}:oh={rh}:c=black@0",
            t.rotation.to_radians()
        ));
        (out_w, out_h) = (rw, rh);
    }
    let x = (p.center[0] - out_w / 2.0).round() as i64;
    let y = (p.center[1] - out_h / 2.0).round() as i64;
    if layer.background == Background::Blur && !p.covers(canvas_f) {
        let background = blurred(canvas, t, resize, convert, &color);
        return Ok(still(
            format!(
                ",split[pic{index}][bgin{index}];[bgin{index}]{background}[bg{index}];\
                 [pic{index}]{chain}[fg{index}];\
                 [bg{index}][fg{index}]overlay={x}:{y}{alpha}"
            ),
            Spot::At(0, 0),
        ));
    }
    Ok(still(format!(",{chain}{alpha}"), Spot::At(x, y)))
}

/// A keyed layer's picture. It is laid out once at the largest size its
/// keys reach (never past the source's own pixels, which would only be
/// blown up), then each frame is faded, sized, turned on a square any turn
/// fits, and moved along the keys, which the preview follows too.
#[allow(clippy::too_many_arguments)]
fn moving(
    index: usize,
    layer: &VideoLayer<'_>,
    canvas: (u32, u32),
    source: (u32, u32),
    (resize, convert): (&str, &str),
    color: &str,
    motion: &Motion<'_>,
    mattes: &mut Mattes,
) -> MediaResult<Picture> {
    let canvas_f = (f64::from(canvas.0), f64::from(canvas.1));
    let t = &layer.transform;
    let unit = place(
        canvas_f,
        (f64::from(source.0), f64::from(source.1)),
        &Transform { scale: 1.0, ..*t },
    );
    let [unit_w, unit_h] = unit.size;
    let scales = motion.keys(Property::Scale);
    let turns = motion.keys(Property::Rotation);
    let fades = motion.keys(Property::Opacity);
    let mut fixed = String::from(",");
    if t.has_crop() {
        let [x, y, w, h] = unit.crop.map(|v| v.round() as i64);
        fixed.push_str(&format!("crop={}:{}:{x}:{y},", w.max(2), h.max(2)));
    }
    let largest = if scales.is_empty() {
        t.scale
    } else {
        scales.iter().map(|key| key.value).fold(0.0, f64::max)
    };
    let shrink = if scales.is_empty() {
        1.0
    } else {
        (unit.crop[2] / (unit_w * largest)).min(1.0)
    };
    let (fw, fh) = (
        even(unit_w * largest * shrink),
        even(unit_h * largest * shrink),
    );
    fixed.push_str(&format!(
        "scale={fw}:{fh}:flags={resize}{convert}{color},setsar=1{}",
        flips(t)
    ));
    if let Some(mask) = &layer.mask {
        fixed.push_str(&mattes.filters(index, mask, (fw, fh))?);
    }
    let mut moving = String::new();
    let (most_w, most_h) = (
        f64::from(even(unit_w * largest)),
        f64::from(even(unit_h * largest)),
    );
    if !scales.is_empty() {
        let s = curve(&scales, "t");
        moving.push_str(&format!(
            ",format=rgba,scale=w='2*max(1,floor({unit_w:.6}*({s})/2+0.5))':\
             h='2*max(1,floor({unit_h:.6}*({s})/2+0.5))':eval=frame:flags=bicubic"
        ));
    }
    if !turns.is_empty() || t.rotation.rem_euclid(360.0) != 0.0 {
        let angle = if turns.is_empty() {
            format!("{:.9}", t.rotation.to_radians())
        } else {
            format!("'({})*PI/180'", curve(&turns, "t"))
        };
        // Even, so a picture of even size sits on whole pixels at its centre.
        let side = even(most_w.hypot(most_h).ceil()) + 2;
        if !scales.is_empty() {
            moving.push_str(&format!(
                ",pad=w={side}:h={side}:x=(ow-iw)/2:y=(oh-ih)/2:color=black@0:eval=frame"
            ));
        }
        moving.push_str(&format!(
            ",format=rgba,rotate=a={angle}:ow={side}:oh={side}:c=black@0"
        ));
    }
    let alpha = if fades.is_empty() {
        if layer.opacity < 0.999 {
            format!(",format=rgba,colorchannelmixer=aa={:.3}", layer.opacity)
        } else {
            String::new()
        }
    } else {
        let mixer = format!("colorchannelmixer@op{index}");
        format!(
            ",format=rgba,sendcmd=c='{}',{mixer}=aa=1",
            commands(&mixer, "aa", &fades)
        )
    };
    let centre = |property: Property, plain: f64, size: f64| {
        let keys = motion.keys(property);
        if keys.is_empty() {
            vec![Key {
                at: 0.0,
                value: size / 2.0 + plain * size,
                ease: Ease::Linear,
            }]
        } else {
            keys.into_iter()
                .map(|key| Key {
                    value: size / 2.0 + key.value * size,
                    ..key
                })
                .collect()
        }
    };
    let spot = Spot::Moving {
        x: centre(Property::X, t.x, canvas_f.0),
        y: centre(Property::Y, t.y, canvas_f.1),
    };
    let background =
        (layer.background == Background::Blur).then(|| blurred(canvas, t, resize, convert, color));
    Ok(Picture {
        fixed,
        alpha,
        moving,
        background,
        spot,
    })
}

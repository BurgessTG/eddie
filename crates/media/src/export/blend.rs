//! A picture with a blend mode in export. It is placed on a clear canvas,
//! mixed with what the lanes under it show by FFmpeg's `lut2` on 8-bit RGB,
//! then laid over them by its own alpha (its transparency and opacity), as
//! the preview mixes it: `under + alpha * (blend(under, picture) - under)`.
//! The `lut2` expression repeats [`BlendMode::apply`]'s steps in the same
//! order, so every 8-bit answer equals [`BlendMode::table`]'s.

use openreel_engine::BlendMode;

use super::video::{VIDEO_COLOR_RANGE, VIDEO_COLOR_SPACE};

/// `mode` as a `lut2` expression of the lanes under (`x`) and the picture
/// (`y`), from 0 to 255, rounded to a level.
pub(super) fn expression(mode: BlendMode) -> String {
    let (b, s) = ("ld(0)", "ld(1)");
    let hard_light =
        |b: &str, s: &str| format!("if(lte({s},0.5),{b}*(2*{s}),{b}+(2*{s}-1)-{b}*(2*{s}-1))");
    let blend = match mode {
        BlendMode::Normal => s.to_owned(),
        BlendMode::Darken => format!("min({b},{s})"),
        BlendMode::Multiply => format!("{b}*{s}"),
        BlendMode::ColorBurn => {
            format!("if(gte({b},1),1,if(lte({s},0),0,1-min((1-{b})/{s},1)))")
        }
        BlendMode::LinearBurn => format!("max({b}+{s}-1,0)"),
        BlendMode::Lighten => format!("max({b},{s})"),
        BlendMode::Screen => format!("{b}+{s}-{b}*{s}"),
        BlendMode::ColorDodge => {
            format!("if(lte({b},0),0,if(gte({s},1),1,min({b}/(1-{s}),1)))")
        }
        BlendMode::Add => format!("min({b}+{s},1)"),
        BlendMode::Overlay => hard_light(s, b),
        BlendMode::HardLight => hard_light(b, s),
        BlendMode::SoftLight => format!(
            "if(lte({s},0.5),{b}-(1-2*{s})*{b}*(1-{b}),\
             {b}+(2*{s}-1)*(if(lte({b},0.25),((16*{b}-12)*{b}+4)*{b},sqrt({b}))-{b}))"
        ),
        BlendMode::Difference => format!("abs({b}-{s})"),
        BlendMode::Exclusion => format!("{b}+{s}-2*{b}*{s}"),
    };
    format!("st(0,x/255);st(1,y/255);round(255*clip({blend},0,1))")
}

/// The filters that draw `[{own}]`, a layer's picture, at `place` over
/// `[{current}]` with `mode` on the frames `enable` names, ending on
/// `[v{index}]`. Both run `frames` long on a `canvas` sized frame at
/// `rate`. Under and over meet as RGB from the canvas's BT.709 limited
/// range, and the result goes back to it.
#[allow(clippy::too_many_arguments)]
pub(super) fn filters(
    mode: BlendMode,
    index: usize,
    current: &str,
    own: &str,
    place: &str,
    enable: &str,
    (width, height): (u32, u32),
    rate: ffmpeg_next::Rational,
    frames: i64,
) -> Vec<String> {
    let e = expression(mode);
    vec![
        // The picture alone on a clear canvas: its colour where it shows,
        // its alpha saying how much.
        format!(
            "color=c=black@0:s={width}x{height}:r={rate},format=rgba,\
             trim=end_frame={frames}[bc{index}]"
        ),
        format!(
            "[bc{index}][{own}]overlay={place}:eof_action=repeat:ts_sync_mode=default:\
             format=rgb:enable='{enable}',split[bs{index}][ba{index}]"
        ),
        format!(
            "[{current}]scale=in_color_matrix={VIDEO_COLOR_SPACE}:in_range={VIDEO_COLOR_RANGE},\
             format=gbrp,split[bu{index}][bk{index}]"
        ),
        format!("[bs{index}]format=gbrp[bp{index}]"),
        format!("[bu{index}][bp{index}]lut2=c0='{e}':c1='{e}':c2='{e}'[bm{index}]"),
        format!("[ba{index}]alphaextract[bl{index}]"),
        format!("[bm{index}][bl{index}]alphamerge[bx{index}]"),
        format!(
            "[bk{index}][bx{index}]overlay=0:0:format=gbrp,\
             scale=out_color_matrix={VIDEO_COLOR_SPACE}:out_range={VIDEO_COLOR_RANGE},\
             format=yuv444p,\
             setparams=colorspace={VIDEO_COLOR_SPACE}:range={VIDEO_COLOR_RANGE}[v{index}]"
        ),
    ]
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::process::{Command, Stdio};

    use super::*;

    /// FFmpeg's answer for every pair of levels equals the table the
    /// preview looks up, in every mode.
    #[test]
    fn lut2_gives_the_preview_s_table_for_every_level_in_every_mode() {
        // Under across, picture down: every pair once, as gbrp planes.
        let under: Vec<u8> = (0..65536).map(|i| (i % 256) as u8).collect();
        let over: Vec<u8> = (0..65536).map(|i| (i / 256) as u8).collect();
        let dir = std::env::temp_dir().join(format!("eddie-blend-lut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (x, y) = (dir.join("under.raw"), dir.join("over.raw"));
        std::fs::write(&x, under.repeat(3)).unwrap();
        std::fs::write(&y, over.repeat(3)).unwrap();
        for mode in BlendMode::ALL {
            let e = expression(mode);
            let graph = format!("[0:v][1:v]lut2=c0='{e}':c1='{e}':c2='{e}'[o]");
            let mut child = Command::new("ffmpeg")
                .args(["-v", "error"])
                .args(["-f", "rawvideo", "-pix_fmt", "gbrp", "-s", "256x256", "-i"])
                .arg(&x)
                .args(["-f", "rawvideo", "-pix_fmt", "gbrp", "-s", "256x256", "-i"])
                .arg(&y)
                .args(["-/filter_complex", "pipe:0", "-map", "[o]"])
                .args(["-f", "rawvideo", "-pix_fmt", "gbrp", "pipe:1"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("run ffmpeg");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(graph.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success(), "{mode:?}");
            let table = mode.table();
            let wrong = (0..65536)
                .filter(|&i| {
                    out.stdout[i] != table[usize::from(under[i]) * 256 + usize::from(over[i])]
                })
                .count();
            assert_eq!(wrong, 0, "{mode:?}: {wrong} levels differ");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

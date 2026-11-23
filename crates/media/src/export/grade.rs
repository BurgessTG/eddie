//! A picture's colour grade in export: [`ColorAdjust`]'s matrix as
//! `colorchannelmixer` and its brightness curve as `lutrgb`, the maths the
//! preview's [`ColorAdjust::apply_rgba`] does. A grade with keys gets each
//! frame's values from `sendcmd`, worked out here by the engine's own
//! [`Keyframes::color`], so export grades every frame as the preview does.

use std::fmt::Write;

use openreel_engine::{ColorAdjust, Keyframes};

use super::scratch::FilterGraphFile;
use super::titles::filter_value;
use crate::MediaResult;

type Matrix = [[f64; 4]; 3];

const NAMES: [[&str; 4]; 3] = [
    ["rr", "rg", "rb", "ra"],
    ["gr", "gg", "gb", "ga"],
    ["br", "bg", "bb", "ba"],
];

/// How many times `m` is halved so each coefficient fits the mixer's ±2.
fn doublings(m: &Matrix) -> u32 {
    let largest = m.iter().flatten().fold(0f64, |a, v| a.max(v.abs()));
    let mut doublings = 0;
    while largest / f64::from(1u32 << doublings) > 2.0 {
        doublings += 1;
    }
    doublings
}

/// `m` halved `doublings` times, as `name=value` pairs.
fn terms(m: &Matrix, doublings: u32) -> impl Iterator<Item = (&'static str, f64)> + '_ {
    let k = f64::from(1u32 << doublings);
    NAMES
        .iter()
        .flatten()
        .zip(m.iter().flatten())
        .map(move |(&name, v)| (name, v / k))
}

/// The brightness curve `lutrgb` gives each channel.
fn curve(gamma: f64) -> String {
    format!("round(255*exp({gamma}*log(val/255)))")
}

/// The mixer `{target}` with `m`, then the curve `{lut}` with `gamma`
/// unless there is none. A target or lut of `""` is unnamed.
fn filters(m: &Matrix, doublings: u32, gamma: Option<f64>, target: &str, lut: &str) -> String {
    let format = if doublings == 0 { "rgba" } else { "gbrapf32le" };
    let terms: Vec<String> = terms(m, doublings)
        .map(|(name, v)| format!("{name}={v}"))
        .collect();
    let mut out = format!(
        ",format={format},colorchannelmixer{target}={}",
        terms.join(":")
    );
    if doublings > 0 {
        for _ in 0..doublings {
            out.push_str(",colorchannelmixer=rr=2:gg=2:bb=2");
        }
        out.push_str(",format=rgba");
    }
    if let Some(gamma) = gamma {
        let c = curve(gamma);
        out.push_str(&format!(",lutrgb{lut}=r={c}:g={c}:b={c}"));
    }
    out
}

/// The filters that grade a picture as [`ColorAdjust::apply_rgba`] does
/// for the preview, starting with a comma, or nothing for no change. One
/// `colorchannelmixer` on 8-bit RGBA clamps once, at the end, as the
/// preview does. A matrix past the mixer's ±2 runs halved, then doubled
/// back, on float RGBA, which does not clamp between filters (3 ms more a
/// 1080p frame), and converting back to 8 bits clamps. `lutrgb` then bends
/// brightness with the preview's own formula.
pub(super) fn color_filters(color: &ColorAdjust) -> String {
    if color.is_identity() {
        return String::new();
    }
    let m = color.matrix();
    let gamma = (color.brightness != 0.0).then(|| color.gamma());
    filters(&m, doublings(&m), gamma, "", "")
}

/// A grade with keys, on frames whose times are a batch's (frame `n` at
/// `n` / `rate` seconds).
pub(super) struct Keyed<'a> {
    pub(super) plain: ColorAdjust,
    pub(super) keys: &'a Keyframes,
    /// The clip's start, in seconds after the batch's first frame.
    pub(super) offset: f64,
    /// The batch frames the picture is decoded for, `[first, last)`.
    pub(super) frames: (i64, i64),
    pub(super) rate: ffmpeg_next::Rational,
}

impl Keyed<'_> {
    /// The filters, starting with a comma, that grade frame `n` with the
    /// clip's grade at its time: a `sendcmd` reading a file written here
    /// (kept in `files` until FFmpeg is done) sets mixer `@{name}` and
    /// curve `@{name}b` at each frame where the grade changes. The mixer
    /// is halved as often as the strongest frame needs, so every frame's
    /// values fit it.
    pub(super) fn filters(
        &self,
        name: &str,
        files: &mut Vec<FilterGraphFile>,
    ) -> MediaResult<String> {
        let (num, den) = (
            f64::from(self.rate.numerator()),
            f64::from(self.rate.denominator()),
        );
        let seconds = |n: f64| n * den / num;
        // Runs of frames with one grade: (first frame, matrix, gamma).
        let mut runs: Vec<(i64, Matrix, f64)> = Vec::new();
        for n in self.frames.0..self.frames.1.max(self.frames.0 + 1) {
            let grade = self.keys.color(self.plain, seconds(n as f64) - self.offset);
            let (m, gamma) = (grade.matrix(), grade.gamma());
            if runs.last().is_none_or(|run| run.1 != m || run.2 != gamma) {
                runs.push((n, m, gamma));
            }
        }
        let doublings = runs.iter().map(|run| doublings(&run.1)).max().unwrap_or(0);
        let curved = runs.iter().any(|run| run.2 != 1.0);
        let (mixer, lut) = (
            format!("colorchannelmixer@{name}"),
            format!("lutrgb@{name}b"),
        );
        let mut file = String::new();
        for (index, (n, m, gamma)) in runs.iter().enumerate() {
            // Each run from half a frame before its first frame, so a frame
            // time rounded to microseconds never lands before it.
            let from = seconds(*n as f64 - 0.5).max(0.0);
            let _ = write!(file, "{from:.6}");
            if let Some(next) = runs.get(index + 1) {
                let _ = write!(file, "-{:.6}", seconds(next.0 as f64 - 0.5));
            }
            let mut commands: Vec<String> = terms(m, doublings)
                .map(|(term, v)| format!("{mixer} {term} {v}"))
                .collect();
            if curved {
                let c = curve(*gamma);
                commands.extend(["r", "g", "b"].map(|channel| format!("{lut} {channel} {c}")));
            }
            let _ = writeln!(file, " {};", commands.join(", "));
        }
        let commands = FilterGraphFile::write("cmd", file.as_bytes())?;
        let path = filter_value(&commands.0.to_string_lossy());
        files.push(commands);
        let (_, m, gamma) = &runs[0];
        let target = format!("@{name}");
        let lut = format!("@{name}b");
        Ok(format!(
            ",sendcmd=f={path}{}",
            filters(m, doublings, curved.then_some(*gamma), &target, &lut)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openreel_engine::Property;

    /// The command file a keyed grade writes: one interval per run of
    /// frames with one grade, held at the keys' ends, the mixer halved as
    /// often as the strongest frame needs, and every value the engine's.
    #[test]
    fn keyed_grades_send_each_frames_values_and_hold_outside_the_keys() {
        let mut keys = Keyframes::default();
        keys.set(Property::Exposure, 0.2, 0.0);
        keys.set(Property::Exposure, 0.4, 2.0);
        keys.set(Property::Brightness, 0.0, 0.5);
        let keyed = Keyed {
            plain: ColorAdjust::default(),
            keys: &keys,
            offset: 0.1,
            frames: (0, 20),
            rate: ffmpeg_next::Rational::new(10, 1),
        };
        let mut files = Vec::new();
        let graph = keyed.filters("g7", &mut files).unwrap();
        assert!(graph.contains("colorchannelmixer@g7="), "{graph}");
        assert!(graph.contains("lutrgb@g7b="), "{graph}");
        let written = std::fs::read_to_string(&files[0].0).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        // Frame n is 0.1 n - 0.1 s into the clip: frames 0 to 3 hold the
        // first key, frame 4 is halfway, and from frame 5 the last key holds.
        assert_eq!(lines.len(), 3, "{written}");
        assert!(lines[0].starts_with("0.000000-0.350000 "), "{written}");
        assert!(lines[1].starts_with("0.350000-0.450000 "), "{written}");
        assert!(lines[2].starts_with("0.450000 "), "{written}");
        let rr = |line: &str| -> f64 {
            let at = line.find("colorchannelmixer@g7 rr ").unwrap() + 24;
            line[at..]
                .split(',')
                .next()
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        };
        // Exposure in stops is a gain of 2^(stops / 2.2): 1.37 halfway and
        // 1.88 at 2 stops, within ±2, so the mixer is not halved.
        assert!((rr(lines[0]) - 1.0).abs() < 1e-9, "{written}");
        assert!(
            (rr(lines[1]) - 2f64.powf(1.0 / 2.2)).abs() < 1e-9,
            "{written}"
        );
        assert!(
            (rr(lines[2]) - 2f64.powf(2.0 / 2.2)).abs() < 1e-9,
            "{written}"
        );
        assert!(!graph.contains("gbrapf32le"), "{graph}");
        let lit = format!("lutrgb@g7b g {},", curve(2f64.powf(-0.5)));
        assert!(lines.iter().all(|line| line.contains(&lit)), "{written}");
    }
}

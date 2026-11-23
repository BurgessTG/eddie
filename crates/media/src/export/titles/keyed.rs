//! A title with keys in export: its look worked out for each frame by the
//! same `clip_title_look` the monitor draws with, then sent to its drawtext
//! passes by `sendcmd` wherever it changes.

use std::fmt::Write;

use openreel_engine::Clip;

use super::super::frames::VideoClock;
use super::super::scratch::FilterGraphFile;
use super::{clip_title_look, filter_value, spot, TitleLook, SHARP_SHADOW};
use crate::MediaResult;

/// A keyed title on one batch: runs of frames that draw alike.
pub(in crate::export) struct KeyedTitle {
    /// First batch frame of each run, the look it draws with and its
    /// opacity.
    runs: Vec<(i64, TitleLook, f32)>,
}

impl KeyedTitle {
    /// `title` on batch frames `ranges` of the batch starting at output
    /// frame `start`, drawn `scale` times the size of the canvas its look
    /// is laid out on.
    pub(in crate::export) fn new(
        title: &Clip,
        ranges: &[(i64, i64)],
        start: i64,
        clock: VideoClock,
        scale: f64,
    ) -> Self {
        let (first, end) = (ranges[0].0, ranges[ranges.len() - 1].1);
        let mut runs: Vec<(i64, TitleLook, f32)> = Vec::new();
        for n in first..end {
            let posed = title.posed_at(clock.time(start + n));
            let look = clip_title_look(&posed).scaled(scale);
            let opacity = posed.opacity.clamp(0.0, 1.0);
            if runs
                .last()
                .is_none_or(|run| (run.1, run.2) != (look, opacity))
            {
                runs.push((n, look, opacity));
            }
        }
        Self { runs }
    }

    /// The look whose passes draw every frame: the largest, so a shadow
    /// soft at any size is drawn soft (at no blur where it is sharp).
    pub(in crate::export) fn layout(&self) -> TitleLook {
        self.runs
            .iter()
            .map(|run| run.1)
            .max_by_key(|look| look.font_size)
            .expect("a keyed title shows on a frame")
    }

    /// The `sendcmd` that sets each pass of `title`, drawn by
    /// [`TitleLook::filters`] under `tag` with this title's
    /// [`layout`](Self::layout) and then the highlight `layers` (drawtext
    /// instance names), to each run's look and opacity from half a frame
    /// before the run's first frame, and the command file it reads.
    pub(in crate::export) fn sendcmd(
        &self,
        title: &Clip,
        tag: &str,
        layers: &[String],
        clock: VideoClock,
    ) -> MediaResult<(String, FilterGraphFile)> {
        let rate = clock.rate;
        let seconds = |n: f64| n * f64::from(rate.denominator()) / f64::from(rate.numerator());
        let layout = self.layout();
        let soft = layout.shadow.is_some_and(|s| s.blur >= SHARP_SHADOW);
        let boxed = layout.shadow.is_some() && layout.text_box.is_some();
        let mut file = String::new();
        for (index, (n, look, opacity)) in self.runs.iter().enumerate() {
            let (x, y) = spot(title, look);
            let border = look.outline.map_or(0, |(width, ..)| width);
            let mut commands = Vec::new();
            let mut text = |name: &str, x: &str, y: &str| {
                commands.extend([
                    format!("drawtext@{name} fontsize {}", look.font_size),
                    format!("drawtext@{name} x {x}"),
                    format!("drawtext@{name} y {y}"),
                    format!("drawtext@{name} alpha {opacity:.4}"),
                ]);
            };
            text(tag, &x, &y);
            let shadow = look.shadow.map(|shadow| (shadow, shadow.offset()));
            if boxed {
                text(&format!("{tag}box"), &x, &y);
            }
            if let (true, Some((_, (dx, dy)))) = (soft, shadow) {
                text(
                    &format!("{tag}cast"),
                    &format!("{x}{dx:+}"),
                    &format!("{y}{dy:+}"),
                );
            }
            for layer in layers {
                text(layer, &x, &y);
            }
            for pass in std::iter::once(tag.to_owned())
                .chain(soft.then(|| format!("{tag}cast")))
                .chain(layers.iter().cloned())
            {
                commands.push(format!("drawtext@{pass} borderw {border}"));
            }
            for pass in std::iter::once(tag.to_owned()).chain(boxed.then(|| format!("{tag}box"))) {
                commands.push(format!("drawtext@{pass} boxborderw {}", look.padding));
            }
            match shadow {
                Some((shadow, _)) if soft => {
                    let sigma = if shadow.blur < SHARP_SHADOW {
                        0.0
                    } else {
                        shadow.blur
                    };
                    commands.extend(
                        ["sigma", "sigmaV"]
                            .map(|option| format!("gblur@{tag} {option} {sigma:.2}")),
                    );
                }
                Some((_, (dx, dy))) => commands.extend([
                    format!("drawtext@{tag} shadowx {dx}"),
                    format!("drawtext@{tag} shadowy {dy}"),
                ]),
                None => {}
            }
            let from = seconds(*n as f64 - 0.5).max(0.0);
            let _ = write!(file, "{from:.6}");
            if let Some(next) = self.runs.get(index + 1) {
                let _ = write!(file, "-{:.6}", seconds(next.0 as f64 - 0.5));
            }
            let _ = writeln!(file, " {};", commands.join(", "));
        }
        let commands = FilterGraphFile::write("cmd", file.as_bytes())?;
        let path = filter_value(&commands.0.to_string_lossy());
        Ok((format!("sendcmd=f={path}"), commands))
    }
}

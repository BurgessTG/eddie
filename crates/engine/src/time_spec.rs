//! One time grammar for scripts, chat and the jump box, adapted from
//! fframes' `time_spec.rs` (MIT). Bare numbers are seconds, as everywhere
//! else in Eddie (fframes reads them as frames).
//!
//! | spec                       | meaning                                   |
//! |----------------------------|-------------------------------------------|
//! | `12`, `3.2s`               | seconds                                   |
//! | `120f`                     | a frame at the project rate               |
//! | `1:05.5`, `1:02:03`        | `m:ss` or `h:mm:ss`                       |
//! | `00:01:05:12`              | the readout's `hh:mm:ss:ff` timecode      |
//! | `50%`                      | a fraction of the timeline                |
//! | `start`, `end`             | the timeline's ends                       |
//! | `clip:7`, `clip:7@end`     | a clip's start or end                     |
//! | `clip:7@1.5s`, `clip:7@50%`| an offset into a clip (any time form)     |
//!
//! Ranges are half-open `a..b`, `a..`, `..b`, or `clip:7` for a whole clip.

use crate::ClipId;

/// What a spec resolves against: the project rate, the timeline end and
/// each clip's `(id, start, end)`.
pub struct TimeIndex {
    pub fps: f64,
    pub end: f64,
    pub clips: Vec<(ClipId, f64, f64)>,
}

const FORMS: &str =
    "use seconds (3.2s), a frame (120f), 1:05.5, 00:01:05:12, 50%, start, end or clip:7@end";

impl TimeIndex {
    /// The timeline time a spec names.
    pub fn time(&self, spec: &str) -> Result<f64, String> {
        let spec = spec.trim();
        if let Some(rest) = spec.strip_prefix("clip:") {
            let (id, offset) = rest.split_once('@').unwrap_or((rest, "start"));
            let (start, end) = self.clip(id)?;
            return Self::within(offset, start, end, self.fps)
                .ok_or_else(|| format!("can not parse \"{spec}\": {FORMS}"));
        }
        Self::within(spec, 0.0, self.end, self.fps)
            .ok_or_else(|| format!("can not parse \"{spec}\": {FORMS}"))
    }

    /// The half-open `(start, end)` a range spec names.
    pub fn range(&self, spec: &str) -> Result<(f64, f64), String> {
        let spec = spec.trim();
        let (start, end) = match spec.split_once("..") {
            Some((from, to)) => (
                if from.trim().is_empty() {
                    Ok(0.0)
                } else {
                    self.time(from)
                }?,
                if to.trim().is_empty() {
                    Ok(self.end)
                } else if to.trim().starts_with("clip:") && !to.contains('@') {
                    // A clip on the right ends the range at that clip's end.
                    self.time(&format!("{}@end", to.trim()))
                } else {
                    self.time(to)
                }?,
            ),
            None if spec.starts_with("clip:") && !spec.contains('@') => {
                self.clip(&spec["clip:".len()..])?
            }
            None => {
                return Err(format!(
                    "\"{spec}\" is not a range: use a..b, a.., ..b or clip:7"
                ))
            }
        };
        if end <= start {
            return Err(format!(
                "range \"{spec}\" is empty ({start:.3}s..{end:.3}s)"
            ));
        }
        Ok((start, end))
    }

    fn clip(&self, id: &str) -> Result<(f64, f64), String> {
        let wanted: ClipId = id
            .trim()
            .parse()
            .map_err(|_| format!("\"clip:{id}\" needs a numeric clip id"))?;
        self.clips
            .iter()
            .find(|clip| clip.0 == wanted)
            .map(|&(_, start, end)| (start, end))
            .ok_or_else(|| {
                let mut ids: Vec<String> = self
                    .clips
                    .iter()
                    .take(24)
                    .map(|c| c.0.to_string())
                    .collect();
                if self.clips.len() > ids.len() {
                    ids.push("…".to_owned());
                }
                if ids.is_empty() {
                    format!("no clip {wanted}: the timeline is empty")
                } else {
                    format!("no clip {wanted}; clips: {}", ids.join(", "))
                }
            })
    }

    /// A time inside `[start, end]`: an offset from `start`, a percentage
    /// of the span, or its named ends.
    fn within(spec: &str, start: f64, end: f64, fps: f64) -> Option<f64> {
        let spec = spec.trim();
        let offset = match spec {
            "start" => 0.0,
            "end" => end - start,
            _ => {
                if let Some(percent) = spec.strip_suffix('%') {
                    (end - start) * number(percent)? / 100.0
                } else if let Some(frames) = spec.strip_suffix('f') {
                    number(frames)? / fps
                } else if let Some(seconds) = spec.strip_suffix('s') {
                    number(seconds)?
                } else if spec.contains(':') {
                    clock(spec, fps)?
                } else {
                    number(spec)?
                }
            }
        };
        Some(start + offset)
    }
}

fn number(text: &str) -> Option<f64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

/// `m:ss`, `h:mm:ss` (fractional seconds allowed) or `hh:mm:ss:ff`.
fn clock(text: &str, fps: f64) -> Option<f64> {
    let parts: Vec<&str> = text.split(':').collect();
    let whole = |part: &str| part.parse::<u64>().ok().map(|value| value as f64);
    match parts.as_slice() {
        [m, s] => Some(whole(m)? * 60.0 + seconds(s)?),
        [h, m, s] => Some(whole(h)? * 3600.0 + whole(m)? * 60.0 + seconds(s)?),
        [h, m, s, f] => {
            let frame = whole(f)?;
            (frame < fps.round().max(1.0))
                .then(|| whole(h).zip(whole(m)).zip(whole(s)))
                .flatten()
                .map(|((h, m), s)| h * 3600.0 + m * 60.0 + s + frame / fps)
        }
        _ => None,
    }
}

/// Seconds within a minute; `5.5` and `05` both work.
fn seconds(text: &str) -> Option<f64> {
    number(text).filter(|value| *value >= 0.0 && *value < 60.0 && !text.starts_with(['-', '+']))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> TimeIndex {
        TimeIndex {
            fps: 30.0,
            end: 120.0,
            clips: vec![(7, 10.0, 14.0), (9, 14.0, 20.0)],
        }
    }

    #[test]
    fn times_resolve_every_form() {
        let index = index();
        for (spec, expected) in [
            ("12", 12.0),
            (" 3.2s ", 3.2),
            ("90f", 3.0),
            ("1:05.5", 65.5),
            ("1:02:03", 3723.0),
            // The transport readout's own format round-trips.
            ("00:01:05:15", 65.5),
            ("25%", 30.0),
            ("start", 0.0),
            ("end", 120.0),
            ("clip:7", 10.0),
            ("clip:7@end", 14.0),
            ("clip:7@1.5s", 11.5),
            ("clip:7@50%", 12.0),
            ("clip:9@30f", 15.0),
        ] {
            let got = index.time(spec).unwrap_or_else(|e| panic!("{spec}: {e}"));
            assert!((got - expected).abs() < 1e-9, "{spec}: {got} vs {expected}");
        }
    }

    #[test]
    fn bad_specs_say_what_is_valid() {
        let index = index();
        for spec in ["", "soon", "1:75", "00:00:01:30", "nanf", "1:-5", "clip:x"] {
            assert!(index.time(spec).is_err(), "{spec} must be rejected");
        }
        let unknown = index.time("clip:8@end").unwrap_err();
        assert!(unknown.contains("clips: 7, 9"), "{unknown}");
        assert!(index.time("later").unwrap_err().contains("clip:7@end"));
    }

    #[test]
    fn ranges_are_half_open_and_nonempty() {
        let index = index();
        assert_eq!(index.range("clip:7").unwrap(), (10.0, 14.0));
        // A bare clip on the right means its end.
        assert_eq!(index.range("clip:7..clip:9").unwrap(), (10.0, 20.0));
        assert_eq!(index.range("1:00..").unwrap(), (60.0, 120.0));
        assert_eq!(index.range("..2s").unwrap(), (0.0, 2.0));
        assert_eq!(
            index
                .range("clip:9@end..clip:9")
                .unwrap_err()
                .contains("empty"),
            true
        );
        assert!(index.range("12").is_err());
    }
}

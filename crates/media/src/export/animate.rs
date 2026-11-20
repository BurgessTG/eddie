//! Keyframes in export: FFmpeg expressions and commands that change a value
//! every frame along the same straight lines the preview follows
//! (`openreel_engine::key_value`).

/// `keys` (time on `var`'s clock, value) as one FFmpeg expression in `var`:
/// the first value, plus each step's change times how far through the step
/// `var` is, clipped to 0..1. That is `key_value`'s curve, holds included,
/// without nesting an `if` per key.
pub(super) fn curve(keys: &[(f64, f64)], var: &str) -> String {
    let Some(&(_, first)) = keys.first() else {
        return "0".to_owned();
    };
    let mut expr = format!("{first:.6}");
    for pair in keys.windows(2) {
        let ((a, va), (b, vb)) = (pair[0], pair[1]);
        if vb != va {
            expr.push_str(&format!(
                "+({:.6})*clip(({var}-({a:.6}))/{:.6},0,1)",
                vb - va,
                b - a
            ));
        }
    }
    expr
}

/// The `sendcmd` commands that set filter `target`'s `option` to `keys`'
/// value on every frame (times in seconds of the frames' own clock). Each
/// step is one interval whose argument is a line through it, so no
/// argument holds a comma, which `sendcmd` would read as the next command;
/// the last value holds from its key on. Intervals cannot start before 0.
pub(super) fn commands(target: &str, option: &str, keys: &[(f64, f64)]) -> String {
    let command = |from: f64, to: Option<f64>, arg: String| {
        let to = to.map(|to| format!("-{to:.6}")).unwrap_or_default();
        format!("{:.6}{to} [expr] {target} {option} {arg}", from.max(0.0))
    };
    let mut out = Vec::new();
    let (Some(&(first_at, first)), Some(&(last_at, last))) = (keys.first(), keys.last()) else {
        return String::new();
    };
    if first_at > 0.0 {
        out.push(command(0.0, Some(first_at), format!("{first:.6}")));
    }
    for pair in keys.windows(2) {
        let ((a, va), (b, vb)) = (pair[0], pair[1]);
        if b <= 0.0 {
            continue;
        }
        out.push(command(
            a,
            Some(b),
            format!("{va:.6}+({:.6})*(T-({a:.6}))/{:.6}", vb - va, b - a),
        ));
    }
    out.push(command(last_at, None, format!("{last:.6}")));
    out.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Evaluates `curve`'s output the way FFmpeg would, for its own terms.
    fn eval(keys: &[(f64, f64)], t: f64) -> f64 {
        let mut v = keys[0].1;
        for pair in keys.windows(2) {
            let ((a, va), (b, vb)) = (pair[0], pair[1]);
            v += (vb - va) * ((t - a) / (b - a)).clamp(0.0, 1.0);
        }
        v
    }

    #[test]
    fn the_flat_curve_is_the_preview_s_line_through_the_keys() {
        let keys = [(-0.5, 0.2), (1.0, 1.4), (2.5, 1.4), (3.0, 0.1)];
        let engine: Vec<_> = keys
            .iter()
            .map(|&(at, value)| openreel_engine::Key { at, value })
            .collect();
        for t in [-1.0, -0.5, 0.0, 0.7, 1.0, 2.0, 2.75, 3.0, 9.0] {
            let want = openreel_engine::key_value(&engine, t).unwrap();
            assert!((eval(&keys, t) - want).abs() < 1e-12, "{t}");
        }
        let expr = curve(&keys, "t");
        // The flat step adds no term.
        assert_eq!(expr.matches("clip(").count(), 2, "{expr}");
        assert!(expr.starts_with("0.200000+(1.200000)*clip((t-(-0.500000))/1.500000,0,1)"));
    }

    #[test]
    fn commands_cover_the_frames_from_zero_without_commas() {
        let keys = [(-0.5, 0.2), (1.0, 0.8), (2.0, 0.4)];
        let out = commands("colorchannelmixer@op1", "aa", &keys);
        assert!(!out.contains(','), "{out}");
        assert_eq!(
            out,
            "0.000000-1.000000 [expr] colorchannelmixer@op1 aa \
             0.200000+(0.600000)*(T-(-0.500000))/1.500000;\
             1.000000-2.000000 [expr] colorchannelmixer@op1 aa \
             0.800000+(-0.400000)*(T-(1.000000))/1.000000;\
             2.000000 [expr] colorchannelmixer@op1 aa 0.400000"
        );
    }
}

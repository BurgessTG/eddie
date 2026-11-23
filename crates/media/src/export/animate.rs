//! Keyframes in export: FFmpeg expressions and commands that change a value
//! every frame along the same curves the preview follows
//! (`openreel_engine::key_value`).

use openreel_engine::{Ease, Key};

/// `ease` applied to `p`, an expression from 0 to 1 that it repeats. Only
/// ease in and out has commas (an `if`), so [`commands`] writes that one
/// itself.
fn eased(ease: Ease, p: &str) -> String {
    match ease {
        Ease::Linear => p.to_owned(),
        Ease::EaseIn => format!("({p})*({p})*({p})"),
        Ease::EaseOut => format!("(1-(1-({p}))*(1-({p}))*(1-({p})))"),
        Ease::EaseInOut => {
            format!("if(lt({p},0.5),4*({p})*({p})*({p}),1-(2-2*({p}))*(2-2*({p}))*(2-2*({p}))/2)")
        }
    }
}

/// `keys` (time on `var`'s clock, value, ease) as one FFmpeg expression in
/// `var`: the first value, plus each run's change times its ease of how
/// far through the run `var` is, clipped to 0..1. That is `key_value`'s
/// curve, holds included, without nesting an `if` per key.
pub(super) fn curve(keys: &[Key], var: &str) -> String {
    let Some(first) = keys.first() else {
        return "0".to_owned();
    };
    let mut expr = format!("{:.6}", first.value);
    for pair in keys.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if b.value != a.value {
            let p = format!("clip(({var}-({:.6}))/{:.6},0,1)", a.at, b.at - a.at);
            expr.push_str(&format!(
                "+({:.6})*{}",
                b.value - a.value,
                eased(a.ease, &p)
            ));
        }
    }
    expr
}

/// The `sendcmd` commands that set filter `target`'s `option` to `keys'`
/// value on every frame (times in seconds of the frames' own clock). Each
/// run is an interval whose argument is its curve, so no argument holds a
/// comma, which `sendcmd` would read as the next command: ease in and out,
/// which needs an `if`, is two intervals split at its middle. The last
/// value holds from its key on. Intervals cannot start before 0.
pub(super) fn commands(target: &str, option: &str, keys: &[Key]) -> String {
    let command = |from: f64, to: Option<f64>, arg: String| {
        let to = to.map(|to| format!("-{to:.6}")).unwrap_or_default();
        format!("{:.6}{to} [expr] {target} {option} {arg}", from.max(0.0))
    };
    let mut out = Vec::new();
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else {
        return String::new();
    };
    if first.at > 0.0 {
        out.push(command(0.0, Some(first.at), format!("{:.6}", first.value)));
    }
    for pair in keys.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if b.at <= 0.0 {
            continue;
        }
        let change = b.value - a.value;
        let p = format!("(T-({:.6}))/{:.6}", a.at, b.at - a.at);
        let run = |from: f64, to: f64, share: String| {
            command(
                from,
                Some(to),
                format!("{:.6}+({change:.6})*{share}", a.value),
            )
        };
        if a.ease == Ease::EaseInOut {
            let middle = (a.at + b.at) / 2.0;
            if middle > 0.0 {
                out.push(run(a.at, middle, format!("4*({p})*({p})*({p})")));
            }
            let late = format!("(2-2*({p}))");
            out.push(run(middle, b.at, format!("(1-{late}*{late}*{late}/2)")));
        } else {
            out.push(run(a.at, b.at, eased(a.ease, &p)));
        }
    }
    out.push(command(last.at, None, format!("{:.6}", last.value)));
    out.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(at: f64, value: f64, ease: Ease) -> Key {
        Key { at, value, ease }
    }

    /// Evaluates the FFmpeg expressions `curve` and `commands` write, for
    /// the few functions they use, at `var` (or `T`) = `t`.
    fn eval(expr: &str, t: f64) -> f64 {
        let mut p = Parser {
            s: expr.as_bytes(),
            i: 0,
            t,
        };
        let v = p.sum();
        assert_eq!(p.i, expr.len(), "parsed all of {expr}");
        v
    }

    struct Parser<'a> {
        s: &'a [u8],
        i: usize,
        t: f64,
    }

    impl Parser<'_> {
        fn peek(&self) -> Option<u8> {
            self.s.get(self.i).copied()
        }
        fn sum(&mut self) -> f64 {
            let mut v = self.product();
            while let Some(op @ (b'+' | b'-')) = self.peek() {
                self.i += 1;
                let r = self.product();
                v = if op == b'+' { v + r } else { v - r };
            }
            v
        }
        fn product(&mut self) -> f64 {
            let mut v = self.unary();
            while let Some(op @ (b'*' | b'/')) = self.peek() {
                self.i += 1;
                let r = self.unary();
                v = if op == b'*' { v * r } else { v / r };
            }
            v
        }
        fn unary(&mut self) -> f64 {
            if self.peek() == Some(b'-') {
                self.i += 1;
                return -self.unary();
            }
            self.atom()
        }
        fn args(&mut self) -> Vec<f64> {
            let mut out = vec![self.sum()];
            while self.peek() == Some(b',') {
                self.i += 1;
                out.push(self.sum());
            }
            assert_eq!(self.peek(), Some(b')'));
            self.i += 1;
            out
        }
        fn atom(&mut self) -> f64 {
            let rest = &self.s[self.i..];
            for (name, arity) in [("clip(", 3), ("if(", 3), ("lt(", 2)] {
                if rest.starts_with(name.as_bytes()) {
                    self.i += name.len();
                    let a = self.args();
                    assert_eq!(a.len(), arity);
                    return match name {
                        "clip(" => a[0].clamp(a[1], a[2]),
                        "if(" => {
                            if a[0] != 0.0 {
                                a[1]
                            } else {
                                a[2]
                            }
                        }
                        _ => f64::from(u8::from(a[0] < a[1])),
                    };
                }
            }
            match self.peek() {
                Some(b'(') => {
                    self.i += 1;
                    let v = self.sum();
                    assert_eq!(self.peek(), Some(b')'));
                    self.i += 1;
                    v
                }
                Some(b't' | b'T') => {
                    self.i += 1;
                    self.t
                }
                _ => {
                    let start = self.i;
                    while self.peek().is_some_and(|c| c.is_ascii_digit() || c == b'.') {
                        self.i += 1;
                    }
                    std::str::from_utf8(&self.s[start..self.i])
                        .unwrap()
                        .parse()
                        .unwrap()
                }
            }
        }
    }

    /// One key of every ease, then a flat run and a last key.
    fn every_ease() -> Vec<Key> {
        vec![
            key(-0.5, 0.2, Ease::EaseInOut),
            key(1.0, 1.4, Ease::EaseIn),
            key(2.0, 0.6, Ease::EaseOut),
            key(2.5, 1.0, Ease::Linear),
            key(3.0, 1.0, Ease::EaseIn),
            key(4.0, 0.1, Ease::Linear),
        ]
    }

    #[test]
    fn the_curve_is_the_preview_s_through_the_keys() {
        let keys = every_ease();
        let expr = curve(&keys, "t");
        for i in -20..=50 {
            let t = f64::from(i) / 10.0;
            let want = openreel_engine::key_value(&keys, t).unwrap();
            let got = eval(&expr, t);
            assert!((got - want).abs() < 1e-5, "{t}: {got} against {want}");
        }
        // The flat run from 2.5 s adds no term.
        assert!(!expr.contains("(t-(2.500000))"), "{expr}");
        assert!(expr.contains("(t-(3.000000))"), "{expr}");
    }

    #[test]
    fn commands_follow_the_preview_from_zero_without_commas() {
        let keys = every_ease();
        let out = commands("colorchannelmixer@op1", "aa", &keys);
        assert!(!out.contains(','), "{out}");
        // Each frame's value is the argument of the interval holding it.
        let value = |t: f64| {
            out.split(';')
                .find_map(|command| {
                    let (times, arg) = command.split_once(" [expr] ").unwrap();
                    let arg = arg.rsplit(' ').next().unwrap();
                    let (from, to) = match times.split_once('-') {
                        Some((a, b)) => (a.parse::<f64>().unwrap(), b.parse().unwrap()),
                        None => (times.parse().unwrap(), f64::INFINITY),
                    };
                    (from..to).contains(&t).then(|| eval(arg, t))
                })
                .unwrap()
        };
        for i in 0..=50 {
            let t = f64::from(i) / 10.0;
            let want = openreel_engine::key_value(&keys, t).unwrap();
            assert!(
                (value(t) - want).abs() < 1e-5,
                "{t}: {} against {want}",
                value(t)
            );
        }
        assert!(out.starts_with("0.000000-0.250000 [expr] colorchannelmixer@op1 aa 0.200000+"));
    }
}

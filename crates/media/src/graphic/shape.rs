//! A shape as SVG: its outline path in a box whose long side is 1000
//! units, painted with its fill and outline.

use std::fmt::Write;

use openreel_engine::{Shape, ShapeKind};

/// The box's long side, in SVG units.
const LONG: f64 = 1000.0;

/// The shape's SVG. The outline lies inside the box: the path is inset by
/// half its thickness, so no point of the stroke reaches past the box.
pub(super) fn svg(shape: &Shape) -> String {
    let (w, h) = if shape.ratio >= 1.0 {
        (LONG, LONG / shape.ratio)
    } else {
        (LONG * shape.ratio, LONG)
    };
    let stroke = shape
        .outline
        .map(|color| (color, shape.thickness * w.min(h)));
    let inset = stroke.map_or(0.0, |(_, width)| width / 2.0);
    let path = path(shape, [inset, inset, w - inset, h - inset]);
    let fill = shape
        .fill
        .map_or_else(|| "none".to_owned(), |color| color.hex());
    // A rectangle's corners are square, so a mitred join ends exactly at
    // the box's corner; sharper corners join round to stay inside it.
    let join = if shape.kind == ShapeKind::Rectangle {
        "miter"
    } else {
        "round"
    };
    let stroke = stroke.map_or_else(String::new, |(color, width)| {
        format!(
            r#" stroke="{}" stroke-width="{width:.2}" stroke-linejoin="{join}""#,
            color.hex()
        )
    });
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w:.2}" height="{h:.2}" viewBox="0 0 {w:.2} {h:.2}"><path d="{path}" fill="{fill}"{stroke}/></svg>"#
    )
}

/// The outline path of `shape` filling the box `[x0, y0, x1, y1]`.
fn path(shape: &Shape, [x0, y0, x1, y1]: [f64; 4]) -> String {
    let (bw, bh) = (x1 - x0, y1 - y0);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let mut d = String::new();
    match shape.kind {
        ShapeKind::Rectangle => {
            let r = shape.round * bw.min(bh) / 2.0;
            rounded(&mut d, [x0, y0, x1, y1], r, None);
        }
        ShapeKind::Circle => {
            let (rx, ry) = (bw / 2.0, bh / 2.0);
            let _ = write!(
                d,
                "M{x0:.2} {cy:.2}A{rx:.2} {ry:.2} 0 1 0 {x1:.2} {cy:.2}A{rx:.2} {ry:.2} 0 1 0 {x0:.2} {cy:.2}Z"
            );
        }
        ShapeKind::Triangle => polygon(&mut d, &[(cx, y0), (x1, y1), (x0, y1)]),
        ShapeKind::Star => {
            // A regular star's inner points sit at 0.382 of the outer radius.
            let points: Vec<(f64, f64)> = (0..10)
                .map(|i| {
                    let radius = if i % 2 == 0 { 1.0 } else { 0.381_966 };
                    let angle = (-90.0 + 36.0 * f64::from(i)).to_radians();
                    (radius * angle.cos(), radius * angle.sin())
                })
                .collect();
            polygon(&mut d, &fitted(&points, [x0, y0, x1, y1]));
        }
        ShapeKind::Heart => {
            // Two lobes meeting at the top and a point at the bottom, in a
            // unit box; each side's widest and highest points are curve
            // ends, so the heart touches every side of its box.
            let at = |u: f64, v: f64| (x0 + u * bw, y0 + v * bh);
            let p = |(x, y): (f64, f64)| format!("{x:.2} {y:.2}");
            let curves = [
                [(0.5, 0.2), (0.45, 0.07), (0.36, 0.0), (0.25, 0.0)],
                [(0.25, 0.0), (0.11, 0.0), (0.0, 0.11), (0.0, 0.3)],
                [(0.0, 0.3), (0.0, 0.55), (0.25, 0.75), (0.5, 1.0)],
                [(0.5, 1.0), (0.75, 0.75), (1.0, 0.55), (1.0, 0.3)],
                [(1.0, 0.3), (1.0, 0.11), (0.89, 0.0), (0.75, 0.0)],
                [(0.75, 0.0), (0.64, 0.0), (0.55, 0.07), (0.5, 0.2)],
            ];
            let _ = write!(d, "M{}", p(at(0.5, 0.2)));
            for [_, a, b, end] in curves {
                let _ = write!(
                    d,
                    "C{} {} {}",
                    p(at(a.0, a.1)),
                    p(at(b.0, b.1)),
                    p(at(end.0, end.1))
                );
            }
            d.push('Z');
        }
        ShapeKind::Arrow => {
            // The head is at most 90% of the height long and 60% of the
            // width; the shaft is 40% of the height thick.
            let head = (bh * 0.9).min(bw * 0.6);
            let half = bh * 0.2;
            let neck = x1 - head;
            polygon(
                &mut d,
                &[
                    (x0, cy - half),
                    (neck, cy - half),
                    (neck, y0),
                    (x1, cy),
                    (neck, y1),
                    (neck, cy + half),
                    (x0, cy + half),
                ],
            );
        }
        ShapeKind::Bubble => {
            // The body is the top 80%; the tail hangs from its lower left.
            let bottom = y0 + bh * 0.8;
            let r = shape.round * bw.min(bottom - y0) / 2.0;
            let left = (x0 + bw * 0.16).max(x0 + r);
            let right = (left + bw * 0.18).min(x1 - r);
            let tip = (left - bw * 0.06).max(x0);
            rounded(
                &mut d,
                [x0, y0, x1, bottom],
                r,
                Some([(right, bottom), (tip, y1), (left, bottom)]),
            );
        }
    }
    d
}

/// A rectangle with corners of radius `r`, drawn clockwise from its top
/// left. `tail` goes along the bottom edge, from right to left.
fn rounded(d: &mut String, [x0, y0, x1, y1]: [f64; 4], r: f64, tail: Option<[(f64, f64); 3]>) {
    let _ = write!(
        d,
        "M{:.2} {y0:.2}H{:.2}A{r:.2} {r:.2} 0 0 1 {x1:.2} {:.2}V{:.2}A{r:.2} {r:.2} 0 0 1 {:.2} {y1:.2}",
        x0 + r,
        x1 - r,
        y0 + r,
        y1 - r,
        x1 - r,
    );
    for (x, y) in tail.into_iter().flatten() {
        let _ = write!(d, "L{x:.2} {y:.2}");
    }
    let _ = write!(
        d,
        "H{:.2}A{r:.2} {r:.2} 0 0 1 {x0:.2} {:.2}V{:.2}A{r:.2} {r:.2} 0 0 1 {:.2} {y0:.2}Z",
        x0 + r,
        y1 - r,
        y0 + r,
        x0 + r,
    );
}

fn polygon(d: &mut String, points: &[(f64, f64)]) {
    for (index, (x, y)) in points.iter().enumerate() {
        let _ = write!(d, "{}{x:.2} {y:.2}", if index == 0 { 'M' } else { 'L' });
    }
    d.push('Z');
}

/// `points` stretched so their bounds fill the box.
fn fitted(points: &[(f64, f64)], [x0, y0, x1, y1]: [f64; 4]) -> Vec<(f64, f64)> {
    let bounds = points.iter().fold(
        [f64::MAX, f64::MAX, f64::MIN, f64::MIN],
        |[a, b, c, d], &(x, y)| [a.min(x), b.min(y), c.max(x), d.max(y)],
    );
    points
        .iter()
        .map(|&(x, y)| {
            (
                x0 + (x - bounds[0]) / (bounds[2] - bounds[0]) * (x1 - x0),
                y0 + (y - bounds[1]) / (bounds[3] - bounds[1]) * (y1 - y0),
            )
        })
        .collect()
}

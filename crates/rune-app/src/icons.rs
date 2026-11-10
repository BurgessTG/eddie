//! Glyphs drawn as shapes, with the egui window's numbers, so both windows
//! show the same icons without an icon font.

use std::f32::consts::{PI, TAU};

use rune::{Color, LineCap, Shape};

use crate::theme;

type P = [f32; 2];

/// A circle as a closed polygon.
pub fn circle(c: P, r: f32) -> Vec<P> {
    let n = ((r * 4.0) as usize).clamp(16, 64);
    (0..n)
        .map(|i| {
            let a = TAU * i as f32 / n as f32;
            [c[0] + r * a.cos(), c[1] + r * a.sin()]
        })
        .collect()
}

/// A rounded rectangle as a closed polygon.
pub fn round_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<P> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    if r <= 0.01 {
        return vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]];
    }
    let corners = [
        ([x + w - r, y + r], -PI / 2.0),
        ([x + w - r, y + h - r], 0.0),
        ([x + r, y + h - r], PI / 2.0),
        ([x + r, y + r], PI),
    ];
    let mut points = Vec::with_capacity(20);
    for (c, from) in corners {
        for i in 0..=4 {
            let a = from + PI / 2.0 * i as f32 / 4.0;
            points.push([c[0] + r * a.cos(), c[1] + r * a.sin()]);
        }
    }
    points
}

pub fn disc(shape: Shape, c: P, r: f32, color: Color) -> Shape {
    shape.fill(circle(c, r), color)
}

pub(crate) fn ring(shape: Shape, c: P, r: f32, width: f32, color: Color) -> Shape {
    shape.stroke_with(circle(c, r), true, width, LineCap::Round, color)
}

pub(crate) fn outline(
    shape: Shape,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    width: f32,
    color: Color,
) -> Shape {
    // egui strokes these inside the rect.
    let inset = width / 2.0;
    shape.stroke_with(
        round_rect(
            x + inset,
            y + inset,
            w - width,
            h - width,
            (r - inset).max(0.0),
        ),
        true,
        width,
        LineCap::Round,
        color,
    )
}

fn seg(shape: Shape, a: P, b: P, width: f32, color: Color) -> Shape {
    shape.line(a, b, width, LineCap::Butt, color)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Magnet,
    Split,
    Delete,
    ZoomIn,
    ZoomOut,
    Settings,
}

/// A tool glyph in a 30×30 box.
pub fn tool(icon: Tool, fg: Color) -> Shape {
    let (cx, cy) = (15.0_f32, 15.0_f32);
    let at = |dx: f32, dy: f32| [cx + dx, cy + dy];
    let shape = Shape::new();
    match icon {
        Tool::ZoomIn | Tool::ZoomOut => {
            let mut shape = ring(shape, at(-1.5, -1.5), 5.5, 1.6, fg);
            shape = seg(shape, at(2.5, 2.5), at(7.0, 7.0), 1.6, fg);
            shape = seg(shape, at(-4.0, -1.5), at(1.0, -1.5), 1.4, fg);
            if icon == Tool::ZoomIn {
                shape = seg(shape, at(-1.5, -4.0), at(-1.5, 1.0), 1.4, fg);
            }
            shape
        }
        Tool::Magnet => {
            let mut body = vec![at(-5.0, -3.5)];
            body.extend((0..=12).map(|i| {
                let angle = PI * (i as f32 / 12.0);
                at(-5.0 * angle.cos(), 1.0 + 5.0 * angle.sin())
            }));
            body.push(at(5.0, -3.5));
            let mut shape = shape.stroke_with(body, false, 3.0, LineCap::Butt, fg);
            for x in [-5.0_f32, 5.0] {
                shape = shape.fill(round_rect(cx + x - 1.5, cy - 8.5, 3.0, 3.0, 1.0), fg);
            }
            shape
        }
        Tool::Settings => {
            let mut shape = ring(shape, at(0.0, 0.0), 4.6, 1.6, fg);
            for i in 0..8 {
                let angle = TAU * i as f32 / 8.0;
                let (x, y) = (angle.cos(), angle.sin());
                shape = seg(shape, at(x * 5.4, y * 5.4), at(x * 7.6, y * 7.6), 2.2, fg);
            }
            disc(shape, at(0.0, 0.0), 1.6, fg)
        }
        Tool::Split => {
            let mut shape = outline(shape, cx - 9.0, cy - 5.0, 7.0, 10.0, 2.0, 1.6, fg);
            shape = outline(shape, cx + 2.0, cy - 5.0, 7.0, 10.0, 2.0, 1.6, fg);
            seg(
                shape,
                at(0.0, -8.0),
                at(0.0, 8.0),
                1.4,
                theme::c().destructive,
            )
        }
        Tool::Delete => {
            let mut shape = seg(shape, at(-7.0, -5.0), at(7.0, -5.0), 1.6, fg);
            shape = seg(shape, at(-3.0, -8.0), at(3.0, -8.0), 1.6, fg);
            shape = outline(shape, cx - 5.5, cy - 5.0, 11.0, 13.0, 2.0, 1.6, fg);
            for dx in [-2.5_f32, 0.0, 2.5] {
                shape = seg(shape, at(dx, -1.5), at(dx, 5.0), 1.2, fg);
            }
            shape
        }
    }
    .view_box(30.0, 30.0)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transport {
    ToStart,
    StepBack,
    Play,
    Pause,
    StepForward,
    ToEnd,
}

/// A transport glyph in a 38×32 box. The play disc is the button's own fill.
pub fn transport(icon: Transport, fg: Color) -> Shape {
    let cx = if icon == Transport::Play {
        20.0_f32
    } else {
        19.0
    };
    let cy = 16.0_f32;
    let at = |dx: f32, dy: f32| [cx + dx, cy + dy];
    let tri = |flip: f32| {
        vec![
            at(-5.0 * flip, -7.0),
            at(7.0 * flip, 0.0),
            at(-5.0 * flip, 7.0),
        ]
    };
    let bar = |x: f32, w: f32, r: f32| round_rect(cx + x, cy - 7.0, w, 14.0, r);
    let shape = Shape::new();
    match icon {
        Transport::StepBack | Transport::StepForward => {
            let flip = if icon == Transport::StepForward {
                1.0
            } else {
                -1.0
            };
            let shape = shape.stroke(
                vec![
                    at(-3.0 * flip, -5.0),
                    at(2.0 * flip, 0.0),
                    at(-3.0 * flip, 5.0),
                ],
                1.8,
                fg,
            );
            seg(shape, at(5.5 * flip, -5.5), at(5.5 * flip, 5.5), 1.8, fg)
        }
        Transport::Play => shape.fill(tri(1.0), fg),
        Transport::Pause => shape
            .fill(bar(-5.0, 3.5, 1.0), fg)
            .fill(bar(2.0, 3.5, 1.0), fg),
        Transport::ToStart => shape.fill(bar(-8.0, 2.5, 0.0), fg).fill(tri(-1.0), fg),
        Transport::ToEnd => shape.fill(tri(1.0), fg).fill(bar(5.5, 2.5, 0.0), fg),
    }
    .view_box(38.0, 32.0)
}

/// A spinner arc: three quarters of a ring, turned by `turn` (0..1).
pub fn spinner(size: f32, turn: f32, color: Color) -> Shape {
    let c = size / 2.0;
    let r = size / 2.0 - 1.5;
    let points: Vec<P> = (0..=24)
        .map(|i| {
            let a = TAU * (turn + 0.75 * i as f32 / 24.0);
            [c + r * a.cos(), c + r * a.sin()]
        })
        .collect();
    Shape::new()
        .stroke(points, (size / 9.0).clamp(1.5, 3.0), color)
        .view_box(size, size)
}

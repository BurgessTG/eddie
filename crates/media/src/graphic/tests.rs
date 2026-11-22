use openreel_engine::{Graphic, Rgb, Shape, ShapeKind};

use super::*;

fn pixel(frame: &RawFrame, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * frame.width + x) * 4) as usize;
    frame.rgba[at..at + 4].try_into().unwrap()
}

#[test]
fn every_sticker_draws_something_square() {
    assert!(STICKERS.len() >= 50);
    for sticker in STICKERS {
        let frame = draw_graphic(
            &Graphic::Sticker {
                name: sticker.name.to_owned(),
            },
            64,
        )
        .unwrap_or_else(|error| panic!("{}: {error}", sticker.name));
        assert_eq!((frame.width, frame.height), (64, 64), "{}", sticker.name);
        let opaque = frame.rgba.chunks(4).filter(|p| p[3] == 255).count();
        assert!(opaque > 100, "{} draws almost nothing", sticker.name);
    }
    assert_eq!(
        sticker("Thumbs-Up").map(|s| s.label()),
        Some("Thumbs up".into())
    );
    assert!(draw_graphic(
        &Graphic::Sticker {
            name: "nope".into()
        },
        64
    )
    .is_err());
}

#[test]
fn a_shape_fills_its_box_and_keeps_its_outline_inside_it() {
    let shape = Shape {
        fill: Some(Rgb(255, 0, 0)),
        outline: Some(Rgb(0, 0, 255)),
        thickness: 0.1,
        ratio: 2.0,
        ..Shape::new(ShapeKind::Rectangle)
    };
    let frame = draw_graphic(&Graphic::Shape(shape), 400).unwrap();
    assert_eq!((frame.width, frame.height), (400, 200));
    // The outline is 10% of the 200-pixel side, inside the edge: an
    // outline centred on the edge would leave half of it clipped and the
    // fill showing at 15 pixels in.
    assert_eq!(pixel(&frame, 200, 1), [0, 0, 255, 255]);
    assert_eq!(pixel(&frame, 200, 15), [0, 0, 255, 255]);
    assert_eq!(pixel(&frame, 200, 25), [255, 0, 0, 255]);
    assert_eq!(
        pixel(&frame, 0, 0)[3],
        255,
        "the mitred corner reaches the box"
    );
    assert_eq!(pixel(&frame, 200, 100), [255, 0, 0, 255]);
}

#[test]
fn a_shape_is_clear_outside_and_its_soft_edge_keeps_its_colour() {
    let circle = Shape {
        fill: Some(Rgb(255, 128, 0)),
        ..Shape::new(ShapeKind::Circle)
    };
    let frame = draw_graphic(&Graphic::Shape(circle), 200).unwrap();
    assert_eq!(pixel(&frame, 2, 2)[3], 0, "a circle's corner is clear");
    assert_eq!(pixel(&frame, 100, 100), [255, 128, 0, 255]);
    // Pixels on the edge are partly covered. Straight alpha keeps the fill's
    // colour there; premultiplied pixels would darken it.
    let edge: Vec<[u8; 4]> = frame
        .rgba
        .chunks(4)
        .map(|p| [p[0], p[1], p[2], p[3]])
        .filter(|p| (20..=235).contains(&p[3]))
        .collect();
    assert!(!edge.is_empty());
    for p in edge {
        assert!(p[0] >= 253 && (126..=130).contains(&p[1]), "{p:?}");
    }

    // An outline alone leaves the middle clear.
    let ring = Shape {
        fill: None,
        outline: Some(Rgb::WHITE),
        ..Shape::new(ShapeKind::Circle)
    };
    let frame = draw_graphic(&Graphic::Shape(ring), 200).unwrap();
    assert_eq!(pixel(&frame, 100, 100)[3], 0);
    assert_eq!(pixel(&frame, 100, 3), [255, 255, 255, 255]);
}

#[test]
fn every_shape_draws_inside_its_box_at_its_proportions() {
    for kind in ShapeKind::ALL {
        let shape = Shape {
            outline: Some(Rgb::BLACK),
            ..Shape::new(kind)
        };
        let frame = draw_graphic(&Graphic::Shape(shape), 300).unwrap();
        let ratio = f64::from(frame.width) / f64::from(frame.height);
        assert!(
            (ratio - kind.natural_ratio()).abs() < 0.02,
            "{kind:?} {ratio}"
        );
        // Every shape reaches each side of its box, and the outline stops
        // there.
        let (w, h) = (frame.width, frame.height);
        let covered = |x: u32, y: u32| pixel(&frame, x, y)[3] > 0;
        assert!((0..h).any(|y| covered(0, y)), "{kind:?} left");
        assert!((0..h).any(|y| covered(w - 1, y)), "{kind:?} right");
        assert!((0..w).any(|x| covered(x, 0)), "{kind:?} top");
        assert!((0..w).any(|x| covered(x, h - 1)), "{kind:?} bottom");
    }
    assert!(!Shape {
        fill: None,
        outline: None,
        ..Shape::default()
    }
    .is_valid());
}

#[test]
fn a_graphic_file_is_named_by_its_drawing_and_probes_as_it_says() {
    let folder = std::env::temp_dir().join(format!("eddie-graphic-{}", std::process::id()));
    let star = Graphic::Shape(Shape::new(ShapeKind::Star));
    let file = write_graphic(&star, &folder).unwrap();
    assert_eq!(write_graphic(&star, &folder).unwrap().path, file.path);
    let yellow = Graphic::Shape(Shape {
        fill: Some(Rgb(255, 214, 0)),
        ..Shape::new(ShapeKind::Star)
    });
    assert_ne!(write_graphic(&yellow, &folder).unwrap().path, file.path);

    let probed = crate::probe(&file.info.path).unwrap();
    assert!(probed.photo && probed.alpha && probed.has_video && !probed.has_audio);
    assert_eq!(
        (probed.width, probed.height),
        (file.info.width, file.info.height)
    );
    assert_eq!(file.info.width.max(file.info.height), GRAPHIC_SIDE);
    assert_eq!(file.info.name, "Star");
    let _ = std::fs::remove_dir_all(folder);
}

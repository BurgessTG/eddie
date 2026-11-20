use openreel_engine::ClipKind;
use rune::{Key, Runtime};

use crate::app::Ui;
use crate::testing::{FRAME, click, frames, press, shown, window};

#[test]
fn double_clicking_a_caption_opens_its_text_for_typing() {
    let mut ui = Ui::headless();
    let text = "A person talking stays a person talking";
    let title = ui
        .editor
        .timeline
        .add_clip(text, ClipKind::Title, 0, 0.0, 4.0, 0.0);
    ui.editor.playhead = 1.0;
    ui.show_inspector = false;
    let mut rt = window(ui);
    let (x, y) = rt.find("monitor-title").expect("the caption is shown");
    rt.pointer_move(x, y);
    rt.frame(FRAME);
    for _ in 0..2 {
        rt.pointer_down(x, y);
        rt.frame(FRAME);
        rt.pointer_up(x, y);
        rt.frame(FRAME);
    }
    frames(&mut rt, 2);
    assert_eq!(rt.state.editor.selected, Some(title));
    assert!(rt.state.show_inspector, "Details opens for it");
    assert!(rt.state.typing(), "the text box has the keyboard");

    rt.key(rune::KeyEvent {
        key: Key::Text(" now".into()),
        mods: rune::Mods::default(),
    });
    frames(&mut rt, 2);
    assert_eq!(
        rt.state.editor.timeline.clip(title).unwrap().name,
        format!("{text} now"),
        "typing goes into the focused text box"
    );
}

/// Click the readout, type `spec` over the selected time, press Enter.
fn jump(rt: &mut Runtime<Ui>, spec: &str) {
    click(rt, "playhead-readout");
    assert!(shown(rt, "jump-box"), "a click opens the jump box");
    assert!(rt.state.typing());
    rt.paste(spec);
    rt.frame(FRAME);
    press(rt, Key::Enter);
}

#[test]
fn the_playhead_readout_jumps_to_a_typed_time() {
    let mut ui = Ui::headless();
    ui.editor
        .timeline
        .add_clip("Intro", ClipKind::Title, 0, 0.0, 8.0, 0.0);
    let outro = ui
        .editor
        .timeline
        .add_clip("Outro", ClipKind::Title, 0, 8.0, 4.0, 0.0);
    let mut rt = window(ui);
    jump(&mut rt, &format!("clip:{outro}@50%"));
    let playhead = rt.state.editor.playhead;
    assert!((playhead - 10.0).abs() < 1e-6, "got {playhead}");
    assert!(!shown(&rt, "jump-box"), "Enter closes the box");
    assert!(!rt.state.typing(), "and gives the keys back");

    jump(&mut rt, "clip:99");
    assert!(
        (rt.state.editor.playhead - 10.0).abs() < 1e-6,
        "a bad time does not move the playhead"
    );
    let status = &rt.state.editor.status;
    assert!(status.contains("no clip 99"), "{status}");

    // Escape leaves the playhead where it was.
    click(&mut rt, "playhead-readout");
    rt.paste("2");
    rt.frame(FRAME);
    press(&mut rt, Key::Escape);
    assert!(!shown(&rt, "jump-box"), "Esc closes the box");
    assert!((rt.state.editor.playhead - 10.0).abs() < 1e-6);
}

/// The scrubber spans the whole edit: a press a quarter of the way
/// along moves the playhead a quarter of the way through it.
#[test]
fn a_press_on_the_scrubber_seeks_in_proportion() {
    let mut ui = Ui::headless();
    ui.editor
        .timeline
        .add_clip("Intro", ClipKind::Title, 0, 0.0, 8.0, 0.0);
    let mut rt = window(ui);
    let ([x, y, w, h], _) = rt.presented("transport-scrub").expect("the scrubber");
    let y = y + h / 2.0;
    rt.pointer_move(x + w * 0.25, y);
    rt.frame(FRAME);
    rt.pointer_down(x + w * 0.25, y);
    rt.frame(FRAME);
    let playhead = rt.state.editor.playhead;
    assert!((playhead - 2.0).abs() < 0.1, "25% is 2 s, got {playhead}");
    assert!(rt.state.editor.transport_scrubbing, "held");

    rt.pointer_move(x + w * 0.75, y);
    rt.frame(FRAME);
    let playhead = rt.state.editor.playhead;
    assert!((playhead - 6.0).abs() < 0.1, "75% is 6 s, got {playhead}");

    rt.pointer_up(x + w * 0.75, y);
    frames(&mut rt, 2);
    assert!(
        !rt.state.editor.transport_scrubbing,
        "release ends the scrub"
    );
}

#[test]
fn flips_mirror_rows_and_columns() {
    use super::picture::flipped;
    // a b / c d, each pixel's four bytes standing for its letter.
    let image: Vec<u8> = [1, 2, 3, 4].into_iter().flat_map(|v: u8| [v; 4]).collect();
    let order = |rgba: Vec<u8>| {
        rgba.as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[0])
            .collect::<Vec<_>>()
    };
    assert_eq!(order(flipped(&image, 2, (false, false))), [1, 2, 3, 4]);
    assert_eq!(order(flipped(&image, 2, (true, false))), [2, 1, 4, 3]);
    assert_eq!(order(flipped(&image, 2, (false, true))), [3, 4, 1, 2]);
    assert_eq!(order(flipped(&image, 2, (true, true))), [4, 3, 2, 1]);
}

/// A 2 s 320x180 clip made with FFmpeg, removed with its folder.
struct Footage(std::path::PathBuf);

impl Footage {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "eddie-monitor-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let output = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("color=c=red:s=320x180:r=30:d=2")
            .args(["-c:v", "libx264", "-threads:v", "1", "-pix_fmt", "yuv420p"])
            .arg(dir.join("wide.mp4"))
            .output()
            .expect("generate footage");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self(dir)
    }

    fn path(&self) -> String {
        self.0.join("wide.mp4").to_string_lossy().into_owned()
    }
}

impl Drop for Footage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run frames until `done` holds, for at most 30 s.
fn until(rt: &mut Runtime<Ui>, what: &str, done: impl Fn(&Ui) -> bool) {
    let started = std::time::Instant::now();
    while !done(&rt.state) {
        assert!(
            started.elapsed().as_secs() < 30,
            "timed out waiting for {what}"
        );
        rt.frame(FRAME);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    frames(rt, 2);
}

#[test]
fn a_moved_scaled_turned_picture_previews_where_export_puts_it() {
    let footage = Footage::new();
    let path = footage.path();
    let mut ui = Ui::headless();
    ui.editor.spawn_import(vec![path.clone().into()]);
    let mut rt = window(ui);
    until(&mut rt, "the import", |ui| {
        ui.editor.asset(&path).is_some() && !ui.editor.importing()
    });
    let editor = &mut rt.state.editor;
    editor.add_asset_to_timeline(&path, None, 0.0);
    let id = editor.timeline.clips[0].id;
    // Half size, its centre a quarter right and a quarter up: on the 320x180
    // canvas that is x 160-320 and y 0-90, the top-right quarter.
    editor.edit_clip_transform(id, |t| {
        t.scale = 0.5;
        t.x = 0.25;
        t.y = -0.25;
    });
    editor.playhead = 0.5;
    until(&mut rt, "the picture", |ui| ui.editor.preview_is_current());

    let share = |rt: &Runtime<Ui>, key: &str| {
        let ([fx, fy, fw, fh], _) = rt.presented("monitor").unwrap();
        let ([x, y, w, h], _) = rt.presented(key).unwrap();
        [(x - fx) / fw, (y - fy) / fh, w / fw, h / fh]
    };
    let close =
        |got: [f32; 4], want: [f32; 4]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.01);
    let placed = share(&rt, "monitor-lane0-placed");
    assert!(close(placed, [0.5, 0.0, 0.5, 0.5]), "{placed:?}");

    // A quarter turn about the same centre stands the box on end: 90x160
    // canvas pixels around (240, 45).
    rt.state
        .editor
        .edit_clip_transform(id, |t| t.rotation = 90.0);
    frames(&mut rt, 2);
    let turned = share(&rt, "monitor-lane0-placed");
    let want = [
        (240.0 - 45.0) / 320.0,
        (45.0 - 80.0) / 180.0,
        90.0 / 320.0,
        160.0 / 180.0,
    ];
    assert!(close(turned, want), "{turned:?} against {want:?}");

    // Cropping the left half keeps the right half, fitted: the box is
    // 80x90 and the whole picture inside it starts one box width left.
    rt.state.editor.edit_clip_transform(id, |t| {
        t.rotation = 0.0;
        t.crop_left = 0.5;
    });
    frames(&mut rt, 2);
    let boxed = share(&rt, "monitor-lane0-placed");
    let whole = share(&rt, "monitor-lane0-picture");
    assert!(close(boxed, [0.625, 0.0, 0.25, 0.5]), "{boxed:?}");
    assert!(close(whole, [0.375, 0.0, 0.5, 0.5]), "{whole:?}");
}

/// A half-size clip on video lane 0 over a whole-canvas clip on lane 1:
/// both show, the top one in its quarter, and a press takes hold of the
/// picture under it, the top one first.
#[test]
fn a_picture_in_picture_shows_over_the_lane_under_it() {
    let footage = Footage::new();
    let path = footage.path();
    let mut ui = Ui::headless();
    ui.editor.spawn_import(vec![path.clone().into()]);
    let mut rt = window(ui);
    until(&mut rt, "the import", |ui| {
        ui.editor.asset(&path).is_some() && !ui.editor.importing()
    });
    let editor = &mut rt.state.editor;
    editor.timeline.lanes.video = 2;
    let base = editor
        .timeline
        .add_media_clip("Base", ClipKind::Video, 1, 0.0, 2.0, 0.0, &path);
    let inset = editor
        .timeline
        .add_media_clip("Inset", ClipKind::Video, 0, 0.0, 2.0, 0.0, &path);
    editor.edit_clip_transform(inset, |t| {
        t.scale = 0.5;
        t.x = 0.25;
        t.y = -0.25;
    });
    editor.playhead = 0.5;
    until(&mut rt, "both pictures", |ui| {
        ui.editor.program_layers().len() == 2 && ui.editor.preview_is_current()
    });
    // The second video lane grows the timeline panel; let the monitor
    // finish shrinking.
    frames(&mut rt, 60);

    let share = |rt: &Runtime<Ui>, key: &str| {
        let ([fx, fy, fw, fh], _) = rt.presented("monitor").unwrap();
        let ([x, y, w, h], _) = rt.presented(key).unwrap();
        [(x - fx) / fw, (y - fy) / fh, w / fw, h / fh]
    };
    let close =
        |got: [f32; 4], want: [f32; 4]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.01);
    let under = share(&rt, "monitor-lane1-placed");
    let over = share(&rt, "monitor-lane0-placed");
    assert!(close(under, [0.0, 0.0, 1.0, 1.0]), "{under:?}");
    assert!(close(over, [0.5, 0.0, 0.5, 0.5]), "{over:?}");

    let editor = &rt.state.editor;
    assert_eq!(editor.picture_at((240.0, 45.0)), Some(inset));
    assert_eq!(editor.picture_at((80.0, 135.0)), Some(base));
}

#[test]
fn a_title_previews_where_its_look_puts_it_and_a_drag_moves_it() {
    let mut ui = Ui::headless();
    let title = ui
        .editor
        .timeline
        .add_clip("Drag me", ClipKind::Title, 0, 0.0, 4.0, 0.0);
    ui.editor.playhead = 1.0;
    let mut rt = window(ui);
    frames(&mut rt, 2);
    let (_, canvas_h) = rt.state.editor.canvas();
    let canvas_h = canvas_h as f32;
    let share = |rt: &Runtime<Ui>| {
        let ([fx, fy, fw, fh], _) = rt.presented("monitor").unwrap();
        let ([x, y, w, h], _) = rt.presented("monitor-title").unwrap();
        [(x - fx) / fw, (y - fy) / fh, w / fw, h / fh]
    };
    // A lower third's box: 42 px in and its text top 110 px up the canvas,
    // with 14 px of box round the text.
    let home = share(&rt);
    let canvas_w = rt.state.editor.canvas().0 as f32;
    assert!((home[0] - 28.0 / canvas_w).abs() < 0.005, "{home:?}");
    assert!(
        (home[1] - (canvas_h - 124.0) / canvas_h).abs() < 0.005,
        "{home:?}"
    );

    // Twice the size is twice as tall and grows upward: its lower edge
    // moves only by the doubled box padding and line spacing (about 23
    // canvas pixels), not by the text's height (67).
    rt.state.editor.edit_title_text(title, |t| t.size = 2.0);
    frames(&mut rt, 60);
    let large = share(&rt);
    assert!(
        (large[3] / home[3] - 2.0).abs() < 0.1,
        "{large:?} against {home:?}"
    );
    let lower = (large[1] + large[3] - home[1] - home[3]) * canvas_h;
    assert!((0.0..30.0).contains(&lower), "{lower} px lower");
    rt.state.editor.edit_title_text(title, |t| t.size = 1.0);
    frames(&mut rt, 60);

    // A drag a tenth of the monitor right and down moves it a tenth of the
    // canvas, and selects it.
    let ([_, _, fw, fh], _) = rt.presented("monitor").unwrap();
    let ([x, y, w, h], _) = rt.presented("monitor-title").unwrap();
    let (x, y) = (x + w / 2.0, y + h / 2.0);
    crate::testing::input_arrives();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.frame(FRAME);
    rt.pointer_move(x + fw * 0.1, y + fh * 0.1);
    rt.frame(FRAME);
    rt.pointer_up(x + fw * 0.1, y + fh * 0.1);
    frames(&mut rt, 60);
    let text = rt.state.editor.timeline.clip(title).unwrap().text;
    assert!(
        (text.x - 0.1).abs() < 0.005 && (text.y - 0.1).abs() < 0.005,
        "{text:?}"
    );
    assert_eq!(rt.state.editor.selected, Some(title));
    let moved = share(&rt);
    assert!((moved[0] - home[0] - 0.1).abs() < 0.005, "{moved:?}");
    assert!((moved[1] - home[1] - 0.1).abs() < 0.005, "{moved:?}");

    // Esc during a drag puts it back where the drag started.
    let (x, y) = (x + fw * 0.1, y + fh * 0.1);
    rt.pointer_down(x, y);
    rt.frame(FRAME);
    rt.pointer_move(x - fw * 0.3, y);
    rt.frame(FRAME);
    assert!(rt.state.editor.timeline.clip(title).unwrap().text.x < 0.0);
    press(&mut rt, Key::Escape);
    rt.pointer_up(x - fw * 0.3, y);
    frames(&mut rt, 2);
    assert_eq!(rt.state.editor.timeline.clip(title).unwrap().text, text);
}

/// Halfway through a wipe to the left the incoming picture shows right of
/// the edge xfade puts at `int(320 * 0.5) + 1` = 161 of the 320 canvas
/// pixels and the outgoing one left of it; past the wipe one picture shows
/// alone.
#[test]
fn a_transition_previews_both_pictures_where_export_puts_them() {
    use openreel_engine::{Transition, TransitionKind};
    let footage = Footage::new();
    let path = footage.path();
    let mut ui = Ui::headless();
    ui.editor.spawn_import(vec![path.clone().into()]);
    let mut rt = window(ui);
    until(&mut rt, "the import", |ui| {
        ui.editor.asset(&path).is_some() && !ui.editor.importing()
    });
    let timeline = &mut rt.state.editor.timeline;
    timeline.add_media_clip("Out", ClipKind::Video, 0, 0.0, 1.0, 0.0, &path);
    let next = timeline.add_media_clip("In", ClipKind::Video, 0, 1.0, 1.0, 0.0, &path);
    timeline.clip_mut(next).unwrap().transition =
        Some(Transition::new(TransitionKind::WipeLeft, 0.6));
    rt.state.editor.seek(1.0);
    until(&mut rt, "both pictures", |ui| {
        ui.editor.transition_shown().is_some()
    });

    let share = |rt: &Runtime<Ui>, key: &str| {
        let ([fx, fy, fw, fh], _) = rt.presented("monitor").unwrap();
        let ([x, y, w, h], _) = rt.presented(key).unwrap();
        [(x - fx) / fw, (y - fy) / fh, w / fw, h / fh]
    };
    let close =
        |got: [f32; 4], want: [f32; 4]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.002);
    let from = share(&rt, "monitor-lane0-from");
    let to = share(&rt, "monitor-lane0-to");
    assert!(close(from, [0.0, 0.0, 161.0 / 320.0, 1.0]), "{from:?}");
    assert!(
        close(to, [161.0 / 320.0, 0.0, 159.0 / 320.0, 1.0]),
        "{to:?}"
    );

    rt.state.editor.seek(1.5);
    until(&mut rt, "the next picture", |ui| {
        ui.editor.preview_is_current()
    });
    assert!(!shown(&rt, "monitor-lane0-transition"));
    assert!(shown(&rt, "monitor-lane0-placed"));
}

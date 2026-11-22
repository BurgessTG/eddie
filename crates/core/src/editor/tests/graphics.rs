//! Shapes and stickers: pictures the editor draws, placed over the video,
//! kept out of the bin and drawn again when a project opens elsewhere.

use openreel_engine::{Graphic, Rgb, ShapeKind};

use super::*;
use crate::editor::graphics::GRAPHIC_SCALE;

fn video(path: &str) -> BinAsset {
    BinAsset {
        info: MediaInfo {
            path: path.to_owned(),
            name: path.to_owned(),
            duration: 10.0,
            width: 320,
            height: 180,
            fps: 30.0,
            has_video: true,
            has_audio: false,
            photo: false,
            alpha: false,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks: Vec::new(),
        beats: None,
        thumbs: Vec::new(),
        sweep: sweep::SweepData::default(),
    }
}

fn run(app: &mut Editor, code: &str) -> Result<String, String> {
    let mut engine = ScriptEngine::new()?;
    let (output, commands) = engine.eval_checked(code, &app.script_state_json())?;
    app.apply_edit(commands)?;
    Ok(output)
}

/// An editor with a 10 s video on lane 0 that keeps its drawings in
/// `dir`.
fn editor_with_video(dir: &Path) -> Editor {
    let mut app = Editor::headless();
    app.graphics_dir = Some(dir.to_path_buf());
    app.assets.push(video("a.mp4"));
    app.timeline
        .add_media_clip("a.mp4", ClipKind::Video, 0, 0.0, 10.0, 0.0, "a.mp4");
    app.commit_history();
    app
}

fn graphic_clips(app: &Editor) -> Vec<&Clip> {
    app.timeline
        .clips
        .iter()
        .filter(|clip| clip.graphic.is_some())
        .collect()
}

#[test]
fn shapes_and_stickers_are_drawn_over_the_video_and_kept_out_of_the_bin() {
    let dir = std::env::temp_dir().join(format!("eddie-graphics-{}", std::process::id()));
    let mut app = editor_with_video(&dir);
    let output = run(
        &mut app,
        "const s = addShape({kind: 'heart', fill: '#FF3B30'}, 2, 4);
         const t = addSticker('Fire', 1);
         setShape(s, {kind: 'star', outline: '#FFFFFF'});
         return [stickers().length, stickers().includes('thumbs-up')];",
    )
    .unwrap();
    assert_eq!(output, "[53,true]");

    let video_lane = app
        .timeline
        .clips
        .iter()
        .find(|clip| clip.graphic.is_none())
        .unwrap()
        .track;
    let graphics = graphic_clips(&app);
    assert_eq!(graphics.len(), 2);
    let star = graphics.iter().find(|clip| clip.name == "Star").unwrap();
    let fire = graphics.iter().find(|clip| clip.name == "Fire").unwrap();
    assert_eq!((fire.start, fire.duration), (1.0, 3.0));
    assert_eq!((star.start, star.duration), (2.0, 4.0));
    let Some(Graphic::Shape(shape)) = &star.graphic else {
        panic!("not a shape: {:?}", star.graphic)
    };
    // A new kind brings its own proportions; the colours chosen stay.
    assert_eq!(shape.kind, ShapeKind::Star);
    assert_eq!(shape.ratio, ShapeKind::Star.natural_ratio());
    assert_eq!(
        (shape.fill, shape.outline),
        (Some(Rgb(255, 59, 48)), Some(Rgb::WHITE))
    );
    for clip in &graphics {
        assert!(clip.kind == ClipKind::Video && clip.photo, "{}", clip.name);
        assert_eq!(clip.transform.scale, GRAPHIC_SCALE);
        assert!(clip.track < video_lane, "{} is under the video", clip.name);
        let source = clip.source.as_deref().unwrap();
        assert!(Path::new(source).starts_with(&dir) && Path::new(source).is_file());
        assert!(app.asset(source).is_some_and(|asset| asset.info.alpha));
    }
    // The star's file is its new drawing, not the heart's.
    assert!(star.source.as_deref().unwrap().contains("star-"));

    // Nothing joins the bin, and the canvas stays the video's.
    let state: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
    assert_eq!(state["assets"].as_array().unwrap().len(), 1);
    assert_eq!(app.canvas(), (320, 180));
    let findings = run(&mut app, "return inspect().map((f) => f.kind);").unwrap();
    assert!(!findings.contains("missing-media"), "{findings}");

    // Drawing is left out of the edit's time budget (three new drawings
    // took over 250 ms above in a debug build), so the number of new ones
    // is bounded instead; the edit is undone whole.
    let before = app.timeline.clips.len();
    let many = "for (let i = 0; i < 17; i++) addShape({fill: '#0000' + (16 + i).toString(16)}, 0);";
    let refused = run(&mut app, many).unwrap_err();
    assert!(refused.contains("at most 16 new"), "{refused}");
    assert_eq!(app.timeline.clips.len(), before);
    for (code, error) in [
        ("addSticker('nope')", "stickers() lists"),
        ("addShape({fill: null})", "needs a fill or an outline"),
        ("addShape('hexagon')", "kind is one of"),
        ("setShape(1, {fill: '#000000'})", "needs a shape"),
    ] {
        let refused = run(&mut app, code).unwrap_err();
        assert!(refused.contains(error), "{code}: {refused}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_saved_graphic_is_drawn_again_where_its_file_is_gone() {
    let base = std::env::temp_dir().join(format!("eddie-graphics-open-{}", std::process::id()));
    let mut app = editor_with_video(&base.join("here"));
    run(&mut app, "addSticker('rocket', 0); addShape('arrow', 4);").unwrap();
    let path = base.join("cut.eddie");
    app.save_project_to(&path).unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["media"], serde_json::json!(["a.mp4"]));

    // Opened where the drawings were never made, as on another computer.
    let elsewhere = base.join("elsewhere");
    let mut reopened = Editor::headless();
    reopened.graphics_dir = Some(elsewhere.clone());
    reopened.open_project(&path).unwrap();
    let graphics = graphic_clips(&reopened);
    assert_eq!(graphics.len(), 2);
    for clip in graphics {
        let source = clip.source.as_deref().unwrap();
        assert!(
            Path::new(source).starts_with(&elsewhere) && Path::new(source).is_file(),
            "{source}"
        );
        assert!(reopened.asset(source).is_some());
    }
    assert!(reopened
        .assets
        .iter()
        .all(|asset| asset.info.path == "a.mp4"));
    assert!(!reopened.has_unsaved_changes());
    let _ = std::fs::remove_dir_all(base);
}

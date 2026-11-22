//! Clips played backwards: a script or Reverse writes the clip's stretch
//! of its file backwards, the clip and its linked sound play that file from
//! the other end, and a project whose file is gone makes it again.

use std::time::{Duration, Instant};

use openreel_media::VideoDecoder;

use super::*;
use crate::editor::commands::{self, Note, Reply};

/// Three seconds at 30 fps whose frame n has luma 16 + 3n, with sound.
fn ramp(dir: &Path) -> PathBuf {
    let path = dir.join("ramp.mov");
    let status = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("color=c=black:s=64x48:r=30:d=3,format=yuv420p,geq=lum='16+3*N':cb=128:cr=128")
        .args(["-f", "lavfi", "-i", "sine=f=440:r=48000:d=3"])
        .args(["-c:v", "libx264", "-qp", "0", "-c:a", "pcm_s16le"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    path
}

/// Run frames until nothing imports or reverses.
fn settle(app: &mut Editor) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        app.begin_frame(0.0);
        app.end_frame(false, false);
        if !app.importing() && !app.reversing() {
            return;
        }
        assert!(Instant::now() < deadline, "{}", app.status);
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `run` as the command line and live server ask for it, waiting for any
/// reversed file it needs.
fn run(app: &mut Editor, code: &str) -> serde_json::Value {
    let params = serde_json::json!({ "code": code });
    let mut job = match commands::start(app, "run", &params).unwrap() {
        Reply::Done(outcome) => return outcome.value,
        Reply::Later(job) => job,
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        app.begin_frame(0.0);
        let mut notes: Vec<Note> = Vec::new();
        let answer = job.poll(app, &mut notes);
        app.end_frame(false, false);
        if let Some(answer) = answer {
            return answer.unwrap().value;
        }
        assert!(Instant::now() < deadline, "{code}: {}", app.status);
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The mean of the red channel of the picture `path` shows at `time`.
fn shade(path: &str, time: f64) -> f64 {
    let frame = VideoDecoder::open(path, 64, false)
        .unwrap()
        .frame_at(time)
        .unwrap();
    let reds: Vec<f64> = frame.rgba.chunks(4).map(|p| f64::from(p[0])).collect();
    reds.iter().sum::<f64>() / reds.len() as f64
}

#[test]
fn a_reversed_clip_and_its_sound_play_their_stretch_backwards_and_back_again() {
    let dir = std::env::temp_dir().join(format!("eddie-reverse-clip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let original = ramp(&dir);
    let original = original.to_string_lossy().into_owned();
    let made = dir.join("reversed");
    let mut app = Editor::headless();
    app.reversed_dir = Some(made.clone());
    app.spawn_import(vec![PathBuf::from(&original)]);
    settle(&mut app);

    // Source 0.5..1.5 s, its sound detached into a linked clip.
    let added = run(
        &mut app,
        "const v = addAssetRange('ramp.mov', 0, 0.5, 1); detachAudio(v); return v",
    );
    let picture = added["handles"][added["output"].as_str().unwrap()]
        .as_u64()
        .unwrap();
    let sound = app.timeline.clip(picture).unwrap().linked_clip.unwrap();
    let reversed = run(&mut app, &format!("setReverse({picture}, true)"));
    assert_eq!(reversed["changed"], true, "{reversed}");

    // The file holds 0..2.5 s (the clip, and a second's handle each side
    // where the file has it), so the clip starts 1 s into it.
    let clip = app.timeline.clip(picture).unwrap().clone();
    let spec = clip.reversed.clone().unwrap();
    assert_eq!(
        (spec.original.as_str(), spec.start, spec.end),
        (original.as_str(), 0.0, 2.5)
    );
    let file = clip.source.clone().unwrap();
    assert!(file.starts_with(&*made.to_string_lossy()), "{file}");
    assert!((clip.source_in - 1.0).abs() < 1e-9, "{}", clip.source_in);
    let partner = app.timeline.clip(sound).unwrap();
    assert_eq!(partner.reversed, clip.reversed);
    assert_eq!(partner.source, clip.source);
    assert_eq!(partner.source_in, clip.source_in);
    // Kept out of the bin and the project's media.
    assert_eq!(app.assets.len(), 1);
    assert!(app.made.iter().any(|asset| asset.info.path == file));

    // At clip time t the picture is the original's at 1.5 - t: frame
    // 44 - k in the middle of the clip's frame k.
    for k in [0, 7, 29] {
        let t = (k as f64 + 0.5) / 30.0;
        let shown = shade(&file, clip.source_at(clip.start + t));
        let wanted = shade(&original, (44 - k) as f64 / 30.0 + 0.5 / 30.0);
        assert!(
            (shown - wanted).abs() < 1.5,
            "frame {k}: {shown} against {wanted}"
        );
    }

    // `frames` reads the clip's range in the original, as it plays it.
    let render = crate::editor::pictures::frames(
        &app,
        "ramp.mov",
        &format!("clip:{picture}"),
        dir.join("frames"),
        None,
        None,
    )
    .unwrap();
    let pictured = render().unwrap();
    assert_eq!(
        (pictured["start"].as_f64(), pictured["end"].as_f64()),
        (Some(0.5), Some(1.5))
    );

    // Forwards again: the clip is as it was.
    run(&mut app, &format!("setReverse({sound}, false)"));
    for id in [picture, sound] {
        let clip = app.timeline.clip(id).unwrap();
        assert!(clip.reversed.is_none());
        assert_eq!(clip.source.as_deref(), Some(original.as_str()));
        assert!((clip.source_in - 0.5).abs() < 1e-9, "{}", clip.source_in);
    }

    // Reversed again from the file already made, saved, and the file
    // deleted: the project makes it again when it opens.
    let again = run(&mut app, &format!("setReverse({picture}, true)"));
    assert_eq!(again["changed"], true, "{again}");
    let project = dir.join("p.eddie");
    app.save_project_to(&project).unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&project).unwrap()).unwrap();
    assert_eq!(saved["media"], serde_json::json!([original]));
    std::fs::remove_file(&file).unwrap();
    let mut reopened = Editor::headless();
    reopened.reversed_dir = Some(made.clone());
    reopened.open_project(&project).unwrap();
    settle(&mut reopened);
    let clip = reopened.timeline.clip(picture).unwrap();
    assert_eq!(clip.source.as_deref(), Some(file.as_str()));
    assert!(Path::new(&file).exists(), "{}", reopened.status);
    assert!(reopened.made.iter().any(|asset| asset.info.path == file));
    assert!(reopened.missing_media.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn photos_titles_and_shapes_have_no_direction() {
    let mut app = Editor::headless();
    let title = run(&mut app, "return addTitle('Hello', 0, 2)");
    let id = title["handles"][title["output"].as_str().unwrap()]
        .as_u64()
        .unwrap();
    let params = serde_json::json!({ "code": format!("setReverse({id}, true)") });
    let refused = commands::start(&mut app, "run", &params);
    assert!(refused.is_err(), "a title reversed");
    assert!(app.timeline.clip(id).unwrap().reversed.is_none());
}

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use openreel_engine::{Remade, Stabilize};

use super::*;

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("eddie-remake-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Two seconds at 30 fps whose frame n has luma 16 + 3n (3 levels apart), over a mono ramp
/// whose sample at time t is t / 4.
fn ramp(dir: &Path) -> String {
    let path = dir.join("ramp.mov");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("color=c=black:s=64x48:r=30:d=2,format=yuv420p,geq=lum='16+3*N':cb=128:cr=128")
        .args(["-f", "lavfi", "-i", "aevalsrc='t/4':s=48000:d=2"])
        .args(["-c:v", "libx264", "-qp", "0", "-c:a", "pcm_s16le"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    path.to_string_lossy().into_owned()
}

/// Two seconds at 30 fps of a still, detailed picture seen by a shaking
/// camera: each frame is a 640x360 window moved by up to 36 pixels across
/// and 24 down, over the ramp's sound.
fn shaky(dir: &Path) -> String {
    let path = dir.join("shaky.mov");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(
            "mandelbrot=s=800x450:start_scale=0.5:end_scale=0.5:outer=normalized_iteration_count,\
             trim=end_frame=1,loop=loop=-1:size=1,setpts=N/30/TB,\
             crop=640:360:x='80+24*sin(t*7.1)+12*sin(t*13.3+1)':y='45+16*sin(t*8.3+2)+8*sin(t*15.7)',\
             format=yuv420p",
        )
        .args(["-f", "lavfi", "-i", "aevalsrc='t/4':s=48000:d=2"])
        .args(["-t", "2", "-r", "30"])
        .args(["-c:v", "libx264", "-qp", "0", "-c:a", "pcm_s16le"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    path.to_string_lossy().into_owned()
}

/// Every picture as grey levels, `width` by `height`.
fn greys(path: &Path, (width, height): (usize, usize)) -> Vec<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
        .chunks(width * height)
        .map(<[u8]>::to_vec)
        .collect()
}

/// Every picture's mean luma, in order.
fn lumas(path: &Path) -> Vec<f64> {
    greys(path, (64, 48))
        .iter()
        .map(|frame| frame.iter().map(|&v| f64::from(v)).sum::<f64>() / frame.len() as f64)
        .collect()
}

/// How much the middle half of a 640x360 picture changes from frame to
/// frame, in grey levels: a still scene filmed from a tripod changes
/// little, a shaking camera a lot.
fn shake(path: &Path) -> f64 {
    let frames = greys(path, (640, 360));
    let mut sum = 0.0;
    let mut count = 0.0;
    for pair in frames.windows(2) {
        for y in 90..270 {
            for x in 160..480 {
                let at = y * 640 + x;
                sum += (f64::from(pair[0][at]) - f64::from(pair[1][at])).abs();
                count += 1.0;
            }
        }
    }
    sum / count
}

/// The sound as mono floats at 48 kHz.
fn sound(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map", "0:a:0", "-f", "f32le", "-ac", "1", "-ar", "48000", "-",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| f32::from_le_bytes(b))
        .collect()
}

#[test]
fn a_range_snaps_out_to_whole_frames() {
    assert_eq!(frame_range(30.0, 0.51, 1.49), (0.5, 1.5));
    assert_eq!(frame_range(30.0, 0.5, 1.5), (0.5, 1.5));
    // An empty range still holds one frame.
    assert_eq!(frame_range(25.0, 0.4, 0.4), (0.4, 0.44));
}

fn spec(original: &str, start: f64, end: f64) -> Remade {
    Remade {
        original: original.to_owned(),
        start,
        end,
        reverse: false,
        stabilize: None,
    }
}

/// Small sizes, so a small file reaches the parts that matter on a large
/// one: reversed chunks of 7 frames, motion measured on a copy half the
/// size of a 640x360 picture.
const SMALL: Limits = Limits {
    chunk_frames: 7,
    detect_side: 320,
};

#[test]
fn a_reversed_range_plays_its_frames_and_sound_last_first() {
    let dir = folder("range");
    let source = ramp(&dir);
    let output = dir.join("reversed.mov");
    let mut shares = Vec::new();
    let cancel = AtomicBool::new(false);
    let backwards = Remade {
        reverse: true,
        ..spec(&source, 0.5, 1.5)
    };
    remake_with(
        &backwards,
        &output,
        &mut |share| shares.push(share),
        &cancel,
        SMALL,
    )
    .unwrap();
    assert_eq!(shares.last(), Some(&1.0));

    let info = crate::probe(&output.to_string_lossy()).unwrap();
    assert_eq!((info.width, info.height), (64, 48));
    assert!((info.fps - 30.0).abs() < 1e-6, "fps {}", info.fps);
    assert!(
        (info.duration - 1.0).abs() < 0.01,
        "duration {}",
        info.duration
    );

    // Frames 15..=44, last first: frame j shows source frame 44 - j.
    let original = lumas(Path::new(&source));
    let reversed = lumas(&output);
    assert_eq!(reversed.len(), 30);
    for (j, luma) in reversed.iter().enumerate() {
        let wanted = original[44 - j];
        // Neighbouring frames are about 3.5 levels apart once decoded.
        assert!(
            (luma - wanted).abs() < 1.5,
            "frame {j}: {luma} against {wanted}"
        );
    }

    // Sound at reversed time r is the source's at 1.5 - r.
    let samples = sound(&output);
    assert!(
        (samples.len() as i64 - 48_000).abs() <= 1,
        "{} samples",
        samples.len()
    );
    for r in [0.05, 0.5, 0.95] {
        let heard = samples[(r * 48_000.0) as usize];
        let wanted = (1.5 - r) / 4.0;
        assert!(
            (f64::from(heard) - wanted).abs() < 0.002,
            "at {r}: {heard} against {wanted}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_cancelled_reversal_leaves_nothing_behind() {
    let dir = folder("cancel");
    let source = ramp(&dir);
    let output = dir.join("reversed.mov");
    let cancel = AtomicBool::new(true);
    let backwards = Remade {
        reverse: true,
        ..spec(&source, 0.0, 2.0)
    };
    let result = remake(&backwards, &output, &mut |_| {}, &cancel);
    assert!(result.is_err());
    let left: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["ramp.mov"]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn measured_motion_scales_up_to_the_whole_picture() {
    let measured = "VID.STAB 1\n# accuracy = 15\nFrame 1 (List 0 [])\n\
        Frame 2 (List 2 [(LM 7 -3 340 864 112 0.557601 0.602360),(LM 0 2 960 540 32 0.2 1.5)])\n";
    let scaled = stabilize::scale_motions(measured, 2.0, 1.5).unwrap();
    assert_eq!(
        scaled,
        "VID.STAB 1\n# accuracy = 15\nFrame 1 (List 0 [])\n\
         Frame 2 (List 2 [(LM 14 -5 680 1296 168 0.557601 0.602360),(LM 0 3 1920 810 48 0.2 1.5)])\n"
    );
    assert!(stabilize::scale_motions("(LM 1 2 3)", 2.0, 2.0).is_err());
}

#[test]
fn a_stabilized_range_holds_a_shaking_camera_still_and_keeps_its_sound() {
    let dir = folder("steady");
    let source = shaky(&dir);
    let cancel = AtomicBool::new(false);
    let before = shake(Path::new(&source));
    // The whole file, so the window's own motion is the only shake. Motion
    // is measured at half size, so it must be scaled up to steady this.
    let mut levels = Vec::new();
    for level in [Stabilize::MinimalCrop, Stabilize::MostStable] {
        let output = dir.join(format!("{}.mov", level.name()));
        let steady = Remade {
            stabilize: Some(level),
            ..spec(&source, 0.0, 2.0)
        };
        remake_with(&steady, &output, &mut |_| {}, &cancel, SMALL).unwrap();
        let info = crate::probe(&output.to_string_lossy()).unwrap();
        assert_eq!((info.width, info.height), (640, 360));
        assert_eq!(greys(&output, (640, 360)).len(), 60);
        levels.push(shake(&output));

        // The sound plays forwards, as it was.
        let samples = sound(&output);
        for t in [0.1, 1.0, 1.9] {
            let heard = f64::from(samples[(t * 48_000.0) as usize]);
            assert!((heard - t / 4.0).abs() < 0.002, "at {t}: {heard}");
        }
    }
    // 3.0 levels against 8.8 before; measured unscaled, the motion is half
    // what it was and 7.0 stayed.
    assert!(
        levels[1] < before * 0.5,
        "most stable: {} against {before} before",
        levels[1]
    );
    assert!(
        levels[1] < levels[0] && levels[0] < before,
        "minimal crop {} and most stable {} against {before}",
        levels[0],
        levels[1]
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Every byte of a yuv420p picture `width` by `height`, frame by frame.
fn yuv_frames(path: &Path, filters: &str, (width, height): (usize, usize)) -> Vec<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-vf", filters])
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .chunks(width * height * 3 / 2)
        .map(<[u8]>::to_vec)
        .collect()
}

#[test]
fn frames_move_onto_the_camera_path_as_vidstabtransform_moves_them() {
    let dir = folder("warp");
    // A shaking camera that also turns, so rotation and the edges count.
    let source = dir.join("turning.mov");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(
            "mandelbrot=s=800x450:start_scale=0.5:end_scale=0.5:outer=normalized_iteration_count,\
             trim=end_frame=1,loop=loop=-1:size=1,setpts=N/30/TB,\
             rotate='0.04*sin(t*9.3)':c=gray,\
             crop=640:360:x='80+24*sin(t*7.1)':y='45+16*sin(t*8.3+2)',format=yuv420p",
        )
        .args(["-t", "2", "-r", "30", "-c:v", "libx264", "-qp", "0"])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    let motions = dir.join("motions.trf");
    let detect = format!(
        "vidstabdetect=shakiness=6:fileformat=ascii:result={}",
        motions.display()
    );
    assert!(yuv_frames(&source, &detect, (640, 360)).len() == 60);
    // Tight limits, so shifts and turns are clipped too.
    let options = "smoothing=15:optzoom=1:maxshift=20:maxangle=0.02";
    let transform = format!("vidstabtransform=input={}:{options}", motions.display());
    let wanted = yuv_frames(&source, &transform, (640, 360));

    let picture = Picture {
        width: 640,
        height: 360,
        rate: (30, 1),
    };
    let path = warp::camera_path(
        &stabilize::frame_motions(picture, &motions, options).unwrap(),
        &warp::PathOptions {
            smoothing: 15,
            max_shift: 20,
            max_angle: 0.02,
            width: 640,
            height: 360,
        },
    );
    assert!(path
        .iter()
        .any(|moved| moved.alpha.abs() == 0.02 && moved.x.abs() <= 20.0));
    let mut kept: Option<Vec<u8>> = None;
    let (mut differ, mut worst, mut total) = (0, 0, 0);
    for (index, (frame, wanted)) in yuv_frames(&source, "null", (640, 360))
        .iter()
        .zip(&wanted)
        .enumerate()
    {
        let kept = kept.get_or_insert_with(|| frame.clone());
        warp::warp(frame, kept, (640, 360), path[index.min(path.len() - 1)], 3);
        for (&a, &b) in kept.iter().zip(wanted) {
            let gap = a.abs_diff(b);
            differ += usize::from(gap > 0);
            worst = worst.max(gap);
        }
        total += kept.len();
    }
    assert_eq!(wanted.len(), 60);
    assert!(
        worst <= 1 && differ * 1000 < total,
        "{differ} of {total} bytes differ, worst {worst}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_range_stabilized_and_reversed_plays_its_frames_last_first() {
    let dir = folder("both");
    let source = ramp(&dir);
    let output = dir.join("both.mov");
    let cancel = AtomicBool::new(false);
    let both = Remade {
        reverse: true,
        stabilize: Some(Stabilize::Recommended),
        ..spec(&source, 0.5, 1.5)
    };
    remake_with(&both, &output, &mut |_| {}, &cancel, SMALL).unwrap();
    let original = lumas(Path::new(&source));
    let made = lumas(&output);
    assert_eq!(made.len(), 30);
    for (j, luma) in made.iter().enumerate() {
        let wanted = original[44 - j];
        assert!(
            (luma - wanted).abs() < 1.5,
            "frame {j}: {luma} against {wanted}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

/// A proxy shows, at every source time, the picture the original shows
/// there: the same frame, upright and the preview's width. The original is
/// a phone-style file, turned a quarter by its display matrix, whose
/// picture starts 0.1 s after its sound (the copy once moved its first
/// picture to 0), and each of its frames has its own brightness.
#[test]
fn a_proxy_shows_each_picture_at_the_originals_time() {
    let dir = folder("proxy");
    let picture = dir.join("picture.mov");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("color=c=black:s=1280x720:r=30:d=2,format=yuv420p,geq=lum='16+3*N':cb=128:cr=128")
        .args(["-c:v", "libx264", "-qp", "0"])
        .arg(&picture)
        .status()
        .unwrap();
    assert!(status.success());
    let original = dir.join("phone.mov");
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-display_rotation",
            "90",
            "-itsoffset",
            "0.1",
            "-i",
        ])
        .arg(&picture)
        .args(["-f", "lavfi", "-i", "sine=f=440:r=48000:d=2.2"])
        .args([
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "copy",
            "-c:a",
            "pcm_s16le",
        ])
        .arg(&original)
        .status()
        .unwrap();
    assert!(status.success());
    let original = original.to_string_lossy().into_owned();
    let proxy = dir.join("proxy.mov");
    let cancel = AtomicBool::new(false);
    let mut shares = Vec::new();
    make_proxy(
        &original,
        320,
        &proxy,
        &mut |share| shares.push(share),
        &cancel,
    )
    .unwrap();
    assert_eq!(shares.last(), Some(&1.0));
    let info = crate::probe(&proxy.to_string_lossy()).unwrap();
    assert_eq!(
        (info.width, info.height),
        (320, crate::scaled_height(720, 1280, 320))
    );
    let shade = |path: &str, time: f64| {
        let frame = crate::VideoDecoder::open(path, 64, false)
            .unwrap()
            .frame_at(time)
            .unwrap();
        let reds: Vec<f64> = frame.rgba.chunks(4).map(|p| f64::from(p[0])).collect();
        reds.iter().sum::<f64>() / reds.len() as f64
    };
    let proxy = proxy.to_string_lossy().into_owned();
    // Before the first picture, on it, inside frames across the file, and
    // past the last.
    let mut times = vec![0.02, 0.1];
    times.extend(
        (0..60)
            .step_by(7)
            .map(|n| 0.1 + (f64::from(n) + 0.5) / 30.0),
    );
    times.push(2.15);
    for time in times {
        let (want, got) = (shade(&original, time), shade(&proxy, time));
        assert!(
            (want - got).abs() < 1.5,
            "at {time}: the proxy shows {got}, the original {want}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

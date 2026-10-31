//! Scene boundaries through the public native-frame analysis path.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use openreel_media::{build_scene_segments, motion_samples, probe};

const WIDTH: usize = 128;
const HEIGHT: usize = 96;
static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    frames: usize,
    fps: usize,
}

impl Fixture {
    fn new(
        size: (usize, usize),
        fps: usize,
        frames: usize,
        pixel: impl Fn(usize, usize, usize) -> [u8; 3],
    ) -> Self {
        let (width, height) = size;
        let dir = std::env::temp_dir().join(format!(
            "openreel_scene_continuity_{}_{}",
            std::process::id(),
            FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("fixture directory");
        let fixture = Self {
            path: dir.join("source.mp4"),
            dir,
            frames,
            fps,
        };
        let mut encoder = Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "rawvideo",
                "-pixel_format",
                "rgb24",
                "-video_size",
                &format!("{width}x{height}"),
                "-framerate",
                &fps.to_string(),
                "-threads:v",
                "1",
                "-i",
                "pipe:0",
                "-an",
                "-c:v",
                "libx264rgb",
                "-preset",
                "ultrafast",
                "-crf",
                "0",
                "-threads:v",
                "1",
                "-filter_threads",
                "1",
            ])
            .arg(&fixture.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("fixture encoder");
        let mut input = encoder.stdin.take().expect("encoder input");
        let mut rgb = vec![0; width * height * 3];
        for frame in 0..frames {
            for y in 0..height {
                for x in 0..width {
                    let offset = (y * width + x) * 3;
                    rgb[offset..offset + 3].copy_from_slice(&pixel(frame, x, y));
                }
            }
            input.write_all(&rgb).expect("encode fixture frame");
        }
        drop(input);
        let output = encoder.wait_with_output().expect("finish fixture");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fixture
    }

    fn assert_scene_frames(&self, expected: &[usize]) {
        let info = probe(self.path.to_str().unwrap()).expect("fixture metadata");
        let samples = motion_samples(self.path.to_str().unwrap(), &info).expect("native samples");
        assert_eq!(samples.len(), self.frames, "analyze every decoded frame");
        for (frame, sample) in samples.iter().enumerate() {
            assert!((sample.time - frame as f64 / self.fps as f64).abs() < 1e-9);
        }
        let scenes = build_scene_segments(info.duration, &samples, &[], 50);
        let actual: Vec<_> = scenes
            .iter()
            .map(|scene| {
                samples
                    .iter()
                    .position(|sample| sample.time == scene.start)
                    .expect("scene starts at exact source PTS")
            })
            .collect();
        assert_eq!(actual, expected, "incoming source frame indices");
        assert_eq!(scenes[0].start, 0.0);
        assert_eq!(scenes.last().unwrap().end, info.duration);
        assert!(scenes.iter().all(|scene| scene.end > scene.start));
        assert!(scenes.windows(2).all(|pair| pair[0].end == pair[1].start));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn texture(x: usize, y: usize) -> [u8; 3] {
    let tile = (x / 8) * 73 ^ (y / 8) * 151;
    let detail = (x * 3 + y * 7) % 19;
    [
        (35 + tile % 91 + detail) as u8,
        (30 + (tile * 3) % 83 + detail) as u8,
        (25 + (tile * 7) % 79 + detail) as u8,
    ]
}

#[test]
fn flashes_and_exposure_steps_preserve_the_shot() {
    let fixture = Fixture::new((WIDTH, HEIGHT), 60, 150, |frame, x, y| {
        let gain = if (80..110).contains(&frame) { 1.6 } else { 1.0 };
        let flash = if frame == 20 || (40..42).contains(&frame) || (60..65).contains(&frame) {
            85.0
        } else {
            0.0
        };
        texture(x, y).map(|channel| (channel as f32 * gain + flash).min(255.0) as u8)
    });
    fixture.assert_scene_frames(&[0]);
}

#[test]
fn local_flash_preserves_the_shot() {
    let fixture = Fixture::new((WIDTH, HEIGHT), 60, 90, |frame, x, y| {
        let flash = (30..32).contains(&frame) && x > 20 && y > 12;
        texture(x, y).map(|channel| {
            if flash {
                channel.saturating_add(100)
            } else {
                channel
            }
        })
    });
    fixture.assert_scene_frames(&[0]);
}

#[test]
fn foreground_leaving_the_image_preserves_the_background() {
    let fixture = Fixture::new((WIDTH, HEIGHT), 60, 90, |frame, x, y| {
        let left = if frame < 30 {
            8 - (frame / 6) as i32
        } else {
            -44 - (frame - 30) as i32 * 3
        };
        let object_x = x as i32 - left;
        if (0..64).contains(&object_x) && (4..92).contains(&y) {
            [15, 180 + ((object_x as usize + y) % 8) as u8 * 8, 30]
        } else {
            texture(x, y)
        }
    });
    fixture.assert_scene_frames(&[0]);
}

#[test]
fn unrelated_one_frame_and_short_bright_shots_keep_both_boundaries() {
    for fps in [30, 60, 120] {
        let fixture = Fixture::new((WIDTH, HEIGHT), fps, fps * 2, |frame, x, y| {
            if frame == fps / 2 || (fps..fps + 3).contains(&frame) {
                texture(WIDTH - x - 1, HEIGHT - y - 1).map(|channel| channel.saturating_add(110))
            } else {
                texture(x, y)
            }
        });
        fixture.assert_scene_frames(&[0, fps / 2, fps / 2 + 1, fps, fps + 3]);
    }
}

#[test]
fn same_palette_layout_cuts_survive_repeated_cadence() {
    for repeats in [1, 2, 3] {
        let fixture = Fixture::new(
            (WIDTH, HEIGHT),
            30 * repeats,
            90 * repeats,
            |frame, x, y| {
                let source_frame = frame / repeats;
                let horizontal = source_frame == 20 || (40..42).contains(&source_frame);
                let red = if horizontal {
                    y < HEIGHT / 2
                } else {
                    x < WIDTH / 2
                };
                let detail = ((x / 4 + y / 4 + source_frame / 4) % 3) as u8 * 10;
                if red {
                    [220 + detail, 25 + detail, 35 + detail]
                } else {
                    [25 + detail, 35 + detail, 220 + detail]
                }
            },
        );
        fixture.assert_scene_frames(&[0, 20 * repeats, 21 * repeats, 40 * repeats, 42 * repeats]);
    }
}

#[test]
fn a_shared_textured_half_does_not_hide_a_layout_cut() {
    let fixture = Fixture::new((WIDTH, HEIGHT), 60, 90, |frame, x, y| {
        let horizontal = frame == 30 || (60..62).contains(&frame);
        let red = if horizontal {
            y < HEIGHT / 2
        } else {
            x < WIDTH / 2
        };
        let detail = ((x / 8 + y / 8) % 4) as u8 * 20;
        if red {
            [160 + detail, 20 + detail, 30 + detail]
        } else {
            [20 + detail, 30 + detail, 160 + detail]
        }
    });
    fixture.assert_scene_frames(&[0, 30, 31, 60, 62]);
}

#[test]
fn foreground_exit_during_a_pan_does_not_split_the_shot() {
    for (repeats, mirrored) in [
        (1, false),
        (1, true),
        (2, false),
        (2, true),
        (3, false),
        (3, true),
    ] {
        let fixture = Fixture::new(
            (WIDTH, HEIGHT),
            30 * repeats,
            60 * repeats,
            |frame, x, y| {
                let x = if mirrored { WIDTH - x - 1 } else { x };
                let source_frame = frame / repeats;
                let left = if source_frame < 30 {
                    0
                } else {
                    -80 - (source_frame - 30) as i32 * 2
                };
                let object_x = x as i32 - left;
                if y >= 36 && (0..128).contains(&object_x) {
                    [30, 165 + ((object_x as usize + y) % 12) as u8, 45]
                } else {
                    let pan = source_frame * 2 + if source_frame >= 30 { 12 } else { 0 };
                    texture(x + pan, y)
                }
            },
        );
        fixture.assert_scene_frames(&[0]);
    }
}

/// Smooth, detail-free shading, like out-of-focus clothing near the lens.
/// Its broad folds drift as the fabric moves.
fn defocused(frame: usize, x: usize, y: usize) -> [u8; 3] {
    let shade = 60.0 + 45.0 * ((x + frame * 14) as f32 / 18.0).sin() + y as f32 / 6.0;
    [shade * 0.6, shade * 0.75, shade * 1.1 + 20.0].map(|channel| channel as u8)
}

#[test]
fn foreground_passing_the_lens_preserves_the_shot() {
    for (fps, step) in [(24, 1), (30, 1), (60, 2)] {
        // The occluder wipes in over three source frames, covers the lens
        // for ten, and wipes out over three. Exposure recovers brighter.
        let fixture = Fixture::new((WIDTH, HEIGHT), fps, fps * 3, |frame, x, y| {
            let frame = frame / step;
            let lead = (frame as i32 - 20) * 48;
            let trail = (frame as i32 - 33) * 48;
            // The out-of-focus edge blends over eight pixels.
            let cover = ((lead - x as i32).min(x as i32 - trail) as f32 / 8.0).clamp(0.0, 1.0);
            let gain = if frame >= 34 { 1.15 } else { 1.0 };
            let occluder = defocused(frame, x, y);
            let mut pixel = texture(x, y);
            for channel in 0..3 {
                pixel[channel] = (pixel[channel] as f32 * gain * (1.0 - cover)
                    + occluder[channel] as f32 * cover) as u8;
            }
            pixel
        });
        fixture.assert_scene_frames(&[0]);
    }
}

#[test]
fn featureless_interval_between_different_views_keeps_both_cuts() {
    let fixture = Fixture::new((WIDTH, HEIGHT), 30, 90, |frame, x, y| match frame {
        0..30 => texture(x, y),
        30..40 => defocused(frame, x, y),
        _ => texture(WIDTH - x - 1, HEIGHT - y - 1).map(|channel| channel.saturating_add(60)),
    });
    fixture.assert_scene_frames(&[0, 30, 40]);
}

#[test]
fn textureless_one_frame_inserts_survive_wide_and_tall_frames() {
    for size in [(128, 24), (128, 8), (64, 128)] {
        let fixture = Fixture::new(size, 60, 60, |frame, _, _| match frame {
            20 => [240, 240, 240],
            40 => [15, 20, 225],
            _ => [225, 20, 15],
        });
        fixture.assert_scene_frames(&[0, 20, 21, 40, 41]);
    }
}

#[test]
fn unsupported_midstream_change_cannot_report_a_complete_scene_inventory() {
    let fixture = Fixture::new((WIDTH, HEIGHT), 30, 30, |_, x, y| texture(x, y));
    let mut stream = Vec::new();
    for size in ["128x96", "256x144"] {
        let output = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=s={size}:r=30:d=1"),
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-threads:v",
                "1",
                "-filter_threads",
                "1",
                "-f",
                "mpegts",
                "pipe:1",
            ])
            .output()
            .expect("generate changing-resolution transport stream");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        stream.extend(output.stdout);
    }
    let path = fixture.dir.join("changing.ts");
    std::fs::write(&path, stream).unwrap();
    let info = probe(path.to_str().unwrap()).unwrap();
    let samples = motion_samples(path.to_str().unwrap(), &info);
    assert!(
        samples.is_err(),
        "partial analysis must not be labeled a full source inventory"
    );
}

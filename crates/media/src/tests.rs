use std::collections::HashMap;
use std::sync::mpsc::channel;

use openreel_engine::{ClipKind, Timeline};

use super::*;

#[test]
fn samples_probe_decode_and_export_end_to_end() {
    if !ffmpeg_available() {
        panic!("ffmpeg is required for the media tests");
    }
    let dir = std::env::temp_dir().join(format!("openreel-media-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = generate_samples(&dir).expect("generate samples");
    let video_path = paths[0].to_string_lossy().into_owned();
    let music_path = paths[1].to_string_lossy().into_owned();

    let video = probe(&video_path).expect("probe video");
    assert!(video.has_video && video.has_audio);
    assert_eq!((video.width, video.height), (1280, 720));
    assert!((19.0..21.0).contains(&video.duration), "{}", video.duration);

    let music = probe(&music_path).expect("probe music");
    assert!(music.has_audio && !music.has_video);
    assert!(music.duration > 10.0, "{}", music.duration);

    let peaks = waveform_peaks(&music_path, music.duration, 20).expect("peaks");
    assert!(peaks.len() > 100);
    assert!(peaks.iter().copied().fold(0.0_f32, f32::max) > 0.2);

    let frame = frame_at(&video_path, video.width, video.height, 2.0, 320, false).expect("frame");
    assert_eq!((frame.width, frame.height), (320, 180));
    assert_eq!(frame.rgba.len(), 320 * 180 * 4);
    let jpeg = frame_jpeg_at(&video_path, 2.0, 320).expect("representative JPEG");
    assert_eq!(&jpeg[..2], &[0xff, 0xd8], "JPEG start marker");
    assert_eq!(&jpeg[jpeg.len() - 2..], &[0xff, 0xd9], "JPEG end marker");
    // The public API falls back to the CLI, so exercise the in-process
    // decoder directly to prove that path really works.
    let in_process =
        crate::decode::frame_at(&video_path, 3.0, 320, true).expect("in-process decoded frame");
    assert_eq!((in_process.width, in_process.height), (320, 180));

    let thumbs =
        thumbnails(&video_path, video.width, video.height, video.duration).expect("thumbs");
    assert!(thumbs.len() >= 5, "{}", thumbs.len());
    // Keyed by each still's own PTS: ascending, inside the source.
    assert!(thumbs.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(thumbs
        .iter()
        .all(|(t, _)| (0.0..video.duration).contains(t)));

    // Realtime playback pieces: the paced frame stream must deliver
    // correctly sized frames, and the audio player must start (silently).
    let stream = crate::FrameStream::spawn(&video_path, 1280, 720, 1.0, 320, 30.0)
        .expect("spawn frame stream");
    let streamed = stream
        .frames
        .recv_timeout(std::time::Duration::from_secs(15))
        .expect("first streamed frame");
    assert_eq!((streamed.width, streamed.height), (320, 180));
    stream.stop();

    // Decode the MP3 in process: one second from source 4 s, placed at
    // timeline 1 s, renders exactly that span and is not silent.
    let mut audio = crate::AudioEngine::manual();
    audio.seek(0.0);
    audio.set_limit(3.0);
    audio.set_running(true);
    audio.set_voices(&[(
        1,
        crate::AudioSegment {
            path: music_path.clone(),
            start: 1.0,
            duration: 1.0,
            source_in: 4.0,
            volume: 1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            duck: Vec::new(),
            clean_voice: false,
            smooth_fades: false,
        },
    )]);
    let mut mix = Vec::new();
    while mix.len() < 3 * crate::AUDIO_RATE as usize * 2 {
        let block = audio.render(4096);
        assert!(!block.is_empty(), "audio clock stopped");
        mix.extend_from_slice(block);
    }
    let rate = crate::AUDIO_RATE as usize;
    let loud = |a: usize, b: usize| mix[a * 2..b * 2].iter().any(|s| s.abs() > 0.01);
    assert!(
        !loud(0, rate) && !loud(2 * rate, 3 * rate),
        "audio outside clip"
    );
    assert!(loud(rate, 2 * rate), "decoded music is silent");
    assert!(audio.take_error().is_none());

    // Chop two segments out of the sample video, lay the MP3 under the
    // whole thing, burn a title — then export and re-probe the result.
    let mut tl = Timeline::empty_project();
    tl.add_media_clip("v1", ClipKind::Video, 1, 0.0, 3.0, 2.0, &video_path);
    tl.add_media_clip("v2", ClipKind::Video, 1, 3.0, 2.0, 10.0, &video_path);
    let music_clip = tl.add_media_clip("music", ClipKind::Audio, 2, 0.0, 5.0, 1.0, &music_path);
    tl.clip_mut(music_clip).unwrap().volume = 0.6;
    tl.add_clip("Chop test", ClipKind::Title, 0, 0.5, 2.0, 0.0);

    let infos: HashMap<String, MediaInfo> =
        [(video_path.clone(), video), (music_path.clone(), music)].into();
    let out = dir.join("export.mp4");
    let (tx, rx) = channel();
    export_timeline(
        &tl,
        &infos,
        &out,
        &crate::ExportOptions {
            normalize_loudness: true,
            ..Default::default()
        },
        tx,
    )
    .expect("export");
    let progress: Vec<f64> = rx.try_iter().collect();
    assert!(progress.iter().any(|p| *p >= 0.99), "{progress:?}");

    let exported = probe(&out.to_string_lossy()).expect("probe export");
    assert!(exported.has_video && exported.has_audio);
    assert!(
        (4.6..5.4).contains(&exported.duration),
        "{}",
        exported.duration
    );
    let _ = std::fs::remove_dir_all(&dir);
}

//! Curation and the gallery's filtered adds.

use super::*;

#[test]
fn curation_waits_for_explicit_curate_action() {
    let mut app = Editor::headless();
    app.assets.push(BinAsset {
        info: MediaInfo {
            path: "/media/not-curated-yet.mp4".to_owned(),
            name: "not-curated-yet.mp4".to_owned(),
            duration: 10.0,
            width: 854,
            height: 480,
            fps: 30.0,
            has_video: true,
            has_audio: true,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks: Vec::new(),
        beats: None,
        thumbs: Vec::new(),
        sweep: sweep::SweepData::default(),
    });

    assert_eq!(
        app.assets[0].sweep.status,
        sweep::SweepStatus::NotStarted,
        "a ready import stays untouched until Curate is pressed"
    );
    assert!(app.sweep_rx.is_none());
    assert!(app.sweep_queue.is_empty());
    assert!(!app.curator.open, "import does not open the curator");

    app.playing = true;
    app.curate_asset("/media/not-curated-yet.mp4".to_owned());
    assert_eq!(app.assets[0].sweep.status, sweep::SweepStatus::Running);
    assert!(app.sweep_rx.is_some(), "explicit Curate starts one worker");
    assert!(!app.playing, "gallery pauses playback");
    assert!(app.curator.open, "curation opens the results gallery");
    assert!(app.timeline.clips.is_empty(), "browsing never adds clips");

    let (tx, rx) = channel();
    app.sweep_rx = Some(rx);
    app.curator.open = false;
    tx.send(SweepMsg::Done {
        path: "/media/not-curated-yet.mp4".to_owned(),
        data: sweep::SweepData {
            status: sweep::SweepStatus::Ready,
            ..Default::default()
        },
    })
    .unwrap();
    app.poll_sweeps();
    assert_eq!(app.assets[0].sweep.status, sweep::SweepStatus::Ready);
    assert!(!app.curator.open, "completion respects a return to Program");
    assert!(app.timeline.clips.is_empty());
    app.open_curator_gallery();
    app.set_playing(true);
    assert!(!app.curator.open, "playback returns to Program");
}

#[test]
fn curate_all_new_overlaps_files_and_finishes_every_one() {
    let dir = std::env::temp_dir().join(format!("eddie-parallel-curation-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = Editor::headless();
    let colors = ["red", "green", "blue", "yellow"];
    for color in colors {
        let path = dir.join(format!("{color}.mp4"));
        let output = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("color={color}:s=64x36:r=10:d=2"))
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-threads", "1"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        app.assets.push(BinAsset {
            info: openreel_media::probe(path.to_str().unwrap()).unwrap(),
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumbs: Vec::new(),
            sweep: sweep::SweepData::default(),
        });
    }

    app.curate_all_new();
    let active = app
        .assets
        .iter()
        .filter(|asset| {
            asset.sweep.status == sweep::SweepStatus::Running
                && !app.sweep_queue.contains(&asset.info.path)
        })
        .count();
    let expected = std::thread::available_parallelism().map_or(1, |n| (n.get() / 4).clamp(1, 3));
    assert_eq!(active, expected, "several files curate at once");
    assert_eq!(app.sweep_queue.len(), colors.len() - expected);

    let deadline = Instant::now() + std::time::Duration::from_secs(120);
    while app.sweep_rx.is_some() || !app.sweep_queue.is_empty() {
        app.poll_sweeps();
        assert!(Instant::now() < deadline, "curation timed out");
        thread::sleep(std::time::Duration::from_millis(20));
    }
    for asset in &app.assets {
        assert_eq!(
            asset.sweep.status,
            sweep::SweepStatus::Ready,
            "{}",
            asset.info.name
        );
        assert!(!asset.sweep.scene_clips.is_empty(), "{}", asset.info.name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn saved_curation_returns_with_the_same_file_only() {
    let dir = std::env::temp_dir().join(format!("eddie-saved-curation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("two-shots.mp4");
    let render = |second: &str| {
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=red:s=64x36:r=10:d=2",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                &format!("color={second}:s=64x36:r=10:d=2"),
            ])
            .args([
                "-filter_complex",
                "[0:v][1:v]concat=n=2:v=1:a=0[v]",
                "-map",
                "[v]",
            ])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-threads", "1"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
    };
    render("blue");
    let media = path.to_string_lossy().into_owned();
    let session = |curate: bool| {
        let mut app = Editor::headless();
        app.curation_dir = Some(dir.join("store"));
        app.spawn_import(vec![path.clone()]);
        let deadline = Instant::now() + std::time::Duration::from_secs(120);
        let mut curated = !curate;
        loop {
            app.poll_imports();
            app.poll_sweeps();
            let imported = app.import_rx.is_none() && app.assets[0].state == AssetState::Ready;
            if imported && !curated {
                app.curate_asset(media.clone());
                curated = true;
            }
            if imported && curated && app.sweep_rx.is_none() {
                break;
            }
            assert!(Instant::now() < deadline, "import or curation timed out");
            thread::sleep(std::time::Duration::from_millis(20));
        }
        let state: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
        (
            app.assets[0].sweep.status,
            state["sweepCandidates"].clone(),
            state["sceneClips"].clone(),
        )
    };

    let (status, candidates, scenes) = session(true);
    assert_eq!(status, sweep::SweepStatus::Ready);
    assert!(scenes.as_array().unwrap().len() >= 2, "two shots: {scenes}");

    let (status, restored_candidates, restored_scenes) = session(false);
    assert_eq!(
        status,
        sweep::SweepStatus::Ready,
        "a reopened file keeps its curation"
    );
    assert_eq!(restored_candidates, candidates);
    assert_eq!(restored_scenes, scenes);

    std::thread::sleep(std::time::Duration::from_millis(20));
    render("green");
    let (status, candidates, _) = session(false);
    assert_eq!(
        status,
        sweep::SweepStatus::NotStarted,
        "a replaced file is not matched"
    );
    assert_eq!(candidates, serde_json::json!([]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn curator_add_shown_uses_the_selected_source_and_filter_but_all_scenes_stays_complete() {
    let mut app = Editor::headless();
    for path in ["/media/a.mp4", "/media/b.mp4"] {
        let candidates: Vec<_> = ["action", "reaction", "reaction", "talking"]
            .into_iter()
            .enumerate()
            .map(|(index, category)| sweep::SweepCandidate {
                id: format!("{path}#clip-{index}"),
                asset_path: path.to_owned(),
                asset_name: path.to_owned(),
                title: category.to_owned(),
                source_in: index as f64 * 2.0,
                duration: 2.0,
                score: 80.0 + index as f32,
                reason: "Fixture range".to_owned(),
                stability: 0.9,
                audio_energy: 0.0,
                audio_peak: 0.0,
                category: category.to_owned(),
                tags: if index == 0 {
                    vec!["prime".to_owned()]
                } else {
                    Vec::new()
                },
                motion_score: 0.3,
                audio_score: 0.0,
                music_bed: false,
                visual: None,
                has_video: true,
                transcript: Vec::new(),
                review: sweep::ClipReview {
                    status: if index == 2 {
                        sweep::ReviewStatus::NeedsReview
                    } else {
                        sweep::ReviewStatus::Reviewed
                    },
                    ..Default::default()
                },
                dismissed: index == 1,
            })
            .collect();
        let mut scenes = candidates.clone();
        scenes[1].review.status = sweep::ReviewStatus::Rejected;
        app.assets.push(BinAsset {
            info: MediaInfo {
                path: path.to_owned(),
                name: path.to_owned(),
                duration: 8.0,
                width: 64,
                height: 36,
                fps: 30.0,
                has_video: true,
                has_audio: false,
            },
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumbs: Vec::new(),
            sweep: sweep::SweepData {
                candidates,
                scene_clips: scenes,
                ..Default::default()
            },
        });
    }
    app.selected_asset = Some("/media/b.mp4".to_owned());
    for (filter, expected) in [("reaction", 4.0), ("needs-review", 4.0), ("prime", 0.0)] {
        app.timeline = Timeline::empty_project();
        app.curator.filters = gallery::GalleryFilters::default();
        match filter {
            "prime" => app.curator.filters.quality = gallery::GalleryQuality::Prime,
            "needs-review" => app.curator.filters.quality = gallery::GalleryQuality::NeedsReview,
            category => app.curator.filters.category = Some(category.to_owned()),
        }
        app.add_all_sweep_candidates();
        assert_eq!(app.timeline.clips.len(), 1, "only the shown {filter} range");
        let clip = &app.timeline.clips[0];
        assert_eq!(clip.source.as_deref(), Some("/media/b.mp4"));
        assert_eq!((clip.source_in, clip.duration), (expected, 2.0));
    }
    app.timeline = Timeline::empty_project();
    app.add_all_scene_clips();
    assert_eq!(
        app.timeline.clips.len(),
        4,
        "category and dismissals never hide source scenes"
    );
    for (index, clip) in app.timeline.clips.iter().enumerate() {
        assert_eq!(clip.source.as_deref(), Some("/media/b.mp4"));
        assert_eq!(
            (clip.start, clip.source_in, clip.duration),
            (index as f64 * 2.0, index as f64 * 2.0, 2.0)
        );
    }
    let snapshot: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
    assert_eq!(
        snapshot["sceneClips"].as_array().unwrap().len(),
        8,
        "REPL inventory is not the UI filter"
    );
    assert_eq!(
        snapshot["sweepCandidates"].as_array().unwrap().len(),
        6,
        "only explicit dismissals hide suggestions"
    );
}

#[test]
fn gallery_add_shown_combines_filters_and_preserves_display_order() {
    use gallery::{GalleryDuration, GalleryFilters, GalleryQuality, GallerySort};

    let mut app = Editor::headless();
    for (path, name) in [
        ("/media/expo.mp4", "Expo.mp4"),
        ("/media/interview.mov", "Interview.mov"),
    ] {
        let candidates: Vec<_> = [
            (0.25, 1.5, 82.0, "action", "Rocket launch ready"),
            (10.0, 2.0, 96.0, "action", "Rocket launch now"),
            (20.0, 7.5, 87.0, "action", "Launch later"),
            (40.0, 30.0, 88.0, "talking", "Rocket payload"),
            (80.0, 8.0, 77.0, "action", "Slow launch"),
            (100.0, 29.5, 80.0, "action", "Rocket flame"),
        ]
        .into_iter()
        .enumerate()
        .map(
            |(index, (source_in, duration, score, category, text))| sweep::SweepCandidate {
                id: format!("{path}#pick-{index}"),
                asset_path: path.to_owned(),
                asset_name: name.to_owned(),
                title: format!("Take {index}"),
                source_in,
                duration,
                score,
                reason: if index == 2 {
                    "Crowd portrait"
                } else {
                    "Useful range"
                }
                .to_owned(),
                stability: 0.9,
                audio_energy: 0.1,
                audio_peak: 0.2,
                category: category.to_owned(),
                tags: vec![if matches!(index, 0 | 1 | 3 | 5) {
                    "prime"
                } else {
                    "steady"
                }
                .to_owned()],
                motion_score: 0.3,
                audio_score: 0.4,
                music_bed: false,
                visual: None,
                has_video: true,
                transcript: vec![sweep::TranscriptSegment {
                    start: source_in,
                    end: source_in + duration,
                    speaker: None,
                    text: text.to_owned(),
                    words: Vec::new(),
                    session: None,
                }],
                review: sweep::ClipReview {
                    status: match index {
                        2 => sweep::ReviewStatus::NeedsReview,
                        4 => sweep::ReviewStatus::Local,
                        _ => sweep::ReviewStatus::Reviewed,
                    },
                    ..Default::default()
                },
                dismissed: index == 5,
            },
        )
        .collect();
        let mut scene = candidates[0].clone();
        scene.id = format!("{path}#scene-0");
        scene.source_in = 0.0;
        scene.duration = 200.0;
        scene.review.status = sweep::ReviewStatus::Rejected;
        app.assets.push(BinAsset {
            info: MediaInfo {
                path: path.to_owned(),
                name: name.to_owned(),
                duration: 200.0,
                width: 64,
                height: 36,
                fps: 30.0,
                has_video: true,
                has_audio: false,
            },
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumbs: Vec::new(),
            sweep: sweep::SweepData {
                status: sweep::SweepStatus::Ready,
                candidates,
                scene_clips: vec![scene],
                ..Default::default()
            },
        });
    }
    let before: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
    // Review choices are offered only while the bin holds a curation the
    // removed reviewer saved.
    assert_eq!(
        app.gallery_quality_choices(),
        [
            GalleryQuality::All,
            GalleryQuality::Prime,
            GalleryQuality::Reviewed,
            GalleryQuality::NeedsReview,
            GalleryQuality::Local,
        ]
    );
    fn candidates(app: &mut Editor) -> impl Iterator<Item = &mut sweep::SweepCandidate> {
        app.assets
            .iter_mut()
            .flat_map(|asset| &mut asset.sweep.candidates)
    }
    let saved: Vec<_> = candidates(&mut app)
        .map(|clip| std::mem::take(&mut clip.review.status))
        .collect();
    assert_eq!(
        app.gallery_quality_choices(),
        [GalleryQuality::All, GalleryQuality::Prime]
    );
    for (clip, status) in candidates(&mut app).zip(saved) {
        clip.review.status = status;
    }
    app.selected_asset = Some("/media/interview.mov".to_owned());
    let add_shown = |app: &mut Editor| -> Vec<(f64, f64)> {
        app.timeline = Timeline::empty_project();
        app.add_all_sweep_candidates();
        assert!(app
            .timeline
            .clips
            .iter()
            .all(|clip| clip.source.as_deref() == Some("/media/interview.mov")));
        app.timeline
            .clips
            .iter()
            .map(|clip| (clip.source_in, clip.duration))
            .collect()
    };
    app.curator.filters = GalleryFilters {
        search: "  ROCKET  launch ".to_owned(),
        category: Some("action".to_owned()),
        quality: GalleryQuality::Prime,
        duration: GalleryDuration::TwoToEight,
        ..Default::default()
    };
    assert_eq!(
        add_shown(&mut app),
        [(10.0, 2.0)],
        "every filter must match"
    );
    app.curator.filters.duration = GalleryDuration::All;
    assert_eq!(add_shown(&mut app), [(10.0, 2.0), (0.25, 1.5)]);

    for (search, expected) in [
        ("interview rocket payload", vec![(40.0, 30.0)]),
        ("CROWD steady", vec![(20.0, 7.5)]),
        ("flame", Vec::new()),
        ("not-in-any-clip", Vec::new()),
    ] {
        app.curator.filters = GalleryFilters {
            search: search.to_owned(),
            ..Default::default()
        };
        assert_eq!(add_shown(&mut app), expected, "search {search}");
    }
    for (quality, expected) in [
        (GalleryQuality::NeedsReview, vec![(20.0, 7.5)]),
        (GalleryQuality::Local, vec![(80.0, 8.0)]),
        (
            GalleryQuality::Reviewed,
            vec![(10.0, 2.0), (40.0, 30.0), (0.25, 1.5)],
        ),
    ] {
        app.curator.filters = GalleryFilters {
            quality,
            ..Default::default()
        };
        assert_eq!(add_shown(&mut app), expected);
    }
    for (duration, expected) in [
        (GalleryDuration::UnderTwo, vec![(0.25, 1.5)]),
        (GalleryDuration::TwoToEight, vec![(10.0, 2.0), (20.0, 7.5)]),
        (GalleryDuration::EightToThirty, vec![(80.0, 8.0)]),
        (GalleryDuration::ThirtyPlus, vec![(40.0, 30.0)]),
    ] {
        app.curator.filters = GalleryFilters {
            duration,
            ..Default::default()
        };
        assert_eq!(add_shown(&mut app), expected, "half-open duration bands");
    }
    for (sort, expected) in [
        (
            GallerySort::Score,
            // The Needs review range sorts after every usable one.
            vec![
                (10.0, 2.0),
                (40.0, 30.0),
                (0.25, 1.5),
                (80.0, 8.0),
                (20.0, 7.5),
            ],
        ),
        (
            GallerySort::Source,
            vec![
                (0.25, 1.5),
                (10.0, 2.0),
                (20.0, 7.5),
                (40.0, 30.0),
                (80.0, 8.0),
            ],
        ),
        (
            GallerySort::Shortest,
            vec![
                (0.25, 1.5),
                (10.0, 2.0),
                (20.0, 7.5),
                (80.0, 8.0),
                (40.0, 30.0),
            ],
        ),
        (
            GallerySort::Longest,
            vec![
                (40.0, 30.0),
                (80.0, 8.0),
                (20.0, 7.5),
                (10.0, 2.0),
                (0.25, 1.5),
            ],
        ),
    ] {
        app.curator.filters = GalleryFilters {
            sort,
            ..Default::default()
        };
        assert_eq!(add_shown(&mut app), expected);
        let mut cursor = 0.0;
        for clip in &app.timeline.clips {
            assert_eq!(clip.start, cursor, "display order becomes timeline order");
            cursor += clip.duration;
        }
    }
    app.selected_asset = None;
    app.curator.filters = GalleryFilters {
        search: "rocket launch".to_owned(),
        quality: GalleryQuality::Prime,
        duration: GalleryDuration::TwoToEight,
        ..Default::default()
    };
    app.timeline = Timeline::empty_project();
    app.add_all_sweep_candidates();
    assert_eq!(app.timeline.clips.len(), 2);
    assert_eq!(
        app.timeline.clips[0].source.as_deref(),
        Some("/media/expo.mp4")
    );
    assert_eq!(
        app.timeline.clips[1].source.as_deref(),
        Some("/media/interview.mov")
    );

    app.curator.filters.search = "no matches".to_owned();
    app.timeline = Timeline::empty_project();
    app.add_all_scene_clips();
    assert_eq!(
        app.timeline.clips.len(),
        2,
        "All scenes bypasses every Clip filter"
    );
    assert_eq!(
        (app.timeline.clips[0].start, app.timeline.clips[0].duration),
        (0.0, 200.0)
    );
    assert_eq!(
        (app.timeline.clips[1].start, app.timeline.clips[1].duration),
        (200.0, 200.0)
    );
    let after: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
    assert_eq!(before["sweepCandidates"], after["sweepCandidates"]);
    assert_eq!(before["sceneClips"], after["sceneClips"]);
}

#[test]
#[ignore = "needs OPENREEL_MONTAGE_DIR pointing at the local montage corpus"]
fn local_montage_corpus_curates_and_builds_recap_rehearsal() {
    let Ok(dir) = std::env::var("OPENREEL_MONTAGE_DIR") else {
        println!("SKIP local montage corpus: OPENREEL_MONTAGE_DIR is not set");
        return;
    };
    let mut pieces: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read montage dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ["mp4", "mov", "mkv", "webm"].contains(&ext.to_lowercase().as_str()))
                .unwrap_or(false)
        })
        .collect();
    pieces.sort();
    assert!(!pieces.is_empty(), "no montage videos in {dir}");
    println!("montage corpus: {} file(s) in {dir}", pieces.len());

    let mut app = Editor::headless();
    let expected_assets = pieces.len();
    app.spawn_import(pieces);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
    let mut curation_started = false;
    loop {
        app.poll_imports();
        app.poll_sweeps();
        if app.import_rx.is_none() && app.assets.len() == expected_assets && !curation_started {
            app.curate_all_new();
            curation_started = true;
        }
        let done = curation_started
            && app.import_rx.is_none()
            && app.sweep_rx.is_none()
            && app.sweep_queue.is_empty()
            && app.assets.len() == expected_assets
            && app.assets.iter().all(|asset| {
                matches!(
                    asset.sweep.status,
                    sweep::SweepStatus::Ready | sweep::SweepStatus::Failed
                )
            });
        if done {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "montage import/curation timeout: {}",
            app.status
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    for asset in &app.assets {
        let beds = asset
            .sweep
            .candidates
            .iter()
            .filter(|candidate| candidate.music_bed)
            .count();
        let visual_candidates = asset
            .sweep
            .candidates
            .iter()
            .filter(|candidate| candidate.visual.is_some())
            .count();
        let covered: f64 = asset
            .sweep
            .scene_clips
            .iter()
            .map(|scene| scene.duration)
            .sum();
        println!(
            "curated {}: status={:?} picks={} scenes={} sceneCoverage={covered:.2}/{:.2}s musicBedPicks={} visualPicks={} note={}",
            asset.info.name,
            asset.sweep.status,
            asset.sweep.candidates.len(),
            asset.sweep.scene_clips.len(),
            asset.info.duration,
            beds,
            visual_candidates,
            asset.sweep.note,
        );
        assert!(
            !asset.sweep.scene_clips.is_empty(),
            "no scene clips for {}",
            asset.info.name
        );
        assert!(
            asset.sweep.scene_clips[0].source_in.abs() < 0.001,
            "scene breakdown starts at zero"
        );
        for pair in asset.sweep.scene_clips.windows(2) {
            assert!(
                (pair[0].end() - pair[1].source_in).abs() < 0.001,
                "scene breakdown is contiguous for {}",
                asset.info.name
            );
        }
        let last_scene = asset.sweep.scene_clips.last().expect("last scene");
        assert!(
            (last_scene.end() - asset.info.duration).abs() < 0.01,
            "scene breakdown ends at source duration for {}",
            asset.info.name
        );
        assert!(
            (covered - asset.info.duration).abs() < 0.01,
            "scene breakdown covers the whole source for {}",
            asset.info.name
        );
        let scene_ranges: Vec<String> = asset
            .sweep
            .scene_clips
            .iter()
            .map(|scene| format!("{:.2}-{:.2}", scene.source_in, scene.end()))
            .collect();
        println!(
            "scene ranges {}: {}",
            asset.info.name,
            scene_ranges.join(" ")
        );
        assert!(
            !asset.sweep.candidates.is_empty(),
            "no candidates for {}",
            asset.info.name
        );
        assert_eq!(
            visual_candidates,
            asset.sweep.candidates.len(),
            "every video candidate carries local visual evidence"
        );
        for candidate in &asset.sweep.candidates {
            let visual = candidate.visual_json();
            assert_eq!(visual["source"], "local-proxy");
            assert_eq!(visual["samStatus"], "unavailable");
            assert!(visual["facePresent"].is_null());
            assert!(visual["expression"].is_null());
        }
    }

    app.run_console(
        "var byAsset = {};\n\
         sweepCandidates().forEach(function (c) { if (c.hasVideo) { (byAsset[c.asset] = byAsset[c.asset] || []).push(c); } });\n\
         clips().forEach(function (c) { deleteClip(c.id); });\n\
         var cursor = 0;\n\
         addTitle('2025 RECAP', 0, 2, 'pop-caption');\n\
         Object.keys(byAsset).sort().forEach(function (asset) {\n\
           byAsset[asset].slice(0, 2).forEach(function (c) { var d = Math.min(c.duration, 3); addAssetRange(c.asset, cursor, c.sourceIn, d); cursor += d; });\n\
         });\n\
         console.log('Done — local recap rehearsal built.');"
            .to_owned(),
    );
    let errors: Vec<&String> = app
        .console_output
        .iter()
        .filter(|line| line.starts_with("error"))
        .collect();
    assert!(errors.is_empty(), "console errors: {errors:?}");
    let videos = app
        .timeline
        .clips
        .iter()
        .filter(|clip| clip.kind == ClipKind::Video)
        .count();
    assert!(videos >= expected_assets, "grouped picks landed: {videos}");
    assert!(app.timeline.clips.iter().any(|clip| {
        clip.kind == ClipKind::Title && clip.title_style == openreel_engine::TitleStyle::PopCaption
    }));
    let expected_end = app.timeline.end();

    let output = std::env::var("OPENREEL_RECAP_OUTPUT")
        .unwrap_or_else(|_| "/tmp/recap_2025_local_rehearsal.mp4".to_owned());
    let _ = std::fs::remove_file(&output);
    app.run_console(format!("exportTo('{output}')"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        app.poll_export();
        if app.export_rx.is_none() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "recap export timeout: {}",
            app.status
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(app.status.starts_with("Exported "), "{}", app.status);
    let info = openreel_media::probe(&output).expect("probe local recap");
    assert!(
        info.has_video && info.has_audio,
        "recap must contain picture and sound"
    );
    println!(
        "local recap: {:.2}s clips={} status={} output={}",
        info.duration,
        app.timeline.clips.len(),
        app.status,
        output,
    );
    assert!((info.duration - expected_end).abs() < 0.6);
}

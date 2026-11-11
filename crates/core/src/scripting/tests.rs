use super::*;

#[test]
fn eval_queues_commands_and_persists_variables() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (out, _) = engine.eval("var answer = 40", r#"{"selected": 3, "playhead": 1.5}"#);
    assert_eq!(out, "undefined"); // declaration completion value
    let (out, commands) = engine.eval(
        "seek(2.5); split(); answer + 2",
        r#"{"selected": 3, "playhead": 1.5}"#,
    );
    assert_eq!(out, "42");
    assert_eq!(
        commands,
        vec![
            ScriptCommand::Seek(2.5),
            ScriptCommand::SplitClip {
                clip: 3,
                at: 1.5,
                handle: Some(CLIP_HANDLE_BASE),
            },
        ]
    );
}

#[test]
fn add_asset_range_queues_subclip() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (_, commands) = engine.eval(
        "addAssetRange('a.mp4', 12, 3.5, 4)",
        r#"{"sweepCandidates": []}"#,
    );
    assert_eq!(
        commands,
        vec![ScriptCommand::AddAssetRange {
            asset: "a.mp4".to_owned(),
            at: 12.0,
            source_in: 3.5,
            duration: 4.0,
            handle: Some(CLIP_HANDLE_BASE),
        }]
    );
}

#[test]
fn music_bed_mute_policy_queues_command() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (_, commands) = engine.eval("setMusicBedMuted(false)", "{}");
    assert_eq!(commands, vec![ScriptCommand::SetMusicBedMuted(false)]);
}

#[test]
fn detach_link_commands_queue_in_order() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (_, commands) = engine.eval(
        "detachAudio(4); unlinkClip(4); setEmbeddedAudio(4, false)",
        "{}",
    );
    assert_eq!(
        commands,
        vec![
            ScriptCommand::DetachAudio { clip: 4 },
            ScriptCommand::UnlinkClip { clip: 4 },
            ScriptCommand::SetEmbeddedAudio {
                clip: 4,
                enabled: false,
            },
        ]
    );
}

#[test]
fn tighten_silence_queues_defaults_and_custom_thresholds() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (_, commands) = engine.eval(
        "tightenSilence(9); tightenSilence(10, 0.05, 0.5, 0.12)",
        "{}",
    );
    assert_eq!(
        commands,
        vec![
            ScriptCommand::TightenSilence {
                clip: 9,
                threshold: None,
                min_silence: 0.35,
                padding: 0.08,
            },
            ScriptCommand::TightenSilence {
                clip: 10,
                threshold: Some(0.05),
                min_silence: 0.5,
                padding: 0.12,
            },
        ]
    );
}

#[test]
fn title_and_caption_commands_queue_styles() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (_, commands) = engine.eval(
            "addTitle('Intro', 1, 2, 'pop-caption'); setTitleStyle(8, 'minimal-caption'); addCaptionsForClip(3); addCaptionsForClip(4, 'caption')",
            "{}",
        );
    assert_eq!(
        commands,
        vec![
            ScriptCommand::AddTitle {
                text: "Intro".to_owned(),
                at: 1.0,
                duration: 2.0,
                style: "pop-caption".to_owned(),
                handle: Some(CLIP_HANDLE_BASE),
            },
            ScriptCommand::SetTitleStyle {
                clip: 8,
                style: "minimal-caption".to_owned(),
            },
            ScriptCommand::AddCaptionsForClip {
                clip: 3,
                style: "caption".to_owned(),
            },
            ScriptCommand::AddCaptionsForClip {
                clip: 4,
                style: "caption".to_owned(),
            },
        ]
    );
}

#[test]
fn time_specs_resolve_against_the_request_time_clips() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let state = r#"{"fps":30,"duration":20,"playhead":0,"selected":7,
            "clips":[{"id":7,"start":10,"duration":4},{"id":9,"start":14,"duration":6}]}"#;
    let (out, commands) = engine
        .eval_checked(
            "seek('clip:7@end'); moveClip(9, '0:01.5'); splitClip(7, 'clip:7@50%');
                 addTitle('Hi', '25%', 1); seek(2); split('90f'); insertTime('clip:9', 2);
                 trimClip(7, 'clip:7@1s', null); trimClip(9, -1, 18);
                 const r = timeRange('clip:7..clip:9');
                 try { seek('clip:8'); } catch (e) { console.log(String(e)); }
                 return [time('end'), r.start, r.end].join(',');",
            state,
        )
        .expect("valid specs run");
    assert_eq!(out, "20,10,20");
    assert_eq!(
        commands,
        vec![
            ScriptCommand::Seek(14.0),
            ScriptCommand::MoveClip {
                clip: 9,
                start: 1.5
            },
            ScriptCommand::SplitClip {
                clip: 7,
                at: 12.0,
                handle: Some(CLIP_HANDLE_BASE),
            },
            ScriptCommand::AddTitle {
                text: "Hi".to_owned(),
                at: 5.0,
                duration: 1.0,
                style: "lower-third".to_owned(),
                handle: Some(CLIP_HANDLE_BASE + 1),
            },
            ScriptCommand::Seek(2.0),
            ScriptCommand::SplitClip {
                clip: 7,
                at: 3.0,
                handle: Some(CLIP_HANDLE_BASE + 2),
            },
            ScriptCommand::InsertTime {
                at: 14.0,
                seconds: 2.0
            },
            // A trim edge is a timeline time too; null or -1 keeps one.
            ScriptCommand::TrimClip {
                clip: 7,
                left: Some(11.0),
                right: None,
            },
            ScriptCommand::TrimClip {
                clip: 9,
                left: None,
                right: Some(18.0),
            },
            ScriptCommand::Log("Error: no clip 8; clips: 7, 9".to_owned()),
        ]
    );

    // An uncaught bad spec rejects the whole run, as any thrown error does.
    let error = engine
        .eval_checked("seek(1); seek('soonish');", state)
        .expect_err("bad spec");
    assert!(error.contains("can not parse \"soonish\""), "{error}");
}

#[test]
fn scene_clips_read_complete_breakdown_from_state() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let state = r#"{"sceneClips":[{"id":"clip.mp4#scene-0","sourceIn":0,"duration":2},{"id":"clip.mp4#scene-1","sourceIn":2,"duration":3}]}"#;
    let (out, _) = engine.eval(
        "sceneClips().map(c => c.id + ':' + c.sourceIn + '+' + c.duration).join(',')",
        state,
    );
    assert_eq!(out, "clip.mp4#scene-0:0+2,clip.mp4#scene-1:2+3");
}

#[test]
fn repl_handles_are_never_reused_across_runs() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (first, _) = engine.eval("var a = addTitle('A', 0, 1); a", "{}");
    let (_, mut commands) = engine.eval("addTitle('B', 0, 1); setFade(a, 0, 1)", "{}");
    assert_eq!(first, CLIP_HANDLE_BASE.to_string());
    assert_eq!(
        commands[0],
        ScriptCommand::AddTitle {
            text: "B".to_owned(),
            at: 0.0,
            duration: 1.0,
            style: "lower-third".to_owned(),
            handle: Some(CLIP_HANDLE_BASE + 1),
        }
    );
    // The second run created only handle BASE + 1, so `a` cannot alias it.
    let created = std::collections::HashMap::from([(CLIP_HANDLE_BASE + 1, 5)]);
    assert!(commands[1].resolve_handle(&created).is_err());
}

#[test]
fn transcript_segments_reads_state() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (out, commands) = engine.eval(
            "addAssetRange('a.mp4', 0, transcriptSegments()[0].start, transcriptSegments()[0].end - transcriptSegments()[0].start); transcriptSegments().length",
            r#"{"transcriptSegments": [{"id": "tx:a.mp4#0", "asset": "a.mp4", "start": 4, "end": 7.5, "text": "pricing"}]}"#,
        );
    assert_eq!(out, "1");
    assert_eq!(
        commands,
        vec![ScriptCommand::AddAssetRange {
            asset: "a.mp4".to_owned(),
            at: 0.0,
            source_in: 4.0,
            duration: 3.5,
            handle: Some(CLIP_HANDLE_BASE),
        }]
    );
}

#[test]
fn eval_reports_errors_without_poisoning_context() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (out, _) = engine.eval("throw new Error('boom')", "{}");
    assert!(out.contains("boom"), "{out}");
    let (out, _) = engine.eval("1 + 1", "{}");
    assert_eq!(out, "2");
}

fn assert_rejected(engine: &mut ScriptEngine, code: &str, message: &str) {
    let code = format!("(() => {{\n{code}\n}})()");
    let state = r#"{"selected":1,"playhead":0}"#;
    let (output, commands) = engine.eval(&code, state);
    assert!(output.contains(message), "{code}: {output}");
    assert!(
        commands.is_empty(),
        "{code}: rejected eval returned {} commands",
        commands.len()
    );
    let result = engine.eval_checked(&format!("return {code};"), state);
    assert!(
        result.is_err(),
        "{code}: checked eval unexpectedly succeeded"
    );
    let error = result.unwrap_err();
    assert!(error.contains(message), "{code}: {error}");
}

#[test]
fn command_budget_rejects_overflow_even_when_caught() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    assert_rejected(
        &mut engine,
        "for (let i = 0; i < 2049; i++) { try { play(); } catch (_) {} }",
        "command limit",
    );
    let (_, commands) = engine.eval("seek(2)", "{}");
    assert_eq!(commands, vec![ScriptCommand::Seek(2.0)]);
    let (_, commands) = engine.eval_checked("pause()", "{}").unwrap();
    assert_eq!(commands, vec![ScriptCommand::Pause]);
}

#[test]
fn string_budget_covers_every_owned_command_field() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    for call in [
        "console.log(text)",
        "renameClip(1, text)",
        "addTitle(text, 0, 1, '')",
        "addTitle('', 0, 1, text)",
        "setTitleStyle(1, text)",
        "addCaptionsForClip(1, text)",
        "addAsset(text, 0)",
        "addAssetRange(text, 0, 0, 1)",
        "importMedia(text)",
        "exportTo(text)",
    ] {
        assert_rejected(
                &mut engine,
                &format!(
                    "play(); const text = 'x'.repeat(1024 * 1024 + 1); try {{ {call}; }} catch (_) {{}} pause();"
                ),
                "string byte limit",
            );
    }
}

#[test]
fn string_budget_counts_combined_fields_and_commands_in_utf8_bytes() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    for code in [
            "const text = 'x'.repeat(512 * 1024); console.log(text); renameClip(1, text); try { renameClip(2, 'x'); } catch (_) {}",
            "const text = 'x'.repeat(512 * 1024); try { addTitle(text, 0, 1, text + 'x'); } catch (_) {}",
            "try { console.log('é'.repeat(512 * 1024 + 1)); } catch (_) {}",
        ] {
            assert_rejected(&mut engine, code, "string byte limit");
        }
}

#[test]
fn command_and_string_budget_boundaries_reset_between_evals() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let code = "for (let i = 0; i < 2047; i++) play(); console.log('é'.repeat(512 * 1024));";
    for _ in 0..2 {
        let (output, commands) = engine.eval(code, "{}");
        assert_eq!(output, "undefined");
        assert_eq!(commands.len(), 2048);
        assert!(commands[..2047]
            .iter()
            .all(|command| *command == ScriptCommand::Play));
        assert!(
            matches!(commands.last(), Some(ScriptCommand::Log(text)) if text.len() == 1024 * 1024)
        );
        let (_, commands) = engine.eval_checked(code, "{}").unwrap();
        assert_eq!(commands.len(), 2048);
        assert!(
            matches!(commands.last(), Some(ScriptCommand::Log(text)) if text.len() == 1024 * 1024)
        );
    }
    assert_rejected(
        &mut engine,
        "for (let i = 0; i < 2049; i++) { try { play(); } catch (_) {} }",
        "command limit",
    );
    assert_rejected(
        &mut engine,
        "try { console.log('x'.repeat(1024 * 1024 + 1)); } catch (_) {}",
        "string byte limit",
    );
    let (output, commands) = engine.eval(code, "{}");
    assert_eq!(output, "undefined");
    assert_eq!(commands.len(), 2048);
    let (_, commands) = engine
        .eval_checked(
            "const text = 'x'.repeat(512 * 1024); addTitle(text, 0, 1, text);",
            "{}",
        )
        .unwrap();
    assert!(
        matches!(&commands[..], [ScriptCommand::AddTitle { text, style, .. }] if text.len() + style.len() == 1024 * 1024)
    );
}

#[test]
fn command_budget_also_covers_result_formatting_callbacks() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    assert_rejected(
            &mut engine,
            "return { toJSON() { for (let i = 0; i < 2049; i++) { try { play(); } catch (_) {} } return 'done'; } };",
            "command limit",
        );
}

#[test]
fn failed_checked_eval_cannot_poison_checked_or_console_globals() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    assert!(engine
        .eval_checked(
            "seek(1); globalThis.seek = () => {}; throw new Error('boom');",
            "{}",
        )
        .is_err());
    let (_, commands) = engine.eval_checked("seek(2)", "{}").unwrap();
    assert_eq!(commands, vec![ScriptCommand::Seek(2.0)]);
    let (_, commands) = engine.eval("seek(3)", "{}");
    assert_eq!(commands, vec![ScriptCommand::Seek(3.0)]);
}

#[test]
fn successful_checked_eval_cannot_poison_globals_or_prototypes() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (_, commands) = engine
            .eval_checked(
                "globalThis.seek = () => {}; Object.prototype.poisoned = true; globalThis.__log = () => {};",
                "{}",
            )
            .unwrap();
    assert!(commands.is_empty());
    let code =
        "if (({}).poisoned) throw new Error('poisoned prototype'); seek(2); console.log('ok');";
    let (_, commands) = engine.eval_checked(code, "{}").unwrap();
    assert_eq!(
        commands,
        vec![
            ScriptCommand::Seek(2.0),
            ScriptCommand::Log("ok".to_owned())
        ]
    );
    let (output, commands) = engine.eval(code, "{}");
    assert_eq!(output, "undefined");
    assert_eq!(
        commands,
        vec![
            ScriptCommand::Seek(2.0),
            ScriptCommand::Log("ok".to_owned())
        ]
    );
}

#[test]
fn checked_eval_does_not_inherit_console_globals_or_erase_variables() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (output, _) = engine.eval(
            "var answer = 40; let lexical = 2; globalThis.seek = () => {}; JSON.parse = () => { throw new Error('console JSON'); };",
            "{}",
        );
    assert!(!output.starts_with("error"), "{output}");
    for _ in 0..2 {
        let (output, commands) = engine
                .eval_checked(
                    "if (typeof answer !== 'undefined' || typeof lexical !== 'undefined') throw new Error('console variables leaked'); const s = state(); seek(s.playhead); return s.playhead;",
                    r#"{"playhead":4}"#,
                )
                .unwrap();
        assert_eq!(output, "4");
        assert_eq!(commands, vec![ScriptCommand::Seek(4.0)]);
    }
    let (output, commands) = engine.eval("answer + lexical", "{}");
    assert_eq!(output, "42");
    assert!(commands.is_empty());
    let (_, commands) = engine.eval("seek(1)", "{}");
    assert!(commands.is_empty(), "console's own globals must persist");
}

#[test]
fn nonfinite_numeric_arguments_reject_the_entire_eval_even_when_caught() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    for call in [
        "seek(BAD)",
        "split(BAD)",
        "splitClip(1, BAD)",
        "moveClip(1, BAD)",
        "setOpacity(1, BAD)",
        "setVolume(1, BAD)",
        "setZoom(BAD)",
        "tightenSilence(1, BAD, 0.35, 0.08)",
        "tightenSilence(1, 0.03, BAD, 0.08)",
        "tightenSilence(1, 0.03, 0.35, BAD)",
        "addTitle('title', BAD, 1)",
        "addTitle('title', 0, BAD)",
        "addAsset('asset', BAD)",
        "addAssetRange('asset', BAD, 0, 1)",
        "addAssetRange('asset', 0, BAD, 1)",
        "addAssetRange('asset', 0, 0, BAD)",
    ] {
        for bad in ["NaN", "Infinity", "-Infinity"] {
            let call = call.replace("BAD", bad);
            assert_rejected(
                &mut engine,
                &format!("play(); try {{ {call}; }} catch (_) {{}} pause();"),
                "finite",
            );
        }
    }
    let (_, commands) = engine.eval("seek(1)", "{}");
    assert_eq!(commands, vec![ScriptCommand::Seek(1.0)]);
}

#[test]
fn trim_clip_rejects_nonfinite_edges_before_keep_sentinel_conversion() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    for bad in ["NaN", "Infinity", "-Infinity"] {
        for call in [
            format!("trimClip(1, {bad}, -1)"),
            format!("trimClip(1, -1, {bad})"),
        ] {
            assert_rejected(
                &mut engine,
                &format!("play(); try {{ {call}; }} catch (_) {{}} pause();"),
                "finite",
            );
        }
    }
}

#[test]
fn numeric_arguments_that_overflow_f32_are_rejected() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    for call in [
        "setOpacity(1, 1e100)",
        "setVolume(1, -1e100)",
        "setZoom(1e100)",
        "tightenSilence(1, 1e100)",
    ] {
        assert_rejected(&mut engine, call, "finite");
    }
}

#[test]
fn command_validation_checks_every_nonfinite_field_without_javascript() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        for command in [
            ScriptCommand::Seek(value),
            ScriptCommand::SplitClip {
                clip: 1,
                at: value,
                handle: None,
            },
            ScriptCommand::MoveClip {
                clip: 1,
                start: value,
            },
            ScriptCommand::TrimClip {
                clip: 1,
                left: Some(value),
                right: None,
            },
            ScriptCommand::TrimClip {
                clip: 1,
                left: None,
                right: Some(value),
            },
            ScriptCommand::SetOpacity {
                clip: 1,
                value: value as f32,
            },
            ScriptCommand::SetVolume {
                clip: 1,
                value: value as f32,
            },
            ScriptCommand::SetZoom(value as f32),
            ScriptCommand::TightenSilence {
                clip: 1,
                threshold: Some(value as f32),
                min_silence: 0.35,
                padding: 0.08,
            },
            ScriptCommand::TightenSilence {
                clip: 1,
                threshold: Some(0.03),
                min_silence: value,
                padding: 0.08,
            },
            ScriptCommand::TightenSilence {
                clip: 1,
                threshold: Some(0.03),
                min_silence: 0.35,
                padding: value,
            },
            ScriptCommand::AddTitle {
                text: String::new(),
                at: value,
                duration: 1.0,
                style: String::new(),
                handle: None,
            },
            ScriptCommand::AddTitle {
                text: String::new(),
                at: 0.0,
                duration: value,
                style: String::new(),
                handle: None,
            },
            ScriptCommand::AddAsset {
                asset: String::new(),
                at: value,
                handle: None,
            },
            ScriptCommand::AddAssetRange {
                asset: String::new(),
                at: value,
                source_in: 0.0,
                duration: 1.0,
                handle: None,
            },
            ScriptCommand::AddAssetRange {
                asset: String::new(),
                at: 0.0,
                source_in: value,
                duration: 1.0,
                handle: None,
            },
            ScriptCommand::AddAssetRange {
                asset: String::new(),
                at: 0.0,
                source_in: 0.0,
                duration: value,
                handle: None,
            },
        ] {
            assert!(command.validate().is_err(), "{command:?}");
        }
    }
}

#[test]
fn command_validation_leaves_finite_ranges_and_permissions_to_the_executor() {
    for command in [
        ScriptCommand::Seek(f64::MIN),
        ScriptCommand::Seek(f64::MAX),
        ScriptCommand::SetZoom(f32::MIN),
        ScriptCommand::SetZoom(f32::MAX),
        ScriptCommand::TrimClip {
            clip: 1,
            left: Some(-1.0),
            right: None,
        },
        ScriptCommand::StepFrames(i64::MIN),
        ScriptCommand::SelectClip(u64::MAX),
        ScriptCommand::Import("source.mp4".to_owned()),
        ScriptCommand::Export("output.mp4".to_owned()),
    ] {
        assert!(command.validate().is_ok(), "{command:?}");
    }
}

#[test]
fn string_callbacks_preserve_utf8_nul_and_path_commands() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let code = r#"renameClip(1, 'café 🎬'); console.log('résumé\0', 3); importMedia('source.mp4'); exportTo('output.mp4');"#;
    let expected = vec![
        ScriptCommand::RenameClip {
            clip: 1,
            name: "café 🎬".to_owned(),
        },
        ScriptCommand::Log("résumé\0 3".to_owned()),
        ScriptCommand::Import("source.mp4".to_owned()),
        ScriptCommand::Export("output.mp4".to_owned()),
    ];
    let (output, commands) = engine.eval(code, "{}");
    assert_eq!(output, "undefined");
    assert_eq!(commands, expected);
    let (_, commands) = engine.eval_checked(code, "{}").unwrap();
    assert_eq!(commands, expected);
}

#[test]
fn drawn_text_refuses_control_characters_but_keeps_line_breaks() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    for code in [
        r#"renameClip(1, 'nul\0tail')"#,
        r#"addTitle('bell\u0007', 0, 2)"#,
        r#"addTitle('escape\u001b[0m', 0, 2)"#,
    ] {
        let error = engine.eval_checked(code, "{}").unwrap_err();
        assert!(error.contains("control characters"), "{code}: {error}");
    }
    // Preview and export both draw a line break as a space.
    let (_, commands) = engine
        .eval_checked(r#"addTitle('two\nlines\there', 0, 2)"#, "{}")
        .unwrap();
    assert_eq!(commands.len(), 1);
}

#[test]
fn clip_ids_and_lanes_must_be_whole_numbers() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let state = r#"{"selected": null, "playhead": 0}"#;
    for code in [
        "deleteClip(1.9)",
        "moveClip(2.7, 20)",
        "selectClip(NaN)",
        "deleteClip(-5)",
        "setClipLane(1, 0.5)",
        "setClipLane(1, -1)",
        "setOpacity(Infinity, 1)",
        "renameClip('1', 'name')",
        "detachAudio(2 ** 60)",
    ] {
        let error = engine.eval_checked(code, state).unwrap_err();
        assert!(error.contains("whole numbers"), "{code}: {error}");
    }
    // Nothing selected is not "delete nothing".
    let error = engine.eval_checked("deleteClip()", state).unwrap_err();
    assert!(error.contains("no clip selected"), "{error}");
    let (_, commands) = engine
        .eval_checked("deleteClip(7); setClipLane(7, 0); deleteClip(7.0)", state)
        .unwrap();
    assert_eq!(
        commands,
        vec![
            ScriptCommand::DeleteClip(7),
            ScriptCommand::SetClipLane { clip: 7, lane: 0 },
            ScriptCommand::DeleteClip(7),
        ]
    );
}

#[test]
fn asynchronous_scripts_are_refused_whole() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    for code in [
        // Only the first title would have been queued.
        "return (async () => { addTitle('t1', 10, 1); await null; addTitle('t2', 12, 1); })()",
        // The callback would never have run.
        "Promise.resolve().then(() => addTitle('late', 10, 1)); return 'sync done'",
        "return Promise.resolve(3)",
    ] {
        let error = engine.eval_checked(code, "{}").unwrap_err();
        assert!(error.contains("synchronously"), "{code}: {error}");
    }
    // The next run is a fresh runtime with nothing pending.
    let (output, commands) = engine
        .eval_checked("addTitle('t', 0, 1); return 4", "{}")
        .unwrap();
    assert_eq!((output.as_str(), commands.len()), ("4", 1));
}

#[test]
fn finite_negative_arguments_and_trim_sentinels_are_preserved() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (_, commands) = engine
            .eval_checked(
                "seek(-2); splitClip(1, -3); moveClip(1, -4); trimClip(1, -1, -2); trimClip(1, 0, 2); setOpacity(1, -0.5); setVolume(1, 2); setZoom(-1); tightenSilence(1, -0.1, -0.2, -0.3); addTitle('title', -1, -2); addAsset('asset', -1); addAssetRange('asset', -1, -2, -3);",
                "{}",
            )
            .unwrap();
    assert!(commands.iter().all(|command| command.validate().is_ok()));
    assert_eq!(
        commands,
        vec![
            ScriptCommand::Seek(-2.0),
            ScriptCommand::SplitClip {
                clip: 1,
                at: -3.0,
                handle: Some(CLIP_HANDLE_BASE),
            },
            ScriptCommand::MoveClip {
                clip: 1,
                start: -4.0
            },
            ScriptCommand::TrimClip {
                clip: 1,
                left: None,
                right: None
            },
            ScriptCommand::TrimClip {
                clip: 1,
                left: Some(0.0),
                right: Some(2.0)
            },
            ScriptCommand::SetOpacity {
                clip: 1,
                value: -0.5
            },
            ScriptCommand::SetVolume {
                clip: 1,
                value: 2.0
            },
            ScriptCommand::SetZoom(-1.0),
            ScriptCommand::TightenSilence {
                clip: 1,
                threshold: Some(-0.1),
                min_silence: -0.2,
                padding: -0.3
            },
            ScriptCommand::AddTitle {
                text: "title".to_owned(),
                at: -1.0,
                duration: -2.0,
                style: "lower-third".to_owned(),
                handle: Some(CLIP_HANDLE_BASE + 1),
            },
            ScriptCommand::AddAsset {
                asset: "asset".to_owned(),
                at: -1.0,
                handle: Some(CLIP_HANDLE_BASE + 2),
            },
            ScriptCommand::AddAssetRange {
                asset: "asset".to_owned(),
                at: -1.0,
                source_in: -2.0,
                duration: -3.0,
                handle: Some(CLIP_HANDLE_BASE + 3),
            },
        ]
    );
}

#[test]
fn ordinary_javascript_errors_keep_console_semantics_but_reject_checked_edits() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let code = "seek(1); throw new Error('boom');";
    let (output, commands) = engine.eval(code, "{}");
    assert!(output.contains("boom"), "{output}");
    assert_eq!(commands, vec![ScriptCommand::Seek(1.0)]);
    assert!(engine.eval_checked(code, "{}").is_err());
    let (_, commands) = engine.eval("pause()", "{}");
    assert_eq!(commands, vec![ScriptCommand::Pause]);
}

#[test]
fn eval_deadline_still_interrupts_and_recovers_in_both_contexts() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    assert!(engine.eval_checked("while (true) {}", "{}").is_err());
    let (_, commands) = engine.eval_checked("seek(1)", "{}").unwrap();
    assert_eq!(commands, vec![ScriptCommand::Seek(1.0)]);
    let (output, commands) = engine.eval("while (true) {}", "{}");
    assert!(output.starts_with("error"), "{output}");
    assert!(commands.is_empty());
    let (output, commands) = engine.eval("seek(2); 42", "{}");
    assert_eq!(output, "42");
    assert_eq!(commands, vec![ScriptCommand::Seek(2.0)]);
}

#[test]
fn a_call_the_vm_cannot_interrupt_is_abandoned_and_scripts_go_on() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    engine.eval("var kept = 1", "{}");

    // Well past the wait, so only giving up on the call can answer.
    let stuck = "seek(1); __test_stall(2500)";
    let error = engine.eval_checked(stuck, "{}").unwrap_err();
    assert!(error.contains("time limit"), "{error}");
    assert!(error.contains("built-in call"), "{error}");
    let (_, commands) = engine.eval_checked("seek(2)", "{}").unwrap();
    assert_eq!(commands, vec![ScriptCommand::Seek(2.0)]);
    let (output, _) = engine.eval("kept", "{}");
    assert_eq!(output, "1", "a checked script leaves the console alone");

    let (output, commands) = engine.eval(stuck, "{}");
    assert!(output.contains("time limit"), "{output}");
    assert!(output.contains("variables were reset"), "{output}");
    assert!(commands.is_empty());
    let (output, commands) = engine.eval("seek(3); typeof kept", "{}");
    assert_eq!(output, "undefined");
    assert_eq!(commands, vec![ScriptCommand::Seek(3.0)]);
}

fn sequential_title_snapshot(count: usize) -> String {
    let clips: Vec<_> = (0..count)
        .map(|index| {
            serde_json::json!({
                "id": index + 1,
                "name": "seed",
                "kind": "title",
                "track": 0,
                "start": index as f64,
                "duration": 1.0,
                "sourceIn": 0.0,
                "source": null,
                "opacity": 1.0,
                "volume": 1.0,
                "embeddedAudio": false,
                "linkedClip": null,
                "titleStyle": "lower-third",
            })
        })
        .collect();
    serde_json::json!({
        "playhead": 0.0,
        "playing": false,
        "selected": (count > 0).then_some(count),
        "duration": count as f64,
        "fps": 30.0,
        "magnet": true,
        "musicBedMuted": true,
        "clips": clips,
        "assets": [],
        "sweepCandidates": [],
        "sceneClips": [],
        "transcriptSegments": [],
    })
    .to_string()
}

#[test]
fn explicit_command_batch_fits_the_budget_with_a_large_snapshot() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (_, commands) = engine
        .eval_checked(
            "clearTimeline(); for (let i = 0; i < 2047; i++) addTitle('Shot ' + i, i, 1);",
            &sequential_title_snapshot(500),
        )
        .expect("explicit edits must not exhaust their budget rereading the snapshot");
    let expected = std::iter::once(ScriptCommand::ClearTimeline)
        .chain((0..2047).map(|index| ScriptCommand::AddTitle {
            text: format!("Shot {index}"),
            at: index as f64,
            duration: 1.0,
            style: "lower-third".to_owned(),
            handle: Some(CLIP_HANDLE_BASE + index as u64),
        }))
        .collect::<Vec<_>>();
    assert_eq!(commands, expected);
}

#[test]
fn command_defaults_use_saved_state_and_respect_explicit_arguments() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let (_, commands) = engine
            .eval_checked(
                "addTitle('explicit', 2, 1); addAsset('a.mp4', 3); deleteClip(7); addTitle('default'); addAsset('a.mp4'); deleteClip(); split(4);",
                r#"{"playhead":6.5,"selected":9}"#,
            )
            .unwrap();
    assert_eq!(
        commands,
        vec![
            ScriptCommand::AddTitle {
                text: "explicit".to_owned(),
                at: 2.0,
                duration: 1.0,
                style: "lower-third".to_owned(),
                handle: Some(CLIP_HANDLE_BASE),
            },
            ScriptCommand::AddAsset {
                asset: "a.mp4".to_owned(),
                at: 3.0,
                handle: Some(CLIP_HANDLE_BASE + 1),
            },
            ScriptCommand::DeleteClip(7),
            ScriptCommand::AddTitle {
                text: "default".to_owned(),
                at: 6.5,
                duration: 4.0,
                style: "lower-third".to_owned(),
                handle: Some(CLIP_HANDLE_BASE + 2),
            },
            ScriptCommand::AddAsset {
                asset: "a.mp4".to_owned(),
                at: 6.5,
                handle: Some(CLIP_HANDLE_BASE + 3),
            },
            ScriptCommand::DeleteClip(9),
            ScriptCommand::SplitClip {
                clip: 9,
                at: 4.0,
                handle: Some(CLIP_HANDLE_BASE + 4),
            },
        ]
    );
}

#[test]
fn command_callback_deadline_is_sticky_after_native_work() {
    let mut engine = ScriptEngine::new().expect("quickjs engine");
    let snapshot = sequential_title_snapshot(500);
    // JSON.parse does native work between VM interrupt polls. Expiration
    // must discard commands whether the VM or a caught callback notices it.
    let native_work = "play(); const until = Date.now() + 600; while (Date.now() < until) JSON.parse(__state_json);";
    let code = format!("{native_work} try {{ seek(2); }} catch (_) {{}}");
    let result = engine.eval_checked(&code, &snapshot);
    let error = result.expect_err("expired callback must reject the entire eval");
    assert!(error.contains("time limit"), "{error}");
    let (_, commands) = engine.eval_checked("seek(1)", "{}").unwrap();
    assert_eq!(commands, vec![ScriptCommand::Seek(1.0)]);

    let (output, commands) = engine.eval(&code, &snapshot);
    assert!(output.contains("time limit"), "{output}");
    assert!(commands.is_empty());
    let (_, commands) = engine.eval("seek(1)", "{}");
    assert_eq!(commands, vec![ScriptCommand::Seek(1.0)]);

    let error = engine.eval_checked(native_work, &snapshot).unwrap_err();
    assert!(error.contains("time limit"), "{error}");
}

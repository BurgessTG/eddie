use eddie_core::editor::settings::Settings;
use openreel_engine::ClipKind;
use rune::{Key, Mods, Runtime};

use super::*;
use crate::testing::{chord, click, input_arrives, isolated_config, press, shown, tooltip, window};

/// The window with one title on the timeline, so there is something to export.
fn with_a_clip() -> Runtime<Ui> {
    let mut ui = Ui::headless();
    ui.editor
        .timeline
        .add_clip("Hello", ClipKind::Title, 0, 0.0, 2.0, 0.0);
    window(ui)
}

fn ctrl_e(rt: &mut Runtime<Ui>) {
    let ctrl = Mods {
        cmd: true,
        ..Mods::default()
    };
    chord(rt, Key::Command('e'), ctrl);
}

#[test]
fn ctrl_e_opens_it_and_escape_cancel_and_close_shut_it() {
    let mut rt = with_a_clip();
    let before = rt.state.editor.settings.clone();
    for close in ["escape", "export-cancel", "export-close"] {
        ctrl_e(&mut rt);
        assert!(shown(&rt, "export-dialog"), "Ctrl+E opens Export");
        if close == "escape" {
            press(&mut rt, Key::Escape);
        } else {
            click(&mut rt, close);
        }
        assert!(!shown(&rt, "export-dialog"), "{close} closes it");
    }
    assert_eq!(rt.state.editor.settings, before, "nothing was picked");

    // The Export button opens it too.
    click(&mut rt, "export");
    assert!(shown(&rt, "export-dialog"));
}

#[test]
fn an_empty_timeline_says_so_instead() {
    let mut rt = window(Ui::headless());
    ctrl_e(&mut rt);
    assert!(!shown(&rt, "export-dialog"));
    assert_eq!(rt.state.editor.status, "Nothing to export.");
}

#[test]
fn each_pick_is_saved_and_changes_what_it_makes() {
    isolated_config(
        "export::tests::each_pick_is_saved_and_changes_what_it_makes",
        |_| {
            let mut rt = with_a_clip();
            ctrl_e(&mut rt);
            // A title alone: Auto is 720p, at the project's 30 fps.
            let editor = &rt.state.editor;
            assert_eq!(
                editor.export_output(editor.settings.export),
                ((1280, 720), 30.0)
            );
            for key in [
                "export-resolution-2160p",
                "export-fps-60",
                "export-quality-higher",
            ] {
                input_arrives();
                click(&mut rt, key);
            }
            let picked = ExportSettings {
                resolution: Resolution::P2160,
                fps: Some(60),
                quality: Quality::Higher,
                ..ExportSettings::default()
            };
            assert_eq!(rt.state.editor.settings.export, picked);
            assert_eq!(Settings::load().export, picked, "saved");
            assert_eq!(rt.state.editor.export_output(picked), ((3840, 2160), 60.0));
            assert!(shown(&rt, "export-dialog"), "a pick keeps it open");

            // Audio and GIF have their own settings; the video's stay.
            let height = |rt: &Runtime<Ui>| rt.presented("export-dialog").unwrap().0[3];
            // The summary ends inside the settings, above the divider.
            let summary_fits = |rt: &Runtime<Ui>| {
                let ([_, y, _, h], _) = rt.presented("export-summary").unwrap();
                let ([_, top, _, tall], _) = rt.presented("export-settings").unwrap();
                y + h <= top + tall
            };
            assert!(summary_fits(&rt), "a video's summary fits");
            let video_height = height(&rt);
            for key in [
                "export-kind-audio",
                "export-audio-flac",
                "export-kind-gif",
                "export-gif-240p",
            ] {
                input_arrives();
                click(&mut rt, key);
                assert_eq!(height(&rt), video_height, "{key} keeps the dialog still");
                assert!(summary_fits(&rt), "after {key} the summary fits");
            }
            assert!(!shown(&rt, "export-resolution") && !shown(&rt, "export-audio"));
            let picked = ExportSettings {
                kind: ExportKind::Gif,
                audio: AudioFormat::Flac,
                gif: GifSize::P240,
                ..picked
            };
            assert_eq!(Settings::load().export, picked, "saved");
            assert_eq!(rt.state.editor.export_output(picked), ((428, 240), 15.0));
            assert!(shown(&rt, "export-summary"));
            assert_eq!(summary(&rt.state, picked), "428 × 240 · 15 fps · no sound");
        },
    );
}

#[test]
fn every_control_has_a_tooltip() {
    let mut rt = with_a_clip();
    ctrl_e(&mut rt);
    let mut keys = vec![
        "export-close".to_owned(),
        "export-cancel".to_owned(),
        "export-confirm".to_owned(),
    ];
    keys.extend(
        Resolution::ALL.map(|resolution| format!("export-resolution-{}", resolution.as_str())),
    );
    keys.extend(FRAME_RATES.map(|fps| format!("export-fps-{fps}")));
    keys.extend(Quality::ALL.map(|quality| format!("export-quality-{}", quality.as_str())));
    keys.extend(ExportKind::ALL.map(|kind| format!("export-kind-{}", kind.as_str())));
    let has_tips = |rt: &mut Runtime<Ui>, keys: Vec<String>| {
        for key in keys {
            let help = tooltip(rt, &key);
            assert!(
                help.is_some_and(|help| !help.is_empty()),
                "{key} has no tooltip"
            );
        }
    };
    has_tips(&mut rt, keys);
    click(&mut rt, "export-kind-audio");
    has_tips(
        &mut rt,
        AudioFormat::ALL
            .map(|format| format!("export-audio-{}", format.as_str()))
            .into(),
    );
    let save = tooltip(&mut rt, "export-confirm").unwrap();
    assert!(save.contains("MP3"), "{save}");
    click(&mut rt, "export-kind-gif");
    has_tips(
        &mut rt,
        GifSize::ALL
            .map(|gif| format!("export-gif-{}", gif.as_str()))
            .into(),
    );
    let small = tooltip(&mut rt, "export-gif-240p").unwrap();
    assert_eq!(small, "428 × 240");
    click(&mut rt, "export-kind-video");
    // The Auto resolution names the size it makes of this project.
    let auto = tooltip(&mut rt, "export-resolution-auto").unwrap();
    assert!(auto.contains("1280 × 720"), "{auto}");
}

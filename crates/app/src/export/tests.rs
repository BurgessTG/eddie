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
            };
            assert_eq!(rt.state.editor.settings.export, picked);
            assert_eq!(Settings::load().export, picked, "saved");
            assert_eq!(rt.state.editor.export_output(picked), ((3840, 2160), 60.0));
            assert!(shown(&rt, "export-dialog"), "a pick keeps it open");
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
    for key in keys {
        let help = tooltip(&mut rt, &key);
        assert!(
            help.is_some_and(|help| !help.is_empty()),
            "{key} has no tooltip"
        );
    }
    // The Auto resolution names the size it makes of this project.
    let auto = tooltip(&mut rt, "export-resolution-auto").unwrap();
    assert!(auto.contains("1280 × 720"), "{auto}");
}

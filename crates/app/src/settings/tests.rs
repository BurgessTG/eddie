use eddie_core::editor::settings::Settings;
use eddie_core::editor::sweep::transcribe::Provider;
use openreel_engine::Orientation;
use rune::{Key, Mods, Runtime};

use super::lists::provider_key;
use super::*;
use crate::testing::{
    chord, click, frames, input_arrives, isolated_config, press, shown, tooltip, window,
};
use crate::theme::FIELD_H;

/// The window with Settings opened by its shortcut.
fn settings() -> Runtime<Ui> {
    let mut rt = window(Ui::headless());
    open(&mut rt);
    rt
}

fn open(rt: &mut Runtime<Ui>) {
    let ctrl = Mods {
        cmd: true,
        ..Mods::default()
    };
    chord(rt, Key::Command(','), ctrl);
    assert!(shown(rt, "settings"), "Ctrl+, opens Settings");
}

#[test]
fn escape_enter_and_done_close_it_and_save_nothing() {
    isolated_config(
        "settings::tests::escape_enter_and_done_close_it_and_save_nothing",
        |path| {
            let mut rt = settings();
            let before = rt.state.editor.settings.clone();
            press(&mut rt, Key::Escape);
            assert!(!shown(&rt, "settings"), "Esc closes it");

            open(&mut rt);
            press(&mut rt, Key::Enter);
            assert!(!shown(&rt, "settings"), "Enter presses Done");

            open(&mut rt);
            click(&mut rt, "settings-done");
            assert!(!shown(&rt, "settings"), "Done closes it");

            assert_eq!(rt.state.editor.settings, before);
            assert!(!path.exists(), "nothing changed, so nothing is saved");
        },
    );
}

#[test]
fn a_change_applies_at_once_and_is_saved() {
    isolated_config(
        "settings::tests::a_change_applies_at_once_and_is_saved",
        |_| {
            let mut rt = settings();
            click(&mut rt, "settings-portrait");
            assert_eq!(rt.state.editor.timeline.orientation, Orientation::Portrait);
            assert_eq!(Settings::load().format, Orientation::Portrait);
            assert_eq!(rt.state.editor.status, "Settings saved.");

            click(&mut rt, "settings-language");
            assert!(rt.state.typing(), "plain keys are text in a field");
            rt.paste("en");
            // Done takes the keyboard from the field in the same press.
            click(&mut rt, "settings-done");
            assert!(!shown(&rt, "settings"));
            assert!(!rt.state.typing(), "a closed dialog keeps no keys");
            assert_eq!(Settings::load().transcription.language, "en");

            open(&mut rt);
            assert_eq!(rt.state.settings.edits[Field::Language as usize].text, "en");
        },
    );
}

#[test]
fn a_failed_save_is_shown() {
    isolated_config("settings::tests::a_failed_save_is_shown", |path| {
        // A directory where the file should be makes the rename fail.
        std::fs::create_dir_all(&path).unwrap();
        let mut rt = settings();
        click(&mut rt, "settings-hardware");
        let hardware = rt.state.editor.settings.hardware_decode;
        // The switch is process-wide; leave it as other tests expect.
        openreel_media::set_hardware_decode(false);
        assert!(hardware, "the change still applies to this session");
        assert_eq!(rt.state.editor.status, "Couldn't save settings.");
    });
}

#[test]
fn a_cloud_provider_asks_for_its_url_and_key() {
    isolated_config(
        "settings::tests::a_cloud_provider_asks_for_its_url_and_key",
        |_| {
            let mut rt = settings();
            assert!(!shown(&rt, "settings-url") && !shown(&rt, "settings-key"));

            click(&mut rt, "settings-provider");
            assert!(shown(&rt, "settings-provider-openai"));
            press(&mut rt, Key::Escape);
            assert!(
                !shown(&rt, "settings-provider-openai"),
                "Esc shuts the list"
            );
            assert!(shown(&rt, "settings"), "and leaves the dialog open");

            click(&mut rt, "settings-provider");
            click(&mut rt, "settings-provider-openai");
            assert!(!shown(&rt, "settings-provider-openai"), "a choice shuts it");
            assert_eq!(Settings::load().transcription.provider, Provider::OpenAi);
            assert!(shown(&rt, "settings-url") && shown(&rt, "settings-key"));

            click(&mut rt, "settings-key");
            rt.paste("fixture-key-only");
            press(&mut rt, Key::Escape);
            assert_eq!(Settings::load().transcription.api_key, "fixture-key-only");
        },
    );
}

/// A model folder beside the isolated config file, holding empty files.
fn model_folder(config: &std::path::Path, files: &[&str]) -> std::path::PathBuf {
    let folder = config.with_file_name("test-models");
    std::fs::create_dir_all(&folder).unwrap();
    for file in files {
        std::fs::write(folder.join(file), b"").unwrap();
    }
    folder
}

#[test]
fn the_local_model_is_picked_from_the_installed_ones() {
    isolated_config(
        "settings::tests::the_local_model_is_picked_from_the_installed_ones",
        |path| {
            let mut ui = Ui::headless();
            ui.editor.model_dir = Some(model_folder(
                &path,
                &[
                    "ggml-base.bin",
                    "ggml-large-v3-turbo.bin",
                    "ggml-silero-v5.1.2.bin",
                ],
            ));
            let mut rt = window(ui);
            open(&mut rt);
            assert!(
                shown(&rt, "settings-local-model") && !shown(&rt, "settings-model"),
                "a local model is picked, not typed"
            );

            click(&mut rt, "settings-local-model");
            for key in [
                "settings-best-model",
                "settings-model-large-v3-turbo",
                "settings-model-base",
            ] {
                assert!(
                    tooltip(&mut rt, key).is_some_and(|words| !words.is_empty()),
                    "{key} has no tooltip"
                );
            }
            assert!(
                !shown(&rt, "settings-model-silero-v5.1.2"),
                "the voice detector is not a speech model"
            );
            assert!(!shown(&rt, "settings-other-model"));
            press(&mut rt, Key::Escape);
            assert!(!shown(&rt, "settings-best-model"), "Esc shuts the list");
            assert!(shown(&rt, "settings"), "and leaves the dialog open");
            assert!(!path.exists(), "looking saves nothing");

            click(&mut rt, "settings-local-model");
            click(&mut rt, "settings-model-base");
            assert!(!shown(&rt, "settings-model-base"), "a choice shuts it");
            assert_eq!(Settings::load().transcription.model, "base");

            click(&mut rt, "settings-local-model");
            click(&mut rt, "settings-best-model");
            assert_eq!(Settings::load().transcription.model, "");
        },
    );
}

#[test]
fn a_saved_model_outside_the_folder_stays_a_choice() {
    isolated_config(
        "settings::tests::a_saved_model_outside_the_folder_stays_a_choice",
        |path| {
            let mut ui = Ui::headless();
            ui.editor.model_dir = Some(model_folder(&path, &["ggml-base.bin"]));
            ui.editor.settings.transcription.model = "/models/custom.bin".into();
            let mut rt = window(ui);
            open(&mut rt);
            assert!(
                tooltip(&mut rt, "settings-local-model").is_some_and(|words| !words.is_empty())
            );
            click(&mut rt, "settings-local-model");
            assert!(tooltip(&mut rt, "settings-other-model").is_some());
            click(&mut rt, "settings-other-model");
            assert!(!shown(&rt, "settings-other-model"));
            assert_eq!(
                rt.state.editor.settings.transcription.model,
                "/models/custom.bin"
            );
            assert!(!path.exists(), "keeping the choice saves nothing");

            click(&mut rt, "settings-local-model");
            click(&mut rt, "settings-model-base");
            click(&mut rt, "settings-local-model");
            assert!(
                !shown(&rt, "settings-other-model"),
                "only the saved choice is listed from outside the folder"
            );
        },
    );
}

#[test]
fn a_long_model_list_scrolls_inside_the_window() {
    isolated_config(
        "settings::tests::a_long_model_list_scrolls_inside_the_window",
        |path| {
            let names: Vec<String> = (10..30).map(|n| format!("ggml-model-{n}.bin")).collect();
            let files: Vec<&str> = names.iter().map(String::as_str).collect();
            let mut ui = Ui::headless();
            ui.editor.model_dir = Some(model_folder(&path, &files));
            let mut rt = window(ui);
            open(&mut rt);
            click(&mut rt, "settings-local-model");

            // 21 choices in a list eight rows tall, inside the window.
            let ([_, top, _, height], _) = rt.presented("settings-models").unwrap();
            let rows = MODEL_ROWS as f32;
            assert_eq!(height, rows * FIELD_H + (rows - 1.0) * OPTION_GAP + 14.0);
            assert!(top >= 0.0 && top + height <= 900.0, "{top} + {height}");
            let (_, last) = rt.find("settings-model-model-29").unwrap();
            assert!(last > top + height, "the last choice starts out of sight");

            let (x, y) = rt.find("settings-models-scroll").unwrap();
            input_arrives();
            rt.wheel(x, y, -2000.0);
            frames(&mut rt, 3);
            let (_, last) = rt.find("settings-model-model-29").unwrap();
            assert!(last > top && last < top + height, "the wheel reaches it");
            click(&mut rt, "settings-model-model-29");
            assert_eq!(Settings::load().transcription.model, "model-29");
        },
    );
}

#[test]
fn every_setting_explains_itself() {
    isolated_config("settings::tests::every_setting_explains_itself", |path| {
        let mut ui = Ui::headless();
        // Set here, not through the dialog, so the cloud rows show and
        // nothing is saved.
        ui.editor.settings.transcription.provider = Provider::OpenAi;
        let mut rt = window(ui);
        open(&mut rt);
        for key in [
            "settings-close",
            "settings-hardware",
            "settings-provider",
            "settings-model",
            "settings-url",
            "settings-key",
            "settings-language",
            "settings-vocabulary",
            "settings-landscape",
            "settings-portrait",
            "settings-done",
        ] {
            assert!(
                tooltip(&mut rt, key).is_some_and(|words| !words.is_empty()),
                "{key} has no tooltip"
            );
        }
        click(&mut rt, "settings-provider");
        for provider in Provider::ALL {
            let key = provider_key(provider);
            assert!(
                tooltip(&mut rt, &key).is_some_and(|words| !words.is_empty()),
                "{key} has no tooltip"
            );
        }
        assert!(!path.exists(), "looking saves nothing");
    });
}

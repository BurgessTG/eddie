use std::time::{Duration, Instant};

use eddie_core::editor::settings::Settings;
use rune::{Key, Runtime};

use super::meter_fill;
use crate::app::Ui;
use crate::testing::{click, frames, isolated_config, press, shown, tooltip, window};

/// Frames until `done`, giving the import thread real time.
fn until(rt: &mut Runtime<Ui>, what: &str, done: impl Fn(&Ui) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(&rt.state) {
        assert!(
            Instant::now() < deadline,
            "{what}: {}",
            rt.state.editor.status
        );
        frames(rt, 1);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The panel records with the test microphone, which hears a tone; the
/// take grows on the lanes, then lands as a clip.
#[test]
fn the_microphone_opens_a_panel_whose_record_button_records_a_take() {
    isolated_config(
        "voiceover::tests::the_microphone_opens_a_panel_whose_record_button_records_a_take",
        |config| {
            let dir = config.parent().unwrap().join("project");
            std::fs::create_dir_all(&dir).unwrap();
            let mut ui = Ui::headless();
            ui.editor.save_project_to(&dir.join("talk")).unwrap();
            let mut rt = window(ui);

            let tip = tooltip(&mut rt, "tool-voiceover").unwrap_or_default();
            assert!(tip.contains("Voiceover") && tip.ends_with("(R)"), "{tip}");
            click(&mut rt, "tool-voiceover");
            assert!(shown(&rt, "voiceover-panel"));
            assert!(
                rt.state.editor.voiceover().is_some(),
                "the meter's microphone"
            );
            for key in [
                "voiceover-record",
                "voiceover-hear",
                "voiceover-close",
                "voiceover-meter",
                "voiceover-input",
            ] {
                assert!(tooltip(&mut rt, key).is_some(), "{key} has a tooltip");
            }
            // The meter shows the tone at -12 dB.
            frames(&mut rt, 5);
            let level = rt.state.editor.voiceover().unwrap().meter;
            assert!((meter_fill(level) - 0.8).abs() < 0.02, "{level}");

            // The switch is saved.
            click(&mut rt, "voiceover-hear");
            assert!(rt.state.editor.settings.hear_timeline_while_recording);
            assert!(Settings::load().hear_timeline_while_recording, "saved");

            click(&mut rt, "voiceover-record");
            assert_eq!(rt.state.editor.voiceover().unwrap().countdown(), Some(3));
            frames(&mut rt, 200);
            let take = rt.state.editor.voiceover().unwrap().recording();
            assert!(take.is_some(), "{}", rt.state.editor.status);
            frames(&mut rt, 90);
            assert!(shown(&rt, "take-recording"), "the take grows on the lanes");
            // Esc stops the take before it closes the panel.
            press(&mut rt, Key::Escape);
            assert!(!rt.state.editor.voiceover_busy(), "Esc stops the take");
            assert!(shown(&rt, "voiceover-panel"));
            until(&mut rt, "the take's clip", |ui| {
                ui.editor.pending_takes().is_empty()
                    && ui
                        .editor
                        .timeline
                        .clips
                        .iter()
                        .any(|c| c.name == "Voiceover 1")
            });
            frames(&mut rt, 2);
            let clip = &rt.state.editor.timeline.clips[0];
            assert!(
                clip.duration > 1.0 && clip.duration < 2.0,
                "{}",
                clip.duration
            );
            assert!(
                shown(&rt, &format!("clip-{}", clip.id)),
                "the clip replaces the take"
            );
            assert!(!shown(&rt, "take-recording"));
            assert!(dir.join("talk voiceovers/Voiceover 1.wav").exists());

            // R starts another take from the keyboard; R stops it.
            press(&mut rt, Key::Text("r".into()));
            assert_eq!(rt.state.editor.voiceover().unwrap().countdown(), Some(3));
            press(&mut rt, Key::Text("r".into()));
            assert!(!rt.state.editor.voiceover_busy(), "R cancels the countdown");

            press(&mut rt, Key::Escape);
            assert!(!shown(&rt, "voiceover-panel"));
            frames(&mut rt, 2);
            assert!(
                rt.state.editor.voiceover().is_none(),
                "the microphone closes"
            );
        },
    );
}

#[test]
fn the_meter_spans_sixty_decibels() {
    assert_eq!(meter_fill(0.0), 0.0);
    assert_eq!(meter_fill(0.0005), 0.0, "below -60 dB is empty");
    assert!((meter_fill(0.1) - 2.0 / 3.0).abs() < 1e-6, "-20 dB");
    assert_eq!(meter_fill(1.0), 1.0);
    assert_eq!(meter_fill(2.0), 1.0);
}

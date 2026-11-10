//! What the window's tests share: a headless window over a `Ui` and the
//! pointer and key input a person would give it.

use std::path::PathBuf;

use rune::{Env, Key, KeyEvent, Mods, Runtime};

use crate::app::{self, Ui};

pub const FRAME: f64 = 1.0 / 60.0;

/// A 1400 by 900 window over `ui`, polled and keyed as the real one is.
pub fn window(ui: Ui) -> Runtime<Ui> {
    let mut rt = Runtime::new(ui, app::view, (1400.0, 900.0), Env::new(true, 1.0));
    rt.poll = Some(app::poll);
    rt.on_key = Some(app::on_key);
    frames(&mut rt, 3);
    rt
}

pub fn frames(rt: &mut Runtime<Ui>, count: usize) {
    for _ in 0..count {
        rt.frame(FRAME);
    }
}

pub fn shown(rt: &Runtime<Ui>, key: &str) -> bool {
    rt.find(key).is_some()
}

pub fn click(rt: &mut Runtime<Ui>, key: &str) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("{key} is not shown"));
    rt.pointer_move(x, y);
    rt.frame(FRAME);
    rt.pointer_down(x, y);
    rt.frame(FRAME);
    rt.pointer_up(x, y);
    frames(rt, 2);
}

pub fn press(rt: &mut Runtime<Ui>, key: Key) {
    chord(rt, key, Mods::default());
}

pub fn chord(rt: &mut Runtime<Ui>, key: Key, mods: Mods) {
    rt.key(KeyEvent { key, mods });
    frames(rt, 2);
}

/// The tooltip a control shows once the pointer rests on it.
pub fn tooltip(rt: &mut Runtime<Ui>, key: &str) -> Option<String> {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("{key} is not shown"));
    rt.help_delay = 0.0;
    // Come from outside the window, so an earlier tooltip is not this one.
    rt.pointer_leave();
    rt.frame(FRAME);
    rt.pointer_move(x, y);
    frames(rt, 2);
    rt.help_tooltip().map(|(words, _)| words.to_owned())
}

/// A config folder removed with the test.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "eddie-window-settings-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs `run` in a child test process whose config folder is a fresh
/// temporary one. Changing a setting saves it, so a test that opens
/// Settings runs here and never writes the user's file. `test` is the
/// test's full path, such as `settings::tests::done_closes_it`; `run`
/// gets the path of the config file.
pub fn isolated_config(test: &str, run: impl FnOnce(PathBuf)) {
    const MARK: &str = "OPENREEL_ISOLATED_SETTINGS_TEST";
    if std::env::var(MARK).as_deref() == Ok(test) {
        run(eddie_core::editor::settings::config_path().expect("isolated settings path"));
        return;
    }
    let dir = TestDir::new();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(MARK, test)
        .env("HOME", &dir.0)
        .env("XDG_CONFIG_HOME", &dir.0)
        .env("APPDATA", &dir.0)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "isolated settings test failed or did not run:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
}

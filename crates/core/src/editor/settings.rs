//! App settings: project format, hardware decode and the transcription
//! provider, saved privately beside curations and transcripts.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use openreel_engine::Orientation;

use super::sweep::transcribe::{Provider, TranscribeSettings};
use crate::update::Channel;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// Picture format for new and cleared projects; Settings also
    /// applies it to the open project.
    pub format: Orientation,
    /// Preview playback of expensive files (HEVC, AV1, VP9, above 1440p)
    /// decodes on the GPU (VAAPI, or VideoToolbox on a Mac) when one is
    /// available. Off by default: errors fall back to software, but a
    /// driver that hangs cannot, and only one NVIDIA driver has been tested.
    pub hardware_decode: bool,
    /// Where speech becomes text: local whisper by default, or a cloud API.
    pub transcription: TranscribeSettings,
    /// Which published updates the window looks for.
    pub updates: Channel,
    /// The Meta key and endpoint the removed built-in AI saved at the top
    /// level (`api_key`, `base_url`). Kept so Meta transcription without its
    /// own key still works; there is no field for them any more.
    meta_api_key: String,
    meta_base_url: String,
}

/// The settings folder's name, and the name it had while the editor was
/// called OpenReel.
#[cfg(all(unix, not(target_os = "macos")))]
const FOLDER: (&str, &str) = ("eddie", "openreel");
#[cfg(not(all(unix, not(target_os = "macos"))))]
const FOLDER: (&str, &str) = ("Eddie", "OpenReel");

/// Where the system keeps each program's settings folder.
fn config_home() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }
    #[cfg(not(any(unix, target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

/// The settings folder and the old one it replaces.
fn config_dirs() -> Option<(PathBuf, PathBuf)> {
    let home = config_home()?;
    Some((home.join(FOLDER.0), home.join(FOLDER.1)))
}

/// The settings file. Saved curations, transcripts and local models sit
/// beside it. A folder still under its old name is used where it is.
pub fn config_path() -> Option<PathBuf> {
    let (dir, old) = config_dirs()?;
    let dir = if !dir.exists() && old.is_dir() {
        old
    } else {
        dir
    };
    Some(dir.join("config.json"))
}

/// Give a settings folder still under its old name the new one, with
/// everything in it. The window and the command line do this as they
/// start; a folder that cannot be moved keeps being read where it is, and
/// one that already has the new name is never replaced.
pub fn adopt_old_config() {
    if let Some((dir, old)) = config_dirs() {
        if !dir.exists() && old.is_dir() {
            let _ = std::fs::rename(old, dir);
        }
    }
}

/// Where a config that could not be read is moved when settings are saved.
const UNREADABLE: &str = "config.unreadable.json";

impl Settings {
    pub fn load() -> Self {
        let Some(value) = config_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        else {
            return Self::default();
        };
        let text = |key: &str| value[key].as_str().unwrap_or_default().trim().to_owned();
        Self {
            format: value["format"]
                .as_str()
                .and_then(Orientation::from_name)
                .unwrap_or_default(),
            hardware_decode: value["hardware_decode"].as_bool().unwrap_or(false),
            transcription: TranscribeSettings::from_json(&value["transcription"]),
            updates: value["updates"]
                .as_str()
                .and_then(Channel::from_name)
                .unwrap_or_default(),
            meta_api_key: text("api_key"),
            meta_base_url: text("base_url"),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let save_error = || "Couldn't save settings.".to_owned();
        let path = config_path().ok_or_else(save_error)?;
        let parent = path.parent().ok_or_else(save_error)?;
        std::fs::create_dir_all(parent).map_err(|_| save_error())?;
        let mut body = serde_json::json!({
            "format": self.format.as_str(),
            "hardware_decode": self.hardware_decode,
            "transcription": self.transcription.to_json(),
            "updates": self.updates.as_str(),
        });
        if !self.meta_api_key.is_empty() {
            body["api_key"] = self.meta_api_key.clone().into();
        }
        if !self.meta_base_url.is_empty() {
            body["base_url"] = self.meta_base_url.clone().into();
        }
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        let (temporary, mut file) = loop {
            let temporary = parent.join(format!(
                ".config-{}-{}.tmp",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
            ));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&temporary) {
                Ok(file) => break (temporary, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(save_error()),
            }
        };
        let result = (|| -> std::io::Result<()> {
            file.write_all(body.to_string().as_bytes())?;
            file.sync_all()?;
            drop(file);
            // A config that does not parse loaded as defaults. Whatever it
            // held, a key say, is kept beside the new file, not replaced.
            let damaged = std::fs::read(&path).is_ok_and(|raw| {
                !raw.trim_ascii().is_empty()
                    && serde_json::from_slice::<serde_json::Value>(&raw).is_err()
            });
            if damaged {
                std::fs::rename(&path, parent.join(UNREADABLE))?;
            }
            std::fs::rename(&temporary, &path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result.map_err(|_| save_error())
    }

    /// Whether a Meta key saved by the removed built-in AI is available to
    /// Meta transcription.
    pub fn has_meta_key(&self) -> bool {
        !self.meta_api_key.is_empty()
    }

    /// Transcription settings, with Meta falling back to the saved Meta key
    /// and endpoint when it has none of its own, and local models looked up
    /// in `model_dir`.
    pub fn transcription(&self, model_dir: Option<&Path>) -> TranscribeSettings {
        let mut settings = self.transcription.clone();
        settings.model_dir = model_dir.map(Path::to_path_buf);
        if settings.provider == Provider::Meta {
            if settings.api_key.is_empty() {
                settings.api_key.clone_from(&self.meta_api_key);
            }
            if settings.base_url.is_empty() {
                settings.base_url.clone_from(&self.meta_base_url);
            }
        }
        settings
    }
}

impl super::Editor {
    /// The Settings window edited `self.settings`, which was `before`. The
    /// running editor takes the format and decode choice, the file is
    /// saved, and the status says what happened.
    pub fn settings_changed(&mut self, before: &Settings) {
        if self.settings == *before {
            return;
        }
        let mut clashing = 0;
        if self.settings.format != before.format {
            clashing = self.set_format(self.settings.format);
        }
        if self.settings.hardware_decode != before.hardware_decode {
            openreel_media::set_hardware_decode(self.settings.hardware_decode);
        }
        match self.settings.save() {
            Err(error) => self.status = error,
            // Clips the new format leaves out of place are what to read.
            Ok(()) if clashing > 0 => {}
            Ok(()) => self.status = "Settings saved.".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "openreel-settings-test-{}-{nonce}",
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

    /// Runs `run` in a child test process whose config directory is a fresh
    /// temporary one, so tests never touch the user's settings.
    fn with_isolated_config(test: &str, run: impl FnOnce(PathBuf)) {
        if std::env::var("OPENREEL_ISOLATED_SETTINGS_TEST").as_deref() == Ok(test) {
            run(config_path().expect("isolated settings path"));
            return;
        }
        let dir = TestDir::new();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env("OPENREEL_ISOLATED_SETTINGS_TEST", test)
            .env("HOME", &dir.0)
            .env("XDG_CONFIG_HOME", &dir.0)
            .env("APPDATA", &dir.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated settings test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[test]
    fn a_folder_under_the_old_name_is_read_then_renamed_once() {
        with_isolated_config(
            "editor::settings::tests::a_folder_under_the_old_name_is_read_then_renamed_once",
            |path| {
                let (new, old) = config_dirs().unwrap();
                assert_eq!(path, new.join("config.json"));
                #[cfg(target_os = "linux")]
                assert!(new.ends_with("eddie") && old.ends_with("openreel"));

                let save_old = |format: &str| {
                    std::fs::create_dir_all(old.join("whisper")).unwrap();
                    std::fs::write(old.join("whisper/model.bin"), "model").unwrap();
                    std::fs::write(
                        old.join("config.json"),
                        format!(r#"{{"format":"{format}"}}"#),
                    )
                    .unwrap();
                };
                save_old("portrait");
                // Read where it is until something moves it.
                assert_eq!(config_path().unwrap(), old.join("config.json"));
                assert_eq!(Settings::load().format, Orientation::Portrait);

                adopt_old_config();
                assert!(!old.exists(), "the whole folder moved");
                assert_eq!(config_path().unwrap(), new.join("config.json"));
                assert_eq!(Settings::load().format, Orientation::Portrait);
                assert_eq!(
                    std::fs::read_to_string(new.join("whisper/model.bin")).unwrap(),
                    "model"
                );

                // An older program writing the old folder again changes nothing.
                save_old("landscape");
                adopt_old_config();
                assert!(old.exists());
                assert_eq!(Settings::load().format, Orientation::Portrait);
            },
        );
    }

    #[test]
    fn settings_save_atomically_replaces_previous_file() {
        with_isolated_config(
            "editor::settings::tests::settings_save_atomically_replaces_previous_file",
            |path| {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                let original = r#"{"format":"portrait"}"#;
                std::fs::write(&path, original).unwrap();
                let previous = path.with_file_name("previous.json");
                std::fs::hard_link(&path, &previous).unwrap();
                Settings::default().save().unwrap();
                assert_eq!(Settings::load().format, Orientation::Landscape);
                assert_eq!(
                    std::fs::read_to_string(previous).unwrap(),
                    original,
                    "a reader of the previous file must never see a partial replacement"
                );
            },
        );
    }

    #[test]
    fn settings_round_trip_privately_and_keep_the_saved_meta_key() {
        with_isolated_config(
            "editor::settings::tests::settings_round_trip_privately_and_keep_the_saved_meta_key",
            |path| {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                // A config written by the removed built-in AI.
                std::fs::write(
                    &path,
                    serde_json::json!({"api_key": "fixture-key-only",
                        "base_url": "https://api.meta.ai/v1", "model": "muse-wonder-1.0",
                        "check_edits": true, "contributor_opt_in": false})
                    .to_string(),
                )
                .unwrap();
                let mut settings = Settings::load();
                assert_eq!(
                    settings.transcription(None).api_key,
                    "",
                    "local needs no key"
                );
                settings.transcription.provider = Provider::Meta;
                assert_eq!(settings.transcription(None).api_key, "fixture-key-only");
                assert_eq!(
                    settings.transcription(None).base_url,
                    "https://api.meta.ai/v1"
                );
                assert_eq!(settings.updates, Channel::Releases, "the default");
                for (format, hardware, updates) in [
                    (Orientation::Landscape, false, Channel::Previews),
                    (Orientation::Portrait, true, Channel::Off),
                ] {
                    settings.format = format;
                    settings.hardware_decode = hardware;
                    settings.updates = updates;
                    settings.save().unwrap();
                    assert_eq!(Settings::load(), settings, "settings round-trip");
                    let value: serde_json::Value =
                        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                    let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
                    keys.sort();
                    assert_eq!(
                        keys,
                        [
                            "api_key",
                            "base_url",
                            "format",
                            "hardware_decode",
                            "transcription",
                            "updates"
                        ],
                        "the removed model settings are dropped, the Meta key is kept"
                    );
                    assert_eq!(value["transcription"]["provider"], "meta");
                    assert_eq!(value["transcription"]["api_key"], "", "not copied");
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        assert_eq!(
                            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                            0o600,
                        );
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                            .unwrap();
                    }
                }
                assert_eq!(
                    std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                    1
                );
            },
        );
    }

    #[test]
    fn saving_settings_keeps_a_config_that_could_not_be_read() {
        with_isolated_config(
            "editor::settings::tests::saving_settings_keeps_a_config_that_could_not_be_read",
            |path| {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                // Cut short, as after a full disk: the key is still in it.
                let damaged = r#"{"api_key": "fixture-key-only", "format": "portr"#;
                std::fs::write(&path, damaged).unwrap();
                let settings = Settings::load();
                assert_eq!(settings, Settings::default());
                settings.save().unwrap();
                assert_eq!(Settings::load(), settings);
                let kept = path.with_file_name(UNREADABLE);
                assert_eq!(std::fs::read_to_string(kept).unwrap(), damaged);
            },
        );
    }

    #[test]
    fn a_format_change_is_saved_and_reports_clips_it_leaves_out_of_place() {
        with_isolated_config(
            "editor::settings::tests::a_format_change_is_saved_and_reports_clips_it_leaves_out_of_place",
            |_| {
                use crate::editor::media::{AssetState, BinAsset};
                let mut editor = crate::Editor::headless();
                editor.assets.push(BinAsset {
                    info: openreel_media::MediaInfo {
                        path: "/media/wide.mp4".to_owned(),
                        name: "wide.mp4".to_owned(),
                        duration: 10.0,
                        width: 1920,
                        height: 1080,
                        fps: 30.0,
                        has_video: true,
                        has_audio: true,
                    },
                    size_bytes: 0,
                    state: AssetState::Ready,
                    peaks: Vec::new(),
                    beats: None,
                    thumbs: Vec::new(),
                    sweep: Default::default(),
                });

                // Nothing placed: the change is saved and says only that.
                let before = editor.settings.clone();
                editor.settings.transcription.language = "en".to_owned();
                editor.settings_changed(&before);
                assert_eq!(editor.status, "Settings saved.");
                assert_eq!(Settings::load().transcription.language, "en");

                editor
                    .try_add_asset_range_to_timeline("/media/wide.mp4", "", None, 0.0, 0.0, 1.0)
                    .expect("landscape video fits the default project");
                let before = editor.settings.clone();
                editor.settings.format = Orientation::Portrait;
                editor.settings_changed(&before);
                assert_eq!(editor.timeline.orientation, Orientation::Portrait);
                assert_eq!(Settings::load().format, Orientation::Portrait);
                assert_eq!(editor.status, "1 clip doesn't match the new format.");

                // Undo takes the project back, and the setting shown with it.
                editor.commit_history();
                editor.perform(crate::Action::Undo);
                assert_eq!(editor.timeline.orientation, Orientation::Landscape);
                assert_eq!(editor.settings.format, Orientation::Landscape);
                editor.perform(crate::Action::Redo);
                assert_eq!(editor.timeline.orientation, Orientation::Portrait);
                assert_eq!(editor.settings.format, Orientation::Portrait);

                // An unchanged settings value writes and says nothing.
                editor.status = "untouched".to_owned();
                let before = editor.settings.clone();
                editor.settings_changed(&before);
                assert_eq!(editor.status, "untouched");
            },
        );
    }

    #[test]
    fn settings_save_failure_cleans_temporary_file() {
        with_isolated_config(
            "editor::settings::tests::settings_save_failure_cleans_temporary_file",
            |path| {
                // A directory where the file should be makes the rename fail.
                std::fs::create_dir_all(&path).unwrap();
                let previous = path.join("previous.json");
                std::fs::write(&previous, "keep this file").unwrap();
                let error = Settings::default().save().unwrap_err();
                assert_eq!(error, "Couldn't save settings.");
                assert_eq!(std::fs::read_to_string(previous).unwrap(), "keep this file");
                assert_eq!(
                    std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                    1
                );
            },
        );
    }
}

//! Editor behaviour, by topic. Each file reaches the editor and the names
//! imported here through `use super::*`.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::channel;
use std::thread;

use openreel_engine::Orientation;
use openreel_media::MediaInfo;

use super::export::ExportMsg;
use super::media::AssetState;
use super::*;
use crate::scripting::ScriptCommand;

mod audio;
mod curation;
mod playback;
mod project;
mod script;
mod timeline;

/// Generated sample media (video, music), made once per test run. Every
/// test shares the folder, so no test can remove it: the next run removes
/// the folders of runs whose process has gone.
fn sample_media() -> &'static [PathBuf] {
    const PREFIX: &str = "eddie-samples-";
    static SAMPLES: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    SAMPLES.get_or_init(|| {
        let temp = std::env::temp_dir();
        let running =
            |pid: &str| !Path::new("/proc/self").exists() || Path::new("/proc").join(pid).exists();
        for entry in std::fs::read_dir(&temp).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let pid = name.to_str().and_then(|name| name.strip_prefix(PREFIX));
            if pid.is_some_and(|pid| pid.parse::<u32>().is_ok() && !running(pid)) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        let dir = temp.join(format!("{PREFIX}{}", std::process::id()));
        openreel_media::generate_samples(&dir).expect("generate samples")
    })
}

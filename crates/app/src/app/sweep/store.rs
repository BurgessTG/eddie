//! Finished curations saved beside the settings file, one JSON file per
//! source. A result is restored only for the same path, size and
//! modification time, so an edited or replaced file is never matched.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::{SweepData, SweepStatus};

const VERSION: u64 = 1;

/// Where curations live for this config: `<config dir>/curation`.
pub(in crate::app) fn default_dir() -> Option<PathBuf> {
    Some(crate::app::chat::config_path()?.parent()?.join("curation"))
}

fn identity(path: &str) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((meta.len(), modified))
}

/// FNV-1a: stable across Rust releases, unlike `DefaultHasher`.
fn file_for(dir: &Path, path: &str) -> PathBuf {
    let hash = path.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    dir.join(format!("{hash:016x}.json"))
}

/// Write a finished curation. Failures only cost a later re-curate.
pub(in crate::app) fn save(dir: &Path, path: &str, data: &SweepData) -> Result<(), String> {
    let (size, modified) = identity(path).ok_or("Source file is unavailable")?;
    let body = json!({
        "version": VERSION,
        "path": path,
        "size": size,
        "modified": modified.to_string(),
        "data": data,
    });
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let target = file_for(dir, path);
    let temporary = target.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        // Transcripts are private like the settings file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(body.to_string().as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| error.to_string())
}

/// The saved curation for this exact file, if one exists.
pub(in crate::app) fn load(dir: &Path, path: &str) -> Option<SweepData> {
    let (size, modified) = identity(path)?;
    let raw = std::fs::read(file_for(dir, path)).ok()?;
    let mut value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    if value["version"].as_u64() != Some(VERSION)
        || value["path"].as_str() != Some(path)
        || value["size"].as_u64() != Some(size)
        || value["modified"].as_str() != Some(modified.to_string().as_str())
    {
        return None;
    }
    let mut data: SweepData = serde_json::from_value(value["data"].take()).ok()?;
    (data.status == SweepStatus::Ready).then(|| {
        data.note = format!("{} Restored from a saved curation.", data.note.trim())
            .trim()
            .to_owned();
        data
    })
}

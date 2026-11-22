//! Files export leaves behind only while it runs.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::MediaResult;

/// A file FFmpeg reads while export runs: a filter graph (FFmpeg 9's
/// -/filter_complex reads an argument file, and a 500-shot edit can exceed
/// the OS limit for a single argv string even with just one input) or a
/// mask's matte. Create exclusively, close before spawning (also on
/// Windows), and unlink on every return path, including write, spawn, and
/// encoder errors.
pub(super) struct FilterGraphFile(pub(super) PathBuf);

impl FilterGraphFile {
    pub(super) fn create(graph: &str) -> MediaResult<Self> {
        Self::write("ffgraph", graph.as_bytes())
    }

    /// A file named `….{extension}` holding `bytes`.
    pub(super) fn write(extension: &str, bytes: &[u8]) -> MediaResult<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "openreel-export-{}-{}.{extension}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(mut file) => {
                    let graph_file = Self(path);
                    let written = file.write_all(bytes);
                    drop(file);
                    written.map_err(|e| format!("write export {extension} file: {e}"))?;
                    return Ok(graph_file);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("create export {extension} file: {error}")),
            }
        }
    }
}

impl Drop for FilterGraphFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Private scratch space beside the output, so segments land on the same
/// disk as the export rather than in a RAM-backed temp directory. Removed
/// on every return path.
pub(crate) struct ScratchDir(pub(crate) PathBuf);

impl ScratchDir {
    pub(crate) fn beside(output: &Path) -> MediaResult<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let parent = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = output
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        loop {
            let path = parent.join(format!(
                ".{name}.export-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("export failed: create scratch folder: {error}")),
            }
        }
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

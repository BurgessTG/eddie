//! Room on the GPU for a local whisper model.
//!
//! whisper.cpp kills the process when the GPU runs out of memory while a
//! model starts: a segfault inside `whisper_init_state`, with CUDA and with
//! Vulkan (seen with a game holding 2.9 GB of an 8 GB card). So a model
//! goes on the GPU only when it is expected to fit. The check cannot see
//! memory another program takes afterwards.

use std::path::Path;

/// Apple Silicon builds use Metal without a feature: every such Mac has it
/// and it needs no library the system lacks (see the crate's `Cargo.toml`).
const METAL: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));

/// Whether this build runs whisper on a GPU (`cuda` or `vulkan` feature,
/// or Metal).
pub const BUILD: bool = cfg!(any(feature = "cuda", feature = "vulkan")) || METAL;

/// What this build runs local whisper on, for `eddie --version`.
pub const BACKEND: &str = if cfg!(feature = "cuda") {
    "cuda"
} else if cfg!(feature = "vulkan") {
    "vulkan"
} else if METAL {
    "metal"
} else {
    "cpu"
};

/// Bytes on the GPU whisper.cpp uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory {
    pub free: u64,
    pub total: u64,
}

impl Memory {
    /// Whether a model file is expected to run in what is free. A GPU that
    /// does not report its memory is taken to have room.
    pub fn fits(self, model: &Path) -> bool {
        let bytes = std::fs::metadata(model).map_or(0, |file| file.len());
        self.total == 0 || need(bytes) <= self.free
    }
}

/// GPU memory to ask for before a model starts, from its file size: one and
/// a half times the file plus 512 MiB. On an RTX 2070 SUPER large-v3 (a
/// 2.9 GiB file) took 4.6 GiB with Vulkan, and turbo (1.5 GiB) 2.2 GiB
/// with Vulkan and with CUDA. Both backends report less free memory than
/// `nvidia-smi` does (3.0 GiB against 3.5 GiB), which adds to the margin.
pub fn need(file_bytes: u64) -> u64 {
    file_bytes + file_bytes / 2 + (512 << 20)
}

/// `1.5 GB`, for messages.
pub fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
}

/// Memory on the GPU whisper.cpp will use: the first GPU ggml lists, which
/// is how `whisper_backend_init_gpu` picks device 0. `None` on a CPU build
/// and where no GPU is usable; whisper.cpp runs on the CPU then. Call it
/// after `install_logging_hooks`, since listing devices logs.
#[cfg(any(
    feature = "cuda",
    feature = "vulkan",
    all(target_os = "macos", target_arch = "aarch64")
))]
pub fn memory() -> Option<Memory> {
    use whisper_rs::whisper_rs_sys as sys;
    // SAFETY: ggml owns its device list for the life of the process, the
    // handles come straight from it, and the two out-pointers are live.
    unsafe {
        let device = (0..sys::ggml_backend_dev_count())
            .map(|index| sys::ggml_backend_dev_get(index))
            .find(|device| {
                matches!(
                    sys::ggml_backend_dev_type(*device),
                    sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU
                        | sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU
                )
            })?;
        let (mut free, mut total) = (0usize, 0usize);
        sys::ggml_backend_dev_memory(device, &mut free, &mut total);
        Some(Memory {
            free: free as u64,
            total: total as u64,
        })
    }
}

#[cfg(not(any(
    feature = "cuda",
    feature = "vulkan",
    all(target_os = "macos", target_arch = "aarch64")
)))]
pub fn memory() -> Option<Memory> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    /// The sizes of the real model files, against what they were measured
    /// to take and the cards they should still run on.
    #[test]
    fn need_covers_measured_use_without_shutting_out_small_cards() {
        let large_v3 = need(3_095_033_483);
        let turbo = need(1_624_555_275);
        assert!(large_v3 > 4_710 * MIB, "Vulkan took 4,710 MiB");
        assert!(turbo > 2_202 * MIB, "Vulkan took 2,202 MiB");
        // A 6 GB card under a desktop holding 1 GiB still takes large-v3,
        // and a 4 GB one turbo.
        assert!(large_v3 < 5_100 * MIB);
        assert!(turbo < 3_000 * MIB);
    }

    #[test]
    fn a_gpu_that_reports_no_memory_is_taken_to_have_room() {
        let missing = Path::new("/nonexistent/ggml-large-v3.bin");
        let unknown = Memory { free: 0, total: 0 };
        let full = Memory {
            free: 0,
            total: 8 << 30,
        };
        assert!(unknown.fits(missing));
        assert!(!full.fits(missing));
    }
}

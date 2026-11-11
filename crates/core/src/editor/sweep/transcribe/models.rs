//! Which whisper model runs: the installed ones, the best of them for
//! this build, and whether one fits on the GPU.

use std::path::{Path, PathBuf};

use super::super::gpu;
use super::local::VAD_MODEL;
use super::TranscribeSettings;

/// Where the user's local models are installed: `EDDIE_WHISPER_DIR`, else
/// `whisper/` beside the settings file. Only the window and the CLI call
/// this, so the environment never reaches an editor built for a test.
pub fn installed_model_dir() -> Option<PathBuf> {
    std::env::var_os("EDDIE_WHISPER_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            Some(
                crate::editor::settings::config_path()?
                    .parent()?
                    .join("whisper"),
            )
        })
}

/// Most accurate first, for what runs in reasonable time on a GPU or on
/// the CPU: there large-v3 takes several times longer than turbo for a
/// small accuracy gain, so turbo leads.
fn preferred_models(on_gpu: bool) -> &'static [&'static str] {
    if on_gpu {
        &[
            "large-v3",
            "large-v3-turbo",
            "large-v3-turbo-q5_0",
            "medium",
            "small",
            "base",
        ]
    } else {
        &[
            "large-v3-turbo",
            "large-v3-turbo-q5_0",
            "large-v3",
            "medium",
            "small",
            "base",
        ]
    }
}

pub(super) fn model_file(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("ggml-{name}.bin"))
}

/// Whether this build runs local models on a GPU when one has room.
pub fn gpu_build() -> bool {
    gpu::BUILD
}

/// The model an empty setting uses, by name: the first of this build's
/// preferred models installed in `dir`. A GPU build without room on the
/// GPU uses a smaller one (`place`).
pub fn best_installed(dir: Option<&Path>) -> Option<&'static str> {
    best_for(dir, gpu::BUILD)
}

fn best_for(dir: Option<&Path>, on_gpu: bool) -> Option<&'static str> {
    let dir = dir?;
    preferred_models(on_gpu)
        .iter()
        .copied()
        .find(|name| model_file(dir, name).is_file())
}

/// The whisper models installed in `dir`, by name (`large-v3` for
/// `ggml-large-v3.bin`): this build's preferred models first, in the
/// order `best_installed` tries them, then the rest by name. The voice
/// detector beside them is not one.
pub fn installed_models(dir: Option<&Path>) -> Vec<String> {
    let Some(entries) = dir.and_then(|dir| std::fs::read_dir(dir).ok()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().into_string().ok()?;
            let name = file.strip_prefix("ggml-")?.strip_suffix(".bin")?;
            (file != VAD_MODEL && entry.path().is_file()).then(|| name.to_owned())
        })
        .collect();
    let rank = |name: &str| {
        preferred_models(gpu::BUILD)
            .iter()
            .position(|preferred| *preferred == name)
            .unwrap_or(usize::MAX)
    };
    names.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    names
}

/// The model file to use: a configured name or path, else the best
/// installed for a GPU or for the CPU.
pub(super) fn local_model(settings: &TranscribeSettings, on_gpu: bool) -> Result<PathBuf, String> {
    let requested = settings.model.as_str();
    let dir = settings.model_dir.as_deref();
    let file = |name: &str| {
        dir.map(|dir| model_file(dir, name))
            .filter(|path| path.is_file())
    };
    if !requested.is_empty() {
        let path = PathBuf::from(requested);
        if path.is_file() {
            return Ok(path);
        }
        return file(requested).ok_or_else(|| missing_model(dir, requested));
    }
    best_for(dir, on_gpu)
        .and_then(file)
        .ok_or_else(|| missing_model(dir, preferred_models(on_gpu)[0]))
}

/// A local model and where it runs.
#[derive(Debug, PartialEq)]
pub(super) struct Placed {
    pub(super) model: PathBuf,
    pub(super) on_gpu: bool,
    /// Why this is not the model the build prefers.
    pub(super) note: Option<String>,
}

pub(super) fn model_name(model: &Path) -> String {
    model
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Choose the model and where it runs, given the GPU's memory (`None`
/// without a usable GPU). whisper.cpp crashes the process when a model
/// does not fit (`gpu`), so a chosen model that does not fit is refused,
/// and an empty setting takes the best installed model that fits, or the
/// CPU when none does.
pub(super) fn place(
    settings: &TranscribeSettings,
    memory: Option<gpu::Memory>,
) -> Result<Placed, String> {
    let Some(memory) = memory else {
        return Ok(Placed {
            model: local_model(settings, false)?,
            on_gpu: false,
            note: None,
        });
    };
    let short = |model: &Path| {
        let bytes = std::fs::metadata(model).map_or(0, |file| file.len());
        format!(
            "{} needs about {} of GPU memory and {} is free",
            model_name(model),
            gpu::gigabytes(gpu::need(bytes)),
            gpu::gigabytes(memory.free)
        )
    };
    let first = local_model(settings, true)?;
    if memory.fits(&first) {
        return Ok(Placed {
            model: first,
            on_gpu: true,
            note: None,
        });
    }
    if !settings.model.is_empty() {
        return Err(format!(
            "{}; close other programs using the GPU or choose a smaller model",
            short(&first)
        ));
    }
    let dir = settings.model_dir.as_deref();
    // small and base lose too many words to be worth their speed: when
    // nothing better fits, the CPU's best model is used instead.
    let smaller = preferred_models(true)
        .iter()
        .take_while(|name| **name != "small")
        .filter_map(|name| dir.map(|dir| model_file(dir, name)))
        .find(|model| model.is_file() && memory.fits(model));
    Ok(match smaller {
        Some(model) => Placed {
            note: Some(format!("{}; using {}", short(&first), model_name(&model))),
            model,
            on_gpu: true,
        },
        None => {
            let model = local_model(settings, false)?;
            Placed {
                note: Some(format!(
                    "{}; using {} on the CPU",
                    short(&first),
                    model_name(&model)
                )),
                model,
                on_gpu: false,
            }
        }
    })
}

fn missing_model(dir: Option<&Path>, name: &str) -> String {
    format!(
        "no whisper model ggml-{name}.bin in {}; download it from \
         https://huggingface.co/ggerganov/whisper.cpp",
        dir.map_or("the whisper folder".into(), |dir| dir.display().to_string())
    )
}

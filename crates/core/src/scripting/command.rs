//! What a script may ask for: the commands, their argument checks and
//! the queue one evaluation fills.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
pub enum ScriptCommand {
    Play,
    Pause,
    TogglePlay,
    Seek(f64),
    StepFrames(i64),
    SelectClip(u64),
    SplitClip {
        clip: u64,
        at: f64,
        /// Names the new right-hand piece for later commands in the run.
        handle: Option<u64>,
    },
    DeleteClip(u64),
    MoveClip {
        clip: u64,
        start: f64,
    },
    TrimClip {
        clip: u64,
        left: Option<f64>,
        right: Option<f64>,
    },
    RenameClip {
        clip: u64,
        name: String,
    },
    SetOpacity {
        clip: u64,
        value: f32,
    },
    SetVolume {
        clip: u64,
        value: f32,
    },
    SetCleanVoice {
        clip: u64,
        enabled: bool,
    },
    SetSmoothFades {
        clip: u64,
        enabled: bool,
    },
    SetFade {
        clip: u64,
        fade_in: f64,
        fade_out: f64,
    },
    DetachAudio {
        clip: u64,
    },
    UnlinkClip {
        clip: u64,
    },
    SetClipLane {
        clip: u64,
        lane: u64,
    },
    SetEmbeddedAudio {
        clip: u64,
        enabled: bool,
    },
    SetDuck {
        clip: u64,
        enabled: bool,
    },
    InsertTime {
        at: f64,
        seconds: f64,
    },
    TightenSilence {
        clip: u64,
        /// `None` is the source's own level (`silence_threshold`).
        threshold: Option<f32>,
        min_silence: f64,
        padding: f64,
    },
    AddTitle {
        text: String,
        at: f64,
        duration: f64,
        style: String,
        handle: Option<u64>,
    },
    SetTitleStyle {
        clip: u64,
        style: String,
    },
    AddCaptionsForClip {
        clip: u64,
        style: String,
    },
    AddAsset {
        asset: String,
        at: f64,
        handle: Option<u64>,
    },
    AddAssetRange {
        asset: String,
        at: f64,
        source_in: f64,
        duration: f64,
        handle: Option<u64>,
    },
    Import(String),
    Export(String),
    ClearTimeline,
    SetMagnet(bool),
    SetMusicBedMuted(bool),
    SetZoom(f32),
    CloseGaps,
    Log(String),
}

/// First handle that `addAsset`, `addAssetRange` and `addTitle` return.
/// Handles stand for clips a batch has not created yet; real clip IDs
/// never reach this range.
pub const CLIP_HANDLE_BASE: u64 = 1 << 40;

impl ScriptCommand {
    /// The clip this command targets, if any.
    pub fn target_mut(&mut self) -> Option<&mut u64> {
        match self {
            Self::SelectClip(id) | Self::DeleteClip(id) => Some(id),
            Self::SplitClip { clip, .. }
            | Self::MoveClip { clip, .. }
            | Self::TrimClip { clip, .. }
            | Self::RenameClip { clip, .. }
            | Self::SetOpacity { clip, .. }
            | Self::SetVolume { clip, .. }
            | Self::SetFade { clip, .. }
            | Self::SetCleanVoice { clip, .. }
            | Self::SetSmoothFades { clip, .. }
            | Self::DetachAudio { clip }
            | Self::UnlinkClip { clip }
            | Self::SetClipLane { clip, .. }
            | Self::SetEmbeddedAudio { clip, .. }
            | Self::SetDuck { clip, .. }
            | Self::TightenSilence { clip, .. }
            | Self::SetTitleStyle { clip, .. }
            | Self::AddCaptionsForClip { clip, .. } => Some(clip),
            _ => None,
        }
    }

    /// Replace a handle returned earlier in this batch with the clip its
    /// addition created. `created` maps handles to clip IDs.
    pub fn resolve_handle(
        &mut self,
        created: &std::collections::HashMap<u64, u64>,
    ) -> Result<(), String> {
        if let Some(id) = self.target_mut().filter(|id| **id >= CLIP_HANDLE_BASE) {
            *id = *created
                .get(id)
                .ok_or("that clip handle is not from an earlier addition in this run")?;
        }
        Ok(())
    }

    /// Validate numeric inputs without changing finite values or clamping policy.
    pub fn validate(&self) -> Result<(), String> {
        // A title's name is drawn by FFmpeg, whose arguments end at a NUL.
        if let Self::AddTitle { text, .. } | Self::RenameClip { name: text, .. } = self {
            if text.chars().any(|c| c.is_control() && !c.is_whitespace()) {
                return Err("clip names and title text cannot hold control characters".to_owned());
            }
        }
        let finite = match self {
            Self::Seek(value)
            | Self::SplitClip { at: value, .. }
            | Self::MoveClip { start: value, .. }
            | Self::AddAsset { at: value, .. } => value.is_finite(),
            Self::SetFade {
                fade_in, fade_out, ..
            } => fade_in.is_finite() && fade_out.is_finite(),
            Self::InsertTime { at, seconds } => at.is_finite() && seconds.is_finite(),
            Self::TrimClip { left, right, .. } => {
                left.iter().chain(right).all(|value| value.is_finite())
            }
            Self::SetOpacity { value, .. }
            | Self::SetVolume { value, .. }
            | Self::SetZoom(value) => value.is_finite(),
            Self::TightenSilence {
                threshold,
                min_silence,
                padding,
                ..
            } => {
                threshold.is_none_or(f32::is_finite)
                    && min_silence.is_finite()
                    && padding.is_finite()
            }
            Self::AddTitle { at, duration, .. } => at.is_finite() && duration.is_finite(),
            Self::AddAssetRange {
                at,
                source_in,
                duration,
                ..
            } => at.is_finite() && source_in.is_finite() && duration.is_finite(),
            Self::Play
            | Self::Pause
            | Self::TogglePlay
            | Self::StepFrames(_)
            | Self::SelectClip(_)
            | Self::DeleteClip(_)
            | Self::RenameClip { .. }
            | Self::DetachAudio { .. }
            | Self::UnlinkClip { .. }
            | Self::SetClipLane { .. }
            | Self::SetEmbeddedAudio { .. }
            | Self::SetDuck { .. }
            | Self::SetCleanVoice { .. }
            | Self::SetSmoothFades { .. }
            | Self::SetTitleStyle { .. }
            | Self::AddCaptionsForClip { .. }
            | Self::Import(_)
            | Self::Export(_)
            | Self::ClearTimeline
            | Self::SetMagnet(_)
            | Self::SetMusicBedMuted(_)
            | Self::CloseGaps
            | Self::Log(_) => true,
        };
        if finite {
            Ok(())
        } else {
            Err("command numeric arguments must be finite".to_owned())
        }
    }
}

const MAX_COMMANDS: usize = 2048;

const MAX_COMMAND_STRING_BYTES: usize = 1024 * 1024;

pub(super) const EVAL_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(super) struct CommandQueue {
    pub(super) commands: Vec<ScriptCommand>,
    pub(super) string_bytes: usize,
    pub(super) error: Option<String>,
    pub(super) deadline: Option<Instant>,
    /// Never reused within one engine, so a REPL variable holding an old
    /// handle cannot alias a later batch's addition.
    pub(super) next_handle: u64,
    /// The script looked at `state().audio`.
    pub(super) read_audio: bool,
}

impl CommandQueue {
    pub(super) fn check_active(&mut self) -> rquickjs::Result<()> {
        if let Some(error) = &self.error {
            return Err(rquickjs::Error::new_from_js_message(
                "JavaScript",
                "ScriptCommand",
                error.clone(),
            ));
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(self.reject(format!(
                "evaluation time limit exceeded ({} ms)",
                EVAL_TIMEOUT.as_millis()
            )));
        }
        Ok(())
    }

    pub(super) fn push(
        &mut self,
        string_bytes: usize,
        command: impl FnOnce() -> ScriptCommand,
    ) -> rquickjs::Result<()> {
        self.check_active()?;
        if self.commands.len() >= MAX_COMMANDS {
            return Err(self.reject(format!(
                "command limit exceeded (maximum {MAX_COMMANDS} per eval)"
            )));
        }
        if string_bytes > MAX_COMMAND_STRING_BYTES - self.string_bytes {
            return Err(self.reject(format!(
                "command string byte limit exceeded (maximum {MAX_COMMAND_STRING_BYTES} per eval)"
            )));
        }
        // String arguments borrow QuickJS-owned UTF-8 until these checks pass,
        // so even a rejected single large argument is never copied into Rust.
        let mut command = command();
        if let Err(error) = command.validate() {
            return Err(self.reject(error));
        }
        // Validate raw edges first: NaN and -Infinity must not become None.
        if let ScriptCommand::TrimClip { left, right, .. } = &mut command {
            *left = left.filter(|value| *value >= 0.0);
            *right = right.filter(|value| *value >= 0.0);
        }
        self.string_bytes += string_bytes;
        self.commands.push(command);
        Ok(())
    }

    /// Queue an addition and return the handle later commands can target.
    pub(super) fn push_addition(
        &mut self,
        string_bytes: usize,
        command: impl FnOnce(Option<u64>) -> ScriptCommand,
    ) -> rquickjs::Result<f64> {
        let handle = CLIP_HANDLE_BASE.max(self.next_handle);
        self.push(string_bytes, || command(Some(handle)))?;
        self.next_handle = handle + 1;
        Ok(handle as f64)
    }

    fn reject(&mut self, error: String) -> rquickjs::Error {
        self.commands.clear();
        // Sticky until the eval ends, even if JavaScript catches the exception.
        self.error = Some(error.clone());
        rquickjs::Error::new_from_js_message("JavaScript", "ScriptCommand", error)
    }
}

pub(super) type Queue = Rc<RefCell<CommandQueue>>;

/// A clip ID, handle or lane from JavaScript. rquickjs casts numbers with
/// `as`, which made `deleteClip(1.9)` delete clip 1 and `NaN` name clip 0.
pub(super) struct Whole(pub(super) u64);

impl<'js> rquickjs::FromJs<'js> for Whole {
    fn from_js(_: &rquickjs::Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<Self> {
        // Every whole number below 2^53 is exact in a double.
        const LIMIT: f64 = 9_007_199_254_740_992.0;
        match value.as_number() {
            Some(number) if (0.0..LIMIT).contains(&number) && number.fract() == 0.0 => {
                Ok(Self(number as u64))
            }
            _ => Err(rquickjs::Error::new_from_js_message(
                value.type_name(),
                "whole number",
                "clip IDs and lanes are whole numbers, 0 or more",
            )),
        }
    }
}

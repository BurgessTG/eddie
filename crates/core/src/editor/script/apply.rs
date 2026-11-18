//! One script command applied to the editor. Arguments out of range are
//! refused, never moved into range.

use std::collections::HashMap;
use std::path::PathBuf;

use openreel_engine::{fill_scale, Background, ClipId, ClipKind, Format, TitleStyle, Transform};

use super::super::details;
use crate::scripting::ScriptCommand;
use crate::Editor;

/// Refuse a time before zero instead of moving it there: a script that
/// computed one has a mistake its author should hear about.
fn from_zero(command: &str, what: &str, value: f64) -> Result<(), String> {
    if value < 0.0 {
        return Err(format!(
            "{command} needs a {what} of 0 or later, not {value}"
        ));
    }
    Ok(())
}

/// A title style by its script name; the error names the ones that exist.
fn title_style(name: &str) -> Result<TitleStyle, String> {
    TitleStyle::from_name(name).ok_or_else(|| {
        let known: Vec<&str> = TitleStyle::ALL.iter().map(|style| style.as_str()).collect();
        format!("unknown title style '{name}'; use {}", known.join(", "))
    })
}

/// `current` with the fields of the JSON object `changes`, or the identity
/// for `null`. A `scale` of `"fit"` is 1 and `"fill"` covers `canvas`
/// with a `source`-sized picture after the new crop.
fn changed_transform(
    current: Transform,
    changes: &str,
    canvas: (f64, f64),
    source: (f64, f64),
) -> Result<Transform, String> {
    let changes: Option<serde_json::Map<String, serde_json::Value>> = serde_json::from_str(changes)
        .map_err(|_| {
            "setTransform needs an object such as {scale: 1.5, x: 0.25}, or null".to_owned()
        })?;
    let Some(mut changes) = changes else {
        return Ok(Transform::default());
    };
    let mut value = serde_json::to_value(current).map_err(|error| error.to_string())?;
    let fields = value.as_object_mut().expect("a transform is an object");
    let scale = match changes.get("scale") {
        Some(serde_json::Value::String(word)) => {
            let word = word.clone();
            changes.remove("scale");
            Some(word)
        }
        _ => None,
    };
    for (key, change) in changes {
        if !fields.contains_key(&key) {
            let known: Vec<&str> = fields.keys().map(String::as_str).collect();
            return Err(format!(
                "setTransform has no '{key}'; use {}",
                known.join(", ")
            ));
        }
        fields.insert(key, change);
    }
    let mut transform: Transform = serde_json::from_value(value)
        .map_err(|error| format!("setTransform: {error}; numbers must be finite"))?;
    match scale.as_deref() {
        None => {}
        Some("fit") => transform.scale = 1.0,
        Some("fill") => transform.scale = fill_scale(canvas, source, &transform),
        Some(other) => {
            return Err(format!(
                "setTransform scale is a number, 'fit' or 'fill', not '{other}'"
            ));
        }
    }
    if !transform.is_valid() {
        return Err(format!(
            "setTransform needs a scale of {}-{}, x and y within 10 canvases, and crops of 0 or more that keep {}% of each side",
            openreel_engine::MIN_SCALE,
            openreel_engine::MAX_SCALE,
            openreel_engine::MIN_CROP_KEEP * 100.0
        ));
    }
    Ok(transform)
}

impl Editor {
    /// One executor for console commands and transactional generated edits.
    /// Failures are typed; a console log is never an application error.
    /// `created` maps this batch's addition handles to the clips they made.
    pub fn apply_script_command(
        &mut self,
        mut command: ScriptCommand,
        created: &mut HashMap<u64, ClipId>,
    ) -> Result<Option<String>, String> {
        command.validate()?;
        command.resolve_handle(created)?;
        if let Some(&mut id) = command.target_mut() {
            if self.timeline.clip(id).is_none() {
                return Err(format!("no clip {id}"));
            }
        }
        match command {
            ScriptCommand::Log(msg) => return Ok(Some(msg)),
            ScriptCommand::Play => self.set_playing(true),
            ScriptCommand::Pause => self.set_playing(false),
            ScriptCommand::TogglePlay => self.set_playing(!self.playing),
            ScriptCommand::Seek(t) => self.seek(t),
            ScriptCommand::StepFrames(n) => {
                self.seek(self.playhead + n as f64 / self.timeline.fps);
            }
            ScriptCommand::SelectClip(id) => self.selected = Some(id),
            ScriptCommand::SplitClip { clip, at, handle } => {
                let right = self
                    .timeline
                    .split_at(clip, at)
                    .ok_or_else(|| format!("cannot split clip {clip} at {at}"))?;
                self.selected = Some(right);
                created.extend(handle.map(|handle| (handle, right)));
            }
            ScriptCommand::DeleteClip(id) => {
                self.timeline.remove_clip(id);
                if self.selected == Some(id) {
                    self.selected = None;
                }
            }
            ScriptCommand::MoveClip { clip, start } => {
                from_zero("moveClip", "start", start)?;
                self.timeline.move_clip(clip, start);
            }
            ScriptCommand::TrimClip { clip, left, right } => {
                if let Some(c) = self.timeline.clip(clip) {
                    let (start, end) = (left.unwrap_or(c.start), right.unwrap_or(c.end()));
                    if end <= start {
                        return Err(format!(
                            "trimClip would end clip {clip} at {end} s, not after its start at \
                             {start} s: left is the new start and right the new end"
                        ));
                    }
                }
                // A request takes the nearest frame; the model then rounds
                // one way only where a bound (the media's end) is off the grid.
                if let Some(left) = left {
                    self.timeline.trim_left(clip, self.timeline.snap(left));
                }
                if let Some(right) = right {
                    let mut end = self.timeline.snap(right);
                    if let Some(c) = self.timeline.clip(clip) {
                        if let Some(asset) = c.source.as_ref().and_then(|s| self.asset(s)) {
                            end = end.min(c.start + (asset.info.duration - c.source_in).max(0.0));
                        }
                    }
                    self.timeline.trim_right(clip, end);
                }
            }
            ScriptCommand::RenameClip { clip, name } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.name = name;
                }
            }
            ScriptCommand::SetOpacity { clip, value } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.opacity = value.clamp(0.0, 1.0);
                }
            }
            ScriptCommand::SetTransform { clip, changes } => {
                let canvas = self.canvas();
                let canvas = (f64::from(canvas.0), f64::from(canvas.1));
                let target = self.video_clip(clip, "setTransform")?;
                let source = target
                    .source
                    .as_deref()
                    .and_then(|path| self.asset(path))
                    .map_or(canvas, |asset| {
                        (f64::from(asset.info.width), f64::from(asset.info.height))
                    });
                let transform = changed_transform(target.transform, &changes, canvas, source)?;
                if let Some(target) = self.timeline.clip_mut(clip) {
                    target.transform = transform;
                }
            }
            ScriptCommand::SetBackground { clip, name } => {
                let background = Background::from_name(&name)
                    .ok_or_else(|| format!("unknown background '{name}'; use black or blur"))?;
                self.video_clip(clip, "setBackground")?;
                if let Some(target) = self.timeline.clip_mut(clip) {
                    target.background = background;
                }
            }
            ScriptCommand::SetVolume { clip, value } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.volume = value.clamp(0.0, details::MAX_VOLUME);
                }
            }
            ScriptCommand::SetFade {
                clip,
                fade_in,
                fade_out,
            } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.fade_in = fade_in.max(0.0);
                    c.fade_out = fade_out.max(0.0);
                }
            }
            ScriptCommand::DetachAudio { clip } => {
                let has_audio = self
                    .timeline
                    .clip(clip)
                    .and_then(|c| c.source.as_ref())
                    .and_then(|source| self.asset(source))
                    .map(|asset| asset.info.has_audio)
                    .unwrap_or(false);
                if !has_audio {
                    return Err(format!("clip {clip} has no detachable audio"));
                }
                self.selected = Some(
                    self.timeline
                        .detach_audio(clip)
                        .ok_or_else(|| format!("clip {clip} audio is already detached"))?,
                );
            }
            ScriptCommand::SetClipLane { clip, lane } => {
                let Some(kind) = self.timeline.clip(clip).map(|clip| clip.kind) else {
                    return Err(format!("cannot move clip {clip} to lane {lane}"));
                };
                if lane > 64 {
                    return Err(format!("cannot move clip {clip} to lane {lane}"));
                }
                // Lanes count within the clip's kind; a new number grows
                // that kind's lanes.
                self.timeline.grow_lanes(kind, lane as usize);
                self.timeline.set_clip_track(clip, lane as usize);
            }
            ScriptCommand::UnlinkClip { clip } => {
                if !self.timeline.unlink_clip(clip) {
                    return Err(format!("clip {clip} is not linked"));
                }
            }
            ScriptCommand::SetEmbeddedAudio { clip, enabled } => {
                let source_has_audio = !enabled
                    || self
                        .timeline
                        .clip(clip)
                        .and_then(|c| c.source.as_ref())
                        .and_then(|source| self.asset(source))
                        .map(|asset| asset.info.has_audio)
                        .unwrap_or(false);
                if !source_has_audio || !self.timeline.set_embedded_audio(clip, enabled) {
                    return Err(format!(
                        "cannot {} embedded audio on clip {clip}",
                        if enabled { "enable" } else { "disable" }
                    ));
                }
            }
            ScriptCommand::TightenSilence {
                clip,
                threshold,
                min_silence,
                padding,
            } => {
                self.tighten_clip_silence(clip, threshold, min_silence, padding)?;
            }
            ScriptCommand::AddTitle {
                text,
                at,
                duration,
                style,
                handle,
            } => {
                from_zero("addTitle", "time", at)?;
                if duration <= 0.0 {
                    return Err(format!("addTitle needs a positive length, not {duration}"));
                }
                let style = title_style(&style)?;
                let track = self.timeline.free_lane(ClipKind::Title, at, at + duration);
                let id = self
                    .timeline
                    .add_clip(&text, ClipKind::Title, track, at, duration, 0.0);
                if let Some(clip) = self.timeline.clip_mut(id) {
                    clip.title_style = style;
                }
                self.selected = Some(id);
                created.extend(handle.map(|handle| (handle, id)));
            }
            ScriptCommand::SetTitleStyle { clip, style } => {
                let style = title_style(&style)?;
                if let Some(target) = self.timeline.clip_mut(clip) {
                    if target.kind != ClipKind::Title {
                        return Err(format!("clip {clip} is not a title"));
                    }
                    target.title_style = style;
                }
            }
            ScriptCommand::AddCaptionsForClip { clip, style } => {
                let style = title_style(&style)?;
                return Ok(Some(self.caption_clip(clip, style)?.note(clip)));
            }
            ScriptCommand::AddAsset { asset, at, handle } => {
                from_zero("addAsset", "time", at)?;
                let (path, name, duration) = self
                    .asset_named(&asset)
                    .map(|a| (a.info.path.clone(), a.info.name.clone(), a.info.duration))?;
                let id =
                    self.try_add_asset_range_to_timeline(&path, &name, None, at, 0.0, duration)?;
                created.extend(handle.map(|handle| (handle, id)));
            }
            ScriptCommand::AddAssetRange {
                asset,
                at,
                source_in,
                duration,
                handle,
            } => {
                from_zero("addAssetRange", "time", at)?;
                from_zero("addAssetRange", "source start", source_in)?;
                if duration <= 0.0 {
                    return Err(format!(
                        "addAssetRange needs a positive length, not {duration}"
                    ));
                }
                let (path, name) = self
                    .asset_named(&asset)
                    .map(|a| (a.info.path.clone(), a.info.name.clone()))?;
                let id = self
                    .try_add_asset_range_to_timeline(&path, &name, None, at, source_in, duration)?;
                created.extend(handle.map(|handle| (handle, id)));
            }
            ScriptCommand::Import(path) => self.spawn_import(vec![PathBuf::from(path)]),
            ScriptCommand::Export(path) => self.start_export(PathBuf::from(path)),
            ScriptCommand::ClearTimeline => self.clear_timeline(),
            ScriptCommand::SetMagnet(on) => self.viewport.magnet = on,
            ScriptCommand::SetMusicBedMuted(on) => self.mute_music_beds = on,
            ScriptCommand::SetFormat(name) => {
                let format = Format::from_name(&name).ok_or_else(|| {
                    let known: Vec<&str> = Format::ALL.iter().map(|f| f.as_str()).collect();
                    format!("unknown format '{name}'; use {}", known.join(", "))
                })?;
                self.set_format(format);
                // Settings shows the project's format, as after undo.
                self.settings.format = format;
            }
            ScriptCommand::SetZoom(px) => self.viewport.set_zoom(px, self.timeline.end()),
            ScriptCommand::CloseGaps => {
                self.timeline.close_gaps();
            }
            ScriptCommand::SetDuck { clip, enabled } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.duck = enabled;
                }
            }
            ScriptCommand::SetCleanVoice { clip, enabled } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.clean_voice = enabled;
                }
            }
            ScriptCommand::SetSmoothFades { clip, enabled } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.smooth_fades = enabled;
                }
            }
            ScriptCommand::InsertTime { at, seconds } => {
                from_zero("insertTime", "time", at)?;
                if seconds <= 0.0 {
                    return Err(format!("insertTime needs a positive length, not {seconds}"));
                }
                self.timeline.insert_time(at, seconds);
            }
        }
        Ok(None)
    }

    /// The video clip a picture command targets.
    fn video_clip(&self, clip: ClipId, command: &str) -> Result<&openreel_engine::Clip, String> {
        match self.timeline.clip(clip) {
            Some(target) if target.kind == ClipKind::Video => Ok(target),
            Some(_) => Err(format!(
                "{command} needs a video clip; clip {clip} is not one"
            )),
            None => Err(format!("{command}: there is no clip {clip}")),
        }
    }
}

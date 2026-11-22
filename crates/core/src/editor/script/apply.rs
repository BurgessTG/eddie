//! One script command applied to the editor. Arguments out of range are
//! refused, never moved into range.

use std::collections::HashMap;
use std::path::PathBuf;

use openreel_engine::{
    fill_scale, Background, BlendMode, ChromaKey, ClipId, ClipKind, ColorAdjust, Filter, Format,
    Mask, MaskShape, Property, TextLook, TitleStyle, Transform, Transition, TransitionKind,
    MAX_OUTLINE, MAX_SPEED, MAX_TEXT_OFFSET, MAX_TEXT_SIZE, MAX_TRANSITION, MIN_SPEED,
    MIN_TEXT_SIZE, MIN_TRANSITION,
};

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

/// A script's JSON object of fields to change, or `None` for `null`.
fn changes_object(
    command: &str,
    example: &str,
    changes: &str,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, String> {
    serde_json::from_str(changes)
        .map_err(|_| format!("{command} needs an object such as {example}, or null"))
}

/// `current` with the fields in `changes`; a name it does not have is an
/// error that lists the ones it does.
fn merged<T: serde::Serialize + serde::de::DeserializeOwned>(
    command: &str,
    current: T,
    changes: serde_json::Map<String, serde_json::Value>,
) -> Result<T, String> {
    let mut value = serde_json::to_value(current).map_err(|error| error.to_string())?;
    let fields = value.as_object_mut().expect("settings are an object");
    for (key, change) in changes {
        if !fields.contains_key(&key) {
            let known: Vec<&str> = fields.keys().map(String::as_str).collect();
            return Err(format!(
                "{command} has no '{key}'; use {}",
                known.join(", ")
            ));
        }
        fields.insert(key, change);
    }
    serde_json::from_value(value).map_err(|error| {
        // JSON has no NaN or Infinity: JSON.stringify writes them as null.
        let hint = if error.to_string().contains("null") {
            "; numbers must be finite"
        } else {
            ""
        };
        format!("{command}: {error}{hint}")
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
    let Some(mut changes) = changes_object("setTransform", "{scale: 1.5, x: 0.25}", changes)?
    else {
        return Ok(Transform::default());
    };
    let scale = match changes.get("scale") {
        Some(serde_json::Value::String(word)) => {
            let word = word.clone();
            changes.remove("scale");
            Some(word)
        }
        _ => None,
    };
    let mut transform = merged("setTransform", current, changes)?;
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

/// `current` with the fields of the JSON object `changes`, or no grade for
/// `null`. A filter is named; an unknown one lists those that exist.
fn changed_color(current: ColorAdjust, changes: &str) -> Result<ColorAdjust, String> {
    let Some(changes) = changes_object("setColor", "{saturation: 0.2, filter: 'warm'}", changes)?
    else {
        return Ok(ColorAdjust::default());
    };
    if let Some(name) = changes.get("filter") {
        if name.as_str().and_then(Filter::from_name).is_none() {
            let known: Vec<&str> = Filter::ALL.iter().map(|f| f.as_str()).collect();
            return Err(format!("unknown filter {name}; use {}", known.join(", ")));
        }
    }
    let mut changes = changes;
    if let Some(name) = changes.get_mut("filter") {
        let filter = name
            .as_str()
            .and_then(Filter::from_name)
            .unwrap_or_default();
        *name = serde_json::Value::from(filter.as_str());
    }
    let color = merged("setColor", current, changes)?;
    if !color.is_valid() {
        return Err(format!(
            "setColor takes brightness, contrast, saturation, temperature and tint from -1 to 1, exposure from -{0} to {0} stops and strength from 0 to 1",
            openreel_engine::MAX_EXPOSURE
        ));
    }
    Ok(color)
}

/// `current` (a circle when the clip has no mask) with the fields of the
/// JSON object `changes`, or no mask for `null`.
fn changed_mask(current: Option<Mask>, changes: &str) -> Result<Option<Mask>, String> {
    let example = "{shape: 'heart', width: 0.5}";
    let Some(mut changes) = changes_object("setMask", example, changes)? else {
        return Ok(None);
    };
    if let Some(shape) = changes.get_mut("shape") {
        let Some(known) = shape.as_str().and_then(MaskShape::from_name) else {
            let names: Vec<&str> = MaskShape::ALL.iter().map(|s| s.as_str()).collect();
            return Err(format!("setMask shape is one of {}", names.join(", ")));
        };
        *shape = known.as_str().into();
    }
    let mask = merged("setMask", current.unwrap_or_default(), changes)?;
    if !mask.is_valid() {
        return Err(format!(
            "setMask takes x and y from -{0} to {0}, width and height from {1} to {2}, \
             rotation from -360 to 360, and feather and round from 0 to 1",
            openreel_engine::MASK_MAX_OFFSET,
            openreel_engine::MASK_MIN_SIZE,
            openreel_engine::MASK_MAX_SIZE
        ));
    }
    Ok(Some(mask))
}

/// `current` (green when the clip has no key) with the fields of the JSON
/// object `changes`, or no key for `null`.
fn changed_chroma(current: Option<ChromaKey>, changes: &str) -> Result<Option<ChromaKey>, String> {
    let example = "{color: '#00B140', strength: 0.5}";
    let Some(changes) = changes_object("setChroma", example, changes)? else {
        return Ok(None);
    };
    let key = merged("setChroma", current.unwrap_or_default(), changes)?;
    if !key.is_valid() {
        return Err("setChroma takes strength, softness and spill from 0 to 1".to_owned());
    }
    Ok(Some(key))
}

/// `current` with the fields of the JSON object `changes`, or the style's
/// look for `null`.
fn changed_text(current: TextLook, changes: &str) -> Result<TextLook, String> {
    let Some(changes) = changes_object("setText", "{size: 1.5, color: '#FFD600'}", changes)? else {
        return Ok(TextLook::default());
    };
    let text = merged("setText", current, changes)?;
    if !text.is_valid() {
        return Err(format!(
            "setText takes a size of {MIN_TEXT_SIZE} to {MAX_TEXT_SIZE}, a boxOpacity of 0 to 1, an outline of 0 to {MAX_OUTLINE} and x and y from -{MAX_TEXT_OFFSET} to {MAX_TEXT_OFFSET}"
        ));
    }
    Ok(text)
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
            ScriptCommand::RippleDelete(id) => {
                let ripple = self.timeline.ripple_delete(id);
                if self
                    .selected
                    .is_some_and(|id| self.timeline.clip(id).is_none())
                {
                    self.selected = None;
                }
                let mut notes = Vec::new();
                if let Some(ripple) = ripple.filter(|ripple| ripple.titles > 0) {
                    notes.push(format!("removed {} title(s) over it", ripple.titles));
                }
                if let Some(ripple) = ripple.filter(|ripple| ripple.lanes_kept > 0) {
                    notes.push(format!(
                        "{} lane(s) kept their place under a clip across the gap",
                        ripple.lanes_kept
                    ));
                }
                if !notes.is_empty() {
                    return Ok(Some(format!("rippleDelete({id}): {}", notes.join("; "))));
                }
            }
            ScriptCommand::DuplicateClip { clip, handle } => {
                let copy = self
                    .duplicate_clip(clip)
                    .ok_or_else(|| format!("no clip {clip}"))?;
                self.selected = Some(copy);
                created.extend(handle.map(|handle| (handle, copy)));
            }
            ScriptCommand::FreezeFrame {
                clip,
                at,
                seconds,
                handle,
            } => {
                let freeze = self
                    .freeze_frame(clip, at, seconds)
                    .map_err(|why| format!("freezeFrame: clip {clip}: {why}"))?;
                self.selected = Some(freeze);
                created.extend(handle.map(|handle| (handle, freeze)));
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
                        let source = c.source.as_ref().filter(|_| !c.still());
                        if let Some(asset) = source.and_then(|s| self.asset(s)) {
                            end = end.min(c.time_at(asset.info.duration).max(c.start));
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
                    c.keyframes.clear(Property::Opacity);
                }
            }
            ScriptCommand::SetKeyframe {
                clip,
                property,
                at,
                value,
            } => self.set_keyframe(clip, &property, at, value)?,
            ScriptCommand::RemoveKeyframe { clip, property, at } => {
                self.remove_keyframe(clip, &property, at)?;
            }
            ScriptCommand::ClearKeyframes { clip, property } => {
                self.clear_keyframes(clip, &property)?;
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
                // A field it sets is one value for the whole clip again.
                let set: Vec<Property> = match serde_json::from_str(&changes) {
                    Ok(serde_json::Value::Object(fields)) => [
                        Property::X,
                        Property::Y,
                        Property::Scale,
                        Property::Rotation,
                    ]
                    .into_iter()
                    .filter(|p| fields.contains_key(p.as_str()))
                    .collect(),
                    _ => vec![
                        Property::X,
                        Property::Y,
                        Property::Scale,
                        Property::Rotation,
                    ],
                };
                if let Some(target) = self.timeline.clip_mut(clip) {
                    target.transform = transform;
                    for property in set {
                        target.keyframes.clear(property);
                    }
                }
            }
            ScriptCommand::SetText { clip, changes } => {
                let text = match self.timeline.clip(clip) {
                    Some(target) if target.kind == ClipKind::Title => {
                        changed_text(target.text, &changes)?
                    }
                    _ => return Err(format!("setText needs a title; clip {clip} is not one")),
                };
                if let Some(target) = self.timeline.clip_mut(clip) {
                    target.text = text;
                }
            }
            ScriptCommand::SetMask { clip, changes } => {
                let mask = changed_mask(self.video_clip(clip, "setMask")?.mask, &changes)?;
                if let Some(target) = self.timeline.clip_mut(clip) {
                    target.mask = mask;
                }
            }
            ScriptCommand::SetChroma { clip, changes } => {
                let chroma = changed_chroma(self.video_clip(clip, "setChroma")?.chroma, &changes)?;
                if let Some(target) = self.timeline.clip_mut(clip) {
                    target.chroma = chroma;
                }
            }
            ScriptCommand::SetColor { clip, changes } => {
                let color = changed_color(self.video_clip(clip, "setColor")?.color, &changes)?;
                if let Some(target) = self.timeline.clip_mut(clip) {
                    target.color = color;
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
            ScriptCommand::SetBlend { clip, name } => {
                let blend = BlendMode::from_name(&name).ok_or_else(|| {
                    let known: Vec<&str> =
                        BlendMode::ALL.iter().map(|mode| mode.as_str()).collect();
                    format!("unknown blend mode '{name}'; use {}", known.join(", "))
                })?;
                self.video_clip(clip, "setBlend")?;
                if let Some(target) = self.timeline.clip_mut(clip) {
                    target.blend = blend;
                }
            }
            ScriptCommand::SetTransition {
                clip,
                kind,
                seconds,
            } => {
                self.video_clip(clip, "setTransition")?;
                let transition = if kind.is_empty() || kind.eq_ignore_ascii_case("none") {
                    None
                } else {
                    let kind = TransitionKind::from_name(&kind).ok_or_else(|| {
                        let known: Vec<&str> = TransitionKind::ALL
                            .iter()
                            .map(|kind| kind.as_str())
                            .collect();
                        format!("unknown transition '{kind}'; use {}", known.join(", "))
                    })?;
                    if !(MIN_TRANSITION..=MAX_TRANSITION).contains(&seconds) {
                        return Err(format!(
                            "setTransition takes {MIN_TRANSITION} to {MAX_TRANSITION} seconds, not {seconds}"
                        ));
                    }
                    if self.timeline.clip_before(clip).is_none() {
                        return Err(format!(
                            "setTransition: clip {clip} has no clip right before it on its lane"
                        ));
                    }
                    Some(Transition::new(kind, seconds))
                };
                if let Some(target) = self.timeline.clip_mut(clip) {
                    target.transition = transition;
                }
            }
            ScriptCommand::SetVolume { clip, value } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.volume = value.clamp(0.0, details::MAX_VOLUME);
                    c.keyframes.clear(Property::Volume);
                }
            }
            ScriptCommand::SetSpeed { clip, speed } => {
                if !(MIN_SPEED..=MAX_SPEED).contains(&speed) {
                    return Err(format!(
                        "setSpeed takes {MIN_SPEED} to {MAX_SPEED}, not {speed}"
                    ));
                }
                let target = self
                    .timeline
                    .clip(clip)
                    .ok_or_else(|| format!("setSpeed: there is no clip {clip}"))?;
                if target.kind == ClipKind::Title || target.source.is_none() {
                    return Err(format!(
                        "setSpeed needs a video or audio clip; clip {clip} is not one"
                    ));
                }
                if target.still() {
                    let what = if target.photo {
                        "photo"
                    } else {
                        "freeze frame"
                    };
                    return Err(format!(
                        "setSpeed: clip {clip} is a {what}; trim it to change how long it shows"
                    ));
                }
                self.timeline.set_speed(clip, speed);
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
                    .map(|a| (a.info.path.clone(), a.info.name.clone(), a.clip_length()))?;
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
            command @ (ScriptCommand::AddMarker { .. }
            | ScriptCommand::SetMarker { .. }
            | ScriptCommand::RemoveMarker(_)
            | ScriptCommand::ClearMarkers) => return self.apply_marker_command(command),
            ScriptCommand::SetDuck { clip, enabled } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.duck = enabled;
                }
            }
            ScriptCommand::SetReduceNoise { clip, enabled } => {
                if let Some(c) = self.timeline.clip_mut(clip) {
                    c.reduce_noise = enabled;
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

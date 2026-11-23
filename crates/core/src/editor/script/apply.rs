//! One script command applied to the editor. Arguments out of range are
//! refused, never moved into range.

use std::collections::HashMap;
use std::path::PathBuf;

use openreel_engine::{
    Background, BlendMode, ClipId, ClipKind, Format, Graphic, Property, Shape, ShapeKind,
    TitleStyle, Transition, TransitionKind, MAX_SPEED, MAX_TRANSITION, MIN_SPEED, MIN_TRANSITION,
};

mod changes;

use super::super::details;
use crate::scripting::ScriptCommand;
use crate::Editor;
use changes::{
    changed_chroma, changed_color, changed_mask, changed_shape, changed_text, changed_transform,
};

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
                ease,
            } => self.set_keyframe(clip, &property, at, value, &ease)?,
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
                // A field it sets is one value for the whole clip again.
                let fields = match serde_json::from_str(&changes) {
                    Ok(serde_json::Value::Object(fields)) => Some(fields),
                    _ => None,
                };
                let set = Property::COLOR.into_iter().filter(|p| {
                    let name = match p {
                        Property::FilterStrength => "strength",
                        p => p.as_str(),
                    };
                    fields.as_ref().is_none_or(|f| f.contains_key(name))
                });
                let set: Vec<Property> = set.collect();
                if let Some(target) = self.timeline.clip_mut(clip) {
                    target.color = color;
                    for property in set {
                        target.keyframes.clear(property);
                    }
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
            ScriptCommand::AddShape {
                look,
                at,
                duration,
                handle,
            } => {
                let shape = changed_shape("addShape", Shape::new(ShapeKind::default()), &look)?;
                let id = self.add_graphic(Graphic::Shape(shape), None, at, duration)?;
                created.extend(handle.map(|handle| (handle, id)));
            }
            ScriptCommand::AddSticker {
                name,
                at,
                duration,
                handle,
            } => {
                let sticker = openreel_media::sticker(&name)
                    .ok_or_else(|| format!("no sticker '{name}'; stickers() lists the names"))?;
                let graphic = Graphic::Sticker {
                    name: sticker.name.to_owned(),
                };
                let id = self.add_graphic(graphic, None, at, duration)?;
                created.extend(handle.map(|handle| (handle, id)));
            }
            ScriptCommand::SetShape { clip, changes } => {
                let current = match self.timeline.clip(clip).map(|clip| clip.graphic.as_ref()) {
                    Some(Some(Graphic::Shape(shape))) => *shape,
                    Some(_) => {
                        return Err(format!("setShape needs a shape; clip {clip} is not one"))
                    }
                    None => return Err(format!("setShape: there is no clip {clip}")),
                };
                let shape = changed_shape("setShape", current, &changes)?;
                self.set_graphic(clip, Graphic::Shape(shape))?;
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
                let (path, name, duration) = self.asset_named(&asset).map(|a| {
                    let length = a.clip_length(self.settings.photo_length);
                    (a.info.path.clone(), a.info.name.clone(), length)
                })?;
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
            ScriptCommand::SetReverse { clip, enabled } => self.set_reverse(clip, enabled)?,
            ScriptCommand::SetStabilize { clip, level } => {
                let level = match level.as_str() {
                    "" => None,
                    name => Some(openreel_engine::Stabilize::parse(name).ok_or_else(|| {
                        format!(
                            "unknown stabilize level '{name}'; use minimal-crop, recommended or most-stable, or null"
                        )
                    })?),
                };
                self.set_stabilize(clip, level)?
            }
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

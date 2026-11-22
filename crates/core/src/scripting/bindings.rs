//! The native functions scripts call. Each one queues a `ScriptCommand`;
//! none touches the editor.

use std::rc::Rc;

#[cfg(test)]
use std::time::Duration;

use rquickjs::function::{IntoJsFunc, Opt};
use rquickjs::{CString, Ctx, Function, Object};

use super::command::{Queue, ScriptCommand, Whole};

/// One VM's globals and the queue its functions fill.
struct Api<'a, 'js> {
    ctx: &'a Ctx<'js>,
    globals: Object<'js>,
    queue: &'a Queue,
}

impl<'js> Api<'_, 'js> {
    fn set<P>(&self, name: &str, function: impl IntoJsFunc<'js, P> + 'js) -> rquickjs::Result<()> {
        self.globals
            .set(name, Function::new(self.ctx.clone(), function)?)
    }

    /// The queue, for a function to own.
    fn queue(&self) -> Queue {
        Rc::clone(self.queue)
    }
}

/// Define every native function on the context's globals.
pub(super) fn install(ctx: &Ctx<'_>, queue: &Queue) -> rquickjs::Result<()> {
    let api = Api {
        ctx,
        globals: ctx.globals(),
        queue,
    };
    reports(&api)?;
    transport(&api)?;
    clip_edits(&api)?;
    additions(&api)?;
    project(&api)?;
    helpers(&api)?;
    Ok(())
}

/// What a script says and whether it read the measured mix.
fn reports(api: &Api<'_, '_>) -> rquickjs::Result<()> {
    let q = api.queue();
    api.set("__log", move |msg: CString<'_>| {
        q.borrow_mut()
            .push(msg.len(), || ScriptCommand::Log(msg.as_str().to_owned()))
    })?;
    let q = api.queue();
    api.set("__audio_read", move || q.borrow_mut().read_audio = true)?;
    Ok(())
}

/// Playback and the playhead.
fn transport(api: &Api<'_, '_>) -> rquickjs::Result<()> {
    let q = api.queue();
    api.set("play", move || {
        q.borrow_mut().push(0, || ScriptCommand::Play)
    })?;
    let q = api.queue();
    api.set("pause", move || {
        q.borrow_mut().push(0, || ScriptCommand::Pause)
    })?;
    let q = api.queue();
    api.set("toggle", move || {
        q.borrow_mut().push(0, || ScriptCommand::TogglePlay)
    })?;
    let q = api.queue();
    api.set("__seek", move |t: f64| {
        q.borrow_mut().push(0, || ScriptCommand::Seek(t))
    })?;
    let q = api.queue();
    api.set("step", move |n: i64| {
        q.borrow_mut().push(0, || ScriptCommand::StepFrames(n))
    })?;
    Ok(())
}

/// Changes to clips already on the timeline.
fn clip_edits(api: &Api<'_, '_>) -> rquickjs::Result<()> {
    let q = api.queue();
    api.set("selectClip", move |Whole(id): Whole| {
        q.borrow_mut().push(0, || ScriptCommand::SelectClip(id))
    })?;
    let q = api.queue();
    api.set("__split_clip", move |Whole(clip): Whole, at: f64| {
        q.borrow_mut()
            .push_addition(0, |handle| ScriptCommand::SplitClip { clip, at, handle })
    })?;
    let q = api.queue();
    api.set("__delete_clip", move |Whole(clip): Whole| {
        q.borrow_mut().push(0, || ScriptCommand::DeleteClip(clip))
    })?;
    let q = api.queue();
    api.set("rippleDelete", move |Whole(clip): Whole| {
        q.borrow_mut().push(0, || ScriptCommand::RippleDelete(clip))
    })?;
    let q = api.queue();
    api.set("duplicateClip", move |Whole(clip): Whole| {
        q.borrow_mut()
            .push_addition(0, |handle| ScriptCommand::DuplicateClip { clip, handle })
    })?;
    let q = api.queue();
    api.set(
        "__freeze_frame",
        move |Whole(clip): Whole, at: f64, seconds: f64| {
            q.borrow_mut()
                .push_addition(0, |handle| ScriptCommand::FreezeFrame {
                    clip,
                    at,
                    seconds,
                    handle,
                })
        },
    )?;
    let q = api.queue();
    api.set("__move_clip", move |Whole(clip): Whole, start: f64| {
        q.borrow_mut()
            .push(0, || ScriptCommand::MoveClip { clip, start })
    })?;
    let q = api.queue();
    api.set(
        "__trim_clip",
        move |Whole(clip): Whole, left: f64, right: f64| {
            q.borrow_mut().push(0, || ScriptCommand::TrimClip {
                clip,
                left: Some(left),
                right: Some(right),
            })
        },
    )?;
    let q = api.queue();
    api.set(
        "renameClip",
        move |Whole(clip): Whole, name: CString<'_>| {
            q.borrow_mut()
                .push(name.len(), || ScriptCommand::RenameClip {
                    clip,
                    name: name.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set("setOpacity", move |Whole(clip): Whole, value: f32| {
        q.borrow_mut()
            .push(0, || ScriptCommand::SetOpacity { clip, value })
    })?;
    let q = api.queue();
    api.set(
        "__set_keyframe",
        move |Whole(clip): Whole, property: CString<'_>, at: f64, value: f64| {
            q.borrow_mut()
                .push(property.len(), || ScriptCommand::SetKeyframe {
                    clip,
                    property: property.as_str().to_owned(),
                    at,
                    value,
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__remove_keyframe",
        move |Whole(clip): Whole, property: CString<'_>, at: f64| {
            q.borrow_mut()
                .push(property.len(), || ScriptCommand::RemoveKeyframe {
                    clip,
                    property: property.as_str().to_owned(),
                    at,
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__clear_keyframes",
        move |Whole(clip): Whole, property: CString<'_>| {
            q.borrow_mut()
                .push(property.len(), || ScriptCommand::ClearKeyframes {
                    clip,
                    property: property.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__set_transform",
        move |Whole(clip): Whole, changes: CString<'_>| {
            q.borrow_mut()
                .push(changes.len(), || ScriptCommand::SetTransform {
                    clip,
                    changes: changes.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__set_color",
        move |Whole(clip): Whole, changes: CString<'_>| {
            q.borrow_mut()
                .push(changes.len(), || ScriptCommand::SetColor {
                    clip,
                    changes: changes.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__set_mask",
        move |Whole(clip): Whole, changes: CString<'_>| {
            q.borrow_mut()
                .push(changes.len(), || ScriptCommand::SetMask {
                    clip,
                    changes: changes.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__set_chroma",
        move |Whole(clip): Whole, changes: CString<'_>| {
            q.borrow_mut()
                .push(changes.len(), || ScriptCommand::SetChroma {
                    clip,
                    changes: changes.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__set_text",
        move |Whole(clip): Whole, changes: CString<'_>| {
            q.borrow_mut()
                .push(changes.len(), || ScriptCommand::SetText {
                    clip,
                    changes: changes.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__set_background",
        move |Whole(clip): Whole, name: CString<'_>| {
            q.borrow_mut()
                .push(name.len(), || ScriptCommand::SetBackground {
                    clip,
                    name: name.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__set_blend",
        move |Whole(clip): Whole, name: CString<'_>| {
            q.borrow_mut().push(name.len(), || ScriptCommand::SetBlend {
                clip,
                name: name.as_str().to_owned(),
            })
        },
    )?;
    let q = api.queue();
    api.set("setVolume", move |Whole(clip): Whole, value: f32| {
        q.borrow_mut()
            .push(0, || ScriptCommand::SetVolume { clip, value })
    })?;
    let q = api.queue();
    api.set(
        "__set_transition",
        move |Whole(clip): Whole, kind: CString<'_>, seconds: f64| {
            q.borrow_mut()
                .push(kind.len(), || ScriptCommand::SetTransition {
                    clip,
                    kind: kind.as_str().to_owned(),
                    seconds,
                })
        },
    )?;
    let q = api.queue();
    api.set("setSpeed", move |Whole(clip): Whole, speed: f64| {
        q.borrow_mut()
            .push(0, || ScriptCommand::SetSpeed { clip, speed })
    })?;
    let q = api.queue();
    api.set(
        "setReduceNoise",
        move |Whole(clip): Whole, enabled: bool| {
            q.borrow_mut()
                .push(0, || ScriptCommand::SetReduceNoise { clip, enabled })
        },
    )?;
    let q = api.queue();
    api.set("setCleanVoice", move |Whole(clip): Whole, enabled: bool| {
        q.borrow_mut()
            .push(0, || ScriptCommand::SetCleanVoice { clip, enabled })
    })?;
    let q = api.queue();
    api.set(
        "setSmoothFades",
        move |Whole(clip): Whole, enabled: bool| {
            q.borrow_mut()
                .push(0, || ScriptCommand::SetSmoothFades { clip, enabled })
        },
    )?;
    let q = api.queue();
    api.set(
        "setFade",
        move |Whole(clip): Whole, fade_in: f64, fade_out: f64| {
            q.borrow_mut().push(0, || ScriptCommand::SetFade {
                clip,
                fade_in,
                fade_out,
            })
        },
    )?;
    let q = api.queue();
    api.set("detachAudio", move |Whole(clip): Whole| {
        q.borrow_mut()
            .push(0, || ScriptCommand::DetachAudio { clip })
    })?;
    let q = api.queue();
    api.set("unlinkClip", move |Whole(clip): Whole| {
        q.borrow_mut()
            .push(0, || ScriptCommand::UnlinkClip { clip })
    })?;
    let q = api.queue();
    api.set(
        "setClipLane",
        move |Whole(clip): Whole, Whole(lane): Whole| {
            q.borrow_mut()
                .push(0, || ScriptCommand::SetClipLane { clip, lane })
        },
    )?;
    let q = api.queue();
    api.set(
        "setEmbeddedAudio",
        move |Whole(clip): Whole, enabled: bool| {
            q.borrow_mut()
                .push(0, || ScriptCommand::SetEmbeddedAudio { clip, enabled })
        },
    )?;
    let q = api.queue();
    api.set("setDuck", move |Whole(clip): Whole, enabled: bool| {
        q.borrow_mut()
            .push(0, || ScriptCommand::SetDuck { clip, enabled })
    })?;
    let q = api.queue();
    api.set("setReverse", move |Whole(clip): Whole, enabled: bool| {
        q.borrow_mut()
            .push(0, || ScriptCommand::SetReverse { clip, enabled })
    })?;
    let q = api.queue();
    api.set(
        "__set_stabilize",
        move |Whole(clip): Whole, level: CString<'_>| {
            q.borrow_mut()
                .push(level.len(), || ScriptCommand::SetStabilize {
                    clip,
                    level: level.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set("__insert_time", move |at: f64, seconds: f64| {
        q.borrow_mut()
            .push(0, || ScriptCommand::InsertTime { at, seconds })
    })?;
    let q = api.queue();
    api.set(
        "__tighten_silence",
        move |Whole(clip): Whole, min_silence: f64, padding: f64, threshold: Opt<f32>| {
            q.borrow_mut().push(0, || ScriptCommand::TightenSilence {
                clip,
                threshold: threshold.0,
                min_silence,
                padding,
            })
        },
    )?;
    Ok(())
}

/// Titles, captions, shapes, stickers and media placed on the timeline.
fn additions(api: &Api<'_, '_>) -> rquickjs::Result<()> {
    let q = api.queue();
    api.set(
        "__add_title",
        move |text: CString<'_>, at: f64, duration: f64, style: CString<'_>| {
            q.borrow_mut()
                .push_addition(text.len().saturating_add(style.len()), |handle| {
                    ScriptCommand::AddTitle {
                        text: text.as_str().to_owned(),
                        at,
                        duration,
                        style: style.as_str().to_owned(),
                        handle,
                    }
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "setTitleStyle",
        move |Whole(clip): Whole, style: CString<'_>| {
            q.borrow_mut()
                .push(style.len(), || ScriptCommand::SetTitleStyle {
                    clip,
                    style: style.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__add_captions_for_clip",
        move |Whole(clip): Whole, style: CString<'_>| {
            q.borrow_mut()
                .push(style.len(), || ScriptCommand::AddCaptionsForClip {
                    clip,
                    style: style.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__add_shape",
        move |look: CString<'_>, at: f64, duration: f64| {
            q.borrow_mut()
                .push_addition(look.len(), |handle| ScriptCommand::AddShape {
                    look: look.as_str().to_owned(),
                    at,
                    duration,
                    handle,
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__add_sticker",
        move |name: CString<'_>, at: f64, duration: f64| {
            q.borrow_mut()
                .push_addition(name.len(), |handle| ScriptCommand::AddSticker {
                    name: name.as_str().to_owned(),
                    at,
                    duration,
                    handle,
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__set_shape",
        move |Whole(clip): Whole, changes: CString<'_>| {
            q.borrow_mut()
                .push(changes.len(), || ScriptCommand::SetShape {
                    clip,
                    changes: changes.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set("__add_asset", move |asset: CString<'_>, at: f64| {
        q.borrow_mut()
            .push_addition(asset.len(), |handle| ScriptCommand::AddAsset {
                asset: asset.as_str().to_owned(),
                at,
                handle,
            })
    })?;
    let q = api.queue();
    api.set(
        "__add_asset_range",
        move |asset: CString<'_>, at: f64, source_in: f64, duration: f64| {
            q.borrow_mut()
                .push_addition(asset.len(), |handle| ScriptCommand::AddAssetRange {
                    asset: asset.as_str().to_owned(),
                    at,
                    source_in,
                    duration,
                    handle,
                })
        },
    )?;
    Ok(())
}

/// Import, export and whole-timeline settings.
fn project(api: &Api<'_, '_>) -> rquickjs::Result<()> {
    let q = api.queue();
    api.set("importMedia", move |path: CString<'_>| {
        q.borrow_mut().push(path.len(), || {
            ScriptCommand::Import(path.as_str().to_owned())
        })
    })?;
    let q = api.queue();
    api.set("exportTo", move |path: CString<'_>| {
        q.borrow_mut().push(path.len(), || {
            ScriptCommand::Export(path.as_str().to_owned())
        })
    })?;
    let q = api.queue();
    api.set("clearTimeline", move || {
        q.borrow_mut().push(0, || ScriptCommand::ClearTimeline)
    })?;
    let q = api.queue();
    api.set("setMagnet", move |on: bool| {
        q.borrow_mut().push(0, || ScriptCommand::SetMagnet(on))
    })?;
    let q = api.queue();
    api.set("__set_format", move |name: CString<'_>| {
        q.borrow_mut().push(name.len(), || {
            ScriptCommand::SetFormat(name.as_str().to_owned())
        })
    })?;
    let q = api.queue();
    api.set("setMusicBedMuted", move |on: bool| {
        q.borrow_mut()
            .push(0, || ScriptCommand::SetMusicBedMuted(on))
    })?;
    let q = api.queue();
    api.set("setZoom", move |px: f32| {
        q.borrow_mut().push(0, || ScriptCommand::SetZoom(px))
    })?;
    let q = api.queue();
    api.set("closeGaps", move || {
        q.borrow_mut().push(0, || ScriptCommand::CloseGaps)
    })?;
    let q = api.queue();
    api.set(
        "__add_marker",
        move |at: f64, name: CString<'_>, color: CString<'_>| {
            q.borrow_mut()
                .push(name.len() + color.len(), || ScriptCommand::AddMarker {
                    at,
                    name: name.as_str().to_owned(),
                    color: color.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set(
        "__set_marker",
        move |Whole(marker): Whole, changes: CString<'_>| {
            q.borrow_mut()
                .push(changes.len(), || ScriptCommand::SetMarker {
                    marker,
                    changes: changes.as_str().to_owned(),
                })
        },
    )?;
    let q = api.queue();
    api.set("removeMarker", move |Whole(marker): Whole| {
        q.borrow_mut()
            .push(0, || ScriptCommand::RemoveMarker(marker))
    })?;
    let q = api.queue();
    api.set("clearMarkers", move || {
        q.borrow_mut().push(0, || ScriptCommand::ClearMarkers)
    })?;
    Ok(())
}

/// Native helpers the prelude calls; they queue nothing.
fn helpers(api: &Api<'_, '_>) -> rquickjs::Result<()> {
    api.set(
        "__resolve_time",
        |ctx: rquickjs::Ctx<'_>,
         spec: String,
         fps: f64,
         end: f64,
         clips: Vec<f64>,
         markers: Vec<f64>,
         range: bool| {
            let index = openreel_engine::time_spec::TimeIndex {
                fps,
                end,
                clips: clips
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|clip| (clip[0] as u64, clip[1], clip[2]))
                    .collect(),
                markers: markers
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|marker| (marker[0] as u64, marker[1]))
                    .collect(),
            };
            let resolved = if range {
                index.range(&spec).map(|(start, end)| vec![start, end])
            } else {
                index.time(&spec).map(|time| vec![time])
            };
            resolved.map_err(|error| rquickjs::Exception::throw_message(&ctx, &error))
        },
    )?;
    // One native call the VM cannot interrupt, without the
    // megabyte strings a real one needs.
    #[cfg(test)]
    api.set("__test_stall", |ms: u64| {
        std::thread::sleep(Duration::from_millis(ms))
    })?;
    Ok(())
}

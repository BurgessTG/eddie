//! What can be asked of an editor from outside: a method name and JSON
//! parameters. The command line runs these on a project file and the live
//! server on a running editor, so a request means the same thing either way.
//! Paths arrive absolute; the command line resolves them where it was run.

use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::thread;
use std::time::Instant;

use serde_json::{json, Value};

use super::media::AssetState;
use super::sweep::transcribe::{self, Provider, TranscribeSettings};
use super::sweep::SweepStatus;
use super::{inspect, pictures, Editor};
use crate::scripting::ScriptEngine;

mod job;

pub use job::{Job, TranscribeMsg};

/// A finished request: the answer, and whether the project file is now
/// behind the editor.
pub struct Outcome {
    pub value: Value,
    pub changed: bool,
}

impl Outcome {
    fn read(value: Value) -> Self {
        Self {
            value,
            changed: false,
        }
    }
}

pub enum Reply {
    Done(Outcome),
    /// Still working; `Job::poll` each frame until it answers.
    Later(Job),
}

/// What a job reports while it works.
pub enum Note {
    /// One line for whoever asked; the command line prints it on stderr.
    Progress(String),
    /// For everyone listening to a live editor.
    Event(Value),
}

/// Methods that change the timeline, which a window holds back while the
/// user is mid-drag.
pub fn edits_timeline(method: &str) -> bool {
    matches!(method, "run" | "undo" | "redo")
}

pub fn start(app: &mut Editor, method: &str, params: &Value) -> Result<Reply, String> {
    let render = |app: &Editor, render: pictures::Render| {
        let (tx, rx) = channel();
        let wake = app.wake.clone();
        thread::spawn(move || {
            let _ = tx.send(render());
            wake.wake();
        });
        Reply::Later(Job::Render(rx))
    };
    Ok(match method {
        "state" if !app.mix_measured() => measure(app, method, params),
        "state" => Reply::Done(Outcome::read(
            serde_json::from_str(&app.script_state_json()).map_err(|error| error.to_string())?,
        )),
        "inspect" => Reply::Done(Outcome::read(json!({
            "findings": inspect::inspect(&app.timeline, &app.assets),
        }))),
        "run" => run(app, params)?,
        "undo" | "redo" => {
            let changed = app.step_history(method == "redo");
            Reply::Done(Outcome {
                value: json!({
                    "changed": changed,
                    "saved": false,
                    "status": app.status,
                    "summary": summary(app),
                }),
                changed,
            })
        }
        "import" => {
            let media = paths(params, "media")?;
            if media.is_empty() {
                return Err("missing <media...>".to_owned());
            }
            app.spawn_import(media.clone());
            Reply::Later(Job::Import(media))
        }
        "transcribe" => transcribe(app, params)?,
        "curate" => curate(app, params)?,
        "sheet" => {
            let sheet = pictures::sheet(app, path(params, "output")?, params["range"].as_str())?;
            render(app, sheet)
        }
        "strip" => {
            let strip = pictures::strip(app, text(params, "asset")?, path(params, "output")?)?;
            render(app, strip)
        }
        "frames" => {
            let positive = |key: &str| match &params[key] {
                Value::Null => Ok(None),
                value => value
                    .as_f64()
                    .filter(|number| number.is_finite() && *number > 0.0)
                    .map(Some)
                    .ok_or_else(|| format!("{key} needs a positive number")),
            };
            let frames = pictures::frames(
                app,
                text(params, "asset")?,
                text(params, "range")?,
                path(params, "directory")?,
                positive("every")?,
                positive("width")?.map(|width| width as u32),
            )?;
            render(app, frames)
        }
        "export" => {
            let output = path(params, "output")?;
            if !app.missing_media.is_empty() {
                return Err(format!(
                    "{} media file(s) are missing: {}",
                    app.missing_media.len(),
                    app.missing_media.join(", ")
                ));
            }
            if app.exporting() {
                return Err("An export is already running.".to_owned());
            }
            app.start_export(output);
            if !app.exporting() {
                return Err(app.status.clone());
            }
            Reply::Later(Job::Export {
                started: Instant::now(),
                reported: 0.0,
            })
        }
        other => return Err(format!("unknown method '{other}'")),
    })
}

fn text<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params[key].as_str().ok_or_else(|| format!("missing {key}"))
}

fn absolute(text: &str, key: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(text);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(format!("{key} must be an absolute path, not {text}"))
    }
}

fn path(params: &Value, key: &str) -> Result<PathBuf, String> {
    absolute(text(params, key)?, key)
}

fn strings<'a>(params: &'a Value, key: &str) -> Result<Vec<&'a str>, String> {
    match &params[key] {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().ok_or_else(|| format!("{key} takes strings")))
            .collect(),
        _ => Err(format!("{key} takes a list")),
    }
}

fn paths(params: &Value, key: &str) -> Result<Vec<PathBuf>, String> {
    strings(params, key)?
        .into_iter()
        .map(|text| absolute(text, key))
        .collect()
}

pub fn summary(app: &Editor) -> Value {
    json!({
        "project": app.project_path.as_deref().map(Path::to_string_lossy),
        "clips": app.timeline.clips.len(),
        "duration": app.timeline.end(),
        "assets": app.assets.iter().map(|asset| &asset.info.name).collect::<Vec<_>>(),
        "missingMedia": app.missing_media,
    })
}

/// Answer `method` once the mix it reads has been measured.
fn measure(app: &mut Editor, method: &str, params: &Value) -> Reply {
    app.measure_mix();
    Reply::Later(Job::Measure {
        method: method.to_owned(),
        params: params.clone(),
    })
}

/// Run a script as one edit and one undo step. A dry run reports what the
/// edit would do, then puts the editor back.
fn run(app: &mut Editor, params: &Value) -> Result<Reply, String> {
    let code = text(params, "code")?;
    // "true" or 1 must not run for real.
    let dry_run = match &params["dryRun"] {
        Value::Null => false,
        Value::Bool(dry_run) => *dry_run,
        _ => return Err("dryRun is true or false".to_owned()),
    };
    let findings = inspect::inspect(&app.timeline, &app.assets);
    // Whatever the user had just changed is its own step, not this one's.
    app.commit_history();
    let before = app.edit_snapshot();
    let mut engine = ScriptEngine::new()?;
    let evaluated = engine.eval_checked(code, &app.script_state_json());
    if engine.read_audio() && !app.mix_measured() {
        // It looked at a mix not measured yet. Nothing it queued has been
        // applied, so measure and run it again.
        return Ok(measure(app, "run", params));
    }
    let (output, commands) = evaluated?;
    app.rehearsing = dry_run;
    let applied = app.apply_edit(commands);
    if applied.is_err() {
        // `apply_edit` has already put a refused edit back.
        app.rehearsing = false;
    }
    let (notes, handles) = applied?;
    let changed = app.differs_from(&before);
    let introduced: Vec<_> = inspect::inspect(&app.timeline, &app.assets)
        .into_iter()
        .filter(|finding| !findings.contains(finding))
        .collect();
    let mut value = json!({
        "changed": changed,
        "saved": false,
        "output": (output != "undefined").then_some(output),
        "notes": notes,
        "handles": handles,
        "introducedFindings": introduced,
        "summary": summary(app),
    });
    if dry_run {
        // Nothing is kept, so this reply is the only place to read the
        // timeline the edit would leave.
        value["clips"] = app.script_clips().into();
        app.restore_edit(before);
        app.rehearsing = false;
    } else {
        app.commit_history();
    }
    Ok(Reply::Done(Outcome {
        value,
        changed: changed && !dry_run,
    }))
}

/// The saved speech settings, overridden per request.
fn transcribe_settings(app: &Editor, params: &Value) -> Result<TranscribeSettings, String> {
    let models = app.model_dir.as_deref();
    let mut settings = app.settings.transcription(models);
    if let Some(name) = params["provider"].as_str() {
        let provider = Provider::from_name(name)
            .ok_or_else(|| format!("unknown provider '{name}' (local, meta, openai)"))?;
        if provider != settings.provider {
            // Another provider's key and endpoint do not carry over.
            let mut other = app.settings.clone();
            other.transcription = TranscribeSettings {
                provider,
                ..Default::default()
            };
            settings = other.transcription(models);
        }
    }
    if let Some(key) = params["apiKey"].as_str().map(str::trim) {
        if !key.is_empty() {
            settings.api_key = key.to_owned();
        }
    }
    for (key, field) in [
        ("model", &mut settings.model),
        ("language", &mut settings.language),
        ("vocabulary", &mut settings.vocabulary),
    ] {
        if let Some(value) = params[key].as_str() {
            *field = value.to_owned();
        }
    }
    Ok(settings)
}

/// Transcribe the named assets, or every asset with sound, off the editor's
/// thread. One file failing does not stop the rest.
fn transcribe(app: &mut Editor, params: &Value) -> Result<Reply, String> {
    let settings = transcribe_settings(app, params)?;
    let force = params["force"].as_bool().unwrap_or(false);
    let wanted = strings(params, "assets")?;
    let mut targets = Vec::new();
    for name in &wanted {
        let asset = app.asset_named(name)?;
        targets.push(asset.info.clone());
    }
    if wanted.is_empty() {
        targets = app
            .assets
            .iter()
            .filter(|asset| asset.state == AssetState::Ready && asset.info.has_audio)
            .map(|asset| asset.info.clone())
            .collect();
    }
    let store = app.curation_dir.clone();
    let wake = app.wake.clone();
    let (tx, rx) = channel();
    thread::spawn(move || {
        for info in targets {
            let started = Instant::now();
            let mut progress = |message: &str| {
                let _ = tx.send(TranscribeMsg::Progress(
                    info.name.clone(),
                    message.to_owned(),
                ));
                wake.wake();
            };
            let result = transcribe::transcribe_cached(
                &settings,
                store.as_deref(),
                &info,
                force,
                &mut progress,
            );
            let _ = tx.send(TranscribeMsg::Done {
                path: info.path.clone(),
                name: info.name.clone(),
                seconds: started.elapsed().as_secs_f64(),
                result,
            });
            wake.wake();
        }
        // The job ends when it sees the channel closed, so close it before
        // the wake that makes it look.
        drop(tx);
        wake.wake();
    });
    Ok(Reply::Later(Job::Transcribe {
        rx,
        results: Vec::new(),
        failed: false,
    }))
}

/// Curate the named assets, or every ready one never curated. A saved
/// curation answers at once unless `force` asks for a new one.
fn curate(app: &mut Editor, params: &Value) -> Result<Reply, String> {
    let force = params["force"].as_bool().unwrap_or(false);
    let wanted = strings(params, "assets")?;
    let mut targets = Vec::new();
    for name in &wanted {
        let asset = app.asset_named(name)?;
        targets.push(asset.info.path.clone());
    }
    if wanted.is_empty() {
        targets = app
            .assets
            .iter()
            .filter(|asset| asset.state == AssetState::Ready)
            .map(|asset| asset.info.path.clone())
            .collect();
    }
    let mut saved = Vec::new();
    for path in &targets {
        let ready = app
            .asset(path)
            .is_some_and(|asset| asset.sweep.status == SweepStatus::Ready);
        if ready && !force {
            saved.push(path.clone());
        } else {
            // False only when it is already queued or running: wait for that.
            app.queue_curation(path.clone());
        }
    }
    Ok(Reply::Later(Job::Curate {
        assets: targets
            .into_iter()
            .map(|path| (path, String::new()))
            .collect(),
        saved,
    }))
}

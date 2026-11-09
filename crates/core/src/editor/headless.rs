//! The `eddie` command line: the editor without a window, for agents and
//! scripts. Each command loads a project file, does one thing, saves when
//! it changed the project, and prints one JSON object on stdout.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::sweep::transcribe::{self, Provider};
use super::{inspect, Editor};
use crate::scripting::ScriptEngine;

const USAGE: &str = "\
Usage: eddie <command> <project.eddie> ...

  new <project> [media...]           create a project, importing media
  import <project> <media...>        add media to a project's bin
  state <project>                    the state JSON scripts see
  inspect <project>                  measured timeline problems
  run <project> <file.js> [--dry-run]
  run <project> -e <code> [--dry-run]
                                     run a script as one edit: every command
                                     applies, or none does. Each run starts
                                     fresh, and importMedia/exportTo are
                                     refused: use import and export
  transcribe <project> [asset...] [--provider local|meta|openai]
             [--model M] [--language en] [--vocabulary \"Eddie, Wonder\"]
             [--force]
                                     transcribe media (all with sound when
                                     none is named); saved per file, so only
                                     --force transcribes again
  export <project> <out.mp4>         render the timeline
  help                               this text and the script API

Every command prints one JSON object. Errors print {\"ok\":false,...} and
exit 1. Without a command, eddie opens the editor window.";

const COMMANDS: &[&str] = &[
    "new",
    "import",
    "state",
    "inspect",
    "run",
    "transcribe",
    "export",
    "help",
];

/// Run a command-line command, or `None` when `args` (without the program
/// name) do not start with one, so the window should open instead.
pub fn cli_main(args: &[OsString]) -> Option<i32> {
    let command = args.first()?.to_str()?;
    if !COMMANDS.contains(&command) && command != "--help" && command != "-h" {
        return None;
    }
    let args: Vec<String> = args[1..]
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let result = match command {
        "help" | "--help" | "-h" => {
            println!("{USAGE}\n\nScript API:\n{}", script_help());
            return Some(0);
        }
        "new" => new(&args),
        "import" => import(&args),
        "state" => state(&args),
        "inspect" => inspect_project(&args),
        "run" => run(&args),
        "transcribe" => transcribe(&args),
        "export" => export(&args),
        _ => unreachable!(),
    };
    let (code, value) = match result {
        Ok(value) => (if value["ok"] == false { 1 } else { 0 }, value),
        Err(error) => (1, json!({ "ok": false, "error": error })),
    };
    println!("{value}");
    Some(code)
}

fn script_help() -> String {
    ScriptEngine::new()
        .map(|mut engine| engine.eval("help()", "{}").0)
        .unwrap_or_else(|error| format!("scripting unavailable: {error}"))
}

/// A windowless editor. Saved curation returns on import, as in the window.
fn editor() -> Editor {
    let mut app = Editor::headless();
    app.curation_dir = super::sweep::default_curation_dir();
    app.model_dir = transcribe::installed_model_dir();
    app.make_thumbnails = false;
    app
}

fn project_arg(args: &[String]) -> Result<PathBuf, String> {
    let path = args.first().ok_or("missing <project>")?;
    Ok(std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path)))
}

fn open(path: &Path) -> Result<Editor, String> {
    let mut app = editor();
    app.open_project(path)?;
    wait_for_imports(&mut app);
    Ok(app)
}

fn wait_for_imports(app: &mut Editor) {
    while app.import_rx.is_some() || !app.import_queue.is_empty() {
        app.poll_imports();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn save(app: &mut Editor, path: &Path) -> Result<(), String> {
    app.save_project_to(path)
}

/// Import `media` into the bin and report what came in and what did not.
fn import_media(app: &mut Editor, media: &[String]) -> Value {
    let paths: Vec<PathBuf> = media
        .iter()
        .map(|path| std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path)))
        .collect();
    app.spawn_import(paths.clone());
    wait_for_imports(app);
    let (imported, failed): (Vec<_>, Vec<_>) = paths
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .partition(|path| app.asset(path).is_some());
    json!({ "imported": imported, "failed": failed })
}

fn summary(app: &Editor, path: &Path) -> Value {
    json!({
        "project": path,
        "clips": app.timeline.clips.len(),
        "duration": app.timeline.end(),
        "assets": app.assets.iter().map(|asset| &asset.info.name).collect::<Vec<_>>(),
        "missingMedia": app.missing_media,
    })
}

fn new(args: &[String]) -> Result<Value, String> {
    let path = project_arg(args)?;
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    let mut app = editor();
    let media = import_media(&mut app, &args[1..]);
    save(&mut app, &path)?;
    let path = app.project_path.clone().unwrap_or(path);
    Ok(json!({ "ok": true, "media": media, "summary": summary(&app, &path) }))
}

fn import(args: &[String]) -> Result<Value, String> {
    let path = project_arg(args)?;
    if args.len() < 2 {
        return Err("missing <media...>".to_owned());
    }
    let mut app = open(&path)?;
    let media = import_media(&mut app, &args[1..]);
    save(&mut app, &path)?;
    Ok(json!({ "ok": true, "media": media, "summary": summary(&app, &path) }))
}

fn state(args: &[String]) -> Result<Value, String> {
    let app = open(&project_arg(args)?)?;
    serde_json::from_str(&app.script_state_json()).map_err(|error| error.to_string())
}

fn inspect_project(args: &[String]) -> Result<Value, String> {
    let app = open(&project_arg(args)?)?;
    Ok(json!({
        "ok": true,
        "findings": inspect::inspect(&app.timeline, &app.assets),
    }))
}

fn run(args: &[String]) -> Result<Value, String> {
    let path = project_arg(args)?;
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let rest: Vec<&String> = args[1..].iter().filter(|arg| *arg != "--dry-run").collect();
    let code = match rest.as_slice() {
        [flag, code] if flag.as_str() == "-e" => (*code).clone(),
        [file] => std::fs::read_to_string(file)
            .map_err(|error| format!("couldn't read {file}: {error}"))?,
        _ => return Err("run needs <file.js> or -e <code>".to_owned()),
    };
    let mut app = open(&path)?;
    let before = inspect::inspect(&app.timeline, &app.assets);
    let saved = app.timeline.clone();
    let mut engine = ScriptEngine::new()?;
    let (output, commands) = engine.eval_checked(&code, &app.script_state_json())?;
    let (notes, handles) = app.apply_edit(commands)?;
    let changed = app.timeline != saved;
    let introduced: Vec<_> = inspect::inspect(&app.timeline, &app.assets)
        .into_iter()
        .filter(|finding| !before.contains(finding))
        .collect();
    if changed && !dry_run {
        save(&mut app, &path)?;
    }
    Ok(json!({
        "ok": true,
        "changed": changed,
        "saved": changed && !dry_run,
        "output": (output != "undefined").then_some(output),
        "notes": notes,
        "handles": handles,
        "introducedFindings": introduced,
        "summary": summary(&app, &path),
    }))
}

/// Transcribe with the saved settings, overridden by flags and by
/// `EDDIE_TRANSCRIBE_API_KEY`. One file failing does not stop the rest.
fn transcribe(args: &[String]) -> Result<Value, String> {
    let path = project_arg(args)?;
    let saved = super::Settings::load();
    let models = transcribe::installed_model_dir();
    let mut settings = saved.transcription(models.as_deref());
    if let Ok(key) = std::env::var("EDDIE_TRANSCRIBE_API_KEY") {
        settings.api_key = key.trim().to_owned();
    }
    let mut force = false;
    let mut wanted = Vec::new();
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        let mut value = |flag: &str| {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--force" => force = true,
            "--provider" => {
                let name = value("--provider")?;
                let provider = Provider::from_name(&name)
                    .ok_or_else(|| format!("unknown provider '{name}' (local, meta, openai)"))?;
                if provider != settings.provider {
                    // Another provider's key and endpoint do not carry over.
                    let mut other = saved.clone();
                    other.transcription = transcribe::TranscribeSettings {
                        provider,
                        ..Default::default()
                    };
                    settings = other.transcription(models.as_deref());
                    if let Ok(key) = std::env::var("EDDIE_TRANSCRIBE_API_KEY") {
                        settings.api_key = key.trim().to_owned();
                    }
                }
            }
            "--model" => settings.model = value("--model")?,
            "--language" => settings.language = value("--language")?,
            "--vocabulary" => settings.vocabulary = value("--vocabulary")?,
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            asset => wanted.push(asset.to_owned()),
        }
    }
    let app = open(&path)?;
    let mut targets = Vec::new();
    for name in &wanted {
        let asset = app
            .assets
            .iter()
            .find(|asset| asset.info.path == *name || asset.info.name == *name)
            .ok_or_else(|| format!("no asset '{name}' in the project"))?;
        targets.push(asset.info.clone());
    }
    if wanted.is_empty() {
        targets = app
            .assets
            .iter()
            .filter(|asset| asset.info.has_audio)
            .map(|asset| asset.info.clone())
            .collect();
    }
    let store = app.curation_dir.clone();
    let mut results = Vec::new();
    let mut failed = false;
    for info in targets {
        let started = Instant::now();
        let mut progress = |message: &str| eprintln!("{}: {message}", info.name);
        let result =
            transcribe::transcribe_cached(&settings, store.as_deref(), &info, force, &mut progress);
        let seconds = started.elapsed().as_secs_f64();
        results.push(match result {
            Ok(transcript) => {
                let words: usize = transcript.segments.iter().map(|s| s.words.len()).sum();
                let entry = json!({
                    "asset": info.name,
                    "turns": transcript.segments.len(),
                    "words": words,
                    "source": transcript.source,
                    "saved": transcript.cached,
                    "seconds": seconds,
                    "note": transcript.note,
                });
                entry
            }
            Err(error) => {
                failed = true;
                json!({ "asset": info.name, "error": error })
            }
        });
    }
    Ok(json!({ "ok": !failed, "assets": results }))
}

fn export(args: &[String]) -> Result<Value, String> {
    let path = project_arg(args)?;
    let output = args.get(1).ok_or("missing <out.mp4>")?;
    let output = std::path::absolute(output).unwrap_or_else(|_| PathBuf::from(output));
    let mut app = open(&path)?;
    if !app.missing_media.is_empty() {
        return Err(format!(
            "{} media file(s) are missing: {}",
            app.missing_media.len(),
            app.missing_media.join(", ")
        ));
    }
    app.start_export(output);
    if app.export_rx.is_none() {
        return Err(app.status.clone());
    }
    let started = Instant::now();
    let mut reported = 0.0;
    while app.export_rx.is_some() {
        if app.poll_export() {
            let (file, _) = app.exported.clone().expect("finished export");
            return Ok(json!({
                "ok": true,
                "output": file,
                "seconds": started.elapsed().as_secs_f64(),
            }));
        }
        let progress = app.export_progress.unwrap_or(0.0);
        if progress >= reported + 0.1 {
            reported = progress;
            eprintln!("export {:.0}%", progress * 100.0);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(app.status.clone())
}

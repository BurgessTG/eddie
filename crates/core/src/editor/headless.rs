//! The `eddie` command line: the editor without a window, for agents and
//! scripts. A command names a project and asks for one thing (`commands`).
//! When an editor already has that project open (a window, or `eddie
//! serve`), the command is forwarded to it; otherwise it loads the project
//! file, acts, and saves when it changed the project. Either way it prints
//! one JSON object on stdout.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver};
use std::time::Instant;

use serde_json::{json, Value};

use super::commands::{self, Job, Note, Outcome, Reply};
use super::sweep::transcribe;
use super::{server, Editor, NoPreview, Settings, Wake};
use crate::scripting::ScriptEngine;

/// The guide for agents, as written for this build.
const GUIDE: &str = include_str!("../../../../docs/agents.md");

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
                                     refused: use import and export.
                                     --dry-run keeps nothing and adds `clips`,
                                     the timeline the edit would leave
  transcribe <project> [asset...] [--provider local|meta|openai]
             [--model M] [--language en] [--vocabulary \"Eddie, Wonder\"]
             [--force]
                                     transcribe media (all with sound when
                                     none is named); saved per file, so only
                                     --force transcribes again
  curate <project> [asset...] [--force]
                                     find shots and rank ranges in media (all
                                     when none is named), for sceneClips() and
                                     sweepCandidates(). Transcribes files with
                                     no saved transcript. Saved per file, so
                                     only --force curates again
  sheet <project> <out.jpg> [range]  the program as pictures: one labelled
                                     cell per visible shot, titles drawn; 16
                                     cells at most, so a timeline range
                                     (10..25, clip:7) shows a part in full.
                                     `shots` counts pictures, not captions
  strip <project> <asset> <out.jpg>  16 pictures through one source, marked *
                                     where the timeline already shows it
  frames <project> <asset> <range|time> <out-dir> [--every seconds]
         [--width px]                timestamped pictures of a source range,
                                     one JPEG each, 40 at most (16 spread over
                                     the range without --every). Times are
                                     source times; clip:7 is the source range
                                     clip 7 shows
  export <project> <out.mp4>         render the timeline
  help                               this text and the script API
  guide                              the guide for agents: the API, the time
                                     grammar and the rules that prevent the
                                     usual mistakes

Live editing. While a project is open in the Eddie window or in `serve`,
every command above goes to that editor instead of the file: edits show in
the window, each `run` is one Undo step, and replies carry \"live\":true.

  serve <project>                    keep the project open without a window:
                                     media and decoders stay loaded between
                                     commands and every change is saved
  stop <project>                     end `serve`
  undo <project> | redo <project>    step the running editor's history
  save <project>                     write the project file now (a window
                                     saves only when asked; `serve` always)
  events <project>                   one JSON line per change in the running
                                     editor, until it closes

Every command but help and guide prints one JSON object. Errors print
{\"ok\":false,...} and exit 1. `saved` says whether the project file was
written; transcribe and curate mark a file answered from its saved result
with \"reused\":true.
Without a command, eddie opens the editor window.";

const COMMANDS: &[&str] = &[
    "new",
    "import",
    "state",
    "inspect",
    "run",
    "transcribe",
    "curate",
    "sheet",
    "strip",
    "frames",
    "export",
    "serve",
    "stop",
    "undo",
    "redo",
    "save",
    "events",
    "help",
    "guide",
];

/// Run a command-line command, or `None` when `args` (without the program
/// name) are empty or start with a flag or a file to open, so the window
/// should open instead. Any other first word is a mistyped command: an
/// agent waiting on JSON must not get a window.
pub fn cli_main(args: &[OsString]) -> Option<i32> {
    let first = args.first()?;
    let known = |word: &&str| COMMANDS.contains(word) || matches!(*word, "--help" | "-h");
    let Some(command) = first.to_str().filter(known) else {
        let word = first.to_string_lossy();
        if word.starts_with('-') || Path::new(first).exists() {
            return None;
        }
        let error = format!(
            "unknown command '{word}' (and no such file to open); `eddie help` lists the commands"
        );
        println!("{}", json!({ "ok": false, "error": error }));
        return Some(1);
    };
    let args: Vec<String> = args[1..]
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let result = match command {
        "help" | "--help" | "-h" => {
            println!("{USAGE}\n\nScript API:\n{}", script_help());
            return Some(0);
        }
        "guide" => {
            print!("{GUIDE}");
            return Some(0);
        }
        "new" => new(&args),
        "serve" => serve(&args),
        "events" => events(&args),
        _ => request(command, &args),
    };
    let (code, value) = match result {
        Ok(value) => (if value["ok"] == false { 1 } else { 0 }, value),
        Err(error) => (1, json!({ "ok": false, "error": error })),
    };
    // A reader that went away (`eddie events | head`) is not an error.
    let _ = writeln!(std::io::stdout(), "{value}");
    Some(code)
}

fn script_help() -> String {
    ScriptEngine::new()
        .map(|mut engine| engine.eval("help()", "{}").0)
        .unwrap_or_else(|error| format!("scripting unavailable: {error}"))
}

fn absolute(path: &str) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path))
}

fn project_arg(args: &[String]) -> Result<PathBuf, String> {
    Ok(absolute(args.first().ok_or("missing <project>")?))
}

/// A command's arguments as the request it makes. Paths become absolute
/// here, where the command was run; an editor elsewhere serves them.
fn params(command: &str, args: &[String]) -> Result<Value, String> {
    let mut flags = Vec::new();
    let mut plain = Vec::new();
    let mut values = serde_json::Map::new();
    let (switches, options): (&[&str], &[&str]) = match command {
        "run" => (&["--dry-run"], &["-e"]),
        "transcribe" => (
            &["--force"],
            &["--provider", "--model", "--language", "--vocabulary"],
        ),
        "curate" => (&["--force"], &[]),
        "frames" => (&[], &["--every", "--width"]),
        _ => (&[], &[]),
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if switches.contains(&arg.as_str()) {
            flags.push(arg.as_str());
        } else if options.contains(&arg.as_str()) {
            let value = rest.next().ok_or_else(|| format!("{arg} needs a value"))?;
            values.insert(arg.trim_start_matches('-').to_owned(), json!(value));
        } else if arg.starts_with("--") {
            return Err(format!("unknown option {arg}"));
        } else {
            plain.push(arg.as_str());
        }
    }
    let flag = |name: &str| flags.contains(&name);
    let at = |index: usize, name: &str| {
        plain
            .get(index)
            .copied()
            .ok_or_else(|| format!("missing {name}"))
    };
    let at_most = |count: usize| match plain.get(count) {
        Some(extra) => Err(format!("unexpected argument '{extra}'")),
        None => Ok(()),
    };
    let positive = |key: &str| match values.get(key) {
        None => Ok(Value::Null),
        Some(value) => value
            .as_str()
            .and_then(|text| text.parse::<f64>().ok())
            .filter(|number| number.is_finite() && *number > 0.0)
            .map(|number| json!(number))
            .ok_or_else(|| format!("--{key} needs a positive number")),
    };
    Ok(match command {
        "run" => {
            let code = match (values.get("e"), plain.as_slice()) {
                (Some(code), []) => code.clone(),
                (None, [file]) => json!(std::fs::read_to_string(file)
                    .map_err(|error| format!("couldn't read {file}: {error}"))?),
                _ => return Err("run needs <file.js> or -e <code>".to_owned()),
            };
            json!({ "code": code, "dryRun": flag("--dry-run") })
        }
        "import" => {
            if plain.is_empty() {
                return Err("missing <media...>".to_owned());
            }
            json!({ "media": plain.iter().map(|path| absolute(path)).collect::<Vec<_>>() })
        }
        "transcribe" => {
            let mut request = json!({ "assets": plain, "force": flag("--force") });
            for key in ["provider", "model", "language", "vocabulary"] {
                if let Some(value) = values.get(key) {
                    request[key] = value.clone();
                }
            }
            if let Ok(key) = std::env::var("EDDIE_TRANSCRIBE_API_KEY") {
                request["apiKey"] = json!(key);
            }
            request
        }
        "curate" => json!({ "assets": plain, "force": flag("--force") }),
        "sheet" => {
            at_most(2)?;
            json!({ "output": absolute(at(0, "<out.jpg>")?), "range": plain.get(1) })
        }
        "strip" => {
            at_most(2)?;
            json!({ "asset": at(0, "<asset>")?, "output": absolute(at(1, "<out.jpg>")?) })
        }
        "frames" => {
            at_most(3)?;
            json!({
                "asset": at(0, "<asset>")?,
                "range": at(1, "<range|time>")?,
                "directory": absolute(at(2, "<out-dir>")?),
                "every": positive("every")?,
                "width": positive("width")?,
            })
        }
        "export" => {
            at_most(1)?;
            json!({ "output": absolute(at(0, "<out.mp4>")?) })
        }
        _ => {
            at_most(0)?;
            json!({})
        }
    })
}

/// A windowless editor that sleeps until a worker has something for it.
struct Session {
    app: Editor,
    woken: Receiver<()>,
    started: Instant,
}

impl Session {
    /// Saved curation returns on import and transcription uses the saved
    /// speech provider, as in the window.
    fn new() -> Self {
        let (wake, woken) = sync_channel(1);
        let wake = Wake::new(move || {
            // One pending wake is enough: the next frame sees everything.
            let _ = wake.try_send(());
        });
        let mut app = Editor::headless_with(wake, Box::new(NoPreview));
        app.settings = Settings::load();
        app.curation_dir = super::sweep::default_curation_dir();
        app.model_dir = transcribe::installed_model_dir();
        app.make_thumbnails = false;
        Self {
            app,
            woken,
            started: Instant::now(),
        }
    }

    fn open(path: &Path) -> Result<Self, String> {
        let mut session = Self::new();
        session.app.open_project(path)?;
        session.until(|app| (!importing(app)).then_some(()));
        Ok(session)
    }

    /// Run frames until `poll` answers, as a window would draw them.
    fn until<T>(&mut self, mut poll: impl FnMut(&mut Editor) -> Option<T>) -> T {
        loop {
            self.app.begin_frame(self.started.elapsed().as_secs_f64());
            let answer = poll(&mut self.app);
            self.app.end_frame(false, false);
            if let Some(answer) = answer {
                return answer;
            }
            match self.app.repaint_after() {
                Some(after) => drop(self.woken.recv_timeout(after)),
                None => drop(self.woken.recv()),
            }
        }
    }

    /// Wait for a job, printing its progress on stderr.
    fn finish(&mut self, mut job: Job) -> Result<Outcome, String> {
        self.until(|app| {
            let mut notes = Vec::new();
            let answer = job.poll(app, &mut notes);
            for note in notes {
                if let Note::Progress(line) = note {
                    eprintln!("{line}");
                }
            }
            answer
        })
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Outcome, String> {
        match commands::start(&mut self.app, method, params)? {
            Reply::Done(outcome) => Ok(outcome),
            Reply::Later(job) => self.finish(job),
        }
    }
}

fn importing(app: &Editor) -> bool {
    app.import_rx.is_some() || !app.import_queue.is_empty()
}

fn new(args: &[String]) -> Result<Value, String> {
    let path = project_arg(args)?;
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    let mut session = Session::new();
    let media = if args.len() > 1 {
        let request = params("import", &args[1..])?;
        session.request("import", &request)?.value["media"].take()
    } else {
        json!({ "imported": [], "failed": [] })
    };
    session.app.save_project_to(&path)?;
    Ok(json!({
        "ok": true,
        "media": media,
        "saved": true,
        "summary": commands::summary(&session.app),
    }))
}

/// Ask the editor that has the project open, or be that editor for one
/// request.
fn request(command: &str, args: &[String]) -> Result<Value, String> {
    let project = project_arg(args)?;
    let request = params(command, &args[1..])?;
    let state = command == "state";
    if let Some(mut live) = server::connect(&project) {
        let mut value = live.request(command, &request, &mut |line| eprintln!("{line}"))?;
        if let Some(object) = value.as_object_mut() {
            // `state` is exactly what scripts see, here as there.
            if state && object.get("ok") == Some(&json!(true)) {
                object.remove("ok");
            } else if !state {
                object.insert("live".to_owned(), json!(true));
            }
        }
        return Ok(value);
    }
    if matches!(command, "stop" | "undo" | "redo" | "save") {
        return Err(format!(
            "{command} needs a running editor: open the project in Eddie, or run eddie serve"
        ));
    }
    let mut session = Session::open(&project)?;
    let outcome = session.request(command, &request)?;
    let mut value = outcome.value;
    if outcome.changed {
        session.app.save_project_to(&project)?;
        value["saved"] = json!(true);
    }
    if !state && value.get("ok").is_none() {
        value["ok"] = json!(true);
    }
    Ok(value)
}

/// Keep the project open and answer requests until `eddie stop`. Prints one
/// line when it is listening and one when it stops.
fn serve(args: &[String]) -> Result<Value, String> {
    let project = project_arg(args)?;
    params("serve", &args[1..])?;
    let mut session = Session::open(&project)?;
    let socket = session.app.serve()?;
    let ready = json!({
        "ok": true,
        "serving": project,
        "socket": socket,
        "pid": std::process::id(),
    });
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{ready}");
    let _ = stdout.flush();
    session.until(|app| app.serve_stopped().then_some(()));
    // Dropping the editor sends the answer to `stop` before this exits.
    drop(session);
    Ok(json!({ "ok": true, "stopped": project }))
}

fn events(args: &[String]) -> Result<Value, String> {
    let project = project_arg(args)?;
    params("events", &args[1..])?;
    let mut live = server::connect(&project)
        .ok_or("events needs a running editor: open the project in Eddie, or run eddie serve")?;
    let mut stdout = std::io::stdout();
    live.events(&mut |line| {
        writeln!(stdout, "{line}")
            .and_then(|()| stdout.flush())
            .is_ok()
    })?;
    Ok(json!({ "ok": true, "closed": project }))
}

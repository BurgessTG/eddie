//! Installing an update over the running `Eddie.app`.
//!
//! The new app is unzipped beside the running one, checked (a valid
//! signature from the same Developer ID team, and Gatekeeper accepts it),
//! then swapped in by two renames. A failure at any step leaves the running
//! app where it was.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Release, RELEASES};

/// The `.app` this program runs from.
fn running_app() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("Couldn't find the running Eddie: {error}"))?;
    // Eddie.app/Contents/MacOS/eddie
    let app = exe
        .ancestors()
        .nth(3)
        .filter(|app| app.extension().is_some_and(|ext| ext == "app"))
        .ok_or_else(|| {
            format!("Eddie is not running from an app, so it does not update itself. Download it from {RELEASES}.")
        })?;
    if app.to_string_lossy().contains("/AppTranslocation/") {
        return Err(
            "Move Eddie to the Applications folder, open it from there, then update.".to_owned(),
        );
    }
    Ok(app.to_path_buf())
}

/// The Developer ID team that signed `app`; none for an ad hoc signature.
fn team(app: &Path) -> Option<String> {
    let output = Command::new("codesign")
        .args(["--display", "--verbose=2"])
        .arg(app)
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && *team != "not set")
        .map(str::to_owned)
}

/// Run `command`; when it fails, `refusal` is the status bar's sentence and
/// the tool's own words go to stderr.
fn run(command: &mut Command, refusal: &str) -> Result<(), String> {
    let output = command.output().map_err(|error| {
        eprintln!("eddie: update: {error}");
        refusal.to_owned()
    })?;
    if output.status.success() {
        return Ok(());
    }
    eprintln!(
        "eddie: update: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Err(refusal.to_owned())
}

/// Removes a folder when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn install(release: &Release) -> Result<PathBuf, String> {
    install_over(running_app()?, release)
}

/// Install `release` in place of `app`.
pub(super) fn install_over(app: PathBuf, release: &Release) -> Result<PathBuf, String> {
    let file = release
        .macos_arm64
        .as_ref()
        .ok_or("This release has no Mac app.")?;
    let Some(team_id) = team(&app) else {
        return Err(format!(
            "This Eddie was not signed by its publisher, so it does not update itself. Download Eddie {} from {RELEASES}.",
            release.version
        ));
    };
    let folder = app.parent().ok_or("The app has no folder.")?;
    // Beside the app, so the swap is a rename on one volume.
    let stage = Scratch(folder.join(format!(".Eddie-update-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&stage.0);
    std::fs::create_dir(&stage.0)
        .map_err(|error| format!("Eddie can't write to {}: {error}", folder.display()))?;
    let zip = stage.0.join("Eddie.zip");
    super::download(file, &zip)?;
    let unpacked = stage.0.join("unpacked");
    run(
        Command::new("ditto")
            .args(["-x", "-k"])
            .arg(&zip)
            .arg(&unpacked),
        "Couldn't unpack the update. Nothing was installed.",
    )?;
    let new = unpacked.join("Eddie.app");
    run(
        Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&new),
        "The update's signature is not valid. Nothing was installed.",
    )?;
    if team(&new).as_deref() != Some(team_id.as_str()) {
        return Err(
            "The update was not signed by Eddie's publisher. Nothing was installed.".to_owned(),
        );
    }
    run(
        Command::new("spctl")
            .args(["--assess", "--type", "execute"])
            .arg(&new),
        "macOS does not accept the update. Nothing was installed.",
    )?;
    let old = stage.0.join("previous.app");
    std::fs::rename(&app, &old)
        .map_err(|error| format!("Couldn't replace {}: {error}", app.display()))?;
    if let Err(error) = std::fs::rename(&new, &app) {
        let _ = std::fs::rename(&old, &app);
        return Err(format!("Couldn't replace {}: {error}", app.display()));
    }
    // The running program keeps its files open; the old app goes with the
    // stage folder.
    Ok(app)
}

/// Open `app` once process `pid` (this one) has left.
pub fn relaunch(app: &Path, project: Option<&Path>) -> std::io::Result<()> {
    let script = r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done
if [ -n "$3" ]; then exec /usr/bin/open -a "$2" "$3"; else exec /usr/bin/open "$2"; fi"#;
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("eddie-relaunch")
        .arg(std::process::id().to_string())
        .arg(app)
        .arg(project.map(Path::as_os_str).unwrap_or_default())
        .spawn()
        .map(drop)
}

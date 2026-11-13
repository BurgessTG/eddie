//! Looking for a newer Eddie: once at start and once a day, on the channel
//! Settings names. A release that is found shows as a button in the status
//! bar. On a Mac it installs over the app and offers a restart; elsewhere it
//! opens the release notes, since the package manager owns the files.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use eddie_core::update::{self, Channel, Release, Version};
use rune::{El, row, text};

use crate::app::Ui;
use crate::dialogs::Discard;
use crate::icons;
use crate::theme::{self, Variant, c};

const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, PartialEq)]
pub enum Phase {
    UpToDate,
    Available(Release),
    Installing(Release),
    /// Installed over the app at this path; a restart runs it.
    Installed(Release, PathBuf),
}

enum Answer {
    Checked(Channel, Result<Option<Release>, String>),
    Installed(Result<PathBuf, String>),
}

pub struct State {
    /// Where releases are looked for; `None` turns looking off (tests and
    /// renders to a file).
    base: Option<String>,
    /// The channel last looked at, and when to look again.
    checked: Option<Channel>,
    next: Option<Instant>,
    answers: Option<Receiver<Answer>>,
    pub phase: Phase,
    /// The app to start once this one has left.
    pub relaunch: Option<PathBuf>,
}

impl State {
    /// Looking for releases under `base`; `None` never looks.
    pub fn new(base: Option<String>) -> Self {
        Self {
            base,
            checked: None,
            next: None,
            answers: None,
            phase: Phase::UpToDate,
            relaunch: None,
        }
    }

    /// The window's: GitHub's releases, or those under `EDDIE_UPDATES_URL`.
    pub fn published() -> Self {
        Self::new(Some(
            std::env::var("EDDIE_UPDATES_URL").unwrap_or_else(|_| update::RELEASES.to_owned()),
        ))
    }

    /// Only a window that looks for updates installs one, so only it
    /// restarts into one.
    pub fn take_relaunch(&mut self) -> Option<PathBuf> {
        self.base.as_ref()?;
        self.relaunch.take()
    }

    pub fn busy(&self) -> bool {
        matches!(self.phase, Phase::Installing(_))
    }
}

/// The release's name: its version, and its build when it is a newer
/// build of the version running.
fn name(release: &Release) -> String {
    let running = update::this_build().version;
    match Version::parse(&release.version) {
        Some(version) if version == running => {
            format!("{} build {}", release.version, release.build)
        }
        _ => release.version.clone(),
    }
}

/// Once per poll: take answers in, and start a check when one is due or
/// the channel changed.
pub fn sync(ui: &mut Ui, waker: impl Fn() + Send + 'static) {
    let Some(base) = ui.updates.base.clone() else {
        return;
    };
    let state = &mut ui.updates;
    if let Some(answer) = state.answers.as_ref().and_then(|rx| rx.try_recv().ok()) {
        state.answers = None;
        match answer {
            Answer::Checked(checked, result) => {
                let installing = matches!(state.phase, Phase::Installing(_) | Phase::Installed(..));
                match result {
                    // A check for a channel since left is stale.
                    _ if Some(checked) != state.checked || installing => {}
                    Ok(Some(release)) => state.phase = Phase::Available(release),
                    Ok(None) => state.phase = Phase::UpToDate,
                    // Offline: try again at the next check, without a word.
                    Err(error) => eprintln!("eddie: {error}"),
                }
            }
            Answer::Installed(result) => {
                let release = match std::mem::replace(&mut state.phase, Phase::UpToDate) {
                    Phase::Installing(release) => release,
                    other => {
                        state.phase = other;
                        return;
                    }
                };
                match result {
                    Ok(app) => {
                        ui.editor.status =
                            format!("Eddie {} is installed. Restart to use it.", name(&release));
                        state.phase = Phase::Installed(release, app);
                    }
                    Err(error) => {
                        ui.editor.status = error;
                        state.phase = Phase::Available(release);
                    }
                }
            }
        }
    }
    let state = &mut ui.updates;
    let channel = ui.editor.settings.updates;
    let due = state.next.is_none_or(|next| Instant::now() >= next);
    let settled = !matches!(state.phase, Phase::Installing(_) | Phase::Installed(..));
    if state.answers.is_none() && settled && (due || state.checked != Some(channel)) {
        state.checked = Some(channel);
        state.next = Some(Instant::now() + EVERY);
        if channel == Channel::Off {
            state.phase = Phase::UpToDate;
            return;
        }
        let (tx, rx) = mpsc::channel();
        state.answers = Some(rx);
        std::thread::Builder::new()
            .name("update-check".to_owned())
            .spawn(move || {
                let found = update::check(&base, channel, &update::this_build());
                let _ = tx.send(Answer::Checked(channel, found));
                waker();
            })
            .ok();
    }
}

/// How long the window may sleep before the next check is due.
pub fn wake_in(ui: &Ui) -> Option<Duration> {
    ui.updates.base.as_ref()?;
    let next = ui.updates.next?;
    Some(next.saturating_duration_since(Instant::now()))
}

/// Download and install `release` over the running app, off the UI thread.
fn install(ui: &mut Ui, release: Release) {
    let (tx, rx) = mpsc::channel();
    let waker = ui.waker();
    let job = release.clone();
    ui.updates.answers = Some(rx);
    ui.updates.phase = Phase::Installing(release);
    ui.editor.status = format!("Downloading Eddie {}…", name(&job));
    std::thread::Builder::new()
        .name("update-install".to_owned())
        .spawn(move || {
            let _ = tx.send(Answer::Installed(update::install(&job)));
            waker();
        })
        .ok();
}

/// The status bar's update control, when there is something to show.
pub fn control(ui: &Ui) -> Option<El> {
    let c = c();
    Some(match &ui.updates.phase {
        Phase::UpToDate => return None,
        Phase::Available(release) if update::SELF_INSTALLS => {
            let release = release.clone();
            theme::button(
                "update",
                &format!("Update to {}", name(&release)),
                Variant::Secondary,
                &format!(
                    "Download Eddie {}, check it is signed by its publisher and install it. Eddie keeps running until you restart it.\nWhat changed: {}",
                    name(&release),
                    release.notes
                ),
                true,
            )
            .on_press(move |ui: &mut Ui| install(ui, release.clone()))
        }
        Phase::Available(release) => {
            let notes = release.notes.clone();
            theme::button(
                "update",
                &format!("Eddie {} is out", name(release)),
                Variant::Secondary,
                &format!(
                    "Update with the package manager that installed Eddie, such as yay -Syu. Click to read what changed:\n{notes}"
                ),
                true,
            )
            .on_press(move |ui: &mut Ui| {
                if let Err(error) = crate::shell::open_externally(&notes) {
                    ui.editor.status = format!("Could not open the release notes: {error}");
                }
            })
        }
        Phase::Installing(release) => row()
            .key("update")
            .gap(6.0)
            .center_y()
            .help(format!("Downloading and checking Eddie {}", name(release)))
            .child(
                rune::el()
                    .size(14.0, 14.0)
                    .shape(icons::spinner(14.0, ui.turn(), c.muted_fg)),
            )
            .child(
                text("Updating")
                    .font(theme::sized(12.0))
                    .color(c.muted_fg)
                    .single_line(),
            ),
        Phase::Installed(release, app) => {
            let app = app.clone();
            theme::button(
                "update",
                "Restart",
                Variant::Primary,
                &format!(
                    "Restart Eddie to use {}. Asks to save changes first.",
                    name(release)
                ),
                true,
            )
            .on_press(move |ui: &mut Ui| ui.request_discard(Discard::Restart(app.clone())))
        }
    })
}

#[cfg(test)]
mod tests;

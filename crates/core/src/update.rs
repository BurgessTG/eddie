//! Updates: whether a published Eddie is newer than this one, and on macOS
//! installing it over the running app.
//!
//! `.github/workflows/release.yml` publishes a tagged release for everyone;
//! `preview.yml` publishes every `master` commit CI passed as the rolling
//! `preview` release, for those who choose Previews. Each carries an
//! `update.json` beside its files (`packaging/release/manifest.sh`).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Where releases are published.
pub const RELEASES: &str = "https://github.com/BurgessTG/eddie/releases";

/// Which updates the user wants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Channel {
    /// Tagged releases.
    #[default]
    Releases,
    /// Tagged releases and every `master` commit CI passed.
    Previews,
    /// No checking.
    Off,
}

impl Channel {
    pub const ALL: [Self; 3] = [Self::Releases, Self::Previews, Self::Off];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Releases => "releases",
            Self::Previews => "previews",
            Self::Off => "off",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|channel| channel.as_str() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Releases => "Releases",
            Self::Previews => "Previews",
            Self::Off => "Off",
        }
    }
}

/// A release version, `major.minor.patch`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// `1.2.3` or `v1.2.3`; anything else is not a version.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
        let mut next = || parts.next()?.parse::<u64>().ok();
        let version = Self(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// What this program is: its version and, when CI built it, the number of
/// commits on `master` it was built from (`EDDIE_BUILD`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Build {
    pub version: Version,
    pub build: Option<u64>,
}

pub fn this_build() -> Build {
    Build {
        version: Version::parse(env!("CARGO_PKG_VERSION")).expect("the workspace version"),
        build: option_env!("EDDIE_BUILD").and_then(|build| build.parse().ok()),
    }
}

/// A published Eddie, as its `update.json` describes it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Release {
    pub version: String,
    /// Commits on `master` it was built from.
    pub build: u64,
    pub commit: String,
    /// The page that says what changed.
    pub notes: String,
    /// The signed, notarized app for Apple Silicon.
    #[serde(rename = "macos-arm64")]
    pub macos_arm64: Option<Download>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Download {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

impl Release {
    fn key(&self) -> Option<(Version, u64)> {
        Some((Version::parse(&self.version)?, self.build))
    }

    /// Whether this is newer than `running`: a later version, or a later
    /// commit of the same version when `running` knows its commit count. A
    /// build made by hand knows none, so only a later version is newer.
    pub fn is_newer_than(&self, running: &Build) -> bool {
        let Some((version, build)) = self.key() else {
            return false;
        };
        match running.build {
            Some(running_build) => (version, build) > (running.version, running_build),
            None => version > running.version,
        }
    }
}

/// An agent that refuses plain HTTP whenever `url` is HTTPS, redirects
/// included.
fn agent(url: &str) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(600)))
            .timeout_connect(Some(Duration::from_secs(15)))
            .https_only(url.starts_with("https:"))
            .http_status_as_error(false)
            .user_agent(concat!("eddie/", env!("CARGO_PKG_VERSION")))
            .build(),
    )
}

/// The newest release under `base` (`RELEASES`, or a test server) that
/// `channel` offers and that is newer than `running`. Blocks on the
/// network, so call it off the UI thread.
pub fn check(base: &str, channel: Channel, running: &Build) -> Result<Option<Release>, String> {
    let base = base.trim_end_matches('/');
    let manifests = match channel {
        Channel::Off => return Ok(None),
        Channel::Releases => vec![format!("{base}/latest/download/update.json")],
        Channel::Previews => vec![
            format!("{base}/latest/download/update.json"),
            format!("{base}/download/preview/update.json"),
        ],
    };
    let agent = agent(base);
    let mut newest: Option<Release> = None;
    let mut failures = Vec::new();
    for url in &manifests {
        match fetch_manifest(&agent, url) {
            Ok(Some(release)) if release.is_newer_than(running) => {
                if newest
                    .as_ref()
                    .is_none_or(|best| release.key() > best.key())
                {
                    newest = Some(release);
                }
            }
            Ok(_) => {}
            Err(error) => failures.push(error),
        }
    }
    match failures.len() {
        n if n == manifests.len() => Err(failures.remove(0)),
        _ => Ok(newest),
    }
}

/// The manifest at `url`; none when nothing is published there yet.
fn fetch_manifest(agent: &ureq::Agent, url: &str) -> Result<Option<Release>, String> {
    let mut response = agent
        .get(url)
        .call()
        .map_err(|error| format!("Couldn't check for updates: {error}"))?;
    match response.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        status => return Err(format!("Couldn't check for updates: HTTP {status}")),
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .map_err(|error| format!("Couldn't check for updates: {error}"))?;
    serde_json::from_str(&body)
        .map(Some)
        .map_err(|error| format!("The update description is not readable: {error}"))
}

/// Download `file` to `to`, keeping it only when it is exactly the size and
/// SHA-256 its manifest gives.
pub fn download(file: &Download, to: &Path) -> Result<(), String> {
    let failed = |error: &dyn std::fmt::Display| format!("The download failed: {error}");
    let response = agent(&file.url)
        .get(&file.url)
        .call()
        .map_err(|e| failed(&e))?;
    if response.status().as_u16() != 200 {
        return Err(failed(&format!("HTTP {}", response.status().as_u16())));
    }
    let mut reader = response
        .into_body()
        .into_with_config()
        .limit(file.size.saturating_add(1))
        .reader();
    let mut out = std::fs::File::create(to).map_err(|e| failed(&e))?;
    let mut hasher = Sha256::new();
    let mut length = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    let result = loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break Ok(()),
            Ok(read) => read,
            Err(error) => break Err(failed(&error)),
        };
        length += read as u64;
        hasher.update(&buffer[..read]);
        if let Err(error) = out.write_all(&buffer[..read]) {
            break Err(failed(&error));
        }
    };
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let result = result.and_then(|()| {
        if length != file.size || !digest.eq_ignore_ascii_case(file.sha256.trim()) {
            Err(
                "The download is not the file that was published. Nothing was installed."
                    .to_owned(),
            )
        } else {
            Ok(())
        }
    });
    if result.is_err() {
        let _ = std::fs::remove_file(to);
    }
    result
}

/// Whether this program can replace itself: a signed `Eddie.app` on macOS.
/// Elsewhere the package manager that installed it updates it.
pub const SELF_INSTALLS: bool = cfg!(target_os = "macos");

/// Download `release` and put it where the running app is. Returns the
/// app's path, to start it again from.
pub fn install(release: &Release) -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        mac::install(release)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = release;
        Err("Update Eddie with the package manager that installed it.".to_owned())
    }
}

/// Start `app` once this process has left, opening `project` in it.
pub fn relaunch(app: &Path, project: Option<&Path>) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::relaunch(app, project)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, project);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod mac;

#[cfg(test)]
mod tests;

# Changelog

## 0.2.0 (2026-11-13)

Eddie no longer has an AI inside. It is an editor that any agent drives
from outside, through a command line and a sandboxed script API, while you
keep editing by hand in the window.

### Added

- The `eddie` command line: `new`, `import`, `state`, `inspect`, `run`,
  `transcribe`, `curate`, `export`, `help` and `guide`. Each command prints
  one JSON object. `run` applies a script as one edit that rolls back
  completely when any part fails.
- `eddie sheet`, `strip` and `frames`: pictures of the edit and its sources,
  for agents that cannot watch video.
- Live editing: while a project is open in the window or in `eddie serve`,
  commands go to that editor over a private Unix socket. `undo`, `redo`,
  `save`, `stop` and `events` talk to it.
- `docs/agents.md`, the guide for agents, also printed by `eddie guide`.
- Local transcription with whisper.cpp, the default, needing no key. Meta
  Voice Transcribe and OpenAI-compatible endpoints are optional providers.
- GPU transcription with `--features vulkan` or `--features cuda`. A model
  goes on the GPU only when it fits the free memory.
- Settings → Speech model lists the installed local models.
- One writer per project: commands started together take turns instead of
  losing each other's edits.
- Music ducks under speech on a raised-cosine curve, the same in preview
  and export.
- `eddie --version`.
- macOS on Apple Silicon: Eddie builds and runs there. Shortcuts use Cmd,
  the menu bar is the window's titlebar, hardware decode uses VideoToolbox
  and local transcription uses Metal.
- `packaging/macos/bundle.sh` builds `Eddie.app`, which opens from Finder
  with its own icon. Double-clicking a `.eddie` project opens it in Eddie,
  and media opened with Eddie is imported. FFmpeg is inside the app, built
  from source for it, so it needs nothing else installed and runs on
  macOS 11 or later. With `EDDIE_SIGN_IDENTITY` it is signed with a
  Developer ID, and `packaging/macos/notarize.sh` has Apple notarize it.
- `EDDIE_FFMPEG` names the `ffmpeg` command to use.
- Updates: the window looks for a newer Eddie at start and once a day, and
  the status bar says when one is out. On a Mac it installs the signed,
  notarized app in place and restarts into it; on Linux it points to the
  package manager. Settings → Updates chooses Releases, Previews (each
  `master` commit that passed CI) or Off.
- Download links that never change: each release and preview also carries
  `Eddie-macos-arm64.zip`, so `releases/latest/download/Eddie-macos-arm64.zip`
  is always the newest release.
- Eddie's website, https://burgesstg.github.io/eddie/ (`site/`, published
  to GitHub Pages), offers the newest Mac build.
- `eddie editors` lists every running editor and the project it has open.
  A window whose project was never saved works on an untitled project in
  Eddie's settings folder, so agents reach it too; it used to have no
  socket until the user saved.
- Pushing a version tag publishes the release: the Mac app on GitHub and
  both AUR packages (`.github/workflows/release.yml`).

### Changed

- Titles, video and sound have lanes of their own, shown in that order
  from the top, and a clip moves only between lanes of its kind. Scripts
  count lanes within a kind (`track`, `setClipLane`). Projects are saved
  as version 2; version 1 projects open with their lanes sorted by kind,
  keeping which video shows where two overlap.
- The window is drawn with [Rune](https://github.com/anomalyco/rune). egui
  is gone.
- The built-in chat and model reviewer are removed. Curation is local:
  scene detection, transcription and measured ranking.
- The editor is its own crate, `eddie-core`, with no UI toolkit.
- Settings moved from `openreel/` to `eddie/` under the config directory.
  The old folder is renamed on the first start.
- The packages build with Vulkan, so local transcription uses any GPU with
  a Vulkan driver and the CPU otherwise.
- Zoom fits timelines of any length; the ruler reads hours.

### Fixed

- Script arguments out of range are refused instead of clamped, and an
  async script fails whole instead of half applying.
- A script stuck inside one built-in call no longer hangs the editor.
- `sheet` and `strip` cannot overwrite the project or its media.
- A socket client can no longer exhaust the editor's memory.
- Titles are drawn as typed, including `:`, `%`, `\` and quotes.
- A full GPU no longer crashes transcription.
- A dry run no longer interrupts playback.
- The File menu is wide enough for its longest shortcut.
- Closing the window with unsaved edits keeps it open under "Save
  changes?" instead of hiding it and showing it again. On macOS, Cmd+Q and
  the Dock's Quit ask too instead of dropping the edits.
- With the magnet on, a clip let go next to its neighbour now meets it
  exactly instead of trading places with it. A clip dragged a little way
  into another stops against it; it takes the other's place only when it
  covers at least half of the shorter clip.

## 0.1.0 (2026-11-06)

The first release: the egui editor with the built-in curator.

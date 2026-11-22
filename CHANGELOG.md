# Changelog

## 0.3.0 (not released yet)

### Added

- Any video on any canvas. Settings → Format offers Original (the first
  video's shape, the default), 16:9, 9:16, 1:1, 4:5, 4:3 and 21:9; footage
  of another shape fits inside with bars instead of being refused.
- A Transform card in Details for video clips: scale, position, rotation,
  crop, horizontal and vertical flip, Fit, Fill and Reset. Drag the picture
  on the monitor to move it; it snaps to the centre.
- A blurred background: the picture, enlarged and blurred, fills the canvas
  around it instead of black.
- Scripts: `setFormat`, `setTransform` and `setBackground`; `state()` adds
  `canvas`, `formats` and each clip's `transform` and `background`.
  `inspect()` reports a picture moved off the canvas.
- A Colour card in Details for video clips: brightness, contrast,
  saturation, exposure, temperature and tint, plus nine filters (Vivid,
  Dramatic, Warm, Cool, Fade, Vintage, Sepia, Mono, Noir) with a strength,
  picked from tiles that show each filter on the clip's own frame. The
  monitor shows the grade as export renders it. Scripts use
  `setColor`, and `state()` gives each clip's `color`.
- Clip speed from 0.25× to 4× in Details (Timing → Speed), with the sound
  kept at its pitch in preview and export. A clip keeps its source range,
  so its length follows the speed; slowing stops where the next clip on the
  lane starts. Scripts use `setSpeed`, and `state()` gives each clip's
  `speed`.
- An Export dialog: resolution (Auto, 480p, 720p, 1080p, 2K, 4K), frame
  rate (24, 25, 30, 50, 60) and quality (Lower, Recommended, Higher). It
  remembers the last choice. Titles keep their size and place on the
  frame at any resolution. `eddie export` takes `--resolution`, `--fps`
  and `--quality`.
- Photos (PNG, JPEG, WebP and HEIC) as clips. One added whole lasts 3
  seconds; trim either edge to any length. A phone photo shows upright
  from its orientation, an iPhone's HEIC included, and photos take the
  same transform, background and colour as video. `state()` marks photo
  assets and clips `photo`. Import's file list shows `IMG_0001.HEIC` and
  other upper-case names on Linux too.
- Edit tools: Copy, Cut and Paste (Ctrl+C, Ctrl+X, Ctrl+V, Cmd on a Mac;
  a paste lands at the playhead), Duplicate (Ctrl+D), Ripple delete
  (Shift+Delete), which closes the gap on every lane, and Freeze frame
  (F), which holds the picture at the playhead for 3 seconds. They are in
  the Edit menu, a new right-click menu on clips and the timeline toolbar.
  Scripts use `rippleDelete`, `duplicateClip` and `freezeFrame`.
- A Text card in Details for titles: size, text colour, a box behind the
  text with its colour and opacity, an outline with its colour and
  thickness, and position, each over the title's style, from colour
  swatches or a typed `#RRGGBB`. Drag a title on the monitor to move it.
  Scripts use `setText`, `state()` gives each title's `text`, and
  `inspect()` reports a title moved off the canvas.
- Transitions: a Transition card in Details for a video clip that follows
  another on its lane, with Mix, Dip to black, Dip to white, and Wipe and
  Slide left, right, up and down, from 0.1 to 5 seconds (0.5 by default).
  A transition is centred on the cut and moves no clip; the monitor plays
  it as export renders it, and the timeline marks it on the cut. Scripts
  use `setTransition`, `state()` gives each clip's `transition`, and
  `inspect()` reports one that plays shorter than set or runs past a
  file's pictures. The sound crossfades under it at equal power, in
  preview and export.
- Picture-in-picture and overlays. Video lanes stack: the top lane draws
  over the ones under it, and a picture made smaller, a clip below full
  opacity or a PNG with transparency shows the lanes beneath, in preview,
  export and transitions. Right-click a file in the bin and pick Add as
  overlay to put it at the playhead on a lane above the video there.
  Drag on the monitor to move the top picture under the pointer.
  `eddie sheet` draws each picture moved, scaled and turned as export
  does, over the lanes it does not hide, and each cell lists them as
  `under`.
- Keyframes for position, scale, rotation, opacity and volume. In
  Details, the diamond beside each adds a key at the playhead or takes one
  away; once a value has keys, changing it there, or dragging the picture
  on the monitor, sets a key at the playhead. Values run in a straight
  line from key to key, the same in preview and export, and keys stay on
  their moment of the footage through trims, splits and speed changes.
  Previous key and Next key move the playhead between them, and the
  timeline marks them on the clip. Scripts use `setKeyframe`,
  `removeKeyframe` and `clearKeyframes`, and `state()` gives each clip's
  `keyframes`.
- Blend modes for a picture over other lanes: Darken, Multiply, Colour
  burn, Linear burn, Lighten, Screen, Colour dodge, Add, Overlay, Soft
  light, Hard light, Difference and Exclusion, picked under Opacity in
  Details. The monitor, export and `eddie sheet` mix the same colours.
  Scripts use `setBlend`, `state()` gives each clip's `blend`, and
  `inspect()` reports a blended picture with nothing under it.
- Voiceover recording: the microphone on the timeline toolbar (or R)
  opens a panel with the input's level. Record counts down 3 seconds,
  then records from the playhead while the timeline plays, silent unless
  Hear the timeline is on. R, Space or Esc stops. The take is saved as a
  WAV in a "<project> voiceovers" folder beside the project and lands on
  a free audio lane in step with the picture. The Mac app asks for
  microphone access the first time.

### Changed

- A clip below full opacity, or a picture smaller than the canvas, shows
  the video lanes under it instead of black. A clip on a lane under a
  full-canvas, opaque picture is still covered.
- Projects saved as Landscape or Portrait now render at exactly 16:9 or
  9:16. They took the shape of their largest video of that orientation
  before.

### Fixed

- Splitting a moved, cropped or graded clip, or removing silence from one,
  kept that look only on the first piece; every piece keeps it now.
- The Keyboard shortcuts window scrolls when the Eddie window is too short
  to show the whole list.
- A title below full opacity now fades its box and outline in export and
  contact sheets too, as the monitor showed it; they stayed solid before.
- Export now fills the corners around a turned picture with
  transparency; it left them unpainted, so they could show leftover
  pixels.

## 0.2.1 (2026-11-20)

### Fixed

- Eddie builds again on Arch Linux, which moved to FFmpeg 9.1: the AUR
  package stopped in `hwcontext_cuda.h` while generating FFmpeg's
  bindings. Nothing else changed.

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

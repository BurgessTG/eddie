# Eddie

*The editor that watches the footage for you.*

[![CI](https://github.com/BurgessTG/eddie/actions/workflows/ci.yml/badge.svg)](https://github.com/BurgessTG/eddie/actions/workflows/ci.yml)

A fast desktop video editor with an AI clip curator. Rust end to end:
`egui` + `wgpu` for the UI, in-process FFmpeg (`ffmpeg-next`) for playback,
and the FFmpeg CLI tools for probing and export.

Import footage, let the curator break it into every shot and the moments
worth using, then cut those ranges together on a free-form timeline and
export an H.264/AAC MP4. You can also edit by chat or from a sandboxed
JavaScript console.

## Features

**Editor**

- Media bin: a grid of thumbnails and waveforms. Drag and drop, File →
  Import media, or pass file paths on the command line. Imports show a
  placeholder immediately.
- Free-form timeline: lanes of any kind, gaps allowed; dragging above or
  below the lanes creates new ones. Move, trim, split, swap adjacent clips
  and move clips across lanes. The magnet snaps edges while dragging.
- Landscape or portrait projects (Settings → Format). Video of the other
  orientation can't be placed.
- Linked audio: detach a video's sound into a clip that moves, trims and
  splits with it, or unlink it for J/L cuts. Audio clips have fades and
  can duck under speech.
- Program monitor with frame stepping. Audio drives the playhead, and the
  next cut's first picture is prerolled so short clips play through.
- Titles and generated captions in four styles, identical in preview and
  export. Remove silence tightens pauses out of speech.
- Undo/redo covers every timeline edit, whether made by hand, in Details,
  in the console or by chat.
- Projects (`.eddie`) store the timeline and media paths. Footage moved
  with its project is found again; anything else is relinked with File →
  Locate missing media.
- Export to H.264/AAC MP4 in limited-range BT.709, normalized to -16 LUFS.

**Curator**

Select a video and press **Curate**. Curation never runs on import, and
results are saved per file.

- **Local analysis, always:** every frame is checked for scene cuts, with
  flashes, objects passing the lens and lighting changes rejected. You get
  a complete list of shots plus ranked suggestions.
- **With a Meta Model API key:** the selected model reviews frames and the
  transcript in 24-second windows, picks useful ranges inside each shot,
  and can ask SAM 3.1 to track a subject. Edges are then checked against
  the source pictures frame by frame, and speech keeps complete thoughts.
- Every range gets a category (`reaction`, `talking`, `action`, `broll`,
  `establishing`), tags, quality scores and a review status. Anything the
  model could not validate stays available as **Needs review**.
- **Gallery:** search names, tags, review notes and transcripts; filter by
  type, quality and duration. **Add shown** appends exactly what is
  displayed. **All scenes** shows every shot, rejected ones included.

**Chat and scripting**

- Chat (right dock) turns typed requests into edits. It shows what it's
  doing while it works and can be cancelled. Each AI edit undoes in one
  click, and a failed batch rolls back completely.
- The JS console (View → JS console) runs QuickJS with memory and time
  limits and no filesystem or OS access. Run `help()` for the API.

## Requirements

- Rust, pinned in `mise.toml` (use [mise](https://mise.jdx.dev)).
- Build: FFmpeg 9 development libraries, ALSA development headers,
  `pkg-config`, `clang` (for bindgen), `cmake` and a C++ compiler (for
  whisper.cpp).
- Runtime: `ffmpeg` and `ffprobe` 9 on `PATH`; ALSA (or PipeWire/Pulse
  through ALSA) for sound.
- Optional: a Meta Model API key for model review, transcription and SAM.
  Without one, curation runs locally and chat needs a keyless localhost
  endpoint.
- Optional: whisper.cpp's multilingual base model (`ggml-base.bin`) in
  `whisper/` beside the settings file, or set `EDDIE_WHISPER_MODEL`. It
  adds word timing so speech can be cut between sentences.

Settings live in `~/.config/openreel/config.json` on Linux,
`~/Library/Application Support/OpenReel/` on macOS and
`%APPDATA%\OpenReel\` on Windows.

## Run

```sh
git clone https://github.com/BurgessTG/eddie.git
cd eddie
mise exec -- cargo run --release                  # judge performance in release
mise exec -- cargo run --release -- a.mp4 b.mov   # import files on launch
SDL_AUDIODRIVER=dummy mise exec -- cargo test     # full suite, headless
mise exec -- cargo run -p openreel-media --example generate_samples -- samples
```

The last command generates test media into `samples/`. CI runs formatting
and the engine tests only; the media and app suites need FFmpeg 9 and run
locally.

Open Settings with the gear in the right dock to set the API key, base URL
(`https://api.meta.ai/v1` by default), model (`muse-wonder-1.0`, the
default, or `muse-spark-1.3`) and format. **Check connection** validates
the key and model without running inference.

## Shortcuts

Press F1 or Ctrl+/ in the app for the full list. Cmd replaces Ctrl on
macOS.

| Key | Action |
| --- | --- |
| Space | Play / pause |
| ← / → | Step one frame (Shift: one second) |
| ↑ / ↓ | Previous / next cut |
| Home / End | Go to start / end |
| S or B | Split at the playhead |
| T | Add title |
| M | Toggle magnet |
| Delete | Delete the selected clip |
| Ctrl+Z / Ctrl+Shift+Z | Undo / redo |
| Ctrl+= / Ctrl+- / Shift+Z | Zoom in / out / fit timeline |
| Ctrl+N / O / S | New / open / save project |
| Ctrl+I / Ctrl+E | Import media / export MP4 |
| Ctrl+K | Focus chat |

## Layout

| Crate | Role |
| --- | --- |
| `crates/engine` (`openreel-engine`) | UI-independent timeline model: lanes, linked clips, edits, titles, timecode. |
| `crates/media` (`openreel-media`) | FFmpeg: probe, native decode and preview, audio mixer, scene detection, waveforms, thumbnails, export. |
| `crates/app` (`openreel-app`, binary `eddie`) | egui app: media bin, timeline, monitor, gallery, curator, chat, QuickJS console. |

## Limitations

- Meta's ASR returns speaker turns only. Without the local whisper model,
  speech edges are turn-level.
- SAM mask payloads are not decoded; subject evidence uses boxes only.
- Burned-in text (title cards, news bars) is tagged by the model, not
  detected locally, so local-only curation can't exclude it.
- Model curation is slow: several passes per review window. Model scores
  and sparse frame sampling are suggestions, not proof that every good
  moment was found.
- Undo history is not saved with a project.
- Sound-card output latency is not compensated.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.

# Eddie


A fast, cross-platform desktop video editor. Rust end to end: `egui` + `wgpu`
for the UI, FFmpeg tools for media.

> **Status: slice 2 — real media.** Import real video/audio (file dialog or
> drag & drop), chop it on the timeline (move, trim edges, split at the
> playhead), watch and hear it in the program monitor, burn in titles, and
> export an H.264/AAC MP4.

Requires `ffmpeg`, `ffprobe`, and `ffplay` on `PATH` at runtime.

## Design system

shadcn/ui tokens translated to egui (shadcn itself is React-only):
zinc dark palette, 6/8/10px radius scale, borderless-at-rest buttons with
hover/pressed fills, Inter when installed, and `primary` / `outline` /
`ghost` / `destructive` variants plus `Badge`. See `crates/app/src/theme.rs`.

## Layout

| Crate | Role |
| --- | --- |
| `crates/engine` (`openreel-engine`) | UI-independent timeline model: clips with media sources, frame snapping, move/trim/split, timecode. Unit-tested. |
| `crates/media` (`openreel-media`) | FFmpeg-backed probe, frame decode/streaming, waveforms, thumbnails, sample generation, timeline export, audio playback. End-to-end tested. |
| `crates/app` (`openreel`) | egui desktop app: media bin, custom timeline with filmstrips/waveforms, program monitor, inspector, export UI. |

Media architecture note: video decode for scrubbing and playback runs
in-process through libavcodec (`ffmpeg-next`), with a persistent scrub
decoder, frame threading, two-tier scrub resolution, and pooled frame
buffers. Probe/export/audio still use the FFmpeg CLI tools, and frame
grabs fall back to the CLI if the in-process path rejects a file. Next:
hardware decode with zero-copy GPU textures.

## Run

Rust is pinned per-project with [mise](https://mise.jdx.dev) (`mise.toml`).

```sh
cd ~/Projects/openreel
mise exec -- cargo run          # debug build
mise exec -- cargo test         # engine + media end-to-end tests
mise exec -- cargo run -p openreel-media --example generate_samples -- samples
```

If your shell has `mise activate` loaded, plain `cargo run` works in this
directory.

First run: import media (File menu, drag & drop, or pass file paths as
arguments), then double-click a bin tile or drag it onto the timeline.

## Roadmap

1. **UI shell + timeline model** (done)
2. **Real media: import, chop, preview + audio, export** (done)
3. Hardware decode + zero-copy GPU preview (`ffmpeg-next` or wgpu textures)
4. Project save/load, undo/redo
5. Proxies for 4K–8K/RAW, transitions and effects, docking panels

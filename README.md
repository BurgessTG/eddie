# Eddie

A fast desktop video editor with an AI clip curator. Rust end to end:
`egui` + `wgpu` for the UI, in-process FFmpeg (`ffmpeg-next`) for video and
audio playback, and the FFmpeg CLI tools for probing and export.

Import footage, let the curator break it into every shot and the moments
worth using, then cut those ranges together on a free-form timeline and
export an H.264/AAC MP4. You can also edit by chat or from a sandboxed
JavaScript console.

## Features

**Editor**

- Media bin: an image grid of thumbnails and waveforms. Imports queue and
  show a placeholder immediately. Drag & drop, File menu, or file paths
  on the command line.
- Free-form timeline: lanes of any kind, gaps allowed, dragging above or
  below the lanes creates new ones. Move, trim, swap adjacent clips
  (including unequal lengths), and move clips across lanes. Magnet snaps
  edges while dragging and never ripples clips closed.
- Linked audio: detach a video's sound into a linked audio clip that moves,
  trims and splits with it, or unlink it for J/L cuts.
- Program monitor with frame stepping. The preview prefetches the next
  cut's first picture so short clips play through with fewer stalls.
  Audio is mixed in process and drives the playhead, so a stalled picture
  holds sound with it.
- Titles and generated captions in four styles, the same in preview and
  export. Remove silence tightens pauses out of speech.
- Export to H.264/AAC MP4, converted to limited-range BT.709 colour.

**Curator**

Select a video and press **Curate**. Curation never runs on import.

- **Local analysis, always:** every frame is checked for visual scene
  boundaries. Flashes, foreground objects passing the lens, and lighting
  changes are rejected as cuts. The result is a complete, source-ordered
  list of every shot, plus ranked suggestions scored from motion, audio,
  and stability.
- **With a Meta Model API key:** the selected model reviews timestamped
  frame sequences and the transcript in 24-second windows. It picks useful
  ranges inside each shot, and SAM 3.1 can track a specific subject on
  request. Edges are then checked against the actual source pictures at
  frame level. Speech ranges keep complete thoughts instead of cutting
  mid-sentence.
- Every range gets a category (`reaction`, `talking`, `action`, `broll`,
  `establishing`), production tags (`detail`, `wide-shot`, `reveal`,
  `camera-move`, ...), quality subscores, and a review status. Anything the
  model could not validate stays available as **Needs review**; nothing is
  silently dropped.
- **Gallery:** search across names, tags, review notes, and transcripts;
  filter by type, quality, and duration; sort by score, source order, or
  length. **Add shown** appends exactly what is displayed. **All scenes**
  shows every shot, rejected material included.

**Chat and scripting**

- Chat (right dock) turns plain-language requests into edits. One-click
  intents such as *Reactions first* or *Tighten the intro* run locally or
  through the model. Every AI edit can be undone in one click, and a
  failed batch rolls back completely.
- The JS console (View → JS console) runs QuickJS with memory and time
  limits and no filesystem or OS access. Scripts enqueue editor commands;
  they never mutate state directly. Run `help()` for the API:
  `clips()`, `sweepCandidates()`, `sceneClips()`, `addAssetRange(...)`,
  `split()`, `tightenSilence(id)`, `exportTo(path)`, and more.

## Requirements

- Rust, pinned in `mise.toml` (use [mise](https://mise.jdx.dev)).
- Build: FFmpeg 9 development libraries, ALSA development headers,
  `pkg-config`, `clang` (for bindgen), `cmake` and a C++ compiler (for
  whisper.cpp).
- Runtime: `ffmpeg` and `ffprobe` on `PATH`; ALSA (or PipeWire/Pulse through
  ALSA) for sound.
- Optional: a Meta Model API key for model review, transcription, and SAM.
- Optional: whisper.cpp's multilingual base model (`ggml-base.bin`) at
  `~/.config/openreel/whisper/ggml-base.bin` or `EDDIE_WHISPER_MODEL`. It
  adds word timing to Meta's turn-level transcripts so speech can be cut
  between sentences.
  Without one, curation runs locally and chat needs a keyless
  localhost endpoint.

## Run

```sh
git clone https://github.com/BurgessTG/eddie.git
cd eddie
mise exec -- cargo run --release            # judge performance in release
mise exec -- cargo run --release -- a.mp4 b.mov   # import files on launch
mise exec -- cargo test                     # engine, media end-to-end, timeline UI
mise exec -- cargo run -p openreel-media --example generate_samples -- samples
```

Settings (Chat → Settings) take the API key, base URL
(`https://api.meta.ai/v1` by default), and model (`muse-spark-1.3` or
`muse-wonder-1.0`). The key is stored only in the local config file.
**Check connection** validates the key and model without running inference.

## Shortcuts

| Key | Action |
| --- | --- |
| Space | Play / pause |
| ← / → | Step one frame |
| Home / End | Go to start / end |
| S or B | Split at the playhead |
| M | Toggle magnet |
| Delete | Delete the selected clip |
| Cmd/Ctrl+K | Focus chat |

## Layout

| Crate | Role |
| --- | --- |
| `crates/engine` (`openreel-engine`) | UI-independent timeline model: lanes, linked clips, move/trim/split/swap, titles, timecode. |
| `crates/media` (`openreel-media`) | FFmpeg: probe, native decode and preview worker, evidence sampling, scene detection, waveforms, thumbnails, export. |
| `crates/app` (`openreel-app`, binary `eddie`) | egui app: media bin, timeline, program monitor, gallery, curator pipeline, chat, QuickJS console. |

The UI follows shadcn/ui conventions translated to egui (zinc palette,
Inter, borderless buttons, segmented tabs, cards); tokens and variants live
in `crates/app/src/theme.rs`. `AGENTS.md` records design decisions and the
detailed curator contract.

## Limitations

- Audio is the playback clock, but sync is not loopback-measured, and the
  device delay is estimated where the sound server does not report it.
- Meta's ASR returns speaker turns only. Word timing comes from a local
  whisper model when installed; without it, speech edges are turn-level,
  and noisy audio can still fail to align.
- SAM mask payloads are not decoded; subject evidence uses boxes only.
- Model scores and sparse frame sampling are suggestions, not proof that
  every good moment was found.
- No project save/load or general undo yet (AI edits have one-step undo).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.

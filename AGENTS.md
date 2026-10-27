# OpenReel build guidance

Read this before changing code. It records decisions the user has made.
Update it when a decision changes.

## Stack

- Rust only. `crates/engine` owns the timeline model. `crates/media` owns
  FFmpeg. `crates/app` owns egui UI.
- FFmpeg 9 is the target. In-process decode uses `ffmpeg-next` 9.
- Builds need FFmpeg dev libraries, `pkg-config`, and `clang` for bindgen.
- Runtime needs `ffmpeg`, `ffprobe`, and `ffplay` on `PATH` until probe,
  export, and audio also move in-process.

## Commands

```sh
cd ~/Projects/openreel
mise exec -- cargo run            # debug
mise exec -- cargo run --release  # use this to judge performance
mise exec -- cargo test           # engine, media end-to-end, timeline UI
mise exec -- cargo fmt
```

Rust is pinned in `mise.toml`. Do not switch toolchains casually.

## Storage

The dev machine has a small disk. Rust target directories are the main cost.

- `target/` has reached 5.8 GB here. Watch it with `du -sh target`.
- The dev profile sets `debug = 0` and `incremental = false` on purpose.
  Do not re-enable them without asking.
- Build one profile at a time. When a release build is stale, run
  `mise exec -- cargo clean --release`. It freed 1.2 GB here.
- Never create scratch crates with their own `target/` for probes. Reuse
  the workspace, or delete the scratch directory when done.
- Do not run `cargo clean` mid-iteration. It trades disk for a multi-minute
  rebuild on this CPU. Clean at slice boundaries.
- Sample media is generated, not committed by hand. Keep `samples/` small.

## pstack

The poteto pstack skills are installed in Muse user scope
(`~/.config/muse/skills`, 34 skills, validated 2026-10-25).

- Start new sessions with `/poteto-mode` for non-trivial work.
- Use `/tdd` when a bug has a cheap local test. Timeline interactions have
  synthetic-pointer tests in `timeline_ui.rs`. Add cases there.
- Use `/interrogate` before shipping contested designs. In one-model
  sessions, apply its rubric directly and say so.
- `setup-pstack` targets Cursor model rules. It does not map onto Muse
  Code. Skip it here.
- Skills installed mid-session do not hot-load. Restart the session to get
  slash invocation.

Known audit findings not yet fixed:

- Audio still shells out to `ffplay`. It needs the in-process treatment
  video got, plus a real A/V sync clock.

The media bin and import pipeline live in `crates/app/src/app/media.rs`.
Chat editing lives in `crates/app/src/app/chat.rs`. Its HTTP call runs on
a worker thread in app code. Generated JavaScript still goes through
`scripting.rs` and mutates state only as `ScriptCommand`s.
The import QC sweep lives in `crates/app/src/app/sweep.rs`, with motion
sampling and segmentation in `crates/media/src/sweep.rs`. It transcribes
with `muse-voice-transcribe-1.0` when a Meta API key is configured,
model-ranks segments through chat completions, and falls back to local
ranking when the model is unavailable. Candidates are suggestions only;
the source asset stays whole. SAM 3.1 subject presence is not wired yet.
Media paths passed as command-line arguments are imported on launch.

## UI rules

- shadcn is the design standard. Tokens and button variants live in
  `crates/app/src/theme.rs`. Use them. Do not hand-roll new colors.
- No verbose tags, badges, or explainer paragraphs in the UI. A control
  gets a short label. Explanation goes in a hover tooltip.
- Every button needs a hover tooltip that says what it does and its
  shortcut if it has one.
- Feedback lives in the status bar. Keep messages to one short sentence.
- Importing shows a placeholder card immediately with the file name and
  known metadata. The thumbnail area stays greyed with a spinner until
  probe, waveform, and thumbnails finish. Placeholders cannot be added
  to the timeline.
- Buttons are borderless at rest. Hover and pressed fills carry the
  affordance. Do not reintroduce hard outlines.
- The media bin is an image grid. Thumbnails and waveforms only. Names,
  metadata, and paths live in the hover tooltip.
- Agent control goes through the QuickJS console in
  `crates/app/src/scripting.rs`. JS enqueues `ScriptCommand`s. It never
  mutates editor state directly, gets no OS or filesystem access from the
  sandbox, and runs under memory and time limits.

## Performance rules

- The target is a responsive editor on weak hardware. Judge with
  `--release`. The status bar shows UI frame ms and RSS. Keep both down.
- No frame-sized allocations per frame at steady state. Pool buffers.
- No process spawn per frame. Reuse decoders. Coalesce stale scrub work.
- Never buffer a whole media decode in memory. Stream it.
- Scrub at low resolution while dragging. Settle at full preview size.
- Export quality is not the place to save time. Preview is the proxy.

## Testing rules

- The chat-edit dev harness lives in `crates/app/src/dev_harness.rs`
  (test-only; `dev-harness` feature enables the `muse exec` driver).
  `cargo test` runs the canned scenarios offline. Live stand-in runs
  need `OPENREEL_LIVE_HARNESS=1`. It never validates Spark/Wonder;
  the live-API checklist is at the top of that file.
- Bug fixes start with a failing test when a local test path exists.
- Media changes need the end-to-end test in `crates/media` to pass. It
  generates samples, decodes, streams, exports, and re-probes the output.
- Screenshot the real window before calling UI work done.
- "It compiles" is not verification. Run the feature surface.

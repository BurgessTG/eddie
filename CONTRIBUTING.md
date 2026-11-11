# Contributing to Eddie

Bug reports, fixes and features are welcome. For a large change, open an
issue first so the design is agreed before the work.

## Build and test

The [README](README.md#requirements) lists what a build needs. Rust is
pinned in `mise.toml`.

```sh
mise exec -- cargo run --release                             # the editor
SDL_AUDIODRIVER=dummy mise exec -- cargo test --workspace    # every test
mise exec -- cargo fmt --all
mise exec -- cargo clippy --workspace --all-targets -- -D warnings
```

CI runs the tests, the format check and clippy on every push and pull
request, and builds once with `--features vulkan`, the build the packages
ship.

## Where code goes

The [layout table](README.md#layout) names the four crates. Two rules
follow from it:

- Anything both the window and the command line need lives once, in
  `eddie-core`. `crates/app` only draws the editor and turns input into
  its calls.
- A script changes the editor only by queueing a `ScriptCommand`
  (`crates/core/src/scripting.rs`). It never touches editor state, files
  or the operating system.

Keyboard shortcuts live in one table, `crates/core/src/shortcuts.rs`. It
drives key handling, menu labels, tooltips and the shortcuts window, so add
a shortcut there.

## Tests

- A bug fix starts with a test that fails without the fix.
- Tests must run in CI: no sound device, window, GPU or installed whisper
  model, and no files outside the repository and the test's own temp
  folder.
- A change to decoding, playback or export must keep the end-to-end test in
  `crates/media` passing. It generates media, decodes, plays, exports and
  probes the result.
- A change to the command line or the script API updates
  [docs/agents.md](docs/agents.md) and `eddie help` with it.

## The window

- Every button has a tooltip that says what it does, with its shortcut.
- A control gets a short label. Explanation goes in the tooltip, and
  feedback goes in the status bar as one short sentence.
- Colours and button styles come from `crates/app/src/theme.rs`.
- Look at the result before sending it: `eddie --snap out.png --size
  1440x900` renders the window to a PNG without a display.

## Performance

Eddie should stay responsive on weak hardware. Judge it with `--release`;
the status bar shows frame time and memory. Do not allocate a frame-sized
buffer per frame, start a process per frame, or hold a whole decoded file
in memory.

## Pull requests

Keep a pull request to one change, and say what you ran to check it. By
contributing you agree to license your work under both of the project's
licenses, [MIT](LICENSE-MIT) and [Apache-2.0](LICENSE-APACHE).

# Packaging

- `aur/PKGBUILD` is the `eddie` package: it builds a tagged release.
- `aur-git/PKGBUILD` is `eddie-git`: it builds the latest `master`.
- `macos/bundle.sh` builds `Eddie.app` (see [macOS](#macos)).

Both PKGBUILDs build with `--features vulkan`, so local transcription runs
on any GPU with a Vulkan driver and on the CPU where there is none. The
binary links only `libvulkan`, which the window already needs. A CUDA
build (`--features cuda`) is faster on NVIDIA cards but does not start
without the CUDA libraries, so it is not packaged.

Each package installs `eddie`, the desktop entry (`eddie.desktop`), the
`.eddie` file type (`eddie.xml`), the icons and the docs.

## Build a package from a checkout

makepkg writes beside the PKGBUILD, so build in a copy:

```sh
mkdir -p /tmp/eddie-pkg && cp packaging/aur-git/PKGBUILD /tmp/eddie-pkg/ && cd /tmp/eddie-pkg
makepkg -si
```

## Cut a release

1. Set `version` in the root `Cargo.toml`, run `cargo check` so
   `Cargo.lock` follows, and date the release in `CHANGELOG.md`. Commit.
2. Tag the commit `vX.Y.Z` and push the tag.
3. In `aur/PKGBUILD` set `pkgver`, reset `pkgrel` to 1 and run `updpkgsums`.
   Build it with `makepkg -s` and install the result.
4. Copy the PKGBUILD to the AUR repository, run
   `makepkg --printsrcinfo > .SRCINFO` there, commit both and push.

## macOS

```sh
packaging/macos/bundle.sh [output folder]
```

It builds an FFmpeg from source (`macos/ffmpeg.sh`), the release binary
against it, and writes `Eddie.app` into `target/release`, or into the
folder given. It needs Xcode's command line tools, Rust, and
`brew install pkgconf cmake meson ninja`. The finished app needs nothing
installed: it runs on Apple Silicon with macOS 11.0 or later.

- `macos/Info.plist` declares the `.eddie` project type with Eddie as its
  owner, and lists Eddie under Open With for movies and audio without
  making it their default app. The script sets the version from the root
  `Cargo.toml`.
- `macos/icon.png` and `macos/project.png` are the app's and the project
  file's icons, 1024 px, rendered from the SVGs beside them with
  `rsvg-convert`. Render them again after changing an SVG; the script
  makes each `.icns` from the PNG.
- `macos/ffmpeg.sh` builds FFmpeg 9.0.2 into `target/ffmpeg-macos`, with
  x264 (export), FreeType and HarfBuzz (titles), dav1d (AV1) and LAME (the
  tests make mp3 samples). Those five are static, so they sit inside
  FFmpeg's own libraries. Each source archive is checked against the
  sha256 in the script. FFmpeg looks for nothing else on the Mac
  (`--disable-autodetect`), so nothing Homebrew has installed ends up in
  the app. The script builds again only when it or
  `MACOSX_DEPLOYMENT_TARGET` (11.0 unless set) has changed.
- The script copies that FFmpeg's `ffmpeg` command to
  `Contents/MacOS/ffmpeg` and the seven libraries either program loads to
  `Contents/Frameworks`, and points each load at
  `@executable_path/../Frameworks`. Eddie uses the `ffmpeg` beside its own
  program before any other. The app is about 52 MB.
- The script stops when a file in the app still loads a library from
  outside it or from anywhere but that FFmpeg. `LSMinimumSystemVersion` is
  the newest macOS any file in the app was built for, 11.0.
- The licences of FFmpeg and the five libraries go to
  `Contents/Resources/licenses/<package>-<version>/`, with `SOURCES.txt`
  listing each source's address and checksum and how FFmpeg was
  configured. FFmpeg is built with `--enable-gpl` for x264, so it is
  under the GPL version 2 or later. Giving the app to someone else means
  meeting that licence, which includes offering the source of FFmpeg and
  those libraries.
- To run the tests on that FFmpeg, put `target/ffmpeg-macos/bin` first in
  `PATH` and set `PKG_CONFIG_PATH=target/ffmpeg-macos/lib/pkgconfig` and
  `MACOSX_DEPLOYMENT_TARGET=11.0`. Every workspace test passes that way.
- `crates/core/build.rs` links clang's runtime library on macOS.
  whisper.cpp's Metal code asks which macOS it runs on, and built for a
  macOS older than 15 the binary does not link without it.
- Without `EDDIE_SIGN_IDENTITY` the signature is ad hoc: the app runs on
  the Mac that built it, and a copy downloaded from elsewhere gets the
  Gatekeeper warning. With
  `EDDIE_SIGN_IDENTITY="Developer ID Application: Name (TEAM)"` every
  program and library is signed with the hardened runtime and a
  timestamp. `macos/notarize.sh` then sends the app to Apple with the
  keychain profile `eddie-notary` (its first lines say how to store it),
  staples Apple's ticket to the app and writes
  `Eddie-<version>-macos-arm64.zip` beside it. Signing with a Developer
  ID and notarizing have not been run yet.
- Finder's icon view shows a preview of the project's text on `.eddie`
  files, not the project icon. Declaring the type as `public.data` instead
  of `public.json` did not change that.

It has only been built and run in virtual machines on Apple Silicon. This
app was built on macOS 15.7 and checked there with Homebrew and the
checkout moved away. It has not run on a second Mac, or on macOS 11 to 14.

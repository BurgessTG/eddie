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
   `Cargo.lock` follows, and date the release in `CHANGELOG.md`: its
   heading becomes `## X.Y.Z (YYYY-MM-DD)`. Commit and push.
2. Tag the commit `vX.Y.Z` and push the tag. `.github/workflows/release.yml`
   does the rest:
   - checks that the tag is the `Cargo.toml` version and that
     `CHANGELOG.md` has its dated section, whose text becomes the notes;
   - builds, signs and notarizes the Mac app (`macos-app.yml`);
   - publishes the GitHub Release with the app, the FFmpeg sources and
     `update.json`, and marks it latest, so every Eddie on Releases is
     offered it within a day;
   - builds `eddie` from the tag in an Arch container, then pushes both
     AUR packages (`release/aur.sh`).

Every `master` commit CI passes is published by `preview.yml` as the
`preview` pre-release, replacing the one before. Only Eddies set to
Updates → Previews look at it.

`release/manifest.sh` writes `update.json`: the version, the build (the
number of commits on `master`, which the app also carries as
`EDDIE_BUILD`), the commit, the notes page, and the Mac zip's address,
size and sha256. `release/aur.sh` can be run by hand; `DRY_RUN=1` shows
what it would push.

### Secrets

The workflows need these repository secrets (Settings → Secrets and
variables → Actions). Without the Mac ones `preview.yml` skips with a
notice and a release fails before publishing anything.

- `MACOS_CERTIFICATE_P12` and `MACOS_CERTIFICATE_PASSWORD`: the
  Developer ID Application identity exported from Keychain Access as a
  `.p12` with a password, then `base64 -i identity.p12 | pbcopy`.
- `APPSTORE_KEY_P8`, `APPSTORE_KEY_ID` and `APPSTORE_ISSUER_ID`: an App
  Store Connect API key with the Developer role (App Store Connect →
  Users and Access → Integrations → App Store Connect API). The secret is
  the text of the downloaded `.p8`. `notarytool` uses it in place of the
  `eddie-notary` keychain profile.
- `AUR_SSH_KEY`: a private SSH key without a passphrase whose public half
  is on the AUR account that maintains `eddie` and `eddie-git` (My Account
  → SSH Public Key). Make a new key for this only.

`gh secret set NAME < file` sets one from a file without showing it.

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
  `Eddie-<version>-macos-arm64.zip` beside it. Both ran on macOS 26.6
  with Xcode 26.6 (2026-11-12): the hardened runtime needed no
  entitlements, Apple accepted the app in under a minute, `stapler
  validate` passed and `spctl -a -vv` answers `accepted`,
  `source=Notarized Developer ID`.
- When cargo cannot fetch Rune (`the SSL certificate is invalid`),
  Homebrew's OpenSSL has no certificate file. Run the script with
  `CARGO_NET_GIT_FETCH_WITH_CLI=true`, so cargo fetches with `git`.
- Finder's icon view shows a preview of the project's text on `.eddie`
  files, not the project icon. Declaring the type as `public.data` instead
  of `public.json` did not change that.

It has been built and run on two machines, each running the copy it
built. A virtual machine on macOS 15.7 checked it with Homebrew and the
checkout moved away. A MacBook Air M1 on macOS 26.6 checked it from
`/Applications`: every command, an export, `--snap`, a local
transcription on Metal, the workspace tests, and the window, which loaded
no library from Homebrew and played a 12 s timeline in time without a
sound overrun. A copy has not been carried to a Mac that did not build
it, and it has not run on macOS 11 to 14.

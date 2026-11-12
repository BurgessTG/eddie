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

It builds the release binary and writes `Eddie.app` into `target/release`,
or into the folder given. It needs Xcode's command line tools and the
Homebrew packages the build needs (see the README).

- `macos/Info.plist` declares the `.eddie` project type with Eddie as its
  owner, and lists Eddie under Open With for movies and audio without
  making it their default app. The script sets the version from the root
  `Cargo.toml`.
- `macos/icon.png` and `macos/project.png` are the app's and the project
  file's icons, 1024 px, rendered from the SVGs beside them with
  `rsvg-convert`. Render them again after changing an SVG; the script
  makes each `.icns` from the PNG.
- FFmpeg is bundled. The script finds the FFmpeg the binary was linked
  against, copies its `ffmpeg` command to `Contents/MacOS/ffmpeg` and every
  library either program loads to `Contents/Frameworks`, and points each
  load at `@executable_path/../Frameworks`. Eddie uses the `ffmpeg` beside
  its own program before any other. With Homebrew's `ffmpeg-full` that is
  100 libraries from 79 packages and an app of about 117 MB (27 MB
  without them).
- The script stops when a file in the app still loads a library from
  outside it, or when a bundled package has no licence text. The texts go
  to `Contents/Resources/licenses/<package>-<version>/`.
- `LSMinimumSystemVersion` is the newest macOS any bundled file was built
  for (27.2 on the build machine: Homebrew builds for the macOS it runs
  on).
- `ffmpeg-full` is built with `--enable-gpl --enable-version3`, so the
  bundled FFmpeg is under the GPL version 3. Giving the app to someone
  else means meeting that licence, which includes offering the source of
  those libraries.
- The signature is ad hoc. The app is not signed with a Developer ID or
  notarized, so a copy downloaded from elsewhere gets the Gatekeeper
  warning.
- Finder's icon view shows a preview of the project's text on `.eddie`
  files, not the project icon. Declaring the type as `public.data` instead
  of `public.json` did not change that.

It has only been built and run in a virtual machine (macOS 27.2, Apple
Silicon).

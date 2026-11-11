# Packaging

- `aur/PKGBUILD` is the `eddie` package: it builds a tagged release.
- `aur-git/PKGBUILD` is `eddie-git`: it builds the latest `master`.

Both build with `--features vulkan`, so local transcription runs on any GPU
with a Vulkan driver and on the CPU where there is none. The binary links
only `libvulkan`, which the window already needs. A CUDA build
(`--features cuda`) is faster on NVIDIA cards but does not start without
the CUDA libraries, so it is not packaged.

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

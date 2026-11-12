#!/bin/sh
# Builds Eddie.app: the release binary, its icon and an Info.plist that lets
# Finder open .eddie projects and media with it.
#
#   packaging/macos/bundle.sh [output folder]
#
# The app lands in target/release unless a folder is given. It needs Xcode's
# command line tools (sips, iconutil, plutil, codesign) and the same FFmpeg
# the build needs, since the binary links Homebrew's libraries where they are.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
target=${CARGO_TARGET_DIR:-$root/target}
out=${1:-$target/release}
app=$out/Eddie.app

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
[ -n "$version" ] || { echo "No version in $root/Cargo.toml" >&2; exit 1; }

(cd "$root" && cargo build --release --locked -p openreel-app)

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target/release/eddie" "$app/Contents/MacOS/eddie"

cp "$here/Info.plist" "$app/Contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$version" "$app/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$version" "$app/Contents/Info.plist"

# icns <1024 px png> <name>: writes Resources/<name>.icns. iconutil wants
# every size as a file with this exact name.
icns() {
    iconset=$(mktemp -d)/$2.iconset
    mkdir "$iconset"
    for size in 16 32 128 256 512; do
        sips -z "$size" "$size" "$1" --out "$iconset/icon_${size}x${size}.png" >/dev/null
        double=$((size * 2))
        sips -z "$double" "$double" "$1" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    done
    iconutil --convert icns --output "$app/Contents/Resources/$2.icns" "$iconset"
    rm -rf "$(dirname "$iconset")"
}
icns "$here/icon.png" eddie
icns "$here/project.png" eddie-project

# An ad-hoc signature: Apple silicon runs nothing unsigned, and this needs no
# Apple account. A downloaded copy still gets the Gatekeeper warning.
codesign --force --sign - "$app"

echo "$app"

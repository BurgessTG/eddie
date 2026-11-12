#!/bin/sh
# Builds Eddie.app: the release binary, an FFmpeg built from source for it
# (ffmpeg.sh), its icons and an Info.plist that lets Finder open .eddie
# projects and media with it.
#
#   packaging/macos/bundle.sh [output folder]
#
# The app lands in target/release unless a folder is given. Building it
# needs Xcode's command line tools (sips, iconutil, plutil, otool,
# install_name_tool, codesign), Rust, and what ffmpeg.sh needs. The
# finished app needs none of them: it runs on a Mac with macOS
# MACOSX_DEPLOYMENT_TARGET (11.0 unless set) or later and nothing else
# installed.
#
# Its signature is ad hoc unless EDDIE_SIGN_IDENTITY names one from the
# keychain. An app for other people is signed with a Developer ID, then
# notarized by notarize.sh:
#
#   EDDIE_SIGN_IDENTITY="Developer ID Application: Name (TEAM)" packaging/macos/bundle.sh
#   packaging/macos/notarize.sh
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
target=${CARGO_TARGET_DIR:-$root/target}
out=${1:-$target/release}
app=$out/Eddie.app
macos=$app/Contents/MacOS
frameworks=$app/Contents/Frameworks
resources=$app/Contents/Resources
plist=$app/Contents/Info.plist
tab=$(printf '\t')

die() {
    echo "$1" >&2
    exit 1
}

# quiet <command>: runs it and shows what it printed only when it fails.
# install_name_tool warns about the signature on every change it makes.
quiet() {
    said=$("$@" 2>&1) || die "$*: $said"
}

# loads <file>: the libraries it names, one per line.
loads() {
    otool -L "$1" | sed -e 1d -e 's/^[[:space:]]*//' -e 's/ (compatibility version .*$//'
}

# rpaths <file>: the folders it searches for @rpath libraries.
rpaths() {
    otool -l "$1" | sed -n '/LC_RPATH/{n;n;s/^ *path //;s/ (offset [0-9]*)$//;p;}'
}

# in_rpath <file> <name>: the library <name> in the first of the file's
# search folders that has it.
in_rpath() {
    rpaths "$1" | while read -r rpath; do
        case $rpath in
            @loader_path*) rpath=$(dirname "$1")${rpath#@loader_path} ;;
        esac
        if [ -e "$rpath/$2" ]; then
            echo "$rpath/$2"
            break
        fi
    done
}

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
[ -n "$version" ] || die "No version in $root/Cargo.toml"

# The binary and its FFmpeg are built for the same macOS, and the binary
# against that FFmpeg, whatever other FFmpeg this Mac has.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
ffmpeg_home=$("$here/ffmpeg.sh")
(cd "$root" &&
    PKG_CONFIG_PATH="$ffmpeg_home/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" \
        cargo build --release --locked -p openreel-app)

rm -rf "$app"
mkdir -p "$macos" "$frameworks" "$resources/licenses"
cp "$target/release/eddie" "$macos/eddie"

# Eddie runs the `ffmpeg` command for export, titles, sheets and strips, so
# the one from the same FFmpeg goes beside it, where Eddie looks first.
avcodec=$(loads "$macos/eddie" | grep '/libavcodec\.' || true)
case $avcodec in
    "$ffmpeg_home"/lib/*) ;;
    *) die "$target/release/eddie was not built with the FFmpeg in $ffmpeg_home (libavcodec: ${avcodec:-not loaded})" ;;
esac
ffmpeg=$ffmpeg_home/bin/ffmpeg
cp "$ffmpeg" "$macos/ffmpeg"
chmod u+w "$macos/ffmpeg"

# Every library the two programs load that is not part of macOS is copied
# into Frameworks and loaded from there, and so is every library a copy
# loads. `todo` lists the files still to read, each with the file it was
# copied from: @loader_path and @rpath in a copy mean the original's folder.
# `copied` lists each library in Frameworks with its original.
todo=$(mktemp)
copied=$(mktemp)
trap 'rm -f "$todo" "$copied"' EXIT
printf '%s\t%s\n' "$macos/eddie" "$target/release/eddie" >>"$todo"
printf '%s\t%s\n' "$macos/ffmpeg" "$ffmpeg" >>"$todo"

next=1
while entry=$(sed -n "${next}p" "$todo") && [ -n "$entry" ]; do
    next=$((next + 1))
    file=${entry%%"$tab"*}
    original=${entry#*"$tab"}
    itself=$(otool -D "$file" | sed -n 2p)
    while read -r library; do
        case $library in
            "" | "$itself" | /usr/lib/* | /System/*) continue ;;
            @loader_path/*) from=$(dirname "$original")/${library#@loader_path/} ;;
            @rpath/*) from=$(in_rpath "$original" "${library#@rpath/}") ;;
            /*) from=$library ;;
            *) from= ;;
        esac
        [ -n "$from" ] && [ -e "$from" ] || die "$original loads $library, which was not found"
        # The app carries the licences of that FFmpeg and of nothing else.
        case $from in
            "$ffmpeg_home"/lib/*) ;;
            *) die "$original loads $library, which is part of neither macOS nor the FFmpeg in $ffmpeg_home" ;;
        esac
        real=$(realpath "$from")
        name=$(basename "$real")
        known=$(awk -F "$tab" -v name="$name" '$1 == name { print $2 }' "$copied")
        if [ -z "$known" ]; then
            cp "$real" "$frameworks/$name"
            chmod u+w "$frameworks/$name"
            printf '%s\t%s\n' "$name" "$real" >>"$copied"
            printf '%s\t%s\n' "$frameworks/$name" "$real" >>"$todo"
        elif [ "$known" != "$real" ]; then
            die "Two libraries are named $name: $known and $real"
        fi
        quiet install_name_tool -change "$library" "@executable_path/../Frameworks/$name" "$file"
    done <<EOF
$(loads "$file")
EOF
    if [ -n "$itself" ]; then
        quiet install_name_tool -id "@executable_path/../Frameworks/$(basename "$file")" "$file"
    fi
    while read -r rpath; do
        [ -z "$rpath" ] || quiet install_name_tool -delete_rpath "$rpath" "$file"
    done <<EOF
$(rpaths "$file")
EOF
done

# A file that still loads something from outside the app would run on this
# Mac and fail on the next.
minimum=0
for file in "$macos/eddie" "$macos/ffmpeg" "$frameworks"/*.dylib; do
    while read -r library; do
        case $library in
            /usr/lib/* | /System/* | @executable_path/../Frameworks/*) ;;
            *) die "$file still loads $library" ;;
        esac
    done <<EOF
$(loads "$file")
EOF
    [ -z "$(rpaths "$file")" ] || die "$file still searches $(rpaths "$file")"
    built_for=$(otool -l "$file" | awk '$1 == "minos" { print $2 }')
    [ -n "$built_for" ] || die "Cannot tell which macOS $file was built for"
    minimum=$(printf '%s\n%s\n' "$minimum" "$built_for" | sort -t . -k 1,1n -k 2,2n -k 3,3n | tail -n 1)
done

# The app asks for the newest macOS any of its files was built for.
cp "$here/Info.plist" "$plist"
plutil -replace CFBundleShortVersionString -string "$version" "$plist"
plutil -replace CFBundleVersion -string "$version" "$plist"
plutil -replace LSMinimumSystemVersion -string "$minimum" "$plist"

# The licences go with the app: Eddie's, and those of FFmpeg and the
# libraries built into it, with the list of the sources they came from.
cp "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$resources/licenses/"
cp -R "$ffmpeg_home/licenses/." "$resources/licenses/"

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
    iconutil --convert icns --output "$resources/$2.icns" "$iconset"
    rm -rf "$(dirname "$iconset")"
}
icns "$here/icon.png" eddie
icns "$here/project.png" eddie-project

# Apple silicon runs nothing unsigned, and changing a library's names
# broke the signature it had. An ad-hoc signature needs no Apple account:
# the app runs, but a downloaded copy gets the Gatekeeper warning. With an
# identity every program and library gets the hardened runtime and a
# timestamp, which Apple's notary service requires. The libraries are
# signed first: the app's signature covers theirs.
identity=${EDDIE_SIGN_IDENTITY:--}
sign() {
    if [ "$identity" = - ]; then
        quiet codesign --force --sign - "$1"
    else
        quiet codesign --force --options runtime --timestamp --sign "$identity" "$1"
    fi
}
for file in "$frameworks"/*.dylib "$macos/ffmpeg"; do
    sign "$file"
done
sign "$app"
codesign --verify --deep --strict "$app"

echo "$app"

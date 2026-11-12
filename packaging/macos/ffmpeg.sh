#!/bin/sh
# Builds the FFmpeg that Eddie.app carries, from source, with the five
# libraries Eddie needs beyond FFmpeg's own code: x264 for export, FreeType
# and HarfBuzz for titles (the drawtext filter), dav1d for AV1, and LAME,
# the mp3 encoder Eddie's tests make their samples with, so the tests run
# on the FFmpeg the app carries.
#
#   packaging/macos/ffmpeg.sh
#
# It prints the folder the build is installed in, target/ffmpeg-macos, and
# builds only when that folder is not already this script's work. It needs
# Xcode's command line tools, pkg-config, meson and ninja.
#
# Homebrew's FFmpeg is built for the macOS it is installed on and loads a
# hundred libraries. This one is built for MACOSX_DEPLOYMENT_TARGET (11.0
# unless set, the first macOS on Apple silicon) and loads nothing that is
# not part of macOS.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
target=${CARGO_TARGET_DIR:-$root/target}
prefix=$target/ffmpeg-macos
downloads=$target/ffmpeg-sources
build=$prefix/build
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"

die() {
    echo "$1" >&2
    exit 1
}

# Name, version, the archive's sha256 and where it comes from. FFmpeg,
# FreeType, HarfBuzz, dav1d and LAME are releases, with the checksums
# Homebrew records for the same files. x264 has no releases: this is the
# commit Homebrew builds.
sources() {
    cat <<'EOF'
x264 r3222 6eeb82934e69fd51e043bd8c5b0d152839638d1ce7aa4eea65a3fedcf83ff224 https://code.videolan.org/videolan/x264/-/archive/b35605ace3ddf7c1a5d67a2eb553f034aef41d55/x264-b35605ace3ddf7c1a5d67a2eb553f034aef41d55.tar.bz2
freetype 2.15.0 93cd47eb719c325107f2d3cf087d06299e944c3b48a40b55537302304dea1dd5 https://download.savannah.gnu.org/releases/freetype/freetype-2.15.0.tar.xz
harfbuzz 15.1.0 0fc7c565443a5f4be7da65ac5ee5bd737d9b158dc908708b7689a0a5796b0c8d https://github.com/harfbuzz/harfbuzz/releases/download/15.1.0/harfbuzz-15.1.0.tar.xz
dav1d 1.5.4 2abfb0c89212e6e4733a54e0ae509ec00a5b845a6360946f918806e14aedb011 https://code.videolan.org/videolan/dav1d/-/archive/1.5.4/dav1d-1.5.4.tar.bz2
lame 4.0 3df5124d5ad3a98312ffd7ba6a9b36230e4f8a3e66d3ce0f425e336c32d216eb https://downloads.sourceforge.net/project/lame/lame/4.0/lame-4.0.tar.gz
ffmpeg 9.0.2 8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz
EOF
}

# What FFmpeg is built with. Everything FFmpeg has of its own stays in:
# every decoder and container a user may import. Nothing is looked for on
# the Mac (--disable-autodetect), so a library Homebrew happens to have
# cannot end up loaded by the app; what macOS itself provides is named.
# Of the devices only lavfi stays, which export reads silence from: the
# app has no use for a camera or a microphone and must not ask for one.
configuration="--enable-shared --disable-static --enable-gpl \
--enable-libx264 --enable-libfreetype --enable-libharfbuzz --enable-libdav1d \
--enable-libmp3lame \
--disable-autodetect --enable-videotoolbox --enable-audiotoolbox \
--enable-zlib --enable-bzlib \
--disable-indevs --enable-indev=lavfi --disable-outdevs \
--disable-network --disable-ffplay --disable-doc \
--disable-debug --pkg-config-flags=--static"

case $prefix in
    *[[:space:]]*) die "FFmpeg's build cannot work in a folder with a space in its name: $prefix" ;;
esac

# The folder is this script's work when it was finished by this very
# script for this macOS.
key="$(shasum -a 256 "$0" | cut -d ' ' -f 1) for macOS $MACOSX_DEPLOYMENT_TARGET"
if [ "$(cat "$prefix/built-by" 2>/dev/null)" = "$key" ]; then
    echo "$prefix"
    exit 0
fi

for tool in clang make pkg-config meson ninja curl; do
    command -v "$tool" >/dev/null || die "Building FFmpeg needs $tool, which is not installed"
done

rm -rf "$prefix"
mkdir -p "$prefix/licenses" "$prefix/log" "$build" "$downloads"
jobs=$(sysctl -n hw.ncpu)

# pkg-config sees only what this script built.
export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"

# fetch <name>: downloads its archive unless it is here already, checks it
# and unpacks it to $build/<name>.
fetch() {
    # shellcheck disable=SC2046
    set -- $(sources | awk -v name="$1" '$1 == name')
    archive=$downloads/$(basename "$4")
    if [ ! -f "$archive" ]; then
        echo "Downloading $1 $2" >&2
        curl -fsSL --retry 3 -o "$archive.part" "$4"
        mv "$archive.part" "$archive"
    fi
    sum=$(shasum -a 256 "$archive" | cut -d ' ' -f 1)
    [ "$sum" = "$3" ] ||
        die "$archive is not $1 $2: its sha256 is $sum, not $3. Delete it to download it again."
    mkdir -p "$build/$1"
    tar -xf "$archive" -C "$build/$1" --strip-components 1
    echo "Building $1 $2" >&2
}

# run <name> <command>: runs it in $build/<name>. What it prints goes to a
# log, and the end of the log is shown when it fails.
run() {
    name=$1
    shift
    if ! (cd "$build/$name" && "$@") >>"$prefix/log/$name.log" 2>&1; then
        tail -n 40 "$prefix/log/$name.log" >&2
        die "$name: $1 failed. The whole log is $prefix/log/$name.log"
    fi
}

# licence <name> <file>...: keeps the texts a copy of the library must
# carry, in licenses/<name>-<version>.
licence() {
    name=$1
    shift
    texts=$prefix/licenses/$name-$(sources | awk -v name="$name" '$1 == name { print $2 }')
    mkdir -p "$texts"
    for text; do
        cp "$build/$name/$text" "$texts/"
    done
}

# The five libraries are static, so they end up inside FFmpeg's own and
# the app carries no file for them.
fetch x264
run x264 ./configure --prefix="$prefix" --enable-static --enable-pic --disable-cli --disable-opencl
run x264 make -j "$jobs" install
licence x264 COPYING

fetch freetype
run freetype ./configure --prefix="$prefix" --enable-static --disable-shared --with-pic \
    --with-zlib=yes --with-bzip2=no --with-png=no --with-harfbuzz=no --with-brotli=no
run freetype make -j "$jobs" install
licence freetype LICENSE.TXT docs/FTL.TXT docs/GPLv2.TXT

fetch harfbuzz
run harfbuzz meson setup out --prefix="$prefix" --libdir=lib --buildtype=release \
    --default-library=static --auto-features=disabled -Dfreetype=enabled \
    -Dtests=disabled -Ddocs=disabled -Dutilities=disabled
run harfbuzz ninja -C out install
licence harfbuzz COPYING

fetch dav1d
run dav1d meson setup out --prefix="$prefix" --libdir=lib --buildtype=release \
    --default-library=static -Denable_tools=false -Denable_tests=false
run dav1d ninja -C out install
licence dav1d COPYING

# LAME calls ID3 functions it never declares, which newer compilers refuse
# and C23 forbids; Homebrew builds it with the same two settings.
fetch lame
run lame env ac_cv_prog_cc_c23=no CFLAGS="-O2 -Wno-implicit-function-declaration" \
    ./configure --prefix="$prefix" --enable-static --disable-shared --with-pic \
    --disable-frontend --disable-decoder
run lame make -j "$jobs" install
licence lame COPYING LICENSE

# FFmpeg finds LAME by its header and library, not through pkg-config, so
# it is told where they are.
fetch ffmpeg
# shellcheck disable=SC2086
run ffmpeg ./configure --prefix="$prefix" --cc=clang \
    --extra-cflags="-I$prefix/include" --extra-ldflags="-L$prefix/lib" $configuration
run ffmpeg make -j "$jobs" install
licence ffmpeg LICENSE.md COPYING.GPLv2 COPYING.LGPLv2.1

has() {
    "$prefix/bin/ffmpeg" -v quiet "-$1" | grep -q " $2 " ||
        die "The FFmpeg that was built has no $2 among its $1"
}
has filters drawtext
has encoders libx264
has encoders libmp3lame
has decoders libdav1d
has devices lavfi

# The GPL asks that whoever gets the program can get its source: the app
# carries this list beside the licences.
{
    echo "The FFmpeg in this app was built by packaging/macos/ffmpeg.sh in"
    echo "https://github.com/BurgessTG/eddie from these sources, for macOS"
    echo "$MACOSX_DEPLOYMENT_TARGET and later:"
    echo
    sources | while read -r name version sum url; do
        echo "$name $version"
        echo "  $url"
        echo "  sha256 $sum"
    done
    echo
    echo "FFmpeg was configured with:"
    echo "  $configuration" | tr -s ' '
} >"$prefix/licenses/SOURCES.txt"

rm -rf "$build"
echo "$key" >"$prefix/built-by"
echo "$prefix"

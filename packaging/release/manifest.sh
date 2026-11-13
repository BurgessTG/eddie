#!/bin/sh
# Writes the update.json that Eddie reads to find a newer build
# (crates/core/src/update.rs). The release and preview workflows publish it
# beside the files it names.
#
#   manifest.sh <version> <build> <commit> <notes url> [<mac zip> <its url>]
#
# <build> is the number of commits on master the build came from. The Mac
# zip's size and sha256 are read from the file; Eddie installs a download
# only when both match.
set -eu

[ $# -eq 4 ] || [ $# -eq 6 ] || {
    echo "usage: manifest.sh <version> <build> <commit> <notes url> [<mac zip> <its url>]" >&2
    exit 1
}

# Every value is a version, a number, a hash or a URL: none needs escaping,
# so refuse anything that would.
for value in "$@"; do
    case $value in
    *'"'* | *'\'* | *'
'*) echo "manifest.sh: $value cannot go in JSON as it is" >&2; exit 1 ;;
    esac
done
case $2 in '' | *[!0-9]*) echo "manifest.sh: build $2 is not a number" >&2; exit 1 ;; esac

printf '{\n  "version": "%s",\n  "build": %s,\n  "commit": "%s",\n  "notes": "%s"' "$1" "$2" "$3" "$4"
if [ $# -eq 6 ]; then
    if command -v sha256sum >/dev/null; then
        sum=$(sha256sum "$5" | cut -d ' ' -f 1)
    else
        sum=$(shasum -a 256 "$5" | cut -d ' ' -f 1)
    fi
    size=$(wc -c <"$5" | tr -d ' ')
    printf ',\n  "macos-arm64": {\n    "url": "%s",\n    "sha256": "%s",\n    "size": %s\n  }' "$6" "$sum" "$size"
fi
printf '\n}\n'

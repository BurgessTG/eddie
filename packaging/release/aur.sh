#!/bin/sh
# Publishes Eddie's two AUR packages for a release:
#
#   packaging/release/aur.sh <tag> <build> [work folder]
#
# - eddie: packaging/aur/PKGBUILD at that version, with the checksum of
#   GitHub's archive of the tag and <build> (commits on master at the tag).
# - eddie-git: packaging/aur-git/PKGBUILD, its pkgver set to the latest
#   master.
#
# Each is committed to its AUR repository with a fresh .SRCINFO and pushed
# when it changed. Run it as a user who is not root (makepkg refuses root),
# on Arch with base-devel, git, openssh and pacman-contrib, and an SSH key
# the AUR knows for the package's maintainer. With AUR_BUILD=1 the eddie
# package is built from the tag before anything is pushed. With DRY_RUN=1
# the repositories are read over HTTPS and nothing is pushed.
set -eu

[ $# -ge 2 ] || { echo "usage: aur.sh <tag> <build> [work folder]" >&2; exit 1; }
tag=$1
build=$2
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
work=${3:-$(mktemp -d)}
version=${tag#v}
case $version in
'' | *[!0-9.]*) echo "aur.sh: $tag is not a release tag such as v0.3.0" >&2; exit 1 ;;
esac
case $build in '' | *[!0-9]*) echo "aur.sh: build $build is not a number" >&2; exit 1 ;; esac

remote() {
    if [ "${DRY_RUN:-}" = 1 ]; then
        echo "https://aur.archlinux.org/$1.git"
    else
        echo "ssh://aur@aur.archlinux.org/$1.git"
    fi
}

# publish <package> <message>: commit the PKGBUILD and .SRCINFO in
# $work/<package> and push them when they changed.
publish() {
    (
        cd "$work/$1"
        makepkg --printsrcinfo >.SRCINFO
        git add PKGBUILD .SRCINFO
        if git diff --cached --quiet; then
            echo "$1: already up to date"
            return
        fi
        git -c user.name="Jacob Burgess" -c user.email="burgesstg@users.noreply.github.com" \
            commit -q -m "$2"
        if [ "${DRY_RUN:-}" = 1 ]; then
            echo "$1: would push \"$2\":"
            git show --stat HEAD | sed 's/^/  /'
        else
            git push -q origin HEAD:master
            echo "$1: pushed \"$2\""
        fi
    )
}

mkdir -p "$work"
for package in eddie eddie-git; do
    rm -rf "${work:?}/$package"
    git clone -q "$(remote "$package")" "$work/$package"
done

# eddie: the tagged release.
cp "$root/packaging/aur/PKGBUILD" "$work/eddie/PKGBUILD"
(
    cd "$work/eddie"
    sed -i -e "s/^pkgver=.*/pkgver=$version/" -e 's/^pkgrel=.*/pkgrel=1/' \
        -e "s/^_build=.*/_build=$build/" PKGBUILD
    updpkgsums
    if [ "${AUR_BUILD:-}" = 1 ]; then
        makepkg --syncdeps --noconfirm --cleanbuild
        rm -rf src pkg ./*.pkg.tar.* ./*.tar.gz
    fi
)
publish eddie "Update to $version"

# eddie-git: the latest master. makepkg works out its pkgver from a clone.
cp "$root/packaging/aur-git/PKGBUILD" "$work/eddie-git/PKGBUILD"
(
    cd "$work/eddie-git"
    makepkg --nobuild --nodeps --noprepare
    rm -rf src eddie
)
publish eddie-git "Update to $(sed -n 's/^pkgver=//p' "$work/eddie-git/PKGBUILD")"

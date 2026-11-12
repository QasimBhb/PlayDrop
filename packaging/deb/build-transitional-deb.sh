#!/usr/bin/env bash
# Build the transitional playdrop-cosmic-applet package.
#
# Usage: packaging/deb/build-transitional-deb.sh [--package-version <version>]
#   --package-version <version>   write this version instead of the workspace's, to test an
#                                 upgrade locally. Never use it for a release.
#
# Output: packaging/deb/dist/playdrop-cosmic-applet_<version>_all.deb
#
# Up to 0.11.0 the COSMIC panel applet was a package of its own that depended on
# `playdrop (= <version>)`. From 0.12.0 the applet is inside the playdrop package. This
# empty package (only its doc folder) takes the old one's place, so that:
#   - `apt-get install --no-remove ./playdrop-<v>.deb ./playdrop-cosmic-applet-<v>.deb`,
#     which is what 0.11.0's built-in updater runs when the applet is installed, finds an
#     upgrade for every package and removes nothing;
#   - once installed, it depends on playdrop with no upper version bound, so it never holds
#     a later playdrop back and can be removed at any time.
# The release keeps publishing it as playdrop-cosmic-applet-<v>-linux-amd64.deb (the name
# 0.11.0 looks for) for two releases after the merge (docs/release/distribution.md).
set -euo pipefail
umask 022

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
dist="$here/dist"
name=playdrop-cosmic-applet

# Keep in step with build-deb.sh: the first version whose playdrop package carries the applet.
first_merged=0.12.0

version_override=
while [ $# -gt 0 ]; do
    case "$1" in
        --package-version)
            [ $# -ge 2 ] || { echo "--package-version needs a version" >&2; exit 2; }
            version_override="$2"
            shift
            ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

command -v dpkg-deb >/dev/null || { echo "missing dpkg-deb" >&2; exit 1; }

version="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$repo/Cargo.toml" | head -n1)"
[ -n "$version" ] || { echo "could not read the workspace version" >&2; exit 1; }
version="${version_override:-$version}"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
root="$stage/${name}_${version}_all"

# Debian policy: even an empty package carries its copyright file.
install -Dm644 "$here/debian/copyright" "$root/usr/share/doc/$name/copyright"

install -d "$root/DEBIAN"
installed_size="$(du -sk --exclude=DEBIAN "$root" | cut -f1)"
sed -e "s/@VERSION@/$version/" -e "s/@INSTALLED_SIZE@/$installed_size/" \
    -e "s/@FIRST_MERGED@/$first_merged/" \
    "$here/debian/transitional-control.in" > "$root/DEBIAN/control"

mkdir -p "$dist"
out="$dist/${name}_${version}_all.deb"
dpkg-deb --build --root-owner-group "$root" "$out"

if command -v lintian >/dev/null; then
    lintian --no-tag-display-limit "$out" || true
fi

echo "built $out"

#!/usr/bin/env bash
# Build the optional COSMIC panel applet .deb for Pop!_OS 24.04.
#
# Usage: packaging/deb/build-applet-deb.sh [--no-build]
#   --no-build   reuse the release binary already built (under CARGO_TARGET_DIR
#                when it is set, otherwise apps/cosmic-applet/target/) instead of
#                running cargo
#
# Output: packaging/deb/dist/playdrop-cosmic-applet_<version>_<arch>.deb
#
# The applet is a separate package on purpose: it is optional, only useful on
# COSMIC, and built against a pinned libcosmic from git (apps/cosmic-applet has
# its own Cargo.lock). Installing it adds a panel applet to choose in COSMIC
# Settings; it changes nothing until the user adds it to a panel.
set -euo pipefail
umask 022

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
applet="$repo/apps/cosmic-applet"
dist="$here/dist"
bin=playdrop-cosmic-applet

build=1
for arg in "$@"; do
    case "$arg" in
        --no-build) build=0 ;;
        -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

for tool in dpkg-deb dpkg; do
    command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 1; }
done

# One version number covers every part of PlayDrop. The applet has its own manifest, so
# refuse to package it when that manifest was not bumped with the workspace.
version="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$repo/Cargo.toml" | head -n1)"
[ -n "$version" ] || { echo "could not read the workspace version" >&2; exit 1; }
applet_version="$(sed -n '/^\[package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$applet/Cargo.toml" | head -n1)"
if [ "$applet_version" != "$version" ]; then
    echo "apps/cosmic-applet is version '$applet_version' but the workspace is '$version';" >&2
    echo "set the applet's version and run 'cargo update -p playdrop-ipc -p playdrop-core' there" >&2
    exit 1
fi
arch="$(dpkg --print-architecture)"

bindir="${CARGO_TARGET_DIR:-$applet/target}/release"

if [ "$build" = 1 ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
    cargo build --release --locked --manifest-path "$applet/Cargo.toml"
fi
[ -x "$bindir/$bin" ] || { echo "missing $bindir/$bin" >&2; exit 1; }

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
root="$stage/${bin}_${version}_${arch}"

install -Dm755 "$bindir/$bin" "$root/usr/bin/$bin"
if command -v strip >/dev/null; then
    strip --strip-unneeded "$root/usr/bin/$bin"
fi

# COSMIC Settings lists every desktop file with X-CosmicApplet=true. The panel icon comes
# from the playdrop package, which this one depends on, so no file is shipped twice.
install -Dm644 "$applet/data/org.playdrop.PlayDrop.CosmicApplet.desktop" \
    "$root/usr/share/applications/org.playdrop.PlayDrop.CosmicApplet.desktop"

install -Dm644 "$here/debian/copyright" "$root/usr/share/doc/$bin/copyright"

install -d "$root/DEBIAN"
installed_size="$(du -sk --exclude=DEBIAN "$root" | cut -f1)"
sed -e "s/@VERSION@/$version/g" -e "s/@ARCH@/$arch/" -e "s/@INSTALLED_SIZE@/$installed_size/" \
    "$here/debian/applet-control.in" > "$root/DEBIAN/control"
install -m755 "$here/debian/postinst" "$root/DEBIAN/postinst"
install -m755 "$here/debian/postrm"   "$root/DEBIAN/postrm"

mkdir -p "$dist"
out="$dist/${bin}_${version}_${arch}.deb"
dpkg-deb --build --root-owner-group "$root" "$out"

if command -v lintian >/dev/null; then
    lintian --no-tag-display-limit "$out" || true
fi

echo "built $out"

#!/usr/bin/env bash
# Build the PlayDrop .deb for Pop!_OS 24.04 and Ubuntu 24.04.
#
# Usage: packaging/deb/build-deb.sh [--no-build]
#   --no-build   reuse target/release binaries instead of running cargo
#
# Output: packaging/deb/dist/playdrop_<version>_<arch>.deb
#
# This is a plain dpkg-deb build rather than cargo-deb because cargo-deb reads
# its metadata only from [package.metadata.deb] in Cargo.toml, and packaging is
# kept out of the crate manifests.
set -euo pipefail
umask 022

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
linux="$repo/packaging/linux"
dist="$here/dist"

build=1
for arg in "$@"; do
    case "$arg" in
        --no-build) build=0 ;;
        -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

for tool in dpkg-deb dpkg; do
    command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 1; }
done

# Version comes from [workspace.package] in the root Cargo.toml.
version="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$repo/Cargo.toml" | head -n1)"
[ -n "$version" ] || { echo "could not read the workspace version" >&2; exit 1; }
arch="$(dpkg --print-architecture)"

if [ "$build" = 1 ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
    cargo build --release --locked --manifest-path "$repo/Cargo.toml" \
        -p playdropd -p playdrop-desktop
fi

for bin in playdropd playdrop-desktop; do
    [ -x "$repo/target/release/$bin" ] || { echo "missing target/release/$bin" >&2; exit 1; }
done

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
root="$stage/playdrop_${version}_${arch}"

# Binaries, stripped to keep the package small.
install -Dm755 "$repo/target/release/playdropd"        "$root/usr/bin/playdropd"
install -Dm755 "$repo/target/release/playdrop-desktop" "$root/usr/bin/playdrop-desktop"
if command -v strip >/dev/null; then
    strip --strip-unneeded "$root/usr/bin/playdropd" "$root/usr/bin/playdrop-desktop"
fi

# Desktop integration, shared with the Flatpak.
install -Dm644 "$linux/org.playdrop.PlayDrop.desktop" \
    "$root/usr/share/applications/org.playdrop.PlayDrop.desktop"
install -Dm644 "$linux/org.playdrop.Daemon.desktop" \
    "$root/etc/xdg/autostart/org.playdrop.Daemon.desktop"
install -Dm644 "$linux/org.playdrop.PlayDrop.Daemon1.service" \
    "$root/usr/share/dbus-1/services/org.playdrop.PlayDrop.Daemon1.service"
install -Dm644 "$linux/playdropd.service" \
    "$root/usr/lib/systemd/user/playdropd.service"
install -Dm644 "$linux/org.playdrop.PlayDrop.metainfo.xml" \
    "$root/usr/share/metainfo/org.playdrop.PlayDrop.metainfo.xml"
install -Dm644 "$linux/icons/hicolor/scalable/apps/org.playdrop.PlayDrop.svg" \
    "$root/usr/share/icons/hicolor/scalable/apps/org.playdrop.PlayDrop.svg"
install -Dm644 "$linux/icons/hicolor/symbolic/apps/org.playdrop.PlayDrop-symbolic.svg" \
    "$root/usr/share/icons/hicolor/symbolic/apps/org.playdrop.PlayDrop-symbolic.svg"

# Optional ufw application profile. Passive: it changes no rule until the user
# runs `sudo ufw allow PlayDrop`.
install -Dm644 "$here/debian/ufw-playdrop" "$root/etc/ufw/applications.d/playdrop"

# Debian policy: copyright file, licence text referenced from common-licenses.
install -Dm644 "$here/debian/copyright" "$root/usr/share/doc/playdrop/copyright"

# Control area.
install -d "$root/DEBIAN"
installed_size="$(du -sk --exclude=DEBIAN "$root" | cut -f1)"
sed -e "s/@VERSION@/$version/" -e "s/@ARCH@/$arch/" -e "s/@INSTALLED_SIZE@/$installed_size/" \
    "$here/debian/control.in" > "$root/DEBIAN/control"
install -m755 "$here/debian/postinst" "$root/DEBIAN/postinst"
install -m755 "$here/debian/postrm"   "$root/DEBIAN/postrm"
install -m644 "$here/debian/conffiles" "$root/DEBIAN/conffiles"

mkdir -p "$dist"
out="$dist/playdrop_${version}_${arch}.deb"
dpkg-deb --build --root-owner-group "$root" "$out"

if command -v lintian >/dev/null; then
    lintian --no-tag-display-limit "$out" || true
fi

echo "built $out"

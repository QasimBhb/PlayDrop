#!/usr/bin/env bash
# Build the PlayDrop .deb for Pop!_OS 24.04 and Ubuntu 24.04: the daemon, the window, the
# command line and the COSMIC panel applet, in one package.
#
# Usage: packaging/deb/build-deb.sh [--no-build] [--package-version <version>]
#   --no-build                    reuse the release binaries already built instead of
#                                 running cargo
#   --package-version <version>   write this version into the package instead of the
#                                 workspace's, to test an upgrade locally; the programs
#                                 inside still report the workspace version. Never use it
#                                 for a release.
#
# Environment:
#   CARGO_TARGET_DIR             where the main workspace builds (default: target/)
#   PLAYDROP_APPLET_TARGET_DIR   where apps/cosmic-applet builds (default:
#                                apps/cosmic-applet/target/). The applet is its own cargo
#                                workspace with its own Cargo.lock and target folder.
#
# Output: packaging/deb/dist/playdrop_<version>_<arch>.deb
#
# This is a plain dpkg-deb build rather than cargo-deb because cargo-deb reads
# its metadata only from [package.metadata.deb] in Cargo.toml, and packaging is
# kept out of the crate manifests. build-transitional-deb.sh builds the empty
# playdrop-cosmic-applet package that moves 0.11.0 applet installs over to this one.
set -euo pipefail
umask 022

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
linux="$repo/packaging/linux"
applet="$repo/apps/cosmic-applet"
dist="$here/dist"

# The first version in which this package carries the applet. The applet's old package is
# replaced by this one below that version (Replaces in debian/control.in); see README.md,
# "The COSMIC applet". Keep it in step with build-transitional-deb.sh.
first_merged=0.12.0

build=1
version_override=
while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) build=0 ;;
        --package-version)
            [ $# -ge 2 ] || { echo "--package-version needs a version" >&2; exit 2; }
            version_override="$2"
            shift
            ;;
        -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

for tool in dpkg-deb dpkg readelf; do
    command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 1; }
done

# Version comes from [workspace.package] in the root Cargo.toml. One version number covers
# every part of PlayDrop; the applet has its own manifest, so refuse to package it when that
# manifest was not bumped with the workspace.
version="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$repo/Cargo.toml" | head -n1)"
[ -n "$version" ] || { echo "could not read the workspace version" >&2; exit 1; }
applet_version="$(sed -n '/^\[package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$applet/Cargo.toml" | head -n1)"
if [ "$applet_version" != "$version" ]; then
    echo "apps/cosmic-applet is version '$applet_version' but the workspace is '$version';" >&2
    echo "set the applet's version and the three version lines in its Cargo.lock" >&2
    exit 1
fi
if [ -n "$version_override" ]; then
    version="$version_override"
    echo "packaging as version $version (override; the programs report the workspace version)"
fi
arch="$(dpkg --print-architecture)"

# Where cargo puts the binaries: CARGO_TARGET_DIR when set (this repo sits on a drive where
# linking under target/ stalls, so local builds point it at ext4), otherwise target/. The
# applet's own target folder is separate, so its locked libcosmic tree never mixes with
# the main one.
bindir="${CARGO_TARGET_DIR:-$repo/target}/release"
applet_bindir="${PLAYDROP_APPLET_TARGET_DIR:-$applet/target}/release"

if [ "$build" = 1 ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
    cargo build --release --locked --manifest-path "$repo/Cargo.toml" \
        -p playdropd -p playdrop-desktop -p playdrop-cli
    CARGO_TARGET_DIR="${PLAYDROP_APPLET_TARGET_DIR:-$applet/target}" \
        cargo build --release --locked --manifest-path "$applet/Cargo.toml"
fi

# playdrop is the command line from the playdrop-cli crate (docs/guide/cli.md).
for bin in playdropd playdrop-desktop playdrop; do
    [ -x "$bindir/$bin" ] || { echo "missing $bindir/$bin" >&2; exit 1; }
done
[ -x "$applet_bindir/playdrop-cosmic-applet" ] \
    || { echo "missing $applet_bindir/playdrop-cosmic-applet" >&2; exit 1; }

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
root="$stage/playdrop_${version}_${arch}"

# Binaries, stripped to keep the package small.
install -Dm755 "$bindir/playdropd"        "$root/usr/bin/playdropd"
install -Dm755 "$bindir/playdrop-desktop" "$root/usr/bin/playdrop-desktop"
install -Dm755 "$bindir/playdrop"         "$root/usr/bin/playdrop"
install -Dm755 "$applet_bindir/playdrop-cosmic-applet" "$root/usr/bin/playdrop-cosmic-applet"
if command -v strip >/dev/null; then
    strip --strip-unneeded "$root/usr/bin/playdropd" "$root/usr/bin/playdrop-desktop" \
        "$root/usr/bin/playdrop" "$root/usr/bin/playdrop-cosmic-applet"
fi

# Every shared library the programs load must come from a package named in Depends of
# debian/control.in. The list is fixed there rather than worked out per build, so a new
# library has to be added on purpose: this check stops the build when one appears.
#   soname            package (Ubuntu 24.04)
allowed_libs="
    libc.so.6         libc6
    libm.so.6         libc6
    libgcc_s.so.1     libgcc-s1
    libfontconfig.so.1 libfontconfig1
    libxkbcommon.so.0 libxkbcommon0
    ld-linux-x86-64.so.2 libc6
"
unknown=
for bin in "$root"/usr/bin/*; do
    for lib in $(readelf -d "$bin" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do
        if ! printf '%s\n' "$allowed_libs" | awk '{print $1}' | grep -qxF "$lib"; then
            unknown="$unknown ${bin##*/}:$lib"
        fi
    done
done
if [ -n "$unknown" ]; then
    echo "linked libraries not covered by Depends:$unknown" >&2
    echo "add their packages to debian/control.in and to the list in this script" >&2
    exit 1
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

# The COSMIC panel applet. COSMIC Settings lists every desktop file with X-CosmicApplet=true
# as an applet to add; the entry has NoDisplay=true, so no launcher shows it, and nothing
# autostarts it. Only cosmic-panel runs it, once the user adds it to a panel.
install -Dm644 "$applet/data/org.playdrop.PlayDrop.CosmicApplet.desktop" \
    "$root/usr/share/applications/org.playdrop.PlayDrop.CosmicApplet.desktop"

# Optional ufw application profile. Passive: it changes no rule until the user
# runs `sudo ufw allow PlayDrop`.
install -Dm644 "$here/debian/ufw-playdrop" "$root/etc/ufw/applications.d/playdrop"

# Debian policy: copyright file, licence text referenced from common-licenses.
install -Dm644 "$here/debian/copyright" "$root/usr/share/doc/playdrop/copyright"

# Control area.
install -d "$root/DEBIAN"
installed_size="$(du -sk --exclude=DEBIAN "$root" | cut -f1)"
sed -e "s/@VERSION@/$version/" -e "s/@ARCH@/$arch/" -e "s/@INSTALLED_SIZE@/$installed_size/" \
    -e "s/@FIRST_MERGED@/$first_merged/" \
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

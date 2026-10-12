#!/usr/bin/env bash
# Builds PlayDrop for iPhone on Linux and installs it on the connected phone with xtool
# (README "Building on Linux with xtool"). One command for the whole chain:
#   1. the Rust core and its Swift bindings (scripts/build-core.sh), when missing or asked
#   2. the Info.plists and entitlements xtool reads (scripts/xtool-prepare.py)
#   3. `xtool dev`, which builds the Swift package, signs the app and installs it
#
# Usage: scripts/xtool-dev.sh [--core] [--debug-core] [xtool dev options...]
#   --core          rebuild the Rust core even if it is already there (after a change under
#                   crates/)
#   --debug-core    build the Rust core in debug (faster to compile, slow transfers)
#   anything else   passed to `xtool dev`, for example --configuration release
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ios_root="$(cd "$here/.." && pwd)"
core_pkg="$ios_root/Packages/PlayDropCore"

die() { printf 'xtool-dev: %s\n' "$*" >&2; exit 1; }

rebuild_core=0
core_flags=()
xtool_args=()
for arg in "$@"; do
    case "$arg" in
        --core) rebuild_core=1 ;;
        --debug-core) rebuild_core=1; core_flags+=(--debug) ;;
        -h|--help) sed -n '2,13p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) xtool_args+=("$arg") ;;
    esac
done

[ "$(uname -s)" = Linux ] || die "this wrapper is for Linux; on a Mac use Xcode (README \"Building on a Mac\")."
command -v xtool >/dev/null 2>&1 || die "xtool not found: install it from https://xtool.sh and run 'xtool setup'."
command -v python3 >/dev/null 2>&1 || die "python3 not found: it is needed to prepare the plists."

if [ "$rebuild_core" -eq 1 ] \
    || [ ! -d "$core_pkg/PlayDropCoreFFI.xcframework/ios-arm64" ] \
    || [ ! -f "$core_pkg/Sources/PlayDropCore/PlayDropCore.swift" ]; then
    "$here/build-core.sh" --device-only ${core_flags[@]+"${core_flags[@]}"}
else
    echo "xtool-dev: Rust core already built (pass --core to rebuild it after a change under crates/)"
fi

python3 "$here/xtool-prepare.py"

cd "$ios_root"
exec xtool dev ${xtool_args[@]+"${xtool_args[@]}"}

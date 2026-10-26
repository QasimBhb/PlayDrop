#!/usr/bin/env bash
# Regenerate packaging/flatpak/cargo-sources.json from Cargo.lock.
#
# The Flatpak build runs cargo offline, so every crate in Cargo.lock has to be listed as a
# download in cargo-sources.json. Run this after every change to Cargo.lock and commit the
# result together with the lock file (Flathub keeps the generated file in the repo too).
#
# Usage: packaging/flatpak/update-sources.sh
#
# Needs git and python3. The generator (flatpak-builder-tools' flatpak-cargo-generator.py)
# is cloned into ~/.cache/playdrop/flatpak-builder-tools, never into the repo. Its two Python
# packages, aiohttp and tomlkit, come from `uv` when it is installed (nothing is installed
# system-wide); otherwise python3 must already have them.
# Set FLATPAK_CARGO_GENERATOR=/path/to/flatpak-cargo-generator.py to use your own copy.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$here/cargo-sources.json"

generator="${FLATPAK_CARGO_GENERATOR:-}"
if [ -z "$generator" ]; then
    tools="${XDG_CACHE_HOME:-$HOME/.cache}/playdrop/flatpak-builder-tools"
    if [ -d "$tools/.git" ]; then
        git -C "$tools" pull --quiet --ff-only
    else
        mkdir -p "$(dirname "$tools")"
        git clone --quiet --depth 1 https://github.com/flatpak/flatpak-builder-tools "$tools"
    fi
    generator="$tools/cargo/flatpak-cargo-generator.py"
fi
[ -f "$generator" ] || { echo "generator not found: $generator" >&2; exit 1; }

# -I keeps Python from importing anything from the generator's own folder or the cwd.
sdk="org.freedesktop.Sdk//25.08"
if command -v uv >/dev/null; then
    uv run --quiet --no-project --with aiohttp --with tomlkit -- \
        python3 -I "$generator" "$repo/Cargo.lock" -o "$out"
elif python3 -c 'import aiohttp, tomlkit' 2>/dev/null; then
    python3 -I "$generator" "$repo/Cargo.lock" -o "$out"
elif command -v flatpak >/dev/null && flatpak info --user "$sdk" >/dev/null 2>&1; then
    # No uv and no modules on the host (Pop!_OS ships python3 without pip or venv): the
    # Freedesktop SDK the Flatpak build itself needs has a Python with pip. The two
    # packages go into a throwaway folder, nothing is installed on the host.
    work="$(mktemp -d "${TMPDIR:-/tmp}/playdrop-cargo-sources.XXXXXX")"
    trap 'rm -rf "$work"' EXIT
    cp "$generator" "$work/generator.py"
    cp "$repo/Cargo.lock" "$work/Cargo.lock"
    flatpak run --share=network --filesystem="$work" --command=sh "$sdk" -c \
        "python3 -m pip install --quiet --target '$work/lib' aiohttp tomlkit \
         && cd '$work' && PYTHONPATH='$work/lib' python3 generator.py Cargo.lock -o cargo-sources.json"
    cp "$work/cargo-sources.json" "$out"
else
    echo "python3 lacks aiohttp or tomlkit; install uv (https://docs.astral.sh/uv/)," >&2
    echo "run: python3 -m pip install --user aiohttp tomlkit, or install the Flatpak SDK" >&2
    echo "($sdk, see README.md) and run this script again" >&2
    exit 1
fi

echo "wrote $out ($(grep -c '"type": "archive"' "$out") crates)"

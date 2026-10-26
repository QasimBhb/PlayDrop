#!/usr/bin/env python3
"""Checks that cargo-sources.json lists every crate in Cargo.lock.

The Flatpak build runs cargo offline from the vendored list in cargo-sources.json, so a
crate added to Cargo.lock without regenerating that file breaks the release build only at
release time (v0.3.0 failed this way on 2026-10-17, one crate short). This check runs in CI
on every push that touches Cargo.lock and at the start of the Flatpak release job, so the
gap is found at once.

Usage: packaging/flatpak/check-sources.py
Exit codes: 0 in step, 1 crates missing or extra (listed on stderr), 2 a file could not be
read. Standard library only (Python 3.11 or newer, for tomllib).

To fix a failure: packaging/flatpak/update-sources.sh, then commit the result.
"""
import json
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
LOCK = REPO / "Cargo.lock"
SOURCES = HERE / "cargo-sources.json"
VENDOR = "cargo/vendor/"


def fail(message: str, code: int = 2) -> None:
    print(f"check-sources: {message}", file=sys.stderr)
    sys.exit(code)


def locked_crates() -> set[str]:
    """Every registry crate in Cargo.lock as `name-version`; workspace crates have no source."""
    try:
        lock = tomllib.loads(LOCK.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as err:
        fail(f"cannot read {LOCK}: {err}")
    crates = set()
    for package in lock.get("package", []):
        source = package.get("source", "")
        if source.startswith("registry+"):
            crates.add(f"{package['name']}-{package['version']}")
        elif source:
            fail(f"{package['name']} comes from {source}; this check only knows registry crates")
    return crates


def vendored_crates() -> set[str]:
    """Every archive the generated file unpacks into cargo/vendor/<name>-<version>."""
    try:
        entries = json.loads(SOURCES.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as err:
        fail(f"cannot read {SOURCES}: {err}")
    crates = set()
    for entry in entries:
        dest = entry.get("dest", "")
        if entry.get("type") == "archive" and dest.startswith(VENDOR):
            crates.add(dest[len(VENDOR):])
    return crates


def main() -> None:
    locked = locked_crates()
    vendored = vendored_crates()
    missing = sorted(locked - vendored)
    extra = sorted(vendored - locked)
    if not missing and not extra:
        print(f"check-sources: {len(locked)} crates in step")
        return
    for crate in missing:
        print(f"check-sources: missing from cargo-sources.json: {crate}", file=sys.stderr)
    for crate in extra:
        print(f"check-sources: not in Cargo.lock any more: {crate}", file=sys.stderr)
    fail("run packaging/flatpak/update-sources.sh and commit cargo-sources.json", code=1)


if __name__ == "__main__":
    main()

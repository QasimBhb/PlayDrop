#!/usr/bin/env python3
"""Keeps MARKETING_VERSION in apps/ios/project.yml equal to the workspace version.

The workspace version is `version` under [workspace.package] in the root Cargo.toml. That one
number is the version of every PlayDrop artifact, so project.yml must not be edited by hand.

Usage:
  sync-version.py           rewrite the MARKETING_VERSION line when it differs (idempotent)
  sync-version.py --check   change nothing; exit 1 when the two differ

Exit codes: 0 in step (or fixed), 1 differ (--check only), 2 a file or line could not be read.
Standard library only.
"""
import argparse
import re
import sys
from pathlib import Path

IOS_ROOT = Path(__file__).resolve().parent.parent
REPO_ROOT = IOS_ROOT.parent.parent
CARGO_TOML = REPO_ROOT / "Cargo.toml"
PROJECT_YML = IOS_ROOT / "project.yml"

SECTION = re.compile(r"^\[(.+?)\]\s*$")
VERSION = re.compile(r'^version\s*=\s*"([^"]+)"')
MARKETING = re.compile(r"^(\s*MARKETING_VERSION:\s*)(\S+)(\s*)$")


def fail(message: str) -> None:
    print(f"sync-version: {message}", file=sys.stderr)
    sys.exit(2)


def workspace_version() -> str:
    try:
        lines = CARGO_TOML.read_text(encoding="utf-8").splitlines()
    except OSError as err:
        fail(f"cannot read {CARGO_TOML}: {err}")
    in_section = False
    for line in lines:
        section = SECTION.match(line)
        if section:
            in_section = section.group(1) == "workspace.package"
            continue
        if in_section:
            found = VERSION.match(line)
            if found:
                return found.group(1)
    fail(f"no version under [workspace.package] in {CARGO_TOML}")
    return ""  # unreachable


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--check", action="store_true", help="report a difference instead of fixing it")
    args = parser.parse_args()

    version = workspace_version()
    try:
        text = PROJECT_YML.read_text(encoding="utf-8")
    except OSError as err:
        fail(f"cannot read {PROJECT_YML}: {err}")

    lines = text.splitlines(keepends=True)
    hits = [i for i, line in enumerate(lines) if MARKETING.match(line.rstrip("\r\n"))]
    if len(hits) != 1:
        fail(f"expected exactly one MARKETING_VERSION line in {PROJECT_YML}, found {len(hits)}")

    index = hits[0]
    ending = lines[index][len(lines[index].rstrip("\r\n")):]
    match = MARKETING.match(lines[index].rstrip("\r\n"))
    current = match.group(2)

    if current == version:
        print(f"sync-version: project.yml is at {version}, in step with Cargo.toml")
        return 0
    if args.check:
        print(
            f"sync-version: project.yml has MARKETING_VERSION {current} but Cargo.toml has {version}; "
            "run apps/ios/scripts/sync-version.py",
            file=sys.stderr,
        )
        return 1

    lines[index] = f"{match.group(1)}{version}{match.group(3)}{ending}"
    PROJECT_YML.write_text("".join(lines), encoding="utf-8")
    print(f"sync-version: project.yml MARKETING_VERSION {current} -> {version}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Writes the files xtool.yml points at under build/xtool, from the Xcode sources.

Xcode expands $(SETTING) references in Info.plist and the entitlements at build time;
xtool copies the files as they are. This script does the expansion with the values
project.yml gives those settings, so the plists stay single-source. It also drops the
entitlements xtool cannot provision (README "Building on Linux with xtool"):

  com.apple.developer.networking.multicast  granted by Apple per app on request and not
                                            part of xtool's capability list; left in the
                                            signature, a profile without it makes iOS
                                            refuse the install. Discovery then falls back
                                            to address and QR pairing, as documented.

Everything else (App Group, keychain group) is handed to xtool, which rewrites the
identifiers for the signed-in team (and silently drops the App Group on a free team; the
app and the extension cope, see EmbeddedProvisioningProfile).

Usage: scripts/xtool-prepare.py    (from anywhere; paths are resolved from the script)
"""
from __future__ import annotations

import plistlib
import re
import sys
from pathlib import Path

IOS_ROOT = Path(__file__).resolve().parent.parent
OUT_DIR = IOS_ROOT / "build" / "xtool"
PROJECT_YML = IOS_ROOT / "project.yml"

# Entitlement keys removed from both bundles before signing with xtool.
UNPROVISIONABLE_ENTITLEMENTS = {"com.apple.developer.networking.multicast"}

# What Xcode would substitute, per bundle. Versions come from project.yml.
APP = {
    "source": IOS_ROOT / "PlayDrop" / "Resources" / "Info.plist",
    "entitlements": IOS_ROOT / "PlayDrop" / "Resources" / "PlayDrop.entitlements",
    "out": "PlayDrop",
    "settings": {
        "DEVELOPMENT_LANGUAGE": "en",
        "EXECUTABLE_NAME": "PlayDrop",
        "PRODUCT_BUNDLE_IDENTIFIER": "org.playdrop.ios",
        "PRODUCT_NAME": "PlayDrop",
        "PRODUCT_MODULE_NAME": "PlayDrop",
        "PRODUCT_BUNDLE_PACKAGE_TYPE": "APPL",
    },
}
SHARE = {
    "source": IOS_ROOT / "PlayDropShare" / "Info.plist",
    "entitlements": IOS_ROOT / "PlayDropShare" / "PlayDropShare.entitlements",
    "out": "PlayDropShare",
    "settings": {
        "DEVELOPMENT_LANGUAGE": "en",
        "EXECUTABLE_NAME": "PlayDropShare",
        "PRODUCT_BUNDLE_IDENTIFIER": "org.playdrop.ios.share",
        "PRODUCT_NAME": "PlayDropShare",
        "PRODUCT_MODULE_NAME": "PlayDropShare",
        "PRODUCT_BUNDLE_PACKAGE_TYPE": "XPC!",
    },
}

SETTING = re.compile(r"\$\(([A-Za-z_][A-Za-z0-9_]*)\)")


def fail(message: str) -> None:
    print(f"xtool-prepare: {message}", file=sys.stderr)
    sys.exit(1)


def versions_from_project_yml() -> dict[str, str]:
    """MARKETING_VERSION and CURRENT_PROJECT_VERSION as project.yml sets them."""
    text = PROJECT_YML.read_text()
    found = {}
    for key in ("MARKETING_VERSION", "CURRENT_PROJECT_VERSION"):
        m = re.search(rf"^\s*{key}:\s*([^\s#]+)", text, re.MULTILINE)
        if not m:
            fail(f"{key} not found in {PROJECT_YML}")
        found[key] = m.group(1).strip("\"'")
    return found


def expand(value, settings: dict[str, str]):
    """Replaces $(NAME) in every string of a plist value; unknown names are an error."""
    if isinstance(value, str):
        def sub(m: re.Match) -> str:
            name = m.group(1)
            if name not in settings:
                fail(f"no value for $({name}); add it to the settings in this script")
            return settings[name]
        return SETTING.sub(sub, value)
    if isinstance(value, list):
        return [expand(v, settings) for v in value]
    if isinstance(value, dict):
        return {k: expand(v, settings) for k, v in value.items()}
    return value


def write_plist(path: Path, value) -> None:
    with path.open("wb") as f:
        plistlib.dump(value, f, sort_keys=False)


def prepare(bundle: dict, versions: dict[str, str]) -> None:
    settings = {**bundle["settings"], **versions}

    with bundle["source"].open("rb") as f:
        info = plistlib.load(f)
    write_plist(OUT_DIR / f"{bundle['out']}-Info.plist", expand(info, settings))

    with bundle["entitlements"].open("rb") as f:
        entitlements = plistlib.load(f)
    for key in UNPROVISIONABLE_ENTITLEMENTS:
        entitlements.pop(key, None)
    # $(AppIdentifierPrefix) in keychain-access-groups: xtool replaces the whole list with
    # <team id>.<bundle id>, so the placeholder only has to go.
    entitlements = expand(entitlements, {**settings, "AppIdentifierPrefix": ""})
    write_plist(OUT_DIR / f"{bundle['out']}.entitlements", entitlements)


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    versions = versions_from_project_yml()
    for bundle in (APP, SHARE):
        prepare(bundle, versions)
    print(f"xtool-prepare: wrote {OUT_DIR.relative_to(IOS_ROOT)}/ "
          f"(version {versions['MARKETING_VERSION']} build {versions['CURRENT_PROJECT_VERSION']})")


if __name__ == "__main__":
    main()

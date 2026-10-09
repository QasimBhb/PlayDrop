#!/usr/bin/env bash
# What a Linux machine can verify about the iOS app without Xcode:
#   1. every Swift file parses (`swiftc -parse`; imports are not resolved, so UIKit and
#      SwiftUI code is syntax-checked only),
#   2. the PlayDropShared package builds and its tests pass (`swift build`, `swift test`:
#      Foundation only, so it runs on Linux),
#   3. the Rust core type-checks and is clippy-clean for aarch64-apple-ios,
#   4. every plist, entitlements file and privacy manifest parses, the asset and string
#      catalogs are valid JSON, and project.yml is valid YAML (python3; PyYAML if installed),
#   5. the app, extension and test code type-checks against the real generated bindings
#      and stub Apple frameworks, and the SwiftUI files only use symbols that exist
#      (scripts/typecheck-linux.sh, which says exactly what it covers).
# Needs a Swift toolchain on PATH or in $SWIFT_TOOLCHAIN (its `usr/bin` is used), and the
# Rust targets `aarch64-apple-ios` and `aarch64-apple-ios-sim`. The Rust step fakes the
# Apple SDK with stub `cc` and `ar` scripts, so it never links; that is the Mac's job.
#
# Usage: apps/ios/scripts/check-linux.sh [--swift-only|--rust-only|--resources-only|--typecheck-only] [paths...]
#        apps/ios/scripts/check-linux.sh --list    print what would be checked, run nothing
# With paths, only those Swift files or directories are parsed (step 1) and the other steps
# are skipped, except step 2 when a path is inside Packages/PlayDropShared.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ios_root="$(cd "$here/.." && pwd)"
repo_root="$(cd "$ios_root/../.." && pwd)"

mode=all
paths=()
for a in "$@"; do
    case "$a" in
        --swift-only) mode=swift ;;
        --rust-only) mode=rust ;;
        --resources-only) mode=resources ;;
        --typecheck-only) mode=typecheck ;;
        --list) mode=list ;;
        -h|--help) sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) paths+=("$a") ;;
    esac
done

if [ -n "${SWIFT_TOOLCHAIN:-}" ]; then
    export PATH="$SWIFT_TOOLCHAIN/usr/bin:$PATH"
fi

# Prints, NUL-separated, every Swift file under the given paths (default: the whole app),
# skipping build output and the generated bindings.
swift_files() {
    local targets=("$@")
    if [ ${#targets[@]} -eq 0 ]; then
        targets=("$ios_root")
    fi
    find "${targets[@]}" -name '*.swift' \
        -not -path '*/.build/*' -not -path '*/build/*' -not -path '*/DerivedData/*' \
        -not -name 'PlayDropCore.swift' -print0 | sort -z
}

# Prints, NUL-separated, every resource file step 4 validates.
resource_files() {
    find "$ios_root" \( -name '*.plist' -o -name '*.entitlements' -o -name '*.xcprivacy' \
        -o -name 'Contents.json' -o -name '*.xcstrings' -o -name 'project.yml' \) \
        -not -path '*/.build/*' -not -path '*/build/*' -not -path '*/DerivedData/*' \
        -not -path '*.xcodeproj/*' -not -path '*.xcframework/*' -print0 | sort -z
}

parse_swift() {
    local files=()
    while IFS= read -r -d '' f; do files+=("$f"); done < <(swift_files "$@")
    local targets=("$@")
    if [ ${#files[@]} -eq 0 ]; then
        echo "check-linux: no Swift files under ${targets[*]:-$ios_root}"
        return 0
    fi
    local bad=0
    for f in "${files[@]}"; do
        if ! out="$(swiftc -parse "$f" 2>&1)"; then
            echo "$out"
            bad=$((bad + 1))
        fi
    done
    echo "check-linux: parsed ${#files[@]} Swift files, $bad with errors"
    [ "$bad" -eq 0 ]
}

shared_tests() {
    (cd "$ios_root/Packages/PlayDropShared" && swift build 2>&1 | tail -n 20 && swift test 2>&1 | tail -n 30)
}

# Runs in a subshell so the fake SDKROOT and the stub compilers do not leak into the Swift
# steps that follow (swiftc would take the fake SDK as its sysroot).
rust_check() (
    local stubs="$ios_root/scripts/stubs"
    export SDKROOT="${SDKROOT:-$stubs/fake-sdk}"
    mkdir -p "$SDKROOT"
    export CC_aarch64_apple_ios="$stubs/stub-cc" AR_aarch64_apple_ios="$stubs/stub-ar"
    export CC_aarch64_apple_ios_sim="$stubs/stub-cc" AR_aarch64_apple_ios_sim="$stubs/stub-ar"
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_root/target/ios-check}"
    (cd "$repo_root" && cargo clippy -p playdrop-ffi --target aarch64-apple-ios -- -D warnings \
        && cargo clippy -p playdrop-ffi --target aarch64-apple-ios-sim -- -D warnings)
)

# Validates the plists (plistlib), the JSON catalogs and project.yml. Apple's plutil is not
# on Linux; Python's parser rejects the same malformed XML.
check_resources() {
    local files=()
    while IFS= read -r -d '' f; do files+=("$f"); done < <(resource_files)
    python3 -I - "${files[@]}" <<'PY'
import json, plistlib, sys
try:
    import yaml
except ImportError:
    yaml = None
bad = 0
for path in sys.argv[1:]:
    try:
        if path.endswith((".plist", ".entitlements", ".xcprivacy")):
            with open(path, "rb") as fh:
                plistlib.load(fh)
        elif path.endswith((".json", ".xcstrings")):
            with open(path, encoding="utf-8") as fh:
                json.load(fh)
        elif path.endswith(".yml"):
            if yaml is None:
                print(f"skipped (no PyYAML): {path}")
                continue
            with open(path, encoding="utf-8") as fh:
                yaml.safe_load(fh)
    except Exception as exc:  # report every broken file, not just the first
        print(f"{path}: {exc}")
        bad += 1
print(f"check-linux: validated {len(sys.argv) - 1} resource files, {bad} with errors")
sys.exit(1 if bad else 0)
PY
}

# --list: what each step would look at, without running anything.
list_checks() {
    local f
    echo "1. swiftc -parse:"
    while IFS= read -r -d '' f; do echo "   ${f#"$ios_root"/}"; done < <(swift_files ${paths[@]+"${paths[@]}"})
    echo "2. swift build and swift test in Packages/PlayDropShared"
    echo "3. cargo clippy -p playdrop-ffi for aarch64-apple-ios and aarch64-apple-ios-sim (stub cc and ar)"
    echo "4. resource validation:"
    while IFS= read -r -d '' f; do echo "   ${f#"$ios_root"/}"; done < <(resource_files)
    echo "5. scripts/typecheck-linux.sh (bindings, stub frameworks, app, extension, tests, SwiftUI symbols)"
}

# Absolute form of a file or directory path, so a relative path still matches the package.
abs_path() {
    if [ -d "$1" ]; then (cd "$1" && pwd); else echo "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; fi
}

if [ "$mode" = list ]; then
    list_checks
    exit 0
fi

if [ ${#paths[@]} -gt 0 ]; then
    parse_swift "${paths[@]}"
    for p in "${paths[@]}"; do
        case "$(abs_path "$p")" in
            "$ios_root/Packages/PlayDropShared"*) shared_tests; break ;;
        esac
    done
    exit 0
fi

case "$mode" in
    swift) parse_swift; shared_tests ;;
    rust) rust_check ;;
    resources) check_resources ;;
    typecheck) "$here/typecheck-linux.sh" ;;
    all) parse_swift; shared_tests; rust_check; check_resources; "$here/typecheck-linux.sh" ;;
esac

#!/usr/bin/env bash
# Type-checks the iOS app's Swift code on Linux, as far as that is possible without Xcode:
#   1. generates the real UniFFI Swift bindings from a host build of playdrop-ffi and repairs
#      the C header exactly as build-core.sh does (backticked `open` fields, a UniFFI bug),
#   2. builds stub modules for the Apple frameworks the app uses (linux-typecheck/modules:
#      UIKit, AppIntents, AudioToolbox, CoreTransferable, CryptoKit, os, Security,
#      UniformTypeIdentifiers, UserNotifications, Darwin) plus the real PlayDropShared and
#      the real generated PlayDropCore,
#   3. compiles every app and extension file that does not import SwiftUI, and the unit
#      tests, in Swift 5 mode with complete strict concurrency, as the Xcode targets do,
#   4. cross-checks the SwiftUI files against the symbols of step 3 (ui-symbols.py): names,
#      argument labels and private members.
# Stubs only model the API shapes the app uses, with Apple's labels; they can hide a wrong
# assumption about Apple behaviour but not a mismatch between our own files. Errors fail the
# run; warnings fail it too, except the ones Linux alone produces (bridging casts from
# NSSecureCoding that always fail without Objective-C, and the compiler noticing that the
# stub `os` module lacks Apple's log-section constant).
#
# Needs a Swift 6 toolchain (PATH or $SWIFT_TOOLCHAIN), cargo, and python3.
# Output goes to $CARGO_TARGET_DIR (default <repo>/target)/ios-typecheck.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ios_root="$(cd "$here/.." && pwd)"
repo_root="$(cd "$ios_root/../.." && pwd)"
kit="$here/linux-typecheck"
if [ -n "${SWIFT_TOOLCHAIN:-}" ]; then
    export PATH="$SWIFT_TOOLCHAIN/usr/bin:$PATH"
fi
export PATH="$HOME/.cargo/bin:$PATH"
# A leftover SDKROOT (check-linux.sh fakes one for the iOS Rust check) would become swiftc's
# sysroot and hide the host C library; this script only ever targets the Linux host.
unset SDKROOT
target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
out="$target_dir/ios-typecheck"
mods="$out/modules"
gen="$out/generated"
sg="$out/symbols"
rm -rf "$out"
mkdir -p "$mods" "$gen/ffi" "$sg"

step() { printf '\n==> %s\n' "$*"; }
die() { printf 'typecheck-linux: %s\n' "$*" >&2; exit 1; }
failed=0

# Runs swiftc and prints its diagnostics (one line each) without Linux-only noise; marks the
# run failed when an error or a real warning remains.
compile() {
    local log="$1"; shift
    swiftc "$@" > "$log.raw" 2>&1 || true
    grep -E '^[^ ].*: (error|warning): ' "$log.raw" \
        | grep -v "to unrelated type .* always fails" \
        | grep -v "is missing from the 'os' module" \
        | sed "s|$ios_root/||" | sort -u > "$log" || true
    cat "$log"
    if [ -s "$log" ] || grep -q ': error: ' "$log.raw"; then
        failed=1
    fi
}

# ---- 1. Bindings -------------------------------------------------------------------------
step "Generating the Swift bindings (host build of playdrop-ffi)"
(cd "$repo_root" && cargo build -q -p playdrop-ffi)
host_lib="$target_dir/debug/libplaydrop_ffi.so"
[ -f "$host_lib" ] || die "expected $host_lib after the host build"
(cd "$repo_root" && cargo run -q -p uniffi-bindgen -- generate --library "$host_lib" \
    --language swift --no-format --out-dir "$gen")
header="$gen/PlayDropCoreFFI.h"
# Same repair as build-core.sh step 5b, with GNU sed's -i (no suffix argument).
sed -i '/^[[:space:]]*\/\//!s/`\([A-Za-z_][A-Za-z0-9_]*\)`/\1/g' "$header"
if grep -n '`' "$header" | grep -v '^[0-9]*:[[:space:]]*//'; then
    die "PlayDropCoreFFI.h still has backticks outside comments; update build-core.sh step 5b"
fi
cp "$header" "$gen/ffi/"
printf 'module PlayDropCoreFFI {\n    header "PlayDropCoreFFI.h"\n    export *\n}\n' > "$gen/ffi/module.modulemap"
echo "bindings: $(wc -l < "$gen/PlayDropCore.swift") lines, header repaired"

# ---- 2. Modules --------------------------------------------------------------------------
step "Building stub framework modules, PlayDropShared and PlayDropCore"
emit() {
    local name="$1"; shift
    compile "$out/$name.log" -emit-module -parse-as-library -module-name "$name" \
        -emit-module-path "$mods/$name.swiftmodule" -I "$mods" "$@"
}
for name in os Darwin Security CryptoKit AudioToolbox UniformTypeIdentifiers UserNotifications UIKit CoreTransferable AppIntents; do
    emit "$name" -swift-version 5 "$kit/modules/$name.swift"
done
emit PlayDropShared -swift-version 6 "$ios_root"/Packages/PlayDropShared/Sources/PlayDropShared/*.swift
# The generated file escapes a parameter named `open`, which Swift 6.x warns about; that is
# UniFFI's output, not ours, so its warnings are not counted.
swiftc -emit-module -parse-as-library -module-name PlayDropCore -swift-version 5 \
    -emit-module-path "$mods/PlayDropCore.swiftmodule" -I "$mods" -I "$gen/ffi" \
    -emit-symbol-graph -emit-symbol-graph-dir "$sg" \
    "$gen/PlayDropCore.swift" 2>&1 | grep -E ': error: ' && failed=1 || true
swift-symbolgraph-extract -module-name PlayDropShared -I "$mods" -target "$(uname -m)-unknown-linux-gnu" \
    -output-dir "$sg" -minimum-access-level public

# ---- 3. App, extension and tests -----------------------------------------------------------
# Files that import SwiftUI cannot be compiled here (step 4 covers them).
non_swiftui() {
    local f
    for f in "$@"; do
        grep -q '^import SwiftUI' "$f" || printf '%s\0' "$f"
    done
}
strict=(-swift-version 5 -strict-concurrency=complete)

step "App target (non-SwiftUI files)"
mapfile -d '' app_all < <(find "$ios_root/PlayDrop" -name '*.swift' -print0 | sort -z)
mapfile -d '' app < <(non_swiftui "${app_all[@]}")
compile "$out/app.log" -emit-module -module-name PlayDrop -enable-testing "${strict[@]}" \
    -emit-module-path "$out/PlayDrop.swiftmodule" -I "$mods" -I "$gen/ffi" \
    -emit-symbol-graph -emit-symbol-graph-dir "$sg" -symbol-graph-minimum-access-level private \
    "${app[@]}" "$kit"/shims/*.swift
echo "app: ${#app[@]} of ${#app_all[@]} files compiled"

step "Share extension (non-SwiftUI files)"
# AppOpener's responder-chain fallback uses the Objective-C runtime (selectors, IMP casts),
# which Linux Swift lacks: a copy with that one function body cut out is compiled instead.
python3 -I - "$ios_root/PlayDropShare/AppOpener.swift" "$out/AppOpener.swift" <<'PY'
import sys
s = open(sys.argv[1]).read()
sig = "    private static func openThroughResponderChain(_ url: URL, from start: UIResponder) -> Bool {"
i = s.index(sig)
j = s.index("\n    }\n", i)
open(sys.argv[2], "w").write(s[:i] + sig + "\n        false" + s[j:])
PY
mapfile -d '' ext_all < <(find "$ios_root/PlayDropShare" -name '*.swift' -print0 | sort -z)
ext=()
while IFS= read -r -d '' f; do
    case "$f" in */AppOpener.swift) ext+=("$out/AppOpener.swift") ;; *) ext+=("$f") ;; esac
done < <(non_swiftui "${ext_all[@]}")
compile "$out/ext.log" -emit-module -module-name PlayDropShare "${strict[@]}" \
    -emit-module-path "$out/PlayDropShare.swiftmodule" -I "$mods" \
    -emit-symbol-graph -emit-symbol-graph-dir "$sg" -symbol-graph-minimum-access-level private \
    "${ext[@]}" "$kit"/shims/*.swift
echo "extension: ${#ext[@]} of ${#ext_all[@]} files compiled"

step "Unit tests (against the app module)"
mapfile -d '' tests < <(find "$ios_root/PlayDropTests" -name '*.swift' -print0 | sort -z)
compile "$out/tests.log" -emit-module -module-name PlayDropTests "${strict[@]}" \
    -emit-module-path "$out/PlayDropTests.swiftmodule" -I "$mods" -I "$out" -I "$gen/ffi" \
    "${tests[@]}"
echo "tests: ${#tests[@]} files compiled"

# ---- 4. SwiftUI files ----------------------------------------------------------------------
step "SwiftUI files: names and argument labels"
mapfile -t swiftui < <(grep -rl '^import SwiftUI' "$ios_root/PlayDrop" "$ios_root/PlayDropShare" | sort)
python3 -I "$kit/ui-symbols.py" "$sg" "$ios_root" "${swiftui[@]}" || failed=1

if [ "$failed" -ne 0 ]; then
    die "problems found (see above)"
fi
echo
echo "typecheck-linux: clean"

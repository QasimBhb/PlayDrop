#!/usr/bin/env bash
# Builds the Rust core for iOS and produces the two generated inputs of the PlayDropCore
# Swift package, which are never committed:
#   Packages/PlayDropCore/PlayDropCoreFFI.xcframework          libplaydrop_ffi.a for the device
#                                                              and the simulator, plus headers
#   Packages/PlayDropCore/Sources/PlayDropCore/PlayDropCore.swift   the UniFFI Swift bindings
#
# Runs on macOS with Xcode 16 or later and rustup. Run it once after cloning, and again after
# any change under crates/; then `xcodegen generate` (only needed once) and build in Xcode.
#
# Usage: scripts/build-core.sh [--debug] [--sim-only]
#   --debug     debug Rust build (faster to compile, much slower transfers; never ship it)
#   --sim-only  build only the Apple silicon simulator slice, for a quicker inner loop
#               (a device build then fails at link time until you run without this flag)
set -euo pipefail

usage() {
    sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

# ---- Options --------------------------------------------------------------------------
profile=release
sim_only=0
for arg in "$@"; do
    case "$arg" in
        --debug) profile=debug ;;
        --sim-only) sim_only=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "build-core: unknown option '$arg'" >&2; usage >&2; exit 2 ;;
    esac
done

# ---- Paths ----------------------------------------------------------------------------
# Resolved from this script's location so it works from any working directory.
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ios_root="$(cd "$here/.." && pwd)"
repo_root="$(cd "$ios_root/../.." && pwd)"
core_pkg="$ios_root/Packages/PlayDropCore"
bindings_dir="$core_pkg/Sources/PlayDropCore"
xcframework="$core_pkg/PlayDropCoreFFI.xcframework"

# The deployment target must match project.yml, or the linker warns about every object
# built for a newer iOS than the app.
export IPHONEOS_DEPLOYMENT_TARGET=17.0

device_target=aarch64-apple-ios
sim_target=aarch64-apple-ios-sim
ffi_crate=playdrop-ffi
lib_name=libplaydrop_ffi
# The C module UniFFI generates for `module_name = "PlayDropCore"` in uniffi.toml.
ffi_module=PlayDropCoreFFI

# Expanded as ${arr[@]+"${arr[@]}"} below: macOS ships bash 3.2, where "${arr[@]}" of an
# empty array is an "unbound variable" error under `set -u`.
cargo_profile_flag=()
if [ "$profile" = release ]; then
    cargo_profile_flag=(--release)
fi

step() { printf '\n==> %s\n' "$*"; }
die() { printf 'build-core: %s\n' "$*" >&2; exit 1; }

# ---- 1. Check the tools ---------------------------------------------------------------
step "Checking tools"
[ "$(uname -s)" = Darwin ] || die "this script needs macOS with Xcode (it runs xcodebuild)."
export PATH="$HOME/.cargo/bin:$PATH"
command -v rustup >/dev/null 2>&1 || die "rustup not found: install it from https://rustup.rs and run this script again."
command -v cargo >/dev/null 2>&1 || die "cargo not found: install the Rust toolchain with rustup."
command -v xcodebuild >/dev/null 2>&1 || die "xcodebuild not found: install Xcode 16 or later and run 'xcode-select -s /Applications/Xcode.app'."
xcrun --sdk iphoneos --show-sdk-path >/dev/null 2>&1 \
    || die "the iOS SDK is missing: open Xcode once, or run 'xcode-select -s /Applications/Xcode.app'."
echo "rustc $(rustc --version | cut -d' ' -f2), $(xcodebuild -version | head -n 1), profile $profile"

# ---- 2. Add the Rust targets ----------------------------------------------------------
# Apple silicon only: aarch64-apple-ios for phones, aarch64-apple-ios-sim for the simulator
# on Apple silicon Macs. `rustup target add` is a no-op when they are already installed.
step "Adding Rust targets"
targets=("$sim_target")
if [ "$sim_only" -eq 0 ]; then
    targets=("$device_target" "$sim_target")
fi
rustup target add "${targets[@]}"

# ---- 3. Build the static library per target -------------------------------------------
# `cargo rustc --crate-type staticlib` builds only the .a (the crate also lists cdylib and
# rlib), so no iOS dylib is linked for nothing. Output: target/<triple>/<profile>/libplaydrop_ffi.a
cd "$repo_root"
for t in "${targets[@]}"; do
    step "Building $ffi_crate for $t ($profile)"
    cargo rustc -p "$ffi_crate" --lib --target "$t" ${cargo_profile_flag[@]+"${cargo_profile_flag[@]}"} --crate-type staticlib
    [ -f "target/$t/$profile/$lib_name.a" ] || die "expected target/$t/$profile/$lib_name.a after the build."
done

# ---- 4. Build the host library for the bindings generator -----------------------------
# uniffi-bindgen 0.32 in library mode reads the interface from a compiled library; on macOS
# the host cdylib is a .dylib. iOS-specific variables are cleared so this is a plain macOS
# build even when the script runs from inside an Xcode build phase.
step "Building the host library for uniffi-bindgen"
env -u SDKROOT -u IPHONEOS_DEPLOYMENT_TARGET -u PLATFORM_NAME \
    cargo build -p "$ffi_crate" ${cargo_profile_flag[@]+"${cargo_profile_flag[@]}"}
host_lib="target/$profile/$lib_name.dylib"
[ -f "$host_lib" ] || die "expected $host_lib after the host build."

# ---- 5. Generate the Swift bindings ---------------------------------------------------
# Produces PlayDropCore.swift, PlayDropCoreFFI.h and PlayDropCoreFFI.modulemap in a temp dir.
step "Generating Swift bindings"
work="$(mktemp -d "${TMPDIR:-/tmp}/playdrop-core.XXXXXX")"
trap 'rm -rf "$work"' EXIT
gen="$work/generated"
mkdir -p "$gen"
env -u SDKROOT -u IPHONEOS_DEPLOYMENT_TARGET -u PLATFORM_NAME \
    cargo run -q -p uniffi-bindgen -- generate --library "$host_lib" \
    --language swift --no-format --out-dir "$gen"
for f in PlayDropCore.swift "$ffi_module.h" "$ffi_module.modulemap"; do
    [ -f "$gen/$f" ] || die "uniffi-bindgen did not produce $f (check module_name in crates/playdrop-ffi/uniffi.toml)."
done

# ---- 5b. Repair the C header ------------------------------------------------------------
# UniFFI bug (seen in uniffi-bindgen 0.32.2; re-check on every UniFFI upgrade and drop this
# step once the header comes out clean): callback vtable fields named after Swift keywords
# are written with Swift's backtick escaping into the C header too, for example
#     UniffiCallbackInterfaceSecretStoreMethod1 _Nonnull `open`;
# which clang rejects, so `import PlayDropCoreFFI` fails. Backticks are never valid C, so
# they are stripped from identifiers on every line that is not a `//` comment (the header's
# comments quote code in backticks and are left alone). PlayDropCore.swift keeps its
# backticks: there they are valid Swift and required. BSD sed (macOS) needs `-i ''`.
header="$gen/$ffi_module.h"
sed -i '' '/^[[:space:]]*\/\//!s/`\([A-Za-z_][A-Za-z0-9_]*\)`/\1/g' "$header"
if grep -n '`' "$header" | grep -v '^[0-9]*:[[:space:]]*//' >&2; then
    die "$ffi_module.h still has backticks outside comments after the repair (lines above); the UniFFI output changed, update step 5b."
fi

# ---- 6. Move the Swift file into the package ------------------------------------------
mkdir -p "$bindings_dir"
mv -f "$gen/PlayDropCore.swift" "$bindings_dir/PlayDropCore.swift"

# ---- 7. Lay out the headers -----------------------------------------------------------
# The header and the module map go into a subdirectory named after the module. Xcode copies
# every library XCFramework's headers into one shared include directory, so a top-level
# module.modulemap would collide with any other binary package; Clang also looks for
# <search path>/<module>/module.modulemap, so `import PlayDropCoreFFI` still resolves.
headers="$work/headers"
mkdir -p "$headers/$ffi_module"
cp "$gen/$ffi_module.h" "$headers/$ffi_module/$ffi_module.h"
cp "$gen/$ffi_module.modulemap" "$headers/$ffi_module/module.modulemap"

# ---- 8. Create the XCFramework --------------------------------------------------------
# xcodebuild refuses to overwrite, so the old one is removed first.
step "Creating $(basename "$xcframework")"
rm -rf "$xcframework"
xc_args=()
for t in "${targets[@]}"; do
    xc_args+=(-library "target/$t/$profile/$lib_name.a" -headers "$headers")
done
xcodebuild -create-xcframework "${xc_args[@]}" -output "$xcframework"

# ---- 9. Report --------------------------------------------------------------------------
step "Done"
for t in "${targets[@]}"; do
    printf '  %-28s %s\n' "$t" "$(du -h "target/$t/$profile/$lib_name.a" | cut -f1)"
done
printf '  %-28s %s\n' "PlayDropCoreFFI.xcframework" "$(du -sh "$xcframework" | cut -f1)"
printf '  %-28s %s\n' "PlayDropCore.swift" "$(du -h "$bindings_dir/PlayDropCore.swift" | cut -f1)"
if [ "$sim_only" -eq 1 ]; then
    echo "Simulator slice only: run without --sim-only before building for a device."
fi
echo "Next: 'xcodegen generate' (first time), then build the PlayDrop scheme in Xcode."

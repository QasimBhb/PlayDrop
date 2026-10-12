#!/usr/bin/env bash
# Builds the Rust core for iOS and produces the two generated inputs of the PlayDropCore
# Swift package, which are never committed:
#   Packages/PlayDropCore/PlayDropCoreFFI.xcframework          libplaydrop_ffi.a for the device
#                                                              and the simulator, plus headers
#   Packages/PlayDropCore/Sources/PlayDropCore/PlayDropCore.swift   the UniFFI Swift bindings
#
# Runs on macOS with Xcode 16 or later, or on Linux with the Swift toolchain and the iOS SDK
# that `xtool setup` installs (README "Building on Linux with xtool"); rustup on both. Run it
# once after cloning, and again after any change under crates/.
#
# Usage: scripts/build-core.sh [--debug] [--sim-only|--device-only]
#   --debug        debug Rust build (faster to compile, much slower transfers; never ship it)
#   --sim-only     only the Apple silicon simulator slice, for a quicker inner loop on a Mac
#                  (a device build then fails at link time until you run without this flag)
#   --device-only  only the device slice; the default on Linux, which has no simulator
set -euo pipefail

usage() {
    sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

# ---- Options --------------------------------------------------------------------------
profile=release
slices=both
for arg in "$@"; do
    case "$arg" in
        --debug) profile=debug ;;
        --sim-only) slices=sim ;;
        --device-only) slices=device ;;
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

# The deployment target must match project.yml and Package.swift, or the linker warns about
# every object built for a newer iOS than the app.
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
# macOS: Xcode's clang and SDKs through xcrun, as cc-rs expects. Linux: the clang of the
# Swift toolchain and the SDK from the Swift SDK bundle `xtool setup` built; cc-rs takes
# the compiler from CC_<target> and the sysroot from SDKROOT, and rustc needs no linker for
# a static library, so no Apple tool is involved.
step "Checking tools"
export PATH="$HOME/.cargo/bin:$PATH"
command -v rustup >/dev/null 2>&1 || die "rustup not found: install it from https://rustup.rs and run this script again."
command -v cargo >/dev/null 2>&1 || die "cargo not found: install the Rust toolchain with rustup."
host="$(uname -s)"
case "$host" in
    Darwin)
        command -v xcodebuild >/dev/null 2>&1 || die "xcodebuild not found: install Xcode 16 or later and run 'xcode-select -s /Applications/Xcode.app'."
        xcrun --sdk iphoneos --show-sdk-path >/dev/null 2>&1 \
            || die "the iOS SDK is missing: open Xcode once, or run 'xcode-select -s /Applications/Xcode.app'."
        host_lib_ext=dylib
        echo "rustc $(rustc --version | cut -d' ' -f2), $(xcodebuild -version | head -n 1), profile $profile"
        ;;
    Linux)
        [ "$slices" = both ] && slices=device
        [ "$slices" = sim ] && die "there is no iOS simulator on Linux; drop --sim-only."
        command -v swift >/dev/null 2>&1 || die "swift not found: install the Swift 6.4 toolchain (swiftly) and put it on PATH."
        swift sdk list 2>/dev/null | grep -qx darwin || die "the 'darwin' Swift SDK is missing: run 'xtool setup' (README \"Building on Linux with xtool\")."
        sdk_bundle="$HOME/.swiftpm/swift-sdks/darwin.artifactbundle"
        ios_sdk="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["targetTriples"]["arm64-apple-ios"]["sdkRootPath"])' "$sdk_bundle/swift-sdk.json" 2>/dev/null || true)"
        [ -n "$ios_sdk" ] && [ -d "$sdk_bundle/$ios_sdk" ] || die "could not find the iOS SDK in $sdk_bundle/swift-sdk.json; re-run 'xtool setup'."
        ios_sdk="$sdk_bundle/$ios_sdk"
        swift_bin="$(dirname "$(command -v swift)")"
        [ -x "$swift_bin/clang" ] && [ -x "$swift_bin/llvm-ar" ] || die "clang or llvm-ar is missing next to $swift_bin/swift; the Swift toolchain is incomplete."
        # cc-rs reads these for the device target only; the host build below is untouched.
        export "CC_${device_target//-/_}=$swift_bin/clang"
        export "AR_${device_target//-/_}=$swift_bin/llvm-ar"
        export SDKROOT="$ios_sdk"
        host_lib_ext=so
        echo "rustc $(rustc --version | cut -d' ' -f2), $(swift --version 2>/dev/null | head -n 1), SDK $(basename "$ios_sdk"), profile $profile"
        ;;
    *) die "unsupported host '$host': this script runs on macOS or Linux." ;;
esac

# ---- 2. Add the Rust targets ----------------------------------------------------------
# Apple silicon only: aarch64-apple-ios for phones, aarch64-apple-ios-sim for the simulator
# on Apple silicon Macs. `rustup target add` is a no-op when they are already installed.
step "Adding Rust targets"
case "$slices" in
    both) targets=("$device_target" "$sim_target") ;;
    device) targets=("$device_target") ;;
    sim) targets=("$sim_target") ;;
esac
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
# uniffi-bindgen 0.32 in library mode reads the interface from a compiled library: the host
# cdylib (.dylib on macOS, .so on Linux). iOS-specific variables are cleared so this is a
# plain host build even when the script runs from inside an Xcode build phase.
step "Building the host library for uniffi-bindgen"
env -u SDKROOT -u IPHONEOS_DEPLOYMENT_TARGET -u PLATFORM_NAME \
    cargo build -p "$ffi_crate" ${cargo_profile_flag[@]+"${cargo_profile_flag[@]}"}
host_lib="target/$profile/$lib_name.$host_lib_ext"
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
# backticks: there they are valid Swift and required. Written to a new file and moved over
# the original because in-place editing differs between BSD and GNU sed.
header="$gen/$ffi_module.h"
sed '/^[[:space:]]*\/\//!s/`\([A-Za-z_][A-Za-z0-9_]*\)`/\1/g' "$header" > "$header.repaired"
mv -f "$header.repaired" "$header"
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
# Written by hand on both hosts (Linux has no xcodebuild): one directory per slice holding
# the static library and the headers, and an Info.plist listing them, which is exactly what
# `xcodebuild -create-xcframework` produces for a library. The old one is removed first.
step "Creating $(basename "$xcframework")"
rm -rf "$xcframework"
mkdir -p "$xcframework"
slice_dir() {
    case "$1" in
        "$device_target") echo "ios-arm64" ;;
        "$sim_target") echo "ios-arm64-simulator" ;;
    esac
}
{
    cat <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AvailableLibraries</key>
	<array>
EOF
    for t in "${targets[@]}"; do
        slice="$(slice_dir "$t")"
        mkdir -p "$xcframework/$slice"
        cp "target/$t/$profile/$lib_name.a" "$xcframework/$slice/$lib_name.a"
        cp -R "$headers" "$xcframework/$slice/Headers"
        cat <<EOF
		<dict>
			<key>BinaryPath</key>
			<string>$lib_name.a</string>
			<key>HeadersPath</key>
			<string>Headers</string>
			<key>LibraryIdentifier</key>
			<string>$slice</string>
			<key>LibraryPath</key>
			<string>$lib_name.a</string>
			<key>SupportedArchitectures</key>
			<array>
				<string>arm64</string>
			</array>
			<key>SupportedPlatform</key>
			<string>ios</string>
EOF
        if [ "$t" = "$sim_target" ]; then
            cat <<'EOF'
			<key>SupportedPlatformVariant</key>
			<string>simulator</string>
EOF
        fi
        echo "		</dict>"
    done
    cat <<'EOF'
	</array>
	<key>CFBundlePackageType</key>
	<string>XFWK</string>
	<key>XCFrameworkFormatVersion</key>
	<string>1.0</string>
</dict>
</plist>
EOF
} > "$xcframework/Info.plist"

# ---- 9. Report --------------------------------------------------------------------------
step "Done"
for t in "${targets[@]}"; do
    printf '  %-28s %s\n' "$t" "$(du -h "target/$t/$profile/$lib_name.a" | cut -f1)"
done
printf '  %-28s %s\n' "PlayDropCoreFFI.xcframework" "$(du -sh "$xcframework" | cut -f1)"
printf '  %-28s %s\n' "PlayDropCore.swift" "$(du -h "$bindings_dir/PlayDropCore.swift" | cut -f1)"
case "$slices" in
    sim) echo "Simulator slice only: run without --sim-only before building for a device." ;;
    device) echo "Device slice only: a simulator build needs the simulator slice (macOS, without --device-only)." ;;
esac
if [ "$host" = Linux ]; then
    echo "Next: scripts/xtool-dev.sh installs the app on a connected iPhone."
else
    echo "Next: 'xcodegen generate' (first time), then build the PlayDrop scheme in Xcode."
fi

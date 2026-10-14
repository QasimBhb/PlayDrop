#!/usr/bin/env bash
# Wi-Fi baseline between this desktop and the phone with iperf3, both directions.
#
# Usage: bench/iperf3-phone.sh [--secs 10] [--runs 3] [--streams "1 4"] [--tool PATH]
#
# The phone has no iperf3. This script pushes an Android arm64 build to
# /data/local/tmp (shell-writable, no root, no phone setting changed) and runs it as the
# server; the desktop is the client. "desktop to phone" is a plain client run, "phone to
# desktop" is the same with -R. Prints a markdown table of MiB/s (TCP goodput as iperf3
# receives it, 1 MiB = 1,048,576 bytes, to match the PlayDrop rows).
#
# Getting the phone binary (kept out of the repo, in /tmp/pd-bench-tools):
#   The Android NDK's bionic has no pthread_cancel, which iperf3 3.16+ needs, so build
#   3.15 (single threaded). With NDK 27.1.12297006 (apps/android/local.properties has
#   sdk.dir):
#     mkdir -p /tmp/pd-bench-tools/src && cd /tmp/pd-bench-tools/src
#     curl -sSL -o iperf-3.15.tar.gz https://github.com/esnet/iperf/archive/refs/tags/3.15.tar.gz
#     mkdir b315 && tar xzf iperf-3.15.tar.gz -C b315 --strip-components=1 && cd b315
#     TC="$SDK/ndk/27.1.12297006/toolchains/llvm/prebuilt/linux-x86_64/bin"
#     PATH="$TC:$PATH" CC=aarch64-linux-android26-clang AR=llvm-ar RANLIB=llvm-ranlib \
#       ./configure --host=aarch64-linux-android --disable-shared --without-openssl --without-sctp
#     PATH="$TC:$PATH" make -j4
#     "$TC/llvm-strip" -o /tmp/pd-bench-tools/iperf3-3.15-android-arm64 src/iperf3
#   Do not link it statically: a static NDK binary is refused by Android 16 ("executable's
#   TLS segment is underaligned"). The dynamic build links the phone's own libc.
#   An iperf3 3.15 client against the desktop's 3.16 is wire compatible.

set -euo pipefail

secs=10
runs=3
streams="1 4"
tool="/tmp/pd-bench-tools/iperf3-3.15-android-arm64"
port=5201

while [[ $# -gt 0 ]]; do
    case "$1" in
        --secs) secs="$2"; shift 2 ;;
        --runs) runs="$2"; shift 2 ;;
        --streams) streams="$2"; shift 2 ;;
        --tool) tool="$2"; shift 2 ;;
        -h | --help) sed -n '2,10p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[[ -x "$tool" ]] || { echo "missing $tool: see the header of this script" >&2; exit 1; }
command -v iperf3 >/dev/null || { echo "iperf3 not installed on the desktop" >&2; exit 1; }
adb get-state >/dev/null 2>&1 || { echo "no adb device" >&2; exit 1; }

phone_ip="$(adb shell ip -f inet addr show wlan0 | tr -d '\r' | sed -nE 's/.*inet ([0-9.]+)\/.*/\1/p' | head -1)"
[[ -n "$phone_ip" ]] || { echo "phone has no Wi-Fi IPv4 address" >&2; exit 1; }

cleanup() { adb shell "pkill iperf3" >/dev/null 2>&1 || true; }
trap cleanup EXIT

adb push "$tool" /data/local/tmp/iperf3 >/dev/null
adb shell chmod 755 /data/local/tmp/iperf3
cleanup
adb shell "/data/local/tmp/iperf3 -s -p $port -D" >/dev/null
sleep 1

link="$(adb shell dumpsys wifi | tr -d '\r' | grep -m1 -i 'link speed' \
    | grep -oE '(Wi-Fi standard|RSSI|Link speed|Rx Link speed|Frequency): [^,]*' | paste -sd, - || true)"
echo "iperf3 desktop <-> phone, $(date -u +%F), phone $phone_ip, $secs s x $runs runs" >&2
echo "Phone Wi-Fi: $link" >&2
echo "Desktop: $(iperf3 --version | head -1), phone: iperf 3.15 (NDK 27.1 arm64)" >&2

echo
echo "| Direction | Streams | Run MiB/s | Mean MiB/s | Mean Mbit/s |"
echo "|---|---|---|---|---|"
for dir in "desktop to phone" "phone to desktop"; do
    flag=()
    [[ "$dir" == "phone to desktop" ]] && flag=(-R)
    for p in $streams; do
        vals=()
        for _ in $(seq 1 "$runs"); do
            json="$(iperf3 -c "$phone_ip" -p "$port" -t "$secs" -P "$p" "${flag[@]}" -J)"
            vals+=("$(python3 -I -c '
import json, sys
d = json.load(sys.stdin)
print(round(d["end"]["sum_received"]["bits_per_second"] / 8 / 1048576, 1))' <<<"$json")")
            sleep 2
        done
        python3 -I - "$dir" "$p" "${vals[@]}" <<'PY'
import sys
d, p, *v = sys.argv[1:]
v = [float(x) for x in v]
m = sum(v) / len(v)
print(f"| {d} | {p} | {', '.join(f'{x:.1f}' for x in v)} | {m:.1f} | {m * 8 * 1048576 / 1e6:.0f} |")
PY
    done
done

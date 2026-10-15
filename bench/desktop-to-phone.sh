#!/usr/bin/env bash
# Desktop to phone through the running daemon, one markdown row per dataset, with the
# result checked on the phone (file count and sha256 of every file).
#
# Usage: bench/desktop-to-phone.sh [--root /tmp/pd-bench] [--peer LABEL] [--runs N]
#                                  [--datasets "big-1g small-1000 ..."] [--no-verify]
#
# Needs: playdropd running and paired with the phone (CLAUDE.md "Commands" has the start
# line), the phone on adb and online in `playdrop peers`, bench/make-dataset.sh already
# run, the release CLI (`cargo build --release -p playdrop-cli`), python3, adb.
#
# Each run is `playdrop send --to <peer> --json <dataset>`, which hands the folder to the
# daemon (as a file descriptor on Linux, docs/ipc.md section 2), follows the daemon's
# events until the transfer ends and prints the final transfer snapshot. Wall time runs
# from just before that call to its exit, so it includes manifest building, hashing on
# both sides and the Android MediaStore writes (plus the CLI's start and connect, a few
# ms). MiB/s is total file bytes over that wall time.
#
# The Android app puts files in /sdcard/Download/PlayDrop/<dataset>/... (MediaStore
# Downloads, apps/android .../core/FileNames.kt). A bulk drop (64 files or more, mean under
# 1 MiB) lands in the app's own folder first and a mover copies it into Downloads afterwards
# (DownloadsSink.kt, "Landing"): the send returns when the files are on the phone, and the
# "In Downloads +s" column is how much later the last of them reached Downloads (0 when
# they were written there directly). The script clears that dataset folder on the phone
# before each run. It changes no phone setting. Screen state is whatever you leave it as;
# the script records it.

set -euo pipefail

root="/tmp/pd-bench"
peer=""
runs=1
datasets="tree small-1000 mixed-5000 medium-100 big-1g"
verify=1
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
playdrop="$repo/target/release/playdrop"
phone_dir="/sdcard/Download/PlayDrop"
landing_dir="/sdcard/Android/data/org.playdrop.android/files/landing"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --root) root="$2"; shift 2 ;;
        --peer) peer="$2"; shift 2 ;;
        --runs) runs="$2"; shift 2 ;;
        --datasets) datasets="$2"; shift 2 ;;
        --no-verify) verify=0; shift ;;
        -h | --help) sed -n '2,23p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[[ -x "$playdrop" ]] || { echo "missing $playdrop: cargo build --release -p playdrop-cli" >&2; exit 1; }
adb get-state >/dev/null 2>&1 || { echo "no adb device" >&2; exit 1; }
"$playdrop" status >/dev/null || exit 1

if [[ -z "$peer" ]]; then
    # First online peer that advertises file transfer and is not another desktop host.
    peer="$("$playdrop" peers --json | python3 -I -c '
import json, sys
for p in json.load(sys.stdin)["data"]:
    if p["online"] and "transfer" in p["capabilities"]:
        print(p["label"]); break')"
fi
[[ -n "$peer" ]] || { echo "no online peer; open the PlayDrop app on the phone" >&2; exit 1; }

model="$(adb shell getprop ro.product.model | tr -d '\r')"
android="$(adb shell getprop ro.build.version.release | tr -d '\r')"
screen="$(adb shell dumpsys power | tr -d '\r' | grep -m1 -o 'mWakefulness=[A-Za-z]*' || true)"
link="$(adb shell dumpsys wifi | tr -d '\r' | grep -m1 -i 'link speed' \
    | grep -oE '(Wi-Fi standard|RSSI|Link speed|Rx Link speed|Frequency): [^,]*' | paste -sd, - || true)"
echo "Desktop to phone, $(date -u +%F), peer '$peer' ($model, Android $android), $screen" >&2
echo "Wi-Fi: ${link:-unknown}" >&2
# A debuggable APK carries a Rust core built without optimisation: its numbers are not
# the release numbers. The notebook must say which one it measured.
if adb shell dumpsys package org.playdrop.android | grep -q 'pkgFlags=.*DEBUGGABLE'; then
    echo "App build: DEBUGGABLE (not a release build; do not publish these as release numbers)" >&2
else
    echo "App build: release" >&2
fi
echo "Daemon: $("$playdrop" status --json | python3 -I -c 'import json,sys; d=json.load(sys.stdin)["data"]; print(d["daemon_version"], d["addr"])')" >&2

echo
echo "| Dataset | Files | MiB | Run | State | Wall s | MiB/s | In Downloads +s | Verified on phone |"
echo "|---|---|---|---|---|---|---|---|---|"

for name in $datasets; do
    dir="$root/$name"
    [[ -d "$dir" ]] || { echo "missing $dir: run bench/make-dataset.sh" >&2; exit 1; }
    expect_files="$(wc -l <"$root/manifests/$name.sha256")"
    for run in $(seq 1 "$runs"); do
        adb shell "rm -rf '$phone_dir/$name'"
        sleep 1
        set +e
        t0="$(date +%s%N)"
        out="$("$playdrop" send --to "$peer" --json "$dir")"
        rc=$?
        t1="$(date +%s%N)"
        set -e
        if [[ -z "$out" ]]; then
            echo "| $name | | | $run | error | | | |"
            continue
        fi
        verdict="skipped"
        landing_s="0"
        if [[ "$verify" == 1 && "$rc" == 0 ]]; then
            # Wait for the mover to empty the landing folder (a dataset delivered straight to
            # Downloads never fills it). The folder is unreadable from adb on some phones: then
            # find prints nothing and the wait ends at once.
            t2="$(date +%s%N)"
            for _ in $(seq 1 1800); do
                left="$(adb shell "find '$landing_dir' -type f 2>/dev/null | wc -l" | tr -d '\r')"
                [[ "${left:-0}" == 0 ]] && break
                sleep 1
            done
            landing_s="$(python3 -I -c 'import sys; print(f"{int(sys.argv[1]) / 1e9:.1f}")' "$(( $(date +%s%N) - t2 ))")"
            got="$(adb shell "find '$phone_dir/$name' -type f | wc -l" | tr -d '\r')"
            # Both sides normalised to "sum path" (one space), sorted by path.
            phone_sums="$(adb shell "cd '$phone_dir' && find '$name' -type f -exec sha256sum {} +" \
                | tr -d '\r' | sed 's/  */ /' | LC_ALL=C sort -k2)"
            want_sums="$(sed 's/  */ /' "$root/manifests/$name.sha256" | LC_ALL=C sort -k2)"
            if [[ "$got" == "$expect_files" && "$phone_sums" == "$want_sums" ]]; then
                verdict="yes: $got files, all sums match"
            else
                verdict="NO: $got of $expect_files files, sums differ"
            fi
        fi
        python3 -I - "$name" "$run" "$verdict" "$out" "$((t1 - t0))" "$landing_s" <<'PY'
import json, sys
name, run, verdict, line, wall_ns, landing_s = sys.argv[1:7]
d = json.loads(line)
wall = int(wall_ns) / 1e9
mib = d["total"] / 1048576
print(f"| {name} | {d['file_count']} | {mib:.1f} | {run} | {d['state']} "
      f"| {wall:.3f} | {mib / wall:.2f} | {landing_s} | {verdict} |")
PY
    done
done

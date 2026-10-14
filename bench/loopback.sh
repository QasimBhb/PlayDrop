#!/usr/bin/env bash
# Loopback transport benchmark: PlayDrop HTTP/2 and raw framing against iperf3, all on
# 127.0.0.1. Prints a markdown table (MiB/s). Needs the release CLI:
#   cargo build --release -p playdrop-cli
#
# Usage: bench/loopback.sh [--size 1GiB] [--runs 3] [--iperf-secs 10] [--bin PATH]
#
# Loopback has no radio, so this isolates protocol and crypto cost. It is the only row
# that says anything about HTTP/2 vs raw framing; the Wi-Fi rows are in
# desktop-to-phone.sh and iperf3-phone.sh.

set -euo pipefail

size="1GiB"
runs=3
iperf_secs=10
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo/target/release/playdrop"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --size) size="$2"; shift 2 ;;
        --runs) runs="$2"; shift 2 ;;
        --iperf-secs) iperf_secs="$2"; shift 2 ;;
        --bin) bin="$2"; shift 2 ;;
        -h | --help) sed -n '2,12p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[[ -x "$bin" ]] || { echo "missing $bin: run cargo build --release -p playdrop-cli" >&2; exit 1; }
command -v iperf3 >/dev/null || { echo "iperf3 not installed" >&2; exit 1; }

tmp="$(mktemp -d)"
pids=()
cleanup() {
    for p in "${pids[@]:-}"; do
        [[ -n "$p" ]] && kill "$p" 2>/dev/null || true
    done
    rm -rf "$tmp"
}
trap cleanup EXIT

wait_port() {
    local port="$1" i
    for i in $(seq 1 50); do
        if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
            exec 3>&- 3<&-
            return 0
        fi
        sleep 0.1
    done
    echo "nothing listening on $port" >&2
    return 1
}

# One client run reports "<mode>: mean M MiB/s, min A, max B over N run(s)".
declare -A mean min max
port=42200
for mode in h2 raw; do
    "$bin" bench --role server --addr "127.0.0.1:$port" --mode "$mode" >"$tmp/server-$mode.log" 2>&1 &
    pids+=("$!")
    wait_port "$port"
    "$bin" bench --role client --addr "127.0.0.1:$port" --mode "$mode" --size "$size" --runs "$runs" \
        | tee "$tmp/client-$mode.log" >&2
    line="$(grep ' MiB/s, min ' "$tmp/client-$mode.log" | tail -1)"
    mean[$mode]="$(sed -E 's/.*mean ([0-9.]+) MiB.*/\1/' <<<"$line")"
    min[$mode]="$(sed -E 's/.*min ([0-9.]+),.*/\1/' <<<"$line")"
    max[$mode]="$(sed -E 's/.*max ([0-9.]+) over.*/\1/' <<<"$line")"
    kill "${pids[-1]}" 2>/dev/null || true
    port=$((port + 1))
done

iperf3 -s -p 5301 >"$tmp/iperf-server.log" 2>&1 &
pids+=("$!")
wait_port 5301
iperf3 -c 127.0.0.1 -p 5301 -t "$iperf_secs" -J >"$tmp/iperf.json"
iperf_mib="$(python3 -I - "$tmp/iperf.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
print(f"{d['end']['sum_received']['bits_per_second'] / 8 / 1048576:.1f}")
PY
)"

ratio() { awk -v a="$1" -v b="$2" 'BEGIN {printf "%.2f", a / b}'; }

echo
echo "Loopback, $(date -u +%F), host $(hostname), kernel $(uname -r), $(nproc) cores, size $size x $runs runs, load average at end: $(cut -d" " -f1-3 /proc/loadavg)"
echo
echo "| Path | Mean MiB/s | Min | Max | Share of iperf3 |"
echo "|---|---|---|---|---|"
echo "| iperf3 -c 127.0.0.1 -t $iperf_secs (1 stream) | $iperf_mib | | | 1.00 |"
for mode in h2 raw; do
    echo "| playdrop bench --mode $mode | ${mean[$mode]} | ${min[$mode]} | ${max[$mode]} | $(ratio "${mean[$mode]}" "$iperf_mib") |"
done
echo
echo "h2 vs raw: $(ratio "${mean[h2]}" "${mean[raw]}")"

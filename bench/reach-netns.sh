#!/usr/bin/env bash
# Gate R5 of docs/design/iroh-reach.md, and the dependency-tree half of R3.
#
# Two hosts in network namespaces, each behind its own NAT namespace, with a middle namespace
# holding a local iroh-relay and no route between the hosts except through it. They pair over a
# temporary direct link that is then deleted. Then:
#
#   relay    the default: no IP socket on the hosts, and mid does not forward either. Only the
#            relay can carry anything. Records MiB/s.
#   direct   --reach-direct, mid forwards, hole punching allowed. Records MiB/s separately, and
#            says which path the host reported.
#   dev      relay-only again, with the relay in --dev mode (plain http, no QUIC address
#            discovery), to show what that mode gives you.
#
# An nft counter in each host namespace counts every packet it sends, by destination: the relay,
# the peer, its old LAN address, and anything else, which must stay 0 (SR-35). `old_lan` is the
# LAN dial every reach dial comes after: TCP SYNs to the address the peer had when they met. They
# reach the NAT at most and carry nothing, and the run fails if they are ever more than that.
#
# Rootless: runs itself under `unshare -Urn`. Needs ip, nft, nsenter, openssl, and:
#   iroh-relay  cargo install iroh-relay --version 1.4.1 --features server \
#               --root ~/.cache/pd-p12/relay --locked
#   playdrop    cargo build --release -p playdrop-cli --features iroh
#
# Environment: PLAYDROP_BIN, IROH_RELAY_BIN, REACH_SIZE_MIB (default 256), REACH_WORK (default
# ~/.cache/pd-p12/netns, keep it on ext4, never under the repo), REACH_MODES (default
# "relay direct dev"), RUST_LOG (passed to the hosts), REACH_PRUNE_ONLY (tidy old runs and stop).
set -euo pipefail

NFT=/usr/sbin/nft
SIZE_MIB=${REACH_SIZE_MIB:-256}
WORK_ROOT=${REACH_WORK:-$HOME/.cache/pd-p12/netns}
TARGET=${CARGO_TARGET_DIR:-$HOME/.cache/playdrop-target-iroh}
BIN=${PLAYDROP_BIN:-$TARGET/release/playdrop}
RELAY_BIN=${IROH_RELAY_BIN:-$HOME/.cache/pd-p12/relay/bin/iroh-relay}

if [ "${REACH_NETNS_INSIDE:-}" != 1 ]; then
    # R3: the default builds' trees have no iroh. Outside the namespace: cargo may want its state.
    # A tree that could not be listed is a failure: no output is not the same as no iroh.
    here=$(cd "$(dirname "$0")/.." && pwd)
    for package in playdropd playdrop-cli; do
        tree=$(cd "$here" && cargo tree -p "$package" -e normal --offline) || {
            echo "R3 NOT CHECKED: cargo tree failed for $package" >&2
            exit 1
        }
        if grep -q iroh <<<"$tree"; then
            echo "R3 FAILED: the default $package tree has iroh in it" >&2
            exit 1
        fi
        echo "R3: default $package dependency tree has no iroh"
    done
    for tool in "$BIN" "$RELAY_BIN"; do
        [ -x "$tool" ] || { echo "missing $tool, see the top of this script" >&2; exit 1; }
    done
    export REACH_NETNS_INSIDE=1
    exec unshare -Urn "$0" "$@"
fi

RUN=$WORK_ROOT/$(date +%Y%m%d-%H%M%S)
mkdir -p "$RUN"
chmod 700 "$RUN"
MARKER=.reach-netns-run
: >"$RUN/$MARKER"
# Old runs keep their results.txt and nothing else: a payload is gigabytes. REACH_WORK can point
# anywhere, so only a folder named like a run that holds this script's marker counts as a run.
for old in "$WORK_ROOT"/*/; do
    old=${old%/}
    [ "$old" = "$RUN" ] && continue
    [[ "${old##*/}" =~ ^[0-9]{8}-[0-9]{6}$ ]] || continue
    [ -f "$old/$MARKER" ] || continue
    find "$old" -mindepth 1 ! -name results.txt ! -name "$MARKER" -delete 2>/dev/null || true
done
if [ -n "${REACH_PRUNE_ONLY:-}" ]; then
    exit 0
fi
RESULTS=$RUN/results.txt
say() { echo "$*" | tee -a "$RESULTS"; }

PIDS=()
HOST_PIDS=()
cleanup() {
    set +e
    for p in "${HOST_PIDS[@]:-}" "${PIDS[@]:-}"; do
        [ -n "$p" ] && kill "$p" 2>/dev/null
    done
    wait 2>/dev/null
    # This run keeps what explains it, not its payloads.
    find "$RUN" \( -name 'payload*' -o -name '*.part' \) -delete 2>/dev/null
}
trap cleanup EXIT

# A network namespace is a sleeping process holding it.
new_ns() {
    unshare -n sleep infinity &
    PIDS+=($!)
    NS_LAST=$!
    sleep 0.05
    nsenter -t "$NS_LAST" -n ip link set lo up
    nsenter -t "$NS_LAST" -n sysctl -qw net.ipv6.conf.all.disable_ipv6=1
}
in_ns() {
    local ns=$1
    shift
    nsenter -t "$ns" -n "$@"
}
# Joins two namespaces: link NS1 NAME1 ADDR1/24 NS2 NAME2 ADDR2/24.
link() {
    ip link add tmp_x type veth peer name tmp_y
    ip link set tmp_x netns "$1"
    ip link set tmp_y netns "$4"
    in_ns "$1" ip link set tmp_x name "$2"
    in_ns "$4" ip link set tmp_y name "$5"
    in_ns "$1" ip addr add "$3" dev "$2"
    in_ns "$4" ip addr add "$6" dev "$5"
    in_ns "$1" ip link set "$2" up
    in_ns "$4" ip link set "$5" up
}

new_ns; H_A=$NS_LAST
new_ns; N_A=$NS_LAST
new_ns; MID=$NS_LAST
new_ns; N_B=$NS_LAST
new_ns; H_B=$NS_LAST

link "$H_A" eth0 10.1.0.2/24 "$N_A" lan 10.1.0.1/24
link "$N_A" wan 10.100.1.2/24 "$MID" to_a 10.100.1.1/24
link "$N_B" wan 10.100.2.2/24 "$MID" to_b 10.100.2.1/24
link "$H_B" eth0 10.2.0.2/24 "$N_B" lan 10.2.0.1/24
in_ns "$H_A" ip route add default via 10.1.0.1
in_ns "$H_B" ip route add default via 10.2.0.1
in_ns "$N_A" ip route add default via 10.100.1.1
in_ns "$N_B" ip route add default via 10.100.2.1
for n in "$N_A" "$N_B"; do
    in_ns "$n" sysctl -qw net.ipv4.ip_forward=1
    in_ns "$n" "$NFT" -f - <<'NFT'
table ip nat {
    chain post {
        type nat hook postrouting priority srcnat; policy accept;
        oifname "wan" masquerade
    }
}
# A NAT: only what answers something sent from inside comes back in.
table ip filter {
    chain forward {
        type filter hook forward priority filter; policy drop;
        ct state established,related accept
        iifname "lan" accept
    }
    # What a router's firewall does. Without it the first punch packet to arrive leaves a
    # conntrack entry on the NAT, the host's own packet then collides with that entry and the
    # NAT gives it another port, and iroh stays on the relay: seen here at every default-timing
    # run, a real cost on a NAT built that way.
    chain input {
        type filter hook input priority filter; policy accept;
        iifname "wan" ct state new drop
    }
}
NFT
done

RELAY_IP=10.100.1.1

# The relay's certificate: a private CA and an IP address SAN. --dev mode needs none of this: it
# is plain http and has no QUIC address discovery, which needs TLS.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 2 \
    -subj "/CN=PlayDrop reach test CA" -keyout "$RUN/ca.key" -out "$RUN/ca.pem" 2>/dev/null
openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
    -subj "/CN=$RELAY_IP" -keyout "$RUN/relay.key" -out "$RUN/relay.csr" 2>/dev/null
printf 'subjectAltName=IP:%s\nextendedKeyUsage=serverAuth\n' "$RELAY_IP" >"$RUN/ext"
openssl x509 -req -in "$RUN/relay.csr" -CA "$RUN/ca.pem" -CAkey "$RUN/ca.key" -CAcreateserial \
    -days 2 -extfile "$RUN/ext" -out "$RUN/relay.pem" 2>/dev/null
cat >"$RUN/relay.toml" <<TOML
enable_relay = true
http_bind_addr = "0.0.0.0:3340"
enable_quic_addr_discovery = true
enable_metrics = false
[tls]
https_bind_addr = "0.0.0.0:8443"
quic_bind_addr = "0.0.0.0:7842"
cert_mode = "Manual"
manual_cert_path = "$RUN/relay.pem"
manual_key_path = "$RUN/relay.key"
TOML

RELAY_PID=
start_relay() {
    if [ "$1" = dev ]; then
        nsenter -t "$MID" -n env RUST_LOG=warn "$RELAY_BIN" --dev >"$RUN/relay-dev.log" 2>&1 &
    else
        nsenter -t "$MID" -n env RUST_LOG=warn "$RELAY_BIN" --config-path "$RUN/relay.toml" \
            >>"$RUN/relay.log" 2>&1 &
    fi
    RELAY_PID=$!
    PIDS+=("$RELAY_PID")
    sleep 0.5
    kill -0 "$RELAY_PID" || { echo "the relay did not start, see $RUN" >&2; exit 1; }
}
stop_relay() {
    kill "$RELAY_PID" 2>/dev/null || true
    wait "$RELAY_PID" 2>/dev/null || true
}

# One host: `playdrop host` with commands through a FIFO and its lines in a log.
# start_host NAME NS RELAY_URL EXTRA_FLAGS...
start_host() {
    local name=$1 ns=$2 url=$3
    shift 3
    mkdir -p "$RUN/$name/data" "$RUN/$name/in"
    chmod 700 "$RUN/$name" "$RUN/$name/data"
    rm -f "$RUN/$name/cmd"
    mkfifo "$RUN/$name/cmd"
    : >"$RUN/$name/log.$STAGE"
    # Held open read-write so the host never sees EOF between commands.
    nsenter -t "$ns" -n sh -c "exec \"\$0\" \"\$@\" <>\"$RUN/$name/cmd\" >\"$RUN/$name/log.$STAGE\" 2>&1" \
        "$BIN" host --data-dir "$RUN/$name/data" --receive-dir "$RUN/$name/in" \
        --name "$name" --port 42187 --bind 0.0.0.0 --no-discovery \
        --reach-relay "$url" "$@" &
    HOST_PIDS+=($!)
    eval "PID_$name=$!"
}
# A FIFO write blocks for good once its reader is gone, so a crashed host must not hang the run.
cmd() {
    if ! timeout 5 sh -c 'echo "$1" >"$2"' _ "$2" "$RUN/$1/cmd"; then
        echo "$1 did not take the command '$2': did it crash? see $RUN/$1/log.$STAGE" >&2
        return 1
    fi
}
stop_hosts() {
    cmd alpha quit || true
    cmd bravo quit || true
    local p
    for p in "${HOST_PIDS[@]}"; do
        wait "$p" 2>/dev/null || true
    done
    HOST_PIDS=()
}
# wait_log NAME TEXT SECONDS: waits for a line in the host's log of this stage.
wait_log() {
    local i
    for ((i = 0; i < $3 * 10; i++)); do
        grep -q -- "$2" "$RUN/$1/log.$STAGE" && return 0
        sleep 0.1
    done
    echo "timed out waiting for '$2' from $1, see $RUN/$1/log.$STAGE" >&2
    return 1
}

# Counts what a host sends, by destination (SR-35). Reads back with counters_of.
install_counters() { # NS RELAY PEER_INNER PEER_NAT PEER_OLD_LAN
    in_ns "$1" "$NFT" flush ruleset
    in_ns "$1" "$NFT" -f - <<NFT
table inet count {
    counter relay {}
    counter peer {}
    counter old_lan {}
    counter other {}
    chain out {
        type filter hook output priority filter; policy accept;
        oifname "lo" accept
        ip daddr $2 counter name relay accept
        ip daddr { $3, $4 } counter name peer accept
        ip daddr $5 counter name old_lan accept
        counter name other accept
    }
}
NFT
    # The counter can count: one packet to an address that is neither, then back to zero.
    in_ns "$1" bash -c 'echo x >/dev/udp/192.0.2.1/9' 2>/dev/null || true
    if counters_of "$1" >/dev/null; then
        echo "the 'other' counter did not see a test packet" >&2
        exit 1
    fi
    in_ns "$1" "$NFT" reset counters table inet count >/dev/null
}
counters_of() { # NS
    in_ns "$1" "$NFT" -j list counters table inet count |
        python3 -c '
import json, sys
old_lan_max = 64
out = {}
for item in json.load(sys.stdin)["nftables"]:
    c = item.get("counter")
    if c:
        out[c["name"]] = (c["packets"], c["bytes"])
for name in ("relay", "peer", "old_lan", "other"):
    p, b = out.get(name, (0, 0))
    print(f"{name}={p} packets/{b/1048576:.1f} MiB", end="  ")
print()
other = out.get("other", (0, 0))[0]
old_lan = out.get("old_lan", (0, 0))[0]
if old_lan > old_lan_max:
    print(f"old_lan is over {old_lan_max} packets: more than the LAN dials explain")
sys.exit(1 if other or old_lan > old_lan_max else 0)
'
}

PAIR_URL="https://$RELAY_IP:8443"
CA_FLAGS=(--reach-relay-ca "$RUN/ca.pem")

# --- Meet on a temporary direct link -------------------------------------------------------------
STAGE=pair
start_relay tls
link "$H_A" temp 10.9.0.1/24 "$H_B" temp 10.9.0.2/24
start_host alpha "$H_A" "$PAIR_URL" "${CA_FLAGS[@]}"
start_host bravo "$H_B" "$PAIR_URL" "${CA_FLAGS[@]}"
sleep 1
cmd bravo pair
sleep 0.5
cmd alpha "pair 10.9.0.2:42187"
wait_log alpha "PAIR with" 15
wait_log bravo "PAIR with" 15
cmd alpha y
cmd bravo y
wait_log alpha "paired with" 15
wait_log bravo "paired with" 15
wait_log alpha "online:" 15
cmd alpha "grants bravo +remote"
cmd bravo "grants alpha +remote"
for name in alpha bravo; do
    for ((i = 0; i < 100; i++)); do
        grep -q '"endpoint": "' "$RUN/$name/data/reach.json" 2>/dev/null && break
        sleep 0.1
    done
    grep -q '"endpoint": "' "$RUN/$name/data/reach.json" || {
        echo "$name did not learn its peer's endpoint, see $RUN" >&2
        exit 1
    }
done
stop_hosts
stop_relay
# Cut it: from here the only way between them is through mid.
in_ns "$H_A" ip link del temp
say "paired over a temporary link, reach endpoints exchanged, link deleted"

SRC=$RUN/payload.bin
head -c "$((SIZE_MIB * 1024 * 1024))" /dev/urandom >"$SRC"
SUM=$(sha256sum "$SRC" | cut -d' ' -f1)

# run_send: alpha sends the file to bravo. Prints MiB/s, and checks the hash.
run_send() {
    local name=payload-$STAGE.bin
    local src=$RUN/$name
    ln -f "$SRC" "$src"
    local t0 t1
    t0=$(date +%s.%N)
    cmd alpha "send bravo $src"
    wait_log bravo "saved .*$name" $((60 + SIZE_MIB)) >&2
    t1=$(date +%s.%N)
    local got
    got=$(sha256sum "$RUN/bravo/in/$name" | cut -d' ' -f1)
    if [ "$got" != "$SUM" ]; then
        say "HASH MISMATCH in stage $STAGE"
        exit 1
    fi
    python3 -c "print(f'{$SIZE_MIB / ($t1 - $t0):.1f}')"
}

check_counters() {
    local failed=0
    say "  alpha sent: $(counters_of "$H_A")" || failed=1
    counters_of "$H_A" >/dev/null || failed=1
    say "  bravo sent: $(counters_of "$H_B")" || failed=1
    counters_of "$H_B" >/dev/null || failed=1
    if [ "$failed" = 1 ]; then
        say "  SR-35 FAILED: a host sent to an address that is neither the relay nor the peer, or too much to its old LAN address"
        OVERALL=1
    else
        say "  SR-35 ok: nothing to any host but the relay and the peer"
    fi
}

path_line() { # the line `peers` prints right now
    cmd alpha peers
    sleep 0.5
    grep -E 'note: .*(on the relay|direct path)' "$RUN/alpha/log.$STAGE" | tail -1 || true
}

OVERALL=0
for mode in ${REACH_MODES:-relay direct dev}; do
    STAGE=$mode
    say ""
    say "== $mode =="
    extra=("${CA_FLAGS[@]}")
    url=$PAIR_URL
    case $mode in
    relay)
        in_ns "$MID" sysctl -qw net.ipv4.ip_forward=0
        start_relay tls
        ;;
    direct)
        in_ns "$MID" sysctl -qw net.ipv4.ip_forward=1
        start_relay tls
        extra+=(--reach-direct)
        ;;
    dev)
        in_ns "$MID" sysctl -qw net.ipv4.ip_forward=0
        start_relay dev
        url="http://$RELAY_IP:3340"
        extra=()
        ;;
    esac
    install_counters "$H_A" "$RELAY_IP" 10.2.0.2 10.100.2.2 10.9.0.2
    install_counters "$H_B" "$RELAY_IP" 10.1.0.2 10.100.1.2 10.9.0.1
    # A relay of another URL means the stored peer values must be refreshed: the dev stage has
    # another URL, and reach needs both to name the same relay. It rewrites the stored value.
    if [ "$mode" = dev ]; then
        python3 - "$RUN" "$url" <<'PY'
import json, sys
run, url = sys.argv[1:]
for name in ("alpha", "bravo"):
    p = f"{run}/{name}/data/reach.json"
    data = json.load(open(p))
    for e in data["peers"].values():
        e["relay"] = url
    json.dump(data, open(p, "w"))
PY
    fi
    t0=$(date +%s.%N)
    start_host alpha "$H_A" "$url" "${extra[@]}"
    start_host bravo "$H_B" "$url" "${extra[@]}"
    wait_log alpha "online:" 90
    wait_log bravo "online:" 30
    t1=$(date +%s.%N)
    say "  online across networks after $(python3 -c "print(f'{$t1 - $t0:.1f}')") s"
    if [ "$mode" = direct ]; then
        # Give hole punching time before measuring, and record what the host says.
        for ((i = 0; i < 30; i++)); do
            line=$(path_line)
            case $line in *"direct path"*) break ;; esac
            sleep 1
        done
    fi
    say "  host says: $(path_line)"
    rate=$(run_send)
    say "  $SIZE_MIB MiB alpha to bravo: $rate MiB/s, hash matches"
    say "  host says after the send: $(path_line)"
    check_counters
    stop_hosts
    stop_relay
done

say ""
say "results in $RESULTS"
exit "$OVERALL"

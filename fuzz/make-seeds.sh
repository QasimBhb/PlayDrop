#!/usr/bin/env bash
# Writes the committed seed corpora under fuzz/seeds/<target>/ (threat-model.md SR-33).
#
# Seeds are valid samples from docs/protocol.md and the core's unit tests, so the fuzzers
# start from inputs that reach deep into each decoder instead of dying in the first check.
# Only bash, printf and sha256sum are used; no build step. Re-running rewrites every seed.
#
# Usage: fuzz/make-seeds.sh
set -euo pipefail

cd "$(dirname "$0")"
out=seeds
rm -rf "$out"
mkdir -p "$out"/{beacon,mdns_txt,manifest,bundle,frame,session_json,qr_uri}

# Writes stdin to seeds/<target>/<name>.
seed() { cat >"$out/$1/$2"; }

# Four-byte big-endian length of the string in $1, as raw bytes.
be32() {
    local n=${#1}
    printf "\\x$(printf %02x $((n >> 24 & 255)))\\x$(printf %02x $((n >> 16 & 255)))"
    printf "\\x$(printf %02x $((n >> 8 & 255)))\\x$(printf %02x $((n & 255)))"
}

# One length-prefixed session frame.
frame() { be32 "$1"; printf '%s' "$1"; }

# One TXT RDATA string: length byte, then the bytes.
txt() { printf "\\x$(printf %02x ${#1})%s" "$1"; }

# Device ID of the Ed25519 SPKI whose key is 32 bytes of $1 (hex), the parties the
# session_json target pairs with.
device_id() {
    {
        printf '\x30\x2a\x30\x05\x06\x03\x2b\x65\x70\x03\x21\x00'
        for _ in $(seq 32); do printf "\\x$1"; done
    } | sha256sum | cut -c1-64
}

N=$(printf '4c%.0s' $(seq 16))
T=$(printf '9a%.0s' $(seq 16))
HEX64=$(printf 'a8%.0s' $(seq 32))
PEER=$(device_id 22)
LOCAL=$(device_id 11)

# --- beacon: protocol.md 8.1 shapes, private and "Add device" -------------------------
printf '{"t":"%s","v":1,"n":"%s"}' "$T" "$N" | seed beacon private
printf '{"t":"%s","v":1,"n":"%s","name":"Pop desktop","type":"desktop","port":42187}' "$T" "$N" | seed beacon add_device
printf '{"t":"%s","v":2,"n":"%s","future":[1,2]}' "$T" "$N" | seed beacon unknown_key
printf '{"t":"%s","v":1,"n":"%s","name":"Ev\\u202eil\\u0007","type":"phone","port":42188}' "$T" "$N" | seed beacon bidi_name

# --- mdns_txt: SRV port (2 bytes BE) then TXT RDATA ------------------------------------
{ printf '\xa4\xcb'; txt "v=1"; txt "n=$N"; txt "t=$T"; } | seed mdns_txt private
{ printf '\xa4\xcb'; txt "v=1"; txt "n=$N"; txt "t=$T"; txt "name=Pop desktop"; txt "type=desktop"; } | seed mdns_txt add_device
{ printf '\xa4\xcb'; txt "V=1"; txt "v=9"; txt "N=$N"; txt "t=$T"; txt "flag"; } | seed mdns_txt case_and_duplicates

# --- manifest: protocol.md 7.2 example and variants ------------------------------------
printf '%s' '{"id":"c1d2e3f4a5b6c7d8e9f00112233445f0","chunkSize":1048576,"totalBytes":684388316,"note":"Holiday","files":[{"fid":0,"path":["Holiday","IMG_0001.HEIC"],"size":2811004,"mtime":1791460800000,"mime":"image/heic"},{"fid":1,"path":["Holiday","clip.mp4"],"size":681574400,"mtime":1791460900000,"mime":"video/mp4"},{"fid":2,"path":["Holiday","notes.txt"],"size":2912,"mtime":1791460950000,"inBundle":true}],"dirs":[["Holiday","empty"]],"bundle":{"size":4608}}' | seed manifest spec_example
printf '%s' '{"id":"0123456789abcdef0123456789abcdef","chunkSize":65536,"totalBytes":0,"files":[{"fid":0,"path":["a"],"size":0,"mtime":-1,"inBundle":true},{"fid":1,"path":["b","c"],"size":0,"mtime":0}],"bundle":{"size":1536}}' | seed manifest empty_members
printf '%s' '{"id":"0123456789abcdef0123456789abcdef","chunkSize":65536,"totalBytes":1,"files":[{"fid":0,"path":["café","CON.txt"],"size":1,"mtime":0}]}' | seed manifest device_name

# --- bundle: structured input; a few fixed byte patterns get it started ------------------
printf '\x03\x00\x10\x00\x00\x00\x00\x00\x00\x00\x00\x01\x41\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00\x01\x42\x00\x00\x00' | seed bundle two_members
printf '\x02\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00\x01\x07\x01\x03\x02\x10\x00\x20\x00\x00' | seed bundle resume
printf '\x01\xff\x0b\x00\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x01\x00\x01\x94\x00\x00\x00\x01' | seed bundle corrupt

# --- frame: one read-size byte, then length-prefixed frames ------------------------------
HELLO='{"t":"hello","v":1,"seq":1,"protocolVersion":1,"deviceId":"'"$PEER"'","name":"Pixel 9","type":"phone","port":42187,"app":"playdrop-android/0.1.0","keepAlive":300,"trust":"paired","caps":{"in":["ping","find","clipboard","transfer","outbox"],"out":["ping","clipboard","input.key","transfer"]}}'
{ printf '\x00'; frame "$HELLO"; } | seed frame hello
{
    printf '\x07'
    frame "$HELLO"
    frame '{"t":"capabilities","v":1,"seq":2,"caps":{"in":["ping","transfer"],"out":["ping","transfer"]}}'
    frame '{"t":"ping","v":1,"seq":3,"message":"Hello from the desktop"}'
    frame '{"t":"pong","v":1,"seq":4,"re":3}'
    frame '{"t":"clipboard","v":1,"seq":5,"kind":"text","mime":"text/plain;charset=utf-8","hash":"9b1d","text":"https://example.org/a","sensitive":false}'
    frame '{"t":"input","v":1,"seq":6,"kind":"pointer","op":"move","dx":12.5,"dy":-3}'
    frame '{"t":"media","v":1,"seq":7,"op":"command","player":"spotify","action":"seek","value":90000}'
} | seed frame session
{
    printf '\x01'
    frame '{"t":"ping","v":1,"seq":1}'
    frame '{"t":"ping","v":1,"seq":1}'
    frame '{"t":"hologram","v":1,"seq":2}'
    frame '{"t":"ping","v":7,"seq":3}'
    frame '{"t":"error","v":9,"seq":4,"code":"forbidden"}'
} | seed frame duplicate_unknown_version
{ printf '\x05'; printf '\x00\x01\x00\x01'; } | seed frame too_large
{ printf '\x03'; frame '{"t":"ping","v":1,"seq":1}'; printf '\x00\x00\x00\x40{"t":'; } | seed frame truncated

# --- session_json: payloads separated by 0x1E, shared seq numbering -----------------------
{
    printf '%s' "$HELLO"; printf '\x1e'
    printf '%s' '{"t":"pair_request","v":1,"seq":2,"method":"sas","commit":"'"$HEX64"'"}'; printf '\x1e'
    printf '%s' '{"t":"pair_reveal","v":1,"seq":3,"nonce":"Hk3sL9eT0pYw2uZq6vMd1g"}'
} | seed session_json sas_responder
{
    printf '%s' '{"t":"pair_response","v":1,"seq":1,"re":1,"nonce":"q8pZ2mVt4kQ1xR7bN0cWfA"}'; printf '\x1e'
    printf '%s' '{"t":"pair_confirm","v":1,"seq":2}'
} | seed session_json sas_initiator
{
    printf '%s' '{"t":"pair_request","v":1,"seq":1,"method":"qr","proof":"'"$HEX64"'"}'; printf '\x1e'
    printf '%s' '{"t":"pair_confirm","v":1,"seq":2,"re":1,"proof":"'"$HEX64"'"}'
} | seed session_json qr
{
    printf '%s' '{"t":"media","v":1,"seq":1,"op":"state","player":"spotify","name":"Spotify","playing":true,"title":"Song","artist":"Artist","album":"Album","durationMs":215000,"positionMs":61200,"positionAt":1791460800123,"volume":70,"can":["play","pause","next","previous","seek","volume"],"art":{"id":"5c0e","size":40211}}'; printf '\x1e'
    printf '%s' '{"t":"presence","v":1,"seq":2,"state":"asleep","reason":"doze","expectBackMs":900000}'; printf '\x1e'
    printf '%s' '{"t":"outbox_ready","v":1,"seq":3,"count":3,"bytes":734003200,"ids":["c1d2"]}'; printf '\x1e'
    printf '%s' '{"t":"input","v":1,"seq":4,"kind":"key","key":"Enter","mods":["ctrl"],"action":"press"}'; printf '\x1e'
    printf '%s' '{"t":"transfer","v":1,"seq":5,"id":"c1d2","state":"cancelled","reason":"user"}'; printf '\x1e'
    printf '%s' '{"t":"announce_key","v":1,"seq":6,"kid":7,"key":"m3V9Qw"}'; printf '\x1e'
    printf '%s' '{"t":"battery","v":1,"seq":7,"level":64,"charging":true,"low":false}'; printf '\x1e'
    printf '%s' '{"t":"find","v":1,"seq":8,"action":"start"}'; printf '\x1e'
    printf '%s' '{"t":"unpair","v":1,"seq":9}'
} | seed session_json companion
printf '%s' "${HELLO/$PEER/$LOCAL}" | seed session_json hello_local

# --- qr_uri: protocol.md 6.3 shape with real-length values --------------------------------
QR="playdrop://pair?id=$PEER&fp=$HEX64&addr=192.168.1.20:42187&addr=[fd00::20]:42187&secret=Zq0tR3vBn1Kc8wYe4pLm2A&exp=1791460920"
printf '%s' "$QR" | seed qr_uri spec_example
printf '%s' "${QR//\[/%5B}" | sed 's/\]/%5D/g' | seed qr_uri percent_encoded
printf '%s' "PLAYDROP://pair?future=1&id=$PEER&fp=$HEX64&addr=%5Bfe80::1%253%5D:1&secret=Zq0tR3vBn1Kc8wYe4pLm2A&exp=0" | seed qr_uri scoped_ipv6

echo "seeds written to $(pwd)/$out"

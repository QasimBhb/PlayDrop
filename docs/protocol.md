# PlayDrop Protocol, version 1

Status: draft for Phase 1. Licence: GPL-3.0-or-later.

**This document is normative.** `PLAN.md` is the rationale; `docs/threat-model.md` holds the security requirements (SR-n) implemented here, and section 11 lists deliberate differences. On wire behaviour this document wins. Details neither fixed are chosen here with a one-line reason marked *Choice*.

MUST, SHOULD and MAY are as in RFC 2119. Examples shorten long hex with `...`. JSON is UTF-8; binary integers are unsigned big-endian.

---

## 1. Constants

| Name | Value |
|---|---|
| Protocol | PlayDrop Protocol, `protocolVersion` = `1` |
| TCP port (TLS 1.3 + HTTP/2) | `42187`, configurable override |
| UDP port (beacon) | `42187` |
| Multicast group | `239.255.42.187` (IPv4 organization-local) |
| mDNS service | `_playdrop._tcp` |
| ALPN | `playdrop/1` |
| Default chunk size | 1 MiB |
| Pairing attempt timeout | 30 s |
| "Add device" window | 120 s |

Hosts MUST NOT bind or announce on TCP/UDP 1714-1764 or 53317, or join 224.0.0.167; v1 has no compatibility mode. The TCP port is bound exclusively (SR-31). If it is taken, diagnostics say so; there is no random-port fallback, which would break last-known dial and the firewall rule.

---

## 2. Identity

- Each device creates one Ed25519 keypair and a self-signed X.509 v3 certificate on first run.
- **Device ID** = lowercase hex SHA-256 of the DER SubjectPublicKeyInfo (64 characters). The raw 32 bytes are the "SPKI hash".
- **Fingerprint** (`fp`) = lowercase hex SHA-256 of the whole DER certificate. Used in the QR payload.
- **Short ID** = the first 16 characters of the lowercase, unpadded RFC 4648 base32 encoding of the SPKI hash, in groups of four (`k3qd-7f2m-xa9p-41bc`). Every trust prompt shows it next to the name (SR-8).
- Certificate: CN = Device ID, random serial, `notAfter` = 99991231235959Z. Verifiers ignore dates. *Choice:* wrong phone clocks must not break trust, which comes from the pin.
- **A new key is a new device** (SR-10): no "key changed, accept?" path and no pin migration in v1.

---

## 3. Transport

### 3.1 TLS profile (SR-11)

- TCP, then **TLS 1.3 only**, then HTTP/2 (RFC 9113).
- **Ed25519 only** for certificates and signatures.
- **Client certificate required.** Both sides use a custom verifier, no CA store: it accepts only a pinned SPKI hash, except during an open "Add device" window (6.1), when any valid self-signed Ed25519 certificate enters unpaired mode.
- **ALPN required** (section 4).
- **No session tickets, no resumption, no 0-RTT.** Every connection does a full handshake, so a revoked pin cannot return through a cached session.
- **No SNI:** the client uses the peer's IP literal as server name.
- Key exchange X25519, then P-256. Ciphers `TLS_AES_128_GCM_SHA256` and `TLS_CHACHA20_POLY1305_SHA256` (order in 10).
- One TCP connection per peer pair carries session, blobs and transfers; never a second socket per payload.

### 3.2 Dialling

A host dials only a pinned peer, a device the user picked in "Add device", or a scanned QR code (SR-1). Addresses come from verified adverts (8.3), the last-known list (8 per peer, newest first) and QR. All candidates are tried at once, staggered by 250 ms; the first to complete TLS with the expected SPKI hash wins. *Choice:* a stale address then costs 250 ms, not a timeout.

### 3.3 Duplicate connections

If two live connections exist between the same pair, one dialled by each side, compare Device IDs as lowercase hex strings. The connection dialled by the **lower** ID is kept. The **higher** device closes its own outbound with `GOAWAY` (`NO_ERROR`, debug data `duplicate_connection`) once it has the peer's `hello` on the surviving connection. In-flight transfers resume on the survivor (7.6).

A new connection from the same dialler replaces the old one (it redials only when it thinks the old one is dead). If the lower device gets an inbound from the higher while holding an outbound, it PINGs the outbound; no ACK in 2 s means it keeps the inbound instead.

### 3.4 Keep-alive

HTTP/2 PING frames, not application frames: desktop every 60 s, mobile every 300 s, announced in `hello.keepAlive`; only the side with the shorter interval pings. No ACK within 15 s means dead. Nobody closes an idle connection sooner than 2.5 times the larger interval. A phone about to sleep SHOULD send `presence` `asleep` first.

---

## 4. Versioning and capabilities (SR-12)

- `protocolVersion` is one integer; this document is 1.
- **The version is chosen by ALPN only.** A host offers one ALPN per version at or above its minimum, highest first (`playdrop/2`, `playdrop/1`), and TLS selects one. No fallback to older, plaintext or unauthenticated modes. A release raises the minimum by dropping old ALPN strings.
- `hello.protocolVersion` MUST equal the ALPN result, else close with `unsupported_version`. *Choice:* hello carries no version list, so there is one authenticated source of truth.
- The discovery `v` key is a UI hint only. New versions are for incompatible changes; compatible additions use capabilities and per-frame `v`.
- **Capabilities** come in two lists, as in KDE Connect: `in` (what this host accepts) and `out` (what it may send). A host MUST NOT send anything the peer's `in` list lacks. v1 strings: `ping`, `find`, `clipboard`, `clipboard.image`, `battery`, `media.state`, `media.control`, `presence`, `input.pointer`, `input.key`, `input.presenter`, `transfer`, `transfer.bundle`, `outbox`, `blobs`.
- Unknown capabilities, fields and frame types are ignored.

---

## 5. Session plane

### 5.1 Stream

The dialler sends `POST /v1/session` (`content-type: application/vnd.playdrop.frames`, no length); the listener replies `200` at once. Both directions stay open for the connection's life; its end closes the connection. A second session stream gets `409 conflict`. Each side sends `hello` first, without waiting.

### 5.2 Frame encoding

```
[ length: u32 BE ][ JSON object of `length` bytes ]
```

- Maximum frame size 64 KiB; a larger length closes the connection (SR-15).
- Every frame has `t` (type), `v` (frame version, from 1) and `seq`. Replies also carry `re`, the `seq` being answered.
- **Replay rule (SR-16):** `seq` starts at 1 on each connection, per direction, and increases by exactly 1 per frame. A frame whose `seq` is not greater than the last accepted one is a duplicate and is dropped with no effect. A gap, malformed JSON or a missing `t`/`v`/`seq` closes the connection.
- A `v` newer than supported gets `error` `unsupported_frame_version` and is ignored.
- Binary data never travels in frames; it uses blobs (5.15). Examples below omit `seq` unless `re` refers to it.

### 5.3 Frame types and grants

| `t` | Sender | Capability | Grant at receiver | Unpaired |
|---|---|---|---|---|
| `hello` | both, first | - | - | yes |
| `capabilities` | both | - | - | no |
| `announce_key` | both | - | - | no |
| `ping` / `pong` | both | `ping` | - | yes |
| `find` | both | `find` | `find` | no |
| `clipboard` | both | `clipboard` | `clipboardIn` | no |
| `battery` | both | `battery` | - | no |
| `media` | both | `media.*` | `media` | no |
| `presence` | both | `presence` | - | no |
| `outbox_ready` | both | `outbox` | - | no |
| `input` | controller | `input.*` | `input` | no |
| `transfer` | receiver | `transfer` | - | no |
| `pair_request`, `pair_response`, `pair_reveal`, `pair_confirm`, `pair_reject` | see 6 | - | - | yes |
| `unpair` | both | - | - | no |
| `error` | both | - | - | yes |

**Authorisation (SR-13, SR-14).** Every frame and request is checked against the grants of the Device ID authenticated by TLS, never against anything in the payload. A frame lacking its grant is dropped with no effect; the receiver MAY send one `error` `forbidden` per frame type per connection so the sender can hide the feature. On an unpaired connection, frames not marked "yes" get `not_paired`.

### 5.4 hello

```json
{"t":"hello","v":1,"seq":1,"protocolVersion":1,
 "deviceId":"3f9ac21e7b04...","name":"Pixel 9","type":"phone","port":42187,
 "app":"playdrop-android/0.1.0","keepAlive":300,"trust":"paired",
 "caps":{"in":["ping","find","clipboard","transfer","outbox"],"out":["ping","clipboard","input.key","transfer"]}}
```

- `deviceId` MUST match the certificate, else `error` `identity_mismatch` and close.
- `type`: `desktop`, `laptop`, `phone`, `tablet`, `tv`. `port` is the sender's listening port.
- `name` never feeds a trust decision. Receivers strip C0/C1 controls and bidi overrides and cut it to 64 characters (SR-8); after pairing the UI shows a local label that peer renames never change.
- `trust` is `paired` if the sender has the peer pinned. On a mismatch the pinning side marks the pin "removed by peer" and asks to pair again. *Choice:* KDE Connect's silent pairing loss is a top complaint.

### 5.5 capabilities

```json
{"t":"capabilities","v":1,"caps":{"in":["ping","transfer"],"out":["ping","transfer"]}}
```
Full replacement, e.g. when the user turns a feature off.

### 5.6 ping, pong, find

```json
{"t":"ping","v":1,"seq":41,"message":"Hello from the desktop"}
{"t":"pong","v":1,"re":41}
{"t":"find","v":1,"action":"start"}
```
`ping` shows a notification (`message` optional, max 256 bytes) and is always answered. `find` rings at full volume until dismissed or `"action":"stop"` arrives.

### 5.7 clipboard

```json
{"t":"clipboard","v":1,"kind":"text","mime":"text/plain;charset=utf-8",
 "hash":"9b1d...c0","text":"https://example.org/a","sensitive":false}
{"t":"clipboard","v":1,"kind":"image","mime":"image/png",
 "hash":"77e2...4a","blob":{"id":"77e2...4a","size":482113}}
```

- `hash` = lowercase hex BLAKE3 of the content. Text is hashed as UTF-8 after converting CRLF and lone CR to LF. *Choice:* Windows rewrites line endings, which would defeat echo detection.
- Text up to 32 KiB is inline; longer text (up to 1 MiB, the sync limit in SR-24) and images go as a blob whose `id` equals `hash`.
- **Echo prevention:** each host remembers the hash of the last clipboard it applied from a peer and the last it sent, and MUST NOT send a local change whose hash equals either. Received clipboards are never forwarded to other peers.
- `sensitive: true` marks a password-manager clip sent by hand (SR-24); it never appears in notifications and is written with history and cloud upload off.
- Sending needs the sender's `clipboardOut` grant; applying needs the receiver's `clipboardIn`.

### 5.8 battery

```json
{"t":"battery","v":1,"level":64,"charging":true,"low":false}
```
Pushed on session start and on change, at most once per 60 s unless `charging` or `low` changes.

### 5.9 media

```json
{"t":"media","v":1,"op":"state","player":"spotify","name":"Spotify","playing":true,
 "title":"Song","artist":"Artist","album":"Album","durationMs":215000,
 "positionMs":61200,"positionAt":1791460800123,"volume":70,
 "can":["play","pause","next","previous","seek","volume"],"art":{"id":"5c0e...19","size":40211}}
{"t":"media","v":1,"op":"command","player":"spotify","action":"seek","value":90000}
```
- `action`: `play`, `pause`, `playPause`, `stop`, `next`, `previous`, `seek` (absolute ms), `volume` (0-100).
- Position is sent with `positionAt` (Unix ms) and extrapolated, never streamed; state is coalesced to 2 frames per second per player. *Choice:* KDE's MPRIS flood drained batteries.

### 5.10 presence

```json
{"t":"presence","v":1,"state":"asleep","reason":"doze","expectBackMs":900000}
```
`state`: `active`, `idle`, `locked`, `asleep`. After `asleep` the peer expects a drop and shows the device as asleep rather than gone.

### 5.11 outbox_ready

```json
{"t":"outbox_ready","v":1,"count":3,"bytes":734003200,"ids":["c1d2...f0"]}
```
Sent when items are queued and on session start if any wait. Foreground-only hosts (iOS) pull at once (7.8).

### 5.12 input

```json
{"t":"input","v":1,"kind":"session","op":"start"}
{"t":"input","v":1,"kind":"pointer","op":"move","dx":12.5,"dy":-3}
{"t":"input","v":1,"kind":"pointer","op":"button","button":"left","action":"click"}
{"t":"input","v":1,"kind":"key","key":"Enter","mods":["ctrl"],"action":"press"}
{"t":"input","v":1,"kind":"presenter","action":"next"}
```
- The phone opens an input session with `session`/`start` and ends it with `stop`. The desktop applies input only inside a session, with the `input` grant, while unlocked (SR-25), and releases every key and button on `stop` or disconnect.
- Pointer `op`: `move`, `button` (`left`/`right`/`middle`; `down`/`up`/`click`/`double`), `scroll` (`dx`, `dy`, `unit` `line` or `pixel`). Presenter: `next`, `previous`, `pointerOn`, `pointerOff`, `blackout`.
- `key` uses W3C UI Events `key` values, or `text` for a literal string; `mods`: `ctrl`, `alt`, `shift`, `meta`. *Choice:* a documented, neutral vocabulary that maps to `SendInput` and libei.
- Never answered except by `error`. Pointer moves are coalesced to one per 8 ms; receivers cap keys at 60 per second.

### 5.13 transfer

```json
{"t":"transfer","v":1,"id":"c1d2...f0","state":"accepted"}
{"t":"transfer","v":1,"id":"c1d2...f0","state":"cancelled","reason":"user"}
```
The receiver pushes this when a pending offer is accepted or declined, or when it cancels or fails a transfer. The sender then reads `GET /v1/transfers/{id}`. *Choice:* push the decision instead of holding the POST open or polling.

### 5.14 error

```json
{"t":"error","v":1,"re":45,"code":"forbidden","message":"media control is off for this device"}
```
`re` is omitted for unsolicited errors. Codes are in section 9.

### 5.15 Blobs

`GET /v1/blobs/{id}` returns the bytes with `content-type` and `content-length`. `id` = lowercase hex BLAKE3 of the bytes, so the fetcher verifies it. A blob lives 5 minutes after its frame and is served only to the Device ID it was announced to.

---

## 6. Pairing

### 6.1 "Add device" window and unpaired connections (SR-6)

- Pairing is possible only while the local user has opened "Add device". The window closes after 120 s, on success, or after 3 failed attempts, and cannot open while the desktop is locked.
- Outside the window, a connection from an unpinned certificate is closed right after the handshake with no UI (SR-15).
- Inside it, the connection is in **unpaired mode**: only `POST /v1/session` (else `403 not_paired`) and frames marked "yes" in 5.3. At most 3 such connections and 5 `pair_request` per minute per source IP (else `rate_limited`); one pairing at a time (else `busy`).
- On success the connection becomes trusted in place. Both sides send `capabilities` and `announce_key` (8.2).

The **initiator** is the device whose user picked the other one from its "Add device" list. Both devices must have their windows open.

### 6.2 SAS pairing (SR-7)

```
I -> R  pair_request  {method:"sas", commit = hex BLAKE3(nonce_i)}
R -> I  pair_response {nonce_r}
I -> R  pair_reveal   {nonce_i}      R checks the commit; mismatch -> pair_reject commit_mismatch
both    show SAS; pair_confirm when the local user taps "Pair"
both    pin the peer after sending and receiving pair_confirm
```

```json
{"t":"pair_request","v":1,"seq":2,"method":"sas","commit":"5f0c...a2"}
{"t":"pair_response","v":1,"re":2,"nonce":"q8pZ2mVt4kQ1xR7bN0cWfA"}
{"t":"pair_reveal","v":1,"nonce":"Hk3sL9eT0pYw2uZq6vMd1g"}
{"t":"pair_confirm","v":1}
{"t":"pair_reject","v":1,"reason":"user"}
```

Nonces are 16 fresh random bytes per attempt, base64url without padding. The pairing nonce is `nonce_i || nonce_r`; commit-then-reveal stops a man in the middle from grinding a key to hit 20 bits.

```
lo, hi           = the two Device IDs, sorted as lowercase hex strings
spki_lo, spki_hi = DER SubjectPublicKeyInfo of the device with that ID, as seen in this handshake
ekm = TLS-Exporter("EXPORTER-playdrop-pair", context = empty, length = 32)   (RFC 8446 section 7.5)
h   = BLAKE3( ascii(lo) || ascii(hi) || spki_lo || spki_hi || nonce_i || nonce_r || ekm )
x   = first 20 bits of h, big-endian: (h[0] << 12) | (h[1] << 4) | (h[2] >> 4)
SAS = x mod 1000000, as 6 zero-padded decimal digits
```

- Inputs are fixed length (64, 64, 44, 44, 16, 16, 32 bytes), so no separators are needed.
- `ekm` binds the SAS to this TLS session: a man in the middle holds two sessions with different exporter values, so the screens disagree.
- *Choice:* 20 bits reach 1,048,575, so the value is reduced mod 10^6; a few values become twice as likely, and the guess chance stays about one in a million.
- Both screens show the SAS, sanitised name and Short ID, with no button focused by default.
- An attempt not complete 30 s after `pair_request` ends with `pair_reject` `timeout` and a close.
- `pair_reject` reasons: `user`, `timeout`, `commit_mismatch`, `busy`, `rate_limited`, `qr_proof_invalid`, `qr_expired`.

### 6.3 QR pairing (SR-9)

Device A opens "Add device" and shows a QR code; device B scans it in the app.

```
playdrop://pair?id=<deviceId>&fp=<certFingerprint>&addr=<ip:port>[&addr=...]&secret=<base64 one-time secret>&exp=<unix seconds>
playdrop://pair?id=3f9ac21e7b04...&fp=a81c...9e&addr=192.168.1.20:42187&addr=[fd00::20]:42187&secret=Zq0tR3vBn1Kc8wYe4pLm2A&exp=1791460920
```

- IPv6 addresses are bracketed; values are percent-encoded (RFC 3986). `secret` is 16 random bytes, base64url without padding; `exp` is at most 120 s after display.
- The first attempt consumes the secret, success or not; it also dies when the QR screen closes or `exp` passes. The QR screen is excluded from screen capture.
- B MUST abort before sending anything unless A's certificate matches both `id` and `fp`.
- The secret replaces the SAS comparison. Proofs are HMAC-SHA256 keyed by the secret over the raw SPKI hashes and the 6.2 exporter value:

```
proof_b = HMAC-SHA256(secret, "pd-qr-b-v1" || spkihash_A || spkihash_B || ekm)
proof_a = HMAC-SHA256(secret, "pd-qr-a-v1" || spkihash_B || spkihash_A || ekm)
```

```json
{"t":"pair_request","v":1,"seq":2,"method":"qr","proof":"0e4b...77"}
{"t":"pair_confirm","v":1,"re":2,"proof":"d91a...3c"}
```

A checks `proof_b` against B's certificate and this session, then shows "Add <name> (<Short ID>)?" and pins B only after one tap. It then sends `pair_confirm` with `proof_a`; B checks it and pins A. A bad proof gets `pair_reject` `qr_proof_invalid`.

### 6.4 Grants, unpair and auto-accept (SR-14)

Each pin holds per-device grants: `files` (auto-accept), `clipboardIn`, `clipboardOut`, `input`, `media`, `find`. Defaults at pairing: all on except `input`. Grants change only from the local UI, never from a frame.

- With `files`, an offer from that device is accepted at once (`201`) whenever `totalBytes` fits in free space minus max(2 GiB, 5% of the volume), however large. A notification with "Open" and "Show in folder" appears when it finishes. Auto-accept is per pinned key only, never per name, address or network.
- Without `files`, offers wait for the user (`202`).

```json
{"t":"unpair","v":1}
```
Unpair, from either side, closes all connections with that peer within 1 s, deletes its pin, grants, tokens and transfers, and rotates the local announce key (8.2). An offline unpair reaches the peer through `hello.trust`.

---

## 7. Transfer plane

### 7.1 Endpoints

All endpoints except `/v1/session` need a paired connection. Transfer and outbox IDs are bound to the Device ID that created them or that they are queued for; any other Device ID gets `404 not_found`, the same as a missing ID (SR-13).

| Method and path | Purpose | Success | Errors |
|---|---|---|---|
| `POST /v1/session` | Session stream | 200 | 409 |
| `GET /v1/blobs/{id}` | Clipboard image, art, long text | 200 | 404 |
| `POST /v1/transfers` | Offer a manifest | 201, 202, 200 | 400, 403, 409, 413, 507 |
| `PUT /v1/transfers/{id}/files/{fid}` | Stream or resume one file | 200 | 401, 404, 409, 410, 422 |
| `PUT /v1/transfers/{id}/bundle` | Stream the tar bundle | 200 | 401, 404, 409, 410, 422 |
| `GET /v1/transfers/{id}` | State, tokens, verified offsets | 200 | 404 |
| `DELETE /v1/transfers/{id}` | Cancel | 204 | 404 |
| `GET /v1/outbox` | Items queued for the caller | 200 | - |
| `GET /v1/outbox/{id}` | One item's manifest | 200 | 404 |
| `GET /v1/outbox/{id}/files/{fid}` | Pull one file | 200, 206 | 404, 416 |
| `GET /v1/outbox/{id}/bundle` | Pull the bundle | 200, 206 | 404, 416 |
| `DELETE /v1/outbox/{id}` | Puller confirms receipt | 204 | 404 |

*Choice:* PLAN.md names only the outbox list and item; pulling bytes needs the `files`, `bundle` and `DELETE` sub-resources, which mirror the push side.

HTTP error body:
```json
{"error":{"code":"offset_mismatch","message":"expected 4194304","verifiedOffset":4194304}}
```

### 7.2 Manifest

```json
POST /v1/transfers
{"id":"c1d2e3f4a5b6c7d8e9f00112233445f0","chunkSize":1048576,"totalBytes":684388316,"note":"Holiday",
 "files":[
   {"fid":0,"path":["Holiday","IMG_0001.HEIC"],"size":2811004,"mtime":1791460800000,"mime":"image/heic"},
   {"fid":1,"path":["Holiday","clip.mp4"],"size":681574400,"mtime":1791460900000,"mime":"video/mp4"},
   {"fid":2,"path":["Holiday","notes.txt"],"size":2912,"mtime":1791460950000,"inBundle":true}],
 "dirs":[["Holiday","empty"]],
 "bundle":{"size":4608}}
```

- `id`: 16 random bytes, lowercase hex, made by the sender. *Choice:* persisted before sending, so retries are idempotent and resume survives restarts.
- `fid` is the index in `files`. `mtime` is Unix ms. No mode field: received files are never executable (SR-23).
- **Paths (SR-17)** are arrays of components, NFC-normalised. The whole manifest is rejected with `400 path_invalid` if any component is empty, `.` or `..`; contains `/`, `\`, NUL, C0/C1 controls, `<>:"|?*` or U+202A-202E, U+2066-2069; ends in a dot or space; is a Windows device name (CON, PRN, AUX, NUL, COM0-9, LPT0-9, including superscript digits, with or without extension); or exceeds 255 UTF-8 bytes; or if depth exceeds 32, a path exceeds 1,024 bytes, or two paths differ only by case. The same rules apply on every platform.
- Writes stay beneath the receive folder, never follow symlinks and never overwrite; collisions get " (n)" (SR-18).
- **No hashes in the manifest**, so a large file starts moving at once.
- `inBundle` files travel in the bundle in `fid` order; `bundle.size` is the exact tar length (7.7). *Choice:* senders SHOULD bundle files under 256 KiB; receivers accept any split.
- Zero-length files are never streamed; the receiver creates them on completion.
- Limits (SR-20): 100,000 entries and a 16 MiB body, else `413 too_large`. `totalBytes` must fit in free space minus the reserve in 6.4, else `507 no_space`, checked again on every write. At most 4 concurrent transfers per peer, else `429 rate_limited`.
- **Chunk size:** the sender proposes a power of two from 64 KiB to 16 MiB (default 1 MiB); the receiver's answer, same rules, binds for the transfer's life.

### 7.3 Offer responses and tokens

```json
201 Created
{"id":"c1d2...f0","state":"accepted","chunkSize":1048576,
 "files":[{"fid":0,"token":"mY0...Q","verifiedOffset":0},{"fid":1,"token":"Tq7...a","verifiedOffset":0}],
 "bundle":{"token":"b4K...w","verifiedOffset":0}}

202 Accepted
{"id":"c1d2...f0","state":"pending","expiresAt":1791461400000}
```

- `201` when auto-accepted. `202` when waiting for the user; the decision arrives as a `transfer` frame. A pending offer expires after 10 minutes. *Choice:* LocalSend's 30-second expiry is a documented complaint.
- A known `id` with the same manifest returns `200` with the `GET` body; a changed manifest gets `409 conflict`. `403 declined` refuses outright.
- **Tokens (SR-13):** each accepted file and the bundle get 16 random bytes, base64url, sent as `pd-token` on each `PUT`, bound to the offering Device ID and valid only on its connections. Otherwise `401 bad_token`. A deselected file is `skipped` with no token.

### 7.4 Streaming a file

```
PUT /v1/transfers/c1d2...f0/files/1
content-type: application/vnd.playdrop.chunks
content-range: bytes 0-681574399/681574400
pd-token: Tq7...a
pd-chunk-size: 1048576
```

Body: one record per chunk, nothing else.

```
[ chunk bytes (chunkSize, last may be short) ][ BLAKE3(chunk bytes), 32 bytes ] ...repeated
```

- `content-range` is the **file** range carried (inclusive end, total = declared size). The start MUST equal the receiver's verified offset, which is chunk-aligned, or 0 to restart the file (7.6); anything else gets `409 offset_mismatch` (SR-21). The end MUST be within the declared size; bytes past it are rejected. Body length = `(end - start + 1) + 32 * chunks`.
- The sender hashes each chunk as it reads it: no pre-read, no second pass. The receiver verifies each on arrival, writes it to `<name>.part` and advances the verified offset; a cut request keeps whole verified chunks only.
- One HTTP/2 request per file, many in parallel. A range ending at end of file carries the trailer `pd-root: <hex root>`.

**Root and prefix hashes:**
```
root   = BLAKE3( "playdrop-root-v1"   || u64(fileSize)       || u32(chunkSize) || h_0 || ... || h_last )
prefix = BLAKE3( "playdrop-prefix-v1" || u64(verifiedOffset) || u32(chunkSize) || h_0 || ... || h_(k-1) )
```
`h_i` are the chunk hashes in order. Both sides compute these from stored chunk hashes without re-reading the file. *Choice:* a flat two-level tree is easy everywhere and survives resume; it is not `b3sum` of the file.

```json
200 OK
{"fid":1,"state":"verified","verifiedOffset":681574400,"root":"8a4f...e2"}
```
A partial range returns `"state":"receiving"` and the current offset. `.part` is renamed only after the root verifies.

**Integrity failures (SR-22):**
- `422 chunk_hash_mismatch`: the receiver stops reading, replies with `verifiedOffset` at the bad chunk and resets the stream (`RST_STREAM NO_ERROR`). The sender re-sends from there. A chunk is re-sent at most 3 times; on the next failure the file is `failed`, its `.part` is deleted and the user sees "failed integrity check from <label>".
- `422 root_mismatch`: handled the same way, with no retry: the file is `failed` and its `.part` deleted.
- `410 cancelled`: `PUT` on a cancelled transfer.

### 7.5 Progress and cancel

```json
GET /v1/transfers/c1d2...f0
200 OK
{"id":"c1d2...f0","state":"active","chunkSize":1048576,"totalBytes":684388316,"bytesVerified":312143836,
 "files":[{"fid":0,"state":"verified","size":2811004,"verifiedOffset":2811004,"root":"11ab...90","token":"mY0...Q"},
          {"fid":1,"state":"receiving","size":681574400,"verifiedOffset":309329920,"prefix":"e03c...51","token":"Tq7...a"}],
 "bundle":{"state":"verified","size":4608,"verifiedOffset":4608,"token":"b4K...w"}}
```
Transfer `state`: `pending`, `accepted`, `active`, `complete`, `declined`, `cancelled`, `failed`, `expired`. File `state`: `waiting`, `receiving`, `verified`, `skipped`, `failed`. `prefix` is present when `verifiedOffset` is above 0 and below `size`.

```
DELETE /v1/transfers/c1d2...f0      {"reason":"user"}      ->  204
```
`reason`: `user`, `source_changed`, `error`. The receiver deletes `.part` files. A receiver that cancels sends a `transfer` frame.

### 7.6 Resume (SR-21)

- The sender records each source's size, mtime and inode or file ID at offer time, and keeps every chunk hash it sends.
- After a drop it calls `GET /v1/transfers/{id}` and, per unfinished file, compares `prefix` with its own recorded hashes and re-checks the source. All match: `PUT` from `verifiedOffset`, re-hashing only from there. Prefix, mtime or inode differs, or its hashes are lost: `PUT` from 0, and the receiver discards that `.part`. Size differs: cancel with `source_changed` and re-offer, since the size was part of what was accepted.
- **Durability:** the receiver persists a verified offset and its chunk hashes only after the data is flushed (`fdatasync`), batched every 16 MiB or 1 s, and on restart truncates each `.part` to it.
- Both sides persist ID, manifest and file states. A `.part` idle for 24 h is deleted and its transfer `failed` (SR-20). When all files are `verified` or `skipped`, the transfer is `complete` and the user is notified.

### 7.7 Bundle

`PUT /v1/transfers/{id}/bundle` uses the same headers, records, range, trailer, root, prefix, errors and resume rules as a file. The stream is a deterministic POSIX ustar archive, so a sender can regenerate any offset:

- Members are the `inBundle` files in `fid` order. **Each member's name is its decimal `fid`** (`"2"`); the receiver takes the path from the manifest, never from the archive. *Choice:* names in the tar carry no meaning, so zip-slip and long-name pax headers cannot arise.
- Regular files only; `uid`/`gid` 0, empty `uname`/`gname`, mode 0644 (ignored on receipt), `mtime` = manifest `mtime` in whole seconds. No pax or GNU extension headers.
- Two 512-byte zero blocks end it; `bundle.size` is the exact total.

The receiver extracts with its own loop under the 7.2 write rules, not a library unpacker. A member that is not a regular file, or differs from the manifest in name, order or size, fails the bundle with `422 bundle_mismatch` (SR-19). Resume mid-member appends to its `.part`.

### 7.8 Outbox

Any host may hold an outbox for a peer that cannot listen, such as a backgrounded iPhone. The holder serves; the puller is the receiver and keeps the resume state.

```json
GET /v1/outbox
200 OK
{"items":[{"id":"c1d2...f0","files":2,"totalBytes":684388316,"note":"Holiday","createdAt":1791460990000}]}
```
Only items queued for the caller's Device ID are listed or served. `GET /v1/outbox/{id}` returns the 7.2 manifest with the holder's `chunkSize`.

```
GET /v1/outbox/c1d2...f0/files/1
range: bytes=309329920-
pd-prefix: e03c...51
```
- Body: 7.4 records plus a `pd-root` trailer. A chunk-aligned `range` whose `pd-prefix` matches the holder's hashes, with the source unchanged, gives `206`; otherwise `200` from 0 and the puller discards its `.part`. Misaligned ranges get `416 range_invalid`.
- The puller verifies as in 7.4 (SR-22), then sends `DELETE /v1/outbox/{id}`. Unpulled items expire after 7 days. The puller's own `files` grant decides acceptance.

---

## 8. Discovery

mDNS, the beacon and last-known dial run concurrently; manual IP and QR are user actions; there is no subnet scan.

### 8.1 Private adverts (SR-4)

Outside "Add device", adverts carry no name, type, Device ID or port, only:

| Key | Value |
|---|---|
| `v` | highest protocol version, a hint only (SR-12) |
| `n` | 16 random bytes, lowercase hex |
| `t` | tag, 16 bytes, lowercase hex (8.2) |

During an open "Add device" window (at most 120 s) adverts also carry `name` (sanitised, at most 63 bytes) and `type`, and the beacon carries `port`.

**mDNS.** Service `_playdrop._tcp.local.`. The instance name is 16 random hex characters, and it and `n` are replaced on every network change and every 15 minutes. TXT: `v=1 n=4c1f... t=9ae0...`; TXT records over 400 bytes are dropped unparsed. iOS lists `_playdrop._tcp` in `NSBonjourServices`; Android holds the `MulticastLock` only during discovery windows.

**Beacon.** One JSON object per UDP datagram to `239.255.42.187:42187`, at most 512 bytes, `n` fresh per datagram:

```json
{"t":"9ae0...71","v":1,"n":"4c1f...b2"}
{"t":"07d3...e8","v":1,"n":"a95e...0c","name":"Pop desktop","type":"desktop","port":42187}
```
The second form is for "Add device" only. TTL 1, on every up, non-loopback IPv4 interface. *Choice:* same-link protocol; IPv6 relies on mDNS and last-known dial in v1. Sent at 0, 1 and 3 s after start or a network change, then every 60 s on desktop; mobile hosts send only during discovery windows.

### 8.2 Announce key (SR-5)

Each device holds one 256-bit announce key K, stored like its private key (SR-28).

```
t = first 16 bytes of HMAC-SHA256(K, "pd-announce-v1" || n)      (n as its 16 raw bytes)
```

```json
{"t":"announce_key","v":1,"kid":7,"key":"m3V9...Qw"}
```
- `key` is K, base64url; `kid` rises by 1 per rotation. Sent only over a paired session: after pairing, at every session start and after a rotation. The receiver keeps the highest `kid` only.
- **Rotation:** any unpair creates a new K, pushed to every connected paired peer, with no grace period. Offline peers return through last-known dial, or are found by the rotating device, whose copies of their keys are unchanged.
- Matching an advert means computing `t` with each pinned peer's K; cheap with few pins.

### 8.3 Discovery rules (CVE-2020-26164, SR-1 to SR-3)

1. **No dial from an unverified advert.** A dial happens only if the tag verifies against a pinned peer's K, only to the packet's source IP and the peer's stored port, never to a payload address or a multicast, broadcast or loopback address, and at most once per peer per 5 s. Named "Add device" adverts are only listed until the user picks one.
2. **Never answer a beacon.** mDNS packets from off-link sources are dropped.
3. **Bounded input:** 5 packets per second per source IP, 200 overall, the rest dropped unparsed. Advert cache: 256 entries, 120 s TTL, LRU. "Add device" list: 32 entries, at most 10 new per minute.
4. **Names are display only**, sanitised, shown with the Short ID once known; pairing prompts use the authenticated `hello` name.
5. Malformed datagrams and the host's own adverts are dropped silently.

---

## 9. Error codes

| Code | HTTP | Meaning |
|---|---|---|
| `bad_request` | 400 | Malformed JSON or missing field |
| `path_invalid` | 400 | Manifest path breaks 7.2 |
| `bad_token` | 401 | Missing or wrong `pd-token` |
| `not_paired` | 403 | Needs a paired connection |
| `forbidden` | 403 | Grant missing for this device |
| `declined` | 403 | Offer declined |
| `not_found` | 404 | Unknown ID, or an ID bound to another device |
| `conflict` | 409 | Second session stream, or changed manifest under a known ID |
| `offset_mismatch` | 409 | Range start is neither the verified offset nor 0 |
| `cancelled` | 410 | Transfer was cancelled |
| `expired` | 410 | Offer, blob or outbox item expired |
| `too_large` | 413 | Manifest over limits |
| `range_invalid` | 416 | Range not chunk-aligned or past end |
| `chunk_hash_mismatch` | 422 | A chunk failed its BLAKE3 check |
| `root_mismatch` | 422 | Whole-file root differs |
| `bundle_mismatch` | 422 | Tar member differs from manifest |
| `rate_limited` | 429 | Rate or concurrency limit exceeded |
| `internal` | 500 | Unexpected failure |
| `no_space` | 507 | Would cross the free-space reserve |
| `source_changed` | - | Cancel reason: source size changed |
| `unsupported_version` | - | `hello.protocolVersion` differs from ALPN; close |
| `unsupported_frame_version` | - | Frame `v` too new |
| `identity_mismatch` | - | `hello.deviceId` differs from certificate; close |
| `duplicate_connection` | - | GOAWAY debug data (3.3) |
| `timeout`, `commit_mismatch`, `qr_proof_invalid`, `qr_expired`, `busy` | - | `pair_reject` reasons |

Session codes travel in `error` frames, HTTP codes in the 7.1 error body. Oversized, malformed and out-of-sequence frames close the connection without an error frame.

---

## 10. Implementation notes

- **Connection limits (SR-15):** 16 handshakes in progress, 10 s handshake timeout, 4 connections per source IP, 64 in total.
- **HTTP/2 limits (SR-15):** `MAX_CONCURRENT_STREAMS` 128; `MAX_HEADER_LIST_SIZE` 16 KiB, with CONTINUATION frames counted against it; over 100 stream resets per second closes the connection. Pin an `h2` release with the 2023 rapid-reset and 2024 CONTINUATION fixes.
- **HTTP/2 windows:** `h2` defaults (64 KiB) cap throughput. Set `INITIAL_WINDOW_SIZE` to 8 MiB and the connection window to 32 MiB on both sides; `MAX_FRAME_SIZE` 64 KiB; send `WINDOW_UPDATE` at half-window.
- **Socket buffers:** target 4 MiB each way. On Linux, setting `SO_RCVBUF`/`SO_SNDBUF` disables autotuning, so set them only if the `tcp_rmem`/`tcp_wmem` maximum is lower. Enable `TCP_NODELAY`.
- **Crypto:** prefer AES-128-GCM when both ends have AES hardware (about 4x faster on mid-range phones); a client without it lists ChaCha20-Poly1305 first and servers honour the client's order.
- **No per-file connections.** Default concurrency is 4 file `PUT`s plus the bundle; more streams do not help on Wi-Fi.
- **I/O:** hash in chunk-sized buffers, write each hash with its chunk, preallocate `.part` files, batch session frames per TLS write. Linux kTLS with `sendfile` MAY be used.
- **Windows firewall:** the program-scoped rule allows any remote address, so VPN peers work; mutual TLS with pinning is the access control.
- **Phase 1 gate (measured 2026-10-08):** tuned HTTP/2 ran about 27% slower than raw framing over the same TLS socket on loopback (about 2,080 vs 2,845 MiB/s). Both exceed any Wi-Fi link by more than an order of magnitude, so HTTP/2 stays for v1. The real question is CPU cost per byte on a phone, which loopback does not measure. Phase 3 measures it on Android; if it costs noticeable battery, a `playdrop/2` ALPN moves bulk data to raw framing and keeps these JSON shapes.

---

## 11. Deviations from the threat model

| SR | This spec | Why |
|---|---|---|
| SR-7 | SAS uses BLAKE3 over sorted Device IDs and full SPKIs, 20 bits mod 10^6; commit is BLAKE3 | SAS inputs and hash were fixed before the threat model. Every protective element is kept (commit-then-reveal, fresh nonces, handshake keys, TLS exporter); strength is the same, about 10^-6 per attempt |
| SR-4 | "Add device" beacons also carry `port` | Needed for the port override; the port is visible in every TCP connection and the mDNS SRV record anyway |
| SR-21 | A changed source **size** cancels with `source_changed` instead of restarting at 0 | The size was accepted and checked against free space, so it needs a new offer. Mtime, inode and prefix changes restart at 0 as SR-21 says |
| SR-14 | A frame lacking its grant is dropped, but one `forbidden` error per type per connection MAY be sent | Lets the sender hide the feature and say why (PLAN.md anti-pattern 18) |

### 11.1 Implementation notes (Phase 1 code)

Where the Phase 1 code (`crates/playdrop-core`) differs from or settles a gap in this document. Each is open for review before the core API freeze.

- **Requests flow only from the dialler (3.1, 3.3, 7).** HTTP/2 lets only the client open streams, so a host sends requests only on a connection it dialled. A pair may hold two connections, one dialled by each side. The session stream lives on one of them: the one dialled by the lower Device ID when both carry one. A second session on a non-preferred connection gets `409 conflict` and that connection stays open for requests; a preferred one replaces the live session, whose stream is ended without closing its connection. A newer connection from the same dialler replaces the older one.
- **Ending a session stream does not close its connection (5.1)**, because the connection may still carry requests (bullet above). Protocol errors still close it.
- **No `duplicate_connection` debug data (3.3).** The `h2` crate cannot attach GOAWAY debug data; the server sends GOAWAY `NO_ERROR` and the client simply closes the socket.
- **Equal keep-alive intervals (3.4):** the dialler pings.
- **No `transfer` frame for an expired offer (5.13, 7.3):** its state has no `expired` value; `GET` reports `expired`.
- **`PUT` on an offer that is not accepted (7.1):** pending gets `409 conflict`, declined gets `403 declined`, expired `410 expired`, cancelled `410 cancelled`.
- **Sender persistence (7.6) is receiver-only for now:** a restarted receiver resumes; a restarted sender re-offers.
- **"Removed by peer" (5.4)** is reported to the UI but not yet stored in the pin.
- **Benchmark (section 10, last bullet):** on loopback, tuned HTTP/2 measured about 25 to 30% below raw framing on the same TLS socket. Not acted on; the Phase 1 review decides.
- **"Removed by peer" is now stored (5.4, Phase 3):** the pin keeps a `removedByPeer` flag until the next successful pairing (a fresh pin) or unpair; this replaces the bullet above.
- **Text blobs carry normalised newlines (5.7, 5.15):** a long text clip is stored with CRLF and lone CR converted to LF, so the blob `id` (BLAKE3 of the bytes) equals the clip `hash`. The fetcher rejects a blob whose `id` differs from `hash`, whose body exceeds the announced `size`, or whose bytes do not hash to `id`.
- **Blob expiry answer (5.15):** an expired blob is answered `410 expired` for 30 minutes after its 5 minutes end, then `404 not_found`; the holder keeps at most 64 MiB of blobs and evicts the oldest first.
- **Outbox source changed (7.8):** when a queued source changed since it was queued (size, mtime or file ID), the holder cannot serve it at all and answers `410 expired` with the message "source changed since it was queued".
- **Outbox decline (7.8):** a puller that declines (or cancels) a pulled item still sends `DELETE /v1/outbox/{id}`, so the holder stops announcing it at every session start.
- **Media state goes only to peers with the sender's `media` grant (5.9):** what is playing is private, so the grant gates sending as well as receiving.

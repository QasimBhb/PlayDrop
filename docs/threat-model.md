# PlayDrop threat model

Version 1, 2026-10-08. Covers protocol v1 and the v1 feature set in PLAN.md. Phase tags P0-P6 are the "Build phases" in PLAN.md; P0 means the rule must be written into `docs/protocol.md` before code.

**Baseline: the LAN is hostile.** The user is often on guest, campus, hotel, mesh and VPN networks, so every other host on the segment is treated as an attacker.

Modelled facts: TCP 42187 with TLS 1.3 mutual auth and HTTP/2; UDP 42187 beacon to 239.255.42.187; mDNS `_playdrop._tcp`; Ed25519 self-signed certificates; Device ID = SHA-256 of the SubjectPublicKeyInfo (SPKI); pinning by Device ID; 6-digit SAS or QR with a one-time secret; per-pinned-device auto-accept; unattended desktop daemon; Android `connectedDevice` service; iOS foreground only; automatic desktop clipboard sync; remote input injected on the desktop.

---

## 1. Assets and trust boundaries

**Assets, most valuable first:** (1) private keys and the pin store; (2) control of the desktop through remote input; (3) clipboard contents (passwords, 2FA codes); (4) files in transit, in the receive folder, and everything outside it; (5) grants and auto-accept settings; (6) device identity and presence on the LAN; (7) daemon availability.

```
  +------------------- PHONE (OS sandbox) -------------------+
  |  other apps --intents / URL scheme--> [B5]               |
  |  Compose / SwiftUI UI --UniFFI, in-process--> core host  |
  |  Keystore / Keychain [B6]            (FGS or iOS app)    |
  +--------------------------------|-------------------------+
                                   | [B1] TLS 1.3 mTLS, TCP 42187
          [B2] UDP beacon, mDNS    |  (cleartext discovery)
  =================================|============ LAN (hostile) =====
     A1 passive sniffer    A2 active attacker (ARP/mDNS spoof, rogue AP)
     A3 malicious paired device (holds a valid pinned key)
  =================================|=================================
  +------------------- DESKTOP (one OS user) ----------------+
  |  playdropd: socket, keys, pin store, grants              |
  |     ^ [B3] D-Bus session bus / named pipe                |
  |     |      <-- other same-user processes, Flatpak apps   |
  |  Slint UI + tray                                         |
  |  playdropd --[B4]--> receive folder, clipboard, input    |
  |  key file / DPAPI blob [B6]                              |
  |  ........ [B7] other local OS users (shared PC) .........|
  +----------------------------------------------------------+
```

| Boundary | Trust rule |
|---|---|
| B1 Network session | Trusted only when the peer's SPKI hash matches a pin, then limited by that peer's grants |
| B2 Discovery | Never trusted. Hints only; cannot cause a dial, a prompt or unbounded state |
| B3 Local IPC | Same OS user only; sandboxed callers rejected |
| B4 Effects | Each effect needs a grant and is confined (receive folder, unlocked session) |
| B5 Phone entry points | Only the user-facing share target is exported |
| B6 Storage at rest | OS-protected per platform, never in backups |
| B7 Other OS users | Separate keys, folders and IPC; a port collision is DoS only |
| B8 Relay (only with the `iroh` build feature, a named relay and the `remote` grant) | Never trusted. Carries encrypted packets and learns who talks to whom, when and how much |

---

## 2. Attacker profiles

| ID | Attacker | Can | Cannot, by design |
|---|---|---|---|
| A1 | Passive LAN sniffer | Read multicast, mDNS, TLS metadata | Read TLS content; learn names outside pairing mode |
| A2 | Active LAN attacker (ARP/mDNS spoof, rogue AP, forged beacons) | Inject, drop, redirect, MITM, flood | Impersonate a pinned peer; force a dial to a third party; downgrade |
| A3 | Malicious paired device (stolen phone, compromised peer) | Use a valid key and its grants | Escape the receive folder; type while locked or without the input grant |
| A4 | Malicious app on the same phone or desktop | Call exported components and IPC | Read Keystore or Keychain keys; call the daemon from a Flatpak sandbox |
| A5 | Other local user on a shared desktop | Run processes, bind free ports | Read the victim's keys, files or IPC |
| A6 | Physical access to an unlocked device | Anything the UI allows | Pair silently, or pair while the desktop is locked |

---

## 3. Threat table

L = likelihood on a hostile LAN. Mitigations cite section 4.

| # | STRIDE | Threat | Entry | Impact | L | Mitigation | Residual | Phase |
|---|---|---|---|---|---|---|---|---|
| **Discovery (CVE-2020-26164 classes)** | | | | | | | | |
| T1 | S | Name spoofing in prompts ("Qasim's Pixel") | B2 | User pins attacker | H | Prompts only in pairing mode; SAS, short ID, local labels (SR-6, SR-8) | User ignores SAS | P1, P2 |
| T2 | T | Forged beacon makes the daemon dial any IP:port | B2 | Scan or reflection via victim | H | Dial only on a pinned peer's tag, to source IP (SR-1, SR-5) | Replay causes one failed, rate-limited dial | P1 |
| T3 | D | Broadcast amplification with spoofed source | B2 | Victim as reflector | M | Never reply; TTL 1; off-link drop (SR-3) | Standard mDNS replies, link-local | P1 |
| T4 | D | State exhaustion from unique-ID floods | B2 | CPU and memory exhaustion | H | Size, rate, cache caps (SR-2) | Discovery slow during flood; last-known dial works | P1 |
| T5 | I | Name, type and stable ID visible to the LAN; tracking across networks | B2 | Profiling, targeting | H | Anonymous rotating adverts (SR-4, SR-5) | "A PlayDrop device is here" | P0, P1 |
| T6 | I | Unpaired ex-peer keeps tracking | B2 | Tracking | M | Rotate announce key on unpair (SR-5) | Current peers can track | P1 |
| **Pairing** | | | | | | | | |
| T7 | S | MITM during SAS pairing | B1 | Attacker pinned both ways | M | SAS bound to SPKIs, nonces, TLS exporter (SR-7) | 10^-6 per attempt, 3 attempts | P0, P1 |
| T8 | S | SAS brute force by grinding keys offline | B1 | Silent MITM | M | Commit-then-reveal (SR-7) | As T7 | P0, P1 |
| T9 | S | Pairing fatigue: repeated unsolicited requests | B1 | Attacker pinned | H | No prompt outside pairing mode; no default Accept (SR-6, SR-8) | SAS still protects | P1, P2 |
| T10 | S | QR secret intercepted (shoulder surf, screen share, photo) | QR | Attacker pairs as scanner | M | 120 s, single use, capture-blocked, displayer confirms (SR-9) | Live race within 120 s, caught by confirm | P1, P2 |
| T11 | S | Attacker QR shown to the user's phone | QR | User pins attacker | L | SPKI must equal QR Device ID; confirm (SR-9) | Deliberate scan | P3, P4 |
| T12 | S | Pin bypass on key rotation ("same device, new key") | B1 | Silent re-pin | M | New key = new device (SR-10) | Re-pair after reinstall | P1 |
| T13 | E | A6 pairs a device on an unlocked, unattended desktop | UI | Persistent access | L | No pairing while locked; pairing notice and date (SR-6, SR-8) | Unlocked unattended PC | P2 |
| **Transport and session** | | | | | | | | |
| T14 | T | Version downgrade by rewriting discovery `v` | B2 | Weaker behaviour | M | Version only via ALPN, minimum enforced (SR-12) | None | P0, P1 |
| T15 | T | TLS downgrade; resumed session after pin revoked | B1 | Weak channel, revoked peer back | L | TLS 1.3 only, no tickets, no 0-RTT (SR-11) | Slightly slower reconnect | P1 |
| T16 | T | Replay of clipboard, input or find frames | B1 | Repeated actions | L | No 0-RTT; sequence numbers; bound tokens (SR-11, SR-13, SR-16) | None known | P1 |
| T17 | I | SNI or certificate leaks identity | B1 | Tracking | M | No SNI; TLS 1.3 hides certs (SR-11) | Size and timing analysis | P1 |
| T18 | D | Handshake floods, slowloris, HTTP/2 rapid reset or CONTINUATION floods on 42187 | B1 | Daemon down | H | Connection, stream, frame limits (SR-15) | A2 can jam the LAN | P1 |
| T19 | E | Peer B reads or cancels peer A's transfer or outbox (BOLA) | B1 | Theft, sabotage | M | IDs bound to Device ID (SR-13) | None | P1 |
| T20 | D | Another local user or instance squats the port | B7 | DoS | L | Exclusive bind, diagnostics; squatter has no key (SR-31) | DoS only | P2 |
| **Transfer engine** | | | | | | | | |
| T21 | E | Auto-accept path traversal: `../`, `\`, NUL, CON/COM1, trailing dot, `:` streams, overlong or bidi names (`gpj.exe`) | Manifest | Overwrite `~/.bashrc` or Startup, then code execution | H for A3 | Component rules; reject manifest (SR-17) | None known | P1 |
| T22 | E | Symlinks, hardlinks, devices or zip-slip names in the tar bundle; planted symlink in folder | Bundle | Write outside folder | H for A3 | Files and dirs only; beneath-only writes (SR-18, SR-19) | None known | P1 |
| T23 | D | Disk exhaustion: huge manifests, lying sizes, millions of entries, abandoned `.part` | Manifest | Disk full | M | Limits and cleanup (SR-20) | Fill up to reserve | P1 |
| T24 | T | Resume offset lies, holes, overlaps; source changed between sessions | Range | Spliced file accepted | M | Exact-offset rule, prefix hash (SR-21) | None | P1 |
| T25 | T | Hash mismatch kept, retried forever, or promoted | Chunk | Corrupt file, loop | L | Bounded retry, no promotion (SR-22) | None | P1 |
| T26 | E | Auto-accepted malware opened from "Open" | Receive | One-click execution | M | Mark of the Web, no exec bit, open folder (SR-23) | User runs it | P2, P3 |
| **Clipboard and remote input** | | | | | | | | |
| T27 | I | Auto sync sends passwords and 2FA codes to the phone, then into keyboard history or cloud | B4 | Credential theft | H | Toggles; password-manager hints; `EXTRA_IS_SENSITIVE` (SR-24) | Managers that set no hint | P2, P3 |
| T28 | T | Pastejacking: A3 pushes `curl x \| sh` | B1 | Command on paste | M | Clipboard-in grant, notice, cap (SR-14, SR-24) | User pastes in terminal | P2 |
| T29 | E | Stolen or malicious phone types into the PC at 1,000 keys/s | B1 | Desktop compromise | M | Grant off by default, active session, indicator, kill switch, rate cap (SR-25) | Granted attacker while unlocked | P5 |
| T30 | E | Input into the lock screen (password guessing) or UAC and elevated windows | B4 | Unlock or elevation | M | No input when locked, fail closed; never elevated (SR-25) | None known | P5 |
| T31 | T | Stuck modifiers after a drop | B4 | Unintended actions | L | Release all on end (SR-25) | None | P5 |
| **Local IPC, entry points, storage, logs** | | | | | | | | |
| T32 | E | Linux: any process or Flatpak app calls Pair, SetGrant, EnableInput, or "send file at path" | B3 | Silent pairing, host file exfiltration | M | UID check, Flatpak rejection, fds only (SR-26) | Unsandboxed same-user malware | P2 |
| T33 | E | Windows: default pipe DACL, pipe squatting, remote clients | B3 | Others drive daemon; UI talks to impostor | M | SID-named pipe, explicit DACL, both ends verify (SR-27) | Same-user malware | P2 |
| T34 | E | Android: another app fires our share or notification-action intents to send silently | B5 | Exfiltration, malware drop | M | Unexported components; visible send card (SR-29) | User misses a 2 s card | P3 |
| T35 | E | iOS: any app or page opens `playdrop://` to act | B5 | Silent action | L | Scheme carries no commands (SR-30) | None | P4 |
| T36 | I | Key theft at rest or via Auto Backup, iCloud, roaming profile | B6 | Permanent impersonation | M | Platform storage, backup exclusion (SR-28) | Root or same-user desktop malware | P1-P4 |
| T37 | I | Logs leak clipboard, names, tokens, SAS, QR secrets | Logs | Leak via bug reports | H | Redaction, canary test (SR-32) | Debug builds | P1 |
| T38 | I | Another local user reads the receive folder | B7 | Disclosure | L | 0700 folder (SR-23) | Admin or root | P2 |
| T39 | T | Parser bugs before authentication | B1, B2 | Crash, corruption | M | Caps, fuzzing (SR-33) | Dependency bugs | P1, P6 |
| **Reach across networks (only with the `iroh` feature)** | | | | | | | | |
| T40 | I | The relay records who talks to whom, when and how much | B8 | Metadata leak, and a stable ID to track | M | No default relay, user names it, statement to the user (SR-35, SR-39) | Whoever runs the named relay | P12 |
| T41 | S | A relay or stranger forges an Endpoint ID or redirects a dial | B8, B1 | Impersonation | M | Inner TLS against the pin decides trust; the ID decides nothing (SR-37) | None | P12 |
| T42 | E | A stranger on the internet reaches the pairing code or session plane | B8, B1 | Pairing from afar, new attack surface | H | Pinned and granted endpoints only, closed before inner TLS, no pairing over reach (SR-36) | Parser bugs in iroh | P12 |
| T43 | D | Connection or stream floods through the relay | B8 | Daemon down, memory | M | One connection and one stream per endpoint, SR-15 limits apply inside (SR-38) | A paired peer can still waste bandwidth | P12 |
| T45 | I, D | With `--reach-direct`, an endpoint holding only this host's Endpoint ID, or a paired peer, names an address and this host sends it UDP probes, showing its own address | B8, B1 | Reveals the device's address, small UDP reflection at a third party | M | Off by default, relay only has no IP socket (SR-35); the flag's help says what it exposes | Whoever turns on direct; the gate does not stop it. Measured by `only_a_host_that_asked_for_direct_probes_what_a_stranger_names` | P12 |
| T46 | D | A paired peer names another device's Endpoint ID before that device does, and this host keeps the first (protocol.md 12) | B1 | That other device cannot be reached over reach from this host until the first peer is unpaired | L | Accepted for the spike. `playdrop diagnose`, and `peers` in `playdrop host`, show a dropped frame. Dials that find the wrong key behind the ID back off, up to an hour | Proof of possession of the key would end this; `docs/design/iroh-reach.md` requires it before reach leaves spike status | P12 |
| T47 | D | A peer is away and a reach dial runs into its 10 s timeout | B8 | A send to that peer waits up to about 10 s for that dial's result after its own LAN dial (21 s seen in all, with a silent LAN address). LAN dials are not held up, and nothing piles up: one dial per peer runs at a time, outside the dial lock, on its own task, at most one per 15 s | L | Single-flight dial, proven by `dial_tasks_do_not_pile_up_behind_a_reach_dial` and `a_lan_dial_is_not_held_up_by_a_reach_dial` | One relay query per 15 s for an ID nobody holds (T46), as that looks the same as a peer that is away | P12 |
| T44 | I | Relay URLs or Endpoint IDs in logs and bug reports | Logs | Reveals a private relay and tracks a device | L | Debug only, redacted in diagnostics (SR-40) | Debug builds | P12 |
| **Notification mirroring (protocol.md 5.16)** | | | | | | | | |
| T48 | I | A stolen or compromised paired desktop reads the phone's notifications: one-time codes, private messages | B1 | Private messages and sign-in codes read | M | Three locks, each default off, and the sender's filter (SR-41); turning off or unpairing stops it at once (SR-42) | What the user allowed | P15 |
| T49 | I | Notification text ends up in logs or bug reports | Logs | Private messages in a file that gets shared | M | Never logged at any level, not in diagnostics (SR-43) | None known | P15 |
| T50 | D | A paired phone floods the desktop with notifications | B1 | Desktop buried in notifications, memory | L | Size limits, 30 `posted` per minute per connection, 20 shown per peer (SR-44) | A paired peer can still show 30 a minute | P15 |

---

## 4. Security requirements

Each item names its test. The tag is the phase that must pass it.

**Discovery**

- **SR-1 (P1)** A beacon or mDNS record causes a dial only if its tag verifies against a pinned peer's announce key. The dial goes only to the packet's source IP (never an address in the payload), never to multicast, broadcast or loopback, and at most once per peer per 5 s. *Test:* 10,000 forged beacons produce zero outbound SYNs.
  - *Path choice (protocol.md 3.5, Phase 11):* a verified advert for a peer already online can start a path re-check, which dials only that verified source and the pinned last-known addresses, at most once per peer per 60 s. It does not take the 5 s reconnect budget, so a reconnect right after a re-check is a second dial inside 5 s; both need a verified tag or an existing pin, and neither can repeat faster than its own limit.
- **SR-2 (P1)** Drop unparsed: beacons over 512 bytes, TXT over 400 bytes. Rate: 5 packets/s per source IP, 200/s total. Cache: 256 entries, 120 s TTL, LRU; pairing list 32. *Test:* 1,000,000 unique IDs grow memory under 5 MiB and use under 10% of one core.
- **SR-3 (P1)** Never answer a beacon. Beacons use `IP_MULTICAST_TTL=1`. Drop mDNS packets from off-link sources. *Test:* a spoofed-source beacon gets no packet back.
- **SR-4 (P0, P1)** Outside pairing mode, the mDNS instance name is 16 random hex characters rotated on every network change and every 15 minutes; TXT and beacon carry only `v` (hint), `n` (16-byte nonce) and `t` (tag). Pairing mode (at most 120 s) adds name and type. *Test:* a 30-minute idle capture shows no name and no identifier repeated across rotations.
- **SR-5 (P0, P1)** Each device has a 256-bit announce key K, sent to peers over the authenticated session at pairing. `t = first 16 bytes of HMAC-SHA256(K, "pd-announce-v1" || n)`. Any unpair rotates K and pushes it to remaining peers. *Test:* a removed device cannot match new beacons.

**Pairing and identity**

- **SR-6 (P1, P2)** Pairing requests are rejected with no UI unless the user opened "Add device". The mode ends after 120 s, on success, or after 3 failures, and cannot start while the desktop is locked. *Test:* an unsolicited request gets a closed connection and no prompt.
- **SR-7 (P0, P1)** Commit-then-reveal SAS: the initiator sends `SHA-256(nonce_i)` before seeing `nonce_r`. `SAS = SHA-256("pd-sas-v1" || SPKIhash_i || SPKIhash_r || nonce_i || nonce_r || exporter("EXPORTER-playdrop-pair", 32)) mod 10^6`, over the SPKIs seen in the handshake, with fresh nonces per attempt. *Test:* a MITM proxy always yields mismatched SAS; a wrong reveal aborts.
- **SR-8 (P2, P3, P4)** Prompts show SAS, a short ID (first 16 base32 characters of the Device ID) and the name with controls and bidi overrides stripped, at most 64 characters. Both screens must confirm; no button is focused by default. After pairing, a user-editable local label is shown and peer renames never change it. A new pairing raises a desktop notification; the device list shows pairing date and last seen. *Test:* a U+202E name renders stripped.
- **SR-9 (P1, P2)** QR holds the full Device ID, addresses, port, a 128-bit secret and an expiry of at most 120 s. The first attempt consumes the secret. The scanner checks the server SPKI hash equals the Device ID before sending anything, then proves the secret with `HMAC-SHA256(secret, both SPKI hashes || exporter)`. The QR screen uses `FLAG_SECURE` (Android) and `WDA_EXCLUDEFROMCAPTURE` (Windows). The displayer shows the new device and needs one tap before pinning. *Test:* reuse fails; a screenshot is blank.
- **SR-10 (P1)** A known name or address with a new key is an unpaired device. No "key changed, accept?" path and no pin migration in v1. *Test:* after key regeneration, old pin and grants are untouched and the device appears as new.

**Transport**

- **SR-11 (P1)** rustls: TLS 1.3 only, Ed25519 only, client auth required, a verifier accepting only pinned SPKI hashes (any key only in pairing mode, followed by SR-7), no tickets or resumption, no 0-RTT, ALPN `playdrop/1` required, IP-literal server name so no SNI is sent. *Test:* TLS 1.2, other key types, no client cert, early data and missing ALPN each fail.
- **SR-12 (P0, P1)** Version is negotiated only by ALPN; discovery `v` is a hint; a minimum is enforced; no fallback to older or unauthenticated modes. *Test:* rewriting TXT to `v=0` changes nothing.
- **SR-13 (P1)** Every request and frame is checked against the authenticated Device ID's grants. Transfer IDs and file tokens are 128-bit random, bound to the creating Device ID, valid only on its connections. The outbox returns only the caller's items. *Test:* peer B touching peer A's transfer gets 404.
- **SR-14 (P1, P2)** Per-device grants: files (auto-accept), clipboard in, clipboard out, input, media, find. Defaults: all on except input. Grants change only from the local UI. Unpair closes connections within 1 s and deletes grants and tokens. *Test:* a frame lacking its grant is dropped.
  - *Path choice (protocol.md 3.5, Phase 11):* unpair also closes at once a connection draining for its 10 s grace and any candidate a path check holds, before or after HTTP/2; a session opening at that moment is not placed, a dial finishing after it is not kept, and a connection that finished TLS before it and starts HTTP/2 after it is closed and serves nothing. A serving connection gets 200 ms for the `unpair` frame plus 300 ms for GOAWAY, so it is gone within the 1 s. *Test:* `tests/phase11_path.rs`.
- **SR-15 (P1)** Limits: 16 pending handshakes, 10 s handshake timeout, 4 connections per IP, 64 total; unpinned peers dropped after the handshake outside pairing mode. HTTP/2: 128 streams, 16 KiB headers, over 100 resets/s closes, CONTINUATION bounded, `h2` pinned at a version with the 2023 rapid-reset and 2024 CONTINUATION fixes. Session frames over 64 KiB close the connection. A path check still holds at most two alternatives per dial, and one found to be on a path already measured closes at its first, empty probe response (protocol.md 3.5). *Test:* slowloris, rapid reset and a 10,000-handshake flood leave a pinned session working.
  - *Path probe (protocol.md 3.5, Phase 11):* `GET /v1/probe` is served only on a pinned connection (`403 not_paired` otherwise, never in pairing mode), for at most 8 MiB (`400 bad_request` above), one probe in flight and 10 per minute per connection (`429 rate_limited` otherwise). The body is zeros, so serving costs only the TLS encryption, and a peer can make a host send at most about 80 MiB per connection per minute, no more than one transfer it could already request. A dialler holds at most two extra connections per check, so with the one in use it stays under the 4 per source address. On the serving side at most two connections per peer may be open without a request other than a probe (a third closes the oldest), and one still unused 20 s after its handshake is closed, so a pinned peer cannot fill the per-address and total limits with idle connections. *Test:* `tests/phase11_path.rs`.
- **SR-16 (P1)** Frames carry a per-connection sequence number; duplicates are dropped; unknown types ignored; malformed frames close the stream. *Test:* a duplicated clipboard frame has no effect.

**Transfer engine**

- **SR-17 (P1)** Manifest paths are arrays of components. Reject a component that is empty, `.` or `..`; contains `/`, `\`, NUL, C0/C1 controls or U+202A-202E, U+2066-2069; or exceeds 255 UTF-8 bytes. Reject depth over 32 and paths over 1,024 bytes. NFC-normalise. On every platform also reject `<>:"|?*`, trailing dot or space, and CON, PRN, AUX, NUL, COM0-9, LPT0-9 (including superscript digits) with or without an extension, plus case-insensitive duplicates. Any violation rejects the whole manifest. *Test:* a 200-name hostile corpus, all rejected, on Linux and Windows CI.
- **SR-18 (P1)** Writes go through a receive-folder handle: `cap-std` with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` on Linux; `FILE_FLAG_OPEN_REPARSE_POINT` plus a final-path check on Windows. Create exclusively; collisions get " (n)", never overwrite. `.part` is renamed only after the root hash verifies (a bundle member once its own chunks are verified and durable, see the Phase 8 note under SR-22). *Phase 9:* on Linux, Android and Apple the exclusive create is a rename that refuses an existing name (`RENAME_NOREPLACE`), so the staged file lands in one step and no empty placeholder can be left by a kill; Windows keeps the exclusive create followed by a rename onto it (protocol.md 11.1). *Test:* a planted symlink to `~/.bashrc` is not followed.
- **SR-19 (P1)** The bundle reader accepts only regular files and directories; symlinks, hardlinks, devices, FIFOs and PAX path overrides fail the bundle. Entries pass SR-17 and must match the manifest by name and size. Mode bits are ignored. Extraction is our own loop over SR-18, not `tar::Archive::unpack`. *Test:* zip-slip and symlink bundles write nothing outside the folder.
- **SR-20 (P1)** At most 100,000 entries and 16 MiB of manifest. Declared total must fit in free space minus max(2 GiB, 5%), checked at accept and on every write; ENOSPC fails cleanly. Bytes past a declared size are rejected. 4 concurrent transfers per peer. `.part` idle for 24 h is deleted. *Test:* a 1 PB manifest is refused before any write.
- **SR-21 (P1)** `Content-Range` must start exactly at the receiver's verified, chunk-aligned offset and end within the declared size. On resume the receiver sends the BLAKE3 hash of its verified prefix; the sender checks it against its recorded chunk hashes plus the source's size, mtime and inode or file ID. Any difference restarts that file at 0. *Test:* editing the source before resume yields a full re-send with a matching hash.
- **SR-22 (P1)** A failed chunk is re-requested at most 3 times, then the file fails, its `.part` is deleted and the user sees "failed integrity check from <label>". A final root mismatch is handled the same. *Test:* a fault-injecting sender produces no final file. *Phase 8 note:* bundle members are placed as soon as their own chunks are verified and durable, before the bundle root arrives (protocol.md 11.1). No unverified byte is ever placed: each chunk passed its BLAKE3 check on arrival, and the root is computed from those same chunk hashes, so it can only disagree when the sender's own state is inconsistent. A root mismatch still fails the bundle with the same message and deletes everything still staged; members already placed are kept, the rule the Android landing folder already followed in Phase 7. Order, names and sizes are still checked member by member against the manifest (SR-19). A streamed (non-bundle) file is still placed only after its root verifies.
- **SR-23 (P2, P3)** Windows: received files get `Zone.Identifier` with `ZoneId=3`. Linux: files 0600 in a receive folder created 0700, never executable. "Open" on executables, scripts, `.desktop`, `.lnk` and `.msi` opens the folder instead. *Test:* a received `.exe` triggers SmartScreen; a `.sh` is not executable.

**Clipboard and input**

- **SR-24 (P2, P3)** Auto-sync skips a clip if: Windows has `ExcludeClipboardContentFromMonitorProcessing`, `CanIncludeInClipboardHistory`=0 or `CanUploadToCloudClipboard`=0; Linux offers `x-kde-passwordManagerHint` = `secret`; Android `ClipDescription.EXTRA_IS_SENSITIVE` is true. Manual "Send clipboard" of such a clip asks first and sends `sensitive=true`. Received clips are written with history and cloud upload disabled on Windows, and `EXTRA_IS_SENSITIVE` on Android when flagged. Toggles per device and direction. Text over 1 MiB is not synced. *Test:* copying from KeePassXC (COSMIC) and Bitwarden (Windows) sends nothing.
- **SR-25 (P5)** Input is applied only when: the device holds the input grant (enabled only from the desktop UI); the user started an input session on the phone; the desktop is unlocked (`WTS_SESSION_LOCK` on Windows, logind `LockedHint` on Linux; unknown counts as locked); the session has been idle under 10 minutes. The desktop shows "Remote input from <label>" with Stop, plus a tray kill switch. Cap 60 key events/s. Release all keys and buttons when the session ends or drops. The daemon never runs elevated or with `uiAccess`; Linux uses only the RemoteDesktop portal. *Test:* locking mid-session stops input within 100 ms; a 1,000 keys/s burst is throttled.

**Local IPC and platform**

- **SR-26 (P2)** D-Bus: the daemon requires `GetConnectionUnixUser` to equal its UID and rejects callers whose `/proc/<pid>/root/.flatpak-info` names another app. Files are passed as fds (type `h`), never paths. The UI checks the name owner's UID and `/proc/<pid>/exe`. *Test:* other UIDs and Flatpak apps are refused.
- **SR-27 (P2)** Windows: pipe `\\.\pipe\playdrop-<user SID>` with `FILE_FLAG_FIRST_PIPE_INSTANCE`, `PIPE_REJECT_REMOTE_CLIENTS`, and a DACL of user SID and SYSTEM only. The daemon checks the client's token SID; the UI checks the server's SID and image path. *Test:* another user and an SMB client cannot connect.
- **SR-28 (P1 Linux, P2 Windows, P3 Android, P4 iOS)** Keys, announce key, pins and grants: Linux in `$XDG_DATA_HOME/playdrop/`, directory 0700, files 0600, refuse to start on looser modes; Windows DPAPI `CurrentUser` in `%LOCALAPPDATA%\PlayDrop` with a user-only ACL; Android wrapped by a non-exportable AES-256-GCM Keystore key (StrongBox when present, no user-auth so screen-off receive works), excluded in `dataExtractionRules`; iOS Keychain `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`. *Test:* `chmod 644` stops the daemon with a clear message; a backup holds no key.
- **SR-29 (P3)** Android services and receivers are `exported=false`; notification actions use explicit `FLAG_IMMUTABLE` PendingIntents. The share activity, the only exported component, shows "Sending to <label>" with Cancel for 2 s. *Test:* a broadcast from another UID to the action receiver fails.
- **SR-30 (P4)** iOS `playdrop://` only opens the app and drains the App Group queue; it takes no parameters. QR is scanned only in-app. *Test:* parameters are ignored.
- **SR-31 (P2)** Exclusive bind on 42187 (`SO_EXCLUSIVEADDRUSE` on Windows, no `SO_REUSEPORT` on Linux); on failure, diagnostics say the port is held. The Windows firewall rule is program-scoped. *Test:* a second user's daemon reports the conflict.

**Hygiene and assurance**

- **SR-32 (P1)** Never logged: private and announce keys, QR secrets, SAS, nonces, tokens, clipboard text, input events. File names at debug level only. Device IDs truncated to 8 characters. The path id of protocol.md 3.5 is a salted hash that says nothing about the interface name, goes only to pinned peers in the `200` to a `bytes=0` probe, and is not logged or put in reports. Log files 0600; diagnostics export redacts the same way. *Test:* CI seeds canary strings and greps all logs.
- **SR-33 (P1, gate P6)** `cargo-fuzz` targets for beacon, TXT, manifest, bundle and frame decoders run nightly; `#![forbid(unsafe_code)]` in parsers. P6 needs 8 CPU-hours per target with no crash. *Test:* fuzz report in the ship gate.
- **SR-34 (P6)** An external review checks every SR against release builds on the P6 matrix, including a hostile-LAN run with ARP spoofing and forged mDNS. *Test:* signed checklist.
- **SR-35 (P12)** Reach is off unless all three hold: the build has the `iroh` feature, the user named a relay, and both sides gave that device the `remote` grant (default off; grants change only from the local UI, SR-14). Without a named relay no endpoint exists and no host outside the LAN is contacted. By default the endpoint has no IP socket and speaks only to the relay; a direct path needs `--reach-direct`, and what that exposes is T45. No third-party address lookup, no public DNS fallback, and no port mapping on the router; only the relay this user named is contacted, so a peer naming another relay is not dialled. A daemon refuses to turn the grant on with no relay named, saying so, and `playdrop remote <peer> on` prints what the relay can see (SR-39, anti-pattern 18). *Test:* with no relay named no endpoint exists; in network namespaces a packet counter shows nothing went to any host but the relay and the peer.
- **SR-36 (P12)** A reach connection is accepted only from a stored Endpoint ID of a pinned peer holding the `remote` grant, and is closed before any inner TLS otherwise (the outer QUIC handshake has to finish first, for the ID to be known). The device the inner TLS authenticates must be one that ID was stored for. Only such IDs are dialled. Pairing never runs over reach, even with the "Add device" window open. *Test:* an unknown endpoint, an unpinned one, and a pinned one with the grant off are all refused; a pinned device on another peer's Endpoint ID is cut off after the inner handshake; a reach connection during pairing mode gets no pairing.
- **SR-37 (P12)** The Endpoint ID never decides trust. The inner TLS 1.3 mutual authentication against the pin (SR-11) and the grants (SR-13, SR-14) are the same code on this path. The endpoint key is derived from the device key with a separate context, so one key is not used in two protocols. *Test:* a peer with the right Endpoint ID and the wrong device key fails the inner handshake.
- **SR-38 (P12)** One QUIC connection and one bidirectional stream per endpoint: a newer connection from an endpoint replaces its older one only after the inner TLS and a first request, a second stream gets no credit, and an unknown ALPN is refused by QUIC. SR-15's limits apply to what the stream carries, with the synthetic address making the per-address limit per-peer. *Test:* extra streams are refused and a pinned session keeps working; a dial that fails its inner TLS does not cut the session its dialler already has.
- **SR-39 (P12)** The user is told plainly, wherever reach is turned on, that the relay can see which devices connect, when, and how much data moves, though not the contents; the Endpoint ID is stable until the device key changes. `playdrop peers` and `playdrop diagnose` say when a peer is on the relay or a direct path across networks, and that it is the slow path (anti-patterns 13, 18). *Test:* the status line is asserted in a test.
- **SR-40 (P12)** Relay URLs and Endpoint IDs appear only at debug level and are redacted from the diagnostics report (SR-32). iroh's own targets log both above that, so a host's log setup turns them off unless the filter names `iroh`. *Test:* added to the canary test, through a layer filtered the way hosts filter.
- **SR-41 (P15)** Notification mirroring needs all three locks, each default off: Android "Notification access" granted by a person, the per-device `notificationsOut` grant on the phone, and a per-app allow list on the phone (empty by default). Pairing never turns `notificationsOut` on, and only the phone's own UI changes it (SR-14). The phone never sends notifications from apps not on the allow list or from PlayDrop itself; ongoing and foreground-service notifications, group summaries, or anything with `VISIBILITY_SECRET`; anything a work profile or secure folder owns; content Android itself redacts goes as redacted; `VISIBILITY_PRIVATE` (Android's default, which only hides content on the phone's lock screen) is sent with its content: ticking the app on the allow list is the consent, and the app picker says so. *Test:* with any lock off nothing is sent; filtered kinds are never sent.
- **SR-42 (P15)** Turning the grant off, unpairing, or revoking notification access stops mirroring at once and removes every mirrored copy on the desktop for that peer (the phone sends no more `posted`, and on grant off sends `removed` for what it mirrored, which needs no grant as it carries no content; the desktop closes copies on unpair, offline and on its own grant off). Exception: on Windows a mirrored toast cannot be closed or counted yet, so copies stay in the notification centre until the user clears them (never run on a real PC; open TODO). *Test:* unpair closes the desktop copies; `removed` still arrives after the grant went off.
- **SR-43 (P15)** Notification app names, titles, text and keys are never logged at any level and are not in the diagnostics report (SR-32 canary extended). The key is opaque: the first 128 bits of BLAKE3 keyed by a per-process random salt over the Android key, which never leaves the phone. *Test:* the canary test seeds a notification and greps the logs.
- **SR-44 (P15)** `app` at most 64 bytes, `title` 256, `text` 1 KiB (over the limit: dropped with `error` `bad_request`), at most 30 `posted` frames per minute per connection (more are dropped), at most 20 copies shown per peer (the oldest closes). *Test:* the 31st frame in a minute is dropped; the 21st copy closes the oldest.

---

## 5. Out of scope for v1

| Item | Reason |
|---|---|
| Plain-HTTP browser "receive via link" | Not in v1. **Future risk:** unauthenticated and unencrypted on a hostile LAN. If added: off by default, labelled unencrypted, 128-bit single-use URL token expiring in 10 minutes, read-only, never touches pairing or grants |
| Same-user malware on the desktop | It already owns the user's session; a 0600 file cannot stop it. TPM or Secret Service keys can come later |
| Rooted or jailbroken phones | OS boundaries no longer hold |
| Key rotation without re-pair | A signed handover adds pin-bypass surface; re-pairing is rare and safe |
| Traffic analysis | Padding costs throughput, the top priority |
| BLE, Wi-Fi Direct; notification mirroring, SMS, SFTP | Not in v1; each needs its own model |
| Supply chain | Release process (cargo-deny, cargo-audit, reproducible builds) |
| Attacks on locked, powered-off devices | OS disk encryption is the control |

---

## 6. Decisions on the open questions (2026-10-08)

1. **Private discovery: accepted.** Outside "Add device", nearby devices show no names to anyone. Pairing is rare and hostile LANs are the owner's normal case.
2. **Skipped sensitive clips: show a quiet notice** ("not sent: marked sensitive"). Silent failures are banned by PLAN.md anti-pattern 18.
3. **Remote input cap: 60 keys per second is fine.** Input is enabled only from the desktop and stops when it locks.
4. **Auto-accept ceiling: none.** Own devices always auto-accept down to the free-space reserve (free space minus max(2 GiB, 5% of the volume), per protocol.md 6.4). Zero friction between own devices is the product's reason to exist.
5. **Windows firewall scope: any remote address.** Mutual TLS with pinning is the access control; subnet-only would break peers reached over VPN.

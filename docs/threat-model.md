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

---

## 4. Security requirements

Each item names its test. The tag is the phase that must pass it.

**Discovery**

- **SR-1 (P1)** A beacon or mDNS record causes a dial only if its tag verifies against a pinned peer's announce key. The dial goes only to the packet's source IP (never an address in the payload), never to multicast, broadcast or loopback, and at most once per peer per 5 s. *Test:* 10,000 forged beacons produce zero outbound SYNs.
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
- **SR-15 (P1)** Limits: 16 pending handshakes, 10 s handshake timeout, 4 connections per IP, 64 total; unpinned peers dropped after the handshake outside pairing mode. HTTP/2: 128 streams, 16 KiB headers, over 100 resets/s closes, CONTINUATION bounded, `h2` pinned at a version with the 2023 rapid-reset and 2024 CONTINUATION fixes. Session frames over 64 KiB close the connection. *Test:* slowloris, rapid reset and a 10,000-handshake flood leave a pinned session working.
- **SR-16 (P1)** Frames carry a per-connection sequence number; duplicates are dropped; unknown types ignored; malformed frames close the stream. *Test:* a duplicated clipboard frame has no effect.

**Transfer engine**

- **SR-17 (P1)** Manifest paths are arrays of components. Reject a component that is empty, `.` or `..`; contains `/`, `\`, NUL, C0/C1 controls or U+202A-202E, U+2066-2069; or exceeds 255 UTF-8 bytes. Reject depth over 32 and paths over 1,024 bytes. NFC-normalise. On every platform also reject `<>:"|?*`, trailing dot or space, and CON, PRN, AUX, NUL, COM0-9, LPT0-9 (including superscript digits) with or without an extension, plus case-insensitive duplicates. Any violation rejects the whole manifest. *Test:* a 200-name hostile corpus, all rejected, on Linux and Windows CI.
- **SR-18 (P1)** Writes go through a receive-folder handle: `cap-std` with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` on Linux; `FILE_FLAG_OPEN_REPARSE_POINT` plus a final-path check on Windows. Create exclusively; collisions get " (n)", never overwrite. `.part` is renamed only after the root hash verifies. *Test:* a planted symlink to `~/.bashrc` is not followed.
- **SR-19 (P1)** The bundle reader accepts only regular files and directories; symlinks, hardlinks, devices, FIFOs and PAX path overrides fail the bundle. Entries pass SR-17 and must match the manifest by name and size. Mode bits are ignored. Extraction is our own loop over SR-18, not `tar::Archive::unpack`. *Test:* zip-slip and symlink bundles write nothing outside the folder.
- **SR-20 (P1)** At most 100,000 entries and 16 MiB of manifest. Declared total must fit in free space minus max(2 GiB, 5%), checked at accept and on every write; ENOSPC fails cleanly. Bytes past a declared size are rejected. 4 concurrent transfers per peer. `.part` idle for 24 h is deleted. *Test:* a 1 PB manifest is refused before any write.
- **SR-21 (P1)** `Content-Range` must start exactly at the receiver's verified, chunk-aligned offset and end within the declared size. On resume the receiver sends the BLAKE3 hash of its verified prefix; the sender checks it against its recorded chunk hashes plus the source's size, mtime and inode or file ID. Any difference restarts that file at 0. *Test:* editing the source before resume yields a full re-send with a matching hash.
- **SR-22 (P1)** A failed chunk is re-requested at most 3 times, then the file fails, its `.part` is deleted and the user sees "failed integrity check from <label>". A final root mismatch is handled the same. *Test:* a fault-injecting sender produces no final file.
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

- **SR-32 (P1)** Never logged: private and announce keys, QR secrets, SAS, nonces, tokens, clipboard text, input events. File names at debug level only. Device IDs truncated to 8 characters. Log files 0600; diagnostics export redacts the same way. *Test:* CI seeds canary strings and greps all logs.
- **SR-33 (P1, gate P6)** `cargo-fuzz` targets for beacon, TXT, manifest, bundle and frame decoders run nightly; `#![forbid(unsafe_code)]` in parsers. P6 needs 8 CPU-hours per target with no crash. *Test:* fuzz report in the ship gate.
- **SR-34 (P6)** An external review checks every SR against release builds on the P6 matrix, including a hostile-LAN run with ARP spoofing and forged mDNS. *Test:* signed checklist.

---

## 5. Out of scope for v1

| Item | Reason |
|---|---|
| Plain-HTTP browser "receive via link" | Not in v1. **Future risk:** unauthenticated and unencrypted on a hostile LAN. If added: off by default, labelled unencrypted, 128-bit single-use URL token expiring in 10 minutes, read-only, never touches pairing or grants |
| Same-user malware on the desktop | It already owns the user's session; a 0600 file cannot stop it. TPM or Secret Service keys can come later |
| Rooted or jailbroken phones | OS boundaries no longer hold |
| Key rotation without re-pair | A signed handover adds pin-bypass surface; re-pairing is rare and safe |
| Traffic analysis | Padding costs throughput, the top priority |
| Relay, BLE, Wi-Fi Direct; notification mirroring, SMS, SFTP | Not in v1; each needs its own model |
| Supply chain | Release process (cargo-deny, cargo-audit, reproducible builds) |
| Attacks on locked, powered-off devices | OS disk encryption is the control |

---

## 6. Decisions on the open questions (2026-10-08)

1. **Private discovery: accepted.** Outside "Add device", nearby devices show no names to anyone. Pairing is rare and hostile LANs are the owner's normal case.
2. **Skipped sensitive clips: show a quiet notice** ("not sent: marked sensitive"). Silent failures are banned by PLAN.md anti-pattern 18.
3. **Remote input cap: 60 keys per second is fine.** Input is enabled only from the desktop and stops when it locks.
4. **Auto-accept ceiling: none.** Own devices always auto-accept down to the free-space reserve (free space minus max(2 GiB, 5% of the volume), per protocol.md 6.4). Zero friction between own devices is the product's reason to exist.
5. **Windows firewall scope: any remote address.** Mutual TLS with pinning is the access control; subnet-only would break peers reached over VPN.

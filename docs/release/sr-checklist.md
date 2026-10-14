# SR-34 checklist: external review of the security requirements

`docs/threat-model.md` SR-34: "An external review checks every SR against release builds on
the P6 matrix, including a hostile-LAN run with ARP spoofing and forged mDNS. Test: signed
checklist." This file is that checklist, prepared by the project. It is not signed. Nothing
here is a claim that a reviewer has looked at anything.

How to read it:

- **Automated evidence** names tests that exist in the tree (found by reading the code and
  the phase handoffs, 2026-10-13). The last full run recorded anywhere is Phase 5's
  (`cargo test --workspace`: 381 passed, 8 ignored, clippy clean). The suite was not
  re-run for this document. A reviewer should run it against the release commit.
- **Manual / live** is what a person must do because no test covers it.
- **Status** is the project's own: **Covered** (a test or a check of the whole rule exists),
  **Partial** (part of the rule is tested; the gap is named), **Owed** (needs a platform or a
  hostile run nobody has done), **Gap** (no test found by grep; may be missing).
- **Reviewer sign-off** is blank on purpose: name, date, build identifier (commit and the
  SHA-256 of the artifact checked), verdict (accept, accept with notes, reject).

Build under review: ______________ (commit)  Artifacts: .deb ______ Flatpak ______
Windows installer ______ Android APK ______ iOS build ______

## 1. Discovery

| SR | Rule (short) | Automated evidence | Manual / live | Status | Reviewer sign-off |
|---|---|---|---|---|---|
| SR-1 | A beacon or mDNS record causes a dial only if its tag verifies against a pinned peer; dial goes to the source IP only; at most once per peer per 5 s | `discovery/engine.rs`: `verified_uses_source_ip_and_stored_port`, `forged_and_misplaced_adverts_produce_nothing`; `discovery/limits.rs`: `dial_limiter_once_per_five_seconds`; `discovery/tag.rs`: `verify_rejects_wrong_tag_nonce_or_empty_set`; `beacon.rs`: `datagrams_through_parser_and_processor`; fuzz target `beacon` | Hostile run H2: 10,000 forged beacons, count outbound SYNs (must be 0) | Partial (unit level; the SYN count is not automated) | |
| SR-2 | Drop oversize beacons (512) and TXT (400); 5 packets/s per source, 200/s total; caches 256 entries, 120 s TTL | `advert.rs`: `oversized_beacon_is_dropped_even_if_valid_json`, `oversized_txt_is_dropped`; `limits.rs`: `per_source_limit_resets_each_second`, `global_limit_caps_floods_and_table_size`; `cache.rs`: `advert_cache_is_capped_under_flood`, `add_device_caps_at_32`, `lru_evicts_least_recently_used`, `ttl_expires_entries`; `beacon.rs`: `global_rate_limit_drops_flood_unparsed`; fuzz `beacon`, `mdns_txt` | H2: 1,000,000 unique IDs, memory under 5 MiB and under 10% of one core (not automated) | Partial | |
| SR-3 | Never answer a beacon; TTL 1; drop off-link mDNS | `discovery/mod.rs`: `source_rules`; `beacon.rs`: `schedule_offsets`; fuzz `beacon` | H2: spoofed-source beacon must draw no reply packet (tcpdump on a third host) | Partial | |
| SR-4 | Outside pairing mode the mDNS name is 16 random hex chars rotated every 15 min and on network change; TXT/beacon carry only `v`, `n`, `t` | `advert.rs`: `txt_private_carries_only_v_n_t`; `mdns.rs`: `labels_are_16_lowercase_hex_and_fresh`; `service.rs` tests; fuzz `mdns_txt` | H1: 30-minute idle capture (tcpdump, Wireshark `mdns`) shows no name and no repeated identifier across rotations | Partial | |
| SR-5 | Announce key K per device, sent over the authenticated session; tag = HMAC-SHA256(K, "pd-announce-v1" \|\| n)[0..16]; unpair rotates K | `discovery/tag.rs`: `tag_vectors`, `verify_finds_the_matching_peer`; `pin_store`: `remove_rotates_local_key`, `announce_key_keeps_highest_kid`; `session/types.rs`: `announce_key_round_trip` | H2: unpair, then check the removed device cannot match new beacons | Partial | |

## 2. Pairing and identity

| SR | Rule (short) | Automated evidence | Manual / live | Status | Reviewer sign-off |
|---|---|---|---|---|---|
| SR-6 | No pairing prompt unless "Add device" is open; 120 s, 3 failures; not while the desktop is locked | `host/lock.rs`: `pairing_is_refused_while_locked`; `session/pairing.rs`: `add_device_window_rules`, `pair_request_rate_limit`, `timeout_path`; `discovery/engine.rs`: `unpaired_only_while_window_open`; fuzz `session_json` | H3: unsolicited pair request gets a closed connection and no prompt; lock the desktop, try "Add device" on COSMIC and Windows | Partial (Windows lock owed) | |
| SR-7 | Commit-then-reveal SAS bound to SPKIs, nonces and the TLS exporter | `session/pairing.rs`: `sas_test_vector`, `commit_and_mismatch`, `wrong_reveal_is_commit_mismatch`, `mitm_sees_different_sas`, `full_sas_pairing` | H1: TLS-terminating proxy between phone and desktop during pairing; SAS must differ | Covered by test; live MITM owed | |
| SR-8 | Prompt shows SAS, Short ID, sanitised name (no controls or bidi, 64 chars), no default-focused button; local label; pairing notification and dates | `session/hello.rs`: `sanitises_names`; `advert.rs`: `name_sanitisation`; `host_smoke.rs`: `last_seen_survives_restart`; iOS `FileNamesTests`, `PeerRefusalsTests`; `tray.rs`: `device_lines_show_last_seen_when_offline` | Look at the SAS prompt on each UI: no focused default; pair with a device named with U+202E (H3) | Partial (UI focus is visual only) | |
| SR-9 | QR: full Device ID, 128-bit secret, 120 s, single use, SPKI check, capture-blocked, displayer confirms | `session/pairing.rs`: `qr_round_trip_with_ipv6`, `qr_proofs`, `full_qr_pairing`, `qr_wrong_secret_and_expiry`, `qr_scanner_rejects_bad_proof_a_and_wrong_peer`, `qr_request_without_offer_is_expired`; `host_smoke.rs`: `qr_pairing_and_single_use_secret`; fuzz `qr_uri` | Screenshot of the QR screen is blank on Android (`FLAG_SECURE`) and Windows (`WDA_EXCLUDEFROMCAPTURE`) | Partial (capture block owed) | |
| SR-10 | Known name or address with a new key is a new device; no "key changed, accept?" | `identity/mod.rs`: `device_id_is_sha256_of_spki`, `rejects_mismatched_key`; `session/hello.rs`: `trust_mismatch`; `pin_store`: `unknown_device_is_not_found` | Regenerate a key, check the old pin and grants are untouched and the device shows as new | Partial | |

## 3. Transport

| SR | Rule (short) | Automated evidence | Manual / live | Status | Reviewer sign-off |
|---|---|---|---|---|---|
| SR-11 | TLS 1.3 only, Ed25519 only, client auth, pinned-SPKI verifier, no tickets or 0-RTT, ALPN `playdrop/1`, no SNI | `tls/mod.rs`: `tls12_client_refused`, `non_ed25519_client_certificate_refused`, `missing_client_certificate_refused`, `missing_alpn_refused`, `wrong_alpn_refused`, `configs_disable_resumption_and_early_data`, `mutual_pinning_handshake`, `wrong_server_key_rejected_by_client`, `unpinned_client_rejected_outside_window_accepted_inside`, `window_closing_after_verify_still_rejects` | `testssl.sh` or `openssl s_client -tls1_2` against port 42187; confirm no SNI in a capture | Covered by test; external scan owed | |
| SR-12 | Version only by ALPN; discovery `v` is a hint; minimum enforced | `tls/mod.rs`: `wrong_alpn_refused`; `session/hello.rs`: `identity_and_version_checks`; `frame.rs`: `newer_version_gets_error_reply_and_is_ignored`; fuzz `session_json` | H2: rewrite TXT to `v=0`, nothing changes | Partial | |
| SR-13 | Every request checked against the authenticated Device ID; IDs/tokens 128-bit and bound to the creator; outbox returns only the caller's items | `host_smoke.rs`: `requests_are_bound_to_the_tls_device`; `host/blobs.rs`: `blob_ttl_and_binding`; `host/outbox.rs`: `outbox_range_and_prefix`; `transfers.rs` tests | With two paired phones, peer B requests peer A's transfer: 404 | Covered by test; two-peer live owed | |
| SR-14 | Per-device grants, defaults all on except input; changed only from the local UI; unpair closes within 1 s | `pin_store`: `grants_defaults`; `session/types.rs`: `grant_and_unpaired_tables`; `phase5_core.rs` (input refused without grant); `host_smoke.rs`: `pair_send_ping_unpair`; ipc `unknown_events_are_an_error_the_client_can_skip` | Unpair from the desktop and time the phone's session close (target 1 s) | Partial (1 s timing not measured) | |
| SR-15 | Handshake gate (16 pending, 10 s), 4 connections per IP, 64 total; h2 limits, rapid-reset and CONTINUATION bounds; frames over 64 KiB close | `session/limits.rs`: `defaults_match_section_10`, `per_ip_and_total_limits`, `handshake_gate`, `reset_rate`; `frame.rs`: `oversize_length_is_fatal_before_body`; `h2` 0.4.20 in `Cargo.lock` (past the 2023 and 2024 fixes); fuzz `frame` | H4: slowloris, rapid reset and a 10,000-handshake flood while a pinned session keeps working | Partial (counters tested; the floods are not) | |
| SR-16 | Per-connection sequence numbers; duplicates dropped; unknown types ignored; malformed frames close the stream | `frame.rs`: `duplicate_dropped_gap_fatal`, `first_frame_must_be_seq_one`, `duplicate_clipboard_has_no_effect`, `unknown_type_passes_through_and_counts`, `malformed_and_missing_fields_are_fatal`; fuzz `frame`, `session_json` | | Covered | |

## 4. Transfer engine

| SR | Rule (short) | Automated evidence | Manual / live | Status | Reviewer sign-off |
|---|---|---|---|---|---|
| SR-17 | Manifest path rules: no `..`, separators, controls, bidi, over-length, reserved Windows names; NFC; case-insensitive duplicates; whole manifest rejected | `transfer/manifest.rs`: `hostile_components_rejected`, `friendly_components_accepted`, `depth_and_length_limits`, `case_insensitive_duplicates_rejected`, `structural_rules`; `sender.rs`: `stream_names_follow_sr17`; iOS `FileNamesTests`, Android `FileNamesTest`; fuzz `manifest` | The "200-name hostile corpus on Linux and Windows CI" in the threat model: the test above is the corpus; confirm its size and that Windows CI ran it | Covered on Linux; Windows CI run not checked | |
| SR-18 | Beneath-only writes (`openat2` on Linux), exclusive create, collisions get " (n)", `.part` renamed after the root hash verifies | `transfer/fsx.rs`: `symlinks_not_followed`, `planted_symlink_to_outside_is_not_followed_when_creating`, `create_unique_never_overwrites`, `collision_names`; `receiver.rs`: `planted_symlink_not_followed`, `part_file_survives_drop_and_finalizes` | Plant a symlink to `~/.bashrc` in the receive folder, send a file of that name | Covered on Linux; Windows reparse-point path CI only | |
| SR-19 | Bundle reader accepts only regular files and directories, entries match the manifest, own extraction loop | `transfer/bundle.rs`: `rejects_deviations`, `header_fields`, `restart_mid_member_is_identical`; fuzz `bundle`, `manifest`; `receiver.rs`: `receiver_full_flow_with_bundle_and_reload` | Zip-slip and symlink bundles from a hostile sender (H5) | Covered by test; hostile sender owed | |
| SR-20 | 100,000 entries, 16 MiB manifest, free-space reserve, ENOSPC clean, 4 transfers per peer, stale `.part` deleted at 24 h | `manifest.rs`: `entry_limit`; `receiver.rs`: `space_cancel_skip_and_cleanup`, `half_made_staging_without_a_record_is_replaced`; `transfers.rs`: `lifecycle_and_bounds` | Offer a 1 PB manifest: refused before any write; fill a small tmpfs receive dir. Windows free-space check is a TODO in `host/platform.rs` | Partial (Windows owed) | |
| SR-21 | Exact-offset resume, prefix hash check, source change restarts the file | `sender.rs`: `resume_decisions`, `stream_source_resume_check`; `chunk.rs`: `resume_encoding_matches_full`; `phase1_gates.rs`: `gate_a_kill_and_resume_500mib`; `e2e_tests.rs`: `duplex_kill_and_resume_64mib` | Edit the source file between kill and resume: full re-send, hash matches | Covered by test; edit-the-source live check owed | |
| SR-22 | A failed chunk retried at most 3 times, then the file fails and `.part` is deleted; "failed integrity check" shown | `receiver.rs`: `chunk_retries_then_fail`, `part_file_root_mismatch_deletes_part`; `chunk.rs`: `flipped_byte_detected_at_its_chunk` | Fault-injecting sender produces no final file | Covered | |
| SR-23 | Windows Zone.Identifier; Linux 0600 files in a 0700 folder; "Open" on risky types opens the folder | `playdropd/src/open.rs`: `sr23_opens_the_folder_for_risky_files`, `risky_extensions_ignore_case`, `paths_outside_the_receive_folder_are_refused`; `fsx.rs`: `private_modes_and_nested_dirs`; Android `FileNamesTest` (`isRiskyToOpen`) | Windows: received `.exe` triggers SmartScreen (needs Windows 11) | Partial (Windows owed) | |

## 5. Clipboard and input

| SR | Rule (short) | Automated evidence | Manual / live | Status | Reviewer sign-off |
|---|---|---|---|---|---|
| SR-24 | Skip sensitive clips (Windows exclusion formats, `x-kde-passwordManagerHint`, `EXTRA_IS_SENSITIVE`); manual send asks; 1 MiB cap | `clipboard/wayland.rs`: `password_hint_detection`, `read_limited_skips_over_one_mib`; `clipboard/windows.rs`: `exclusion_marks`; `clipboard/coalesce.rs`; Android `ClipboardOutcomeTest`; `host_smoke.rs`: `clipboard_push_to_all_with_echo_rule` | Copy from KeePassXC on COSMIC and Bitwarden on Windows: nothing is sent (threat model's own test) | Partial (password-manager runs owed) | |
| SR-25 | Input only with the grant, a started session, unlocked desktop, under 10 min idle; indicator and kill switch; 60 keys/s; release all keys; no elevation | `host/input.rs`: `key_cap_per_peer`, `idle_sessions_expire`; `phase5_core.rs`: `input_session_ends_when_the_controller_drops`; `input/plan.rs`: `release_all_is_reverse_press_order_and_empties`; iOS `RemoteInputRulesTests`, Android `ui/input` tests | Lock the desktop mid-session: input stops within 100 ms; send 1,000 keys/s: throttled (not automated) | Partial | |

## 6. Local IPC and platform

| SR | Rule (short) | Automated evidence | Manual / live | Status | Reviewer sign-off |
|---|---|---|---|---|---|
| SR-26 | D-Bus: caller UID must match; other Flatpak apps refused; files as fds | `playdrop-ipc/src/linux.rs`: `our_flatpak_is_allowed_and_others_are_not`, `parses_application_name`; `server/linux.rs`: `fd_path_accepts_files_and_dirs_and_refuses_others`, `verdict_cache_is_bounded`; `tests/dbus_round_trip.rs`; client checks `daemon_exe_names` | Call the daemon from another UID (`sudo -u` with a bus address) and from a Flatpak app: both refused | Partial (other UID and Flatpak caller live owed) | |
| SR-27 | Windows pipe `playdrop-<SID>`, first instance, reject remote, DACL; both ends verify | `pipe.rs` tests (`hello_request_and_interleaved_event`, `version_mismatch_is_refused`); `client.rs`: `windows_image_names`; Windows code compiled on the CI runner | Another user and an SMB client cannot connect (needs Windows 11) | Owed | |
| SR-28 | Keys, pins, grants stored per platform (0700/0600 refuse looser; DPAPI; Android Keystore wrapped, backup-excluded; iOS Keychain) | `identity/mod.rs`: `modes_are_private_and_loose_modes_are_refused`, `symlinked_key_is_refused`; `pin_store`: `file_is_private_and_loose_mode_is_refused`, `tampered_pin_is_refused`, `debug_never_prints_keys`; `identity/sealing.rs` tests; Android `SealFormatTest`, `allowBackup=false` and `data_extraction_rules` in the manifest; iOS `KeychainSecretStoreTests` (Mac CI only) | `chmod 644` stops the daemon with a clear message; `adb backup` / device transfer holds no key; DPAPI blob on Windows | Partial (Windows, iOS device owed) | |
| SR-29 | Android components `exported=false` except the share activity (and BootReceiver); immutable PendingIntents; 2 s cancel card | `AndroidManifest.xml` (one exported launcher, one share activity, BootReceiver); `NotificationPlanTest`; Phase 3 live: "SR-29 refusals from the shell UID" | `adb shell am broadcast` to the action receiver from the shell UID is refused (done in Phase 3); repeat on the release APK | Partial (re-run on the release build) | |
| SR-30 | iOS `playdrop://` takes no parameters; QR scanned only in-app | Code rule in `URLSchemeHandler.swift`; Phase 4 decision table; no dedicated test found by grep | `xcrun simctl openurl` / Safari with parameters on the iPhone: ignored | Gap (no test) | |
| SR-31 | Exclusive bind on 42187; diagnostics name the holder; program-scoped firewall rule | `net/server.rs` (no `SO_REUSEPORT`); `probe.rs`: `listener_down_comes_first`, `listening_sockets_are_read_from_the_proc_table`, `probe_finds_a_live_listener_on_loopback`; `Error::PortHeld` mapped in ffi | A second user's daemon reports the conflict; Windows `SO_EXCLUSIVEADDRUSE` (TODO in `net/server.rs`) and the firewall rule on a real install | Partial (Windows owed) | |

## 7. Hygiene and assurance

| SR | Rule (short) | Automated evidence | Manual / live | Status | Reviewer sign-off |
|---|---|---|---|---|---|
| SR-32 | Never log keys, secrets, SAS, nonces, tokens, clipboard text, input; file names at debug only; Device IDs truncated; log files 0600; redacted diagnostics | `host/events.rs`: `events_round_trip_and_secrets_stay_redacted`; `clipboard/mod.rs`: `debug_never_prints_content`; `pin_store`: `debug_never_prints_keys`; ipc logs `op` names only (`docs/ipc.md` 4) | The threat model asks for a CI test that seeds canary strings and greps every log. **No such test was found.** Reviewer: run a session with `RUST_LOG=trace` and a canary clipboard and grep the logs and the diagnostics export | Gap (CI canary test missing) | |
| SR-33 | cargo-fuzz targets for beacon, TXT, manifest, bundle, frame decoders nightly; `forbid(unsafe_code)` in parsers; 8 CPU-hours per target with no crash | `fuzz/fuzz_targets/`: `beacon`, `mdns_txt`, `manifest`, `bundle`, `frame`, `session_json`, `qr_uri`; `.github/workflows/fuzz.yml` nightly (about 10 minutes per target); workspace `unsafe_code = "forbid"` lint | The 8 CPU-hour runs per target, command in `fuzz/README.md`; log tails in `docs/release/fuzz-logs/` | **Met 2026-10-13/14**: every target ran 8 forks x 1 h with no crash; `session_json` found one SR-8 cosmetic bug in 4 minutes (name cut on a space kept the space), fixed and re-run clean for the full hour; `docs/phases/phase-6-ship-gate.md` 1.2 | |
| SR-34 | External review including a hostile-LAN run | This file and section 8 | The whole review | Owed | |

## 8. Hostile-LAN run plan (SR-34)

A procedure for the reviewer. **Nothing in it has been run, and the project ran no attack.**
Run it only on a network the reviewer owns or has written permission to attack: a lab
router with no uplink, not a home, guest, campus or office network.

### 8.1 Lab

| Role | Machine | Notes |
|---|---|---|
| Victim desktop | Pop!_OS COSMIC 24.04 and a Windows 11 PC, release builds installed from the GitHub release | `playdropd` running, paired with the victim phone |
| Victim phone | Android 16 (release APK) and an iPhone (foreground) | paired to the desktops |
| Attacker | A Linux laptop (Kali or Ubuntu) on the same segment, wired if the AP isolates clients | root |
| Observer | A fourth host on a mirror port, or the attacker in promiscuous mode | `tcpdump`, Wireshark |

Record the build identifiers on page one of this file. Capture every run to a pcap named
`H<n>-<step>.pcapng`.

### 8.2 Tools

| Need | Tool |
|---|---|
| ARP spoofing / MITM positioning | `bettercap` (`arp.spoof`, `net.sniff`), `arpspoof` from `dsniff`, or `ettercap -T -M arp` |
| Forged mDNS | `avahi-publish -s`, Python `zeroconf` (ServiceInfo with chosen TXT), `scapy` raw mDNS responses |
| Forged beacons and floods | `scapy` UDP to 239.255.42.187:42187 (the beacon format is `docs/protocol.md` section 8), `hping3` for SYN floods |
| TLS interception for the pairing MITM | `mitmproxy --mode transparent` (or `socat` with a self-made Ed25519 certificate) |
| HTTP/2 abuse | `h2spec`, a small `h2`-library client for rapid reset (CVE-2023-44487) and CONTINUATION floods, `slowhttptest` / a raw TLS socket for slowloris |
| TLS posture | `testssl.sh`, `openssl s_client -tls1_2 -connect <ip>:42187 -alpn playdrop/1` |
| Observation | `tcpdump -i <if> -w`, `ss -tnp`, `/proc/<pid>/status` for memory, `top -b -p <pid>` for CPU |

### 8.3 Procedures

**H1: ARP spoofing and passive capture (SR-4, SR-7, SR-11, SR-13).**
1. With the victims idle, capture 30 minutes of mDNS and UDP 42187 from the observer.
   Pass: no device name, no stable identifier repeated across rotations (SR-4).
2. Poison the desktop and phone ARP caches (`bettercap -eval "set arp.spoof.targets <desktop>,<phone>; arp.spoof on"`)
   so that traffic passes through the attacker. Send a 500 MiB file, a clipboard push and
   a ping between the victims. Pass: transfers continue or fail, but the capture shows only
   TLS 1.3 ciphertext, no SNI, no certificate in clear (SR-11), and no new pairing or
   permission appears.
3. With "Add device" open on the desktop, run a TLS-terminating proxy and pair through it.
   Pass: the two SAS values differ and the user rejects (SR-7); a wrong or reused QR secret
   fails (SR-9).
4. Pair, then ARP-spoof and drop the attacker in as a "pinned peer" with a self-made key.
   Pass: handshake refused (SR-11), no UI prompt (SR-6).

**H2: forged mDNS and beacons (SR-1 to SR-5, SR-12).**
1. Publish `_playdrop._tcp` services with random 16-hex names, plausible and garbage TXT,
   and a service naming the attacker's IP and an open port. Run `tcpdump 'src host <victim> and tcp[tcpflags] & tcp-syn != 0'`
   on the attacker. Pass: zero SYNs from the victims to the attacker (SR-1).
2. With scapy, send 10,000 beacons from distinct source ports with fresh nonces, and 1,000,000
   unique IDs over ten minutes. Record the daemon's resident memory and CPU before and
   after. Pass: growth under 5 MiB, under 10% of one core (SR-2); the discovery of real
   peers keeps working or recovers through last-known dial.
3. Send a beacon with a spoofed source address and watch for any reply (SR-3). Send mDNS
   from an off-link source. Pass: no reply, no state change.
4. Rewrite TXT `v=0` and `v=9`. Pass: no behaviour change (SR-12).
5. Unpair one device, then send beacons computed with its old announce key. Pass: not
   matched (SR-5).

**H3: pairing abuse (SR-6, SR-8, SR-9).**
1. Without "Add device" open, send a pairing request from the attacker. Pass: connection
   closed, no prompt, no notification.
2. Open "Add device", advertise the name `Qasim's Pixel` and a name with U+202E, from the
   attacker. Pass: names rendered stripped, with the Short ID, and no default-focused
   Accept (SR-8). Repeat 10 requests in a minute: rate limited.
3. Lock the desktop and try "Add device": refused with "desktop is locked".

**H4: resource exhaustion on TCP 42187 (SR-15).**
1. 10,000 half-open handshakes (`hping3 -S --flood -p 42187`) and 100 full TLS handshakes
   with no client certificate, held open. Pass: a pinned phone's session and a running
   transfer keep working; unpinned connections are dropped.
2. Slowloris against the h2 stream of a connection that completed the handshake with a
   throw-away key outside pairing mode: it must be dropped right after the handshake.
3. HTTP/2 rapid reset (more than 100 resets/s) and CONTINUATION flood from a pinned test
   device (pair the attacker deliberately for this step only). Pass: the offending connection
   closes; the daemon's memory stays flat; other sessions are unaffected.

**H5: hostile paired peer (SR-13, SR-17 to SR-22).** Pair the attacker as a normal device
and send manifests and bundles from a custom client: `../` and `..\` names, a 200-name
hostile list, symlink and hardlink bundle entries, a 1 PB manifest, 200,000 entries, lying
sizes, a resume offset that does not match, a changed source. Also ask for another peer's
transfer ID and outbox. Pass: every case refused or failed cleanly, nothing written outside
the receive folder, free space reserve respected, other peer's data returns 404.

**H6: local attacker (SR-26, SR-27, SR-28, SR-29, SR-31).** On the desktop as another user:
call the D-Bus methods (`gdbus call --session`), bind 42187 first, read the data directory.
On Windows: connect to the pipe from another user and over SMB. On Android: fire the share,
action and boot intents from the shell UID. Pass: all refused, port conflict is reported
with the holder.

### 8.4 Report

For each step: build, date, tool and version, exact command, pcap name, result (pass,
fail, not run) and the reviewer's initials. Failures are filed as issues with the pcap.

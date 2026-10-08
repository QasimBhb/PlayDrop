# PlayDrop plan

PlayDrop is an open-source companion app that links a phone to a computer. It runs on Android, iOS, Windows and Linux, with Pop!_OS COSMIC as the first Linux target and Ubuntu GNOME as the second. It does two jobs over the local network, with no account and no cloud. The first is a file drop that is at least as fast as LocalSend, and it adds resume and steady speed for thousands of small files. The second is a lightweight, always-on companion link in the style of KDE Connect: ping or find my phone, clipboard push, battery, media control, and later opt-in notification mirroring and remote input. The user runs both incumbents today and says neither "works how I want". KDE Connect does a lot but is slow and unreliable with files, and LocalSend is fast but has no companion features, no resume, and slows down sharply as the file count grows. Speed is the user's top priority. This file is the only project document. It replaces the research notes, the report, the older plan and the Cursor plan, and every factual bullet links to its source. Claims that could not be matched to a research note are marked **unverified**.

Research date: all sources were gathered in September 2026. "Notes" means the six research briefs this file replaces.

---

## User requirements (answered 2026-10-08)

These answers close most of Phase 0. They override anything below that disagrees.

| Question | Answer | Consequence |
|---|---|---|
| Phones | Android and iPhone, both from day one | iOS ships in v1 as sender plus foreground receiver; Android gets the full companion set |
| Device pairs | Phone to PC, phone to phone, PC to PC | Every host can hold an outbox, not only desktops. Android can serve an outbox to an iPhone. PC to PC is daemon to daemon |
| Who builds | Fully agentic (AI agents write all code) | Native UIs per platform are affordable; pick well-documented toolkits so agents have examples to follow |
| Licence | Must be able to reuse both KDE Connect and LocalSend code | **GPL-3.0-or-later.** KDE Connect is GPL-2.0-or-later and LocalSend is Apache-2.0; both can be combined into GPL-3.0, not into Apache. Keep copyright headers and SPDX tags on every reused file |
| Daily desktops | Pop!_OS COSMIC and Windows 11 | These two are the v1 test matrix. Ubuntu GNOME must work but is not gated |
| Daily features | Automatic clipboard sync, remote input (trackpad, keyboard, presenter), media control, battery, ping/find | All in v1 except remote input on COSMIC, which depends on the new RemoteDesktop portal (Phase 5, flagged risk). Notification mirroring is not required |
| Receive from own devices | Auto-accept, notify after | Per-pinned-device auto-accept is the default once paired; a notification shows what landed |
| Network | Single 5 GHz router, mesh, VPN on phone or PC, often guest/campus/hotel Wi-Fi | All four fallbacks (last-known dial, QR, hotspot, USB tether) are v1 features, not Phase 7 |
| Interop | None. PlayDrop only | Own protocol and port. No LocalSend or KDE compatibility modes |
| UI layer | Native per platform | Decision A below is locked: Kotlin/Compose, SwiftUI, Rust desktop UI |

**The two complaints that define the product**

1. KDE Connect pairing with COSMIC "is not working well, I'm not sure why". Root cause is not known. Likely candidates from the research: the port 1716 conflict when both `cosmic-utils/kdeconnect` and official `kdeconnectd` are installed, broadcast discovery being filtered on a mesh or VPN, and pairing loss after IP change ([cosmic-utils/kdeconnect](https://github.com/cosmic-utils/kdeconnect), [bug 395749](https://bugs.kde.org/show_bug.cgi?id=395749)). PlayDrop's answer: key-based identity, concurrent multi-path discovery, and a diagnostics screen that names the failing layer.
2. LocalSend is reliable but every send is too many steps: share sheet, open LocalSend, pick device, tap send, accept on the desktop. The user wants KDE Connect's flow: a persistent Android notification with a "Send clipboard" action, and desktop copy that lands on the phone with no action at all.

**UX requirements derived from that**

- Android: a persistent foreground-service notification with actions "Send clipboard" and "Send to <default device>". One tap, no app open.
- Android share sheet: "PlayDrop: <default device>" sends immediately to the last-used or pinned-default peer. No device picker unless the user asks for it.
- Desktop: any clipboard change is pushed to paired phones automatically (COSMIC via `ext-data-control-v1`, Windows via `AddClipboardFormatListener`). Phone clipboard changes push to the desktop automatically while the app is foreground; Android 10+ blocks background clipboard reads, so the notification action covers the background case.
- iOS: share extension plus a "Send clipboard" shortcut in the app and a widget or Shortcut action; no background clipboard is possible.
- Receiving: auto-accept from pinned devices, then one notification per transfer with "Open" and "Show in folder".
- Diagnostics: when a device is missing, show which discovery path failed and offer the next one (QR, hotspot, USB tether).

---

## How KDE Connect works

**Discovery**
- The classic discovery method is a UDP broadcast of a `kdeconnect.identity` JSON packet on port 1716. It announces a TCP port (default 1716) where the daemon accepts the encrypted control connection ([NixOS #311448](https://github.com/NixOS/nixpkgs/issues/311448)).
- TCP and UDP 1714-1764 must be open on the firewall for KDE Connect or GSConnect to work ([NixOS #311448](https://github.com/NixOS/nixpkgs/issues/311448), [NixOS #258690](https://github.com/NixOS/nixpkgs/issues/258690)).
- The identity body carries `deviceId`, `deviceName`, `deviceType`, `protocolVersion`, `tcpPort`, `incomingCapabilities` and `outgoingCapabilities`. `deviceId` is a UUIDv4 with underscores in place of hyphens ([Valent protocol reference](https://valent.andyholmes.ca/documentation/protocol.html)).
- `deviceType` values are `desktop`, `laptop`, `phone`, `tablet` and `tv` ([kdeconnect-meta protocol.md](https://github.com/KDE/kdeconnect-meta/blob/work/protocol-schemas/protocol.md)).
- Discovery breaks behind firewalls, VPNs and client-isolated Wi-Fi. With an OpenVPN TUN device only manual connect-by-IP works, and users are told to disable the VPN or bridge it with TAP ([KDE Discuss: behind a VPN](https://discuss.kde.org/t/kde-connect-behind-a-vpn/46037)).
- Manual "connect by IP" is the fallback when automatic discovery fails ([KDE Discuss](https://discuss.kde.org/t/kde-connect-behind-a-vpn/46037)).

**Pairing and trust**
- Pairing is trust on first use. Peers exchange `kdeconnect.pair` packets (`{"pair": true}` to request or accept, `false` to reject or unpair), and by convention a request times out after 30 seconds ([Valent protocol reference](https://valent.andyholmes.ca/documentation/protocol.html)).
- After acceptance, each side stores the peer's certificate against its `deviceId` and checks it on every TLS connection. A hash computed from both public keys is shown so the user can compare the two screens ([KDE Connect privacy policy](https://community.kde.org/KDEConnect/PrivacyPolicy)).
- The protocol schema documents versions 7 and 8. Version 8 adds a pairing timestamp that is used in the verification code calculation ([kdeconnect-meta protocol.md](https://github.com/KDE/kdeconnect-meta/blob/work/protocol-schemas/protocol.md)). Valent's reference says 7 is the only valid value ([Valent](https://valent.andyholmes.ca/documentation/protocol.html)). The exact verification-code algorithm was not found ([kde_connect notes, gaps](https://github.com/KDE/kdeconnect-meta/blob/work/protocol-schemas/protocol.md)).

**Transport and packet format**
- Every packet is `{id, type: "kdeconnect.<plugin>[.<action>]", body}`, sent as newline-delimited JSON over the TLS control socket ([Valent protocol reference](https://valent.andyholmes.ca/documentation/protocol.html)).
- A packet with binary data adds `payloadSize` and `payloadTransferInfo: {port}`. The bytes then flow over a second, short-lived TCP socket on that port ([Valent protocol reference](https://valent.andyholmes.ca/documentation/protocol.html)).
- The control connection uses TLS 1.2 and runs peer to peer with no cloud relay ([KDE Connect privacy policy](https://community.kde.org/KDEConnect/PrivacyPolicy), [kdeconnect.org](https://kdeconnect.org/)).
- There is no internet or relay mode, by design ([kdeconnect.org](https://kdeconnect.org/)).
- Bluetooth support is disputed. A developer blog says it is disabled by default and not fully functional ([blog.cyril.by](https://blog.cyril.by/en/software/kde-connect-over-bluetooth)), while a release announcement reportedly says you can now connect over Bluetooth ([Phoronix forum](https://www.phoronix.com/forums/forum/software/desktop-linux/1059288-kde-connect-1-10-released-to-improve-the-android-device-integration/page2)).

**Plugins**
- Each feature is a plugin in its own namespace: battery, clipboard, connectivity_report, find/ring, lock, MPRIS, mousepad, presenter, notifications (with reply), ping, runcommand, sftp, share, sms and telephony ([kdeconnect-meta protocol.md](https://github.com/KDE/kdeconnect-meta/blob/work/protocol-schemas/protocol.md)).
- Battery and connectivity polling is deprecated in favour of push updates ([kdeconnect-meta protocol.md](https://github.com/KDE/kdeconnect-meta/blob/work/protocol-schemas/protocol.md)).
- Browsing the phone's files is not done over the share channel. The phone runs an SFTP server that the desktop mounts ([kdeconnect-kde plugins/sftp](https://github.com/KDE/kdeconnect-kde/tree/master/plugins/sftp)).
- Capabilities are negotiated per packet type, so a client can implement a subset of plugins and still interoperate ([Valent protocol reference](https://valent.andyholmes.ca/documentation/protocol.html)).

**Platform ports**

| Port | Facts | Source |
|---|---|---|
| Plasma desktop | Qt/C++ around a UI-agnostic core that exposes D-Bus interfaces; version 26.04.0 (April 2026, approximate) | [Wikipedia](https://en.wikipedia.org/wiki/KDE_Connect) |
| Android | Kotlin/Gradle, version 1.33.9 (Oct 2025, approximate) | [kdeconnect-android](https://invent.kde.org/network/kdeconnect-android), [Wikipedia](https://en.wikipedia.org/wiki/KDE_Connect) |
| iOS | Swift, on the App Store for iOS 14+ and macOS 13+; entitlements need KDE e.V. signing; version 0.4.3 | [kdeconnect-ios](https://github.com/KDE/kdeconnect-ios), [community.kde.org](https://community.kde.org/KDEConnect) |
| Windows | Built from the desktop tree. Its status conflicts between sources: "officially supported since 2021" versus "not officially supported" (see Conflicts) | [community.kde.org](https://community.kde.org/KDEConnect), [Wikipedia](https://en.wikipedia.org/wiki/KDE_Connect) |
| GSConnect | GNOME Shell extension (GJS). Now community-maintained with no dedicated developers, and it will not work if KDE Connect is also installed | [GSConnect](https://github.com/GSConnect/gnome-shell-extension-gsconnect), [README](https://github.com/shell-extensions/gsconnect/blob/main/README.md) |
| Valent | GTK/libpeas app for any GTK desktop; still alpha in the latest assessment found (2023) | [Linux Uprising](https://www.linuxuprising.com/2023/02/valent-is-kde-connect-client-for-gtk.html) |
| cosmic-utils/kdeconnect ("Connected") | Native Rust KDE Connect implementation for COSMIC: a D-Bus service plus a panel applet in the COSMIC Store. Has pairing, battery, clipboard, contacts, MPRIS, notifications with replies, SFTP, SMS and telephony. Remote input, presenter and virtual display are missing ("require RTP"). You must uninstall official KDE Connect first because both use port 1716 | [cosmic-utils/kdeconnect](https://github.com/cosmic-utils/kdeconnect) |

**Licence**
- The desktop repo ships `LICENSES/GPL-2.0-or-later.txt` ([kdeconnect-kde](https://github.com/KDE/kdeconnect-kde/blob/master/LICENSES/GPL-2.0-or-later.txt)). The project as a whole is described as dual LGPL/GPL ([Wikipedia](https://en.wikipedia.org/wiki/KDE_Connect)).
- The licences of GSConnect and cosmic-utils/kdeconnect were not confirmed. Check them before reading their code ([kde_connect notes gap; GSConnect](https://github.com/GSConnect/gnome-shell-extension-gsconnect)).

**KDE Connect 2.0 and the NLnet grant**
- NLnet funds a KDE Connect project ([NLnet](https://nlnet.nl/project/KDE-Connect/)). The 2.0 plan reportedly adds mDNS discovery on all devices, because many networks block broadcast but allow service discovery, and aims to rebuild discovery and transport with "multicast and modern TLS". The source article returned 403, so this is secondhand ([Phoronix](https://www.phoronix.com/news/KDE-Connect-2.0-Plans)).
- On Android, merge request !375 announces `_kdeconnect._udp` through `NsdManager` alongside broadcast, not instead of it ([MR !375](https://invent.kde.org/network/kdeconnect-android/-/merge_requests/375)). No source confirmed that mDNS has shipped in a stable release by September 2026 ([MR !91](https://invent.kde.org/network/kdeconnect-android/-/merge_requests/91)).

**CVE-2020-26164 (CVSS 5.5, fixed before kdeconnect-kde 20.08.2)**
- The GUI identified peers only by `deviceName`, which the attacker controls and which is sent in cleartext broadcast ([oss-sec](https://seclists.org/oss-sec/2020/q4/56)).
- Forged broadcasts could make `kdeconnectd` connect to any IP and port and send its identity. Spoofed source IPs gave broadcast amplification, and floods of distinct `deviceId`s exhausted CPU and memory ([oss-sec](https://seclists.org/oss-sec/2020/q4/56), [CVE summary](https://cyberstrike.io/cve/CVE-2020-26164/)).

**Documented complaints**
- File speed is 250-300 KB/s, and about 1 MiB/s at best by dragging onto the device ([KDE Discuss](https://discuss.kde.org/t/extremely-slow-file-transfers-between-android-and-ubuntu-with-kde-connect/18769)).
- A 1.4 GB file took about an hour, showing "Stalled" most of the time ([KDE Discuss](https://discuss.kde.org/t/kde-connect-file-transfer-stalled-most-of-the-time/8930)). Another report gives 10-20 Mbps on 6 GHz for multi-file sends ([KDE Discuss](https://discuss.kde.org/t/kde-connect-file-transfer-protocol-and-speed/43126)).
- One review says larger transfers "silently fail", received files land in inconsistent places, and it scores the app 5/10 ([Ctrl blog](https://www.ctrl.blog/entry/review-kde-connect.html)).
- Battery: bug 442782 (open since 2021) reports about 33% of daily battery drained while paired. The cause is the desktop daemon's 10-second TCP keep-alive, plus MPRIS players emitting about four D-Bus signals a second that get relayed to the phone ([bug 442782](https://bugs.kde.org/show_bug.cgi?id=442782)).
- Clipboard sync has been broken on Wayland for years, by design ([bug 410162](https://bugs.kde.org/show_bug.cgi?id=410162), [bug 359747](https://bugs.kde.org/show_bug.cgi?id=359747)).
- Duplicate and replayed notification floods still happen as of 25.12.3 ([KDE Discuss](https://discuss.kde.org/t/kde-connect-duplicated-notifications/40269)).
- Pairings are lost after suspend, reboot or an IP change ([bug 395749](https://bugs.kde.org/show_bug.cgi?id=395749), [Launchpad 1895469](https://bugs.launchpad.net/bugs/1895469)).
- On Windows, connections fail even with the firewall open, and the Store build's GUI is broken ([KDE Discuss](https://discuss.kde.org/t/kde-connect-trouble-connecting-windows-pc-and-android/9709), [KDE Discuss](https://discuss.kde.org/t/windows-store-version-kde-connect-not-working-gui-all-messed-up/1075)).
- On iOS the app cannot receive files from the desktop, ping works only while the app is open, and there is no notification mirroring ([How-To Geek](https://www.howtogeek.com/804091/kde-connect-arrives-on-iphone-and-ipad-for-linux-fans/)). The iOS README calls being unresponsive in the background "more or less intended" ([kdeconnect-ios](https://github.com/KDE/kdeconnect-ios)).

---

## How LocalSend works

**Discovery**
- A UDP multicast announcement goes to `224.0.0.167:53317` with `announce: true`, `alias`, `version`, `deviceModel`, `deviceType`, `fingerprint`, `port`, `protocol` (http/https) and an optional `download` flag ([protocol README](https://github.com/localsend/protocol/blob/main/README.md)).
- Peers answer with `POST /api/localsend/v2/register` (or over UDP). When multicast fails, the app scans the local subnet over HTTP on TCP 53317 ([protocol README](https://github.com/localsend/protocol/blob/main/README.md)).
- "Favorites" let users save a peer's IP address ([PCWorld](https://www.pcworld.com/article/2288153/localsend-a-free-super-fast-way-to-transfer-files-over-wi-fi.html)).
- Discovery breaks under AP isolation (umbrella issue #527), the Mac firewall (#1028), a shared VPN (#1598), and across VLANs even with IGMPv3 (#1840) ([#527](https://github.com/localsend/localsend/issues/527), [#1028](https://github.com/localsend/localsend/issues/1028), [#1598](https://github.com/localsend/localsend/issues/1598), [#1840](https://github.com/localsend/localsend/issues/1840)).

**Protocol v2.2 endpoints (all under `/api/localsend/v2/`)**

| Endpoint | Shape | Source |
|---|---|---|
| `POST /register` | Two-way peer registration | [README](https://github.com/localsend/protocol/blob/main/README.md) |
| `POST /prepare-upload` | Request: `info` (device) plus `files` map of `FileMetadata` {`id`, `fileName`, `size`, `fileType`, optional `sha256`, `preview`, `modified`, `accessed`}. Response: `sessionId` plus a `fileId -> token` map, issued only after the human accepts | [README](https://github.com/localsend/protocol/blob/main/README.md), [Go bindings](https://pkg.go.dev/github.com/GennoBou/localsend/pkg/protocol) |
| `POST /upload?sessionId&fileId&token` | Raw binary body, one file per request; may run in parallel; 422 on checksum mismatch | [README](https://github.com/localsend/protocol/blob/main/README.md) |
| `POST /cancel` | Abort the session | [README](https://github.com/localsend/protocol/blob/main/README.md) |
| `POST /prepare-download`, `GET /download` | Reverse flow for "share via link" to a browser, deliberately over plain HTTP because browsers reject self-signed certificates | [README](https://github.com/localsend/protocol/blob/main/README.md) |
| `GET /info` | Device info for debugging | [README](https://github.com/localsend/protocol/blob/main/README.md) |
| Optional `?pin=` | 401 when the PIN is missing or wrong | [README](https://github.com/localsend/protocol/blob/main/README.md) |

- v1.9.0 (April 2023) introduced protocol v2 with a v1 fallback. The notes do not record what changed between v2.0, v2.1 and v2.2 ([changelog](https://localsend.org/changelog)).

**Trust model**
- The fingerprint is the SHA-256 of a self-signed certificate, or a random string for devices on plain HTTP. It is mainly used to ignore your own announcements, not to pin peers ([README](https://github.com/localsend/protocol/blob/main/README.md)).
- Real authorisation is the human accept step, the per-file token and the optional PIN. No public third-party audit was found ([localsend notes; DEV article](https://dev.to/onsen/localsend-the-best-open-source-airdrop-alternative-1b99)).
- "Quick Save" auto-accepts every incoming transfer without a prompt ([PCWorld](https://www.pcworld.com/article/2288153/localsend-a-free-super-fast-way-to-transfer-files-over-wi-fi.html)).
- Users keep asking for per-device trust: "Trusted Device", "auto-accept only from specific devices", and a pairing system ([#861](https://github.com/localsend/localsend/issues/861), [#845](https://github.com/localsend/localsend/issues/845), [#2430](https://github.com/localsend/localsend/issues/2430)).

**Stack**
- The app is written in Flutter/Dart (Dart SDK ^3.11), with Refena for state. Refena is archived ([pubspec](https://raw.githubusercontent.com/localsend/localsend/main/pubspec.yaml), [refena](https://github.com/refena/refena)).
- Rust code (`rust_lib_localsend_app`) is bridged through flutter_rust_bridge. The sender uses `rhttp` ([DeepWiki](https://deepwiki.com/localsend/localsend/1-overview)).
- The receiver reportedly remains in Dart (low confidence, secondhand) ([localsend notes](https://localsend.org/changelog)).
- Independent Rust implementations of the protocol exist: `localsend-rs` and `wylited/localsend` (v2.1) ([crates.io](https://crates.io/crates/localsend-rs), [wylited](https://github.com/wylited/localsend)).
- `lsend` runs LocalSend's official Rust core crate as a systemd user service, with a separate control CLI ([lsend](https://github.com/MetehanYurtseven/lsend)).
- On Windows, the GUI app itself sits in the tray and starts hidden at login. It is not a separate daemon ([DeepWiki: Windows integration](https://deepwiki.com/localsend/localsend/4.2-windows-integration)).

**Licence**
- The app repo is Apache-2.0 according to the GitHub API. It has 92,739 stars and was created 2022-12-16 ([GitHub](https://github.com/localsend/localsend)).
- The protocol repo has no licence field set and is at v2.2 ([protocol repo](https://github.com/localsend/protocol)). One roundup calls the app "MIT-style" (see Conflicts) ([competitors notes; GitHub](https://github.com/localsend/localsend)).

**Performance**
- Speed ranges from 10 MB/s on 2.4 GHz to 50-60 MB/s on 5 GHz with the same router and devices ([Discussion #1551](https://github.com/localsend/localsend/discussions/1551)).
- On an Intel Mac: about 6 MB/s encrypted, 13 MB/s unencrypted on 2.4 GHz and 17 MB/s unencrypted on 5 GHz ([Miragecraft](https://www.miragecraft.com/notepad/optimizing-transfer-speed-for-localsend)).
- Some users see about 600 KB/s over HTTPS ([#1429](https://github.com/localsend/localsend/issues/1429)), and transfers to Android are often 2-3 MB/s ([android-file-transfer-mac](https://www.android-file-transfer-mac.com/localsend-transfer-speed/)).
- **File-count collapse:** sending 7,000 photos (under 1 GB) slowed from 1 MB/s at 100 files to 600 KB/s at 500 and 80 KB/s at 1,000. The same content zipped into one archive held a steady 4 MB/s on 2.4 GHz. The cost is per file, not per byte ([#1657](https://github.com/localsend/localsend/issues/1657)).
- **Direction asymmetry:** 1-2 MB/s from Windows to Android against 30-40 MB/s the other way on the same setup ([Discussion #2045](https://github.com/localsend/localsend/discussions/2045)).

**Complaints**
- There is no resume, and the request is open ([#1191](https://github.com/localsend/localsend/issues/1191)).
- There is no clipboard sync (#566, #187, #1819, #2565), and a third-party tool exists just to add it ([#566](https://github.com/localsend/localsend/issues/566), [localsend-courier](https://github.com/freaxnx01/localsend-courier)).
- On iOS the device drops off peer lists when backgrounded and transfers are lost after a few minutes ([#1468](https://github.com/localsend/localsend/issues/1468), [#2610](https://github.com/localsend/localsend/issues/2610)).
- Pending requests expire after about 30 seconds ([#2612](https://github.com/localsend/localsend/issues/2612)).
- The tray tries to hide before the state layer has initialised ([#2927](https://github.com/localsend/localsend/issues/2927)).
- The Flatpak 1.16.1 build failed to launch on Pop!_OS COSMIC Alpha 3 with an `rhttp` error raised through flutter_rust_bridge ([cosmic-epoch #1305](https://github.com/pop-os/cosmic-epoch/issues/1305)).
- On first run on iOS, receiving fails until photo permission is granted ([#2025](https://github.com/localsend/localsend/issues/2025)).

**Version history that matters**
- 1.16.0 (2024-11-03) changed the sender to a Rust HTTP client with multithreading, "if the sending device is the bottleneck". No measured speed-up was published ([changelog](https://localsend.org/changelog)).
- 1.18.0 (2026-08-10) added background transfers and a quick-settings tile on Android ([changelog](https://localsend.org/changelog), [PR #2629](https://github.com/localsend/localsend/pull/2629)).
- 1.18.1 (2026-08-11) complied with the Android 17+ network permission ([changelog](https://localsend.org/changelog)).
- 1.18.2 (2026-08-21) added IPv6, SHA-256 checksum validation, drag-and-drop "receive via link", and a first official CLI ([changelog](https://localsend.org/changelog)).

---

## Other apps worth knowing

| App | What it does better | What to steal | Licence | Source |
|---|---|---|---|---|
| Warpinator | Linux Mint's LAN tool: auto-accept-friendly folder drag-and-drop; one user logged about 183 MB/s. No resume, and one report of files being overwritten | Frictionless receive from trusted peers; folder-first UX | Open source; exact licence not in notes | [Warpinator](https://github.com/linuxmint/warpinator), [Mint forums](https://forums.linuxmint.com/viewtopic.php?t=349494), [#169](https://github.com/linuxmint/warpinator/issues/169), [overwrite report](https://forums.linuxmint.com/viewtopic.php?t=422137) |
| PairDrop | No-install browser transfer over WebRTC; pairing across networks with shared codes; persistent pairing. Slower than native apps | Code-based pairing across networks as a later idea | Open source; exact licence not in notes | [Cloudron forum](https://forum.cloudron.io/topic/9930/pairdrop-local-remote-file-sharing-in-your-browser-mobile-inspired-by-airdrop-fork-alternative-of-snapdrop/26?page=1), [Big Iron](https://www.bigiron.cc/guides/localsend-vs-pairdrop-replacing-airdrop-in-a-mixed-household) |
| Quick Share, rquickshare, NearDrop | BLE discovery plus Wi-Fi LAN or Wi-Fi Direct, so no shared network is needed. The LAN leg uses mDNS, TCP with length-prefixed protobuf, UKEY2, and a 10 s keep-alive; a "bandwidth upgrade" switches medium | Start on a slow medium, upgrade to the fastest for bulk data; mDNS on LAN | Quick Share proprietary; clients open source, licence not in notes | [NearDrop PROTOCOL.md](https://github.com/grishka/NearDrop/blob/master/PROTOCOL.md), [rquickshare](https://github.com/Martichou/rquickshare) |
| AirDrop / Continuity | BLE plus peer-to-peer Wi-Fi (AWDL); Universal Clipboard with no user action. Apple only | The UX benchmark; AWDL is not open to third parties | Proprietary | [OWL](https://github.com/seemoo-lab/owl), [Apple Universal Clipboard](https://support.apple.com/en-us/102430) |
| Phone Link / Samsung Flow | Built into Windows: calls, SMS, notification mirroring, Android apps. Flow is Samsung only | Shell-level integration sets user expectations | Proprietary | [Phone Link vs Flow](https://www.alibaba.com/product-insights/phone-link-vs-samsung-flow-is-microsoft-finally-beating-samsung-at-its-own-game.html) |
| Syncthing | Continuous sync with independently hashed 128 KiB-16 MiB blocks; headless REST API. No iOS; relay limited to 1-2 MiB/s | Block-hash addressing; UI talks to the daemon over local REST | MPL-2.0 | [BEP v1](https://docs.syncthing.net/specs/bep-v1.html), [Syncthing forum](https://forum.syncthing.net/t/file-transfer-speeds/19913) |
| croc / Magic Wormhole / sendme | Relay or PAKE transfer across NAT with scriptable CLIs. sendme (iroh) resumes verified downloads; Wormhole has no resume | Verified resume; short codes | Open source; exact licences not in notes | [getcroc](https://getcroc.com/blog/compare-file-transfer-tools), [dashbeam](https://dashbeam.net/en/compare/wormhole) |
| Valent | Lighter GTK KDE Connect client, but alpha with no stable release | Its protocol reference doc is the best public spec | Not confirmed | [Linux Uprising](https://www.linuxuprising.com/2023/02/valent-is-kde-connect-client-for-gtk.html), [Valent #163](https://github.com/andyholmes/valent/discussions/163) |
| cosmic-utils/kdeconnect | The only native COSMIC companion today (applet in the COSMIC Store); work in progress; conflicts on 1716 | Proves a D-Bus service plus libcosmic applet works on COSMIC | Not confirmed | [repo](https://github.com/cosmic-utils/kdeconnect), [#117](https://github.com/cosmic-utils/kdeconnect/issues/117) |
| lsend | A daemon (`lsendd`) plus a control CLI around LocalSend's Rust core; systemd user service; cached peers | Daemon and CLI split; proof that a LocalSend-compatible module can be embedded | Not in notes | [lsend](https://github.com/MetehanYurtseven/lsend) |

---

## What is actually fast on a LAN

**Measured throughput (different devices and methods in each row; not a controlled benchmark)**

| Path | Application throughput | Conditions | Source |
|---|---|---|---|
| Infrastructure Wi-Fi 5 GHz, good app | 50-60 MB/s | LocalSend, same router as the 10 MB/s run on 2.4 GHz | [#1551](https://github.com/localsend/localsend/discussions/1551) |
| Infrastructure Wi-Fi, outlier | ~183 MB/s | Warpinator 1.8.8 on Mint 22.1, likely wired or high-end | [Mint forums](https://forums.linuxmint.com/viewtopic.php?t=349494) |
| Infrastructure Wi-Fi, typical apps | 2-20 MB/s | LocalSend and Warpinator reports | [Miragecraft](https://www.miragecraft.com/notepad/optimizing-transfer-speed-for-localsend), [Warpinator #68](https://github.com/linuxmint/warpinator/issues/68) |
| KDE Connect share | 0.15-1 MB/s, with stalls | Android to Ubuntu | [KDE Discuss](https://discuss.kde.org/t/extremely-slow-file-transfers-between-android-and-ubuntu-with-kde-connect/18769) |
| Wi-Fi 6 phone radio ceiling | 240-280 Mbps | 2 spatial streams, dense 20 MHz channels, iperf | [shankarwifi](https://shankarwifi.com/iperf/) |
| USB tethering | 230 Mbps (USB2, 1.17 ms) vs Wi-Fi tether 30 Mbps (25.5 ms); iPhone 13: 105 vs 77 Mbps | Tether benchmarks | [XDA](https://xdaforums.com/t/usb-tethering-benchmarks-latency-and-bandwidth.4092827/), [BGR](https://www.bgr.com/2146879/mobile-hotspot-usb-tethering-speed-explained/) |
| Hotspot, LocalSend | ~30-40 Mbps | Phone to PC over a 5 GHz hotspot | [android-file-transfer-mac](https://www.android-file-transfer-mac.com/localsend-transfer-speed/) |
| Wi-Fi Direct, stock Android | 19-23 Mbit/s one hop; 10.6 at two hops; 6.2 at three | Academic study | [arXiv 2604.07889](https://arxiv.org/pdf/2604.07889) |
| Quick Share | ~20 MB/s typical; 7-8 MB/s Galaxy to Windows | Wi-Fi Direct/LAN | [androidayuda](https://en.androidayuda.com/applications/Tutorials/Quick-Share-vs.-Nearby-Share:-A-Definitive-Guide-and-Real-World-Comparison/) |
| Bluetooth 5.x | 1-3 Mbps (100 MB takes 7-13 min) | Real file transfer | [files-share.com](https://files-share.com/blog/file-transfer-over-bluetooth-speed-limits-and-better-alternatives-2026) |
| Wi-Fi Aware | No number published ("at Wi-Fi speeds") | Android docs | [Android Wi-Fi Aware](https://developer.android.com/develop/connectivity/wifi/wifi-aware) |

**The app, not the radio, is the bottleneck**
- The same LocalSend rig moved about 6 MB/s encrypted and 17 MB/s unencrypted on 5 GHz. Radios can carry several times what apps deliver ([Miragecraft](https://www.miragecraft.com/notepad/optimizing-transfer-speed-for-localsend), [shankarwifi](https://shankarwifi.com/iperf/)).
- The file-count collapse and the direction asymmetry point to per-file and per-request overhead in the software ([#1657](https://github.com/localsend/localsend/issues/1657), [#2045](https://github.com/localsend/localsend/discussions/2045)).

**Protocol choice on a LAN**
- QUIC reached about 40% of TLS 1.3-over-TCP sustained throughput at high bandwidth, because it processes packets in user space ([Fastly](https://www.fastly.com/blog/measuring-quic-vs-tcp-computational-efficiency)).
- QUIC's advantages concentrate in lossy conditions, which are rare on a healthy LAN ([quic_vs_tcp survey](https://github.com/Shenggan/quic_vs_tcp)).
- Quinn reached up to 8.22 Gbit/s in one benchmark suite, but MsQuic beats it by 50% or more under loss ([ETH thesis](https://nsg.ee.ethz.ch/files/public/theses/2024-quic_benchmark_suite/thesis-1.pdf)).
- iroh reached 1.0 in June 2026. It dials by key, skips hole punching on a shared LAN, falls back to relays, and ships Swift and Kotlin bindings. No LAN throughput figure was found ([TechTimes](https://www.techtimes.com/articles/318490/20260616/peer-peer-library-iroh-10-ships-dial-devices-key-not-ip-address.htm), [iroh docs](https://docs.iroh.computer/what-is-iroh)).
- WebRTC in a Rust implementation measured 689 Mbps on one data channel, but an academic LAN evaluation measured about 145 Mbps. The spread is large ([webrtc.rs](https://webrtc.rs/blog/2026/07/31/announcing-webrtc-v0.20.0.html), [Helsinki](https://tuhat.helsinki.fi/ws/portalfiles/portal/167373638/Eskola_webrtc.pdf)).
- The transport notes argue that raw TCP+TLS 1.3 with pipelined chunks is enough for one large file, while HTTP/2 multiplexing mainly helps with many small files (see Conflicts) ([transport notes; Fastly](https://www.fastly.com/blog/measuring-quic-vs-tcp-computational-efficiency)).

**Parallel streams, buffers, sendfile, crypto**
- Parallel TCP streams mainly offset one connection's backoff after loss. On Wi-Fi, one practitioner found extra streams did not help because "the bottleneck is the path" ([Pham](https://cpham.perso.univ-pau.fr/TCP/11_220.pdf)).
- Nginx deployments commonly use 1 MiB per `sendfile()` call; 2 MB+ chunks maximise throughput when there are few clients ([getpagespeed](https://www.getpagespeed.com/server-setup/nginx/nginx-sendfile-tcp-nopush-tcp-nodelay)).
- `sendfile()` avoids the copy through user space and measured about 8% faster than read/write in informal tests ([superpat](https://blog.superpat.com/zero-copy-in-linux-with-sendfile-and-splice), [dev.to](https://dev.to/douglasmakey/optimizing-large-file-transfers-in-linux-with-go-an-exploration-of-tcp-and-syscall-15eo)).
- ARMv8 crypto extensions make AES-GCM about 4x faster on midrange phones. ChaCha20 is the better default only without AES hardware ([khimananda](https://khimananda.com/blog/aes-vs-chacha20-which-cipher-and-when), [ADHDecode](https://adhdecode.com/cryptography/secure-hardware-and-physical-security/aes-ni-hardware-deep/)).

**Direct and alternative links**
- Wi-Fi Direct is not deprecated in current Android docs ([Android Wi-Fi Direct](https://developer.android.com/develop/connectivity/wifi/wifi-direct)).
- On Windows, Wi-Fi Direct is exposed through the `Windows.Devices.WiFiDirect` API ([Microsoft Q&A](https://answers.microsoft.com/en-us/windows/forum/all/how-to-implement-wifi-direct-between-windows10-and/4ca49250-38a0-4327-9f39-47284a9af800)). On Linux it runs through the `wpa_supplicant` P2P module ([w1.fi](https://w1.fi/wpa_supplicant/devel/p2p.html)).
- Android has supported Wi-Fi Aware since 8.0. Android 13 needs `NEARBY_WIFI_DEVICES`, and some devices cannot run Aware alongside Wi-Fi Direct, a SoftAP or tethering ([Android Wi-Fi Aware](https://developer.android.com/develop/connectivity/wifi/wifi-aware)).
- iOS Wi-Fi Aware support ("iOS 18 / iOS 26 SDK, late 2025") comes from a single blog source and is **unverified** ([Medium](https://arpitkulsh.medium.com/beyond-hotspots-wi-fi-aware-nan-and-the-rise-of-proximity-native-apps-87c9be5dc5f9)).
- On iOS, MultipeerConnectivity picks infrastructure Wi-Fi, peer-to-peer Wi-Fi or Bluetooth automatically. Apple publishes no throughput figures ([Apple forums](https://developer.apple.com/forums/tags/multipeerconnectivity)).
- Conclusion: prefer 5 or 6 GHz infrastructure Wi-Fi. Treat USB tethering as the fast fallback, and hotspot or Wi-Fi Direct as compatibility options, not speed features ([transport notes; arXiv](https://arxiv.org/pdf/2604.07889), [XDA](https://xdaforums.com/t/usb-tethering-benchmarks-latency-and-bandwidth.4092827/)).

**Discovery mechanisms and platform gotchas**
- On Android, the Wi-Fi stack drops inbound multicast unless the app holds a reference-counted `MulticastLock`, which "can cause a noticeable battery drain" ([MulticastLock](https://stuff.mit.edu/afs/sipb/project/android/docs/reference/android/net/wifi/WifiManager.MulticastLock.html)). A missing lock silently breaks mDNS ([example PR](https://github.com/ThaerM/Remote-TV-2026-/pull/9)).
- `NsdManager` has been available since API 16 ([NsdManager](https://developer.android.com/reference/android/net/nsd/NsdManager)).
- iOS 14+ requires `NSLocalNetworkUsageDescription` and the exact service types in `NSBonjourServices` ([Apple forums](https://developer.apple.com/forums/thread/672041), [TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)). The permission behaved inconsistently across iOS 17.x and 18.x ([Apple forums 790307](https://developer.apple.com/forums/thread/790307)). A denied permission suppresses discovery silently ([stack notes; PTKD](https://ptkd.com/journal/ios-local-network-privacy-permission)).
- `mdns-sd` is a pure-Rust mDNS-SD crate. `tauri-plugin-dns-sd` uses it on desktop but hands off to `NWBrowser`/`NWListener` and `NsdManager` on mobile ([tauri-plugin-dns-sd](https://docs.rs/tauri-plugin-dns-sd/latest/tauri_plugin_dns_sd/), [mdns-sd](https://github.com/keepsimple1/mdns-sd)).
- Quick Share uses mDNS on its LAN leg ([NearDrop PROTOCOL.md](https://github.com/grishka/NearDrop/blob/master/PROTOCOL.md)).
- No source compared broadcast, multicast and BLE advertising for battery cost or reliability. Windows Firewall behaviour towards mDNS is also unresearched ([transport notes gap; NsdManager](https://developer.android.com/reference/android/net/nsd/NsdManager)).

**Resume patterns**
- anywhere-file uses 4 MiB chunks with a BLAKE3 hash per chunk and a persisted transfer ID. It resumes within 24 hours and re-requests any chunk that fails its hash ([anywhere-file #16](https://github.com/achmadss/anywhere-file/issues/16)).
- bridgeos uses 64 KB chunks and `.part` staging. On resume it truncates back to the last contiguous verified chunk, and it keeps both per-chunk and whole-file BLAKE3 root hashes ([bridgeos](https://github.com/atharvmantri/bridgeos)).
- BLAKE3 hashes 1 KiB chunks into a Merkle tree, so each part can be verified independently. A whole-file SHA-256 cannot verify partial progress ([quantumsequrity](https://quantumsequrity.com/blog/blake3-hashing)).
- The pattern works with any hash: zblob uses SHA-256 ([zblob](https://github.com/p13marc/zblob)).
- No benchmark of chunk size or of BLAKE3's cost on a phone at 100+ MB/s was found. Measure both ([transport notes gap; bridgeos](https://github.com/atharvmantri/bridgeos)).

---

## Platform ceilings

| Platform | Works well | Hard limit or main risk | PlayDrop response | Source |
|---|---|---|---|---|
| Android | A foreground service is exempt from Doze; share intents; the full companion set | Android 14 requires a declared FGS type; Android 15 caps `dataSync` at 6 h per 24 h and blocks it from `BOOT_COMPLETED`; Play restricts battery-exemption requests, notification access and AccessibilityService | `connectedDevice` FGS; ask for battery exemption only after a user action; remote input injected on the desktop side | [FGS 14](https://developer.android.com/about/versions/14/changes/fgs-types-required), [FGS 15](https://developer.android.com/about/versions/15/changes/foreground-service-types), [Doze](https://developer.android.com/training/monitoring-device-state/doze-standby) |
| iOS | Foreground send and receive; share extension; Bonjour after consent | No background listener; no cross-app clipboard events; cannot read other apps' notifications; sockets die on suspend | Sender plus foreground receiver; desktop outbox pulled on open; no always-on claims | [BackgroundTasks](https://developer.apple.com/documentation/backgroundtasks), [quinn #2011](https://github.com/quinn-rs/quinn/issues/2011) |
| Windows | `Shell_NotifyIcon`, `SendInput`, `AddClipboardFormatListener`, app notifications | Firewall alert on first listen; `UserNotificationListener` and `StartupTask` want MSIX identity; MSIX makes the firewall rule harder | Classic installer that adds the firewall rule; MSIX later | [netsh](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/netsh-advfirewall), [StartupTask](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.startuptask?view=winrt-26100) |
| Pop!_OS COSMIC | SNI tray, freedesktop notifications, `ext-data-control-v1` clipboard, MPRIS | RemoteDesktop portal only since August 2026 and buggy; no COSMIC Files extension API found | Tray plus window; data-control clipboard; "Open with PlayDrop" `.desktop`; input later | [cosmic-applets](https://deepwiki.com/pop-os/cosmic-applets/3.4-status-area-and-minimize), [cosmic-comp #2869](https://github.com/pop-os/cosmic-comp/issues/2869) |
| Ubuntu GNOME | Notifications, MPRIS, XDG autostart | Mutter has no data-control; the tray needs the AppIndicator extension; Shell extensions break across releases | Window first; clipboard only through an in-window action; no Shell extension in v1 | [mutter #524](https://gitlab.gnome.org/GNOME/mutter/-/work_items/524), [tray_manager](https://pub.dev/packages/tray_manager) |

**Android**
- Android 14 made a declared foreground service type mandatory. `connectedDevice` needs `FOREGROUND_SERVICE_CONNECTED_DEVICE` plus one of `CHANGE_NETWORK_STATE`, `CHANGE_WIFI_STATE`, `CHANGE_WIFI_MULTICAST_STATE`, NFC, IR or Bluetooth. A missing type throws `MissingForegroundServiceTypeException` ([FGS 14](https://developer.android.com/about/versions/14/changes/fgs-types-required)).
- In Android 15, `dataSync` gets 6 hours per rolling 24 hours, and `camera`, `dataSync`, `mediaPlayback`, `mediaProjection` and `phoneCall` cannot start from `BOOT_COMPLETED` ([FGS 15](https://developer.android.com/about/versions/15/changes/foreground-service-types)).
- `dataSync` is reportedly headed for deprecation in favour of WorkManager-style jobs (secondary source) ([dev.to](https://dev.to/joe_wang_6a4a3e51566e8b52/android-foreground-services-in-2026-what-changed-and-how-to-adapt-2o3d)).
- Play allows a battery-optimisation exemption request only when the core function would break without it, and only after an explicit user action ([Play community](https://support.google.com/googleplay/android-developer/thread/330168645/clarification-on-foreground-service-data-sync-and-request-ignore-battery-optimizations?hl=en)).
- `NotificationListenerService` needs special access from a Settings screen and a core feature that is visible in the store listing ([Android reference](https://developer.android.com/reference/android/service/notification/NotificationListenerService), [Play thread](https://support.google.com/googleplay/android-developer/thread/286379219/policy-questions-about-notificationlistenerservice-requesting-battery-optimizations-and-body-sensors?hl=en)).
- AccessibilityService is restricted for apps that are not accessibility tools, with tightening reported from 28 January 2026 (secondhand date) ([Play policy](https://support.google.com/googleplay/android-developer/answer/10964491?hl=en), [Malwarebytes](https://www.malwarebytes.com/blog/mobile/2026/03/google-cracks-down-on-android-apps-abusing-accessibility)). Using the phone as a trackpad injects input on the desktop, so it avoids this policy ([report reasoning; Play policy](https://support.google.com/googleplay/android-developer/answer/10964491?hl=en)).
- OEM battery optimisation can still kill foreground services ([tauri-plugin-background-service](https://docs.rs/tauri-plugin-background-service)).

**iOS**
- Apps are suspended shortly after going to the background ([BackgroundTasks](https://developer.apple.com/documentation/backgroundtasks)).
- `UIBackgroundModes` is a fixed list with no "run a server" mode ([UIBackgroundModes](https://developer.apple.com/documentation/bundleresources/information-property-list/uibackgroundmodes)). Guideline 2.5.4 limits background services to their intended purposes ([App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)).
- BGTaskScheduler grants roughly 30-second windows, every 15 minutes or more, at the OS's discretion. Force-quitting kills them ([tauri-plugin-background-service](https://docs.rs/tauri-plugin-background-service)).
- Once the app is suspended, low-level networking stops, but `URLSession` background sessions keep working ([Apple forums](https://developers.apple.com/forums/thread/685525)).
- iOS marks suspended apps' sockets defunct (`SOF_DEFUNCT`), and TCP is affected too ([quinn #2011](https://github.com/quinn-rs/quinn/issues/2011)).
- `UIPasteboardChangedNotification` is not delivered for other apps' changes ([forum 760154](https://developer.apple.com/forums/thread/760154), [forum 110504](https://developer.apple.com/forums/thread/110504)).
- Reading other apps' notifications is only possible as a bonded BLE ANCS accessory, not from a LAN app ([ANCS spec](https://developer.apple.com/library/archive/documentation/CoreBluetooth/Reference/AppleNotificationCenterServiceSpecification/Specification/Specification.html), [kdeconnect-ios](https://github.com/KDE/kdeconnect-ios)).
- Local Push Connectivity (`NEAppPushProvider`, iOS 14+) keeps an extension running while the device is on configured Wi-Fi networks. It is entitlement-gated and Apple must grant it ([Apple docs](https://developer.apple.com/documentation/networkextension/local-push-connectivity), [WWDC20 10113](https://developer.apple.com/videos/play/wwdc2020/10113/)).
- Guideline 4.2.7 allows LAN remote-desktop apps to a host the user owns ([App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)).

**Windows**
- A listener that does not use the Firewall API triggers a Security Alert. An elevated `netsh advfirewall` rule at install time pre-authorises it ([netsh advfirewall](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/netsh-advfirewall)).
- MSIX apps cannot use the `Run` key and must declare `StartupTask`. Once the user disables it, the app cannot silently re-enable it ([StartupTask](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.startuptask?view=winrt-26100), [Advanced Installer](https://www.advancedinstaller.com/startup-programs-msix.html)).
- `UserNotificationListener` needs `RequestAccessAsync` consent ([Microsoft Learn](https://learn.microsoft.com/en-us/uwp/api/windows.ui.notifications.management.usernotificationlistener?view=winrt-26100)). The need for package identity comes only from developer reports ([stack notes; Microsoft Learn](https://learn.microsoft.com/en-us/uwp/api/windows.ui.notifications.management.usernotificationlistener?view=winrt-26100)).
- `AppNotificationManager` sends notifications from unpackaged and packaged apps alike ([Microsoft Learn](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/app-notifications/app-notifications-quickstart)).
- `SendInput` and `AddClipboardFormatListener` need no package identity ([SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput), [AddClipboardFormatListener](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-addclipboardformatlistener)).
- WinUI 3 has no tray API, so apps call `Shell_NotifyIcon` directly ([albertakhmetov](https://albertakhmetov.com/posts/2025/using-notifyicon-in-winui-3/)).

**Pop!_OS COSMIC**
- COSMIC is a Rust/iced Wayland-only desktop with its own compositor, cosmic-comp. It has been the default on Pop!_OS 24.04 LTS since around December 2025 ([Wikipedia](https://en.wikipedia.org/wiki/COSMIC_desktop), [The Register](https://www.theregister.com/2025/12/22/popos_2404_cosmic_epoch_1/)). Exact release dates vary between sources ([9to5Linux](https://9to5linux.com/system76-launches-first-stable-release-of-cosmic-desktop-and-pop_os-24-04-lts)).
- Applets are separate libcosmic processes, not in-shell JavaScript. There is no GNOME Shell compatibility layer ([DeepWiki libcosmic](https://deepwiki.com/pop-os/libcosmic/4-applet-development)).
- The Status Area applet is a full StatusNotifierWatcher and host ([DeepWiki cosmic-applets](https://deepwiki.com/pop-os/cosmic-applets/3.4-status-area-and-minimize)).
- `cosmic-notifications` implements the full freedesktop notification spec ([cosmic-notifications](https://github.com/pop-os/cosmic-notifications)).
- Clipboard: COSMIC 1.6 uses `ext-data-control-v1`. The older wlr protocol needs `COSMIC_DATA_CONTROL_ENABLED=1` and gives every window global clipboard access ([NixOS wiki](https://wiki.nixos.org/wiki/COSMIC)). wlr-data-control is deprecated in favour of the ext protocol ([wl-clipboard #242](https://github.com/bugaevc/wl-clipboard/issues/242)).
- Remote input: the RemoteDesktop portal landed in `xdg-desktop-portal-cosmic` in August 2026, and its consent dialog keeps reappearing ([cosmic-comp #2869](https://github.com/pop-os/cosmic-comp/issues/2869)). KDE Connect on Plasma 6.1+ uses the portal with libei ([KDE Discuss](https://discuss.kde.org/t/more-easily-access-kde-connects-remote-input/11590)).
- MPRIS works the same everywhere ([MPRIS spec](https://specifications.freedesktop.org/mpris/latest/), [DeepWiki Audio applet](https://deepwiki.com/pop-os/cosmic-applets/4.1-audio-and-media)).
- No COSMIC Files extension API was found. A `.desktop` "Open with" entry is the portable route ([linux notes; sakopc](https://sakopc.wordpress.com/2023/01/24/how-to-add-send-through-kde-connect-in-nautilus-context-menu/)).
- Packaging: the COSMIC Store installs both `.deb` (through APT) and Flatpak ([DeepWiki cosmic-store](https://deepwiki.com/pop-os/cosmic-store)).
- Autostart: an XDG autostart entry plus D-Bus activation. The D-Bus activation sometimes never fires, and users add a systemd user unit with `Restart=on-failure` ([Fedora discussion](https://discussion.fedoraproject.org/t/disable-kdeconnectd-autostart-and-integrate-it-to-systemd-and-dbus-services/183430), [Arch forum](https://bbs.archlinux.org/viewtopic.php?id=255368)).

**Ubuntu GNOME**
- Mutter implements no data-control protocol, as a deliberate security stance. The working route is a Shell extension ([mutter #524](https://gitlab.gnome.org/GNOME/mutter/-/work_items/524)). Clipboard fails on Ubuntu 26.04 GNOME Wayland for a Rust app that falls back to X11 ([clockode #22](https://github.com/mariinkys/clockode/issues/22)).
- GSConnect is in Ubuntu's archive (55-4 on 24.04) but breaks across GNOME releases. Notifications re-fire endlessly on GNOME 46, and a clipboard API change needed fixing on GNOME 51 ([Launchpad](https://launchpad.net/ubuntu/noble/amd64/gnome-shell-extension-gsconnect/55-4), [#1855](https://github.com/GSConnect/gnome-shell-extension-gsconnect/issues/1855), [utah PR #89](https://github.com/projectbluefin/utah/pull/89)).
- GSConnect clipboard sync works only when the app is focused and pushed by hand ([#1714](https://github.com/GSConnect/gnome-shell-extension-gsconnect/issues/1714)).
- GNOME does not support StatusNotifierItem, and Flutter tray plugins may need the AppIndicator extension just to show an icon ([Avalonia TrayIcon](https://docs.avaloniaui.net/docs/reference/controls/tray-icon), [tray_manager](https://pub.dev/packages/tray_manager)).
- Ubuntu 26.04 "Resolute Raccoon" is the current LTS. The note recorded a 27 August 2026 date from the LTS-upgrade summary page; LTS releases ship in April, so that date is most likely the 26.04.1 point release and is **unverified**. Its GNOME version was not found ([Ubuntu release notes](https://documentation.ubuntu.com/release-notes/26.04/summary-for-lts-users/)).
- Nautilus integration uses a `nautilus-python` script, as KDE Connect ships ([kdeconnect-share.py](https://github.com/KDE/kdeconnect-kde/blob/master/nautilus-extension/kdeconnect-share.py)).

**Flatpak**
- The tray needs `--talk-name=org.kde.StatusNotifierWatcher`, and sending notifications needs `--talk-name=org.freedesktop.Notifications`. The baseline also includes `--share=network`, `--socket=wayland` and `--device=dri` ([Flatpak manifests](https://docs.flatpak.org/en/latest/manifests.html)).
- Reading other apps' notifications needs `--socket=session-bus` plus `BecomeMonitor`, which Flathub reviews as an exception ([Flathub Discourse](https://discourse.flathub.org/t/session-bus-access-for-forwarding-desktop-notifications/3697)).
- Prefer portals over blanket filesystem access ([Flatpak sandbox permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)).

---

## Stack decision

| Option | Evidence for | Evidence against | Source |
|---|---|---|---|
| **A. Rust core + UniFFI + native UIs** (Compose, SwiftUI, Rust desktop UI) | UniFFI runs in production in Firefox and Bitwarden, supports async and foreign traits, and needs no bundled runtime. Phones need a lot of native code anyway | Four UI layers; open UniFFI bug with the tokio runtime plus async trait interfaces (#2576) | [UniFFI](https://mozilla.github.io/uniffi-rs/), [Bitwarden SDK](https://contributing.bitwarden.com/architecture/sdk/), [#2576](https://github.com/mozilla/uniffi-rs/issues/2576) |
| **B. Flutter + flutter_rust_bridge** (LocalSend's path) | One UI; Canonical now leads Flutter desktop; Android FGS plugins exist | The Linux embedder is GTK3 with no server-side decorations (#94381) and no `xdg_toplevel` handle; the tray needs AppIndicator libraries; background-isolate MethodChannel bugs (#83292); gaps in Rust-to-Dart callbacks; LocalSend's COSMIC Flatpak crash | [OMG Ubuntu](https://www.omgubuntu.co.uk/2026/05/flutter-desktop-canonical-maintained), [#94381](https://github.com/flutter/flutter/issues/94381), [#83292](https://github.com/flutter/flutter/issues/83292), [FRB #1925](https://github.com/fzyzcjy/flutter_rust_bridge/discussions/1925), [cosmic-epoch #1305](https://github.com/pop-os/cosmic-epoch/issues/1305) |
| **C. Kotlin Multiplatform / Compose MP** | CMP for iOS has been stable since 1.8.0 (May 2025); 1.11.0 released May 2026 | Linux desktop runs through XWayland; the official tray is built on unmaintained AWT; a JVM must be bundled | [CMP 1.8](https://blog.jetbrains.com/kotlin/2025/05/compose-multiplatform-1-8-0-released-compose-multiplatform-for-ios-is-stable-and-production-ready/), [compose-jb #1631](https://github.com/JetBrains/compose-jb/issues/1631), [#3684](https://github.com/JetBrains/compose-multiplatform/issues/3684) |
| **D. Tauri v2** (and React Native) | The desktop side is mature; there is a background-service plugin | Mobile is "less mature"; guidance points to UniFFI or FRB for production mobile. React Native needs hand-written modules for FGS and Bonjour | [Tauri mobile](https://v2.tauri.app/develop/plugins/develop-mobile/), [RN background 2026](https://www.72technologies.com/blog/react-native-background-tasks-ios-android-2026) |
| **E. .NET MAUI / Avalonia** | Avalonia 12 covers mobile; native Wayland is in 12.1 private preview | MAUI has no GA Linux path (the Avalonia backend is in preview); the Avalonia tray does not work on GNOME | [The Register](https://www.theregister.com/2025/11/13/dotnet_maui_linux_avalonia/), [Avalonia Wayland](https://avaloniaui.net/blog/bringing-wayland-support-to-avalonia) |
| **F. Rust desktop UI: Slint / libcosmic / iced** | Slint has tray support on Wayland, X11 and Windows under GPLv3, royalty-free or commercial licences; libcosmic is COSMIC-native | Slint's licence worries some; iOS support is unconfirmed; libcosmic is thinly documented, without stability promises, and reportedly forked from iced | [Slint FAQ](https://github.com/slint-ui/slint/blob/master/FAQ.md), [slint.dev](https://slint.dev/), [Lobsters survey](https://lobste.rs/s/83yugk/2026_survey_rust_gui_libraries) |

Supporting evidence:
- Syncthing's precedent: a modern mobile client runs the core as a daemon and the UI calls it over local REST, because Android SAF and Go paths never mapped cleanly. Keep storage on the native side and pass file descriptors or streams to the core ([SyncUp](https://github.com/siddarthkay/syncup), [Syncthing forum](https://forum.syncthing.net/t/using-go-on-mobile-discussion-about-alternatives/9994)).
- iroh found maintaining bindings was "a lot of work" because its API kept changing before 1.0. Freeze the core API before generating bindings ([iroh](https://www.iroh.computer/blog/the-road-to-iroh-1-0)).
- No Android-specific problem was found with running tokio inside a foreground service ([Doze docs](https://developer.android.com/training/monitoring-device-state/doze-standby)).
- Mature Linux crates exist: `zbus`, `ksni`, `mpris`, `wl-clipboard-rs`, `ashpd` and `reis` ([ksni](https://github.com/iovxw/ksni), [ashpd](https://github.com/bilelmoussaoui/ashpd), [reis](https://crates.io/crates/reis/dependencies), [mpris](https://docs.rs/mpris/latest/mpris/struct.Player.html)). `notify-rust` and the maturity of `reis` were not verified ([linux notes gaps; reis](https://crates.io/crates/reis/dependencies)).

**Recommendation:** use option A. That means a Rust core with UniFFI, Kotlin and Compose on Android, SwiftUI on iOS, and a Rust desktop daemon that draws its own tray (`ksni` on Linux, `Shell_NotifyIcon` on Windows), with one Slint window for Windows and Linux. A libcosmic applet is optional polish later. This follows the stack notes' synthesis ([stack notes section 10; UniFFI](https://mozilla.github.io/uniffi-rs/)).

**Alternative:** Flutter for all UIs through flutter_rust_bridge. It is defensible for a very small team, provided the UI never hosts the network server ([stack notes; cosmic-epoch #1305](https://github.com/pop-os/cosmic-epoch/issues/1305)).

**Decided 2026-10-08:** option A. The build is fully agentic, so UI code volume is not the constraint; platform fit is. Desktop UI toolkit: Slint first because it has tray support and documentation agents can follow; libcosmic applet as a COSMIC-specific extra once the Slint window works.

---

## Architecture

```
+---------------------------------------------------------------+
| UI shells (thin clients, never own the listening socket)      |
|  Android: Kotlin/Compose  iOS: SwiftUI + share extension      |
|  Desktop: Slint window + tray (ksni / Shell_NotifyIcon)       |
+-------------------------------+-------------------------------+
                                | UniFFI (mobile) / local IPC (desktop:
                                | D-Bus on Linux, named pipe on Windows)
+-------------------------------v-------------------------------+
| Hosts                                                          |
|  playdropd (Linux, Windows)   Android FGS (connectedDevice)     |
|  iOS app process (foreground only)                              |
+-------------------------------+-------------------------------+
                                |
+-------------------------------v-------------------------------+
| playdrop-core (Rust): identity, pin store, discovery, pairing, |
| TLS 1.3, HTTP/2, session plane, transfer engine, resume        |
| No platform storage paths: hosts pass fds/streams              |
+---------------------------------------------------------------+
```
- The UI must never own the socket. This avoids LocalSend's pattern of the GUI acting as the listener and its tray race, and copies KDE Connect's separate daemon model ([DeepWiki](https://deepwiki.com/localsend/localsend/4.2-windows-integration), [#2927](https://github.com/localsend/localsend/issues/2927), [Fedora discussion](https://discussion.fedoraproject.org/t/disable-kdeconnectd-autostart-and-integrate-it-to-systemd-and-dbus-services/183430)).
- The IPC choices (D-Bus and named pipe) are design choices, not research findings.

**Identity and pairing**
- Each device generates a keypair and a self-signed certificate once. The Device ID is the SHA-256 of the public key, so it does not depend on the IP address. This fixes KDE Connect's class of bugs where pairing is lost when the network changes ([bug 395749](https://bugs.kde.org/show_bug.cgi?id=395749), [competitors notes inference](https://discuss.kde.org/t/kdeconnect-loses-connection-and-must-be-re-paired/14335)).
- Pairing shows a short authentication string derived from both certificates on both screens, as KDE Connect's key-hash comparison does ([KDE privacy policy](https://community.kde.org/KDEConnect/PrivacyPolicy)). Alternatively the user scans a QR code carrying the fingerprint, addresses and a one-time secret. Our own SAS algorithm is specified in Phase 0, because KDE's could not be found.
- Accepting pins the peer's key. Auto-accept is granted per pinned device only, never per network, name or IP. This is the feature LocalSend users keep asking for ([#861](https://github.com/localsend/localsend/issues/861), [#2430](https://github.com/localsend/localsend/issues/2430)).

**Discovery: concurrent paths, ranked by trust**

Paths 1 to 3 run at the same time, not one after another. `NsdManager` on Android is slow and unreliable enough (multi-second resolves, silent failures without the multicast lock) that waiting for it before trying the others would make the device list feel broken. The first path that yields a pinned peer wins; the ranking only decides which address to prefer when several answer.
1. mDNS/DNS-SD `_playdrop._tcp`, using `NsdManager` on Android, `NWBrowser`/`NWListener` on iOS and `mdns-sd` on desktop ([tauri-plugin-dns-sd](https://docs.rs/tauri-plugin-dns-sd/latest/tauri_plugin_dns_sd/), [TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)).
2. A UDP multicast beacon on our own group and port as a second signal ([LocalSend README](https://github.com/localsend/protocol/blob/main/README.md)).
3. Direct dial to the last known addresses of pinned peers, started immediately on app or daemon start ([lsend peers cache](https://github.com/MetehanYurtseven/lsend)).
4. Manual IP, QR, hotspot instructions or USB tether instructions. These are v1 features because the user is often on mesh, VPN and guest networks ([KDE Discuss](https://discuss.kde.org/t/kde-connect-behind-a-vpn/46037)).
- No silent full-subnet scan by default. When discovery fails, say which layer failed: multicast blocked, port closed, or Local Network permission denied ([Apple forums 790307](https://developer.apple.com/forums/thread/790307), [#527](https://github.com/localsend/localsend/issues/527)).
- Client isolation blocks device-to-device traffic entirely, and no discovery protocol can fix it. The honest answer is diagnosis plus the manual path ([kde_connect notes inference; #527](https://github.com/localsend/localsend/issues/527)).

**Wire**
- One TCP port serves TLS 1.3 with mutual certificate authentication and HTTP/2. This avoids KDE Connect's second socket per payload, and a single firewall rule covers everything ([Valent](https://valent.andyholmes.ca/documentation/protocol.html)). Prefer AES-GCM when crypto extensions are present ([khimananda](https://khimananda.com/blog/aes-vs-chacha20-which-cipher-and-when)).
- HTTP/2 flow control must be tuned or it caps bulk throughput. The `h2` crate defaults to a 64 KiB stream window and a 64 KiB connection window; raise both to several MiB on both sides, and set the socket send and receive buffers to match. Phase 1's first benchmark compares tuned HTTP/2 against a raw length-prefixed framing over the same TLS socket. If HTTP/2 is more than about 10% slower after tuning, the transfer plane switches to raw framing and HTTP/2 stays only for the session plane. This is a design decision, not a research finding.
- The session plane is one long-lived bidirectional HTTP/2 stream of small versioned frames. It opens with `hello {version, capabilities}`, modelled on KDE Connect's in and out capabilities ([Valent](https://valent.andyholmes.ca/documentation/protocol.html)). Planned frames: `ping`, `find`, `clipboard`, `battery`, `media`, `notification`, `presence`, `outbox_ready`. Push, don't poll, as KDE deprecated polling ([kdeconnect-meta](https://github.com/KDE/kdeconnect-meta/blob/work/protocol-schemas/protocol.md)).
- The transfer plane is HTTP/2 requests on the same connection:

| Operation | Purpose | Improvement over LocalSend v2 |
|---|---|---|
| `POST /v1/transfers` | Offer a manifest: names, sizes, mtimes, chunk size. No hashes up front, so a 4 GB video starts moving immediately instead of after a full read | Receiver returns per-file tokens and the verified byte offsets already on disk |
| `PUT /v1/transfers/{id}/files/{fid}` + `Content-Range` | Stream a file, or resume it from an offset | Many concurrent streams on one connection, with no handshake per file |
| `PUT /v1/transfers/{id}/bundle` | Stream a tar of many tiny files | Removes the per-file cost behind the collapse at 1,000 files ([#1657](https://github.com/localsend/localsend/issues/1657)) |
| `GET /v1/transfers/{id}` | Progress and resume state | Either side can reconnect after a Wi-Fi drop |
| `DELETE /v1/transfers/{id}` | Cancel | Same as `/cancel` |
| `GET /v1/outbox` | Receiver pulls the files queued for it | A phone that cannot listen can still receive (iOS) |

**Integrity and resume**
- Use fixed chunks, starting at 1 MiB and benchmarked from 256 KiB to 4 MiB. Projects in the notes use 64 KiB to 4 MiB ([anywhere-file](https://github.com/achmadss/anywhere-file/issues/16), [bridgeos](https://github.com/atharvmantri/bridgeos)).
- Hash each chunk with BLAKE3 while streaming, not before offering. The sender computes each chunk hash as it reads the chunk and sends it in a trailing frame after the chunk's bytes (or as an HTTP/2 trailer per chunk-sized request when using HTTP/2). The receiver verifies each chunk on arrival and records the verified offset. The whole-file root comes from the chunk tree at the end, so there is no pre-read and no second pass ([BLAKE3](https://quantumsequrity.com/blog/blake3-hashing), [bridgeos](https://github.com/atharvmantri/bridgeos)). On resume, the receiver reports the last verified offset; the sender re-hashes only from there.
- Stage incoming data in `.part` files. On reconnect, truncate back to the last verified contiguous chunk and re-request any chunk that fails its hash. Persist the transfer ID on both sides ([bridgeos](https://github.com/atharvmantri/bridgeos), [anywhere-file](https://github.com/achmadss/anywhere-file/issues/16)).
- Chunk addressing later makes "skip chunks the receiver already has" cheap ([Syncthing BEP](https://docs.syncthing.net/specs/bep-v1.html)).

**Connection lifecycle**

| Platform | Rule | Source |
|---|---|---|
| Desktop | Session always up; the daemon starts from XDG autostart plus D-Bus activation, with an optional systemd user unit for restarts; the Windows Run key for the classic installer | [Fedora](https://discussion.fedoraproject.org/t/disable-kdeconnectd-autostart-and-integrate-it-to-systemd-and-dbus-services/183430), [StartupTask](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.startuptask?view=winrt-26100) |
| Android | Session lives inside the `connectedDevice` FGS; keep-alive measured in minutes, not 10 s; MulticastLock only during discovery windows; reconnect to the last-known address on Wi-Fi change | [bug 442782](https://bugs.kde.org/show_bug.cgi?id=442782), [MulticastLock](https://stuff.mit.edu/afs/sipb/project/android/docs/reference/android/net/wifi/WifiManager.MulticastLock.html) |
| iOS | Assume sockets die on suspend and rebuild them on every foreground; pull the outbox on open; prototype `URLSession` background sessions for big pulls and Local Push Connectivity for alerts | [quinn #2011](https://github.com/quinn-rs/quinn/issues/2011), [Apple forums](https://developers.apple.com/forums/thread/685525), [Local Push](https://developer.apple.com/documentation/networkextension/local-push-connectivity) |
| iOS share extension | Extensions run under a small memory ceiling (about 120 MB, unverified exact figure) and cannot stream large files themselves. The extension copies or links the item into the App Group container, writes a queue entry, and opens the main app via a URL scheme; the main app performs the send. Small text and clipboard sends may go directly from the extension | Design decision; memory figure is general iOS knowledge, not from the notes |
| Phone to phone | Android to Android: both hold a listening FGS, direct connection. Android to iPhone: the Android side holds the outbox and the iPhone pulls on open. iPhone to iPhone: both foreground, direct connection, no background case | Follows from the iOS limits above |
| All | Throttle and coalesce media-state frames (KDE relayed about 4 MPRIS signals a second) | [bug 442782](https://bugs.kde.org/show_bug.cgi?id=442782) |

**Threat-model rules from CVE-2020-26164** ([oss-sec](https://seclists.org/oss-sec/2020/q4/56))
1. An unauthenticated discovery packet never causes an outbound connection on its own. PlayDrop dials only pinned peers or a peer the user picked.
2. Discovery input is rate-limited, and per-device state from discovery is capped and expires.
3. Friendly names never feed a trust decision. The UI always shows the key-derived identity next to the name.
4. Any browser "receive via link" mode is labelled as an unencrypted convenience path, as LocalSend's is ([README](https://github.com/localsend/protocol/blob/main/README.md)).

**Speed targets** (proposed by us, not taken from the research)
- At least 50% of `iperf3` on the same link for one large file as the v1 floor, 80% as the stretch goal. Phone flash write speed and TLS on a mid-range SoC make "within 20%" unrealistic on phones; measure before promising.
- No throughput drop between 100 files and 5,000 files.
- A cold send (discover, connect, handshake) under 1 s, and a warm clipboard or ping send under 50 ms.
- Publish a speed matrix against LocalSend and KDE Connect before shipping.

---

## Decisions

| Decision | Rationale | Source |
|---|---|---|
| A Rust core owns identity, discovery, pairing, TLS, the session plane and the transfer engine | LocalSend already moved its hot path to Rust; the crates exist | [changelog](https://localsend.org/changelog), [ksni](https://github.com/iovxw/ksni) |
| On desktop, a daemon (`playdropd`) owns the socket; the UI is a client | Fixes the GUI-as-server pattern and the tray race | [#2927](https://github.com/localsend/localsend/issues/2927), [SyncUp](https://github.com/siddarthkay/syncup) |
| Own protocol, own port; never bind 1714-1764; do not bind 53317 by default | 1716 holds one app at a time; users must be able to keep KDE Connect installed | [cosmic-utils/kdeconnect](https://github.com/cosmic-utils/kdeconnect), [GSConnect](https://github.com/shell-extensions/gsconnect/blob/main/README.md) |
| No KDE protocol 7/8 interop in v1 (a deliberate trade: we lose the existing KDE Android app users) | Avoids the second socket per payload, no-resume design and CVE-era pre-auth design | [Valent](https://valent.andyholmes.ca/documentation/protocol.html), [oss-sec](https://seclists.org/oss-sec/2020/q4/56) |
| Device ID = SHA-256(public key); pin at pairing; auto-accept per pin only | Survives IP changes; per-device trust is what users ask for | [bug 395749](https://bugs.kde.org/show_bug.cgi?id=395749), [#861](https://github.com/localsend/localsend/issues/861) |
| One TCP port, TLS 1.3 mutual auth, HTTP/2, session and transfer planes | One firewall rule; multiplexing removes the per-file cost | [#1657](https://github.com/localsend/localsend/issues/1657), [Fastly](https://www.fastly.com/blog/measuring-quic-vs-tcp-computational-efficiency) |
| No QUIC, WebRTC or Wi-Fi Direct as the v1 bulk path | QUIC ~40% of TCP+TLS; wide WebRTC spread; Wi-Fi Direct ~20 Mbit/s | [Fastly](https://www.fastly.com/blog/measuring-quic-vs-tcp-computational-efficiency), [Helsinki](https://tuhat.helsinki.fi/ws/portalfiles/portal/167373638/Eskola_webrtc.pdf), [arXiv](https://arxiv.org/pdf/2604.07889) |
| Chunked BLAKE3, `.part` staging, resume from the last verified chunk, tar bundle path | A proven pattern; fixes no-resume and the file-count collapse | [bridgeos](https://github.com/atharvmantri/bridgeos), [#1191](https://github.com/localsend/localsend/issues/1191) |
| Discovery order: mDNS, then multicast beacon, then last-known dial, then manual/QR/hotspot; no silent scan | KDE is moving to mDNS; Quick Share uses it; iOS supports Bonjour | [MR !375](https://invent.kde.org/network/kdeconnect-android/-/merge_requests/375), [NearDrop](https://github.com/grishka/NearDrop/blob/master/PROTOCOL.md), [TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy) |
| Android: `connectedDevice` FGS, not `dataSync` | `dataSync` is capped at 6 h per 24 h and cannot start at boot | [FGS 15](https://developer.android.com/about/versions/15/changes/foreground-service-types) |
| iOS = sender plus foreground receiver, with a desktop outbox; no always-on claims | OS limits confirmed by KDE Connect iOS and LocalSend iOS | [kdeconnect-ios](https://github.com/KDE/kdeconnect-ios), [#1468](https://github.com/localsend/localsend/issues/1468) |
| Windows: classic installer with a firewall rule first; MSIX later | The firewall rule is simple with an installer; v1 does not need notification reading | [netsh](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/netsh-advfirewall) |
| Linux: `.deb` and Flatpak; tray through SNI; no GNOME Shell extension and no required COSMIC applet in v1 | The COSMIC Store installs both; extensions break every release | [cosmic-store](https://deepwiki.com/pop-os/cosmic-store), [#1855](https://github.com/GSConnect/gnome-shell-extension-gsconnect/issues/1855) |
| Clipboard: data-control on COSMIC, `AddClipboardFormatListener` on Windows, an in-window button on GNOME and iOS | Matches each OS's ceiling | [NixOS wiki](https://wiki.nixos.org/wiki/COSMIC), [mutter #524](https://gitlab.gnome.org/GNOME/mutter/-/work_items/524) |
| Remote input: injected on the desktop (portal + libei, `SendInput`), Phase 7 | Avoids Play's AccessibilityService policy; COSMIC's portal is weeks old | [Play policy](https://support.google.com/googleplay/android-developer/answer/10964491?hl=en), [cosmic-comp #2869](https://github.com/pop-os/cosmic-comp/issues/2869) |
| Build order: CLI to CLI on Linux, then the daemon, then Android, then iOS; freeze the core API before bindings | Prove the hardest property first; iroh's binding churn | [iroh](https://www.iroh.computer/blog/the-road-to-iroh-1-0) |
| Licence: GPL-3.0-or-later | User wants to reuse both KDE Connect (GPL-2.0-or-later) and LocalSend (Apache-2.0) code; only GPL-3.0 admits both | [kdeconnect-kde LICENSES](https://github.com/KDE/kdeconnect-kde/blob/master/LICENSES/GPL-2.0-or-later.txt), [LocalSend](https://github.com/localsend/localsend) |
| Scope: phone to PC, phone to phone, PC to PC | User requirement | User answer 2026-10-08 |
| Interop: none | User requirement; also simplest and fastest | User answer 2026-10-08 |
| UI: native per platform (option A), locked | User requirement; agentic build makes UI volume cheap | User answer 2026-10-08 |
| Auto-accept from pinned devices, notify after | User requirement | User answer 2026-10-08 |
| v1 ships: multi-file and folder drop; pairing plus per-pin auto-accept; Android receive with the screen off; one-tap share-sheet send to default device; Android notification actions "Send clipboard" and "Send to default"; automatic clipboard push from desktop and from foreground phone; discovery fallbacks (last-known dial, QR, hotspot, USB tether); ping/find; battery; media control; remote input on Windows | User's daily features plus the two complaints | User answer 2026-10-08 |
| Phase 5 (still v1, after the core is proven): remote input on COSMIC via the RemoteDesktop portal plus libei; presenter mode; GNOME clipboard button | COSMIC's portal is weeks old and buggy, so it lands last | [cosmic-comp #2869](https://github.com/pop-os/cosmic-comp/issues/2869) |
| Deferred: notification mirroring, SMS, SFTP browsing, run-command, Wi-Fi Direct, relay, always-on iOS receive | Not needed by the user; policy-heavy or fragile | [NotificationListenerService](https://developer.android.com/reference/android/service/notification/NotificationListenerService) |

### Resolved on 2026-10-08

Licence (GPL-3.0-or-later), interop (none), companion slice (see v1 list), UI layer (native), phones (both), scope (all three pairs), receive policy (auto-accept, notify after), and the user's own quirks are all answered in "User requirements" above. `docs/product.md` is now a transcription of that section.

### Still open for Phase 0

| Decision | Options | Recommended default |
|---|---|---|
| Port and multicast group | Decided 2026-10-08 | TCP 42187 (TLS 1.3 + HTTP/2), UDP 42187 beacon, group 239.255.42.187 (organization-local scope), mDNS `_playdrop._tcp`. 42187 is unassigned at IANA as of 2026-10-08 and outside both incumbents' ranges. Override via config. |
| Name | PlayDrop | Confirmed by the repo name `QasimBhb/PlayDrop` |
| SAS algorithm | Decided in `docs/protocol.md` | 6 decimal digits from BLAKE3 over both Device IDs, both public keys and the pairing nonce; 30 s timeout |
| Windows packaging | Classic / MSIX / both | Classic first; MSIX only if notification mirroring is ever added |
| Reuse plan for external code | Which LocalSend and KDE Connect pieces to port | Decide per piece in Phase 1 and later. Candidates: LocalSend's Rust HTTP sender tuning, KDE Connect's MPRIS and mousepad plugin logic. Every reused file keeps its original copyright header and an SPDX tag. |

---

## Anti-patterns

Refuse these outright:
1. Reusing code from KDE Connect, GSConnect, Valent, cosmic-utils/kdeconnect or LocalSend without keeping its copyright header, adding an SPDX tag, and recording it in a `THIRD_PARTY.md`. GPL-3.0 allows the reuse; sloppy attribution does not.
2. Binding or announcing on UDP or TCP 1714-1764. Also, binding 53317 or 224.0.0.167 unless an explicit LocalSend-compatibility mode is turned on.
3. Tying the receive server to a window or UI lifetime (Flutter `initState`, an Activity, a SwiftUI view).
4. A silent `/24` TCP subnet scan as the default discovery path.
5. Trust based on names or IP addresses. Favourites or auto-accept without a pinned key.
6. Letting an unauthenticated discovery packet trigger an outbound connection, or letting discovery grow per-device state without a limit (CVE-2020-26164).
7. A second socket or dynamic port per payload.
8. A new connection or handshake per file, and per-file requests for thousands of tiny files instead of the bundle path.
9. A whole-file-only hash as the resume check.
10. Keep-alives measured in seconds on mobile, holding the MulticastLock permanently, or relaying unthrottled MPRIS signal floods.
11. Advertising always-on receive on iOS.
12. A GNOME Shell extension, or a required COSMIC panel applet, in v1.
13. WebRTC data channels as the bulk path. QUIC as the v1 speed fix.
14. Using `dataSync` as the always-on Android service. Requesting a battery exemption at launch without a user action.
15. AccessibilityService for remote input.
16. `--socket=session-bus` in the default Flatpak.
17. Any shared default secret or group password.
18. A feature that fails silently. If the OS lacks support, hide the feature and say why.
19. A plain-HTTP convenience path presented as secure.
20. Generating UniFFI bindings before the core API is frozen. Filesystem paths in the core on mobile.
21. Any UI work before a Rust CLI can send a 500 MB file and resume it.

---

## Build phases

| Phase | Scope | Done when |
|---|---|---|
| 0. Decisions (mostly done 2026-10-08) | Remaining: `docs/protocol.md` (port, group, service name, SAS, frame format, versioning) and `docs/threat-model.md`; `LICENSE` (GPL-3.0-or-later) and `THIRD_PARTY.md` | Wire format versioned; threat model covers every CVE-2020-26164 class; port chosen |
| 1. Core + CLI (two Linux processes or boxes) | Identity, SAS/QR pairing, pin store; concurrent mDNS + multicast + last-known dial; TLS 1.3; HTTP/2 with tuned windows, benchmarked against raw framing in week one; manifest; streaming per-chunk BLAKE3 with resume; tar bundle; `playdropd` and `playdrop send <peer> <files>` | A 500 MB file killed mid-transfer resumes and its hash matches; 5,000 small files hold steady throughput; iperf3 vs PlayDrop recorded, with 50% as the v1 floor and 80% as the stretch goal; a daemon restart needs no re-pair; a wire capture shows no 1716 and no `kdeconnect` |
| 2. Desktop daemon | `playdropd` on Linux and Windows; tray; notifications; autostart; receive folder; drag and drop; diagnostics screen naming the failed discovery path; IPC for the UI; Slint window; Windows installer + firewall rule; automatic clipboard push (COSMIC data-control, Windows listener); PC to PC transfers | Pop!_OS 24.04 COSMIC receives with the window closed; Windows 11 receives while minimised; Ubuntu GNOME works through the window and a notification; copying on the desktop pushes to the phone within 1 s; a closed ufw/firewalld/Windows port shows "port X blocked", not an empty list |
| 3. Android + session plane | Kotlin `connectedDevice` FGS hosting the core; persistent notification with "Send clipboard" and "Send to default" actions; one-tap share-sheet target for the default device; `ACTION_SEND`/`SEND_MULTIPLE`; photo picker/SAF; battery onboarding; session stream with ping, find, clipboard, battery, media; outbox host for iPhones | Share sheet to default device is one tap; the notification action sends the clipboard with the app closed; a 400 MB video arrives with the phone locked; after 30 minutes of Doze the phone is still reachable by last-known IP, or honestly shown as asleep; destroying the Activity does not stop the server; reconnects after a Wi-Fi drop without re-pairing; idle battery cost measured and acceptable |
| 4. iOS | SwiftUI app; share extension that hands large items to the main app through the App Group; foreground receive; outbox pull from desktops and Android phones; Local Network fix-it screen; "Send clipboard" Shortcut action | A camera-roll dump to Linux works; a 2 GB video goes through the share extension handoff; receives while open; pulls queued files on open; denied Local Network permission shows the fix screen; App Store copy is accurate |
| 5. Companion depth | Remote input: `SendInput` on Windows, RemoteDesktop portal plus libei on COSMIC and GNOME; presenter mode; GNOME clipboard button; media control polish | Trackpad latency under 30 ms on LAN on Windows; COSMIC remote input works or the feature hides itself with a reason; a Wi-Fi bounce reconnects without re-pairing; each feature detects missing OS support and hides itself |
| 6. Ship gate | `.deb`, Flatpak, Windows installer, Play, App Store; test matrix of Pop COSMIC, Windows 11, Android 14/15/16 and iOS (GNOME best-effort); throughput notebook vs LocalSend and KDE Connect; mesh, VPN and guest Wi-Fi scenarios tested | Matrix published; a pinned Android device receives with the screen off; multi-file counts match; resume works; empty lists are diagnosed; iOS copy is accurate; official KDE Connect can stay installed alongside; the guest Wi-Fi path (QR, hotspot, USB tether) works and is documented |
| 7. Later | iroh for cross-network; Local Push Connectivity experiment; Wi-Fi Aware research; libcosmic applet; notification mirroring if ever wanted; MSIX | Each ships behind a feature flag until that platform's stack is stable |

---

## Repo layout

```
playdrop/
  Cargo.toml                # workspace
  crates/
    playdrop-core/          # identity, discovery, pairing, tls, http2, session, transfer, resume
    playdrop-cli/           # `playdrop send`, benchmarks, CI
    playdropd/              # desktop daemon: tray, notifications, IPC, autostart
    playdrop-ffi/           # UniFFI bindings (after core API freeze)
  apps/
    android/                # Kotlin/Compose, FGS, share targets, SAF
    ios/                    # SwiftUI, share extension, entitlements
    desktop/                # Slint window (or Flutter if Phase 1 picks B)
  packaging/
    deb/
    flatpak/
    windows/                # classic installer + firewall rule
    msix/                   # later
  bench/                    # iperf3 comparison scripts, speed matrix
  docs/
    product.md              # transcription of "User requirements" above
    protocol.md             # versioned wire spec
    threat-model.md
  LICENSE                   # GPL-3.0-or-later
  THIRD_PARTY.md            # every reused KDE Connect / LocalSend file, origin and licence
  PLAN.md                   # canonical plan and research
  CLAUDE.md                 # short pointer for Claude Code sessions
```

---

## Conflicts and unverified claims

**Conflicts resolved**

| Conflict | Chosen | Why | Source |
|---|---|---|---|
| LocalSend licence: MIT-style vs Apache-2.0 | Apache-2.0 | Taken from the GitHub API repo licence field; "MIT-style" comes from a roundup | [GitHub](https://github.com/localsend/localsend) |
| KDE protocol version: 7 only vs 7 and 8 | Both exist; irrelevant to us | kdeconnect-meta documents version 8's timestamp; Valent's doc is older; we do not interoperate | [kdeconnect-meta](https://github.com/KDE/kdeconnect-meta/blob/work/protocol-schemas/protocol.md), [Valent](https://valent.andyholmes.ca/documentation/protocol.html) |
| COSMIC clipboard: `COSMIC_DATA_CONTROL_ENABLED` vs `ext-data-control-v1` | `ext-data-control-v1` | 1.6 uses it; the variable only enables the deprecated wlr protocol and opens the clipboard to every window | [NixOS wiki](https://wiki.nixos.org/wiki/COSMIC), [wl-clipboard #242](https://github.com/bugaevc/wl-clipboard/issues/242) |
| Discovery: multicast first (Cursor) vs mDNS first (report) | mDNS first | KDE 2.0 direction, Quick Share, iOS Bonjour, native resolvers | [NLnet](https://nlnet.nl/project/KDE-Connect/), [NearDrop](https://github.com/grishka/NearDrop/blob/master/PROTOCOL.md), [TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy) |
| UI: Flutter locked (Cursor) vs open | Native per platform (A), locked by the user on 2026-10-08 | The stack notes rank A higher for long-term fit; the build is agentic | [stack notes; #94381](https://github.com/flutter/flutter/issues/94381) |
| Licence: Apache-2.0 leaning (report) vs reuse both codebases (user) | GPL-3.0-or-later | Only GPL-3.0 can absorb both GPL-2.0-or-later and Apache-2.0 code | [kdeconnect-kde LICENSES](https://github.com/KDE/kdeconnect-kde/blob/master/LICENSES/GPL-2.0-or-later.txt) |
| Ubuntu 26.04 GNOME version: "GNOME 50" vs unknown | Unknown; design for "current GNOME" | The notes explicitly failed to find it | [Ubuntu release notes](https://documentation.ubuntu.com/release-notes/26.04/summary-for-lts-users/) |
| iOS Wi-Fi Aware: shipped vs not | Not relied on; Phase 7 research | Only one blog source | [Medium](https://arpitkulsh.medium.com/beyond-hotspots-wi-fi-aware-nan-and-the-rise-of-proximity-native-apps-87c9be5dc5f9) |
| KDE Windows port: official since 2021 vs not official | Unresolved; not load-bearing | Both claims trace to Wikipedia snapshots | [Wikipedia](https://en.wikipedia.org/wiki/KDE_Connect) |
| Bulk framing: raw TCP+TLS pipelined chunks (transport notes) vs HTTP/2 (report) | HTTP/2 | One connection carries the session and many files; the Phase 1 iperf3 gate catches any overhead, with custom framing as the fallback | [transport notes; #1657](https://github.com/localsend/localsend/issues/1657) |
| Keep-alive: 10 s (Quick Share needs it) vs minutes (KDE battery bug) | Minutes on mobile, measured in Phase 3 | KDE's 10 s cost about 33% of daily battery | [bug 442782](https://bugs.kde.org/show_bug.cgi?id=442782), [NearDrop](https://github.com/grishka/NearDrop/blob/master/PROTOCOL.md) |
| Windows: ship MSIX (stack notes) vs classic first (report) | Classic first | The firewall rule is easy; v1 does not read notifications | [netsh](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/netsh-advfirewall) |
| Remote input: AccessibilityService (stack notes) vs desktop-side injection | Desktop-side | The v1 direction is phone controls PC; avoids the Play policy | [Play policy](https://support.google.com/googleplay/android-developer/answer/10964491?hl=en) |
| MPRIS in the v1 slice (report) vs deferred (Cursor) | Phase 5 | Low risk but not core; keeps the v1 cut small | [MPRIS](https://specifications.freedesktop.org/mpris/latest/) |
| Headline discovery: BLE-assisted (competitors inference) vs LAN mDNS | LAN mDNS for v1; BLE is later research | No source compared BLE cost or reliability | [open-quickshare](https://github.com/ignotusbucius/open-quickshare) |

**Kept as unverified** (not found in any note; do not rely on these without a source)
- The protocol 8 SAS formula ("first 8 hex of SHA-256 over both keys plus timestamp, 30 minute window").
- LocalSend "left MIT in August 2024".
- "8M+ Play downloads" for LocalSend. The notes say no install counts were found.
- "Sefirah proved the hybrid on Windows."
- PairDrop at about 100 Mbit/s vs 300 Mbit/s for TCP, and a WWW'24 result of HTTP/2 beating HTTP/3 by up to 45%. Only the qualitative "WebRTC is slower than native" is sourced ([Big Iron](https://www.bigiron.cc/guides/localsend-vs-pairdrop-replacing-airdrop-in-a-mixed-household)).
- Warpinator's default group password "Warpinator". The rule against shared default secrets stays as a design rule.
- Android 10+ clipboard blocking and Android 15 "restricted settings" for notification access.
- KDE multi-file Share dropping files, and `FileTransferJob` deleting incomplete files.
- Ubuntu 26.04 shipping GNOME 50, and its exact release date (27 August 2026 in the note is probably the 26.04.1 point release).
- The KDE protocol flow "plaintext TCP identity, then TLS". Discovery breaking on IPv6-only networks.
- LocalSend `--hidden` advertising while the HTTP server is unbound. LocalSend checksums "on by default" in 1.18.
- iOS needing the multicast entitlement for raw UDP multicast.
- Syncthing Device ID being a certificate hash.
- The COSMIC Epoch 1 release date of 2025-12-11 (the notes say "around December 2025").
- Apache-2.0 NOTICE and patent-grant obligations (general licence knowledge, not in the notes).
- The maturity of `rustls`, `hyper`/`h2`, `notify-rust` and `reis` (not researched, or flagged as gaps).
- Whether Play accepts `connectedDevice` for a Wi-Fi LAN peer. The docs example is Bluetooth ([dev.to](https://dev.to/joe_wang_6a4a3e51566e8b52/android-foreground-services-in-2026-what-changed-and-how-to-adapt-2o3d)).
- The LocalSend receiver still being Dart (a low-confidence secondhand claim).
- Reference docs cleared for reading, from the Cursor plan, that the notes do not cite: `invent.kde.org/.../kdeconnect-meta/-/blob/master/protocol.md`, `localsend/localsend/blob/main/AGENTS.md`, `flatpak.github.io/xdg-desktop-portal/docs/`.

---

## Working rules

- Use a plain hyphen (-) in all text: code comments, commits, PRs and docs. Never an em-dash.
- No AI attribution anywhere: no "Co-Authored-By" lines, no "Generated with" lines, no session links, in commits, PRs or comments.
- Write commit messages in plain language: what changed and why, readable by a non-expert.
- Never push. Give the user the exact `git push` command for the current branch, plus a PR title and a short bullet description. After a commit, also give a one-line summary of what the branch is about.
- Env files: any added or changed value goes into every checkout (the main repo and every `<repo>-wt/*` worktree), plus `.env.example`. First confirm the file is gitignored. Leave secrets blank and ask for them once. List the files touched.
- Do not delete research material, plans or docs without asking the user first.
- Favour scalable, maintainable code over fast or cheap-to-build code.

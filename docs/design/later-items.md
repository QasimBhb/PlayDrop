# Later items: design note (2026-10-30)

Purpose: assess each row 7 ("Later") item in `PLAN.md` build phases, plus two known open items,
so a future session can pick Phase 12 without re-reading the research. Nothing here is built.
Facts already cited in `PLAN.md` are reused by section name; new facts carry inline links and
anything not confirmed is marked **unverified**. State at writing: v0.5.0 released, Phase 11
starts with USB-tether path choice (`docs/phases/phase-10-daily-speed.md` section 7).

## Summary table

| # | Item | Size | Needs the user | Verdict |
|---|---|---|---|---|
| 1 | iroh for cross-network reach | large | a second network to prove it for real | build next (as a flagged spike) |
| 2 | iOS Local Push Connectivity | medium | Apple entitlement grant, paid team | research only |
| 3 | Wi-Fi Aware | large | a second Aware phone | research only |
| 4 | libcosmic panel applet | small to medium | nothing | build later |
| 5 | Notification mirroring (Android to desktop) | medium | the user wanting it | build later |
| 6 | MSIX for Windows | medium | signing certificate, a Windows machine | drop (for now) |
| 7 | iOS App Group for the share extension | small | paid Apple team | build later (blocked) |
| 8 | Windows toast buttons and tray device lines | small | a Windows machine, a person at the screen | build later |

Constraint for all of them: `PLAN.md` row 7 says each ships behind a feature flag until that
platform's stack is stable.

## 1. iroh for cross-network reach

**What the user gets.** Send and clipboard work between paired devices when no LAN path exists:
guest Wi-Fi with client isolation (which `PLAN.md` Architecture says no discovery protocol can
fix), phone on mobile data, PC at the office. Pairing stays on the LAN or QR.

**Facts.** Per the [iroh 1.0 docs](https://docs.rs/crate/iroh/1.0.0/source/README.md), a peer is
dialed by public key (an Endpoint ID). The first packets go through a relay, then iroh tries a
hole-punched direct path, and falls back to the relay if that fails. Relays only forward
encrypted packets. The [public relays](https://docs.iroh.computer/iroh-services/relays/public)
are run by n0, are free, have no SLA, are rate limited, and n0 recommends self-hosted relays for
production. The transport is QUIC. `PLAN.md` Stack decision notes iroh's pre-1.0 API churn; 1.0
exists now, so that risk is lower but **unverified** for our usage.

**Fit.** Core, new module `playdrop-core::reach` behind a cargo feature `iroh`. Our Ed25519
identity maps naturally to an Endpoint ID (same key type; **unverified** that the SPKI hash
and Endpoint ID can be tied without a second key). Design: carry the existing TLS 1.3 mutual-auth
HTTP/2 session as a byte stream over an iroh bidirectional stream, so protocol, grants and pin
checks do not change (SR-11 still decides trust). LAN paths stay first; iroh is tried only when
all LAN paths fail. Hosts (`playdropd`, Android service) only gain a config toggle and a "relay
reach" line in diagnostics. UniFFI surface grows by one flag, so `playdrop-ffi` gets a minor bump.

**Needs more than an agent.** A real second network. Most of it can be proven on this machine
with Linux network namespaces plus a locally run `iroh-relay`, but a carrier-NAT phone test needs
the user. Deciding to trust n0's public relays or host one is a user call.

**Threat-model impact.** New boundary B8 "relay": it learns who talks to whom, when and how
much (extends A1 and T5, `docs/threat-model.md` section 5 already lists relay as out of scope
needing "its own model"). Touches SR-1, SR-4, SR-12 (version by ALPN), SR-15 (connection limits
now also apply to relayed dials), SR-14 (a new "remote" grant, default off), SR-32 (no relay URLs
or Endpoint IDs in logs). New SRs: dial only pinned Endpoint IDs; no unpaired contact via relay;
a relayed peer never auto-grants beyond its LAN grants; metadata statement for the user.

**Anti-patterns.** 13 (QUIC is not the bulk speed fix; here it is reach only, and QUIC is ~40% of
TCP+TLS speed per `PLAN.md` Decisions, so state "slow path" in the UI), 7 (no second socket per
payload: one iroh stream), 4 and 6 (no scanning; no dial from unauthenticated packets), 18
(say when relay is in use), 19. Conflicts with the "no cloud" pitch unless the relay is optional
and self-hostable; word it that way.

**Size.** Large. **Verdict: build next**, as a flagged spike: it is the only item that fixes the
guest, campus and hotel Wi-Fi case the user named as normal, and an agent can prove most of it alone.

## 2. iOS Local Push Connectivity experiment

**What the user gets.** The iPhone could receive alerts ("file waiting", clipboard) while the app
is not open, but only while joined to a configured Wi-Fi network.

**Facts.** `PLAN.md` Platform ceilings already states: `NEAppPushProvider`, iOS 14+, entitlement
gated, Apple must grant it. The entitlement is `app-push-provider` and is requested through
Apple's [entitlement request page](https://developer.apple.com/documentation/networkextension/local-push-connectivity);
forum reports on whether Apple still grants it to ordinary apps conflict
([thread](https://developer.apple.com/forums/thread/834979)), so grant likelihood is **unverified**.
It was designed for private-network and enterprise apps (**unverified** for a hobby app).

**Fit.** New iOS extension target (`PlayDropPush`) in `apps/ios`, talking to the foreground-only
core over the App Group (item 7), so it depends on a paid team twice. Core change: a small
"wake" frame source on the desktop and Android hosts.

**Needs more than an agent.** Paid Apple team, Apple approving the entitlement, a person on the
iPhone. xtool with a free team cannot sign it.

**Threat-model impact.** A new always-on socket on iOS (B1 extension). Touches SR-28 (Keychain
sharing across extension), SR-15. New SR: extension holds no grants, only a wake hint.

**Anti-patterns.** 11 (never advertise always-on iOS receive; copy must say "on your Wi-Fi only"), 18.

**Size.** Medium. **Verdict: research only**: blocked by an entitlement an agent cannot request.

## 3. Wi-Fi Aware research

**What the user gets.** Phone to phone or phone to PC discovery and data without a router, and
a path through client isolation, if both ends support Neighbor Awareness Networking.

**Facts.** The connected Galaxy A54 (serial `R5CW60WJQGM`) reports `android.hardware.wifi.aware`
true (`adb shell pm list features`, checked 2026-10-30). Android's Aware API needs two Aware
devices. `PLAN.md` Conflicts lists iOS Wi-Fi Aware as not relied on (one blog source). Desktop
Linux and Windows have no usable Aware stack (**unverified**). Wi-Fi Direct is ruled out by
`PLAN.md` Decisions (~20 Mbit/s); Aware data paths may differ, not measured.

**Fit.** Android only: an app-layer module behind the Kotlin service that produces a local
interface address, then the existing core dials it like any peer. Core stays unchanged.

**Needs more than an agent.** A second Aware-capable phone. With one A54 nothing can be
verified end to end.

**Threat-model impact.** New discovery channel B2' (rules T2 to T6 apply: no dial from
unauthenticated frames, rotating identifiers). SR-1, SR-2, SR-4, SR-5 apply; no new boundary
if it only yields an address.

**Anti-patterns.** 4, 6, 18 (hide when unsupported). **Size.** Large. **Verdict: research only**:
single-phone coverage and no desktop support make it a phone-to-phone niche.

## 4. libcosmic panel applet (optional)

**What the user gets.** A native COSMIC panel entry (status, send clipboard, recent transfers)
in place of the SNI tray icon. Never required (anti-pattern 12); SNI keeps working.

**Facts.** Applets are separate libcosmic processes (`PLAN.md` Platform ceilings). A
[template exists](https://github.com/edfloreshz/cosmic-applet-template) and builds with `just`.
libcosmic has no stability promise (`PLAN.md` Stack decision, option F), so expect churn on every
COSMIC release.

**Fit.** New app `apps/cosmic-applet` (separate workspace member, not part of the default build),
a thin client of the daemon over the existing D-Bus API (`docs/ipc.md`). Never owns the socket.

**Needs more than an agent.** Nothing. This machine runs COSMIC, so it can be installed and
looked at; a screenshot check needs the user or a Wayland capture.

**Threat-model impact.** None new: another B3 same-user D-Bus caller, SR-26 applies unchanged.
Flatpak packaging of an applet is **unverified** (applets live in the panel process space).

**Anti-patterns.** 12 (must stay optional and off by default in packages), 3 (no socket).
**Size.** Small to medium. **Verdict: build later**: polish for one desktop, no user complaint.

## 5. Notification mirroring (Android to desktop)

**What the user gets.** Phone notifications appear on the desktop, with dismiss and reply later.
The user said it is not required (`docs/product.md`).

**Facts.** `PLAN.md` Platform ceilings: `NotificationListenerService` needs special access from
a Settings screen and a store-visible core feature; Flatpak reading is a separate concern, not
needed here because the phone is the source. Distribution is GitHub only (CLAUDE.md state), so
Play policy does not block it.

**Fit.** Android app: a listener service feeding a new `notification` session frame
(`PLAN.md` Wire already reserves the name). Core: frame type plus a `notifications` grant.
Desktop `playdropd`: show through the existing notify path (`crates/playdropd/src/notify.rs`).
Windows tray and toasts are limited by item 8. Verifiable alone: A54 on USB plus COSMIC.

**Needs more than an agent.** Only the user's wish to ship it. Special access is granted once
on the phone by hand (a person at the screen).

**Threat-model impact.** Highest sensitivity of any companion feature: notification text can hold
OTP codes and private messages, so a stolen paired device (A3) now reads them. Needs: new grant
(default off, enabled only on the phone), per-app allow list, skip sensitive and ongoing
notifications, SR-24 style rules, no text in logs (SR-32), desktop lock behaviour like SR-25. New
SRs for each of those. Also new Android surface under SR-29.

**Anti-patterns.** 18 (hide with a reason if access not granted). **Size.** Medium.
**Verdict: build later**: a real feature, but the user does not need it and it widens the
data exposed to a stolen pair.

## 6. MSIX packaging for Windows

**What the user gets.** Clean install and uninstall through the Windows store-style flow, and
package identity (needed for `UserNotificationListener`, so only relevant to a Windows-side
notification feature).

**Facts.** `PLAN.md` Decisions: classic installer first, MSIX only if notification mirroring is
ever added; MSIX makes the firewall rule harder and forces `StartupTask`. Signing:
[Microsoft signing overview](https://learn.microsoft.com/en-us/windows/msix/package/signing-package-overview)
puts a code-signing certificate at roughly USD 300 to 500 a year; an OV cert is about USD 150 to
300 a year; [Azure Artifact Signing](https://azure.microsoft.com/en-us/pricing/details/trusted-signing/)
is about USD 9.99 a month for individuals in the US and Canada, organisations in the US, Canada,
EU and UK (these figures come from search snippets, **unverified** against the live pricing
page). Unsigned MSIX will not install for public users. SmartScreen reputation builds slowly
either way.

**Fit.** `packaging/msix/` (already in the repo layout), no core change. Daemon autostart
must use `StartupTask` (SR and `PLAN.md` Platform ceilings).

**Needs more than an agent.** A signing certificate or identity (money, an account, identity
checks), a Windows machine to install and test the firewall and autostart behaviour.

**Threat-model impact.** Package identity changes SR-28 (DPAPI path), SR-27 (pipe name stays by
SID) and SR-31 (firewall rule scope). Needs a re-check of each on Windows.

**Anti-patterns.** None directly; the risk is regressing 14-style "silent failure" when autostart
is disabled by the user. **Size.** Medium. **Verdict: drop** for now: the classic installer works,
the only gain is for item 5 on Windows, which is itself not wanted.

## 7. iOS App Group for the share extension

**What the user gets.** The share sheet on iPhone hands files to the app (`PLAN.md` Phase 4
design). Today the extension cannot pass items to the app, so it is the one iOS device gate that
failed on 2026-10-12 (CLAUDE.md state; `apps/ios/README.md`).

**Fit.** Already written (queue in the App Group container, `playdrop://` with no parameters,
SR-30). The remaining work is entitlement and team configuration, not code.

**Needs more than an agent.** A paid Apple Developer team (App Groups are not available on the
free personal team; the paid price is **unverified** here, widely quoted as USD 99 a year) and a
person to run the signing and an on-phone check.

**Threat-model impact.** None new beyond SR-30 and SR-28 (shared Keychain group is not needed;
the queue holds files, not keys).

**Anti-patterns.** 11 only if the copy overpromises. **Size.** Small. **Verdict: build later**:
blocked on the paid team; it is also a prerequisite for item 2.

## 8. Windows toast buttons and tray device lines

**What the user gets.** The Windows toast shows "Open" and "Show in folder" like `PLAN.md` User
requirements specifies, and the tray menu lists devices and battery as the Linux tray does.

**Fit.** `crates/playdropd/src/notify.rs` (Windows `show_and_wait` clears the actions and has a
`TODO(Phase 2 follow-up)`: buttons need a COM activator registered by the installer) and
`crates/playdropd/src/tray.rs` (`start` ignores the `TrayView`; same TODO). Work: a COM
activator class with CLSID registration in the Inno Setup installer (`packaging/windows/`),
the toast `activated` callback, closing observed through it, and a tray menu built from
`TrayView`.

**Needs more than an agent.** A Windows machine and a person at the screen to click a toast.
CI on Windows can build and unit test the menu model and the activation parser, but not
click a toast; an unattended toast is not a passing test.

**Threat-model impact.** SR-23 applies: "Open" on executables, scripts, `.lnk`, `.msi` opens the
folder. The activator is a new local entry point (B3): it must accept only our own toast
arguments and ignore others, same user only (SR-27 spirit). SR-32: no file names at info level.

**Anti-patterns.** 18 (until it lands, the toast silently lacks the buttons the plan promises;
the UI should not claim them). **Size.** Small. **Verdict: build later**: Windows is a daily
desktop for the user, so this is the cheapest visible gap, but it needs the user's machine.

## Ranking for Phase 12

Items an agent can build and verify alone on this machine (COSMIC desktop, A54 on USB, iPhone via
xtool, CI on Linux, Windows, macOS and Android):

1. **Item 1, iroh reach, as a flagged spike.** Value: the only fix for isolated guest, campus and
   hotel Wi-Fi. Verify with network namespaces and a local `iroh-relay`, plus the A54 on mobile
   data if the user will toggle Wi-Fi. Write the new SRs and the relay-metadata statement first
   (design note, then `docs/protocol.md` addendum), keep LAN first, default off.
2. **Item 5, notification mirroring.** Fully verifiable here (A54 plus COSMIC), medium size, but
   do it only if the user changes their mind: it adds the most sensitive data flow in the product.
3. **Item 4, libcosmic applet.** Small, verifiable on this desktop, but polish, with API churn.

Phase 11 as written (USB-tether versus Wi-Fi path choice, per-member phone cost) comes first
and is not in this list.

## Needs the user

- Item 8: a Windows machine and a person to click toasts (and to check the installer registers the COM class).
- Item 7 (and 2): a paid Apple Developer team; Apple's decision on `app-push-provider` for item 2.
- Item 6: a signing certificate or Azure Artifact Signing identity, and a Windows machine.
- Item 3: a second Wi-Fi Aware phone.
- Item 1: a decision between n0's public relays and a self-hosted relay, and a real non-LAN test
  (mobile data or a different Wi-Fi).
- Item 5: the user saying they want it.

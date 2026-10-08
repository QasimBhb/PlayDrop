# PlayDrop product definition

Answered by the owner on 2026-10-08. `PLAN.md` holds the research and rationale; this file is the
short, stable statement of what the product is. If the two disagree, this file wins for product
scope and `docs/protocol.md` wins for the wire.

## One sentence

PlayDrop links your phones and computers over the local network with no account, no cloud, one
tap to send, and zero taps to keep clipboards in sync.

## Platforms

| Platform | Role in v1 |
|---|---|
| Android | Full companion: always-on background service, receive with screen off, notification actions |
| iPhone | Sender and foreground receiver. Pulls queued files when opened. No background claims |
| Pop!_OS COSMIC | Primary desktop. Daemon, tray, auto clipboard, remote input (last, portal risk) |
| Windows 11 | Primary desktop. Daemon, tray, auto clipboard, remote input |
| Ubuntu GNOME | Must work, not gated. Window-first, clipboard by button |

Device pairs: phone to PC, phone to phone, PC to PC.

## What ships in v1

- Multi-file and folder drop with resume and steady speed for thousands of small files.
- Pair once (6-digit code or QR), then per-device auto-accept with a notification after.
- One-tap send from the Android share sheet to the default device. No device picker by default.
- Android persistent notification with "Send clipboard" and "Send to <default>" actions.
- Desktop clipboard changes push to paired phones automatically.
- Ping, find my phone, battery level, media control.
- Remote input: phone as trackpad, keyboard and presenter for the PC.
- Discovery that works on mesh, VPN and guest Wi-Fi, or tells you which path failed and offers
  QR, hotspot or USB tether.

## What does not ship in v1

Notification mirroring, SMS from PC, browsing the phone's files, run-command, Wi-Fi Direct,
internet relay, always-on iPhone receive, LocalSend or KDE Connect compatibility.

## The two complaints it exists to fix

1. KDE Connect pairing with COSMIC is unreliable and gives no reason when it fails.
2. LocalSend needs five steps per send. KDE Connect's one-tap notification action and silent
   clipboard sync are the bar.

## Non-negotiables

- Speed first. Measured against `iperf3`, published before release.
- Nothing silent. A feature the OS cannot support hides itself and says why.
- Trust is per device key, never per name, IP or network.
- Own protocol, own port (42187). Never 1714-1764 or 53317.
- GPL-3.0-or-later. Reused code is listed in `THIRD_PARTY.md`.

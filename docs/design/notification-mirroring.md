# Notification mirroring (Android to desktop): design note (2026-11-11, Phase 15)

Later-items row 5. Picked in Phase 15 because it is the only open row an agent can build and
prove on this machine (A54 plus COSMIC); every other row needs a paid Apple team, a second
Aware phone, a signing certificate with a Windows PC, or a relay on a second network. The
product names it as a later, opt-in companion feature (`PLAN.md` first paragraph).

## What the user gets

- Notifications posted on the Android phone, from apps the user picked, appear on the desktop
  as desktop notifications titled with the app and the phone's name.
- When the phone's notification goes away (opened, swiped, cancelled by the app), the desktop
  copy closes too. Closing the desktop copy dismisses it on the phone.
- Not in this phase: replies, actions, icons and pictures, desktop to phone, iOS as a source
  (iOS cannot read other apps' notifications, `PLAN.md` platform ceilings).

## Consent, in three locks (all default off)

1. **Android special access.** "Notification access" for PlayDrop in system Settings, granted by
   a person. Without it the listener never runs; the app says why the feature is off and offers
   the Settings screen (anti-pattern 18).
2. **Per-device grant `notificationsOut` on the phone** (new `Grants.notifications_out`, default
   `false`, never turned on by pairing). Only the phone's own UI changes it (SR-14).
3. **Per-app allow list on the phone**, empty by default. A new app's notifications are not
   mirrored until it is ticked.

The desktop holds `notificationsIn` (new `Grants.notifications_in`, default `true`, like
`clipboardIn`): it decides whether it shows what a peer sends. *Choice:* the data owner (the
phone) carries the consent; the receiving side only gets to refuse noise.

Grants added after pairing are read with serde defaults, so existing pin stores load with both
values at their defaults.

## Wire (protocol.md 5.16, capability `notifications`, protocol stays 1)

```json
{"t":"notification","v":1,"op":"posted","key":"3f9a...c1","app":"Signal",
 "title":"Alex","text":"On my way","when":1762900000000}
{"t":"notification","v":1,"op":"removed","key":"3f9a...c1"}
{"t":"notification","v":1,"op":"dismiss","key":"3f9a...c1"}
```

- `posted` and `removed` go phone to desktop; a later `posted` with the same `key` replaces the
  copy. `dismiss` goes desktop to phone and asks it to cancel that notification.
- `key` is opaque: the first 128 bits of BLAKE3 keyed by a per-process random salt over the
  Android notification key, as lowercase hex. The Android key (which names the package and a
  tag) never leaves the phone.
- Limits: `app` at most 64 bytes, `title` 256, `text` 1 KiB (UTF-8, cut at a character
  boundary by the sender, refused over the limit by the receiver with `error` `bad_request`).
  At most 30 `posted` frames per minute per connection; more are dropped. The desktop shows at
  most 20 mirrored notifications per peer at once, closing the oldest.
- Sending `posted` or `removed` needs the sender's `notificationsOut`; showing needs the
  receiver's `notificationsIn`. `dismiss` needs the phone's `notificationsOut` (a desktop the
  phone does not mirror to has nothing to dismiss). A frame without its grant is dropped (5.3).

## What the phone never sends (SR-41)

- Notifications from apps not on the allow list, and from PlayDrop itself.
- Ongoing notifications and foreground-service notifications, group summaries, and anything
  with `VISIBILITY_SECRET`.
- Anything while the work profile or a secure folder owns it (only the main user's are seen).
- Content Android itself redacts (Android 15 hides one-time codes from listeners without
  `RECEIVE_SENSITIVE_NOTIFICATIONS`, which PlayDrop does not request): what arrives redacted is
  sent redacted.
- `VISIBILITY_PRIVATE` is not a filter. It is Android's default for nearly every app and only
  hides content on the phone's lock screen, so treating it as "no content" would leave the
  feature empty (found while building, 2026-11-11). Ticking an app on the allow list is the
  consent for its titles and text, and the app picker says so in plain words.

## Threat-model additions

- **T48 (I)** A stolen or compromised paired desktop reads the phone's notifications: one-time
  codes, private messages. Mitigation: SR-41, SR-42. Residual: what the user allowed.
- **T49 (I)** Notification text ends up in logs or bug reports. Mitigation: SR-43.
- **T50 (D)** A paired phone floods the desktop with notifications. Mitigation: SR-44.
- **SR-41 (P15)** Mirroring needs all three locks above, each default off; pairing never turns
  `notificationsOut` on, and only the phone's own UI changes it. The sender filters as listed
  above. *Test:* with any lock off nothing is sent; filtered kinds are never sent.
- **SR-42 (P15)** Turning the grant off, unpairing, or revoking notification access stops
  mirroring at once and removes every mirrored copy on the desktop for that peer (the phone
  sends nothing more; the desktop closes copies at once on unpair and on its own grant off,
  and 5 minutes after going offline unless the peer came back, see "After a reconnect").
  *Test:* unpair closes the desktop copies.
- **SR-43 (P15)** Notification app names, titles, text and keys are never logged at any level
  and are not in the diagnostics report (SR-32 canary extended). *Test:* the canary test seeds
  a notification and greps the logs.
- **SR-44 (P15)** Size limits, 30 posted frames per minute per connection, 20 shown per peer.
  *Test:* the 31st frame in a minute is dropped; the 21st copy closes the oldest.

## After a reconnect (2026-11-12)

The phone cannot know what a desktop still shows, so each time a peer comes online it sends
its current notifications again (at most 20). When the desktop closed every copy at offline,
up to 20 old notifications popped up again after every Wi-Fi drop. No wire or phone change
fixes it; the desktop table (`crates/playdropd/src/notify/mirror.rs`) now keeps its copies
over a short absence:

- Offline, a peer's copies stay for 5 minutes. Unpair, the desktop's `notificationsIn`
  grant going off, notifications turned off and daemon shutdown still close them at once.
- Back within the 5 minutes, a `posted` that matches a kept copy changes nothing: no popup,
  no replace. The key is salted per phone process, so a kept copy also matches by content:
  a keyed BLAKE3 (random key per daemon run, memory only) over app, title, text and `when`.
  A match under a new key re-maps the copy to it, so a later `removed` or dismissal works.
- Kept copies not sent again within 10 s of the return were removed on the phone while it
  was away, and close.
- Still offline after 5 minutes, the copies close and their content fingerprints are
  remembered (at most 64 per peer, memory only, never logged). When the phone returns, the
  resend that follows (the 10 s after it comes back) does not show them again; when that
  window ends the memory is cleared, so an app that later posts the same content again is
  shown as usual. Trade-off: an old notification still on the phone is missing from the
  desktop's list after a long absence; the user already saw it once. A copy the user
  dismisses while the phone is away, or a kept copy dismissed inside the resend window, is
  remembered the same way, since the `dismiss` cannot reach the phone under its old key.
- The same content under the same key is never shown twice, online or not, so a phone
  listener that reconnects (and sends its set again) does not pop everything up either.
- Windows toasts cannot be closed later (the SR-42 exception), so there going offline only
  remembers the fingerprints at once.

## Fit

- Core: frame type and parsing in `session/types.rs`, grants in `pin_store`, `Host::send_notification`
  and `Host::send_notification_dismiss`, events `NotificationPosted`, `NotificationRemoved`,
  `NotificationDismiss`, rate limit per connection. `playdrop-ffi` 0.5.0 with the same surface,
  noted in `docs/core-api.md`.
- Android: a `NotificationListenerService` (declared with `BIND_NOTIFICATION_LISTENER_SERVICE`),
  the filter as a pure function with JUnit tests, the allow list in settings, a "Notifications"
  screen per device with the grant toggle and the app list, and a fix-it row when access is off.
- Desktop daemon: shows through `notify.rs` (freedesktop on Linux, toast on Windows) with the
  replace and close mapping; closing a copy sends `dismiss`; IPC grows the two grants in its
  grant setter; the window's device page shows the `notificationsIn` toggle.

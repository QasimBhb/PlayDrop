# Local IPC between playdropd and its UIs

Normative since Phase 2, extended in Phase 3 (section 1.1). The Rust contract lives in `crates/playdrop-ipc/src/lib.rs`; this
file explains the transport rules and the security checks (threat-model SR-26, SR-27,
SR-32) that every implementation must keep. PLAN.md "Architecture" fixes the split: the
daemon owns the socket and the core; every UI is a client and never listens on the network.

## 1. Contract

One JSON contract on both platforms:

| Type | Direction | Shape |
|---|---|---|
| `Request` | UI to daemon | `{"op": "<snake_case variant>", ...fields}` |
| `Response` | daemon to UI | `{"result": "ok", "kind": "<variant>", "data": <payload>}` (no `data` for `done`) or `{"result": "err", "code": "<ErrorCode>", "message": "..."}` |
| `Event` | daemon to every UI | `{"source": "host", "event": "<HostEvent variant>", ...}` or `{"source": "daemon", "event": "<DaemonEvent variant>", ...}` |

`IPC_VERSION` is a single integer. A daemon answers a client of a different version with
`ErrorCode::BadRequest` on the hello (pipe) or refuses the method call (D-Bus), and the
client shows "update PlayDrop". The contract enums are `#[non_exhaustive]`; a client must
treat unknown variants as ignorable, not fatal. (`clipboard_pushed.skipped` instead
decodes an unknown value as `other`, because it travels inside a response, which must decode.)

Peer arguments are queries resolved by `Host::resolve_peer`: a hex Device ID, a Short ID
prefix (hyphens optional) or an exact label. An ambiguous query answers
`ErrorCode::Ambiguous`.

### 1.1 Requests and settings added in Phase 3

Same envelope as every other request. `IPC_VERSION` stays 1: every addition is a new
variant or a field with a default, which an older peer ignores or fills in.

| Request (`op`) | Fields | Answer (`kind`) | Maps to |
|---|---|---|---|
| `queue_outbox` | `peer`, `paths` | `outbox_queued` `{id}` | `Host::queue_outbox` (protocol.md 7.8). Files travel like `send_files`: fds through `QueueOutboxFiles` on Linux, paths on Windows |
| `outbox` | none | `outbox` `[OutboxItem]` | `Host::outbox`: items this daemon holds, oldest first |
| `remove_outbox` | `id` | `done` | `Host::remove_outbox`; `not_found` for an unknown ID |
| `pull_outbox` | `peer` | `outbox_pulled` `{ids}` | `Host::pull_outbox`; `offline` without a session. Progress arrives as receive-side transfer events |

| Settings field | Default | Meaning |
|---|---|---|
| `clipboard_images` | `true` | Sync clipboard images (PNG, at most 16 MiB) both ways. Off: the watcher ignores images and received ones are not applied. Missing from an older daemon's answer reads as `true` |

Clipboard caps the UI shows: text up to 1 MiB (over 32 KiB it travels as a blob,
protocol.md 5.15), images up to 16 MiB.

Host events new in Phase 3 pass through unchanged as `Event::Host`: `clipboard_image`
(`data` is base64), `outbox_ready`, `media_state`, `media_command`. A UI ignores the ones it
does not render; an event this build cannot decode is skipped, never fatal (section 1). Both
transports drop a single event over `MAX_MESSAGE_BYTES` (4 MiB) with a warning, so a large
`clipboard_image` reaches the daemon's clipboard but not the UIs.

### 1.2 Requests, settings and events added in Phase 5

`IPC_VERSION` stays 1 (additions only).

| Request (`op`) | Fields | Answer (`kind`) | Maps to |
|---|---|---|---|
| `stop_input` | none | `done` | `Host::stop_input_sessions` (SR-25 kill switch): ends every remote input session this desktop is the target of, releases held keys and buttons, tells the peers. The tray has the same action |
| `media_command` | `peer`, `player`, `action` (protocol.md 5.9 spelling), optional `value` | `done` | `Host::send_media_command`: asks an online peer with `media.control` to control one of its players; a refusal arrives later as a `peer_refused` host event |

| Settings field | Default | Meaning |
|---|---|---|
| `remote_input_supported` | `false` | The desktop can apply remote input (`SendInput` on Windows, the RemoteDesktop portal on Linux). When `false` the window shows the `input` grant toggle disabled with `remote_input_reason` next to it (anti-pattern 18) |
| `remote_input_reason` | absent | A sentence saying why remote input is unavailable |

`Status` gains `input_from` (labels of the peers controlling this desktop right now), so a
window that starts mid-session shows its banner at once.

Daemon event `input_session` `{peer, label, active}`: a peer opened (`active: true`) or ended
(`false`) a remote input session on this desktop; the window shows "Remote input from
<label>" with a Stop button while one is active.

Host events new in Phase 5 pass through unchanged as `Event::Host`: `input` (`peer`,
`input`: the protocol.md 5.12 frame body, tagged on `kind`; the daemon has already applied
it, UIs may ignore it), `peer_refused` (`peer`, `frame_type`, `code`, `message`: the peer
refused one of our frames, usually `forbidden` for a missing grant on its side; the window
shows "<label> does not allow <feature>"), `input_session_ended` (`peer`, `reason`: a
desktop we controlled ended the session; desktops do not control others in v1, so UIs may
ignore it). `PeerSummary` gains `capabilities` (the peer's
`caps.in` while online, empty offline): a UI shows remote input, media control or image
clips only when the matching capability is listed. `unsupported` is no longer emitted.

### 1.3 Settings field added in Phase 7

`IPC_VERSION` stays 1 (additions only).

| Settings field | Default | Meaning |
|---|---|---|
| `autostart_refused` | absent | Why start at login is off although it was asked for: a sentence worded for the user (the Background portal or the desktop refused, or the login entry could not be written). Absent when it was never refused or the setting was turned on again successfully. The daemon stores it with the settings, so a window started later still sees it, and clears it when `SetSettings` carries any `autostart` value. A refusal also arrives as a `settings_changed` event with `autostart: false` and this field set. Missing from an older daemon's answer reads as absent |

### 1.4 Clipboard answers and transfer progress added in 0.3.0

`IPC_VERSION` stays 1 (additions only).

| Answer or field | Meaning |
|---|---|
| `clipboard_pushed.skipped` | Why nothing was sent: `echo`, `no_peers` or `unsupported`. Absent when something was sent. Missing from an older daemon's answer reads as absent. For `send_clipboard` (one device) that daemon set `skipped_echo` for its only "nothing sent" case, so a missing reason there means echo; for `push_clipboard`, `skipped_echo: false` with an empty `sent_to` means the reason is unknown. A value this client does not know decodes as `other` and means "not sent" |
| `clipboard_pushed.skipped_echo` | Kept for older clients. A current daemon sets it exactly when `skipped` is `echo` |
| `files_done` on a transfer snapshot and on `transfer_progress` | Files finished so far, filled on the send side too. Missing from an older daemon reads as 0, which clients show as "no count yet" |

`skipped` values, and what the daemon can honestly tell apart:

- `echo`: the text equals the last clip applied from a peer or the last one sent
  (protocol.md 5.7). For `send_clipboard` to one device this is the only "nothing sent"
  answer; no grant, an offline device and a clip that is too large are errors.
- `no_peers`: no connected device has clipboard sharing on for this computer. The host
  counts offline devices and devices without the `clipboardOut` grant separately but both
  mean nobody to send to, so they share this value (and so does having no paired device).
- `unsupported`: devices are connected and have the grant, but none can take the clip (an
  image to a device without image support, or long text to one that cannot fetch it). It
  wins over `no_peers` when both apply.

There is no reason for a clip marked sensitive: the host sends those, flagged, so a
sensitive clip is never "blocked" at this layer.

A send stays `offered` and sends no `transfer_progress` until bytes stream, so progress
with `bytes: 0` means the receiver has not decided yet only on an older daemon. A client
that must work with both treats `bytes == 0` as still waiting and does not rely on it.

### 1.5 Resumable sends added in 0.4.0

`IPC_VERSION` stays 1 (additions only).

| Field or request | Meaning |
|---|---|
| `resumable` on a transfer snapshot | True on a `failed` send whose record the daemon keeps, so it resumes when the other device is back or when the same files are sent to it again. False once the send is cancelled, resumed or complete. Missing from an older daemon reads as false |
| `cancel_transfer` on a resumable failed id | Forgets the record and tells the receiver, so the send no longer resumes. The snapshot then reads `cancelled` with `resumable: false`. An id the daemon does not know is `not_found` |

Requests are independent and may be issued concurrently. Events are broadcast to every
connected client from the moment it connects; there is no replay. A UI that starts late
rebuilds its view from `Status`, `Peers` and `Transfers` and then applies events.

### 1.6 Reach across networks (spike, protocol.md 12)

| Request | Fields | Answer | Notes |
|---|---|---|---|
| `set_remote_grant` | `peer`, `on` | `done` | The `remote` grant (SR-14, SR-35). Turning it on with no relay named, or on a daemon built without the `iroh` feature, answers `invalid_state` saying so. `playdrop remote <peer> on` prints what the relay can see (SR-39) |
| `reach_paths` | none | `reach_paths`: list of `{short_id, line}` | Peers on the relay or a direct path across networks, with the line to print (SR-39). No URL and no Endpoint ID (SR-40). Empty from a daemon without the feature |

These two are in every build, because `playdrop-ipc` has no `iroh` feature of its own. Nothing calls
them unless the client was built with the feature. `Diagnose` adds the same lines to its summary.

### 1.7 Battery, the local clipboard and transfer names, added in 0.9.0

`IPC_VERSION` stays 1 (additions only). Each new field has a serde default both ways: a new
client reads an older daemon's answer, and an older client ignores the new fields.

| Request, field or value | Meaning |
|---|---|
| `battery` on a `PeerSummary` (`peers` answer) | `{"level": 0-100, "charging": bool, "low": bool}`, the peer's last `battery` frame (protocol.md 5.8). Present only while the peer is online and after it sent one this session; it clears when the peer goes offline or is unpaired. Absent from an older daemon: wait for the `battery` host event as before |
| `names` on a transfer snapshot (`transfers` answer) | What is being sent, for display: the top-level names in manifest order (a file sent alone, or the folder a file sits in), each once, at most 3 (`TRANSFER_NAMES_MAX`); `file_count` is still the full count. A folder of 5,000 files is one name. On the receive side the names come from the peer's manifest after the SR-17 path check the offer prompt's names pass: untrusted text to show, never a path. Absent (empty) from an older daemon |
| `names_more` on a transfer snapshot | `true` only when the transfer has more top-level names than `names` lists (exactly 3 names reads `false`). Absent means `false`, also from an older daemon |
| `send_local_clipboard` `{peer?, allow_sensitive, consent?}` | The daemon reads this computer's clipboard through its own backend (text, or a PNG image while `clipboard_images` is on) and sends it to `peer`, or with no `peer` to every online device with `clipboardOut` (like `push_clipboard`). `allow_sensitive` defaults to false, `consent` to none. Answers `clipboard_pushed`. For a `peer`, an unknown or offline device is `not_found`, `ambiguous` or `offline`, and a device whose `clipboardOut` grant is off is `forbidden`, all before the clipboard is read |
| `clipboard_pushed.skipped: "sensitive"` with `consent` | `send_local_clipboard` only: a password manager marked the clip and the request did not carry a consent the daemon accepts (SR-24). Nothing was sent. `consent` is a one-use token (32 hex characters, 128 random bits, not derived from the clip). Ask the user, then send again with `allow_sensitive: true` and that `consent`. Rules below |
| `clipboard_pushed.consent` | Present only with `skipped: "sensitive"`. Absent from an older daemon and on every other answer. Never log it |
| `clipboard_pushed.skipped: "empty"` | `send_local_clipboard` only: the clipboard holds nothing the daemon sends (no text, and no image or images are off) |
| `invalid_state` answer to `send_local_clipboard` | This session cannot read the clipboard from the background (GNOME, the Flatpak on COSMIC); `message` is the same sentence `Settings` gives for clipboard watching. A client that can read the clipboard itself then sends the text with `send_clipboard` or `push_clipboard` |
| `internal` answer to `send_local_clipboard` | Reading failed, or the clip is over its cap (text 1 MiB, images 16 MiB); `message` names the cause, never the content |

SR-24 rules for `send_local_clipboard`, in order:

1. A send with `allow_sensitive: true` always goes marked sensitive, whether or not the clip
   carried a password manager's mark (a manager that sets no mark is a known residual). So
   `playdrop clip --sensitive` with no text marks the clip just as it does with text.
2. A clip without the mark and `allow_sensitive: false` goes unmarked, as before.
3. A marked clip with `allow_sensitive: false` is refused with `skipped: "sensitive"` and a
   new `consent` token.
4. A marked clip with `allow_sensitive: true` and no `consent` goes, marked. That is the CLI,
   where typing `--sensitive` is the consent.
5. A marked clip with `allow_sensitive: true` and a `consent` goes, marked, only if the token
   is the one the daemon remembers, has not been used, is younger than 60 s, and the clip read
   now has the same digest as the one refused. Otherwise the answer is `skipped: "sensitive"`
   again, with a new token for the clip the clipboard holds now (it changed, or the token is
   stale): ask again.
6. A `consent` sent with `allow_sensitive: true` is used up by that request whatever happens,
   also when the clipboard now holds an unmarked clip (which then goes, marked, by rule 1).
   With `allow_sensitive: false` a `consent` is ignored.

The daemon keeps one remembered refusal, in memory only: one clipboard means only the latest
refusal can match it, so a newer refusal replaces the older one and a client holding the older
token is asked again. The digest is BLAKE3 over the clip's kind (text, or image plus its MIME
type) and then its content, so a text and an image never share one; it never leaves the daemon
and is never logged. In Rust the token is `playdrop_ipc::ConsentToken`, a plain JSON string on
the wire whose `Debug` prints `ConsentToken(..)`, so a logged request or answer never shows it.

A client that sent a `consent` (the user agreed to a marked clip) must not fall back to sending
text it read itself when the answer is `bad_request` or `invalid_state`: that text would go
unmarked. It says the clip was not sent and lets the user try again.

An older daemon cannot decode `send_local_clipboard` and answers `bad_request`. A client that
uses the request treats `bad_request` like `invalid_state`: it falls back to reading the
clipboard itself. An older client that receives `sensitive` or `empty` decodes it as `other`
(section 1.4); it never does in practice, since only the new request produces them. The
daemon logs no clip content for this request (SR-32): the clipboard module logs kind, length
and a short hash at debug, as for every read.

`playdrop-ipc` re-exports every `playdrop-core` type the contract carries in
`playdrop_ipc::types` (`PeerSummary`, `PeerBattery`, `TransferSnapshot`, `TransferStatus`,
`TransferDirection`, `HostEvent` and what its variants carry, `Grants`, `MediaAction`, the
`Input` frame types, the diagnostics types, `OfferedFile`, `FileRoot`, `DeliveredFile`,
`Secret`, `Base64Bytes`, `OFFER_FILES_MAX`, `TRANSFER_NAMES_MAX`), so a client needs no
`playdrop-core` import for the shapes. They are the core's own types: no conversion, no wire
change.

### 1.8 Notification mirroring grants, added in Phase 15

`IPC_VERSION` stays 1 (additions only). Protocol.md 5.16, SR-41 to SR-44.

| Request, field or value | Meaning |
|---|---|
| `grants.notificationsIn` (`peers` answer, `set_grants`) | Show the notifications this peer mirrors. Default `true`, also for devices paired before it existed. Set it with `set_grants` like every other grant. Turning it off closes every copy of that peer's notifications at once (SR-42) |
| `grants.notificationsOut` (`peers` answer, `set_grants`) | Mirror this device's notifications to the peer. Default `false`, never turned on by pairing. Carried for completeness: a desktop has no notifications to mirror, so no desktop client offers it |
| Host events `notification_posted`, `notification_removed`, `notification_dismiss` | Never sent to clients. The daemon shows and closes the desktop copies itself, and the D-Bus `Event` signal is a broadcast other programs on the session bus can match, so the app names, titles, text and keys stay in the daemon (SR-43) |

Both grant fields have serde defaults both ways. An older daemon ignores them in `set_grants`
and leaves them out of `peers`; a client then reads the defaults. An older client that sends
`set_grants` with grants it read from a newer daemon drops both fields, so `notificationsIn`
goes back to `true` and `notificationsOut` to `false`: a client that offers the toggle must be
built against this version. `playdrop_ipc::types` also re-exports `MirroredNotification`.

What the daemon does with a mirrored notification (section 5 has the rest of its behaviour):

- `posted` shows a desktop notification "<app> - <device label>" with the title and the text,
  and no buttons, only while the `notifications` setting is on and the grant is on when the
  event is handled. A later `posted` with the same key replaces it (freedesktop
  `replaces_id`); `removed` closes it.
- At most 20 copies per device are shown; the 21st closes the oldest (SR-44).
- Every copy of a device closes when it goes offline, is unpaired (by either side), or its
  `notificationsIn` grant is turned off; every copy closes when the host restarts or stops,
  or when the `notifications` setting is turned off (SR-42).
- When the user dismisses a copy (freedesktop `NotificationClosed` reason 2), the daemon
  sends `dismiss` to the phone. An expired copy, or one the daemon closed, does not.
- Where the notification server reads the body as markup, the body is escaped, so text from
  the phone shows as typed and cannot add links or images.
- Windows shows a plain toast: no replace, no close and no dismiss-back yet (never run on a
  real PC).
- None of the content or keys is logged at any level, and none is in the `diagnose`
  report (SR-43).

## 2. Linux: D-Bus on the session bus (SR-26)

- Well-known name `org.playdrop.PlayDrop.Daemon1`, object `/org/playdrop/PlayDrop/Daemon1`, interface
  `org.playdrop.PlayDrop.Daemon1`.
- Methods:
  - `Request(in s request) -> (out s response)`: one JSON `Request`, one JSON `Response`.
  - `SendFiles(in s peer, in ah files) -> (out s response)`: the only way to send files.
    Each handle is an fd the client opened on a regular file (`O_RDONLY`) or a directory
    (`O_PATH | O_DIRECTORY`). The daemon resolves each fd to a path by reading
    `/proc/self/fd/<n>`, refuses anything that is not a regular file or directory, and then
    calls `Host::send` with those paths. Paths are never accepted over the bus for sending.
  - `QueueOutboxFiles(in s peer, in ah files) -> (out s response)`: the same descriptor
    rules as `SendFiles`, then `Request::QueueOutbox`. A `queue_outbox` request with paths
    through `Request` answers `BadRequest`, like a path-based `send_files`.
  - `Version() -> (out u ipc_version, out s daemon_version)`.
- Signal `Event(s event)`: one JSON `Event`.
- Owning the name is the single-instance check. A second daemon fails to own it and exits
  with "another PlayDrop daemon is already running".
- D-Bus activation: `packaging/` installs `org.playdrop.PlayDrop.Daemon1.service` so that a UI
  call starts the daemon when it is not running. The name is prefixed with the Flatpak app
  ID `org.playdrop.PlayDrop` because Flatpak exports only activation files under the app ID.
  Flatpak never gets `--socket=session-bus` (anti-pattern 16); the daemon and the window
  live in the same app, which owns the name (`--own-name`).
- Inside Flatpak (`/.flatpak-info` exists) the `/proc/<pid>` checks below are skipped on
  both sides: each sandbox instance has its own PID namespace and cannot resolve the bus's
  host PIDs. The UID check stays, and the Flatpak D-Bus proxy already restricts who may own
  or talk to the name, so the boundary holds; the skip is logged once at info.

Daemon-side checks before any method body runs:

1. `org.freedesktop.DBus.GetConnectionUnixUser(sender)` must equal the daemon's own UID.
   Anything else answers `ErrorCode::Unauthorized` and is logged at warn with the UID only.
2. `org.freedesktop.DBus.GetConnectionUnixProcessID(sender)` gives the caller PID. If
   `/proc/<pid>/root/.flatpak-info` exists, its `[Application] name=` must be
   `org.playdrop.PlayDrop`; any other app ID answers `Unauthorized`. A missing file means
   an unsandboxed process of our user, which is allowed.
3. `SetGrants` additionally requires that the caller is a UI of ours, not merely our user:
   it is still our user's process, which is the SR-14 "local UI" boundary. Grants can never
   be changed from the network by construction, because only the IPC reaches
   `Host::set_grants`.

Client-side checks before the first call:

1. `GetNameOwner("org.playdrop.PlayDrop.Daemon1")` then `GetConnectionUnixUser(owner)` must equal
   the client's UID.
2. `GetConnectionUnixProcessID(owner)` then `readlink /proc/<pid>/exe` must end in
   `/playdropd` (the Flatpak build checks the in-sandbox path). On failure the client
   reports `ConnectError::Unverified` and shows the reason instead of talking.

## 3. Windows: per-user named pipe (SR-27)

- Name `\\.\pipe\playdrop-<user SID>`, created with `FILE_FLAG_FIRST_PIPE_INSTANCE` (the
  single-instance check), `PIPE_REJECT_REMOTE_CLIENTS`, byte mode, and a security
  descriptor whose DACL grants access to the user's SID and `SYSTEM` only.
- Framing: newline-delimited JSON, one `wire::ClientMessage` or `wire::ServerMessage` per
  line, each at most `MAX_MESSAGE_BYTES`. The client's first line is
  `{"type": "hello", "ipc_version": 1}`; the daemon answers `welcome` or closes.
- Requests carry a client-chosen `id`; responses echo it; events carry none.
- `SendFiles` and `QueueOutbox` carry paths. The pipe's DACL plus the token check below
  are the boundary.

Daemon-side check per connection: `GetNamedPipeClientProcessId` then open the process
token and compare its user SID with the daemon's. Mismatch closes the pipe before the
welcome. Client-side check: `GetNamedPipeServerProcessId`, then the server process's token
SID must equal the client's and its image path (`QueryFullProcessImageNameW`) must end in
`\playdropd.exe`.

## 4. Logging (SR-32)

Transports log request `op` names and error codes only. They never log request bodies
(clipboard text, QR URIs), response bodies or event bodies. `Secret<T>` keeps its redacted
`Debug`, so a stray `{:?}` of an event is safe; the transports still do not print events.

## 5. Daemon behaviour the UI relies on

- `TransferComplete` events on the receive side carry `saved` paths and `folder`;
  `OpenReceived` and `ShowInFolder` accept only those paths (under the receive folder) and
  apply the SR-23 rule: executables, scripts, `.desktop`, `.lnk` and `.msi` open the folder.
- `OpenAddDeviceWindow`, `ShowQr`, `PairWith` and `ScanQr` answer `Forbidden` with the
  message "desktop is locked" while the lock state says so (SR-6).
- `SetSettings` with a new `receive_dir` or `device_name` restarts the host; the response
  says `restarted: true` and a `DaemonEvent::HostRestarted` follows. Transfers in flight
  resume on reconnect as in Phase 1.
- Presence and battery: the daemon sets `locked` or `active` presence from the lock watcher
  and reads this computer's battery every 10 s (pushing on change or every 60 s; a desktop
  without a battery sends nothing). Peers' `presence` and `battery` arrive as host events;
  the window keeps a peer that sent `asleep` shown as "Asleep" after it drops. After 0.8.0
  the `peers` answer also carries each online peer's last battery (section 1.7), so a UI
  that starts late shows it at once.
- `find` from a peer rings: a critical, resident notification with the `phone-incoming-call`
  sound, shown again every 3 s until the peer stops, the user dismisses it, or 60 s pass
  (one reminder toast on Windows).
- `Diagnose` returns the core view plus the daemon's port probe. The summary lines are
  ready to display and name the failed layer: "multicast blocked on <iface>", "port 42187
  blocked by the host firewall (ufw): run `sudo ufw allow 42187`", "no interface on the
  same network as <peer>", or "all paths fine". After those come lines that are not a
  network problem: "start at login was not allowed: <reason>" (Flatpak, Background portal
  refused) and informational lines starting with "note: ", such as KDE Connect or
  GSConnect listening on port 1716, which is fine. UIs open their "Can't find your
  device?" help only for the network lines.

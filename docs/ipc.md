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
treat unknown variants as ignorable, not fatal.

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

Requests are independent and may be issued concurrently. Events are broadcast to every
connected client from the moment it connects; there is no replay. A UI that starts late
rebuilds its view from `Status`, `Peers` and `Transfers` and then applies events.

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
  the window keeps a peer that sent `asleep` shown as "Asleep" after it drops.
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

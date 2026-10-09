# Local IPC between playdropd and its UIs

Normative for Phase 2. The Rust contract lives in `crates/playdrop-ipc/src/lib.rs`; this
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
- `SendFiles` carries paths. The pipe's DACL plus the token check below are the boundary.

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
- `Diagnose` returns the core view plus the daemon's port probe. The summary lines are
  ready to display and name the failed layer: "multicast blocked on <iface>", "port 42187
  blocked by the host firewall (ufw): run `sudo ufw allow 42187`", "no interface on the
  same network as <peer>", or "all paths fine".

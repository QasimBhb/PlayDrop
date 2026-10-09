//! D-Bus server on the session bus (`docs/ipc.md` section 2, SR-26).
//!
//! One `org.playdrop.Daemon1` object. Every method first authorises the caller: same UID as
//! the daemon, and either unsandboxed or our own Flatpak app. The verdict is cached per
//! sender unique name, which the bus never reuses, so a client pays the two bus round trips
//! once per connection rather than once per call.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::broadcast;
use zbus::fdo::{self, DBusProxy, RequestNameFlags, RequestNameReply};
use zbus::message::Header;
use zbus::names::{BusName, UniqueName};
use zbus::object_server::SignalEmitter;
use zbus::proxy::CacheProperties;
use zbus::zvariant::OwnedFd;
use zbus::{interface, Connection};

use super::{ServeError, ServerOptions};
use crate::linux::{caller_sandbox, own_uid, self_sandboxed};
use crate::util::AbortOnDrop;
use crate::{
    ErrorCode, Event, FilesOp, IpcError, Request, RequestHandler, Response, DBUS_NAME, DBUS_PATH,
    IPC_VERSION, MAX_MESSAGE_BYTES, MAX_SEND_ITEMS,
};

/// How many caller verdicts to remember. Each UI holds one bus connection, so a handful is
/// typical; the bound only stops a misbehaving local process from growing the map.
const VERDICT_CACHE_CAPACITY: usize = 256;

/// The running D-Bus server.
pub(super) struct Server {
    conn: Connection,
    _events: AbortOnDrop,
}

impl Server {
    pub(super) async fn shutdown(self) {
        if let Err(e) = self.conn.release_name(DBUS_NAME).await {
            tracing::debug!(error = %e, "releasing the bus name failed");
        }
        if let Err(e) = self
            .conn
            .object_server()
            .remove::<Daemon, _>(DBUS_PATH)
            .await
        {
            tracing::debug!(error = %e, "removing the bus object failed");
        }
        // Dropping the connection and aborting the event task closes the socket.
    }
}

fn transport(e: &zbus::Error) -> ServeError {
    ServeError::Transport(e.to_string())
}

pub(super) async fn start(
    handler: Arc<dyn RequestHandler>,
    events: broadcast::Sender<Event>,
    options: ServerOptions,
) -> Result<Server, ServeError> {
    let builder = match &options.bus_address {
        Some(address) => zbus::connection::Builder::address(address.as_str()),
        None => zbus::connection::Builder::session(),
    }
    .map_err(|e| transport(&e))?;
    let conn = builder.build().await.map_err(|e| transport(&e))?;

    let daemon = Daemon {
        handler,
        own_uid: own_uid(),
        version: options.daemon_version(),
        verdicts: Mutex::new(VerdictCache::new(VERDICT_CACHE_CAPACITY)),
    };
    conn.object_server()
        .at(DBUS_PATH, daemon)
        .await
        .map_err(|e| transport(&e))?;

    // Subscribe before the name is ours, so no event sent after a client could first reach
    // us is missed.
    let receiver = events.subscribe();

    // Owning the name is the single-instance check: no queueing, no replacing an existing
    // owner, and nobody may replace us.
    match conn
        .request_name_with_flags(DBUS_NAME, RequestNameFlags::DoNotQueue.into())
        .await
    {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {}
        Ok(_) | Err(zbus::Error::NameTaken) => return Err(ServeError::AlreadyRunning),
        Err(e) => return Err(transport(&e)),
    }
    tracing::info!(name = DBUS_NAME, "ipc server on the session bus");

    let task = tokio::spawn(forward_events(conn.clone(), receiver));
    Ok(Server {
        conn,
        _events: AbortOnDrop::new(task),
    })
}

/// Emit every daemon event as the `Event` signal until the daemon closes the channel.
async fn forward_events(conn: Connection, mut events: broadcast::Receiver<Event>) {
    let emitter = match SignalEmitter::new(&conn, DBUS_PATH) {
        Ok(emitter) => emitter,
        Err(e) => {
            tracing::error!(error = %e, "cannot emit ipc events");
            return;
        }
    };
    loop {
        match events.recv().await {
            Ok(event) => {
                let Ok(json) = serde_json::to_string(&event) else {
                    tracing::warn!("ipc event did not serialise, dropped");
                    continue;
                };
                if json.len() > MAX_MESSAGE_BYTES {
                    tracing::warn!("ipc event over the size cap, dropped");
                    continue;
                }
                if let Err(e) = Daemon::event(&emitter, &json).await {
                    tracing::warn!(error = %e, "emitting an ipc event failed");
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "ipc event forwarder fell behind, events dropped");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// The exported object.
struct Daemon {
    handler: Arc<dyn RequestHandler>,
    own_uid: u32,
    version: String,
    verdicts: Mutex<VerdictCache>,
}

// Must equal `DBUS_INTERFACE`; the macro needs a literal (tested below).
#[interface(name = "org.playdrop.PlayDrop.Daemon1")]
impl Daemon {
    /// One JSON `Request` in, one JSON `Response` out.
    async fn request(
        &self,
        request: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> String {
        if let Err(e) = self.authorize(conn, &header).await {
            return to_json(&Response::Err(e));
        }
        if request.len() > MAX_MESSAGE_BYTES {
            return to_json(&Response::err(
                ErrorCode::TooLarge,
                "request exceeds the IPC message size limit",
            ));
        }
        let Ok(request) = serde_json::from_str::<Request>(&request) else {
            tracing::debug!("ipc request did not parse");
            return to_json(&Response::err(
                ErrorCode::BadRequest,
                "request did not parse",
            ));
        };
        if matches!(
            request,
            Request::SendFiles { .. } | Request::QueueOutbox { .. }
        ) {
            // Paths are never accepted over the bus (SR-26).
            return to_json(&Response::err(
                ErrorCode::BadRequest,
                "files are sent with SendFiles or QueueOutboxFiles and file handles, not paths",
            ));
        }
        to_json(&self.dispatch(request).await)
    }

    /// Send files or folders, passed as file descriptors (SR-26).
    async fn send_files(
        &self,
        peer: String,
        files: Vec<OwnedFd>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> String {
        self.files(FilesOp::Send, peer, files, conn, &header).await
    }

    /// Queue files or folders for a peer to pull, passed as file descriptors (SR-26,
    /// protocol.md 7.8).
    async fn queue_outbox_files(
        &self,
        peer: String,
        files: Vec<OwnedFd>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> String {
        self.files(FilesOp::QueueOutbox, peer, files, conn, &header)
            .await
    }

    /// The contract version and the daemon's package version.
    #[zbus(out_args("ipc_version", "daemon_version"))]
    async fn version(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<(u32, String)> {
        // This method has no JSON answer to carry the refusal, so it uses the bus error.
        self.authorize(conn, &header)
            .await
            .map_err(|e| fdo::Error::AccessDenied(e.message))?;
        Ok((IPC_VERSION, self.version.clone()))
    }

    /// One JSON `Event`, broadcast to every client.
    #[zbus(signal)]
    async fn event(emitter: &SignalEmitter<'_>, event: &str) -> zbus::Result<()>;
}

impl Daemon {
    /// The shared body of `SendFiles` and `QueueOutboxFiles`: authorise, turn each
    /// descriptor back into a checked path, and hand the request to the daemon.
    async fn files(
        &self,
        op: FilesOp,
        peer: String,
        files: Vec<OwnedFd>,
        conn: &Connection,
        header: &Header<'_>,
    ) -> String {
        if let Err(e) = self.authorize(conn, header).await {
            return to_json(&Response::Err(e));
        }
        if files.is_empty() {
            return to_json(&Response::err(ErrorCode::BadRequest, "nothing to send"));
        }
        if files.len() > MAX_SEND_ITEMS {
            return to_json(&Response::err(
                ErrorCode::TooLarge,
                format!("send at most {MAX_SEND_ITEMS} items at once, or their folder"),
            ));
        }
        // Keep every descriptor open until the handler returns, so nothing is swapped
        // between the check and the core opening the path.
        let files: Vec<File> = files
            .into_iter()
            .map(|fd| File::from(std::os::fd::OwnedFd::from(fd)))
            .collect();
        let mut paths = Vec::with_capacity(files.len());
        for file in &files {
            match fd_path(file) {
                Ok(path) => paths.push(path),
                Err(reason) => {
                    tracing::debug!(?op, "ipc file handle refused");
                    return to_json(&Response::err(ErrorCode::BadRequest, reason));
                }
            }
        }
        let response = self.dispatch(op.request(peer, paths)).await;
        drop(files);
        to_json(&response)
    }

    async fn dispatch(&self, request: Request) -> Response {
        let op = request.op();
        tracing::debug!(op, "ipc request");
        let response = self.handler.handle(request).await;
        if let Some(code) = response.error_code() {
            tracing::debug!(op, %code, "ipc request failed");
        }
        response
    }

    /// SR-26: same UID, and unsandboxed or our own Flatpak app.
    async fn authorize(&self, conn: &Connection, header: &Header<'_>) -> Result<(), IpcError> {
        let refused = || IpcError::new(ErrorCode::Unauthorized, "caller is not authorised");
        let Some(sender) = header.sender() else {
            return Err(refused());
        };
        let cached = self
            .verdicts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(sender.as_str());
        let allowed = if let Some(allowed) = cached {
            allowed
        } else {
            // Lookup failures refuse this call but are not cached: they may be transient.
            let allowed = check_caller(conn, sender, self.own_uid)
                .await
                .ok_or_else(refused)?;
            self.verdicts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(sender.as_str().to_owned(), allowed);
            allowed
        };
        if allowed {
            Ok(())
        } else {
            Err(refused())
        }
    }
}

/// Run the SR-26 checks against the bus. `Some(verdict)` is definite and may be cached;
/// `None` means a lookup failed and the call is refused without caching.
async fn check_caller(conn: &Connection, sender: &UniqueName<'_>, own_uid: u32) -> Option<bool> {
    let dbus = match DBusProxy::builder(conn)
        .cache_properties(CacheProperties::No)
        .build()
        .await
    {
        Ok(dbus) => dbus,
        Err(e) => {
            tracing::warn!(error = %e, "ipc caller check: no bus proxy");
            return None;
        }
    };
    let name = BusName::from(sender.to_owned());
    let uid = match dbus.get_connection_unix_user(name.clone()).await {
        Ok(uid) => uid,
        Err(e) => {
            tracing::warn!(error = %e, "ipc caller check: UID lookup failed");
            return None;
        }
    };
    if uid != own_uid {
        tracing::warn!(uid, "ipc caller refused: another user");
        return Some(false);
    }
    if self_sandboxed() {
        // Inside Flatpak the bus PIDs are not resolvable here; the UID check above stands.
        return Some(true);
    }
    let pid = match dbus.get_connection_unix_process_id(name).await {
        Ok(pid) => pid,
        Err(e) => {
            tracing::warn!(error = %e, "ipc caller check: PID lookup failed");
            return None;
        }
    };
    match caller_sandbox(pid) {
        Ok(sandbox) if sandbox.allowed() => Some(true),
        Ok(_) => {
            tracing::warn!(uid, "ipc caller refused: another Flatpak app");
            Some(false)
        }
        Err(e) => {
            tracing::warn!(uid, error = %e, "ipc caller refused: sandbox unreadable");
            None
        }
    }
}

/// Resolve a passed descriptor to the path the daemon will send.
///
/// Only regular files and directories pass. The path comes from `/proc/self/fd/<n>` and must
/// still name the same inode as the descriptor, so a deleted, moved or out-of-namespace file
/// is refused rather than sending whatever now sits at that path.
fn fd_path(file: &File) -> Result<PathBuf, &'static str> {
    let meta = file
        .metadata()
        .map_err(|_| "a file handle could not be read")?;
    if !(meta.is_file() || meta.is_dir()) {
        return Err("only regular files and folders can be sent");
    }
    let link = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|_| "a file handle has no path")?;
    if !link.is_absolute() {
        return Err("a file handle has no path");
    }
    let at_path =
        std::fs::metadata(&link).map_err(|_| "a file was moved or is not visible to PlayDrop")?;
    if at_path.dev() != meta.dev() || at_path.ino() != meta.ino() {
        return Err("a file was moved or is not visible to PlayDrop");
    }
    Ok(link)
}

fn to_json(response: &Response) -> String {
    serde_json::to_string(response).unwrap_or_else(|_| {
        r#"{"result":"err","code":"internal","message":"response did not serialise"}"#.to_owned()
    })
}

/// Caller verdicts by sender unique name, oldest evicted first.
struct VerdictCache {
    map: HashMap<String, bool>,
    order: VecDeque<String>,
    capacity: usize,
}

impl VerdictCache {
    fn new(capacity: usize) -> Self {
        Self {
            map: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn get(&self, sender: &str) -> Option<bool> {
        self.map.get(sender).copied()
    }

    fn insert(&mut self, sender: String, allowed: bool) {
        if self.map.insert(sender.clone(), allowed).is_none() {
            self.order.push_back(sender);
            while self.order.len() > self.capacity {
                if let Some(oldest) = self.order.pop_front() {
                    self.map.remove(&oldest);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_name_matches_the_contract() {
        use zbus::object_server::Interface as _;
        assert_eq!(Daemon::name().as_str(), crate::DBUS_INTERFACE);
    }

    #[test]
    fn verdict_cache_is_bounded() {
        let mut cache = VerdictCache::new(2);
        cache.insert(":1.1".into(), true);
        cache.insert(":1.2".into(), false);
        cache.insert(":1.2".into(), false);
        assert_eq!(cache.get(":1.1"), Some(true));
        cache.insert(":1.3".into(), true);
        assert_eq!(cache.get(":1.1"), None);
        assert_eq!(cache.get(":1.2"), Some(false));
        assert_eq!(cache.get(":1.3"), Some(true));
        assert_eq!(cache.map.len(), 2);
    }

    #[test]
    fn fd_path_accepts_files_and_dirs_and_refuses_others() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("a.txt");
        std::fs::write(&file_path, b"x").expect("write");

        let file = File::open(&file_path).expect("open");
        assert_eq!(
            fd_path(&file).expect("file"),
            std::fs::canonicalize(&file_path).expect("canon")
        );
        let folder = File::open(dir.path()).expect("open dir");
        assert_eq!(
            fd_path(&folder).expect("dir"),
            std::fs::canonicalize(dir.path()).expect("canon")
        );

        let null = File::open("/dev/null").expect("open null");
        assert!(fd_path(&null).is_err());

        // A deleted file no longer resolves to the same inode.
        std::fs::remove_file(&file_path).expect("rm");
        assert!(fd_path(&file).is_err());
    }
}

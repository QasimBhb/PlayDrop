//! D-Bus client (`docs/ipc.md` section 2, SR-26).

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use rustix::fs::{FileType, Mode, OFlags};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use zbus::fdo::{self, DBusProxy};
use zbus::names::{BusName, OwnedUniqueName};
use zbus::proxy::CacheProperties;
use zbus::zvariant::Fd;
use zbus::{proxy, Connection};

use super::{ClientOptions, ConnectError};
use crate::linux::{is_daemon_exe, own_uid, process_exe, self_sandboxed, DAEMON_EXE_NAME};
use crate::pipe::events_stream;
use crate::util::{AbortOnDrop, EVENT_CAPACITY};
use crate::{
    ErrorCode, IpcError, Ok, Request, Response, DBUS_NAME, IPC_VERSION, MAX_MESSAGE_BYTES,
};

// The proxy macro generates a signal type named `Event`, so the contract's `Event` is
// written `crate::Event` in this file.
// Must equal `DBUS_INTERFACE` and `DBUS_PATH`; the macro needs literals (tested below).
#[proxy(
    interface = "org.playdrop.PlayDrop.Daemon1",
    default_path = "/org/playdrop/PlayDrop/Daemon1",
    gen_blocking = false
)]
trait Daemon1 {
    fn request(&self, request: &str) -> zbus::Result<String>;

    fn send_files(&self, peer: &str, files: &[Fd<'_>]) -> zbus::Result<String>;

    fn version(&self) -> zbus::Result<(u32, String)>;

    #[zbus(signal)]
    fn event(&self, event: String) -> zbus::Result<()>;
}

/// A verified connection to the daemon.
pub(super) struct Client {
    proxy: Daemon1Proxy<'static>,
    daemon_version: String,
    events: broadcast::WeakSender<crate::Event>,
    _forwarder: AbortOnDrop,
}

fn transport(e: impl std::fmt::Display) -> ConnectError {
    ConnectError::Transport(e.to_string())
}

impl Client {
    pub(super) async fn connect(options: ClientOptions) -> Result<Self, ConnectError> {
        let conn = match &options.bus_address {
            Some(address) => {
                zbus::connection::Builder::address(address.as_str())
                    .map_err(transport)?
                    .build()
                    .await
            }
            None => Connection::session().await,
        }
        .map_err(transport)?;
        let dbus = DBusProxy::builder(&conn)
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .map_err(transport)?;

        let owner = name_owner(&dbus).await?;
        verify_owner(&dbus, &owner, options.verify_exe).await?;

        // From here on only the verified unique name is addressed, and only its signals
        // are accepted.
        let proxy = Daemon1Proxy::builder(&conn)
            .destination(owner.clone())
            .map_err(transport)?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .map_err(transport)?;

        // Subscribe before the version call so no event after "connected" is missed.
        let signals = proxy.receive_event().await.map_err(transport)?;
        let owner_gone = dbus
            .receive_name_owner_changed_with_args(&[(0, owner.as_str())])
            .await
            .map_err(transport)?;

        let (ipc_version, daemon_version) =
            proxy
                .version()
                .await
                .map_err(|e| match fdo::Error::from(e) {
                    fdo::Error::AccessDenied(m) => {
                        ConnectError::Transport(format!("the daemon refused this client: {m}"))
                    }
                    other => transport(other),
                })?;
        if ipc_version != IPC_VERSION {
            return Err(ConnectError::Version {
                daemon: ipc_version,
                client: IPC_VERSION,
            });
        }

        let (events_tx, _) = broadcast::channel(EVENT_CAPACITY);
        let events = events_tx.downgrade();
        let forwarder = tokio::spawn(forward_events(signals, owner_gone, events_tx));
        Result::Ok(Self {
            proxy,
            daemon_version,
            events,
            _forwarder: AbortOnDrop::new(forwarder),
        })
    }

    pub(super) fn daemon_version(&self) -> &str {
        &self.daemon_version
    }

    pub(super) async fn request(&self, request: &Request) -> Result<Ok, IpcError> {
        let json = serde_json::to_string(request)
            .map_err(|_| IpcError::new(ErrorCode::Internal, "request did not serialise"))?;
        if json.len() > MAX_MESSAGE_BYTES {
            return Err(IpcError::new(
                ErrorCode::TooLarge,
                "the request exceeds the IPC message size limit",
            ));
        }
        let answer = self
            .proxy
            .request(&json)
            .await
            .map_err(|e| call_failed(&e))?;
        parse_response(&answer)
    }

    pub(super) async fn send_files(&self, peer: &str, paths: &[PathBuf]) -> Result<Ok, IpcError> {
        let paths = paths.to_vec();
        let fds = tokio::task::spawn_blocking(move || {
            paths
                .iter()
                .map(|path| open_for_send(path))
                .collect::<Result<Vec<OwnedFd>, IpcError>>()
        })
        .await
        .map_err(|_| IpcError::new(ErrorCode::Internal, "opening the files failed"))??;
        let handles: Vec<Fd<'_>> = fds.iter().map(Fd::from).collect();
        let answer = self
            .proxy
            .send_files(peer, &handles)
            .await
            .map_err(|e| call_failed(&e))?;
        parse_response(&answer)
    }

    pub(super) fn events(&self) -> BroadcastStream<crate::Event> {
        events_stream(&self.events)
    }
}

/// The current owner of the daemon name, asking the bus to start the daemon when there is
/// none. The bus starts it only if the daemon's `.service` file is installed.
async fn name_owner(dbus: &DBusProxy<'_>) -> Result<OwnedUniqueName, ConnectError> {
    let name = BusName::try_from(DBUS_NAME).map_err(transport)?;
    match dbus.get_name_owner(name.clone()).await {
        Result::Ok(owner) => return Result::Ok(owner),
        Err(fdo::Error::NameHasNoOwner(_)) => {}
        Err(e) => return Err(transport(e)),
    }
    let well_known = zbus::names::WellKnownName::try_from(DBUS_NAME).map_err(transport)?;
    match dbus.start_service_by_name(well_known, 0).await {
        Result::Ok(_) => {}
        Err(fdo::Error::ServiceUnknown(_) | fdo::Error::NameHasNoOwner(_)) => {
            return Err(ConnectError::NotRunning)
        }
        Err(e) => return Err(transport(e)),
    }
    match dbus.get_name_owner(name).await {
        Result::Ok(owner) => Result::Ok(owner),
        Err(fdo::Error::NameHasNoOwner(_)) => Err(ConnectError::NotRunning),
        Err(e) => Err(transport(e)),
    }
}

/// SR-26 client checks: the owner runs as our UID and, unless `verify_exe` is off (tests
/// only), its executable is `playdropd`.
async fn verify_owner(
    dbus: &DBusProxy<'_>,
    owner: &OwnedUniqueName,
    verify_exe: bool,
) -> Result<(), ConnectError> {
    let name = BusName::from(owner.as_ref().to_owned());
    let uid = dbus
        .get_connection_unix_user(name.clone())
        .await
        .map_err(transport)?;
    if uid != own_uid() {
        tracing::warn!(uid, "ipc daemon refused: another user owns the name");
        return Err(ConnectError::Unverified(format!(
            "the daemon name is owned by user {uid}, not by you"
        )));
    }
    if !verify_exe || self_sandboxed() {
        // Tests run the server in-process; inside Flatpak the bus PID is not resolvable
        // here. The UID check above stands in both cases.
        return Result::Ok(());
    }
    let pid = dbus
        .get_connection_unix_process_id(name)
        .await
        .map_err(transport)?;
    let exe = process_exe(pid).map_err(|e| {
        ConnectError::Unverified(format!("cannot read the daemon's executable: {e}"))
    })?;
    if !is_daemon_exe(&exe) {
        tracing::warn!("ipc daemon refused: name owner is not {DAEMON_EXE_NAME}");
        return Err(ConnectError::Unverified(format!(
            "the daemon name is owned by {}, not {DAEMON_EXE_NAME}",
            exe.display()
        )));
    }
    Result::Ok(())
}

/// Republish `Event` signals on the client's broadcast channel until the daemon's
/// connection leaves the bus. Dropping the sender then ends every event stream.
async fn forward_events(
    mut signals: EventStream,
    mut owner_gone: fdo::NameOwnerChangedStream,
    events: broadcast::Sender<crate::Event>,
) {
    loop {
        tokio::select! {
            signal = signals.next() => {
                let Some(signal) = signal else { break };
                let Result::Ok(args) = signal.args() else {
                    tracing::debug!("ignoring a malformed daemon event");
                    continue;
                };
                if let Result::Ok(event) = serde_json::from_str::<crate::Event>(&args.event) {
                    // No subscriber is not an error.
                    let _ = events.send(event);
                } else {
                    // A newer daemon may send events this client does not know.
                    tracing::debug!("ignoring an unrecognised daemon event");
                }
            }
            _ = owner_gone.next() => {
                tracing::info!("the PlayDrop daemon left the bus");
                break;
            }
        }
    }
}

/// Open one path for passing to the daemon: files read-only, folders as `O_PATH`.
fn open_for_send(path: &Path) -> Result<OwnedFd, IpcError> {
    let not_found = |e: std::io::Error| {
        IpcError::new(
            ErrorCode::NotFound,
            format!("cannot open {}: {e}", path.display()),
        )
    };
    let meta = std::fs::metadata(path).map_err(not_found)?;
    let (flags, want_dir) = if meta.is_dir() {
        (OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC, true)
    } else if meta.is_file() {
        (OFlags::RDONLY | OFlags::CLOEXEC, false)
    } else {
        return Err(IpcError::new(
            ErrorCode::BadRequest,
            format!("{} is not a regular file or folder", path.display()),
        ));
    };
    let fd = rustix::fs::open(path, flags, Mode::empty()).map_err(|e| not_found(e.into()))?;
    let stat = rustix::fs::fstat(&fd).map_err(|e| not_found(e.into()))?;
    let kind = FileType::from_raw_mode(stat.st_mode);
    let matches = if want_dir {
        kind == FileType::Directory
    } else {
        kind == FileType::RegularFile
    };
    if !matches {
        return Err(IpcError::new(
            ErrorCode::BadRequest,
            format!("{} changed while it was being opened", path.display()),
        ));
    }
    Result::Ok(fd)
}

fn call_failed(e: &zbus::Error) -> IpcError {
    IpcError::new(
        ErrorCode::Internal,
        format!("the PlayDrop daemon did not answer: {e}"),
    )
}

fn parse_response(answer: &str) -> Result<Ok, IpcError> {
    serde_json::from_str::<Response>(answer)
        .map_err(|_| {
            IpcError::new(
                ErrorCode::Internal,
                "the daemon sent an answer this version does not understand",
            )
        })?
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_literals_match_the_contract() {
        // The generated proxy's defaults come from the attribute literals.
        assert_eq!(
            <Daemon1Proxy<'_> as zbus::proxy::Defaults>::INTERFACE
                .as_ref()
                .map(zbus::names::InterfaceName::as_str),
            Some(crate::DBUS_INTERFACE)
        );
        assert_eq!(
            <Daemon1Proxy<'_> as zbus::proxy::Defaults>::PATH
                .as_ref()
                .map(zbus::zvariant::ObjectPath::as_str),
            Some(crate::DBUS_PATH)
        );
    }
}

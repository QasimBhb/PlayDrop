//! D-Bus client (`docs/ipc.md` section 2, SR-26).

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use futures_util::{FutureExt as _, StreamExt};
use rustix::fs::{FileType, Mode, OFlags};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use zbus::fdo::{self, DBusProxy};
use zbus::names::{BusName, OwnedUniqueName};
use zbus::proxy::CacheProperties;
use zbus::zvariant::Fd;
use zbus::{proxy, Connection};

use super::{Activation, ClientOptions, ConnectError};
use crate::linux::{is_daemon_exe, own_uid, process_exe, self_sandboxed, DAEMON_EXE_NAME};
use crate::pipe::events_stream;
use crate::util::{AbortOnDrop, EVENT_CAPACITY};
use crate::{
    ErrorCode, FilesOp, IpcError, Ok, Request, Response, DBUS_NAME, IPC_VERSION, MAX_MESSAGE_BYTES,
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

    fn queue_outbox_files(&self, peer: &str, files: &[Fd<'_>]) -> zbus::Result<String>;

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
    pub(super) async fn connect(
        options: ClientOptions,
        activation: Activation,
    ) -> Result<Self, ConnectError> {
        let conn = bus(&options).await?;
        let dbus = DBusProxy::builder(&conn)
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .map_err(transport)?;

        let owner = name_owner(&dbus, activation).await?;
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

    pub(super) async fn files(
        &self,
        op: FilesOp,
        peer: &str,
        paths: &[PathBuf],
    ) -> Result<Ok, IpcError> {
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
        let answer = match op {
            FilesOp::Send => self.proxy.send_files(peer, &handles).await,
            FilesOp::QueueOutbox => self.proxy.queue_outbox_files(peer, &handles).await,
        }
        .map_err(|e| call_failed(&e))?;
        parse_response(&answer)
    }

    pub(super) fn events(&self) -> BroadcastStream<crate::Event> {
        events_stream(&self.events)
    }
}

/// The session bus, or the one `options` names.
async fn bus(options: &ClientOptions) -> Result<Connection, ConnectError> {
    match &options.bus_address {
        Some(address) => {
            zbus::connection::Builder::address(address.as_str())
                .map_err(transport)?
                .build()
                .await
        }
        None => Connection::session().await,
    }
    .map_err(transport)
}

/// Whether a `NameOwnerChanged` new-owner argument means the name now has an owner (the bus
/// sends an empty string when the owner leaves).
fn owner_appeared(new_owner: Option<&str>) -> bool {
    new_owner.is_some_and(|o| !o.is_empty())
}

/// One bus connection subscribed to the daemon name's owner changes.
pub(super) struct Watch {
    dbus: DBusProxy<'static>,
    changes: fdo::NameOwnerChangedStream,
}

impl Watch {
    pub(super) async fn new(options: &ClientOptions) -> Result<Self, ConnectError> {
        let conn = bus(options).await?;
        let dbus = DBusProxy::builder(&conn)
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .map_err(transport)?;
        let changes = dbus
            .receive_name_owner_changed_with_args(&[(0, DBUS_NAME)])
            .await
            .map_err(transport)?;
        Result::Ok(Self { dbus, changes })
    }

    /// Resolves once the daemon name has an owner, never asking the bus to start it.
    pub(super) async fn appeared(&mut self) -> Result<(), ConnectError> {
        // Changes queued while nobody waited are history: drop them, then look at the name
        // now. The subscription stays, so an owner arriving after the look is not missed.
        while let Some(Some(_)) = self.changes.next().now_or_never() {}
        let name = BusName::try_from(DBUS_NAME).map_err(transport)?;
        if self.dbus.name_has_owner(name).await.map_err(transport)? {
            return Result::Ok(());
        }
        while let Some(change) = self.changes.next().await {
            let Result::Ok(args) = change.args() else {
                continue;
            };
            // A change that was still on the socket during the look above can be old (an
            // owner that came and went), so ask again before saying the daemon is there.
            if owner_appeared(args.new_owner().as_deref()) {
                let name = BusName::try_from(DBUS_NAME).map_err(transport)?;
                if self.dbus.name_has_owner(name).await.map_err(transport)? {
                    return Result::Ok(());
                }
            }
        }
        Err(ConnectError::Transport(
            "the session bus connection closed".to_owned(),
        ))
    }
}

/// The current owner of the daemon name. With [`Activation::Allow`], asks the bus to start
/// the daemon when there is none; the bus starts it only if the daemon's `.service` file is
/// installed. With [`Activation::Never`], no owner is [`ConnectError::NotRunning`].
async fn name_owner(
    dbus: &DBusProxy<'_>,
    activation: Activation,
) -> Result<OwnedUniqueName, ConnectError> {
    let name = BusName::try_from(DBUS_NAME).map_err(transport)?;
    match dbus.get_name_owner(name.clone()).await {
        Result::Ok(owner) => return Result::Ok(owner),
        Err(fdo::Error::NameHasNoOwner(_)) => {}
        Err(e) => return Err(transport(e)),
    }
    if activation == Activation::Never {
        return Err(ConnectError::NotRunning);
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
    fn only_a_non_empty_new_owner_counts_as_appeared() {
        assert!(owner_appeared(Some(":1.42")));
        assert!(!owner_appeared(Some("")));
        assert!(!owner_appeared(None));
    }

    /// A private bus for the activation tests, killed on drop. Its configuration lists one
    /// service directory holding a `.service` file for the daemon name whose `Exec` only
    /// touches a marker file, so the test can see whether the bus was asked to start it.
    struct Bus {
        child: std::process::Child,
        address: String,
        marker: std::path::PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Bus {
        /// `None` (the test skips) when `dbus-daemon` cannot be run, like the transport's
        /// other D-Bus tests.
        fn start() -> Option<Self> {
            use std::io::BufRead as _;
            // A path without spaces: the `Exec` line is split like a shell command.
            let dir = tempfile::Builder::new()
                .prefix("pd-ipc-bus")
                .tempdir_in("/tmp")
                .expect("temp dir");
            let services = dir.path().join("services");
            std::fs::create_dir(&services).expect("services dir");
            let marker = dir.path().join("activated");
            std::fs::write(
                services.join(format!("{DBUS_NAME}.service")),
                format!(
                    "[D-BUS Service]\nName={DBUS_NAME}\nExec=/usr/bin/touch {}\n",
                    marker.display()
                ),
            )
            .expect("service file");
            let config = dir.path().join("bus.conf");
            std::fs::write(
                &config,
                format!(
                    r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <servicedir>{}</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                    services.display()
                ),
            )
            .expect("bus config");
            let mut child = match std::process::Command::new("dbus-daemon")
                .arg(format!("--config-file={}", config.display()))
                .args(["--nofork", "--print-address"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Result::Ok(c) => c,
                Err(e) => {
                    eprintln!("skipping: cannot start dbus-daemon ({e})");
                    return None;
                }
            };
            let mut line = String::new();
            std::io::BufReader::new(child.stdout.take()?)
                .read_line(&mut line)
                .ok()?;
            if line.trim().is_empty() {
                // It ran but printed no address (it refused the config): skip, not fail.
                eprintln!("skipping: dbus-daemon gave no address");
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Some(Self {
                child,
                address: line.trim().to_owned(),
                marker,
                _dir: dir,
            })
        }

        fn options(&self) -> ClientOptions {
            ClientOptions {
                verify_exe: false,
                bus_address: Some(self.address.clone()),
            }
        }
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[tokio::test]
    async fn never_does_not_ask_the_bus_to_start_the_daemon_and_allow_does() {
        let Some(bus) = Bus::start() else { return };
        let bound = std::time::Duration::from_secs(10);

        let never = tokio::time::timeout(bound, Client::connect(bus.options(), Activation::Never))
            .await
            .expect("answered in time");
        assert!(matches!(never, Err(ConnectError::NotRunning)));
        // Give a stray activation time to run before looking.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!bus.marker.exists(), "`Never` started the service");

        // `Allow` asks the bus, which runs the service file's `Exec`. That program never
        // takes the name, so the connect itself fails or times out; only the marker matters.
        // The bus then waits for the name, so do not wait for the connect long.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            Client::connect(bus.options(), Activation::Allow),
        )
        .await;
        let deadline = std::time::Instant::now() + bound;
        while !bus.marker.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            bus.marker.exists(),
            "`Allow` did not ask the bus to start it"
        );
    }

    #[tokio::test]
    async fn one_watch_sees_the_name_appear_again_and_again() {
        let Some(bus) = Bus::start() else { return };
        let five = std::time::Duration::from_secs(5);
        let options = bus.options();
        let mut watch = Watch::new(&options).await.expect("watch");
        let own = || async {
            zbus::connection::Builder::address(bus.address.as_str())
                .expect("address")
                .name(DBUS_NAME)
                .expect("name")
                .build()
                .await
                .expect("own the name")
        };

        // Nobody owns it: still waiting after a while.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), watch.appeared())
                .await
                .is_err(),
            "resolved before anyone owned the name"
        );
        // Someone takes it: the same watch resolves.
        let owner = own().await;
        tokio::time::timeout(five, watch.appeared())
            .await
            .expect("resolved in time")
            .expect("owner seen");
        // Owned now: resolves at once.
        tokio::time::timeout(five, watch.appeared())
            .await
            .expect("immediate")
            .expect("owner seen");
        // The owner leaves: the queued "appeared" and "left" changes are history, so the
        // watch waits again, and resolves when the name is taken a second time.
        drop(owner);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), watch.appeared())
                .await
                .is_err(),
            "an old change was replayed"
        );
        let _again = own().await;
        tokio::time::timeout(five, watch.appeared())
            .await
            .expect("resolved in time")
            .expect("owner seen");
    }

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

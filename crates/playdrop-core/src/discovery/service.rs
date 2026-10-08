//! The [`Discovery`] facade, `docs/protocol.md` section 8.
//!
//! [`Discovery::start`] runs the beacon sender, the beacon receiver, mDNS and a small
//! watcher as concurrent tasks (protocol.md 8: concurrent, never sequential) and hands back
//! a channel of [`DiscoveryEvent`]s. The tasks share one [`Processor`], so both carriers use
//! the same advert cache and "Add device" list.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::advert::{valid_port, AddDeviceFields, Advert};
use super::beacon::{self, BeaconReceiver, BeaconSchedule};
use super::cache::AddDeviceEntry;
use super::engine::{Context, Inbound, OwnNonces, Processor};
use super::mdns;
use super::tag::{tag, PeerKeys, ANNOUNCE_KEY_LEN, NONCE_LEN};
use super::{usable_interfaces, DiscoveryEvent, InterfaceSource, Ipv4Interface};
use crate::constants::{MULTICAST_GROUP, PROTOCOL_VERSION, UDP_PORT};
use crate::{Error, Result};

/// The multicast group of another LAN file-sharing app, never to be joined
/// (protocol.md section 1).
const FORBIDDEN_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);

/// Settings for [`Discovery::start`] (protocol.md 8).
#[derive(Clone)]
pub struct DiscoveryConfig {
    /// This device's own announce key K, used to tag its adverts (protocol.md 8.2).
    pub announce_key: [u8; ANNOUNCE_KEY_LEN],
    /// The TCP listening port, advertised only in "Add device" mode and in the SRV record.
    pub listen_port: u16,
    /// Beacon send schedule: desktop or mobile (protocol.md 8.1).
    pub schedule: BeaconSchedule,
    /// Where the host's IPv4 interfaces come from (protocol.md 8.1, SR-3).
    pub interfaces: Arc<dyn InterfaceSource>,
    /// Run the beacon sender and receiver.
    pub beacon: bool,
    /// Run mDNS registration and browsing.
    pub mdns: bool,
    /// Beacon group and port. Defaults to `239.255.42.187:42187`; tests override the port.
    pub beacon_group: SocketAddrV4,
    /// How often the interface list is re-read to detect network changes. *Choice:* 5 s
    /// notices a Wi-Fi switch quickly at the cost of one cheap system call.
    pub interface_poll: Duration,
    /// Event channel depth. Events beyond it are dropped, never queued without bound (SR-2).
    pub event_capacity: usize,
}

impl DiscoveryConfig {
    /// Defaults for a desktop host: both carriers on, the protocol group and port.
    pub fn new(
        announce_key: [u8; ANNOUNCE_KEY_LEN],
        listen_port: u16,
        interfaces: Arc<dyn InterfaceSource>,
    ) -> Self {
        Self {
            announce_key,
            listen_port,
            schedule: BeaconSchedule::Desktop,
            interfaces,
            beacon: true,
            mdns: true,
            beacon_group: SocketAddrV4::new(MULTICAST_GROUP, UDP_PORT),
            interface_poll: Duration::from_secs(5),
            event_capacity: 256,
        }
    }

    fn validate(&self) -> Result<()> {
        if !valid_port(self.listen_port) {
            return Err(Error::BadRequest(format!(
                "listen port {} is not allowed",
                self.listen_port
            )));
        }
        if !valid_port(self.beacon_group.port()) {
            return Err(Error::BadRequest(format!(
                "beacon port {} is not allowed",
                self.beacon_group.port()
            )));
        }
        let group = *self.beacon_group.ip();
        if !group.is_multicast() || group == FORBIDDEN_GROUP {
            return Err(Error::BadRequest(format!(
                "beacon group {group} is not allowed"
            )));
        }
        Ok(())
    }
}

/// State shared by all discovery tasks.
pub(crate) struct Shared {
    pub(crate) group: SocketAddrV4,
    pub(crate) listen_port: u16,
    pub(crate) schedule: BeaconSchedule,
    own_key: RwLock<[u8; ANNOUNCE_KEY_LEN]>,
    add_fields: RwLock<Option<(String, String)>>,
    window: Arc<AtomicBool>,
    keys: Arc<dyn PeerKeys>,
    iface_source: Arc<dyn InterfaceSource>,
    interfaces: RwLock<Vec<Ipv4Interface>>,
    processor: Mutex<Processor>,
    pub(crate) own: OwnNonces,
    events: mpsc::Sender<DiscoveryEvent>,
    pub(crate) refresh: watch::Sender<u64>,
    pub(crate) burst: watch::Sender<u64>,
    pub(crate) stop: watch::Sender<bool>,
}

impl Shared {
    /// Builds this host's advert for `nonce`: always `v`, `n`, `t`; plus name, type and port
    /// only while the "Add device" window is open and fields are set (protocol.md 8.1, SR-4).
    pub(crate) fn advert(&self, nonce: [u8; NONCE_LEN]) -> Advert {
        let key = *read(&self.own_key);
        let add_device = if self.window.load(Ordering::Acquire) {
            read(&self.add_fields)
                .as_ref()
                .map(|(name, device_type)| AddDeviceFields {
                    name: name.clone(),
                    device_type: device_type.clone(),
                    port: self.listen_port,
                })
        } else {
            None
        };
        Advert {
            v: PROTOCOL_VERSION,
            n: nonce,
            t: tag(&key, &nonce),
            add_device,
        }
    }

    /// Runs one parsed advert through the shared processor.
    pub(crate) fn handle(&self, input: Inbound<'_>, now: Instant) -> Option<DiscoveryEvent> {
        let interfaces = read(&self.interfaces);
        let ctx = Context {
            keys: self.keys.as_ref(),
            add_device_window: self.window.load(Ordering::Acquire),
            interfaces: &interfaces,
            own: &self.own,
        };
        lock(&self.processor).handle(input, &ctx, now)
    }

    /// Queues an event, dropping it if the host is not keeping up (SR-2).
    pub(crate) fn emit(&self, event: DiscoveryEvent) {
        if self.events.try_send(event).is_err() {
            tracing::debug!("discovery event dropped: channel full or closed");
        }
    }

    /// Current usable interfaces.
    pub(crate) fn interfaces(&self) -> Vec<Ipv4Interface> {
        read(&self.interfaces).clone()
    }

    /// Re-reads the interface list; returns `true` if it changed.
    fn reload_interfaces(&self) -> bool {
        let fresh = usable_interfaces(self.iface_source.ipv4_interfaces());
        let mut cur = self
            .interfaces
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if *cur == fresh {
            return false;
        }
        *cur = fresh;
        true
    }

    fn bump_refresh(&self) {
        self.refresh.send_modify(|n| *n = n.wrapping_add(1));
    }

    pub(crate) fn stop_requested(&self) -> watch::Receiver<bool> {
        self.stop.subscribe()
    }
}

/// Handle to running discovery (protocol.md 8). Dropping it aborts the tasks; call
/// [`Discovery::stop`] instead to withdraw the mDNS record cleanly.
pub struct Discovery {
    shared: Arc<Shared>,
    tasks: Vec<JoinHandle<()>>,
}

impl Discovery {
    /// Starts discovery. Must be called inside a Tokio runtime.
    ///
    /// `keys` lists pinned peers' announce keys (SR-1, SR-5). `add_device_window` is the
    /// host's "Add device" flag (protocol.md 6.1): while it is `true`, this host's adverts
    /// carry name, type and port (if set with [`Discovery::set_add_device_fields`]) and
    /// named adverts from others are reported as [`DiscoveryEvent::Unpaired`].
    ///
    /// Fails if the configuration names a forbidden port or group (protocol.md section 1),
    /// or if the beacon socket or mDNS daemon cannot be created.
    pub fn start(
        config: DiscoveryConfig,
        keys: Arc<dyn PeerKeys>,
        add_device_window: Arc<AtomicBool>,
    ) -> Result<(Self, mpsc::Receiver<DiscoveryEvent>)> {
        config.validate()?;
        let (tx, rx) = mpsc::channel(config.event_capacity.max(1));
        let shared = Arc::new(Shared {
            group: config.beacon_group,
            listen_port: config.listen_port,
            schedule: config.schedule,
            own_key: RwLock::new(config.announce_key),
            add_fields: RwLock::new(None),
            window: add_device_window,
            keys,
            iface_source: config.interfaces,
            interfaces: RwLock::new(Vec::new()),
            processor: Mutex::new(Processor::new()),
            own: OwnNonces::default(),
            events: tx,
            refresh: watch::Sender::new(0),
            burst: watch::Sender::new(0),
            stop: watch::Sender::new(false),
        });
        shared.reload_interfaces();

        // Create sockets and the daemon up front so failures reach the caller.
        let receiver = if config.beacon {
            Some(BeaconReceiver::bind(config.beacon_group).map_err(Error::Io)?)
        } else {
            None
        };
        let daemon = if config.mdns {
            Some(mdns::new_daemon()?)
        } else {
            None
        };

        let mut tasks = Vec::new();
        if let Some(receiver) = receiver {
            tasks.push(tokio::spawn(beacon::run_sender(shared.clone())));
            tasks.push(tokio::spawn(beacon::run_receiver(shared.clone(), receiver)));
        }
        if let Some(daemon) = daemon {
            tasks.push(tokio::spawn(mdns::run(shared.clone(), daemon)));
        }
        tasks.push(tokio::spawn(watch_host(
            shared.clone(),
            config.interface_poll,
        )));
        Ok((Self { shared, tasks }, rx))
    }

    /// Stops all tasks, withdrawing the mDNS record first. Waits up to 2 s per task.
    pub async fn stop(mut self) {
        self.shared.stop.send_replace(true);
        for task in self.tasks.drain(..) {
            let abort = task.abort_handle();
            if tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .is_err()
            {
                abort.abort();
            }
        }
    }

    /// Tells discovery the network changed (protocol.md 8.1): re-reads interfaces, rejoins
    /// the beacon group, sends a fresh 0/1/3 s burst and rotates the mDNS instance name and
    /// nonce (SR-4).
    pub fn network_changed(&self) {
        self.shared.reload_interfaces();
        self.shared.bump_refresh();
    }

    /// Sends a beacon burst now, without rotating mDNS. Mobile hosts with
    /// [`BeaconSchedule::DiscoveryWindowsOnly`] call this when a discovery window opens.
    pub fn announce_now(&self) {
        self.shared.burst.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Sets the name and type shown in this host's adverts while "Add device" is open
    /// (protocol.md 8.1). The name is sanitised; an unusable name or type is rejected.
    pub fn set_add_device_fields(&self, name: &str, device_type: &str) -> Result<()> {
        let parsed = AddDeviceFields::parse(name, device_type, self.shared.listen_port)
            .ok_or_else(|| Error::BadRequest("unusable add-device name or type".into()))?;
        *self
            .shared
            .add_fields
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some((parsed.name, parsed.device_type));
        if self.shared.window.load(Ordering::Acquire) {
            self.shared.bump_refresh();
        }
        Ok(())
    }

    /// Replaces this host's announce key after a rotation (protocol.md 8.2, SR-5) and
    /// rotates the adverts so the old key's tags stop at once.
    pub fn set_announce_key(&self, key: [u8; ANNOUNCE_KEY_LEN]) {
        *self
            .shared
            .own_key
            .write()
            .unwrap_or_else(PoisonError::into_inner) = key;
        self.shared.bump_refresh();
    }

    /// Drops cached tag outcomes after pins change, for `PeerKeys` implementations that do
    /// not report a generation (SR-5).
    pub fn peers_changed(&self) {
        lock(&self.shared.processor).peers_changed();
    }

    /// Unpaired devices currently in the "Add device" list (protocol.md 8.3 rule 3).
    pub fn add_device_list(&self) -> Vec<(SocketAddr, AddDeviceEntry)> {
        lock(&self.shared.processor).add_device_entries(Instant::now())
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Watches for host changes the tasks must react to: the "Add device" flag flipping and
/// the interface list changing. Either one triggers a refresh (protocol.md 8.1, SR-4).
///
/// *Choice:* rotating on every window edge also means the named advert and the anonymous
/// one never share an mDNS instance name, so they cannot be linked.
async fn watch_host(shared: Arc<Shared>, interface_poll: Duration) {
    let mut stop = shared.stop_requested();
    let mut window_tick = tokio::time::interval(Duration::from_millis(500));
    let mut iface_tick = tokio::time::interval(interface_poll.max(Duration::from_millis(100)));
    window_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    iface_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut window_was = shared.window.load(Ordering::Acquire);
    loop {
        tokio::select! {
            _ = window_tick.tick() => {
                let now_open = shared.window.load(Ordering::Acquire);
                if now_open != window_was {
                    window_was = now_open;
                    shared.bump_refresh();
                }
            }
            _ = iface_tick.tick() => {
                if shared.reload_interfaces() {
                    tracing::debug!("discovery: interface list changed");
                    shared.bump_refresh();
                }
            }
            () = stopped(&mut stop) => return,
        }
    }
}

/// Resolves once stop has been requested. Wraps `wait_for` so its borrow guard is not held
/// across an await in the callers' `select!`, which would make the tasks non-`Send`.
pub(crate) async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}

fn read<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(PoisonError::into_inner)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

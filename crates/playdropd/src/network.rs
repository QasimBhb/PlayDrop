//! OS network change notifications, so the host re-reads interfaces, rejoins the beacon
//! group and redials at once after a Wi-Fi switch instead of at the next timer tick
//! (`Host::network_changed`).
//!
//! Linux, best effort: `NetworkManager`'s `StateChanged` signal, or `systemd-networkd`'s
//! `OperationalState` property when `NetworkManager` is absent. Neither present: logged at
//! debug and nothing is watched; UIs can still send `Request::NetworkChanged`. Windows:
//! not wired yet (`NotifyIpInterfaceChange` is Phase 2 follow-up work).

use std::time::Duration;

use tokio::sync::mpsc;

/// Changes closer together than this are reported once.
pub const DEBOUNCE: Duration = Duration::from_secs(1);

/// Watches for network changes and sends one `()` per (debounced) change. Returns once the
/// watch is set up (or found impossible); the watching runs in a spawned task.
pub async fn watch(tx: mpsc::Sender<()>) {
    platform::watch(tx).await;
}

#[cfg(target_os = "linux")]
mod platform {
    use futures_util::{Stream, StreamExt as _};
    use tokio::sync::mpsc;

    #[zbus::proxy(
        interface = "org.freedesktop.NetworkManager",
        default_service = "org.freedesktop.NetworkManager",
        default_path = "/org/freedesktop/NetworkManager",
        gen_blocking = false
    )]
    trait NetworkManager {
        #[zbus(signal)]
        fn state_changed(&self, state: u32) -> zbus::Result<()>;
    }

    #[zbus::proxy(
        interface = "org.freedesktop.network1.Manager",
        default_service = "org.freedesktop.network1",
        default_path = "/org/freedesktop/network1",
        gen_blocking = false
    )]
    trait Networkd {
        #[zbus(property)]
        fn operational_state(&self) -> zbus::Result<String>;
    }

    async fn has_owner(conn: &zbus::Connection, name: &'static str) -> bool {
        let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else {
            return false;
        };
        let Ok(bus_name) = zbus::names::BusName::try_from(name) else {
            return false;
        };
        dbus.name_has_owner(bus_name).await.unwrap_or(false)
    }

    pub(super) async fn watch(tx: mpsc::Sender<()>) {
        let conn = match zbus::Connection::system().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "no system bus; network changes are not watched");
                return;
            }
        };
        if has_owner(&conn, "org.freedesktop.NetworkManager").await {
            match NetworkManagerProxy::new(&conn).await {
                Ok(p) => match p.receive_state_changed().await {
                    Ok(s) => {
                        tracing::debug!("watching NetworkManager for network changes");
                        tokio::spawn(forward(s.map(drop), tx));
                        return;
                    }
                    Err(e) => tracing::debug!(error = %e, "NetworkManager signal not subscribed"),
                },
                Err(e) => tracing::debug!(error = %e, "NetworkManager proxy failed"),
            }
        }
        if has_owner(&conn, "org.freedesktop.network1").await {
            match NetworkdProxy::new(&conn).await {
                Ok(p) => {
                    let s = p.receive_operational_state_changed().await;
                    tracing::debug!("watching systemd-networkd for network changes");
                    tokio::spawn(forward(s.map(drop), tx));
                    return;
                }
                Err(e) => tracing::debug!(error = %e, "systemd-networkd proxy failed"),
            }
        }
        tracing::debug!("neither NetworkManager nor systemd-networkd is on the system bus; network changes are not watched");
    }

    /// Forwards changes, collapsing bursts within [`super::DEBOUNCE`] into one.
    async fn forward(mut s: impl Stream<Item = ()> + Unpin, tx: mpsc::Sender<()>) {
        while s.next().await.is_some() {
            let _ =
                tokio::time::timeout(super::DEBOUNCE, async { while s.next().await.is_some() {} })
                    .await;
            if tx.send(()).await.is_err() {
                return;
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use tokio::sync::mpsc;

    #[allow(clippy::unused_async)]
    pub(super) async fn watch(_tx: mpsc::Sender<()>) {
        tracing::debug!("network change notifications are not wired on this platform yet");
    }
}

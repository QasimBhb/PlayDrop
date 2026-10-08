//! The pin store as the host shares it: a `std::sync::RwLock` the TLS verifier reads
//! synchronously during handshakes (protocol.md 3.1), with async, serialised writers that
//! persist every change (SR-28), and the discovery [`PeerKeys`] view (protocol.md 8.2).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard};

use crate::discovery::{DeviceIdString, PeerKeys};
use crate::identity::DeviceId;
use crate::pin_store::{Pin, PinStore};
use crate::{Error, Result};

/// Shared pin store with a change counter.
#[derive(Debug)]
pub(crate) struct Pins {
    store: Arc<RwLock<PinStore>>,
    writer: tokio::sync::Mutex<()>,
    generation: Arc<AtomicU64>,
}

impl Pins {
    pub(crate) fn new(store: PinStore) -> Self {
        Self {
            store: Arc::new(RwLock::new(store)),
            writer: tokio::sync::Mutex::new(()),
            generation: Arc::new(AtomicU64::new(1)),
        }
    }

    /// The lock the TLS verifier reads (it implements `PinLookup`).
    pub(crate) fn lookup(&self) -> Arc<RwLock<PinStore>> {
        Arc::clone(&self.store)
    }

    /// Discovery's view of the pins (protocol.md 8.2).
    pub(crate) fn peer_keys(&self) -> Arc<dyn PeerKeys> {
        Arc::new(PinKeys {
            store: Arc::clone(&self.store),
            generation: Arc::clone(&self.generation),
        })
    }

    pub(crate) fn read(&self) -> RwLockReadGuard<'_, PinStore> {
        self.store.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// A copy of the pin for `id`.
    pub(crate) fn get(&self, id: &DeviceId) -> Option<Pin> {
        self.read().get(id).cloned()
    }

    /// True if `id` is pinned.
    pub(crate) fn contains(&self, id: &DeviceId) -> bool {
        self.read().get(id).is_some()
    }

    /// Applies `f` and saves the store atomically (SR-28). Writers are serialised; the
    /// verifier keeps reading the old state until the write lock is taken. A failing `f`
    /// saves nothing.
    pub(crate) async fn update<R, F>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut PinStore) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let _w = self.writer.lock().await;
        let store = Arc::clone(&self.store);
        let out = tokio::task::spawn_blocking(move || {
            let mut guard = store.write().unwrap_or_else(PoisonError::into_inner);
            let r = f(&mut guard)?;
            guard.save()?;
            Ok::<R, Error>(r)
        })
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e)))??;
        self.generation.fetch_add(1, Ordering::AcqRel);
        Ok(out)
    }
}

/// [`PeerKeys`] over the pin store: every pin with a received announce key, and the port
/// of its newest last-known address (protocol.md 8.2, 8.3 rule 1).
struct PinKeys {
    store: Arc<RwLock<PinStore>>,
    generation: Arc<AtomicU64>,
}

impl PeerKeys for PinKeys {
    fn announce_keys(&self) -> Vec<(DeviceIdString, [u8; 32])> {
        let store = self.store.read().unwrap_or_else(PoisonError::into_inner);
        store
            .iter()
            .filter_map(|p| {
                p.announce_key
                    .as_ref()
                    .map(|k| (p.device_id.to_hex(), k.key))
            })
            .collect()
    }

    fn stored_port(&self, device_id: &str) -> u16 {
        let Ok(id) = device_id.parse::<DeviceId>() else {
            return crate::constants::TCP_PORT;
        };
        let store = self.store.read().unwrap_or_else(PoisonError::into_inner);
        store
            .get(&id)
            .and_then(|p| p.last_known.first().map(std::net::SocketAddr::port))
            .unwrap_or(crate::constants::TCP_PORT)
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

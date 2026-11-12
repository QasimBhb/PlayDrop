//! The `remote` grant and what peers said in their `reach` frames, per pinned peer, in
//! `reach.json` next to `pins.json` (protocol.md 12, SR-14, SR-28).
//!
//! *Choice:* a file of its own and not fields on the pin. `Pin` refuses unknown fields (and
//! `Grants` did until the notification grants, which older readers see only when changed),
//! so a field in `pins.json` would stop every other build (a release, or this source
//! without the feature) from loading the file, and `playdrop-ffi` builds `Grants` with a struct
//! literal, so a new field there would change a surface that must stay as it is. This way
//! `pins.json` is the same in every build and a data dir can move between builds both ways.
//!
//! An entry is tied to one pairing by the pin's `paired_at`. A build without the feature can
//! unpair and pair a device again without touching this file, and an entry from an earlier
//! pairing must not hand its grant to the new one: it reads as absent, so off.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use iroh::{EndpointId, RelayUrl};
use serde::{Deserialize, Serialize};

use super::wire;
use crate::identity::sealing::{self, Sealer};
use crate::identity::{secure_fs, DeviceId};
use crate::pin_store::Pin;
use crate::{Error, Result};

/// File name inside the data dir.
pub(crate) const REACH_FILE: &str = "reach.json";

/// On-disk format version.
const FORMAT_VERSION: u32 = 1;

/// What is stored per peer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Entry {
    /// `paired_at` of the pin this entry belongs to.
    pub(crate) paired_at: u64,
    /// The `remote` grant (default off, SR-35).
    pub(crate) remote: bool,
    /// We told this peer our endpoint and have not taken it back since (protocol.md 12).
    #[serde(default)]
    pub(crate) told: bool,
    /// The peer's Endpoint ID, 64 lowercase hex characters.
    #[serde(default)]
    pub(crate) endpoint: Option<String>,
    /// The relay the peer named.
    #[serde(default)]
    pub(crate) relay: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    peers: BTreeMap<DeviceId, Entry>,
}

/// The store. Reads are synchronous, as an accept decides under no lock of its own.
#[derive(Debug)]
pub(crate) struct ReachStore {
    path: PathBuf,
    sealer: Sealer,
    peers: BTreeMap<DeviceId, Entry>,
}

impl ReachStore {
    /// Loads the file. Anything wrong with it reads as empty: every grant off, which is the
    /// safe side, and a bad spike file must not keep the LAN product from starting.
    pub(crate) fn open(path: PathBuf, sealer: Sealer) -> Self {
        let peers = match Self::load(&path, &sealer) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "reach settings not readable; remote grants read as off");
                BTreeMap::new()
            }
        };
        Self {
            path,
            sealer,
            peers,
        }
    }

    fn load(path: &std::path::Path, sealer: &Sealer) -> Result<BTreeMap<DeviceId, Entry>> {
        let Some(raw) = secure_fs::read_private(path)? else {
            return Ok(BTreeMap::new());
        };
        let (bytes, _) = sealing::unseal(sealer, raw, REACH_FILE)?;
        let file: FileFormat = serde_json::from_slice(&bytes)?;
        if file.version != FORMAT_VERSION {
            return Err(Error::Identity(format!(
                "unsupported reach settings version {}",
                file.version
            )));
        }
        let mut peers = file.peers;
        // A value that fails the frame rules never came from this code: drop it.
        for e in peers.values_mut() {
            let valid = match (&e.endpoint, &e.relay) {
                (Some(a), Some(b)) => wire::parse_values(Some(a), Some(b)).is_ok(),
                _ => false,
            };
            if !valid {
                e.endpoint = None;
                e.relay = None;
            }
        }
        Ok(peers)
    }

    /// Writes the file atomically, 0600 and sealed like `pins.json` (SR-28).
    pub(crate) fn save(&self) -> Result<()> {
        let file = FileFormat {
            version: FORMAT_VERSION,
            peers: self.peers.clone(),
        };
        let json = serde_json::to_vec_pretty(&file)?;
        let bytes = sealing::seal(&self.sealer, &json, REACH_FILE)?;
        secure_fs::write_private_atomic(&self.path, &bytes)
    }

    /// The entry of `pin`'s pairing, if there is one.
    pub(crate) fn entry(&self, pin: &Pin) -> Option<&Entry> {
        self.peers
            .get(&pin.device_id)
            .filter(|e| e.paired_at == pin.paired_at)
    }

    /// True if `pin` holds the `remote` grant.
    pub(crate) fn granted(&self, pin: &Pin) -> bool {
        self.entry(pin).is_some_and(|e| e.remote)
    }

    fn entry_mut(&mut self, pin: &Pin) -> &mut Entry {
        let e = self.peers.entry(pin.device_id).or_default();
        if e.paired_at != pin.paired_at {
            *e = Entry {
                paired_at: pin.paired_at,
                ..Entry::default()
            };
        }
        e
    }

    /// Turns the grant on or off. Only the local UI may call this (SR-14).
    pub(crate) fn set_remote(&mut self, pin: &Pin, on: bool) {
        self.entry_mut(pin).remote = on;
    }

    /// Records whether this peer holds our endpoint.
    pub(crate) fn set_told(&mut self, pin: &Pin, told: bool) {
        self.entry_mut(pin).told = told;
    }

    /// Stores what the peer said; `None` forgets it. True if that changed anything.
    pub(crate) fn set_peer_values(
        &mut self,
        pin: &Pin,
        values: Option<(&EndpointId, &RelayUrl)>,
    ) -> bool {
        let (endpoint, relay) = match values {
            Some((id, url)) => (Some(wire::endpoint_hex(id)), Some(url.to_string())),
            None => (None, None),
        };
        let e = self.entry_mut(pin);
        if e.endpoint == endpoint && e.relay == relay {
            return false;
        }
        e.endpoint = endpoint;
        e.relay = relay;
        true
    }

    /// True if `pin` already has exactly these values.
    pub(crate) fn has_values(&self, pin: &Pin, id: &EndpointId, url: &RelayUrl) -> bool {
        self.entry(pin).is_some_and(|e| {
            e.endpoint.as_deref() == Some(&*wire::endpoint_hex(id))
                && e.relay.as_deref() == Some(&*url.to_string())
        })
    }

    /// Devices other than `device` with this Endpoint ID stored, with their pairing times. The
    /// caller still checks each against its pin.
    pub(crate) fn others_with_endpoint(
        &self,
        endpoint_hex: &str,
        device: &DeviceId,
    ) -> Vec<(DeviceId, u64)> {
        self.peers
            .iter()
            .filter(|(id, e)| *id != device && e.endpoint.as_deref() == Some(endpoint_hex))
            .map(|(id, e)| (*id, e.paired_at))
            .collect()
    }

    /// Forgets a peer (unpair, SR-14).
    pub(crate) fn remove(&mut self, id: &DeviceId) -> bool {
        self.peers.remove(id).is_some()
    }

    /// Entries with the grant on and this Endpoint ID stored, by device. The caller still
    /// checks each against its pin.
    pub(crate) fn granted_with_endpoint(&self, endpoint_hex: &str) -> Vec<(DeviceId, u64)> {
        self.peers
            .iter()
            .filter(|(_, e)| e.remote && e.endpoint.as_deref() == Some(endpoint_hex))
            .map(|(id, e)| (*id, e.paired_at))
            .collect()
    }
}

/// [`ReachStore`] shared the way the host shares its pins: synchronous reads, serialised
/// writers that persist every change.
#[derive(Debug)]
pub(crate) struct SharedStore {
    store: Arc<RwLock<ReachStore>>,
    writer: tokio::sync::Mutex<()>,
}

impl SharedStore {
    pub(crate) fn new(store: ReachStore) -> Self {
        Self {
            store: Arc::new(RwLock::new(store)),
            writer: tokio::sync::Mutex::new(()),
        }
    }

    /// Reads under the lock.
    pub(crate) fn read<R>(&self, f: impl FnOnce(&ReachStore) -> R) -> R {
        f(&self.store.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Applies `f` in memory only, to take back an [`update`](Self::update) whose save failed.
    pub(crate) async fn apply_unsaved<F>(&self, f: F)
    where
        F: FnOnce(&mut ReachStore),
    {
        let _w = self.writer.lock().await;
        f(&mut self.store.write().unwrap_or_else(PoisonError::into_inner));
    }

    /// Applies `f` and saves atomically. `f` returning false means nothing changed, and
    /// nothing is written. A failed save leaves the change in memory: a revoke holds at once.
    pub(crate) async fn update<F>(&self, f: F) -> Result<bool>
    where
        F: FnOnce(&mut ReachStore) -> bool + Send + 'static,
    {
        let _w = self.writer.lock().await;
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            let mut guard = store.write().unwrap_or_else(PoisonError::into_inner);
            let changed = f(&mut guard);
            if changed {
                guard.save()?;
            }
            Ok::<bool, Error>(changed)
        })
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e)))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceIdentity;
    use crate::pin_store::{PinStore, PIN_FILE};

    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::Builder::new()
            .prefix("playdrop-reach-")
            .tempdir_in(std::env::temp_dir())
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        dir
    }

    fn pin() -> Pin {
        let identity = DeviceIdentity::generate().unwrap();
        Pin::new(identity.certificate_der().to_vec(), "peer").unwrap()
    }

    fn sample_values() -> (EndpointId, RelayUrl) {
        (
            iroh::SecretKey::from_bytes(&[5; 32]).public(),
            "https://relay.example.org".parse().unwrap(),
        )
    }

    #[test]
    fn default_is_off_and_values_survive_a_reload() {
        let dir = private_dir();
        let path = dir.path().join(REACH_FILE);
        let p = pin();
        let mut s = ReachStore::open(path.clone(), None);
        assert!(!s.granted(&p), "default off (SR-35)");
        s.set_remote(&p, true);
        let (id, url) = sample_values();
        assert!(s.set_peer_values(&p, Some((&id, &url))));
        assert!(!s.set_peer_values(&p, Some((&id, &url))), "no change");
        s.save().unwrap();
        let s = ReachStore::open(path, None);
        assert!(s.granted(&p));
        let e = s.entry(&p).unwrap();
        assert_eq!(e.endpoint.as_deref(), Some(&*wire::endpoint_hex(&id)));
        assert_eq!(e.relay.as_deref(), Some(&*url.to_string()));
    }

    /// A grant from an earlier pairing does not carry over to a new one, even when a build
    /// without the feature did the unpair and left this file alone.
    #[test]
    fn an_entry_from_an_earlier_pairing_reads_as_absent() {
        let dir = private_dir();
        let mut s = ReachStore::open(dir.path().join(REACH_FILE), None);
        let mut p = pin();
        s.set_remote(&p, true);
        assert!(s.granted(&p));
        p.paired_at += 1;
        assert!(!s.granted(&p));
        assert!(s.entry(&p).is_none());
        let (id, url) = sample_values();
        assert!(s.set_peer_values(&p, Some((&id, &url))));
        assert!(!s.granted(&p), "new values do not bring the old grant back");
    }

    #[test]
    fn a_damaged_file_reads_as_all_off() {
        let dir = private_dir();
        let path = dir.path().join(REACH_FILE);
        secure_fs::write_private_atomic(&path, b"{ not json").unwrap();
        let s = ReachStore::open(path, None);
        assert!(!s.granted(&pin()));
    }

    #[test]
    fn a_stored_value_that_fails_the_rules_is_dropped_on_load() {
        let dir = private_dir();
        let path = dir.path().join(REACH_FILE);
        let p = pin();
        let mut s = ReachStore::open(path.clone(), None);
        s.set_remote(&p, true);
        s.peers.get_mut(&p.device_id).unwrap().endpoint = Some("zz".into());
        s.peers.get_mut(&p.device_id).unwrap().relay = Some("http://8.8.8.8".into());
        s.save().unwrap();
        let s = ReachStore::open(path, None);
        let e = s.entry(&p).unwrap();
        assert!(e.remote);
        assert!(e.endpoint.is_none() && e.relay.is_none());
    }

    /// The compatibility promise: nothing of this reaches `pins.json`, so a release build, or
    /// this source without the feature, loads a data dir this build used, and the other way.
    #[test]
    fn pins_json_stays_exactly_what_every_build_reads() {
        let dir = private_dir();
        let mut pins = PinStore::open(dir.path().join(PIN_FILE)).unwrap();
        let p = pin();
        pins.insert(p.clone()).unwrap();
        pins.save().unwrap();
        let before = std::fs::read(dir.path().join(PIN_FILE)).unwrap();

        let mut s = ReachStore::open(dir.path().join(REACH_FILE), None);
        s.set_remote(&p, true);
        let (id, url) = sample_values();
        s.set_peer_values(&p, Some((&id, &url)));
        s.save().unwrap();
        pins.save().unwrap();

        let after = std::fs::read(dir.path().join(PIN_FILE)).unwrap();
        assert_eq!(before, after);
        let text = String::from_utf8(after).unwrap();
        for word in ["remote", "endpoint", "relay", "reach"] {
            assert!(!text.contains(word), "{word} in pins.json");
        }
        // And a pins file from before this code still loads next to a reach file.
        let again = PinStore::open(dir.path().join(PIN_FILE)).unwrap();
        assert_eq!(again.get(&p.device_id), Some(&p));
    }

    #[test]
    fn granted_with_endpoint_lists_only_grant_holders() {
        let dir = private_dir();
        let mut s = ReachStore::open(dir.path().join(REACH_FILE), None);
        let (a, b) = (pin(), pin());
        let (id, url) = sample_values();
        s.set_remote(&a, true);
        s.set_peer_values(&a, Some((&id, &url)));
        s.set_peer_values(&b, Some((&id, &url)));
        let hex = wire::endpoint_hex(&id);
        assert_eq!(
            s.granted_with_endpoint(&hex),
            vec![(a.device_id, a.paired_at)]
        );
        s.set_remote(&a, false);
        assert_eq!(s.granted_with_endpoint(&hex), Vec::new());
    }
}

//! Pinned peers, per-device grants and announce keys at rest.
//! Implements `docs/protocol.md` 6.4 and 8.2, SR-13, SR-14, SR-28.
//!
//! The store is a plain in-memory value backed by one JSON file. It is not thread-safe on
//! its own: the host holds it behind a lock. Mutations change memory only; call
//! [`PinStore::save`] to persist them. The TLS verifier needs synchronous lookups, so hosts
//! that share the store with a verifier keep it behind a `std::sync::RwLock` (which
//! implements [`PinLookup`](crate::tls::PinLookup) here) or hand the verifier a snapshot.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::identity::{fill_random, inspect_certificate, secure_fs, DeviceId};
use crate::tls::PinLookup;
use crate::{Error, Result};

/// Default pin store file name inside the identity directory (SR-28).
pub const PIN_FILE: &str = "pins.json";

/// At most this many last-known addresses are kept per peer, newest first (protocol.md 3.2).
pub const MAX_LAST_KNOWN: usize = 8;

/// On-disk format version of the pin store file.
const FORMAT_VERSION: u32 = 1;

/// Per-device grants (protocol.md 6.4, SR-14). They change only from the local UI.
#[allow(clippy::struct_excessive_bools)] // One flag per grant is the protocol's model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Grants {
    /// Auto-accept file offers.
    pub files: bool,
    /// Accept clipboard pushes from the peer.
    pub clipboard_in: bool,
    /// Send local clipboard changes to the peer.
    pub clipboard_out: bool,
    /// Remote input from the peer.
    pub input: bool,
    /// Media state and control.
    pub media: bool,
    /// "Find my device" ring.
    pub find: bool,
}

impl Default for Grants {
    /// Defaults at pairing: all on except `input` (protocol.md 6.4, SR-14).
    fn default() -> Self {
        Self {
            files: true,
            clipboard_in: true,
            clipboard_out: true,
            input: false,
            media: true,
            find: true,
        }
    }
}

/// Serde helper: 32 bytes as unpadded base64url, the encoding of `announce_key.key`
/// (protocol.md 8.2).
mod b64_key {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(key: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&data_encoding::BASE64URL_NOPAD.encode(key))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let v = data_encoding::BASE64URL_NOPAD
            .decode(s.as_bytes())
            .map_err(serde::de::Error::custom)?;
        v.try_into()
            .map_err(|_| serde::de::Error::custom("announce key must be 32 bytes"))
    }
}

/// Serde helper: certificate DER as unpadded base64url.
mod b64_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&data_encoding::BASE64URL_NOPAD.encode(v))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        data_encoding::BASE64URL_NOPAD
            .decode(s.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

/// Serde helper: 32 bytes as lowercase hex.
mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&data_encoding::HEXLOWER.encode(v))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let v = data_encoding::HEXLOWER
            .decode(s.as_bytes())
            .map_err(serde::de::Error::custom)?;
        v.try_into()
            .map_err(|_| serde::de::Error::custom("SPKI hash must be 32 bytes"))
    }
}

/// A peer's announce key K and its rotation counter (protocol.md 8.2, SR-5).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnounceKey {
    /// Rotation counter; rises by 1 per rotation.
    pub kid: u32,
    /// The 256-bit key K.
    #[serde(with = "b64_key")]
    pub key: [u8; 32],
}

impl fmt::Debug for AnnounceKey {
    /// Never prints the key (SR-32).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnnounceKey")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

/// This device's own announce key (protocol.md 8.2, SR-5), stored like the private key
/// (SR-28).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalAnnounceKey {
    /// Rotation counter; rises by 1 per rotation.
    pub kid: u32,
    /// The 256-bit key K.
    #[serde(with = "b64_key")]
    pub key: [u8; 32],
}

impl fmt::Debug for LocalAnnounceKey {
    /// Never prints the key (SR-32).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalAnnounceKey")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl LocalAnnounceKey {
    /// Creates a fresh key with `kid` 1.
    pub fn generate() -> Result<Self> {
        let mut key = [0u8; 32];
        fill_random(&mut key)?;
        Ok(Self { kid: 1, key })
    }

    /// Replaces K with fresh random bytes and raises `kid` by 1 (protocol.md 8.2). Called
    /// on every unpair; the new key is then pushed to every connected paired peer.
    pub fn rotate(&mut self) -> Result<()> {
        let mut key = [0u8; 32];
        fill_random(&mut key)?;
        self.kid = self
            .kid
            .checked_add(1)
            .ok_or_else(|| Error::Identity("announce key id exhausted".into()))?;
        self.key = key;
        Ok(())
    }

    /// The form sent to peers in the `announce_key` frame.
    pub fn as_announce_key(&self) -> AnnounceKey {
        AnnounceKey {
            kid: self.kid,
            key: self.key,
        }
    }
}

/// One paired peer (protocol.md 6.4, 8.2, SR-13, SR-14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    /// Device ID, the SHA-256 of the peer's SPKI.
    pub device_id: DeviceId,
    /// Raw SPKI hash; equal to the bytes of `device_id`, kept for direct lookup.
    #[serde(with = "hex32")]
    pub spki_hash: [u8; 32],
    /// The peer certificate seen at pairing, DER.
    #[serde(with = "b64_bytes")]
    pub cert_der: Vec<u8>,
    /// Local, user-editable name. Never sent to the peer.
    pub label: String,
    /// What this peer may do.
    pub grants: Grants,
    /// The peer's announce key, once received over a paired session.
    pub announce_key: Option<AnnounceKey>,
    /// Last addresses this peer was reached at, newest first, at most [`MAX_LAST_KNOWN`].
    pub last_known: Vec<SocketAddr>,
    /// When pairing completed, Unix seconds.
    pub paired_at: u64,
    /// When this peer last sent a session hello or frame, Unix seconds (SR-8: the device
    /// list shows it). `None` until the first session. Absent in files written before
    /// Phase 2, hence the default.
    #[serde(default)]
    pub last_seen: Option<u64>,
}

impl Pin {
    /// Builds a pin for a freshly paired peer from its DER certificate, with default
    /// grants, no announce key yet and `paired_at` set to now.
    pub fn new(cert_der: Vec<u8>, label: impl Into<String>) -> Result<Self> {
        let info = inspect_certificate(&cert_der)?;
        let paired_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        Ok(Self {
            device_id: info.device_id,
            spki_hash: info.spki_hash,
            cert_der,
            label: label.into(),
            grants: Grants::default(),
            announce_key: None,
            last_known: Vec::new(),
            paired_at,
            last_seen: None,
        })
    }

    /// Checks that the stored IDs match the stored certificate and limits hold.
    fn validate(&self) -> Result<()> {
        let info = inspect_certificate(&self.cert_der)?;
        if info.device_id != self.device_id || info.spki_hash != self.spki_hash {
            return Err(Error::Identity(format!(
                "pin {} does not match its certificate",
                self.device_id
            )));
        }
        if self.last_known.len() > MAX_LAST_KNOWN {
            return Err(Error::Identity(format!(
                "pin {} has more than {MAX_LAST_KNOWN} last-known addresses",
                self.device_id
            )));
        }
        Ok(())
    }
}

/// Serialised file layout.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileFormat {
    version: u32,
    local_announce_key: LocalAnnounceKey,
    pins: Vec<Pin>,
}

/// All pins plus the local announce key, backed by one 0600 JSON file (SR-28).
pub struct PinStore {
    path: PathBuf,
    local_announce_key: LocalAnnounceKey,
    pins: BTreeMap<DeviceId, Pin>,
}

impl fmt::Debug for PinStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinStore")
            .field("path", &self.path)
            .field("local_announce_key", &self.local_announce_key)
            .field("pins", &self.pins.len())
            .finish()
    }
}

impl PinStore {
    /// Loads the store at `path`, or creates an empty one with a fresh local announce key
    /// and saves it at once. The parent directory must be private (0700) and the file
    /// 0600 on Unix; looser modes are refused (SR-28). Every pin is checked against its
    /// certificate, and a file that fails is refused rather than partly loaded.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            secure_fs::ensure_private_dir(dir)?;
        }
        if let Some(bytes) = secure_fs::read_private(&path)? {
            return Self::from_json(path, &bytes);
        }
        let store = Self {
            path,
            local_announce_key: LocalAnnounceKey::generate()?,
            pins: BTreeMap::new(),
        };
        store.save()?;
        Ok(store)
    }

    fn from_json(path: PathBuf, bytes: &[u8]) -> Result<Self> {
        let file: FileFormat = serde_json::from_slice(bytes)?;
        if file.version != FORMAT_VERSION {
            return Err(Error::Identity(format!(
                "unsupported pin store version {} in {}",
                file.version,
                path.display()
            )));
        }
        let mut pins = BTreeMap::new();
        for pin in file.pins {
            pin.validate()?;
            if pins.insert(pin.device_id, pin).is_some() {
                return Err(Error::Identity(format!(
                    "duplicate pin in {}",
                    path.display()
                )));
            }
        }
        Ok(Self {
            path,
            local_announce_key: file.local_announce_key,
            pins,
        })
    }

    /// Writes the store atomically: a new 0600 temporary file, flushed, then renamed over
    /// the old one (SR-28).
    pub fn save(&self) -> Result<()> {
        let file = FileFormat {
            version: FORMAT_VERSION,
            local_announce_key: self.local_announce_key.clone(),
            pins: self.pins.values().cloned().collect(),
        };
        let json = serde_json::to_vec_pretty(&file)?;
        secure_fs::write_private_atomic(&self.path, &json)
    }

    /// The file this store saves to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The pin for `id`, if paired.
    pub fn get(&self, id: &DeviceId) -> Option<&Pin> {
        self.pins.get(id)
    }

    /// The pin whose SPKI hash is `spki_hash`, if paired. Device IDs are SPKI hashes, so
    /// this is a direct lookup.
    pub fn get_by_spki(&self, spki_hash: &[u8; 32]) -> Option<&Pin> {
        self.pins.get(&DeviceId::from_spki_hash(*spki_hash))
    }

    /// Adds a pin, replacing any earlier pin for the same Device ID (re-pairing the same
    /// key). Returns the replaced pin. A different key is a different Device ID and so a
    /// separate pin (SR-10).
    pub fn insert(&mut self, pin: Pin) -> Result<Option<Pin>> {
        pin.validate()?;
        Ok(self.pins.insert(pin.device_id, pin))
    }

    /// Unpairs `id` (protocol.md 6.4): deletes its pin, grants and announce key, and
    /// rotates the local announce key (protocol.md 8.2) in the same step so the rotation
    /// cannot be forgotten. Returns the removed pin so the caller can close its
    /// connections within 1 s, drop its tokens and transfers, and push the new local key
    /// to every remaining connected peer. Returns `Ok(None)` and rotates nothing if `id`
    /// was not paired.
    pub fn remove(&mut self, id: &DeviceId) -> Result<Option<Pin>> {
        let Some(pin) = self.pins.remove(id) else {
            return Ok(None);
        };
        if let Err(e) = self.local_announce_key.rotate() {
            self.pins.insert(pin.device_id, pin);
            return Err(e);
        }
        Ok(Some(pin))
    }

    fn get_mut(&mut self, id: &DeviceId) -> Result<&mut Pin> {
        self.pins.get_mut(id).ok_or(Error::NotFound)
    }

    /// Replaces the grants of `id`. Only the local UI may call this (SR-14).
    pub fn set_grants(&mut self, id: &DeviceId, grants: Grants) -> Result<()> {
        self.get_mut(id)?.grants = grants;
        Ok(())
    }

    /// Renames `id` locally.
    pub fn set_label(&mut self, id: &DeviceId, label: impl Into<String>) -> Result<()> {
        self.get_mut(id)?.label = label.into();
        Ok(())
    }

    /// Records that `id` was reached at `addr`: moves or inserts it at the front and keeps
    /// at most [`MAX_LAST_KNOWN`] entries (protocol.md 3.2).
    pub fn push_last_known(&mut self, id: &DeviceId, addr: SocketAddr) -> Result<()> {
        let list = &mut self.get_mut(id)?.last_known;
        list.retain(|a| *a != addr);
        list.insert(0, addr);
        list.truncate(MAX_LAST_KNOWN);
        Ok(())
    }

    /// Records that `id` was last heard from at `at` (Unix seconds). Never moves the
    /// time backwards, so a late write from an older session cannot hide a newer one.
    pub fn set_last_seen(&mut self, id: &DeviceId, at: u64) -> Result<()> {
        let pin = self.get_mut(id)?;
        if pin.last_seen.is_none_or(|t| t < at) {
            pin.last_seen = Some(at);
        }
        Ok(())
    }

    /// Stores a peer's announce key if its `kid` is higher than the one held; the receiver
    /// keeps the highest `kid` only (protocol.md 8.2). Returns whether it was stored.
    pub fn set_announce_key(&mut self, id: &DeviceId, key: AnnounceKey) -> Result<bool> {
        let pin = self.get_mut(id)?;
        if pin.announce_key.as_ref().is_some_and(|k| k.kid >= key.kid) {
            return Ok(false);
        }
        pin.announce_key = Some(key);
        Ok(true)
    }

    /// This device's announce key (protocol.md 8.2).
    pub fn local_announce_key(&self) -> &LocalAnnounceKey {
        &self.local_announce_key
    }

    /// Rotates this device's announce key outside an unpair, for example on user request.
    pub fn rotate_local_announce_key(&mut self) -> Result<&LocalAnnounceKey> {
        self.local_announce_key.rotate()?;
        Ok(&self.local_announce_key)
    }

    /// All pins, ordered by Device ID.
    pub fn iter(&self) -> impl Iterator<Item = &Pin> {
        self.pins.values()
    }

    /// Number of pins.
    pub fn len(&self) -> usize {
        self.pins.len()
    }

    /// Whether no device is paired.
    pub fn is_empty(&self) -> bool {
        self.pins.is_empty()
    }
}

impl<'a> IntoIterator for &'a PinStore {
    type Item = &'a Pin;
    type IntoIter = std::collections::btree_map::Values<'a, DeviceId, Pin>;

    fn into_iter(self) -> Self::IntoIter {
        self.pins.values()
    }
}

impl PinLookup for PinStore {
    fn spki_is_pinned(&self, spki_hash: &[u8; 32]) -> Option<DeviceId> {
        self.get_by_spki(spki_hash).map(|p| p.device_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceIdentity;

    fn new_pin(label: &str) -> Pin {
        let id = DeviceIdentity::generate().unwrap();
        Pin::new(id.certificate_der().to_vec(), label).unwrap()
    }

    #[test]
    fn grants_defaults() {
        let g = Grants::default();
        assert!(g.files && g.clipboard_in && g.clipboard_out && g.media && g.find);
        assert!(!g.input);
        assert_eq!(new_pin("x").grants, g);
        let json = serde_json::to_value(g).unwrap();
        assert_eq!(json["clipboardIn"], true);
        assert_eq!(json["input"], false);
    }

    #[test]
    fn save_and_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state").join(PIN_FILE);
        let mut store = PinStore::open(&path).unwrap();
        assert!(store.is_empty());
        let local = store.local_announce_key().clone();

        let pin = new_pin("Phone");
        let id = pin.device_id;
        assert!(store.insert(pin.clone()).unwrap().is_none());
        store.set_label(&id, "My phone").unwrap();
        let g = Grants {
            input: true,
            ..Grants::default()
        };
        store.set_grants(&id, g).unwrap();
        store
            .set_announce_key(
                &id,
                AnnounceKey {
                    kid: 3,
                    key: [7; 32],
                },
            )
            .unwrap();
        store
            .push_last_known(&id, "192.168.1.20:42187".parse().unwrap())
            .unwrap();
        store.save().unwrap();

        let again = PinStore::open(&path).unwrap();
        assert_eq!(again.local_announce_key(), &local);
        let got = again.get(&id).unwrap();
        assert_eq!(got.label, "My phone");
        assert!(got.grants.input);
        assert_eq!(got.announce_key.as_ref().unwrap().kid, 3);
        assert_eq!(got.last_known.len(), 1);
        assert_eq!(got.cert_der, pin.cert_der);
        assert_eq!(again.spki_is_pinned(&pin.spki_hash), Some(id));
        assert_eq!(again.len(), 1);

        // No temporary files are left behind.
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(PIN_FILE)]);
    }

    #[test]
    fn last_seen_persists_and_never_goes_back() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s").join(PIN_FILE);
        let mut store = PinStore::open(&path).unwrap();
        let pin = new_pin("a");
        let id = pin.device_id;
        assert_eq!(pin.last_seen, None);
        store.insert(pin).unwrap();
        store.set_last_seen(&id, 2_000).unwrap();
        store.set_last_seen(&id, 1_000).unwrap();
        assert_eq!(store.get(&id).unwrap().last_seen, Some(2_000));
        store.save().unwrap();
        assert_eq!(
            PinStore::open(&path).unwrap().get(&id).unwrap().last_seen,
            Some(2_000)
        );
        let unknown = *DeviceIdentity::generate().unwrap().device_id();
        assert!(store.set_last_seen(&unknown, 1).is_err());
    }

    #[test]
    fn pin_without_last_seen_still_loads() {
        let mut v = serde_json::to_value(new_pin("old")).unwrap();
        v.as_object_mut().unwrap().remove("last_seen");
        let pin: Pin = serde_json::from_value(v).unwrap();
        assert_eq!(pin.last_seen, None);
    }

    #[test]
    fn save_replaces_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s").join(PIN_FILE);
        let mut store = PinStore::open(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        store.insert(new_pin("a")).unwrap();
        store.save().unwrap();
        let after = std::fs::read(&path).unwrap();
        assert_ne!(before, after);
        // The written file always parses in full.
        let parsed: serde_json::Value = serde_json::from_slice(&after).unwrap();
        assert_eq!(parsed["pins"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn remove_rotates_local_key() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = PinStore::open(tmp.path().join("s").join(PIN_FILE)).unwrap();
        let pin = new_pin("a");
        let id = pin.device_id;
        store.insert(pin).unwrap();
        let before = store.local_announce_key().clone();
        let removed = store.remove(&id).unwrap().unwrap();
        assert_eq!(removed.device_id, id);
        assert!(store.get(&id).is_none());
        let after = store.local_announce_key();
        assert_eq!(after.kid, before.kid + 1);
        assert_ne!(after.key, before.key);
        // Removing an unknown ID rotates nothing.
        assert!(store.remove(&id).unwrap().is_none());
        assert_eq!(store.local_announce_key().kid, before.kid + 1);
    }

    #[test]
    fn announce_key_keeps_highest_kid() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = PinStore::open(tmp.path().join("s").join(PIN_FILE)).unwrap();
        let pin = new_pin("a");
        let id = pin.device_id;
        store.insert(pin).unwrap();
        assert!(store
            .set_announce_key(
                &id,
                AnnounceKey {
                    kid: 5,
                    key: [1; 32]
                }
            )
            .unwrap());
        assert!(!store
            .set_announce_key(
                &id,
                AnnounceKey {
                    kid: 4,
                    key: [2; 32]
                }
            )
            .unwrap());
        assert!(!store
            .set_announce_key(
                &id,
                AnnounceKey {
                    kid: 5,
                    key: [3; 32]
                }
            )
            .unwrap());
        assert_eq!(
            store.get(&id).unwrap().announce_key.as_ref().unwrap().key,
            [1; 32]
        );
    }

    #[test]
    fn last_known_is_bounded_and_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = PinStore::open(tmp.path().join("s").join(PIN_FILE)).unwrap();
        let pin = new_pin("a");
        let id = pin.device_id;
        store.insert(pin).unwrap();
        for i in 0..12u16 {
            let addr: SocketAddr = format!("10.0.0.{i}:42187").parse().unwrap();
            store.push_last_known(&id, addr).unwrap();
        }
        let again: SocketAddr = "10.0.0.5:42187".parse().unwrap();
        store.push_last_known(&id, again).unwrap();
        let list = &store.get(&id).unwrap().last_known;
        assert_eq!(list.len(), MAX_LAST_KNOWN);
        assert_eq!(list[0], again);
        assert_eq!(list[1], "10.0.0.11:42187".parse::<SocketAddr>().unwrap());
        assert_eq!(list.iter().filter(|a| **a == again).count(), 1);
    }

    #[test]
    fn unknown_device_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = PinStore::open(tmp.path().join("s").join(PIN_FILE)).unwrap();
        let id = new_pin("a").device_id;
        assert!(matches!(store.set_label(&id, "x"), Err(Error::NotFound)));
        assert!(matches!(
            store.set_grants(&id, Grants::default()),
            Err(Error::NotFound)
        ));
    }

    #[test]
    fn tampered_pin_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s").join(PIN_FILE);
        let mut store = PinStore::open(&path).unwrap();
        let mut pin = new_pin("a");
        let other = new_pin("b");
        pin.cert_der.clone_from(&other.cert_der);
        assert!(store.insert(pin.clone()).is_err());

        // Same tampering written straight to disk.
        let good = new_pin("c");
        store.insert(good).unwrap();
        store.save().unwrap();
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["pins"][0]["cert_der"] =
            serde_json::Value::String(data_encoding::BASE64URL_NOPAD.encode(&other.cert_der));
        secure_fs::write_private_atomic(&path, &serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(PinStore::open(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn file_is_private_and_loose_mode_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let path = dir.join(PIN_FILE);
        PinStore::open(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = PinStore::open(&path).unwrap_err();
        assert!(matches!(err, Error::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied));
    }

    #[test]
    fn debug_never_prints_keys() {
        let k = LocalAnnounceKey {
            kid: 9,
            key: [0xab; 32],
        };
        let s = format!("{k:?}");
        assert!(!s.contains("171") && !s.to_lowercase().contains("ab, "));
        assert!(s.contains("kid: 9"));
    }
}

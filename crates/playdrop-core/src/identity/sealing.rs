//! Secrets at rest (`docs/core-api.md` section 2a, SR-28): a host may seal `identity.key`
//! and `pins.json` with a platform key (an Android Keystore AES-256-GCM key). A sealed file
//! is `PDSEALED1` followed by the store's output. A file without the header is plain and is
//! rewritten sealed when a store is configured (migration); a sealed file on a host without
//! a store cannot be read and fails start with [`Error::Identity`].
//!
//! The store is foreign code on mobile. Every caller here already runs on a blocking
//! thread (the host loads the identity and pin store and saves pins in `spawn_blocking`).

use std::fmt;
use std::sync::Arc;

use crate::{Error, Result};

/// Header of a sealed file.
pub const SEALED_MAGIC: &[u8] = b"PDSEALED1";

/// Seals and opens secret files. Implemented by the host.
pub trait SecretStore: Send + Sync + fmt::Debug {
    /// Encrypts bytes before they are written to the data dir.
    fn seal(&self, plain: Vec<u8>) -> std::result::Result<Vec<u8>, SecretStoreError>;
    /// Decrypts bytes read from the data dir.
    fn open(&self, sealed: Vec<u8>) -> std::result::Result<Vec<u8>, SecretStoreError>;
}

/// A store failure; start or the save fails with this text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SecretStoreError {
    /// What went wrong (no key material).
    pub message: String,
}

impl SecretStoreError {
    /// An error with this message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// The optional store, shared.
pub type Sealer = Option<Arc<dyn SecretStore>>;

/// Reads a file's bytes: opens a sealed one through `store`, passes a plain one through.
/// Returns the plain bytes and whether the file was plain (so the caller migrates it).
pub(crate) fn unseal(store: &Sealer, bytes: Vec<u8>, what: &str) -> Result<(Vec<u8>, bool)> {
    let Some(rest) = bytes.strip_prefix(SEALED_MAGIC) else {
        return Ok((bytes, true));
    };
    let Some(store) = store else {
        return Err(Error::Identity(format!(
            "{what} is sealed but this host has no secret store"
        )));
    };
    store
        .open(rest.to_vec())
        .map(|plain| (plain, false))
        .map_err(|e| Error::Identity(format!("{what} could not be unsealed: {e}")))
}

/// The bytes to write for `plain`: sealed with the header when a store is configured.
pub(crate) fn seal(store: &Sealer, plain: &[u8], what: &str) -> Result<Vec<u8>> {
    let Some(store) = store else {
        return Ok(plain.to_vec());
    };
    let sealed = store
        .seal(plain.to_vec())
        .map_err(|e| Error::Identity(format!("{what} could not be sealed: {e}")))?;
    let mut out = Vec::with_capacity(SEALED_MAGIC.len() + sealed.len());
    out.extend_from_slice(SEALED_MAGIC);
    out.extend_from_slice(&sealed);
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// XORs every byte with a fixed key: enough to prove nothing is written in clear.
    #[derive(Debug)]
    pub(crate) struct XorStore(pub u8);

    impl SecretStore for XorStore {
        fn seal(&self, plain: Vec<u8>) -> std::result::Result<Vec<u8>, SecretStoreError> {
            Ok(plain.into_iter().map(|b| b ^ self.0).collect())
        }
        fn open(&self, sealed: Vec<u8>) -> std::result::Result<Vec<u8>, SecretStoreError> {
            Ok(sealed.into_iter().map(|b| b ^ self.0).collect())
        }
    }

    #[test]
    fn seal_round_trip_and_plain_passthrough() {
        let store: Sealer = Some(Arc::new(XorStore(0x5a)));
        let sealed = seal(&store, b"{\"a\":1}", "x").unwrap();
        assert!(sealed.starts_with(SEALED_MAGIC));
        assert!(!sealed.windows(3).any(|w| w == b"\"a\""));
        assert_eq!(
            unseal(&store, sealed.clone(), "x").unwrap(),
            (b"{\"a\":1}".to_vec(), false)
        );
        assert_eq!(
            unseal(&store, b"plain".to_vec(), "x").unwrap(),
            (b"plain".to_vec(), true)
        );
        assert!(matches!(
            unseal(&None, sealed, "x"),
            Err(Error::Identity(_))
        ));
        assert_eq!(seal(&None, b"p", "x").unwrap(), b"p");
    }

    fn private_dir() -> tempfile::TempDir {
        let t = tempfile::Builder::new()
            .prefix("playdrop-seal-")
            .tempdir_in(std::env::temp_dir())
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(t.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        t
    }

    /// With a store, identity and pins survive a restart sealed; nothing readable is left
    /// in the files; a host without the store cannot start from them.
    #[test]
    fn identity_and_pins_round_trip_sealed() {
        use crate::identity::DeviceIdentity;
        use crate::pin_store::{Pin, PinStore, PIN_FILE};
        let tmp = private_dir();
        let dir = tmp.path().join("data");
        let store: Sealer = Some(Arc::new(XorStore(0xa5)));
        let id = DeviceIdentity::load_or_create_sealed(&dir, &store).unwrap();
        let peer = DeviceIdentity::generate().unwrap();
        let mut pins = PinStore::open_sealed(dir.join(PIN_FILE), store.clone()).unwrap();
        pins.insert(Pin::new(peer.certificate_der().to_vec(), "Phone").unwrap())
            .unwrap();
        pins.save().unwrap();
        for f in ["identity.key", PIN_FILE] {
            let raw = std::fs::read(dir.join(f)).unwrap();
            assert!(raw.starts_with(SEALED_MAGIC), "{f} is sealed");
        }
        let raw = std::fs::read(dir.join(PIN_FILE)).unwrap();
        assert!(
            !raw.windows(5).any(|w| w == b"Phone"),
            "no plain text at rest"
        );

        let again = DeviceIdentity::load_or_create_sealed(&dir, &store).unwrap();
        assert_eq!(again.device_id(), id.device_id());
        let pins = PinStore::open_sealed(dir.join(PIN_FILE), store.clone()).unwrap();
        assert_eq!(pins.get(peer.device_id()).unwrap().label, "Phone");

        assert!(matches!(
            DeviceIdentity::load_or_create(&dir),
            Err(Error::Identity(_))
        ));
        assert!(matches!(
            PinStore::open(dir.join(PIN_FILE)),
            Err(Error::Identity(_))
        ));
    }

    /// Plain files from before the store existed are read once and rewritten sealed.
    #[test]
    fn plain_files_migrate_to_sealed() {
        use crate::identity::DeviceIdentity;
        use crate::pin_store::{PinStore, PIN_FILE};
        let tmp = private_dir();
        let dir = tmp.path().join("data");
        let id = DeviceIdentity::load_or_create(&dir).unwrap();
        drop(PinStore::open(dir.join(PIN_FILE)).unwrap());
        assert!(!std::fs::read(dir.join("identity.key"))
            .unwrap()
            .starts_with(SEALED_MAGIC));
        let store: Sealer = Some(Arc::new(XorStore(0x33)));
        let again = DeviceIdentity::load_or_create_sealed(&dir, &store).unwrap();
        assert_eq!(again.device_id(), id.device_id());
        drop(PinStore::open_sealed(dir.join(PIN_FILE), store.clone()).unwrap());
        for f in ["identity.key", PIN_FILE] {
            assert!(
                std::fs::read(dir.join(f))
                    .unwrap()
                    .starts_with(SEALED_MAGIC),
                "{f}"
            );
        }
        let third = DeviceIdentity::load_or_create_sealed(&dir, &store).unwrap();
        assert_eq!(third.device_id(), id.device_id());
    }
}

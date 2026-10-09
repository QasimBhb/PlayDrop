//! Traits the foreign host implements, and the adapters that present them to the core as
//! its own traits. All are synchronous (docs/core-api.md section 7). File handles cross as
//! raw file descriptors: the side that receives one owns it.

use std::fmt;
use std::os::fd::{FromRawFd, IntoRawFd};
use std::sync::Arc;

use playdrop_core::discovery::{InterfaceSource, Ipv4Interface as CoreIpv4Interface};
use playdrop_core::host;

use crate::error::PlayDropError;
use crate::events::HostEvent;
use crate::types::Ipv4Interface;

/// Receives every event the host emits, in order, on one dedicated thread. Keep the
/// implementation quick: a slow listener delays later events (the core drops events when
/// its queue of 4,096 fills).
#[uniffi::export(foreign)]
pub trait HostListener: Send + Sync {
    /// One event. Returning an error only logs it; the stream continues.
    fn on_event(&self, event: HostEvent) -> Result<(), PlayDropError>;
}

/// Where verified files go on a host without a receive folder (Android `MediaStore`).
#[uniffi::export(foreign)]
pub trait ReceiveSink: Send + Sync {
    /// Called once per verified file after the root hash check. `fd` is the verified
    /// staging file, open read-only at offset 0; the sink owns it and must close it. Copy
    /// the bytes to their final place and return where they went (a `content://` URI). The
    /// core deletes the staging file afterwards. An error fails the transfer with the
    /// message shown to the user.
    fn deliver(&self, item: DeliverItem, fd: i32) -> Result<String, PlayDropError>;
    /// Called once per empty directory in the manifest.
    fn make_dir(&self, path: Vec<String>) -> Result<(), PlayDropError>;
}

/// One file handed to [`ReceiveSink::deliver`].
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DeliverItem {
    /// Transfer ID.
    pub transfer_id: String,
    /// Hex Device ID of the sender.
    pub peer: String,
    /// Manifest path components (SR-17 validated).
    pub path: Vec<String>,
    /// Size in bytes.
    pub size: u64,
    /// Modification time, Unix ms.
    pub mtime_ms: i64,
    /// MIME type hint from the manifest.
    pub mime: Option<String>,
}

/// Opens a file to send. Called at offer time and once per upload or bundle member.
#[uniffi::export(foreign)]
pub trait SourceOpener: Send + Sync {
    /// Returns a fresh read-only file descriptor at offset 0 that the core will own and
    /// close. Must be a seekable regular file (`ParcelFileDescriptor.detachFd()` from
    /// `ContentResolver.openFileDescriptor(uri, "r")`).
    fn open(&self) -> Result<i32, PlayDropError>;
}

/// The host's own view of its IPv4 interfaces, for platforms where `getifaddrs` is not
/// reliable. Return every up, non-loopback interface with its index and netmask.
#[uniffi::export(foreign)]
pub trait NetworkInterfaces: Send + Sync {
    /// Current interfaces.
    fn ipv4_interfaces(&self) -> Result<Vec<Ipv4Interface>, PlayDropError>;
}

/// Seals the identity key and pin store at rest (SR-28). Android backs it with an
/// AES-256-GCM Keystore key (`StrongBox` when present, no user authentication so screen-off
/// receive works). Called on a background thread at start and on every pin save.
#[uniffi::export(foreign)]
pub trait SecretStore: Send + Sync {
    /// Encrypts bytes before they are written to the data dir.
    fn seal(&self, plain: Vec<u8>) -> Result<Vec<u8>, PlayDropError>;
    /// Decrypts bytes read from the data dir.
    fn open(&self, sealed: Vec<u8>) -> Result<Vec<u8>, PlayDropError>;
}

// ---------------------------------------------------------------------------------------
// Adapters: foreign trait object -> core trait
// ---------------------------------------------------------------------------------------

/// Presents a foreign [`SecretStore`] to the core.
pub(crate) struct ForeignSecretStore(pub(crate) Arc<dyn SecretStore>);

impl fmt::Debug for ForeignSecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ForeignSecretStore")
    }
}

impl host::SecretStore for ForeignSecretStore {
    fn seal(&self, plain: Vec<u8>) -> Result<Vec<u8>, host::SecretStoreError> {
        self.0.seal(plain).map_err(|e| host::SecretStoreError {
            message: e.to_string(),
        })
    }

    fn open(&self, sealed: Vec<u8>) -> Result<Vec<u8>, host::SecretStoreError> {
        self.0.open(sealed).map_err(|e| host::SecretStoreError {
            message: e.to_string(),
        })
    }
}

/// Presents a foreign [`ReceiveSink`] to the core.
pub(crate) struct ForeignSink(pub(crate) Arc<dyn ReceiveSink>);

impl fmt::Debug for ForeignSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ForeignSink")
    }
}

impl host::ReceiveSink for ForeignSink {
    fn deliver(
        &self,
        item: host::DeliverItem,
        file: std::fs::File,
    ) -> Result<String, host::SinkError> {
        let item = DeliverItem {
            transfer_id: item.transfer_id,
            peer: item.peer,
            path: item.path,
            size: item.size,
            mtime_ms: item.mtime_ms,
            mime: item.mime,
        };
        // Ownership of the descriptor moves to the foreign side, which closes it.
        let fd = file.into_raw_fd();
        self.0.deliver(item, fd).map_err(|e| host::SinkError {
            message: e.to_string(),
        })
    }

    fn make_dir(&self, path: Vec<String>) -> Result<(), host::SinkError> {
        self.0.make_dir(path).map_err(|e| host::SinkError {
            message: e.to_string(),
        })
    }
}

/// Presents a foreign [`SourceOpener`] to the core.
pub(crate) struct ForeignSource(pub(crate) Arc<dyn SourceOpener>);

impl fmt::Debug for ForeignSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ForeignSource")
    }
}

impl host::SourceOpener for ForeignSource {
    fn open(&self) -> std::io::Result<std::fs::File> {
        let fd = self.0.open().map_err(std::io::Error::other)?;
        if fd < 0 {
            return Err(std::io::Error::other(
                "source opener returned a negative fd",
            ));
        }
        #[allow(unsafe_code)]
        // SAFETY: the foreign side detached the descriptor and promised not to use or close
        // it again (`SourceOpener::open` contract), so this `File` is its sole owner. A bad
        // descriptor makes later reads fail with EBADF; it cannot alias another `File`.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        Ok(file)
    }
}

/// Presents a foreign [`NetworkInterfaces`] to the core's discovery.
pub(crate) struct ForeignInterfaces(pub(crate) Arc<dyn NetworkInterfaces>);

impl InterfaceSource for ForeignInterfaces {
    fn ipv4_interfaces(&self) -> Vec<CoreIpv4Interface> {
        let list = match self.0.ipv4_interfaces() {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "host interface list failed; discovery sees none");
                return Vec::new();
            }
        };
        list.into_iter()
            .filter_map(|i| {
                let addr = i.addr.parse().ok()?;
                let netmask = i.netmask.parse().ok()?;
                Some(CoreIpv4Interface {
                    index: i.index,
                    addr,
                    netmask,
                })
            })
            .collect()
    }
}

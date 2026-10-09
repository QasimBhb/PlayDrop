//! Device identity: Ed25519 keypair, self-signed certificate, Device ID, Short ID.
//! Implements `docs/protocol.md` section 2 and SR-10, SR-28.
//!
//! A device has exactly one identity. Its Device ID is the SHA-256 of the DER
//! `SubjectPublicKeyInfo`, so a new key is a new device (SR-10): nothing here migrates or
//! replaces an existing key.

pub mod sealing;
pub(crate) mod secure_fs;

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use rustls::sign::CertifiedKey;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use x509_parser::oid_registry::OID_SIG_ED25519;
use x509_parser::x509::X509Version;

use crate::{Error, Result};

/// File name of the DER certificate inside the identity directory (SR-28).
pub const CERT_FILE: &str = "identity.crt";
/// File name of the PKCS#8 DER private key inside the identity directory (SR-28).
pub const KEY_FILE: &str = "identity.key";

/// Length of a Short ID before grouping, protocol.md section 2.
const SHORT_ID_CHARS: usize = 16;

/// Fills `buf` from the operating system CSPRNG.
pub(crate) fn fill_random(buf: &mut [u8]) -> Result<()> {
    use rand::TryRng;
    rand::rngs::SysRng
        .try_fill_bytes(buf)
        .map_err(|e| Error::Identity(format!("system random source failed: {e}")))
}

/// SHA-256 of `data` as raw bytes.
fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// The SPKI hash of a DER `SubjectPublicKeyInfo`: its raw SHA-256 (protocol.md section 2).
pub fn spki_hash_of(spki_der: &[u8]) -> [u8; 32] {
    sha256(spki_der)
}

/// Device ID of a DER `SubjectPublicKeyInfo` (protocol.md section 2). Pure; no parsing.
pub fn device_id_from_spki(spki_der: &[u8]) -> DeviceId {
    DeviceId(spki_hash_of(spki_der))
}

/// Device ID of a DER certificate, after the checks of [`inspect_certificate`].
pub fn device_id_from_cert(cert_der: &[u8]) -> Result<DeviceId> {
    inspect_certificate(cert_der).map(|info| info.device_id)
}

/// Certificate fingerprint `fp`: lowercase hex SHA-256 of the whole DER certificate
/// (protocol.md section 2, used by QR pairing in 6.3).
pub fn fingerprint_of(cert_der: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&sha256(cert_der))
}

/// What the verifier learns from a peer certificate (protocol.md section 2 and 3.1).
#[derive(Clone, PartialEq, Eq)]
pub struct CertInfo {
    /// Device ID derived from the SPKI.
    pub device_id: DeviceId,
    /// Raw SHA-256 of the DER SPKI.
    pub spki_hash: [u8; 32],
    /// DER `SubjectPublicKeyInfo`, as needed for the SAS input in protocol.md 6.2.
    pub spki_der: Vec<u8>,
}

impl fmt::Debug for CertInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertInfo")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

/// Parses a DER certificate and checks it is a valid self-signed `PlayDrop` certificate
/// (protocol.md section 2 and 3.1, SR-11):
///
/// - one well-formed X.509 v3 certificate with no trailing bytes,
/// - an Ed25519 subject key and an Ed25519 signature algorithm (no parameters),
/// - a signature that verifies under its own key (self-signed).
///
/// Validity dates and the CN are deliberately not checked: trust comes from the pin, and
/// wrong phone clocks must not break it (protocol.md section 2).
pub fn inspect_certificate(cert_der: &[u8]) -> Result<CertInfo> {
    let bad = |what: &str| Error::Identity(format!("peer certificate rejected: {what}"));

    let (rest, cert) =
        x509_parser::parse_x509_certificate(cert_der).map_err(|_| bad("not valid DER X.509"))?;
    if !rest.is_empty() {
        return Err(bad("trailing bytes after certificate"));
    }
    if cert.version() != X509Version::V3 {
        return Err(bad("not an X.509 v3 certificate"));
    }
    let spki = &cert.tbs_certificate.subject_pki;
    if spki.algorithm.algorithm != OID_SIG_ED25519 || spki.algorithm.parameters.is_some() {
        return Err(bad("subject key is not Ed25519"));
    }
    if cert.signature_algorithm.algorithm != OID_SIG_ED25519
        || cert.signature_algorithm.parameters.is_some()
        || cert.tbs_certificate.signature != cert.signature_algorithm
    {
        return Err(bad("signature algorithm is not Ed25519"));
    }

    let key_bytes: [u8; 32] = spki
        .subject_public_key
        .data
        .as_ref()
        .try_into()
        .map_err(|_| bad("Ed25519 key has the wrong length"))?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&key_bytes)
        .map_err(|_| bad("Ed25519 key is not a valid point"))?;
    let signature = ed25519_dalek::Signature::from_slice(cert.signature_value.data.as_ref())
        .map_err(|_| bad("Ed25519 signature has the wrong length"))?;
    key.verify_strict(cert.tbs_certificate.as_ref(), &signature)
        .map_err(|_| bad("self-signature does not verify"))?;

    let spki_der = spki.raw.to_vec();
    let spki_hash = spki_hash_of(&spki_der);
    Ok(CertInfo {
        device_id: DeviceId(spki_hash),
        spki_hash,
        spki_der,
    })
}

/// A Device ID: the SHA-256 of a device's DER `SubjectPublicKeyInfo` (protocol.md section 2).
///
/// Shown and serialised as 64 lowercase hex characters. Ordering compares the raw bytes,
/// which is the same order as comparing the lowercase hex strings, as the duplicate
/// connection rule (protocol.md 3.3) and the SAS input ordering (6.2) require.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId([u8; 32]);

impl DeviceId {
    /// Wraps a raw SPKI hash (protocol.md section 2).
    pub const fn from_spki_hash(spki_hash: [u8; 32]) -> Self {
        Self(spki_hash)
    }

    /// The raw 32-byte SPKI hash this ID encodes.
    pub const fn spki_hash(&self) -> &[u8; 32] {
        &self.0
    }

    /// The 64-character lowercase hex form.
    pub fn to_hex(&self) -> String {
        data_encoding::HEXLOWER.encode(&self.0)
    }

    /// Short ID: the first 16 characters of the lowercase unpadded base32 of the SPKI hash,
    /// in groups of four joined by hyphens (protocol.md section 2, SR-8).
    pub fn short_id(&self) -> String {
        let b32 = data_encoding::BASE32_NOPAD
            .encode(&self.0)
            .to_ascii_lowercase();
        let head = &b32.as_bytes()[..SHORT_ID_CHARS];
        let groups: Vec<&str> = head
            .chunks(4)
            .map(|g| std::str::from_utf8(g).unwrap_or_default())
            .collect();
        groups.join("-")
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({})", self.to_hex())
    }
}

/// Error parsing a [`DeviceId`] from text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("device ID must be 64 lowercase hex characters")]
pub struct ParseDeviceIdError;

impl FromStr for DeviceId {
    type Err = ParseDeviceIdError;

    /// Accepts only the canonical form: exactly 64 lowercase hex characters.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s.len() != 64 {
            return Err(ParseDeviceIdError);
        }
        let bytes = data_encoding::HEXLOWER
            .decode(s.as_bytes())
            .map_err(|_| ParseDeviceIdError)?;
        let arr: [u8; 32] = bytes.try_into().map_err(|_| ParseDeviceIdError)?;
        Ok(Self(arr))
    }
}

impl serde::Serialize for DeviceId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> serde::Deserialize<'de> for DeviceId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Private key bytes that are overwritten with zeros when dropped (SR-28), using
/// `zeroize` so the compiler cannot elide the write.
#[derive(Clone)]
struct SecretBytes(Vec<u8>);

impl Drop for SecretBytes {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

/// This device's identity: Ed25519 key, self-signed certificate and derived IDs
/// (protocol.md section 2, SR-10, SR-28).
pub struct DeviceIdentity {
    device_id: DeviceId,
    spki_der: Vec<u8>,
    cert_der: CertificateDer<'static>,
    key_der: SecretBytes,
    certified_key: Arc<CertifiedKey>,
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceIdentity")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

/// Builds the self-signed certificate for `key` per protocol.md section 2:
/// CN = Device ID, random positive serial, `notAfter` = 99991231235959Z.
fn self_sign(key: &rcgen::KeyPair) -> Result<Vec<u8>> {
    use rcgen::PublicKeyData;

    let device_id = device_id_from_spki(&key.subject_public_key_info());
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())
        .map_err(|e| Error::Identity(format!("certificate parameters: {e}")))?;
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, device_id.to_hex());
    params.distinguished_name = dn;

    let mut serial = [0u8; 16];
    fill_random(&mut serial)?;
    // Positive and minimal as a DER INTEGER: top bit clear, second bit set.
    serial[0] = (serial[0] & 0x7f) | 0x40;
    params.serial_number = Some(rcgen::SerialNumber::from_slice(&serial));
    params.not_after = rcgen::date_time_ymd(9999, 12, 31)
        + std::time::Duration::from_secs(23 * 3600 + 59 * 60 + 59);

    let cert = params
        .self_signed(key)
        .map_err(|e| Error::Identity(format!("self-signing failed: {e}")))?;
    Ok(cert.der().to_vec())
}

impl DeviceIdentity {
    /// Generates a fresh Ed25519 identity in memory (protocol.md section 2).
    pub fn generate() -> Result<Self> {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)
            .map_err(|e| Error::Identity(format!("key generation failed: {e}")))?;
        let key_der = SecretBytes(key.serialize_der());
        let cert_der = self_sign(&key)?;
        Self::from_der(cert_der, key_der.0.clone())
    }

    /// Rebuilds an identity from a DER certificate and PKCS#8 DER private key, checking
    /// that the certificate is a valid Ed25519 `PlayDrop` certificate and that the key
    /// matches it.
    pub fn from_der(cert_der: Vec<u8>, key_der: Vec<u8>) -> Result<Self> {
        let key_der = SecretBytes(key_der);
        let info = inspect_certificate(&cert_der)?;
        let cert_der = CertificateDer::from(cert_der);
        let provider = rustls::crypto::ring::default_provider();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der.0.clone()));
        let certified_key = CertifiedKey::from_der(vec![cert_der.clone()], key, &provider)
            .map_err(|e| Error::Identity(format!("private key unusable: {e}")))?;
        // `from_der` tolerates an unknown match result; require a positive one.
        certified_key
            .keys_match()
            .map_err(|e| Error::Identity(format!("private key does not match certificate: {e}")))?;
        Ok(Self {
            device_id: info.device_id,
            spki_der: info.spki_der,
            cert_der,
            key_der,
            certified_key: Arc::new(certified_key),
        })
    }

    /// Loads the identity stored in `dir`, or creates and stores a new one (SR-28).
    ///
    /// The directory is created with mode 0700 and the files with 0600 on Unix. Existing
    /// paths with looser modes are refused with `PermissionDenied`, never repaired. If the
    /// key exists but the certificate is missing (a crash between the two writes), the
    /// certificate is re-issued for the same key, so the Device ID is unchanged. A
    /// certificate without its key is an error: generating a new key would silently make
    /// this a new device (SR-10), so that decision is left to the user.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        Self::load_or_create_sealed(dir, &None)
    }

    /// [`load_or_create`](Self::load_or_create) with `identity.key` sealed by `store`
    /// (`docs/core-api.md` section 2a). A plain key file is read once and rewritten sealed;
    /// a sealed one without a store fails with [`Error::Identity`]. Call it from a blocking
    /// thread: the store may be foreign code.
    pub fn load_or_create_sealed(dir: &Path, store: &sealing::Sealer) -> Result<Self> {
        secure_fs::ensure_private_dir(dir)?;
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        let key = match secure_fs::read_private(&key_path)? {
            Some(raw) => {
                let (plain, was_plain) = sealing::unseal(store, raw, KEY_FILE)?;
                let plain = SecretBytes(plain);
                if was_plain && store.is_some() {
                    let sealed = sealing::seal(store, &plain.0, KEY_FILE)?;
                    secure_fs::write_private_atomic(&key_path, &sealed)?;
                }
                Some(plain)
            }
            None => None,
        };
        let cert = secure_fs::read_private(&cert_path)?;

        match (cert, key) {
            (Some(cert), Some(key)) => Self::from_der(cert, key.0.clone()),
            (None, Some(key)) => {
                let pkcs8 = PrivatePkcs8KeyDer::from(key.0.as_slice());
                let pair =
                    rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&pkcs8, &rcgen::PKCS_ED25519)
                        .map_err(|e| Error::Identity(format!("stored key unreadable: {e}")))?;
                let cert = self_sign(&pair)?;
                secure_fs::write_private_atomic(&cert_path, &cert)?;
                Self::from_der(cert, key.0.clone())
            }
            (Some(_), None) => Err(Error::Identity(format!(
                "{} exists but {} is missing; the private key is lost. Remove the certificate \
                 to start over as a new device (SR-10)",
                cert_path.display(),
                key_path.display()
            ))),
            (None, None) => {
                let identity = Self::generate()?;
                let key_file =
                    SecretBytes(sealing::seal(store, identity.private_key_der(), KEY_FILE)?);
                let key_tmp = secure_fs::stage_private(&key_path, &key_file.0)?;
                let cert_tmp =
                    match secure_fs::stage_private(&cert_path, identity.certificate_der()) {
                        Ok(tmp) => tmp,
                        Err(e) => {
                            let _ = std::fs::remove_file(&key_tmp);
                            return Err(e);
                        }
                    };
                // Key first: a crash after this point is recovered by the branch above.
                if let Err(e) = secure_fs::commit(&key_tmp, &key_path) {
                    let _ = std::fs::remove_file(&cert_tmp);
                    return Err(e);
                }
                secure_fs::commit(&cert_tmp, &cert_path)?;
                Ok(identity)
            }
        }
    }

    /// This device's ID (protocol.md section 2).
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    /// Raw SHA-256 of the DER SPKI (protocol.md section 2).
    pub fn spki_hash(&self) -> [u8; 32] {
        self.device_id.0
    }

    /// DER `SubjectPublicKeyInfo`, an input to the SAS (protocol.md 6.2).
    pub fn spki_der(&self) -> &[u8] {
        &self.spki_der
    }

    /// Short ID shown in every trust prompt (protocol.md section 2, SR-8).
    pub fn short_id(&self) -> String {
        self.device_id.short_id()
    }

    /// Certificate fingerprint `fp` for the QR payload (protocol.md section 2 and 6.3).
    pub fn fingerprint(&self) -> String {
        fingerprint_of(&self.cert_der)
    }

    /// DER certificate.
    pub fn certificate_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// PKCS#8 DER private key. Handle with care: never log it (SR-28, SR-32).
    pub fn private_key_der(&self) -> &[u8] {
        &self.key_der.0
    }

    /// The certificate and signing key in the form rustls uses (protocol.md 3.1).
    pub fn certified_key(&self) -> Arc<CertifiedKey> {
        Arc::clone(&self.certified_key)
    }
}

/// Default identity directory: `$XDG_DATA_HOME/playdrop` on Linux and the platform's
/// local data directory elsewhere (SR-28). Callers pass the directory explicitly to
/// [`DeviceIdentity::load_or_create`]; this only computes the usual choice.
pub fn default_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "PlayDrop").map(|p| p.data_local_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn identity_round_trips_through_disk() {
        let tmp = tempdir();
        let dir = tmp.path().join("id");
        let a = DeviceIdentity::load_or_create(&dir).unwrap();
        let b = DeviceIdentity::load_or_create(&dir).unwrap();
        assert_eq!(a.device_id(), b.device_id());
        assert_eq!(a.certificate_der(), b.certificate_der());
        assert_eq!(a.private_key_der(), b.private_key_der());
    }

    #[test]
    fn device_id_is_sha256_of_spki() {
        let id = DeviceIdentity::generate().unwrap();
        let (_, cert) = x509_parser::parse_x509_certificate(id.certificate_der()).unwrap();
        let spki = cert.tbs_certificate.subject_pki.raw;
        let expected: [u8; 32] = Sha256::digest(spki).into();
        assert_eq!(id.spki_hash(), expected);
        assert_eq!(
            id.device_id().to_hex(),
            data_encoding::HEXLOWER.encode(&expected)
        );
        assert_eq!(id.device_id().to_hex().len(), 64);
        assert_eq!(device_id_from_spki(spki), *id.device_id());
        assert_eq!(
            device_id_from_cert(id.certificate_der()).unwrap(),
            *id.device_id()
        );
        assert_eq!(id.spki_der(), spki);
    }

    #[test]
    fn certificate_matches_profile() {
        let id = DeviceIdentity::generate().unwrap();
        let (_, cert) = x509_parser::parse_x509_certificate(id.certificate_der()).unwrap();
        assert_eq!(cert.version(), X509Version::V3);
        let cn = cert
            .subject()
            .iter_common_name()
            .next()
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(cn, id.device_id().to_hex());
        assert_eq!(cert.validity().not_after.timestamp(), 253_402_300_799);
        assert_eq!(cert.raw_serial()[0] & 0x80, 0);
        let other = DeviceIdentity::generate().unwrap();
        let (_, cert2) = x509_parser::parse_x509_certificate(other.certificate_der()).unwrap();
        assert_ne!(cert.raw_serial(), cert2.raw_serial());
    }

    #[test]
    fn fingerprint_is_sha256_of_cert() {
        let id = DeviceIdentity::generate().unwrap();
        let expected: [u8; 32] = Sha256::digest(id.certificate_der()).into();
        assert_eq!(id.fingerprint(), data_encoding::HEXLOWER.encode(&expected));
    }

    #[test]
    fn short_id_format() {
        let id = DeviceId::from_spki_hash([0u8; 32]);
        assert_eq!(id.short_id(), "aaaa-aaaa-aaaa-aaaa");
        let real = DeviceIdentity::generate().unwrap();
        let s = real.short_id();
        assert_eq!(s.len(), 19);
        let parts: Vec<&str> = s.split('-').collect();
        assert_eq!(parts.len(), 4);
        for p in parts {
            assert_eq!(p.len(), 4);
            assert!(p
                .chars()
                .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c)));
        }
        let b32 = data_encoding::BASE32_NOPAD
            .encode(&real.spki_hash())
            .to_ascii_lowercase();
        assert_eq!(s.replace('-', ""), b32[..16]);
    }

    #[test]
    fn device_id_text_and_serde() {
        let id = *DeviceIdentity::generate().unwrap().device_id();
        let text = id.to_string();
        assert_eq!(text.parse::<DeviceId>().unwrap(), id);
        assert!(text.to_ascii_uppercase().parse::<DeviceId>().is_err());
        assert!(text[..63].parse::<DeviceId>().is_err());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<DeviceId>(&json).unwrap(), id);
    }

    #[test]
    fn device_id_order_matches_hex_order() {
        let mut ids: Vec<DeviceId> = (0..16)
            .map(|_| *DeviceIdentity::generate().unwrap().device_id())
            .collect();
        let mut hex: Vec<String> = ids.iter().map(DeviceId::to_hex).collect();
        ids.sort();
        hex.sort();
        assert_eq!(ids.iter().map(DeviceId::to_hex).collect::<Vec<_>>(), hex);
    }

    #[test]
    fn rejects_non_ed25519_certificate() {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let cert = rcgen::CertificateParams::new(Vec::<String>::new())
            .unwrap()
            .self_signed(&key)
            .unwrap();
        assert!(inspect_certificate(cert.der()).is_err());
    }

    #[test]
    fn rejects_bad_self_signature() {
        let id = DeviceIdentity::generate().unwrap();
        let mut der = id.certificate_der().to_vec();
        let last = der.len() - 1;
        der[last] ^= 0x01;
        assert!(inspect_certificate(&der).is_err());
    }

    #[test]
    fn rejects_mismatched_key() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        assert!(DeviceIdentity::from_der(
            a.certificate_der().to_vec(),
            b.private_key_der().to_vec()
        )
        .is_err());
    }

    #[test]
    fn recovers_missing_certificate_with_same_id() {
        let tmp = tempdir();
        let dir = tmp.path().join("id");
        let a = DeviceIdentity::load_or_create(&dir).unwrap();
        std::fs::remove_file(dir.join(CERT_FILE)).unwrap();
        let b = DeviceIdentity::load_or_create(&dir).unwrap();
        assert_eq!(a.device_id(), b.device_id());
    }

    #[test]
    fn missing_key_is_an_error() {
        let tmp = tempdir();
        let dir = tmp.path().join("id");
        DeviceIdentity::load_or_create(&dir).unwrap();
        std::fs::remove_file(dir.join(KEY_FILE)).unwrap();
        assert!(DeviceIdentity::load_or_create(&dir).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn modes_are_private_and_loose_modes_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let set = |p: &Path, m: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
        };
        let is_denied = |r: Result<DeviceIdentity>| matches!(r, Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::PermissionDenied);

        let tmp = tempdir();
        let dir = tmp.path().join("id");
        DeviceIdentity::load_or_create(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(KEY_FILE)), 0o600);
        assert_eq!(mode(&dir.join(CERT_FILE)), 0o600);

        set(&dir.join(KEY_FILE), 0o644);
        assert!(is_denied(DeviceIdentity::load_or_create(&dir)));
        set(&dir.join(KEY_FILE), 0o600);

        set(&dir.join(CERT_FILE), 0o640);
        assert!(is_denied(DeviceIdentity::load_or_create(&dir)));
        set(&dir.join(CERT_FILE), 0o600);

        set(&dir, 0o755);
        assert!(is_denied(DeviceIdentity::load_or_create(&dir)));
        set(&dir, 0o700);

        DeviceIdentity::load_or_create(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_key_is_refused() {
        let tmp = tempdir();
        let dir = tmp.path().join("id");
        DeviceIdentity::load_or_create(&dir).unwrap();
        let real = tmp.path().join("elsewhere.key");
        std::fs::rename(dir.join(KEY_FILE), &real).unwrap();
        std::os::unix::fs::symlink(&real, dir.join(KEY_FILE)).unwrap();
        assert!(DeviceIdentity::load_or_create(&dir).is_err());
    }
}

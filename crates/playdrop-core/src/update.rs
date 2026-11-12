//! The update check: every decision, no network (`docs/design/update-check.md`, SR-45 to SR-48).
//!
//! Hosts fetch two small files from GitHub Releases, `SHA256SUMS` and `SHA256SUMS.sig`, and
//! hand the bytes to this module. It answers three questions and nothing else: is the
//! checksum file signed by a pinned release key ([`verify_sums`]), does it name a version
//! newer than the running one ([`SignedSums::offer`]), and does a downloaded file match its
//! signed line ([`Asset::matches`]). It also holds the fixed request facts (the URLs, the
//! `User-Agent`, the size limits, the redirect rule, the once-a-day rule) so that every
//! platform asks the same anonymous question.
//!
//! No sockets, no clock and no files are touched here: the caller passes bytes and
//! millisecond timestamps. That keeps the parser fuzzable (SR-33) and identical on the
//! desktop daemon, Android and iOS (the last two through `playdrop-ffi`).

use std::fmt;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

/// Where the latest release's checksum file lives. The same for every install (SR-45).
pub const SUMS_URL: &str =
    "https://github.com/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS";
/// Where the signature over [`SUMS_URL`]'s bytes lives.
pub const SIG_URL: &str =
    "https://github.com/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS.sig";
/// Base of a release's files: `<base>/v<version>/<name>`.
pub const DOWNLOAD_BASE: &str = "https://github.com/QasimBhb/PlayDrop/releases/download";
/// Base of a release's page: `<base>/v<version>`.
pub const RELEASE_PAGE_BASE: &str = "https://github.com/QasimBhb/PlayDrop/releases/tag";

/// The only `User-Agent` an update request carries: no version, no OS, no ID (SR-45).
pub const USER_AGENT: &str = "PlayDrop";

/// Largest `SHA256SUMS` accepted (SR-48).
pub const MAX_SUMS_LEN: usize = 64 * 1024;
/// Largest `SHA256SUMS.sig` accepted (SR-48).
pub const MAX_SIG_LEN: usize = 1024;
/// Largest release file an update downloads (SR-48).
pub const MAX_DOWNLOAD_LEN: u64 = 512 * 1024 * 1024;
/// Most redirects followed for one request (SR-48).
pub const MAX_REDIRECTS: u32 = 5;
/// Time limit for each of the two check requests, in milliseconds (SR-48).
pub const REQUEST_TIMEOUT_MS: u64 = 15_000;
/// A check that got an answer is not repeated sooner than this (SR-45).
pub const CHECK_EVERY_MS: u64 = 24 * 60 * 60 * 1000;
/// A check that failed may be tried again after this (SR-45).
pub const RETRY_AFTER_FAILURE_MS: u64 = 60 * 60 * 1000;

/// Most lines a checksum file may hold. A release has six files.
const MAX_ENTRIES: usize = 64;
/// Longest file name accepted on a checksum line.
const MAX_NAME_LEN: usize = 128;

/// The Ed25519 public keys a release's `SHA256SUMS.sig` may be made with (SR-46).
///
/// A list, so a key can be rotated: add the new key in one release, drop the old one in a
/// later release. The private half of the first key was made on 2026-11-12 for this purpose
/// alone and lives outside the repository.
pub const TRUSTED_KEYS: &[[u8; 32]] = &[[
    0x3d, 0xc1, 0x96, 0xf0, 0x8c, 0x77, 0x6f, 0x69, 0x3f, 0xfb, 0xc6, 0x96, 0x08, 0xaa, 0xb9, 0xb3,
    0x11, 0x86, 0x02, 0xf5, 0x8d, 0xfd, 0xe9, 0x14, 0x92, 0x38, 0x71, 0xf9, 0x54, 0x35, 0x7f, 0xf8,
]];

/// Why an update check or an update was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    /// An answer was larger than its limit (SR-48).
    #[error("the answer is larger than allowed")]
    TooLarge,
    /// `SHA256SUMS.sig` is not base64 of a 64-byte signature.
    #[error("the signature file is not a signature")]
    BadSignatureEncoding,
    /// No pinned key made this signature over these bytes (SR-46).
    #[error("the checksum file is not signed by a PlayDrop release key")]
    NotSigned,
    /// The checksum file is signed but does not read as `sha256sum` output.
    #[error("the checksum file is malformed: {0}")]
    Malformed(&'static str),
    /// The file names do not agree on one release version.
    #[error("the checksum file does not name one release version")]
    NoVersion,
    /// A version string is not `major.minor.patch`.
    #[error("not a release version")]
    BadVersion,
    /// A trusted key in hex was not 64 hex characters (test keys only).
    #[error("not an Ed25519 public key in hex")]
    BadKey,
}

/// A release version, `major.minor.patch`. Pre-release tags are not releases to update to:
/// GitHub's "latest" never points at one, and [`Version::parse`] refuses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Major.
    pub major: u32,
    /// Minor.
    pub minor: u32,
    /// Patch.
    pub patch: u32,
}

impl Version {
    /// Parses exactly three dot-separated decimal numbers with no sign, space or leading
    /// zero (`0` itself is fine).
    pub fn parse(text: &str) -> Result<Self, UpdateError> {
        let mut parts = text.split('.');
        let mut next = || -> Result<u32, UpdateError> {
            let part = parts.next().ok_or(UpdateError::BadVersion)?;
            let plain = !part.is_empty()
                && part.len() <= 9
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'));
            if !plain {
                return Err(UpdateError::BadVersion);
            }
            part.parse().map_err(|_| UpdateError::BadVersion)
        };
        let version = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        if parts.next().is_some() {
            return Err(UpdateError::BadVersion);
        }
        Ok(version)
    }

    /// The version this build of the core was compiled as, or `None` when that is not a
    /// plain release version (a pre-release such as `0.11.0-rc.1`, or a test build).
    /// Every `PlayDrop` program shares the workspace version, so this is the running
    /// version on the desktop.
    ///
    /// `None` means "this build has no update check". Reading it as 0.0.0 instead would
    /// offer such a build any release, including one older than itself.
    pub fn current() -> Option<Self> {
        Self::parse(env!("CARGO_PKG_VERSION")).ok()
    }
}

/// The plain sentence a UI shows in Settings when the running build is not a release, so it
/// has no update check (see [`Version::current`]).
pub const NOT_A_RELEASE: &str = "This is not a release build, so it does not check for updates.";

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// How this copy of `PlayDrop` was installed, which decides the file an update would fetch
/// and whether the app can install it or only say that one exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstallKind {
    /// The `.deb` on an amd64 Debian-family system.
    Deb,
    /// The Windows installer, x64.
    Windows,
    /// The Android APK.
    Android,
    /// The Flatpak: notice only.
    Flatpak,
    /// The iPhone app: notice only.
    Ios,
    /// Anything else (a source build, another architecture): notice only.
    Other,
}

impl InstallKind {
    /// The release file this install updates from, or `None` when the app can only show a
    /// notice. Names are the ones `.github/workflows/release.yml` publishes.
    pub fn asset_name(self, version: Version) -> Option<String> {
        match self {
            Self::Deb => Some(format!("playdrop-{version}-linux-amd64.deb")),
            Self::Windows => Some(format!("playdrop-{version}-windows-x64-setup.exe")),
            Self::Android => Some(format!("playdrop-{version}-android.apk")),
            Self::Flatpak | Self::Ios | Self::Other => None,
        }
    }
}

/// The optional COSMIC applet's `.deb`, updated together with [`InstallKind::Deb`] when the
/// applet is installed.
pub fn applet_asset_name(version: Version) -> String {
    format!("playdrop-cosmic-applet-{version}-linux-amd64.deb")
}

/// One release file an update downloads: its name, where it is, and the hash it must have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// File name, exactly as signed.
    pub name: String,
    /// `https://github.com/.../releases/download/v<version>/<name>`.
    pub url: String,
    /// The signed SHA-256 of the file.
    pub sha256: [u8; 32],
}

impl Asset {
    /// The signed SHA-256 in lowercase hex.
    pub fn sha256_hex(&self) -> String {
        hex(&self.sha256)
    }

    /// Whether `digest`, the SHA-256 of what was downloaded, is the signed one (SR-46).
    pub fn matches(&self, digest: &[u8]) -> bool {
        // Not secret data, so a plain comparison is enough.
        digest == self.sha256
    }

    /// Whether `bytes` are this file. Hosts that stream a download hash as they go and use
    /// [`Asset::matches`]; this is for small files and tests.
    pub fn matches_bytes(&self, bytes: &[u8]) -> bool {
        self.matches(Sha256::digest(bytes).as_slice())
    }
}

/// A newer release, as offered to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The newer version.
    pub version: Version,
    /// The release page, for "what is new" and for installs that only get a notice.
    pub page_url: String,
    /// The file to download, or `None` when this install kind only gets a notice.
    pub asset: Option<Asset>,
}

/// A checksum file whose signature verified against a pinned key. Only [`verify_sums`] and
/// [`verify_sums_with`] make one, so holding a `SignedSums` means the bytes were signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedSums {
    version: Version,
    entries: Vec<(String, [u8; 32])>,
}

impl SignedSums {
    /// The release version the file names agree on.
    pub fn version(&self) -> Version {
        self.version
    }

    /// The signed SHA-256 for exactly `name`.
    pub fn sha256_of(&self, name: &str) -> Option<[u8; 32]> {
        self.entries
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, sum)| *sum)
    }

    /// The signed entry for `name` as a downloadable [`Asset`].
    pub fn asset(&self, name: &str) -> Option<Asset> {
        let sha256 = self.sha256_of(name)?;
        Some(Asset {
            name: name.to_owned(),
            url: format!("{DOWNLOAD_BASE}/v{}/{name}", self.version),
            sha256,
        })
    }

    /// What to offer an install of `kind` running `current`: nothing unless this release is
    /// strictly newer (so an old or replayed checksum file can never downgrade, SR-46).
    ///
    /// When `kind` installs from a file that this release does not list, the offer carries
    /// no asset and the app falls back to a notice with the release page.
    pub fn offer(&self, current: Version, kind: InstallKind) -> Option<Offer> {
        if self.version <= current {
            return None;
        }
        let asset = kind
            .asset_name(self.version)
            .and_then(|name| self.asset(&name));
        Some(Offer {
            version: self.version,
            page_url: format!("{RELEASE_PAGE_BASE}/v{}", self.version),
            asset,
        })
    }

    /// The applet's `.deb` in this release, when it is listed.
    pub fn applet_asset(&self) -> Option<Asset> {
        self.asset(&applet_asset_name(self.version))
    }
}

/// Verifies `sig` (the bytes of `SHA256SUMS.sig`) over `sums` (the bytes of `SHA256SUMS`)
/// against [`TRUSTED_KEYS`], then parses the checksum file. Nothing is parsed before the
/// signature verifies, so unsigned bytes never reach the line parser's results.
pub fn verify_sums(sums: &[u8], sig: &[u8]) -> Result<SignedSums, UpdateError> {
    verify_sums_with(sums, sig, TRUSTED_KEYS)
}

/// [`verify_sums`] against a caller-given key list. Release hosts always pass
/// [`TRUSTED_KEYS`]; debug builds may add one test key (design note section 8).
pub fn verify_sums_with(
    sums: &[u8],
    sig: &[u8],
    keys: &[[u8; 32]],
) -> Result<SignedSums, UpdateError> {
    if sums.len() > MAX_SUMS_LEN || sig.len() > MAX_SIG_LEN {
        return Err(UpdateError::TooLarge);
    }
    let signature = decode_signature(sig)?;
    let signed = keys.iter().any(|key| {
        ed25519_dalek::VerifyingKey::from_bytes(key)
            .is_ok_and(|key| key.verify_strict(sums, &signature).is_ok())
    });
    if !signed {
        return Err(UpdateError::NotSigned);
    }
    parse_sums(sums)
}

/// Parses a trusted key from 64 hex characters (either case). For test keys only: release
/// keys are compiled in.
pub fn key_from_hex(text: &str) -> Result<[u8; 32], UpdateError> {
    unhex32(text.trim().as_bytes()).ok_or(UpdateError::BadKey)
}

/// Whether a check may run now (SR-45): never sooner than [`CHECK_EVERY_MS`] after the last
/// check that got an answer, and never sooner than [`RETRY_AFTER_FAILURE_MS`] after the last
/// one that failed. A timestamp in the future (the clock was set back) counts as "long ago",
/// so a wrong clock cannot switch checks off for good.
pub fn check_due(last_answer_ms: Option<u64>, last_failure_ms: Option<u64>, now_ms: u64) -> bool {
    let waited = |since: Option<u64>, wait: u64| match since {
        None => true,
        Some(at) if at > now_ms => true,
        Some(at) => now_ms - at >= wait,
    };
    waited(last_answer_ms, CHECK_EVERY_MS) && waited(last_failure_ms, RETRY_AFTER_FAILURE_MS)
}

/// Whether an update request may follow a redirect to `url` (SR-48): HTTPS only, default
/// port only, no user info, and the host is `github.com` or ends in `.githubusercontent.com`.
pub fn redirect_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains(['@', ':', '[', ']', '\\']) {
        return false;
    }
    let plain = authority
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
    plain
        && !authority.starts_with('.')
        && !authority.contains("..")
        && (authority == "github.com" || authority.ends_with(".githubusercontent.com"))
}

fn decode_signature(sig: &[u8]) -> Result<ed25519_dalek::Signature, UpdateError> {
    let text = std::str::from_utf8(sig).map_err(|_| UpdateError::BadSignatureEncoding)?;
    let raw = STANDARD
        .decode(text.trim())
        .map_err(|_| UpdateError::BadSignatureEncoding)?;
    ed25519_dalek::Signature::from_slice(&raw).map_err(|_| UpdateError::BadSignatureEncoding)
}

/// The line parser without the signature check, for the fuzz target only.
#[cfg(fuzzing)]
pub(crate) fn parse_sums_for_fuzzing(sums: &[u8]) -> Option<SignedSums> {
    if sums.len() > MAX_SUMS_LEN {
        return None;
    }
    parse_sums(sums).ok()
}

/// Parses `sha256sum` output: `<64 lowercase hex><space><space or *><name>` per line.
fn parse_sums(sums: &[u8]) -> Result<SignedSums, UpdateError> {
    let text = std::str::from_utf8(sums).map_err(|_| UpdateError::Malformed("not UTF-8"))?;
    let mut entries: Vec<(String, [u8; 32])> = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        if entries.len() == MAX_ENTRIES {
            return Err(UpdateError::Malformed("too many lines"));
        }
        let bytes = line.as_bytes();
        if bytes.len() < 67 || bytes[64] != b' ' || !(bytes[65] == b' ' || bytes[65] == b'*') {
            return Err(UpdateError::Malformed("not a sha256sum line"));
        }
        let sum = unhex32_lower(&bytes[..64]).ok_or(UpdateError::Malformed("bad hash"))?;
        let name = &line[66..];
        if !name_ok(name) {
            return Err(UpdateError::Malformed("bad file name"));
        }
        if entries.iter().any(|(n, _)| n == name) {
            return Err(UpdateError::Malformed("a file is listed twice"));
        }
        entries.push((name.to_owned(), sum));
    }
    let version = version_of(&entries)?;
    Ok(SignedSums { version, entries })
}

/// A release file name: letters, digits, dot, dash and underscore, not starting with a dot
/// or dash, so it can never be a path or an option.
fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        && !name.starts_with(['.', '-'])
}

/// What follows `playdrop-<version>-` in a release file name. Exactly these, so a name such
/// as `playdrop-0.11.0-rc.1-linux-amd64.deb` cannot be read as release 0.11.0.
const ASSET_SUFFIXES: &[&str] = &[
    "android.apk",
    "ios-unsigned.ipa",
    "linux-amd64.deb",
    "linux-x86_64.flatpak",
    "windows-x64-setup.exe",
];
/// What follows `playdrop-cosmic-applet-<version>-`.
const APPLET_SUFFIXES: &[&str] = &["linux-amd64.deb"];

/// The version every `playdrop-...` file name carries. At least one name must carry one and
/// all that do must agree, so a file from another release cannot ride along. A `playdrop-`
/// name whose remainder after `<version>-` is not a known release file fails the whole
/// file: the release run never writes one, so it can only be a mistake or a forgery.
fn version_of(entries: &[(String, [u8; 32])]) -> Result<Version, UpdateError> {
    let mut found: Option<Version> = None;
    for (name, _) in entries {
        let (rest, suffixes) = match name.strip_prefix("playdrop-cosmic-applet-") {
            Some(rest) => (rest, APPLET_SUFFIXES),
            None => match name.strip_prefix("playdrop-") {
                Some(rest) => (rest, ASSET_SUFFIXES),
                None => continue,
            },
        };
        // A version never holds a `-`, so the first one ends it.
        let Some((text, suffix)) = rest.split_once('-') else {
            return Err(UpdateError::NoVersion);
        };
        if !suffixes.contains(&suffix) {
            return Err(UpdateError::NoVersion);
        }
        let version = Version::parse(text).map_err(|_| UpdateError::NoVersion)?;
        match found {
            None => found = Some(version),
            Some(seen) if seen == version => {}
            Some(_) => return Err(UpdateError::NoVersion),
        }
    }
    found.ok_or(UpdateError::NoVersion)
}

fn nibble(b: u8, lower_only: bool) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' if !lower_only => Some(b - b'A' + 10),
        _ => None,
    }
}

fn unhex(bytes: &[u8], lower_only: bool) -> Option<[u8; 32]> {
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = (nibble(bytes[2 * i], lower_only)? << 4) | nibble(bytes[2 * i + 1], lower_only)?;
    }
    Some(out)
}

fn unhex32_lower(bytes: &[u8]) -> Option<[u8; 32]> {
    unhex(bytes, true)
}

fn unhex32(bytes: &[u8]) -> Option<[u8; 32]> {
    unhex(bytes, false)
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};

    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn sign(sums: &[u8]) -> Vec<u8> {
        let mut out = STANDARD.encode(key().sign(sums).to_bytes()).into_bytes();
        out.push(b'\n');
        out
    }

    fn keys() -> Vec<[u8; 32]> {
        vec![key().verifying_key().to_bytes()]
    }

    fn sums_for(version: &str) -> Vec<u8> {
        let names = [
            format!("playdrop-{version}-android.apk"),
            format!("playdrop-{version}-ios-unsigned.ipa"),
            format!("playdrop-{version}-linux-amd64.deb"),
            format!("playdrop-{version}-linux-x86_64.flatpak"),
            format!("playdrop-{version}-windows-x64-setup.exe"),
            format!("playdrop-cosmic-applet-{version}-linux-amd64.deb"),
        ];
        let mut out = String::new();
        for name in names {
            out.push_str(&hex(Sha256::digest(name.as_bytes()).as_slice()));
            out.push_str("  ");
            out.push_str(&name);
            out.push('\n');
        }
        out.into_bytes()
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn a_signed_newer_release_is_offered_with_its_file() {
        let sums = sums_for("0.11.0");
        let signed = verify_sums_with(&sums, &sign(&sums), &keys()).unwrap();
        assert_eq!(signed.version(), v("0.11.0"));
        let offer = signed.offer(v("0.10.0"), InstallKind::Deb).unwrap();
        assert_eq!(offer.version, v("0.11.0"));
        assert_eq!(
            offer.page_url,
            "https://github.com/QasimBhb/PlayDrop/releases/tag/v0.11.0"
        );
        let asset = offer.asset.unwrap();
        assert_eq!(asset.name, "playdrop-0.11.0-linux-amd64.deb");
        assert_eq!(
            asset.url,
            "https://github.com/QasimBhb/PlayDrop/releases/download/v0.11.0/playdrop-0.11.0-linux-amd64.deb"
        );
        assert!(asset.matches_bytes(b"playdrop-0.11.0-linux-amd64.deb"));
        assert!(!asset.matches_bytes(b"something else"));
        assert_eq!(asset.sha256_hex().len(), 64);
        assert!(signed.applet_asset().is_some());
    }

    #[test]
    fn every_install_kind_gets_its_own_file_or_a_notice() {
        let sums = sums_for("1.2.3");
        let signed = verify_sums_with(&sums, &sign(&sums), &keys()).unwrap();
        let name = |kind| {
            signed
                .offer(v("1.2.2"), kind)
                .unwrap()
                .asset
                .map(|a| a.name)
        };
        assert_eq!(
            name(InstallKind::Windows).as_deref(),
            Some("playdrop-1.2.3-windows-x64-setup.exe")
        );
        assert_eq!(
            name(InstallKind::Android).as_deref(),
            Some("playdrop-1.2.3-android.apk")
        );
        assert_eq!(name(InstallKind::Flatpak), None);
        assert_eq!(name(InstallKind::Ios), None);
        assert_eq!(name(InstallKind::Other), None);
    }

    #[test]
    fn the_same_or_an_older_release_is_never_offered() {
        let sums = sums_for("0.10.0");
        let signed = verify_sums_with(&sums, &sign(&sums), &keys()).unwrap();
        assert_eq!(signed.offer(v("0.10.0"), InstallKind::Deb), None);
        assert_eq!(signed.offer(v("0.10.1"), InstallKind::Deb), None);
        assert_eq!(signed.offer(v("1.0.0"), InstallKind::Android), None);
        assert!(signed.offer(v("0.9.9"), InstallKind::Deb).is_some());
    }

    #[test]
    fn a_changed_byte_anywhere_is_refused() {
        let sums = sums_for("0.11.0");
        let sig = sign(&sums);
        for i in 0..sums.len() {
            let mut bad = sums.clone();
            bad[i] ^= 1;
            assert_eq!(
                verify_sums_with(&bad, &sig, &keys()),
                Err(UpdateError::NotSigned),
                "byte {i}"
            );
        }
        let mut bad_sig = STANDARD.decode(sig.trim_ascii()).unwrap();
        bad_sig[10] ^= 1;
        let bad_sig = STANDARD.encode(bad_sig).into_bytes();
        assert_eq!(
            verify_sums_with(&sums, &bad_sig, &keys()),
            Err(UpdateError::NotSigned)
        );
    }

    #[test]
    fn another_key_and_the_pinned_list_refuse_a_test_signature() {
        let sums = sums_for("0.11.0");
        let sig = sign(&sums);
        let other = SigningKey::from_bytes(&[9u8; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            verify_sums_with(&sums, &sig, &[other]),
            Err(UpdateError::NotSigned)
        );
        assert_eq!(verify_sums(&sums, &sig), Err(UpdateError::NotSigned));
        assert_eq!(
            verify_sums_with(&sums, &sig, &[]),
            Err(UpdateError::NotSigned)
        );
        // Any key on the list is enough (rotation).
        assert!(verify_sums_with(&sums, &sig, &[other, keys()[0]]).is_ok());
    }

    #[test]
    fn a_signature_that_is_not_one_is_refused_before_anything_else() {
        let sums = sums_for("0.11.0");
        for sig in [&b""[..], b"not base64 !!", b"AAAA", &[0xff, 0xfe][..]] {
            assert_eq!(
                verify_sums_with(&sums, sig, &keys()),
                Err(UpdateError::BadSignatureEncoding)
            );
        }
    }

    #[test]
    fn oversized_answers_are_refused() {
        let big = vec![b'a'; MAX_SUMS_LEN + 1];
        assert_eq!(
            verify_sums_with(&big, &sign(&big), &keys()),
            Err(UpdateError::TooLarge)
        );
        let sums = sums_for("0.11.0");
        let big_sig = vec![b'A'; MAX_SIG_LEN + 1];
        assert_eq!(
            verify_sums_with(&sums, &big_sig, &keys()),
            Err(UpdateError::TooLarge)
        );
    }

    fn signed_err(text: &str) -> UpdateError {
        verify_sums_with(text.as_bytes(), &sign(text.as_bytes()), &keys()).unwrap_err()
    }

    #[test]
    fn signed_but_malformed_files_are_refused() {
        use fmt::Write as _;

        let h = "a".repeat(64);
        assert!(matches!(
            signed_err("hello\n"),
            UpdateError::Malformed("not a sha256sum line")
        ));
        assert!(matches!(
            signed_err(&format!(
                "{}  playdrop-0.11.0-android.apk\n",
                "A".repeat(64)
            )),
            UpdateError::Malformed("bad hash")
        ));
        for name in ["../x", "a/b", "-rf", ".hidden", "a b", "a\tb", ""] {
            assert!(
                matches!(
                    signed_err(&format!("{h}  {name}\n")),
                    UpdateError::Malformed(_)
                ),
                "{name:?}"
            );
        }
        assert!(matches!(
            signed_err(&format!(
                "{h}  playdrop-0.11.0-android.apk\n{h}  playdrop-0.11.0-android.apk\n"
            )),
            UpdateError::Malformed("a file is listed twice")
        ));
        let mut many = String::new();
        for i in 0..=MAX_ENTRIES {
            let _ = writeln!(many, "{h}  playdrop-0.11.0-f{i}.bin");
        }
        assert!(matches!(
            signed_err(&many),
            UpdateError::Malformed("too many lines")
        ));
    }

    #[test]
    fn the_names_must_agree_on_one_version() {
        let h = "b".repeat(64);
        assert_eq!(signed_err(""), UpdateError::NoVersion);
        assert_eq!(
            signed_err(&format!("{h}  README.txt\n")),
            UpdateError::NoVersion
        );
        assert_eq!(
            signed_err(&format!(
                "{h}  playdrop-0.11.0-android.apk\n{h}  playdrop-0.12.0-linux-amd64.deb\n"
            )),
            UpdateError::NoVersion
        );
        assert_eq!(
            signed_err(&format!("{h}  playdrop-0.11.0rc1-android.apk\n")),
            UpdateError::NoVersion
        );
        assert_eq!(
            signed_err(&format!("{h}  playdrop-0.11.0.apk\n")),
            UpdateError::NoVersion
        );
        // A pre-release name must not read as the release it starts with, and one such
        // name fails the whole file even beside good ones.
        for bad in [
            "playdrop-0.11.0-rc.1-linux-amd64.deb",
            "playdrop-0.11.0-rc1-android.apk",
            "playdrop-0.11.0-linux-arm64.deb",
            "playdrop-0.11.0-android.apk.sig",
            "playdrop-0.11.0-",
            "playdrop-cosmic-applet-0.11.0-rc.1-linux-amd64.deb",
            "playdrop-cosmic-applet-0.11.0-android.apk",
        ] {
            assert_eq!(
                signed_err(&format!("{h}  playdrop-0.11.0-android.apk\n{h}  {bad}\n")),
                UpdateError::NoVersion,
                "{bad}"
            );
        }
        // Every name an install kind asks for is one the parser accepts.
        for kind in [InstallKind::Deb, InstallKind::Windows, InstallKind::Android] {
            let name = kind.asset_name(v("3.4.5")).unwrap();
            let text = format!("{h}  {name}\n");
            let signed =
                verify_sums_with(text.as_bytes(), &sign(text.as_bytes()), &keys()).unwrap();
            assert_eq!(signed.version(), v("3.4.5"), "{name}");
        }
        let text = format!("{h}  {}\n", applet_asset_name(v("3.4.5")));
        assert!(verify_sums_with(text.as_bytes(), &sign(text.as_bytes()), &keys()).is_ok());
        // The binary-mode marker and CRLF line ends are still sha256sum output.
        let text = format!("{h} *playdrop-2.0.0-android.apk\r\n");
        let signed = verify_sums_with(text.as_bytes(), &sign(text.as_bytes()), &keys()).unwrap();
        assert_eq!(signed.version(), v("2.0.0"));
    }

    #[test]
    fn versions_parse_strictly_and_order_by_number() {
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(v("0.11.0").to_string(), "0.11.0");
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "v1.2.3",
            "1.2.3-rc1",
            "01.2.3",
            "1.02.3",
            " 1.2.3",
            "1.2.3 ",
            "+1.2.3",
            "1..3",
            "1.2.9999999999",
        ] {
            assert_eq!(Version::parse(bad), Err(UpdateError::BadVersion), "{bad:?}");
        }
        // A plain release build knows its version; a pre-release build has none, so it is
        // offered nothing rather than everything.
        match Version::current() {
            Some(current) => assert_eq!(current.to_string(), env!("CARGO_PKG_VERSION")),
            None => assert!(Version::parse(env!("CARGO_PKG_VERSION")).is_err()),
        }
    }

    #[test]
    fn a_check_runs_once_a_day_and_retries_a_failure_after_an_hour() {
        let day = CHECK_EVERY_MS;
        let hour = RETRY_AFTER_FAILURE_MS;
        let now = 10 * day;
        assert!(check_due(None, None, now));
        assert!(!check_due(Some(now - day + 1), None, now));
        assert!(check_due(Some(now - day), None, now));
        assert!(!check_due(None, Some(now - hour + 1), now));
        assert!(check_due(None, Some(now - hour), now));
        assert!(!check_due(Some(now - 2 * day), Some(now - 1), now));
        assert!(!check_due(Some(now - 1), Some(now - 2 * hour), now));
        // A clock set back must not silence the check for good.
        assert!(check_due(Some(now + day), Some(now + day), now));
    }

    #[test]
    fn redirects_stay_on_github_over_https() {
        for ok in [
            "https://github.com/QasimBhb/PlayDrop/releases/download/v0.11.0/SHA256SUMS",
            "https://objects.githubusercontent.com/github-production-release-asset/1?x=y",
            "https://release-assets.githubusercontent.com/a/b",
            "https://github.com",
        ] {
            assert!(redirect_allowed(ok), "{ok}");
        }
        for bad in [
            "http://github.com/x",
            "https://evil.example/github.com",
            "https://github.com.evil.example/x",
            "https://githubusercontent.com/x",
            "https://evilgithubusercontent.com/x",
            "https://github.com:8443/x",
            "https://user@github.com/x",
            "https://github.com@evil.example/x",
            "https://GITHUB.com/x",
            "https://.githubusercontent.com/x",
            "https://a..githubusercontent.com/x",
            "https:///x",
            "https://",
            "ftp://github.com/x",
            "//github.com/x",
            "",
        ] {
            assert!(!redirect_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn test_keys_parse_from_hex() {
        let hex_key = hex(&keys()[0]);
        assert_eq!(key_from_hex(&hex_key), Ok(keys()[0]));
        assert_eq!(key_from_hex(&hex_key.to_uppercase()), Ok(keys()[0]));
        assert_eq!(key_from_hex("abc"), Err(UpdateError::BadKey));
        assert_eq!(key_from_hex(&"g".repeat(64)), Err(UpdateError::BadKey));
    }

    /// A checksum file signed with the real release key the way the release run does it
    /// (`openssl pkeyutl -sign -rawin`, then base64). Proves the pinned key is the public
    /// half of that key and that the release run's format is the one read here.
    #[test]
    fn a_signature_made_by_the_release_key_with_openssl_verifies() {
        let sums = b"86addef47ef3d897d2eb81e0dcc2f91634bf88b0746abfddbb34fb868fcd6f78  playdrop-0.0.1-android.apk\n";
        let sig = b"bycXfEqXXfYSZuDUHxVomFQ4qjt4F9Y6T8Y6ZlZiUInE3EL3nGp1Oa917nfrDdQoQ0pheiDlE5xiZud/8BpUAw==";
        let signed = verify_sums(sums, sig).unwrap();
        assert_eq!(signed.version(), v("0.0.1"));
        assert!(signed
            .asset("playdrop-0.0.1-android.apk")
            .unwrap()
            .matches_bytes(b"test vector"));
    }

    /// `docs/release/playdrop-update.pub` is what the release run verifies its own signature
    /// against before publishing; it must be a key this code trusts, or a release could be
    /// published that no install accepts.
    #[test]
    fn the_committed_public_key_file_is_a_pinned_key() {
        let pem = include_str!("../../../docs/release/playdrop-update.pub");
        let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let der = STANDARD.decode(body).unwrap();
        // SubjectPublicKeyInfo for Ed25519 is a fixed 12-byte prefix and the 32-byte key.
        assert_eq!(der.len(), 44);
        let key: [u8; 32] = der[12..].try_into().unwrap();
        assert!(TRUSTED_KEYS.contains(&key));
    }

    #[test]
    fn the_pinned_key_is_the_one_in_the_design_note() {
        assert_eq!(
            hex(&TRUSTED_KEYS[0]),
            "3dc196f08c776f693ffbc69608aab9b3118602f58dfde914923871f954357ff8"
        );
    }
}

//! The update check for the Android and iPhone apps (`docs/design/update-check.md`, SR-45
//! to SR-48; ffi 0.6.0).
//!
//! The apps fetch `SHA256SUMS` and `SHA256SUMS.sig` with their own HTTP client and hand the
//! bytes here; every decision is `playdrop_core::update`'s: the URLs, the `User-Agent`, the
//! limits, the redirect rule, the once-a-day rule, the signature, the newer-than rule and
//! the file hash. Nothing in this module touches the network, a clock or a file, so the
//! same answers come out on every platform.

use playdrop_core::update::{self as core, InstallKind, UpdateError, Version};

/// How this copy of `PlayDrop` was installed, which picks the release file an update
/// downloads. The Android app passes `Android`, the iPhone app `Ios`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum UpdateInstallKind {
    /// The `.deb` on amd64.
    Deb,
    /// The Windows installer.
    Windows,
    /// The Android APK.
    Android,
    /// The Flatpak: notice only.
    Flatpak,
    /// The iPhone app: notice only.
    Ios,
    /// Anything else: notice only.
    Other,
}

impl From<UpdateInstallKind> for InstallKind {
    fn from(k: UpdateInstallKind) -> Self {
        match k {
            UpdateInstallKind::Deb => Self::Deb,
            UpdateInstallKind::Windows => Self::Windows,
            UpdateInstallKind::Android => Self::Android,
            UpdateInstallKind::Flatpak => Self::Flatpak,
            UpdateInstallKind::Ios => Self::Ios,
            UpdateInstallKind::Other => Self::Other,
        }
    }
}

/// One release file to download, with the hash it must have.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct UpdateAsset {
    /// File name, exactly as signed.
    pub name: String,
    /// Where to download it (GitHub Releases).
    pub url: String,
    /// The signed SHA-256 of the file, 64 lowercase hex characters. Check a download with
    /// [`update_asset_matches`].
    pub sha256_hex: String,
}

/// A newer release to offer the user.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct UpdateOffer {
    /// The newer version, `major.minor.patch`.
    pub version: String,
    /// The release page.
    pub page_url: String,
    /// The file to download, or `None` when this install only shows a notice (iPhone) or
    /// the release does not list its file.
    pub asset: Option<UpdateAsset>,
}

impl From<core::Offer> for UpdateOffer {
    fn from(o: core::Offer) -> Self {
        Self {
            version: o.version.to_string(),
            page_url: o.page_url,
            asset: o.asset.map(|a| UpdateAsset {
                sha256_hex: a.sha256_hex(),
                name: a.name,
                url: a.url,
            }),
        }
    }
}

/// Why an update check was refused. The message is fit to show; nothing is offered after
/// any of these. Kotlin sees `UpdateCheckException.<Variant>`, Swift `UpdateCheckError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum UpdateCheckError {
    /// An answer was larger than its limit (SR-48).
    #[error("the update answer is larger than allowed")]
    TooLarge,
    /// The signature file is not a signature.
    #[error("the update signature could not be read")]
    BadSignatureEncoding,
    /// No `PlayDrop` release key signed the checksum file (SR-46).
    #[error("the update could not be verified: it is not signed by a PlayDrop release key")]
    NotSigned,
    /// The checksum file is signed but malformed.
    #[error("the update list is malformed: {detail}")]
    Malformed {
        /// What is wrong with it.
        detail: String,
    },
    /// The checksum file does not name one release version.
    #[error("the update list does not name one release version")]
    NoVersion,
    /// The app's own version is not a plain release (`major.minor.patch`), for example a
    /// pre-release `0.11.0-rc.1`: such a build has no update check. Pass the app's real
    /// version string, never one with its suffix cut off, so this answer can be given; a
    /// UI shows this message in Settings ([`update_check_available`] tells beforehand).
    #[error("This is not a release build, so it does not check for updates.")]
    BadCurrentVersion,
    /// The test key is not 64 hex characters.
    #[error("the test key is not an Ed25519 public key in hex")]
    BadTestKey,
}

impl From<UpdateError> for UpdateCheckError {
    fn from(e: UpdateError) -> Self {
        match e {
            UpdateError::TooLarge => Self::TooLarge,
            UpdateError::BadSignatureEncoding => Self::BadSignatureEncoding,
            UpdateError::NotSigned => Self::NotSigned,
            UpdateError::Malformed(detail) => Self::Malformed {
                detail: detail.to_owned(),
            },
            UpdateError::NoVersion => Self::NoVersion,
            UpdateError::BadVersion => Self::BadCurrentVersion,
            UpdateError::BadKey => Self::BadTestKey,
        }
    }
}

/// Whether an app running `current_version` has an update check at all: only a plain
/// release version (`major.minor.patch`) does. Ask this before any request, so a
/// pre-release or test build never contacts GitHub; when it is false, Settings shows
/// [`update_not_a_release_text`].
#[uniffi::export]
#[allow(
    clippy::needless_pass_by_value,
    reason = "UniFFI hands exported functions owned values"
)]
pub fn update_check_available(current_version: String) -> bool {
    Version::parse(&current_version).is_ok()
}

/// The sentence for Settings when [`update_check_available`] is false.
#[uniffi::export]
pub fn update_not_a_release_text() -> String {
    core::NOT_A_RELEASE.to_owned()
}

/// The checksum file of the latest release: the first of the two requests.
#[uniffi::export]
pub fn update_sums_url() -> String {
    core::SUMS_URL.to_owned()
}

/// The signature over the checksum file: the second request.
#[uniffi::export]
pub fn update_sig_url() -> String {
    core::SIG_URL.to_owned()
}

/// The only `User-Agent` an update request may carry. Send no other identifying header,
/// no cookie and no cache validator (SR-45).
#[uniffi::export]
pub fn update_user_agent() -> String {
    core::USER_AGENT.to_owned()
}

/// Largest checksum file to read, in bytes; stop reading past it (SR-48).
#[uniffi::export]
pub fn update_max_sums_len() -> u64 {
    core::MAX_SUMS_LEN as u64
}

/// Largest signature file to read, in bytes (SR-48).
#[uniffi::export]
pub fn update_max_sig_len() -> u64 {
    core::MAX_SIG_LEN as u64
}

/// Largest release file to download, in bytes (SR-48).
#[uniffi::export]
pub fn update_max_download_len() -> u64 {
    core::MAX_DOWNLOAD_LEN
}

/// Most redirects to follow for one request; each must pass [`update_redirect_allowed`].
#[uniffi::export]
pub fn update_max_redirects() -> u32 {
    core::MAX_REDIRECTS
}

/// Time limit for each check request, in milliseconds (SR-48).
#[uniffi::export]
pub fn update_request_timeout_ms() -> u64 {
    core::REQUEST_TIMEOUT_MS
}

/// Whether a check may run now: at most once a day after one that got an answer, and an
/// hour after one that failed (SR-45). Times are Unix milliseconds; "Check now" skips this
/// but never the settings switch.
#[uniffi::export]
pub fn update_check_due(
    last_answer_ms: Option<u64>,
    last_failure_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    core::check_due(last_answer_ms, last_failure_ms, now_ms)
}

/// Whether a request may follow a redirect to `url`: HTTPS on the default port to
/// `github.com` or a host under `.githubusercontent.com` only (SR-48).
#[uniffi::export]
#[allow(
    clippy::needless_pass_by_value,
    reason = "UniFFI hands exported functions owned values"
)]
pub fn update_redirect_allowed(url: String) -> bool {
    core::redirect_allowed(&url)
}

/// What to offer: `sums` and `sig` are the bytes of the two files, `current_version` the
/// app's own version exactly as built (Android `versionName`, iOS
/// `CFBundleShortVersionString`), suffix and all. Returns `None` when the release is not
/// newer, an offer when it is, and an error when the files fail verification or the app's
/// version is not a plain release ([`UpdateCheckError::BadCurrentVersion`]: this build has
/// no update check; never "offer everything"). Only the release keys built into this
/// library are trusted.
#[uniffi::export]
#[allow(
    clippy::needless_pass_by_value,
    reason = "UniFFI hands exported functions owned values"
)]
pub fn update_offer(
    sums: Vec<u8>,
    sig: Vec<u8>,
    current_version: String,
    kind: UpdateInstallKind,
) -> Result<Option<UpdateOffer>, UpdateCheckError> {
    offer_with_keys(&sums, &sig, &current_version, kind, core::TRUSTED_KEYS)
}

/// [`update_offer`] trusting one more key, `test_key_hex` (64 hex characters), next to the
/// release keys. It exists only so a debug build of an app can be tested against a stand-in
/// server signed with a throwaway key (`docs/design/update-check.md` section 8).
///
/// Only in a library built with the cargo feature `update-test-key`, which the Android
/// debug build type turns on and nothing else does: a release library has no such function,
/// so no release app can call it, by mistake or otherwise.
#[cfg(feature = "update-test-key")]
#[uniffi::export]
#[allow(
    clippy::needless_pass_by_value,
    reason = "UniFFI hands exported functions owned values"
)]
pub fn update_offer_with_test_key(
    sums: Vec<u8>,
    sig: Vec<u8>,
    current_version: String,
    kind: UpdateInstallKind,
    test_key_hex: String,
) -> Result<Option<UpdateOffer>, UpdateCheckError> {
    let mut keys = core::TRUSTED_KEYS.to_vec();
    keys.push(core::key_from_hex(&test_key_hex)?);
    offer_with_keys(&sums, &sig, &current_version, kind, &keys)
}

/// Whether `sha256`, the SHA-256 the app computed over the whole downloaded file, is the
/// signed one for `asset` (SR-46). Anything else means: delete the file and refuse.
#[uniffi::export]
#[allow(
    clippy::needless_pass_by_value,
    reason = "UniFFI hands exported functions owned values"
)]
pub fn update_asset_matches(asset: UpdateAsset, sha256: Vec<u8>) -> bool {
    use std::fmt::Write as _;
    if sha256.len() != 32 {
        return false;
    }
    let got = sha256.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    });
    // The signed value is lowercase hex from the core; not secret, so a plain comparison.
    got == asset.sha256_hex
}

/// The current version is parsed before the signature is checked, so a bad app version is
/// reported as such even when the network answer is also bad.
fn offer_with_keys(
    sums: &[u8],
    sig: &[u8],
    current_version: &str,
    kind: UpdateInstallKind,
    keys: &[[u8; 32]],
) -> Result<Option<UpdateOffer>, UpdateCheckError> {
    let current = Version::parse(current_version)?;
    let signed = core::verify_sums_with(sums, sig, keys)?;
    Ok(signed.offer(current, kind.into()).map(Into::into))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signed with the real release key (the vector in `playdrop_core::update`'s tests).
    const REAL_SUMS: &[u8] = b"86addef47ef3d897d2eb81e0dcc2f91634bf88b0746abfddbb34fb868fcd6f78  playdrop-0.0.1-android.apk\n";
    const REAL_SIG: &[u8] =
        b"bycXfEqXXfYSZuDUHxVomFQ4qjt4F9Y6T8Y6ZlZiUInE3EL3nGp1Oa917nfrDdQoQ0pheiDlE5xiZud/8BpUAw==";

    /// Signed on 2026-11-12 with a throwaway Ed25519 key made by `openssl genpkey` for this
    /// test only (the private half was not kept). Each hash is SHA-256 of the extension.
    #[cfg(feature = "update-test-key")]
    const TEST_SUMS: &[u8] = b"dd37c2d7274f7ea982cb83390c36918fee9ce8889073c44b68cdc00bdb8c3e04  playdrop-9.8.7-android.apk\n\
78324857e8d9bfa749dc301271df54a6572de9f4c3df8a9507cfa7b7d2b25f8e  playdrop-9.8.7-ios-unsigned.ipa\n\
9cfa1468c93fc18652e34a000f0c6614b0fa18f6f4887477ad9b0d36ca6a7eaa  playdrop-9.8.7-linux-amd64.deb\n";
    #[cfg(feature = "update-test-key")]
    const TEST_SIG: &[u8] =
        b"UY3u8f+z9xUOFG8UXbtRV3F/WZlr/T/CW635RA1O4ZszUWzGTCpk6f3CdP0GXG4rrz4+Cr93FGWg2qTTCvllAw==";
    #[cfg(feature = "update-test-key")]
    const TEST_KEY: &str = "16acc86423c3d31fe1c6255e267cdd1c646b1d1747c5b32f3c3cf939bb595cba";

    fn offer(
        sums: &[u8],
        sig: &[u8],
        current: &str,
        kind: UpdateInstallKind,
    ) -> Result<Option<UpdateOffer>, UpdateCheckError> {
        update_offer(sums.to_vec(), sig.to_vec(), current.into(), kind)
    }

    #[cfg(feature = "update-test-key")]
    fn offer_test_key(
        current: &str,
        kind: UpdateInstallKind,
        key: &str,
    ) -> Result<Option<UpdateOffer>, UpdateCheckError> {
        update_offer_with_test_key(
            TEST_SUMS.to_vec(),
            TEST_SIG.to_vec(),
            current.into(),
            kind,
            key.into(),
        )
    }

    #[test]
    fn a_release_signed_by_the_pinned_key_is_offered_with_its_file() {
        let o = offer(REAL_SUMS, REAL_SIG, "0.0.0", UpdateInstallKind::Android)
            .unwrap()
            .unwrap();
        assert_eq!(o.version, "0.0.1");
        assert_eq!(
            o.page_url,
            "https://github.com/QasimBhb/PlayDrop/releases/tag/v0.0.1"
        );
        let asset = o.asset.unwrap();
        assert_eq!(
            asset,
            UpdateAsset {
                name: "playdrop-0.0.1-android.apk".into(),
                url: "https://github.com/QasimBhb/PlayDrop/releases/download/v0.0.1/playdrop-0.0.1-android.apk".into(),
                sha256_hex: "86addef47ef3d897d2eb81e0dcc2f91634bf88b0746abfddbb34fb868fcd6f78".into(),
            }
        );
        // The same or a newer app gets nothing; the iPhone gets a notice without a file.
        assert_eq!(
            offer(REAL_SUMS, REAL_SIG, "0.0.1", UpdateInstallKind::Android),
            Ok(None)
        );
        assert_eq!(
            offer(REAL_SUMS, REAL_SIG, "0.11.0", UpdateInstallKind::Android),
            Ok(None)
        );
        let ios = offer(REAL_SUMS, REAL_SIG, "0.0.0", UpdateInstallKind::Ios)
            .unwrap()
            .unwrap();
        assert_eq!(ios.asset, None);
    }

    #[test]
    fn errors_map_to_their_own_variants() {
        let mut bad = REAL_SUMS.to_vec();
        bad[0] ^= 1;
        assert_eq!(
            offer(&bad, REAL_SIG, "0.0.0", UpdateInstallKind::Android),
            Err(UpdateCheckError::NotSigned)
        );
        assert_eq!(
            offer(REAL_SUMS, b"%%%", "0.0.0", UpdateInstallKind::Android),
            Err(UpdateCheckError::BadSignatureEncoding)
        );
        let big = vec![b'a'; core::MAX_SUMS_LEN + 1];
        assert_eq!(
            offer(&big, REAL_SIG, "0.0.0", UpdateInstallKind::Android),
            Err(UpdateCheckError::TooLarge)
        );
        // An app version that is not a plain release is "no update check on this build",
        // never "offer everything", and the apps can tell before asking anyone.
        for v in [
            "",
            "0.11",
            "0.11.0-debug",
            "0.11.0-rc.1",
            "v0.11.0",
            " 0.11.0",
        ] {
            assert_eq!(
                offer(REAL_SUMS, REAL_SIG, v, UpdateInstallKind::Android),
                Err(UpdateCheckError::BadCurrentVersion),
                "{v:?}"
            );
            assert!(!update_check_available(v.into()), "{v:?}");
        }
        assert!(update_check_available("0.11.0".into()));
        assert_eq!(
            UpdateCheckError::BadCurrentVersion.to_string(),
            update_not_a_release_text()
        );
        assert_eq!(
            UpdateCheckError::from(UpdateError::Malformed("bad hash")),
            UpdateCheckError::Malformed {
                detail: "bad hash".into()
            }
        );
        assert_eq!(
            UpdateCheckError::from(UpdateError::NoVersion),
            UpdateCheckError::NoVersion
        );
        // Every message is a sentence a UI can show.
        assert_eq!(
            UpdateCheckError::Malformed {
                detail: "bad hash".into()
            }
            .to_string(),
            "the update list is malformed: bad hash"
        );
    }

    #[cfg(feature = "update-test-key")]
    #[test]
    fn only_the_test_key_entry_point_trusts_the_test_key() {
        assert_eq!(
            offer(TEST_SUMS, TEST_SIG, "0.11.0", UpdateInstallKind::Android),
            Err(UpdateCheckError::NotSigned)
        );
        let o = offer_test_key("0.11.0", UpdateInstallKind::Android, TEST_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(o.version, "9.8.7");
        assert_eq!(o.asset.unwrap().name, "playdrop-9.8.7-android.apk");
        // Upper case hex is fine; anything else is not a key.
        assert!(offer_test_key(
            "0.11.0",
            UpdateInstallKind::Android,
            &TEST_KEY.to_uppercase()
        )
        .is_ok());
        assert_eq!(
            offer_test_key("0.11.0", UpdateInstallKind::Android, "abc"),
            Err(UpdateCheckError::BadTestKey)
        );
        // The pinned keys still count next to the test key.
        assert!(update_offer_with_test_key(
            REAL_SUMS.to_vec(),
            REAL_SIG.to_vec(),
            "0.0.0".into(),
            UpdateInstallKind::Android,
            TEST_KEY.into()
        )
        .unwrap()
        .is_some());
    }

    #[test]
    fn a_download_matches_only_its_signed_hash() {
        let asset = offer(REAL_SUMS, REAL_SIG, "0.0.0", UpdateInstallKind::Android)
            .unwrap()
            .unwrap()
            .asset
            .unwrap();
        // The signed hash, as bytes.
        let mut digest = [0u8; 32];
        for (i, b) in digest.iter_mut().enumerate() {
            *b = u8::from_str_radix(&asset.sha256_hex[2 * i..2 * i + 2], 16).unwrap();
        }
        assert!(update_asset_matches(asset.clone(), digest.to_vec()));
        let mut other = digest;
        other[31] ^= 1;
        assert!(!update_asset_matches(asset.clone(), other.to_vec()));
        assert!(!update_asset_matches(asset, digest[..31].to_vec()));
    }

    #[test]
    fn the_fixed_request_facts_come_from_the_core() {
        assert_eq!(
            update_sums_url(),
            "https://github.com/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS"
        );
        assert_eq!(
            update_sig_url(),
            "https://github.com/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS.sig"
        );
        assert_eq!(update_user_agent(), "PlayDrop");
        assert_eq!(update_max_sums_len(), 64 * 1024);
        assert_eq!(update_max_sig_len(), 1024);
        assert_eq!(update_max_download_len(), 512 * 1024 * 1024);
        assert_eq!(update_max_redirects(), 5);
        assert_eq!(update_request_timeout_ms(), 15_000);
        let day = 24 * 60 * 60 * 1000;
        assert!(update_check_due(None, None, day));
        assert!(!update_check_due(Some(day), None, day + 1));
        assert!(update_redirect_allowed(
            "https://objects.githubusercontent.com/x".into()
        ));
        assert!(!update_redirect_allowed("http://github.com/x".into()));
    }
}

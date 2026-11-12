//! A release's `SHA256SUMS`, `docs/design/update-check.md` (threat-model.md SR-46, SR-48, SR-33).
//!
//! Input: the checksum file an update check downloads from the internet. In the real path
//! its Ed25519 signature is verified first; the target skips that so the line parser itself
//! is exercised. Whatever parses must name one plain release version, and every file it
//! offers must be exactly the release file its install kind asks for (so a pre-release name
//! such as `playdrop-0.11.0-rc.1-linux-amd64.deb` can never pass for 0.11.0), with a name
//! that cannot be a path or an option and a URL under the fixed release base.
#![no_main]

use libfuzzer_sys::fuzz_target;
use playdrop_core::update::{applet_asset_name, InstallKind, Version, DOWNLOAD_BASE};

fuzz_target!(|data: &[u8]| {
    let Some(sums) = playdrop_core::fuzz_api::update_sums(data) else {
        return;
    };
    let version = sums.version();
    assert_eq!(Version::parse(&version.to_string()), Ok(version));

    let older = Version {
        major: 0,
        minor: 0,
        patch: 0,
    };
    for kind in [
        InstallKind::Deb,
        InstallKind::Windows,
        InstallKind::Android,
        InstallKind::Flatpak,
        InstallKind::Ios,
        InstallKind::Other,
    ] {
        assert!(sums.offer(version, kind).is_none(), "same version offered");
        let Some(offer) = sums.offer(older, kind) else {
            assert_eq!(version, older);
            continue;
        };
        assert_eq!(offer.version, version);
        if let Some(asset) = &offer.asset {
            assert_eq!(Some(&asset.name), kind.asset_name(version).as_ref());
        }
        if let Some(applet) = sums.applet_asset() {
            assert_eq!(applet.name, applet_asset_name(version));
        }
        for asset in offer.asset.iter().chain(sums.applet_asset().iter()) {
            assert!(!asset.name.contains(['/', '\\', ' ']) && !asset.name.starts_with(['.', '-']));
            assert_eq!(asset.url, format!("{DOWNLOAD_BASE}/v{version}/{}", asset.name));
            assert_eq!(asset.sha256_hex().len(), 64);
        }
    }
});

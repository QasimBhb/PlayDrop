//! Test hooks of the update check (`docs/design/update-check.md` section 8), compiled only
//! into debug and test builds: `mod.rs` includes this file under
//! `cfg(any(debug_assertions, test))`, so a release build contains neither the code nor the
//! variable names, and a shipped `PlayDrop` cannot be pointed at another server or key.
//! Check a release binary with
//! `strings target/release/playdropd | grep -c PLAYDROP_UPDATE_` (expect 0).
//!
//! - `PLAYDROP_UPDATE_BASE`: replaces `https://github.com/QasimBhb/PlayDrop/releases`. Only
//!   a stand-in on this computer is accepted: plain `http` to `127.0.0.1` or `localhost`
//!   with a port, and then redirects may also go to that same origin. Any other value,
//!   `https` hosts included, is ignored, so even a debug build cannot be pointed at a
//!   server elsewhere.
//! - `PLAYDROP_UPDATE_TEST_KEY`: one more trusted Ed25519 public key, 64 hex characters.

use playdrop_core::update::key_from_hex;

/// What the hooks change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    /// Replacement for the releases base URL.
    pub base: Option<String>,
    /// The loopback origin `base` names (always set together with `base`).
    pub test_origin: Option<String>,
    /// One more trusted key.
    pub extra_key: Option<[u8; 32]>,
}

/// Reads the hooks from `env` (the real one is `std::env::var`). A value that does not
/// pass its rule is ignored with a warning, never half-applied.
pub fn read(env: impl Fn(&str) -> Option<String>) -> Overrides {
    let mut out = Overrides::default();
    if let Some(base) = env("PLAYDROP_UPDATE_BASE") {
        let base = base.trim().trim_end_matches('/').to_owned();
        if let Some(origin) = loopback_origin(&base) {
            out.test_origin = Some(origin.to_owned());
            out.base = Some(base);
        } else {
            tracing::warn!(
                "PLAYDROP_UPDATE_BASE ignored: use http://127.0.0.1:<port> or http://localhost:<port>"
            );
        }
    }
    if let Some(hex) = env("PLAYDROP_UPDATE_TEST_KEY") {
        if let Ok(key) = key_from_hex(&hex) {
            out.extra_key = Some(key);
        } else {
            tracing::warn!("PLAYDROP_UPDATE_TEST_KEY ignored: not 64 hex characters");
        }
    }
    if out.base.is_some() || out.extra_key.is_some() {
        tracing::warn!("update check test hooks are in effect (debug build)");
    }
    out
}

/// `http://127.0.0.1:<port>` or `http://localhost:<port>` when `base` starts with exactly
/// that origin (followed by nothing or a path).
fn loopback_origin(base: &str) -> Option<&str> {
    let rest = base.strip_prefix("http://")?;
    let authority_len = rest.find('/').unwrap_or(rest.len());
    let (host, port) = rest[..authority_len].split_once(':')?;
    let loopback = host == "127.0.0.1" || host == "localhost";
    let port_ok = !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit());
    (loopback && port_ok).then(|| &base[.."http://".len() + authority_len])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn loopback_http_bases_get_their_origin() {
        let o = read(env(&[(
            "PLAYDROP_UPDATE_BASE",
            "http://127.0.0.1:4100/QasimBhb/PlayDrop/releases/",
        )]));
        assert_eq!(
            o.base.as_deref(),
            Some("http://127.0.0.1:4100/QasimBhb/PlayDrop/releases")
        );
        assert_eq!(o.test_origin.as_deref(), Some("http://127.0.0.1:4100"));
        let o = read(env(&[("PLAYDROP_UPDATE_BASE", "http://localhost:80")]));
        assert_eq!(o.test_origin.as_deref(), Some("http://localhost:80"));
    }

    #[test]
    fn anything_but_a_loopback_stand_in_is_ignored() {
        for bad in [
            "https://mirror.example/r",
            "https://127.0.0.1:4100/r",
            "http://example.com:80/x",
            "http://127.0.0.1/x",
            "http://127.0.0.1:/x",
            "http://127.0.0.1:99999999/x",
            "http://127.0.0.1.evil:80/x",
            "ftp://127.0.0.1:21",
        ] {
            assert_eq!(
                read(env(&[("PLAYDROP_UPDATE_BASE", bad)])),
                Overrides::default(),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_test_key_must_be_hex() {
        let hex = "bfb53bf8307ddbc120f6270c57658e2aa2a82728eaf276f5f5bd70dae9001002";
        let o = read(env(&[("PLAYDROP_UPDATE_TEST_KEY", hex)]));
        assert_eq!(o.extra_key.map(|k| k[0]), Some(0xbf));
        assert_eq!(
            read(env(&[("PLAYDROP_UPDATE_TEST_KEY", "abc")])).extra_key,
            None
        );
    }
}

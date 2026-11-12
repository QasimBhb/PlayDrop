//! The update check's HTTPS client (`docs/design/update-check.md` sections 3 and 7).
//!
//! `ureq` with rustls and the bundled Mozilla roots, configured so a request carries the
//! URL, `Host` and `User-Agent: PlayDrop` and nothing else (SR-45): no `Accept`, no
//! `Accept-Encoding` (no compression feature is built in), no cookies (no cookie feature),
//! no cache validators (nothing is stored between checks). Redirects are followed here, by
//! hand, so every hop is checked against the allow list before anything is sent to it
//! (SR-48). Every call blocks; the caller runs it on the blocking pool.
//!
//! Proxy: `HTTPS_PROXY` (or `https_proxy`) and `ALL_PROXY` are honoured, with `NO_PROXY`,
//! as `curl` and most programs do for an `https` URL; `HTTP_PROXY` alone is not used,
//! because it is meant for plain `http` URLs and every request here is `https`. See
//! [`proxy_from_env`].
//!
//! Logging (SR-32 extension): URLs and outcomes only, never a response header.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use playdrop_core::update::{self, MAX_DOWNLOAD_LEN, MAX_REDIRECTS};
use sha2::{Digest, Sha256};
use ureq::http::{Response, StatusCode};
use ureq::{Agent, Body};

/// Why a request ended without the bytes it asked for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The server did not answer, or stopped sending, within the time limit (SR-48).
    #[error("no answer in time")]
    Timeout,
    /// The host could not be reached at all (no network, no DNS).
    #[error("could not connect: {0}")]
    Connect(String),
    /// A redirect pointed somewhere the allow list does not cover, or to plain HTTP
    /// (SR-48).
    #[error("redirect to a host that is not allowed")]
    RedirectRefused,
    /// More than [`MAX_REDIRECTS`] redirects (SR-48).
    #[error("too many redirects")]
    TooManyRedirects,
    /// The answer was larger than its limit (SR-48).
    #[error("the answer is larger than allowed")]
    TooLarge,
    /// The body ended before the length the server promised, or ran past it.
    #[error("the download has the wrong length")]
    WrongLength,
    /// The server answered with an HTTP error.
    #[error("the server answered {0}")]
    Status(u16),
    /// A download did not finish within [`FetchConfig::download_deadline`], or brought
    /// fewer bytes than [`MinProgress`] asks for in one of its windows.
    #[error("the download was too slow")]
    TooSlow,
    /// The user cancelled the download.
    #[error("cancelled")]
    Cancelled,
    /// The answer could not be read (TLS, protocol, or a local file error).
    #[error("{0}")]
    Other(String),
}

impl FetchError {
    fn from_ureq(e: &ureq::Error) -> Self {
        match e {
            ureq::Error::Timeout(_) => Self::Timeout,
            ureq::Error::BodyExceedsLimit(_) => Self::TooLarge,
            ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => {
                Self::Connect(e.to_string())
            }
            ureq::Error::Io(io) => Self::from_io(io),
            other => Self::Other(other.to_string()),
        }
    }

    fn from_io(e: &std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => Self::Timeout,
            std::io::ErrorKind::UnexpectedEof => Self::WrongLength,
            std::io::ErrorKind::ConnectionRefused => Self::Connect(e.to_string()),
            _ => match e
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<ureq::Error>())
            {
                Some(inner) => Self::from_ureq(inner),
                None => Self::Other(e.to_string()),
            },
        }
    }
}

/// Which hosts a request may be redirected to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectRule {
    /// Debug and test builds with `PLAYDROP_UPDATE_BASE` on a loopback address: redirects
    /// to that same origin (`http://127.0.0.1:<port>`) are also allowed. Always `None` in a
    /// release build.
    pub test_origin: Option<String>,
}

impl RedirectRule {
    /// Whether a redirect to `url` may be followed (SR-48): the release rule of
    /// [`update::redirect_allowed`], or the test origin.
    pub fn allows(&self, url: &str) -> bool {
        if update::redirect_allowed(url) {
            return true;
        }
        self.test_origin.as_deref().is_some_and(|origin| {
            url.strip_prefix(origin)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
    }
}

/// How the client reaches the internet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProxyChoice {
    /// Straight out.
    #[default]
    Direct,
    /// Through the proxy the environment names for `https` (see [`proxy_from_env`]).
    FromEnv,
}

/// Whether the environment names a proxy for `https` URLs: `HTTPS_PROXY`, `https_proxy`,
/// `ALL_PROXY` or `all_proxy`, non-empty. `HTTP_PROXY` alone does not count (it is for
/// plain `http`, as in `curl`). `env` reads one variable; the real one is
/// `std::env::var`.
pub fn proxy_from_env(env: impl Fn(&str) -> Option<String>) -> ProxyChoice {
    let named = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
        .iter()
        .any(|name| env(name).is_some_and(|v| !v.trim().is_empty()));
    if named {
        ProxyChoice::FromEnv
    } else {
        ProxyChoice::Direct
    }
}

/// The least a download must bring: at least `bytes` in every `window`, so a server that
/// trickles a byte at a time (each read within [`FetchConfig::timeout`]) cannot hold an
/// update open for hours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinProgress {
    /// Bytes that must arrive in each window.
    pub bytes: u64,
    /// The window.
    pub window: Duration,
}

/// The real rule: 64 KiB in any 60 s.
pub const MIN_PROGRESS: MinProgress = MinProgress {
    bytes: 64 * 1024,
    window: Duration::from_secs(60),
};

/// The longest a whole update download (every file) may take.
pub const DOWNLOAD_DEADLINE: Duration = Duration::from_mins(30);

/// Settings of a [`Fetcher`].
#[derive(Debug, Clone)]
pub struct FetchConfig {
    /// Time limit for each check request, all redirects included (SR-48). For a download,
    /// the limit for connecting, for the answer's head and for each read.
    pub timeout: Duration,
    /// A whole update download must end within this ([`DOWNLOAD_DEADLINE`]); the caller
    /// turns it into one deadline shared by every file.
    pub download_deadline: Duration,
    /// The slowest a download may run ([`MIN_PROGRESS`]).
    pub min_progress: MinProgress,
    /// Redirect allow list.
    pub redirects: RedirectRule,
    /// Proxy.
    pub proxy: ProxyChoice,
    /// Refuse plain `http` in the client itself as well (off only for the loopback test
    /// origin).
    pub https_only: bool,
}

/// The update client: one agent, the same anonymous request every time.
pub struct Fetcher {
    agent: Agent,
    config: FetchConfig,
}

/// A download that arrived whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// Bytes written.
    pub len: u64,
    /// Their SHA-256.
    pub sha256: [u8; 32],
}

impl Fetcher {
    /// A client for `config`.
    pub fn new(config: FetchConfig) -> Self {
        let proxy = match config.proxy {
            ProxyChoice::Direct => None,
            // `try_from_env` also reads `HTTP_PROXY`, but only after `ALL_PROXY` and
            // `HTTPS_PROXY`; [`proxy_from_env`] picks this branch only when one of those is
            // set, so `HTTP_PROXY` is never what it returns here unless the two named ones
            // fail to parse (then the request fails at the proxy, visibly).
            ProxyChoice::FromEnv => ureq::Proxy::try_from_env(),
        };
        let agent: Agent = Agent::config_builder()
            .proxy(proxy)
            .https_only(config.https_only)
            // Redirects are followed by hand so each target is checked first (SR-48).
            .max_redirects(0)
            // 4xx and 5xx come back as answers, so the status can be reported plainly.
            .http_status_as_error(false)
            .user_agent(update::USER_AGENT)
            // An empty value sends no header at all (ureq's documented behaviour).
            .accept("")
            .accept_encoding("")
            .build()
            .into();
        Self { agent, config }
    }

    /// GETs `url`, following allowed redirects, and returns at most `limit` bytes of a
    /// 2xx answer. The whole exchange, redirects included, has [`FetchConfig::timeout`].
    pub fn get_small(&self, url: &str, limit: usize) -> Result<Vec<u8>, FetchError> {
        let deadline = Instant::now() + self.config.timeout;
        let response = self.get_following(url, Limit::Check(deadline))?;
        if let Some(len) = response.body().content_length() {
            if len > limit as u64 {
                return Err(FetchError::TooLarge);
            }
        }
        let mut body = response.into_body();
        body.with_config()
            .limit(limit as u64)
            .read_to_vec()
            .map_err(|e| FetchError::from_ureq(&e))
    }

    /// Streams `url` into `out`, hashing as it goes. Refuses more than
    /// [`MAX_DOWNLOAD_LEN`], and a body whose length differs from the one the server
    /// promised. `progress` is told the bytes written so far and the promised length.
    ///
    /// Ends with [`FetchError::Timeout`] on a stall longer than [`FetchConfig::timeout`],
    /// with [`FetchError::TooSlow`] at `deadline` or when [`FetchConfig::min_progress`] is
    /// not met, and with [`FetchError::Cancelled`] soon after `cancel` is set (it is read
    /// after every chunk, so within one read).
    pub fn download(
        &self,
        url: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64, Option<u64>),
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Downloaded, FetchError> {
        if cancel.load(Ordering::SeqCst) {
            return Err(FetchError::Cancelled);
        }
        let response = self
            .get_following(url, Limit::Download(deadline))
            .map_err(|e| match e {
                // The shared deadline ran out: the download as a whole was too slow.
                FetchError::Timeout if Instant::now() >= deadline => FetchError::TooSlow,
                e => e,
            })?;
        let promised = response.body().content_length();
        if promised.is_some_and(|len| len > MAX_DOWNLOAD_LEN) {
            return Err(FetchError::TooLarge);
        }
        let mut reader = response
            .into_body()
            .into_with_config()
            .limit(MAX_DOWNLOAD_LEN)
            .reader();
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut len = 0u64;
        let rule = self.config.min_progress;
        let mut window_start = Instant::now();
        let mut window_bytes = 0u64;
        progress(0, promised);
        loop {
            let read = reader.read(&mut buf);
            if cancel.load(Ordering::SeqCst) {
                return Err(FetchError::Cancelled);
            }
            let now = Instant::now();
            let n = match read {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) if now >= deadline => return Err(FetchError::TooSlow),
                Err(e) => return Err(FetchError::from_io(&e)),
            };
            if now >= deadline {
                return Err(FetchError::TooSlow);
            }
            window_bytes += n as u64;
            if now.duration_since(window_start) >= rule.window {
                if window_bytes < rule.bytes {
                    return Err(FetchError::TooSlow);
                }
                window_start = now;
                window_bytes = 0;
            }
            let chunk = &buf[..n];
            hasher.update(chunk);
            out.write_all(chunk)
                .map_err(|e| FetchError::Other(format!("could not save the download: {e}")))?;
            len += n as u64;
            if promised.is_some_and(|p| len > p) {
                return Err(FetchError::WrongLength);
            }
            progress(len, promised);
        }
        if promised.is_some_and(|p| len != p) {
            return Err(FetchError::WrongLength);
        }
        out.flush()
            .map_err(|e| FetchError::Other(format!("could not save the download: {e}")))?;
        Ok(Downloaded {
            len,
            sha256: hasher.finalize().into(),
        })
    }

    /// One GET with redirects followed by hand: at most [`MAX_REDIRECTS`], each target
    /// checked with [`RedirectRule::allows`] before it is contacted, within `limit`.
    fn get_following(&self, url: &str, limit: Limit) -> Result<Response<Body>, FetchError> {
        let mut current = url.to_owned();
        let mut hops = 0u32;
        loop {
            let response = self.get_once(&current, limit)?;
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            if !is_redirect(status) {
                tracing::info!(url = %current, status = status.as_u16(), "update request answered with an error");
                return Err(FetchError::Status(status.as_u16()));
            }
            hops += 1;
            if hops > MAX_REDIRECTS {
                tracing::info!(url = %current, "update request redirected too often");
                return Err(FetchError::TooManyRedirects);
            }
            let next = response
                .headers()
                .get(ureq::http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|location| resolve_location(&current, location))
                .ok_or(FetchError::RedirectRefused)?;
            if !self.config.redirects.allows(&next) {
                // The URL is the server's, not ours, and may be long: its origin is enough
                // to say where it pointed.
                tracing::info!(from = %current, to = %origin_of(&next).unwrap_or("?"), "update redirect refused");
                return Err(FetchError::RedirectRefused);
            }
            tracing::debug!(from = %current, to = %origin_of(&next).unwrap_or("?"), "update request redirected");
            current = next;
        }
    }

    fn get_once(&self, url: &str, limit: Limit) -> Result<Response<Body>, FetchError> {
        let request = self.agent.get(url).config();
        let (Limit::Check(deadline) | Limit::Download(deadline)) = limit;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(FetchError::Timeout);
        }
        let request = match limit {
            Limit::Check(_) => request.timeout_global(Some(left)),
            // The deadline bounds the whole call, body included; a stall is caught sooner
            // by the per-step limits.
            Limit::Download(_) => request
                .timeout_global(Some(left))
                .timeout_connect(Some(self.config.timeout))
                .timeout_recv_response(Some(self.config.timeout))
                .timeout_per_read(Some(self.config.timeout)),
        };
        request
            .build()
            .call()
            .map_err(|e| FetchError::from_ureq(&e))
    }
}

/// The time limits of one request.
#[derive(Debug, Clone, Copy)]
enum Limit {
    /// A check request: the whole exchange, redirects included, ends by this instant.
    Check(Instant),
    /// A download: the whole download ends by this instant, and each step (connecting,
    /// the answer's head, each read) is limited to [`FetchConfig::timeout`].
    Download(Instant),
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// The absolute URL a `Location` header names, relative to `base`. Only absolute URLs and
/// absolute paths are accepted; anything else (a relative path, a scheme-relative `//host`)
/// is refused, since GitHub sends absolute URLs and guessing would only add ways to go
/// wrong.
fn resolve_location(base: &str, location: &str) -> Option<String> {
    let location = location.trim();
    if location.starts_with("https://") || location.starts_with("http://") {
        return Some(location.to_owned());
    }
    if location.starts_with('/') && !location.starts_with("//") {
        return Some(format!("{}{location}", origin_of(base)?));
    }
    None
}

/// `scheme://authority` of an absolute URL.
fn origin_of(url: &str) -> Option<&str> {
    let scheme_end = url.find("://")? + 3;
    let rest = &url[scheme_end..];
    let end = rest
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| scheme_end + i);
    Some(&url[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_resolve_only_when_absolute() {
        let base = "https://github.com/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS";
        assert_eq!(
            resolve_location(base, "https://objects.githubusercontent.com/x?y=z").as_deref(),
            Some("https://objects.githubusercontent.com/x?y=z")
        );
        assert_eq!(
            resolve_location(
                base,
                "/QasimBhb/PlayDrop/releases/download/v1.2.3/SHA256SUMS"
            )
            .as_deref(),
            Some("https://github.com/QasimBhb/PlayDrop/releases/download/v1.2.3/SHA256SUMS")
        );
        assert_eq!(resolve_location(base, "SHA256SUMS"), None);
        assert_eq!(resolve_location(base, "//evil.example/x"), None);
        assert_eq!(
            origin_of("http://127.0.0.1:9/a/b"),
            Some("http://127.0.0.1:9")
        );
        assert_eq!(origin_of("https://github.com"), Some("https://github.com"));
    }

    #[test]
    fn the_test_origin_allows_only_itself() {
        let rule = RedirectRule {
            test_origin: Some("http://127.0.0.1:4000".into()),
        };
        assert!(rule.allows("http://127.0.0.1:4000/a"));
        assert!(rule.allows("http://127.0.0.1:4000"));
        assert!(!rule.allows("http://127.0.0.1:40001/a"));
        assert!(!rule.allows("http://127.0.0.1:4000@evil.example/a"));
        assert!(!rule.allows("http://localhost:4000/a"));
        assert!(rule.allows("https://github.com/x"));
        assert!(!rule.allows("http://github.com/x"));
        let release = RedirectRule { test_origin: None };
        assert!(!release.allows("http://127.0.0.1:4000/a"));
    }

    #[test]
    fn only_an_https_proxy_is_used() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(proxy_from_env(env(&[])), ProxyChoice::Direct);
        assert_eq!(
            proxy_from_env(env(&[("HTTP_PROXY", "http://p:3128")])),
            ProxyChoice::Direct
        );
        assert_eq!(
            proxy_from_env(env(&[("HTTPS_PROXY", "http://p:3128")])),
            ProxyChoice::FromEnv
        );
        assert_eq!(
            proxy_from_env(env(&[("https_proxy", "http://p:3128")])),
            ProxyChoice::FromEnv
        );
        assert_eq!(
            proxy_from_env(env(&[("ALL_PROXY", "http://p:3128")])),
            ProxyChoice::FromEnv
        );
        assert_eq!(
            proxy_from_env(env(&[("HTTPS_PROXY", "  ")])),
            ProxyChoice::Direct
        );
    }
}

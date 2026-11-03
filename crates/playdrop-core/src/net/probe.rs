//! The path probe of protocol.md 3.5: `GET /v1/probe?bytes=N` answered with `N` bytes of
//! zeros, so a dialler holding two connections to the same peer can tell which path is
//! faster by measuring, not by guessing from interface types.
//!
//! - Serving ([`serve`]): pinned connections only (the SR-13 gate in [`super::server`]
//!   already answers `403 not_paired` on an unpaired one), `N` at most [`PROBE_MAX_BYTES`]
//!   (`400 bad_request` above), one probe in flight and [`PROBES_PER_MINUTE`] per
//!   connection (`429 rate_limited` otherwise, SR-15).
//! - Measuring ([`probe`]): a small estimating request, then one sized to take about
//!   [`PROBE_TARGET`], measured after a warm-up so TCP slow start does not decide it
//!   ([`ProbeMeter`]).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use h2::server::SendResponse;
use h2::RecvStream;
use http::{HeaderValue, Method, Request, StatusCode};

use super::connection::{short, Connection, Heard, DUPLICATE_PING_DEADLINE};
use super::{respond_error, response_head, send_all, HttpError};
use crate::session::ErrorCode;

/// Path of the probe endpoint (protocol.md 3.5, 7.1).
pub const PROBE_PATH: &str = "/v1/probe";

/// Most bytes a dialler asks for in one measured request. *Choice:* 4 MiB takes 70 to
/// 150 ms at the 27 to 55 MiB/s measured on the Galaxy A54 (`docs/release/throughput.md`
/// 12.3), long enough for a stable reading after the warm-up and short enough that two
/// probes cost under half a second of link time.
pub const PROBE_BYTES: u64 = 4 * 1024 * 1024;

/// Size of the first, estimating request of a probe. *Choice:* 128 KiB is read within the
/// body limit down to 128 KiB/s, so even a very slow path has little left queued towards the
/// dialler when the rest is cancelled; anything queued there holds up the session frames
/// behind it. A fast path still reaches the [`PROBE_BYTES`] cap: slow start alone delivers
/// this much at several MiB/s on a LAN.
pub const PROBE_ESTIMATE_BYTES: u64 = 128 * 1024;

/// An estimate at or above this (1 MiB/s) means the path is not slow, and the measured
/// request is the full [`PROBE_BYTES`]. *Choice:* 128 KiB finishes during slow start, so on a
/// fast path with a higher round trip the estimate reads low, and a request sized from it
/// would measure a path still ramping up (50 MiB/s at 20 ms read as about 30). What such a
/// request leaves unread on a path that really is this slow is cancelled when the read limit
/// passes.
pub const NOT_SLOW_ESTIMATE: f64 = 1024.0 * 1024.0;

/// How long the measured request should take at the estimated rate. *Choice:* 750 ms
/// stays under [`PROBE_BODY_LIMIT`] when the estimate is right, and a fast path reaches
/// the [`PROBE_BYTES`] cap long before it.
pub const PROBE_TARGET: Duration = Duration::from_millis(750);

/// Bytes left out of the measurement at the start of a probe. *Choice:* 1 MiB: a LAN
/// connection leaves slow start within a handful of round trips (about 0.5 MiB at 50 MiB/s
/// and 5 ms), and the HTTP/2 windows (8 MiB stream, 32 MiB connection, section 10) never
/// stall a 4 MiB body.
pub const PROBE_WARMUP: u64 = 1024 * 1024;

/// Largest `bytes` a host serves; more is `400 bad_request` (SR-15).
pub const PROBE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Probes one connection may ask for per minute; more is `429 rate_limited` (SR-15).
pub const PROBES_PER_MINUTE: usize = 10;

/// How long the dialler waits for the probe's response head.
pub const PROBE_HEAD_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the dialler reads a probe body before it measures what arrived. *Choice:*
/// 1 s bounds a check on a slow path (2 MiB at 2 MiB/s) while a fast one finishes its
/// 4 MiB in a fraction of it.
pub const PROBE_BODY_LIMIT: Duration = Duration::from_secs(1);

/// Fewest bytes a probe must deliver for its rate to count.
const PROBE_MIN_BYTES: u64 = 64 * 1024;

/// Size of the shared block of zeros the server sends from. TLS encrypts it, so zeros
/// cost nothing to generate and reveal nothing.
const ZERO_BLOCK: usize = 64 * 1024;
static ZEROS: [u8; ZERO_BLOCK] = [0; ZERO_BLOCK];

/// Window of the per-connection probe rate limit.
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// True for `GET /v1/probe`, whatever its query.
pub(crate) fn is_probe<B>(req: &Request<B>) -> bool {
    req.method() == Method::GET && req.uri().path() == PROBE_PATH
}

/// Reads `bytes` from a probe query. A missing parameter means [`PROBE_BYTES`]; unknown
/// parameters are ignored; a value that is not a number or exceeds [`PROBE_MAX_BYTES`]
/// is an error.
pub fn parse_probe_bytes(query: Option<&str>) -> Result<u64, String> {
    let mut bytes = PROBE_BYTES;
    for pair in query.unwrap_or_default().split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == "bytes" {
            bytes = v
                .parse::<u64>()
                .map_err(|_| "bytes must be a whole number".to_owned())?;
        }
    }
    if bytes > PROBE_MAX_BYTES {
        return Err(format!("bytes over {PROBE_MAX_BYTES}"));
    }
    Ok(bytes)
}

/// The per-connection probe limits of protocol.md 3.5 (SR-15): one in flight, at most
/// [`PROBES_PER_MINUTE`] started per minute.
#[derive(Debug, Default)]
pub(crate) struct ProbeLimiter {
    started: VecDeque<Instant>,
    in_flight: bool,
}

impl ProbeLimiter {
    /// Starts a probe at `now` if both limits allow it.
    pub(crate) fn try_start(&mut self, now: Instant) -> bool {
        if self.in_flight {
            return false;
        }
        while self
            .started
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= RATE_WINDOW)
        {
            self.started.pop_front();
        }
        if self.started.len() >= PROBES_PER_MINUTE {
            return false;
        }
        self.started.push_back(now);
        self.in_flight = true;
        true
    }

    /// The probe in flight ended (sent, failed or reset).
    pub(crate) fn finish(&mut self) {
        self.in_flight = false;
    }
}

/// Frees the connection's probe slot when the serving task ends, however it ends.
struct InFlight(Arc<Connection>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.probe_finished();
    }
}

/// Answers `GET /v1/probe` on a pinned connection (protocol.md 3.5).
pub(crate) async fn serve(
    conn: Arc<Connection>,
    req: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
) {
    let bytes = match parse_probe_bytes(req.uri().query()) {
        Ok(b) => b,
        Err(m) => {
            let err = HttpError::from_code(ErrorCode::BadRequest, m);
            let _ = respond_error(&mut respond, &err).await;
            return;
        }
    };
    if !conn.probe_try_start() {
        tracing::debug!(peer = %short(&conn.device_id()), "probe refused by its limits");
        let err = HttpError::from_code(ErrorCode::RateLimited, "probe limit reached");
        let _ = respond_error(&mut respond, &err).await;
        return;
    }
    let _slot = InFlight(Arc::clone(&conn));
    let mut head = response_head(StatusCode::OK, Some("application/octet-stream"));
    head.headers_mut()
        .insert(http::header::CONTENT_LENGTH, HeaderValue::from(bytes));
    let mut stream = match respond.send_response(head, bytes == 0) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "probe response failed");
            return;
        }
    };
    let mut left = bytes;
    while left > 0 {
        let n = usize::try_from(left).map_or(ZERO_BLOCK, |l| l.min(ZERO_BLOCK));
        left -= n as u64;
        let block = Bytes::from_static(&ZEROS[..n]);
        if send_all(&mut stream, block, left == 0).await.is_err() {
            // The dialler stopped reading (its time limit) and reset the stream.
            return;
        }
    }
}

/// Measures a probe body: the rate over the bytes after the first `warmup`, timed from
/// the arrival that crossed the warm-up to the last arrival. When the warm-up was never
/// crossed (a very slow path) or nothing came after it, the rate over the whole body
/// from the request is used instead.
#[derive(Debug, Clone)]
pub struct ProbeMeter {
    warmup: u64,
    started: Instant,
    total: u64,
    mark: Option<(u64, Instant)>,
    last: Option<Instant>,
}

impl ProbeMeter {
    /// A meter for a probe requested at `started`.
    pub fn new(warmup: u64, started: Instant) -> Self {
        Self {
            warmup,
            started,
            total: 0,
            mark: None,
            last: None,
        }
    }

    /// Records `len` body bytes arriving at `at`.
    pub fn push(&mut self, len: u64, at: Instant) {
        self.total += len;
        if self.mark.is_none() && self.total >= self.warmup {
            self.mark = Some((self.total, at));
        }
        self.last = Some(at);
    }

    /// Body bytes seen.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The rate in bytes per second, or `None` with no time to divide by.
    #[allow(clippy::cast_precision_loss, reason = "byte counts far below 2^52")]
    pub fn rate(&self) -> Option<f64> {
        let last = self.last?;
        if let Some((at_mark, t0)) = self.mark {
            let secs = last.saturating_duration_since(t0).as_secs_f64();
            if self.total > at_mark && secs > 0.0 {
                return Some((self.total - at_mark) as f64 / secs);
            }
        }
        let secs = last.saturating_duration_since(self.started).as_secs_f64();
        (secs > 0.0 && self.total > 0).then(|| self.total as f64 / secs)
    }
}

/// What a probe found.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeOutcome {
    /// Measured, in bytes per second.
    Rate(f64),
    /// The peer has no probe endpoint (`404`): a host from before protocol.md 3.5.
    Unsupported,
    /// The probe failed; the text says how.
    Failed(String),
}

/// Probes `conn` (a connection this host dialled) in two requests (protocol.md 3.5):
/// [`PROBE_ESTIMATE_BYTES`] for a first estimate, then as many bytes as that estimate
/// says arrive in [`PROBE_TARGET`], between the estimate size and `bytes`, measured after
/// a warm-up. Sizing the second request keeps the server from pushing much more than the
/// dialler reads: the unread rest of a large probe would drain at path speed and hold up
/// session frames on a slow connection for seconds. If the second request fails, the
/// estimate stands.
pub async fn probe(conn: &Connection, bytes: u64) -> ProbeOutcome {
    let first = PROBE_ESTIMATE_BYTES.min(bytes);
    let estimate = match fetch(conn, first, 0).await {
        ProbeOutcome::Rate(r) => r,
        other => return other,
    };
    let n = second_size(estimate, first, bytes);
    if n <= first {
        return ProbeOutcome::Rate(estimate);
    }
    match fetch(conn, n, PROBE_WARMUP.min(n / 4)).await {
        ProbeOutcome::Rate(r) => ProbeOutcome::Rate(r),
        other => {
            tracing::debug!(?other, "measured probe request failed; the estimate stands");
            ProbeOutcome::Rate(estimate)
        }
    }
}

/// What probing the connection in use found (protocol.md 3.5 "Order").
#[derive(Debug, Clone, PartialEq)]
pub enum InUseProbe {
    /// Measured, in bytes per second.
    Rate(f64),
    /// `404`: the peer predates protocol.md 3.5.
    Unsupported,
    /// The probe failed and for 2 s nothing at all arrived: the path is dead.
    Dead(String),
    /// The probe failed twice while the connection still answers PINGs: a stall, not a
    /// dead path.
    Unmeasured(String),
}

/// Probes the connection in use. A failed probe means a dead path only if for 2 s neither a
/// PING's ACK nor any other byte arrives (for example after a network change); a connection
/// that is still heard from gets one more probe, so a passing stall never moves the peer to
/// a slower path.
pub async fn probe_in_use(conn: &Connection, bytes: u64) -> InUseProbe {
    let first = match probe(conn, bytes).await {
        ProbeOutcome::Rate(r) => return InUseProbe::Rate(r),
        ProbeOutcome::Unsupported => return InUseProbe::Unsupported,
        ProbeOutcome::Failed(e) => e,
    };
    if matches!(
        conn.heard_from(DUPLICATE_PING_DEADLINE).await,
        Heard::Nothing | Heard::Closed
    ) {
        return InUseProbe::Dead(first);
    }
    match probe(conn, bytes).await {
        ProbeOutcome::Rate(r) => InUseProbe::Rate(r),
        ProbeOutcome::Unsupported => InUseProbe::Unsupported,
        ProbeOutcome::Failed(e) => InUseProbe::Unmeasured(e),
    }
}

/// The size of the measured request: `max` if the path is not slow
/// ([`NOT_SLOW_ESTIMATE`]), else what `estimate` bytes per second deliver in
/// [`PROBE_TARGET`], at least `first`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a positive size far below 2^52"
)]
pub fn second_size(estimate: f64, first: u64, max: u64) -> u64 {
    if estimate >= NOT_SLOW_ESTIMATE {
        return max.max(first);
    }
    let want = (estimate.max(0.0) * PROBE_TARGET.as_secs_f64()) as u64;
    want.clamp(first, max.max(first))
}

/// One `GET /v1/probe?bytes=n`, read for at most [`PROBE_BODY_LIMIT`] and measured after
/// `warmup` bytes.
async fn fetch(conn: &Connection, bytes: u64, warmup: u64) -> ProbeOutcome {
    let req = match Request::builder()
        .method(Method::GET)
        .uri(format!("{PROBE_PATH}?bytes={bytes}"))
        .body(())
    {
        Ok(r) => r,
        Err(e) => return ProbeOutcome::Failed(e.to_string()),
    };
    let started = Instant::now();
    let (resp, mut send) = match conn.request(req, true).await {
        Ok(v) => v,
        Err(e) => return ProbeOutcome::Failed(e.to_string()),
    };
    let resp = match tokio::time::timeout(PROBE_HEAD_TIMEOUT, resp).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return ProbeOutcome::Failed(e.to_string()),
        Err(_) => return ProbeOutcome::Failed("no answer".into()),
    };
    match resp.status() {
        StatusCode::OK => {}
        StatusCode::NOT_FOUND => return ProbeOutcome::Unsupported,
        s => return ProbeOutcome::Failed(format!("answered {}", s.as_u16())),
    }
    let mut body = resp.into_body();
    let mut meter = ProbeMeter::new(warmup, started);
    let deadline = tokio::time::Instant::now() + PROBE_BODY_LIMIT;
    while meter.total() < bytes {
        match tokio::time::timeout_at(deadline, body.data()).await {
            Ok(Some(Ok(chunk))) => {
                let n = chunk.len();
                let _ = body.flow_control().release_capacity(n);
                meter.push(n as u64, Instant::now());
            }
            Ok(Some(Err(e))) => return ProbeOutcome::Failed(e.to_string()),
            Ok(None) | Err(_) => break,
        }
    }
    // Not all of it was read in time: reset the stream, so the server stops sending and frees
    // its slot, and what it has not sent yet does not queue ahead of later session frames.
    if meter.total() < bytes {
        send.send_reset(h2::Reason::CANCEL);
    }
    drop(body);
    if meter.total() < PROBE_MIN_BYTES.min(bytes) {
        return ProbeOutcome::Failed(format!("only {} bytes arrived", meter.total()));
    }
    meter.rate().map_or_else(
        || ProbeOutcome::Failed("no timing".into()),
        ProbeOutcome::Rate,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;
    const MIB_F: f64 = 1024.0 * 1024.0;

    #[test]
    fn second_request_is_sized_from_the_estimate() {
        let max = PROBE_BYTES;
        // A slow path: what it delivers in 750 ms.
        assert_eq!(second_size(0.8 * MIB_F, PROBE_ESTIMATE_BYTES, max), 629_145);
        // Not slow, though slow start may hide how fast: the full size.
        assert_eq!(second_size(MIB_F, PROBE_ESTIMATE_BYTES, max), max);
        assert_eq!(second_size(2.0 * MIB_F, PROBE_ESTIMATE_BYTES, max), max);
        // A fast path (even underestimated by slow start) asks for the cap.
        assert_eq!(second_size(20.0 * MIB_F, PROBE_ESTIMATE_BYTES, max), max);
        // A very slow path never asks for less than the estimate size.
        assert_eq!(
            second_size(0.1 * MIB_F, PROBE_ESTIMATE_BYTES, max),
            PROBE_ESTIMATE_BYTES
        );
        assert_eq!(
            second_size(-1.0, PROBE_ESTIMATE_BYTES, max),
            PROBE_ESTIMATE_BYTES
        );
    }

    #[test]
    fn query_parsing() {
        assert_eq!(parse_probe_bytes(None), Ok(PROBE_BYTES));
        assert_eq!(parse_probe_bytes(Some("")), Ok(PROBE_BYTES));
        assert_eq!(parse_probe_bytes(Some("bytes=1024")), Ok(1024));
        assert_eq!(
            parse_probe_bytes(Some("x=1&bytes=0&y")),
            Ok(0),
            "unknown ignored"
        );
        assert_eq!(
            parse_probe_bytes(Some("bytes=8388608")),
            Ok(PROBE_MAX_BYTES)
        );
        assert!(
            parse_probe_bytes(Some("bytes=8388609")).is_err(),
            "over 8 MiB"
        );
        assert!(parse_probe_bytes(Some("bytes=-1")).is_err());
        assert!(parse_probe_bytes(Some("bytes=lots")).is_err());
    }

    #[test]
    fn limiter_allows_one_in_flight_and_ten_per_minute() {
        let t = Instant::now();
        let mut l = ProbeLimiter::default();
        assert!(l.try_start(t));
        assert!(!l.try_start(t), "one in flight");
        l.finish();
        for i in 1..PROBES_PER_MINUTE {
            assert!(l.try_start(t + Duration::from_secs(i as u64)), "probe {i}");
            l.finish();
        }
        assert!(
            !l.try_start(t + Duration::from_secs(30)),
            "eleventh in a minute"
        );
        // The first one leaves the window after 60 s.
        assert!(l.try_start(t + Duration::from_secs(60)));
    }

    #[test]
    fn meter_leaves_out_the_warm_up() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut m = ProbeMeter::new(MIB, t0);
        // Slow start: the first MiB takes 100 ms.
        for i in 1..=4 {
            m.push(MIB / 4, ms(25 * i));
        }
        // Then 3 MiB in 60 ms: 50 MiB/s.
        for i in 1..=3 {
            m.push(MIB, ms(100 + 20 * i));
        }
        let rate = m.rate().unwrap() / MIB_F;
        assert!((rate - 50.0).abs() < 0.01, "{rate}");
    }

    #[test]
    fn meter_falls_back_to_the_whole_body_on_a_slow_path() {
        let t0 = Instant::now();
        let mut m = ProbeMeter::new(MIB, t0);
        // 512 KiB in 1 s never crosses the warm-up.
        m.push(MIB / 2, t0 + Duration::from_secs(1));
        let rate = m.rate().unwrap() / MIB_F;
        assert!((rate - 0.5).abs() < 0.01, "{rate}");
        // Nothing arrived: no rate.
        assert!(ProbeMeter::new(MIB, t0).rate().is_none());
    }

    #[test]
    fn meter_with_one_chunk_past_the_warm_up_uses_the_whole_body() {
        let t0 = Instant::now();
        let mut m = ProbeMeter::new(MIB, t0);
        // A single 4 MiB read crosses the warm-up and ends at once: no span after it.
        m.push(4 * MIB, t0 + Duration::from_millis(80));
        let rate = m.rate().unwrap() / MIB_F;
        assert!((rate - 50.0).abs() < 0.01, "{rate}");
    }
}

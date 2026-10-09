//! Wayland backend: `ext-data-control-v1` or `zwlr-data-control-v1` through `wl-clipboard-rs`.
//!
//! Watching uses `wl_clipboard_rs::watch::Watcher`, which 0.9.4 provides: one long-lived
//! Wayland connection that blocks in `poll` until the compositor announces a new selection,
//! and a cancel pipe that wakes it for [`WatchHandle::stop`]. No polling, so an idle daemon
//! costs nothing and a copy is seen as soon as the compositor sends the event. The first event
//! (the selection that existed before the daemon started) is skipped, so starting the daemon
//! never pushes an old clip.
//!
//! Reading the offered data goes through a pipe the source app writes into. A short reader
//! thread enforces [`READ_TIMEOUT`] so an app that never closes its end cannot stall the
//! watcher; if it times out, that reader thread lingers until the app closes the pipe.
//!
//! Writing uses `wl_clipboard_rs::copy` with `foreground(false)`: the crate serves paste
//! requests from its own thread until the compositor cancels the source. Every new selection
//! (our next `apply`, or the user copying anything) cancels the previous source, so at most one
//! serving thread is alive at a time and nothing leaks.

use std::ffi::OsStr;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::{mpsc, oneshot};
use wl_clipboard_rs::paste::{self, MimeType, Seat};
use wl_clipboard_rs::utils::{
    is_primary_selection_supported, PrimarySelectionCheckError, PASSWORD_MANAGER_HINT_MIME_TYPE,
};
use wl_clipboard_rs::{copy, watch};

use super::unsupported::{unsupported, NO_DATA_CONTROL, X11};
use super::MAX_SYNC_BYTES;
use super::{short_hash, size_verdict, text_hash, ClipboardSupport, LocalClip, SizeVerdict};

/// Backend name reported in diagnostics.
const BACKEND: &str = "wl-data-control";
/// Longest wait for a source app to deliver offered data.
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// The password-manager hint value is `secret`; anything longer is not that value.
const HINT_LIMIT: usize = 64;
/// Text MIME types we read, best first. `UTF8_STRING` is always UTF-8; bare `text/plain`
/// has no declared charset, so it comes last.
const TEXT_MIME_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

/// The kind of graphical session, from the environment alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    Wayland { x11_too: bool },
    X11,
    None,
}

fn session_kind(
    wayland_display: Option<&OsStr>,
    display: Option<&OsStr>,
    session_type: Option<&OsStr>,
) -> Session {
    let set = |v: Option<&OsStr>| v.is_some_and(|v| !v.is_empty());
    let x11 = set(display) || session_type == Some(OsStr::new("x11"));
    if set(wayland_display) || session_type == Some(OsStr::new("wayland")) {
        Session::Wayland { x11_too: x11 }
    } else if x11 {
        Session::X11
    } else {
        Session::None
    }
}

pub(super) fn probe() -> ClipboardSupport {
    let session = session_kind(
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        std::env::var_os("DISPLAY").as_deref(),
        std::env::var_os("XDG_SESSION_TYPE").as_deref(),
    );
    let x11_too = match session {
        Session::Wayland { x11_too } => x11_too,
        Session::X11 => return unsupported(X11),
        Session::None => {
            return unsupported(
                "no graphical session was found (WAYLAND_DISPLAY and DISPLAY are not set)",
            )
        }
    };
    // Any answer about the primary selection means a data-control manager was found.
    let support = match is_primary_selection_supported() {
        Ok(_) => ClipboardSupport {
            supported: true,
            backend: BACKEND,
            reason: None,
        },
        Err(PrimarySelectionCheckError::MissingProtocol) => unsupported(NO_DATA_CONTROL),
        Err(PrimarySelectionCheckError::NoSeats) => {
            unsupported("the compositor reports no input seat, so there is no clipboard to watch")
        }
        Err(
            PrimarySelectionCheckError::WaylandConnection(_)
            | PrimarySelectionCheckError::SocketOpenError(_),
        ) if x11_too => unsupported(X11),
        Err(
            PrimarySelectionCheckError::WaylandConnection(_)
            | PrimarySelectionCheckError::SocketOpenError(_),
        ) => unsupported("could not connect to the Wayland display"),
        Err(e) => unsupported(format!("could not query the compositor: {e}")),
    };
    tracing::info!(
        supported = support.supported,
        reason = support.reason.as_deref().unwrap_or(""),
        "clipboard probe"
    );
    support
}

/// Handle to the watcher thread.
pub(super) struct WatchHandle {
    cancel: watch::CancelHandle,
    stop: Arc<AtomicBool>,
}

impl WatchHandle {
    /// Wakes the thread out of its blocking wait; it exits before reporting anything else.
    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.cancel.cancel();
    }
}

pub(super) async fn spawn_watcher(raw: mpsc::Sender<LocalClip>) -> anyhow::Result<WatchHandle> {
    let (ready_tx, ready_rx) = oneshot::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("pd-clip-watch".into())
        .spawn(move || {
            // The watcher is built on its own thread: its Wayland queue stays there.
            let watcher =
                match watch::Watcher::new(watch::ClipboardType::Regular, Seat::Unspecified) {
                    Ok(w) => w,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
            let _ = ready_tx.send(Ok(watcher.cancel_handle()));
            watch_loop(watcher, &raw, &thread_stop);
        })
        .context("could not start the clipboard watcher thread")?;
    let cancel = ready_rx
        .await
        .context("the clipboard watcher thread exited during start-up")?
        .context("could not watch the clipboard")?;
    Ok(WatchHandle { cancel, stop })
}

fn watch_loop(mut watcher: watch::Watcher, raw: &mpsc::Sender<LocalClip>, stop: &AtomicBool) {
    let mut initial = true;
    loop {
        let event = match watcher.next_event() {
            Ok(Some(event)) => event,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "clipboard watcher lost the compositor; stopping");
                break;
            }
        };
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if std::mem::take(&mut initial) {
            continue;
        }
        let watch::ClipboardEvent::Changed {
            mime_types,
            mut offer,
            ..
        } = event
        else {
            continue;
        };
        let Some(text) = read_change(&mime_types, &mut offer) else {
            continue;
        };
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let clip = LocalClip {
            text,
            sensitive: false,
        };
        if raw.blocking_send(clip).is_err() {
            break;
        }
    }
    tracing::debug!("clipboard watcher thread exited");
}

/// Applies the SR-24 and size rules to one selection change and returns the text to report.
fn read_change(mime_types: &[String], offer: &mut watch::Offer<'_>) -> Option<String> {
    if offers_password_hint(mime_types) {
        // Fail closed: an unreadable hint counts as `secret`.
        let secret = offer
            .receive(PASSWORD_MANAGER_HINT_MIME_TYPE)
            .ok()
            .and_then(|pipe| read_limited(pipe, HINT_LIMIT, READ_TIMEOUT).ok())
            .is_none_or(|read| match read {
                Limited::Data(value) => hint_marks_secret(&value),
                Limited::TooLarge => false,
            });
        if secret {
            tracing::debug!("clipboard change carries the password-manager hint; not synced");
            return None;
        }
    }
    let mime = select_text_mime(mime_types)?;
    let pipe = match offer.receive(mime) {
        Ok(pipe) => pipe,
        Err(e) => {
            tracing::debug!(error = %e, "could not receive clipboard text");
            return None;
        }
    };
    match read_limited(pipe, MAX_SYNC_BYTES, READ_TIMEOUT) {
        Ok(Limited::Data(bytes)) => {
            let text = decode(bytes);
            if text.is_empty() {
                return None;
            }
            if size_verdict(text.len()) == SizeVerdict::NeedsBlob {
                tracing::debug!(len = text.len(), "clipboard text over 32 KiB; core decides");
            }
            tracing::debug!(len = text.len(), hash = %short_hash(&text_hash(&text)), "clipboard changed");
            Some(text)
        }
        Ok(Limited::TooLarge) => {
            tracing::debug!(
                limit = MAX_SYNC_BYTES,
                "clipboard text over 1 MiB; not synced"
            );
            None
        }
        Err(e) => {
            tracing::debug!(error = %e, "could not read clipboard text");
            None
        }
    }
}

pub(super) fn read_current() -> anyhow::Result<Option<LocalClip>> {
    let types =
        match paste::get_mime_types_ordered(paste::ClipboardType::Regular, Seat::Unspecified) {
            Ok(types) => types,
            Err(
                paste::Error::ClipboardEmpty | paste::Error::NoSeats | paste::Error::NoMimeType,
            ) => return Ok(None),
            Err(e) => return Err(e).context("could not read the clipboard"),
        };
    let sensitive = offers_password_hint(&types) && read_hint_is_secret();
    let Some(mime) = select_text_mime(&types) else {
        return Ok(None);
    };
    let pipe = match paste::get_contents(
        paste::ClipboardType::Regular,
        Seat::Unspecified,
        MimeType::Specific(mime),
    ) {
        Ok((pipe, _)) => pipe,
        Err(paste::Error::ClipboardEmpty | paste::Error::NoSeats | paste::Error::NoMimeType) => {
            return Ok(None)
        }
        Err(e) => return Err(e).context("could not read the clipboard"),
    };
    match read_limited(pipe, MAX_SYNC_BYTES, READ_TIMEOUT)? {
        Limited::Data(bytes) => {
            let text = decode(bytes);
            Ok((!text.is_empty()).then_some(LocalClip { text, sensitive }))
        }
        Limited::TooLarge => anyhow::bail!("clipboard text is larger than 1 MiB and is not synced"),
    }
}

/// Reads the hint for a one-shot read. Fails closed: unreadable means `secret`.
fn read_hint_is_secret() -> bool {
    paste::get_contents(
        paste::ClipboardType::Regular,
        Seat::Unspecified,
        MimeType::Specific(PASSWORD_MANAGER_HINT_MIME_TYPE),
    )
    .ok()
    .and_then(|(pipe, _)| read_limited(pipe, HINT_LIMIT, READ_TIMEOUT).ok())
    .is_none_or(|read| match read {
        Limited::Data(value) => hint_marks_secret(&value),
        Limited::TooLarge => false,
    })
}

pub(super) fn apply(text: &str, sensitive: bool) -> anyhow::Result<()> {
    let mut options = copy::Options::new();
    options
        .clipboard(copy::ClipboardType::Regular)
        .seat(copy::Seat::All)
        .foreground(false)
        // Offers `x-kde-passwordManagerHint` = `secret` alongside the text (SR-24).
        .sensitive(sensitive);
    options
        .copy(
            copy::Source::Bytes(text.as_bytes().into()),
            copy::MimeType::Text,
        )
        .context("could not write the clipboard")?;
    Ok(())
}

/// Picks the best text MIME type from those offered, or `None` when no text is offered.
fn select_text_mime(offered: &[String]) -> Option<&'static str> {
    TEXT_MIME_TYPES
        .iter()
        .copied()
        .find(|want| offered.iter().any(|have| mime_eq(have, want)))
}

/// MIME comparison that ignores case and spaces around parameters
/// (`text/plain; charset=UTF-8` equals `text/plain;charset=utf-8`).
fn mime_eq(have: &str, want: &str) -> bool {
    let mut have = have.chars().filter(|c| !c.is_whitespace());
    let mut want = want.chars();
    loop {
        match (have.next(), want.next()) {
            (None, None) => return true,
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(&b) => {}
            _ => return false,
        }
    }
}

fn offers_password_hint(offered: &[String]) -> bool {
    offered.iter().any(|m| m == PASSWORD_MANAGER_HINT_MIME_TYPE)
}

/// KDE's convention: the hint's content is the word `secret`.
fn hint_marks_secret(value: &[u8]) -> bool {
    value.trim_ascii() == b"secret"
}

fn decode(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

#[derive(Debug, PartialEq, Eq)]
enum Limited {
    Data(Vec<u8>),
    TooLarge,
}

/// Reads at most `limit` bytes within `timeout`. More than `limit` bytes is `TooLarge`
/// (the reader stops at `limit + 1`, so a huge clip is never held in memory).
fn read_limited<R>(reader: R, limit: usize, timeout: Duration) -> anyhow::Result<Limited>
where
    R: Read + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("pd-clip-read".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
            let result = reader.take(cap).read_to_end(&mut buf).map(|_| buf);
            let _ = tx.send(result);
        })
        .context("could not start a clipboard reader thread")?;
    let bytes = rx
        .recv_timeout(timeout)
        .context("the app owning the clipboard did not deliver its data in time")?
        .context("could not read the clipboard data")?;
    Ok(if bytes.len() > limit {
        Limited::TooLarge
    } else {
        Limited::Data(bytes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn prefers_utf8_text_types() {
        let all = types(&[
            "text/plain",
            "UTF8_STRING",
            "text/plain;charset=utf-8",
            "TEXT",
        ]);
        assert_eq!(select_text_mime(&all), Some("text/plain;charset=utf-8"));
        let no_charset = types(&["STRING", "text/plain", "UTF8_STRING"]);
        assert_eq!(select_text_mime(&no_charset), Some("UTF8_STRING"));
        assert_eq!(
            select_text_mime(&types(&["text/plain"])),
            Some("text/plain")
        );
    }

    #[test]
    fn mime_match_ignores_case_and_spaces() {
        let offered = types(&["text/plain; charset=UTF-8"]);
        assert_eq!(select_text_mime(&offered), Some("text/plain;charset=utf-8"));
        assert!(!mime_eq("text/plainx", "text/plain"));
        assert!(!mime_eq("text/plai", "text/plain"));
    }

    #[test]
    fn no_text_offered_means_nothing_to_sync() {
        assert_eq!(select_text_mime(&types(&["image/png", "text/html"])), None);
        assert_eq!(
            select_text_mime(&types(&[PASSWORD_MANAGER_HINT_MIME_TYPE])),
            None
        );
        assert_eq!(select_text_mime(&[]), None);
    }

    #[test]
    fn password_hint_detection() {
        let keepass = types(&[
            "text/plain;charset=utf-8",
            "UTF8_STRING",
            PASSWORD_MANAGER_HINT_MIME_TYPE,
        ]);
        assert!(offers_password_hint(&keepass));
        assert!(!offers_password_hint(&types(&[
            "text/plain",
            "UTF8_STRING"
        ])));
        assert!(hint_marks_secret(b"secret"));
        assert!(hint_marks_secret(b"secret\n"));
        assert!(!hint_marks_secret(b""));
        assert!(!hint_marks_secret(b"public"));
        assert!(!hint_marks_secret(b"secrets"));
    }

    #[test]
    fn read_limited_skips_over_one_mib() {
        let exact = vec![b'a'; MAX_SYNC_BYTES];
        let got = read_limited(std::io::Cursor::new(exact), MAX_SYNC_BYTES, READ_TIMEOUT).unwrap();
        assert!(matches!(got, Limited::Data(ref v) if v.len() == MAX_SYNC_BYTES));

        let over = vec![b'a'; MAX_SYNC_BYTES + 1];
        let got = read_limited(std::io::Cursor::new(over), MAX_SYNC_BYTES, READ_TIMEOUT).unwrap();
        assert_eq!(got, Limited::TooLarge);
    }

    #[test]
    fn read_limited_times_out_on_a_stalled_source() {
        struct Stalled;
        impl Read for Stalled {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                std::thread::sleep(Duration::from_millis(500));
                Ok(0)
            }
        }
        assert!(read_limited(Stalled, 16, Duration::from_millis(50)).is_err());
    }

    #[test]
    fn invalid_utf8_is_decoded_lossily() {
        assert_eq!(decode(b"ok".to_vec()), "ok");
        assert_eq!(decode(vec![b'a', 0xff, b'b']), "a\u{fffd}b");
    }

    #[test]
    fn session_detection() {
        let s = |v: &'static str| Some(OsStr::new(v));
        assert_eq!(
            session_kind(s("wayland-1"), s(":1"), s("wayland")),
            Session::Wayland { x11_too: true }
        );
        assert_eq!(
            session_kind(s("wayland-0"), None, None),
            Session::Wayland { x11_too: false }
        );
        assert_eq!(session_kind(None, s(":0"), s("x11")), Session::X11);
        assert_eq!(session_kind(None, None, s("x11")), Session::X11);
        assert_eq!(session_kind(s(""), s(""), None), Session::None);
        assert_eq!(session_kind(None, None, s("tty")), Session::None);
    }
}

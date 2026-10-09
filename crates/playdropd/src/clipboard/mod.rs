//! Clipboard watcher and applier (Phase 2 task c, SR-24, SR-32, protocol.md 5.7).
//!
//! The daemon talks to this module only through the API below; the backend is picked by
//! target and, on Linux, by a one-time runtime probe of the compositor:
//!
//! - `wayland.rs`: compositors that offer `ext-data-control-v1` or `zwlr-data-control-v1`
//!   (COSMIC, Sway, KDE Plasma, Hyprland), through `wl-clipboard-rs`.
//! - `windows.rs`: `AddClipboardFormatListener` on a message-only window (untested here).
//! - `unsupported.rs`: everything else, always with a sentence saying why (anti-pattern 18).
//!   GNOME Mutter offers no data-control protocol, so GNOME users get the in-window "Send
//!   clipboard" button instead of automatic sync.
//!
//! Rules every backend follows:
//!
//! - **SR-24 auto-sync skip.** The watcher never reports a clip marked by a password manager
//!   (`x-kde-passwordManagerHint` = `secret` on Linux; the exclusion formats on Windows).
//!   [`read_current`] reports the same mark as `sensitive: true` so the daemon can ask first and
//!   send with `sensitive=true`. [`apply`] with `sensitive` writes the same marks so clipboard
//!   managers and Windows history and cloud upload skip it.
//! - **Size.** Text over [`MAX_SYNC_BYTES`] (1 MiB, SR-24) is never synced. Text over
//!   [`MAX_INLINE_BYTES`] (32 KiB) is still reported: the core answers `TooLarge` until blob
//!   transfer exists and the daemon tells the user, which is better than silently dropping it.
//! - **Coalescing.** The watcher reports at most one clip per [`MIN_REPORT_GAP`] (the last one
//!   of a burst wins) and never repeats the text it reported last, nor the text [`apply`] wrote
//!   last. The core keeps the authoritative echo rule by hash (protocol.md 5.7); this only saves
//!   work.
//! - **SR-32.** Clipboard text is never logged. Debug logs carry lengths and the first 8 hex
//!   characters of a BLAKE3 hash.

mod coalesce;
mod unsupported;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(windows)]
mod windows;

#[cfg(not(any(target_os = "linux", windows)))]
use self::unsupported as backend;
#[cfg(target_os = "linux")]
use self::wayland as backend;
#[cfg(windows)]
use self::windows as backend;

use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Instant;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use self::coalesce::{Coalescer, Poll};

/// Largest text the watcher reports and [`read_current`] returns (SR-24: 1 MiB).
pub const MAX_SYNC_BYTES: usize = 1024 * 1024;
/// Largest text the core sends inline today (protocol.md 5.7); longer text needs blobs.
pub const MAX_INLINE_BYTES: usize = 32 * 1024;
/// Minimum time between two clips reported by the watcher.
pub const MIN_REPORT_GAP: std::time::Duration = std::time::Duration::from_millis(100);

/// Whether automatic clipboard sync works in this session, and why not when it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardSupport {
    /// `true` when [`Watcher::start`], [`read_current`] and [`apply`] can work.
    pub supported: bool,
    /// Backend name for diagnostics: `wl-data-control`, `clipboard-format-listener` or `none`.
    pub backend: &'static str,
    /// A sentence for the user when `supported` is `false`.
    pub reason: Option<String>,
}

/// A text clip read from the local clipboard.
#[derive(Clone, PartialEq, Eq)]
pub struct LocalClip {
    /// The text. Never log it (SR-32).
    pub text: String,
    /// The clip carries a password-manager mark (SR-24). Always `false` from the watcher,
    /// which skips such clips entirely.
    pub sensitive: bool,
}

impl std::fmt::Debug for LocalClip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SR-32: the text itself never reaches a formatter.
        f.debug_struct("LocalClip")
            .field("len", &self.text.len())
            .field("hash", &short_hash(&text_hash(&self.text)))
            .field("sensitive", &self.sensitive)
            .finish()
    }
}

/// Probes the session once and caches the answer. Never panics, even without a display.
pub fn support() -> ClipboardSupport {
    static SUPPORT: OnceLock<ClipboardSupport> = OnceLock::new();
    SUPPORT.get_or_init(backend::probe).clone()
}

/// A running clipboard watcher. Dropping it stops it as well.
pub struct Watcher {
    handle: backend::WatchHandle,
    task: JoinHandle<()>,
}

impl Watcher {
    /// Starts watching the regular clipboard and sends each new text clip to `sink`.
    ///
    /// Fails with the [`ClipboardSupport::reason`] sentence when the session cannot watch.
    pub async fn start(sink: mpsc::Sender<LocalClip>) -> anyhow::Result<Watcher> {
        let support = support();
        if !support.supported {
            return Err(unsupported::error(&support));
        }
        let (raw_tx, raw_rx) = mpsc::channel(8);
        let handle = backend::spawn_watcher(raw_tx).await?;
        let task = tokio::spawn(coalesce_task(raw_rx, sink));
        tracing::info!(backend = support.backend, "clipboard watcher started");
        Ok(Watcher { handle, task })
    }

    /// Stops the watcher promptly. Does not wait for the platform thread to exit.
    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.handle.stop();
        self.task.abort();
        tracing::debug!("clipboard watcher stopped");
    }
}

/// Reads the clipboard once, for the manual "Send clipboard" action.
///
/// Returns `None` when the clipboard holds no text, and an error naming the cause when the
/// session cannot read it or the text is over 1 MiB (SR-24).
///
/// Blocking (a few Wayland round trips, or a clipboard open on Windows): call it from
/// `tokio::task::spawn_blocking`.
pub fn read_current() -> anyhow::Result<Option<LocalClip>> {
    let support = support();
    if !support.supported {
        return Err(unsupported::error(&support));
    }
    let clip = backend::read_current()?;
    if let Some(clip) = &clip {
        tracing::debug!(
            len = clip.text.len(),
            hash = %short_hash(&text_hash(&clip.text)),
            sensitive = clip.sensitive,
            "clipboard read"
        );
    }
    Ok(clip)
}

/// Writes `text` to the clipboard. With `sensitive`, also writes the password-manager marks so
/// clipboard history, cloud upload and other watchers skip it (SR-24).
///
/// Blocking like [`read_current`]. On Linux a crate-owned thread keeps serving the text until
/// the next selection replaces it; a second `apply` ends the first one's thread.
pub fn apply(text: &str, sensitive: bool) -> anyhow::Result<()> {
    let support = support();
    if !support.supported {
        return Err(unsupported::error(&support));
    }
    if text.len() > MAX_SYNC_BYTES {
        anyhow::bail!("clipboard text is larger than 1 MiB and is not synced");
    }
    let hash = text_hash(text);
    remember_applied(hash);
    backend::apply(text, sensitive)?;
    tracing::debug!(len = text.len(), hash = %short_hash(&hash), sensitive, "clipboard applied");
    Ok(())
}

/// Turns raw clips from the platform thread into at most one report per [`MIN_REPORT_GAP`].
async fn coalesce_task(mut raw: mpsc::Receiver<LocalClip>, sink: mpsc::Sender<LocalClip>) {
    let mut coalescer = Coalescer::new(MIN_REPORT_GAP);
    loop {
        let incoming = match coalescer.poll(Instant::now()) {
            Poll::Ready(clip) => {
                tracing::debug!(len = clip.text.len(), "clipboard change reported");
                if sink.send(clip).await.is_err() {
                    break;
                }
                continue;
            }
            Poll::Wait(deadline) => {
                tokio::select! {
                    msg = raw.recv() => msg,
                    () = tokio::time::sleep_until(deadline.into()) => continue,
                }
            }
            Poll::Idle => raw.recv().await,
        };
        let Some(clip) = incoming else { break };
        let hash = text_hash(&clip.text);
        if is_last_applied(&hash) {
            tracing::debug!(hash = %short_hash(&hash), "clipboard change is our own apply; skipped");
            continue;
        }
        coalescer.push(clip, hash);
    }
}

/// What the size rules say about a text of `len` UTF-8 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SizeVerdict {
    /// Fits inline in a clipboard frame.
    Inline,
    /// Over 32 KiB: still reported; the core answers `TooLarge` until blobs exist.
    NeedsBlob,
    /// Over 1 MiB: never synced (SR-24).
    TooLarge,
}

pub(crate) fn size_verdict(len: usize) -> SizeVerdict {
    if len > MAX_SYNC_BYTES {
        SizeVerdict::TooLarge
    } else if len > MAX_INLINE_BYTES {
        SizeVerdict::NeedsBlob
    } else {
        SizeVerdict::Inline
    }
}

/// BLAKE3 of the text with CRLF and lone CR turned into LF, the same normalisation as the wire
/// hash (protocol.md 5.7), so a Windows line-ending rewrite still matches.
pub(crate) fn text_hash(text: &str) -> blake3::Hash {
    if !text.contains('\r') {
        return blake3::hash(text.as_bytes());
    }
    let mut hasher = blake3::Hasher::new();
    let mut rest = text.as_bytes();
    while let Some(pos) = rest.iter().position(|&b| b == b'\r') {
        hasher.update(&rest[..pos]);
        hasher.update(b"\n");
        rest = &rest[pos + 1..];
        if rest.first() == Some(&b'\n') {
            rest = &rest[1..];
        }
    }
    hasher.update(rest);
    hasher.finalize()
}

/// First 8 hex characters of a hash, the only form a clip may take in logs (SR-32).
pub(crate) fn short_hash(hash: &blake3::Hash) -> String {
    hash.to_hex()[..8].to_string()
}

static LAST_APPLIED: Mutex<Option<blake3::Hash>> = Mutex::new(None);

fn remember_applied(hash: blake3::Hash) {
    *LAST_APPLIED.lock().unwrap_or_else(PoisonError::into_inner) = Some(hash);
}

fn is_last_applied(hash: &blake3::Hash) -> bool {
    LAST_APPLIED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        == Some(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_limits() {
        assert_eq!(size_verdict(0), SizeVerdict::Inline);
        assert_eq!(size_verdict(MAX_INLINE_BYTES), SizeVerdict::Inline);
        assert_eq!(size_verdict(MAX_INLINE_BYTES + 1), SizeVerdict::NeedsBlob);
        assert_eq!(size_verdict(MAX_SYNC_BYTES), SizeVerdict::NeedsBlob);
        assert_eq!(size_verdict(MAX_SYNC_BYTES + 1), SizeVerdict::TooLarge);
    }

    #[test]
    fn hash_normalises_line_endings() {
        let lf = text_hash("a\nb\nc\n");
        assert_eq!(text_hash("a\r\nb\rc\r\n"), lf);
        assert_eq!(text_hash("a\nb\nc\n"), lf);
        assert_ne!(text_hash("a\n\nb\nc\n"), lf);
        assert_eq!(text_hash("x\r"), text_hash("x\n"));
        assert_eq!(text_hash("\r\n\r\n"), text_hash("\n\n"));
    }

    #[test]
    fn debug_never_prints_text() {
        let clip = LocalClip {
            text: "canary-hunter2".into(),
            sensitive: false,
        };
        let shown = format!("{clip:?}");
        assert!(!shown.contains("canary"));
        assert!(shown.contains("len: 14"));
    }

    #[test]
    fn short_hash_is_eight_hex() {
        let h = short_hash(&text_hash("abc"));
        assert_eq!(h.len(), 8);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn support_never_panics() {
        let s = support();
        assert_eq!(s.supported, s.reason.is_none());
    }

    /// Writes a clip and reads it back through the real compositor. Needs a Wayland session
    /// with data-control (COSMIC, Sway). Run with
    /// `cargo test -p playdropd -- --ignored clipboard`. It overwrites the clipboard and puts
    /// the previous text back afterwards.
    #[test]
    #[ignore = "needs a Wayland session with data-control; overwrites the clipboard"]
    fn clipboard_apply_then_read_current_round_trip() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() && !cfg!(windows) {
            eprintln!("no WAYLAND_DISPLAY; skipping");
            return;
        }
        let s = support();
        assert!(s.supported, "clipboard unsupported here: {:?}", s.reason);
        let previous = read_current().expect("read before");

        let text = format!("playdrop clipboard test {}", std::process::id());
        apply(&text, false).expect("apply");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let got = read_current().expect("read").expect("some text");
        assert_eq!(got.text, text);
        assert!(!got.sensitive);

        apply(&text, true).expect("apply sensitive");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let got = read_current().expect("read").expect("some text");
        assert_eq!(got.text, text);
        assert!(got.sensitive, "password-manager hint should round-trip");

        if let Some(prev) = previous {
            apply(&prev.text, prev.sensitive).expect("restore");
        }
    }
}

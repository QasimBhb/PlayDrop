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
//! - **Images.** When the clipboard holds no text but offers `image/png`, the watcher reports
//!   the image (only while the caller's image switch is on), with the same SR-24 skip. Other
//!   image formats are not converted: every desktop and phone clipboard offers PNG.
//! - **Size.** Text over [`MAX_SYNC_BYTES`] (1 MiB, SR-24) and images over
//!   [`MAX_IMAGE_BYTES`] (16 MiB, protocol.md 5.15) are never synced, and never held in memory
//!   whole. Text over 32 KiB and every image travel as a blob the peer fetches (core).
//! - **Coalescing.** The watcher reports at most one clip per [`MIN_REPORT_GAP`] (the last one
//!   of a burst wins) and never repeats the clip it reported last, nor the clip [`apply`] or
//!   [`apply_image`] wrote last. The core keeps the authoritative echo rule by hash
//!   (protocol.md 5.7); this only saves work.
//! - **SR-32.** Clipboard content is never logged. Debug logs carry kinds, lengths and the
//!   first 8 hex characters of a BLAKE3 hash.

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

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use self::coalesce::{Coalescer, Poll};

/// Largest text the watcher reports and [`read_current`] returns (SR-24: 1 MiB).
pub const MAX_SYNC_BYTES: usize = 1024 * 1024;
/// Largest image the watcher reports and [`read_current`] returns (protocol.md 5.15: the
/// core refuses image blobs over 16 MiB).
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
/// The image format read from and offered to the local clipboard.
pub const IMAGE_MIME: &str = "image/png";
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

/// What a clip holds. Never log the content (SR-32); `Debug` shows kind and length only.
#[derive(Clone, PartialEq, Eq)]
pub enum ClipContent {
    /// Text, at most [`MAX_SYNC_BYTES`].
    Text(String),
    /// An image, at most [`MAX_IMAGE_BYTES`].
    Image {
        /// MIME type, [`IMAGE_MIME`] from the watcher.
        mime: String,
        /// The encoded image.
        bytes: Vec<u8>,
    },
}

impl ClipContent {
    /// Size in bytes (UTF-8 for text).
    pub fn len(&self) -> usize {
        match self {
            Self::Text(t) => t.len(),
            Self::Image { bytes, .. } => bytes.len(),
        }
    }

    /// `text` or `image`, for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Image { .. } => "image",
        }
    }

    /// The hash the dedup rules compare: [`text_hash`] for text, BLAKE3 of the bytes for an
    /// image.
    pub(crate) fn hash(&self) -> blake3::Hash {
        match self {
            Self::Text(t) => text_hash(t),
            Self::Image { bytes, .. } => blake3::hash(bytes),
        }
    }
}

impl std::fmt::Debug for ClipContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SR-32: the content itself never reaches a formatter.
        write!(f, "{}({} bytes)", self.kind(), self.len())
    }
}

/// A clip read from the local clipboard.
#[derive(Clone, PartialEq, Eq)]
pub struct LocalClip {
    /// Text or image. Never log it (SR-32).
    pub content: ClipContent,
    /// The clip carries a password-manager mark (SR-24). Always `false` from the watcher,
    /// which skips such clips entirely.
    pub sensitive: bool,
}

#[cfg(test)]
impl LocalClip {
    /// A text clip.
    pub fn text(text: impl Into<String>, sensitive: bool) -> Self {
        Self {
            content: ClipContent::Text(text.into()),
            sensitive,
        }
    }

    /// An image clip.
    pub fn image(mime: impl Into<String>, bytes: Vec<u8>, sensitive: bool) -> Self {
        Self {
            content: ClipContent::Image {
                mime: mime.into(),
                bytes,
            },
            sensitive,
        }
    }
}

impl std::fmt::Debug for LocalClip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SR-32: the content itself never reaches a formatter.
        f.debug_struct("LocalClip")
            .field("kind", &self.content.kind())
            .field("len", &self.content.len())
            .field("hash", &short_hash(&self.content.hash()))
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
    /// Starts watching the regular clipboard and sends each new clip to `sink`: text, or an
    /// image while `images` is true (read at each change, so the switch applies at once).
    ///
    /// Fails with the [`ClipboardSupport::reason`] sentence when the session cannot watch.
    pub async fn start(
        sink: mpsc::Sender<LocalClip>,
        images: Arc<AtomicBool>,
    ) -> anyhow::Result<Watcher> {
        let support = support();
        if !support.supported {
            return Err(unsupported::error(&support));
        }
        // Few slots: an image clip can be 16 MiB, and the coalescer keeps only the newest.
        let (raw_tx, raw_rx) = mpsc::channel(2);
        let handle = backend::spawn_watcher(raw_tx, images).await?;
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

/// Reads the clipboard once, for the manual "Send clipboard" action: text, or a PNG image
/// when there is no text and `images` is true.
///
/// Returns `None` when the clipboard holds nothing we sync, and an error naming the cause
/// when the session cannot read it or the clip is over its cap (text 1 MiB, SR-24; images
/// 16 MiB).
///
/// Blocking (a few Wayland round trips, or a clipboard open on Windows): call it from
/// `tokio::task::spawn_blocking`.
pub fn read_current(images: bool) -> anyhow::Result<Option<LocalClip>> {
    let support = support();
    if !support.supported {
        return Err(unsupported::error(&support));
    }
    let clip = backend::read_current(images)?;
    if let Some(clip) = &clip {
        tracing::debug!(
            kind = clip.content.kind(),
            len = clip.content.len(),
            hash = %short_hash(&clip.content.hash()),
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

/// Writes an image to the clipboard, with the same SR-24 marks as [`apply`] when
/// `sensitive`. Only `image/*` types up to [`MAX_IMAGE_BYTES`]; Windows takes PNG only.
///
/// Blocking like [`apply`].
pub fn apply_image(bytes: &[u8], mime: &str, sensitive: bool) -> anyhow::Result<()> {
    let support = support();
    if !support.supported {
        return Err(unsupported::error(&support));
    }
    if !mime.starts_with("image/") {
        anyhow::bail!("the received clip is not an image");
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        anyhow::bail!("clipboard image is larger than 16 MiB and is not synced");
    }
    let hash = blake3::hash(bytes);
    remember_applied(hash);
    backend::apply_image(bytes, mime, sensitive)?;
    tracing::debug!(len = bytes.len(), hash = %short_hash(&hash), sensitive, "clipboard image applied");
    Ok(())
}

/// Turns raw clips from the platform thread into at most one report per [`MIN_REPORT_GAP`].
async fn coalesce_task(mut raw: mpsc::Receiver<LocalClip>, sink: mpsc::Sender<LocalClip>) {
    let mut coalescer = Coalescer::new(MIN_REPORT_GAP);
    loop {
        let incoming = match coalescer.poll(Instant::now()) {
            Poll::Ready(clip) => {
                tracing::debug!(
                    kind = clip.content.kind(),
                    len = clip.content.len(),
                    "clipboard change reported"
                );
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
        let hash = clip.content.hash();
        if is_last_applied(&hash) {
            tracing::debug!(hash = %short_hash(&hash), "clipboard change is our own apply; skipped");
            continue;
        }
        coalescer.push(clip, hash);
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
    fn hash_normalises_line_endings() {
        let lf = text_hash("a\nb\nc\n");
        assert_eq!(text_hash("a\r\nb\rc\r\n"), lf);
        assert_eq!(text_hash("a\nb\nc\n"), lf);
        assert_ne!(text_hash("a\n\nb\nc\n"), lf);
        assert_eq!(text_hash("x\r"), text_hash("x\n"));
        assert_eq!(text_hash("\r\n\r\n"), text_hash("\n\n"));
    }

    #[test]
    fn debug_never_prints_content() {
        let clip = LocalClip::text("canary-hunter2", false);
        let shown = format!("{clip:?}");
        assert!(!shown.contains("canary"));
        assert!(shown.contains("len: 14") && shown.contains("text"));

        let image = LocalClip::image(IMAGE_MIME, b"\x89PNG-canary".to_vec(), true);
        let shown = format!("{image:?}");
        assert!(!shown.contains("canary") && !shown.contains("137"));
        assert!(shown.contains("image") && shown.contains("len: 11"));
        let content = format!("{:?}", image.content);
        assert_eq!(content, "image(11 bytes)");
    }

    #[test]
    fn content_hashes_and_sizes() {
        let image = ClipContent::Image {
            mime: IMAGE_MIME.into(),
            bytes: b"abc".to_vec(),
        };
        assert_eq!(image.hash(), blake3::hash(b"abc"));
        let crlf = ClipContent::Text("a\r\nb".into());
        assert_eq!(crlf.hash(), text_hash("a\nb"));
        assert_eq!(image.len(), 3);
        assert_eq!(image.kind(), "image");
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
        let previous = read_current(false).expect("read before");

        let text = format!("playdrop clipboard test {}", std::process::id());
        apply(&text, false).expect("apply");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let got = read_current(false).expect("read").expect("some text");
        assert_eq!(got.content, ClipContent::Text(text.clone()));
        assert!(!got.sensitive);

        apply(&text, true).expect("apply sensitive");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let got = read_current(false).expect("read").expect("some text");
        assert_eq!(got.content, ClipContent::Text(text.clone()));
        assert!(got.sensitive, "password-manager hint should round-trip");

        let png = b"\x89PNG\r\n\x1a\nnot really a png".to_vec();
        apply_image(&png, IMAGE_MIME, false).expect("apply image");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let got = read_current(true).expect("read").expect("an image");
        assert_eq!(
            got.content,
            ClipContent::Image {
                mime: IMAGE_MIME.into(),
                bytes: png
            }
        );

        if let Some(prev) = previous {
            match prev.content {
                ClipContent::Text(t) => apply(&t, prev.sensitive).expect("restore"),
                ClipContent::Image { mime, bytes } => {
                    apply_image(&bytes, &mime, prev.sensitive).expect("restore");
                }
            }
        }
    }
}

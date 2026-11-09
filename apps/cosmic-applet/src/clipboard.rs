//! Reads the clipboard text once, for "Send clipboard".
//!
//! The IPC has no "send your clipboard to this device" request (the tray's action reads the
//! clipboard inside the daemon), so the applet reads it here through the Wayland data-control
//! protocol, like the daemon does, and sends the text with `send_clipboard`. Same rules as the
//! daemon: at most 1 MiB of text, and a clip that carries the password-manager hint goes out
//! marked sensitive (SR-24). The text is never logged (SR-32).

use std::io::Read;
use std::time::Duration;

use wl_clipboard_rs::paste::{self, ClipboardType, MimeType, Seat};
use wl_clipboard_rs::utils::PASSWORD_MANAGER_HINT_MIME_TYPE;

/// Largest text the daemon accepts in one clip (`docs/ipc.md`, SR-24).
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// Longest wait for the app that owns the clipboard to hand over its data.
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// The hint's value is the word `secret`; anything longer is something else.
const HINT_LIMIT: usize = 64;
/// Text MIME types, best first.
const TEXT_MIME_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

/// Clipboard text and whether a password manager marked it.
#[derive(Clone)]
pub struct Clip {
    /// The text. Never logged.
    pub text: String,
    /// The password-manager hint was present (SR-24).
    pub sensitive: bool,
}

impl std::fmt::Debug for Clip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Clip")
            .field("len", &self.text.len())
            .field("sensitive", &self.sensitive)
            .finish()
    }
}

/// Why the clipboard could not be sent, worded for the popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// Nothing to send: empty, or no text in it.
    NoText,
    /// More than [`MAX_TEXT_BYTES`].
    TooLarge,
    /// The session does not allow reading it, or the owning app did not answer.
    Unreadable(String),
}

impl ReadError {
    /// The sentence the popup shows.
    pub fn message(&self) -> String {
        match self {
            Self::NoText => "Clipboard not sent: there is no text on the clipboard.".to_owned(),
            Self::TooLarge => {
                "Clipboard not sent: the text is larger than 1 MiB. Send it as a file instead."
                    .to_owned()
            }
            Self::Unreadable(why) => format!("Could not read the clipboard: {why}"),
        }
    }
}

/// Picks the best text MIME type among those offered, spelled exactly as the source offered
/// it: some apps match the requested type byte for byte.
pub fn pick_text_mime(offered: &[String]) -> Option<&str> {
    TEXT_MIME_TYPES.into_iter().find_map(|want| {
        offered
            .iter()
            .find(|o| o.eq_ignore_ascii_case(want))
            .map(String::as_str)
    })
}

/// Whether a clip counts as sensitive (SR-24): the source offers the password-manager hint
/// and its value is the word `secret`. `hint` is `None` when the hint could not be read, which
/// fails closed (sensitive).
pub fn is_sensitive(offered: &[String], hint: Option<&[u8]>) -> bool {
    offered.iter().any(|t| t == PASSWORD_MANAGER_HINT_MIME_TYPE)
        && hint.is_none_or(|value| value.trim_ascii() == b"secret")
}

/// Reads the clipboard text. Blocking (Wayland round trips): call it from
/// `tokio::task::spawn_blocking`.
pub fn read_text() -> Result<Clip, ReadError> {
    let types = match paste::get_mime_types_ordered(ClipboardType::Regular, Seat::Unspecified) {
        Ok(t) => t,
        Err(paste::Error::ClipboardEmpty | paste::Error::NoSeats | paste::Error::NoMimeType) => {
            return Err(ReadError::NoText)
        }
        Err(e) => return Err(unreadable(&e)),
    };
    let Some(mime) = pick_text_mime(&types).map(str::to_owned) else {
        return Err(ReadError::NoText);
    };
    let hint = if types.iter().any(|t| t == PASSWORD_MANAGER_HINT_MIME_TYPE) {
        read_hint()
    } else {
        None
    };
    let sensitive = is_sensitive(&types, hint.as_deref());
    let pipe = match paste::get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        MimeType::Specific(&mime),
    ) {
        Ok((pipe, _)) => pipe,
        Err(paste::Error::ClipboardEmpty | paste::Error::NoSeats | paste::Error::NoMimeType) => {
            return Err(ReadError::NoText)
        }
        Err(e) => return Err(unreadable(&e)),
    };
    let bytes = read_limited(pipe, MAX_TEXT_BYTES)?;
    let text = decode(bytes);
    if text.is_empty() {
        return Err(ReadError::NoText);
    }
    Ok(Clip { text, sensitive })
}

fn unreadable(e: &paste::Error) -> ReadError {
    let why = match e {
        paste::Error::MissingProtocol { .. } => {
            "this desktop does not let other apps read the clipboard".to_owned()
        }
        paste::Error::WaylandConnection(_) => "no Wayland session".to_owned(),
        other => other.to_string(),
    };
    ReadError::Unreadable(why)
}

/// The hint's value, or `None` when it cannot be read (which [`is_sensitive`] treats as
/// `secret`).
fn read_hint() -> Option<Vec<u8>> {
    paste::get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        MimeType::Specific(PASSWORD_MANAGER_HINT_MIME_TYPE),
    )
    .ok()
    .and_then(|(pipe, _)| read_limited(pipe, HINT_LIMIT).ok())
}

/// Text as UTF-8, replacing invalid sequences rather than refusing the clip.
pub fn decode(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Reads at most `limit` bytes within [`READ_TIMEOUT`]; more is [`ReadError::TooLarge`]. The
/// read runs on its own thread so an app that never closes the pipe cannot hang the applet.
fn read_limited<R: Read + Send + 'static>(reader: R, limit: usize) -> Result<Vec<u8>, ReadError> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("pd-applet-clip".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
            let result = reader.take(cap).read_to_end(&mut buf).map(|_| buf);
            let _ = tx.send(result);
        })
        .map_err(|e| ReadError::Unreadable(e.to_string()))?;
    let bytes = rx
        .recv_timeout(READ_TIMEOUT)
        .map_err(|_| {
            ReadError::Unreadable("the app holding the clipboard did not answer in time".into())
        })?
        .map_err(|e| ReadError::Unreadable(e.to_string()))?;
    if bytes.len() > limit {
        Err(ReadError::TooLarge)
    } else {
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn picks_utf8_text_first_and_ignores_images() {
        assert_eq!(
            pick_text_mime(&types(&["text/plain", "text/plain;charset=utf-8"])),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(
            pick_text_mime(&types(&["UTF8_STRING", "text/plain"])),
            Some("UTF8_STRING")
        );
        assert_eq!(pick_text_mime(&types(&["image/png"])), None);
    }

    #[test]
    fn requests_the_type_spelled_as_offered() {
        assert_eq!(
            pick_text_mime(&types(&["text/plain;charset=UTF-8"])),
            Some("text/plain;charset=UTF-8")
        );
    }

    #[test]
    fn password_manager_hint_makes_a_clip_sensitive_and_fails_closed() {
        let plain = types(&["text/plain"]);
        let hinted = types(&["text/plain", PASSWORD_MANAGER_HINT_MIME_TYPE]);
        assert!(!is_sensitive(&plain, None));
        assert!(!is_sensitive(&plain, Some(b"secret")));
        assert!(is_sensitive(&hinted, Some(b"secret")));
        assert!(is_sensitive(&hinted, Some(b" secret\n")));
        assert!(!is_sensitive(&hinted, Some(b"public")));
        // Offered but unreadable: treat it as secret.
        assert!(is_sensitive(&hinted, None));
    }

    #[test]
    fn reads_up_to_the_limit_and_refuses_more() {
        let small = read_limited(std::io::Cursor::new(vec![b'a'; 10]), 10).expect("fits");
        assert_eq!(small.len(), 10);
        assert_eq!(
            read_limited(std::io::Cursor::new(vec![b'a'; 11]), 10),
            Err(ReadError::TooLarge)
        );
    }

    #[test]
    fn invalid_utf8_is_replaced_not_refused() {
        assert_eq!(decode(vec![b'h', 0xff, b'i']), "h\u{fffd}i");
    }

    #[test]
    fn debug_never_shows_the_text() {
        let c = Clip {
            text: "hunter2".into(),
            sensitive: true,
        };
        let shown = format!("{c:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("len: 7"));
    }

    #[test]
    fn messages_are_sentences() {
        for e in [
            ReadError::NoText,
            ReadError::TooLarge,
            ReadError::Unreadable("x".into()),
        ] {
            let m = e.message();
            assert!(m.chars().next().is_some_and(char::is_uppercase), "{m}");
        }
    }
}

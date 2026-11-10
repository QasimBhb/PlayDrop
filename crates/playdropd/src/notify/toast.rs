//! "Open" and "Show in folder" on the Windows "received" toast: the activation arguments
//! and the table of toasts that may still be answered.
//!
//! How a click reaches the daemon: the toast is shown by this process and carries an
//! in-process `ToastNotification.Activated` handler, which Windows raises in this process
//! while it runs ("Apps that are running subscribe to this event", `ToastNotification`
//! docs). No COM activator, CLSID or command line entry point is registered, so a click
//! after the daemon has exited does nothing; that case is left out on purpose.
//!
//! What a click may do (threat model B3, SR-23, SR-27 spirit, SR-32):
//! - Each toast gets a fresh 128-bit random token. A button's argument string is
//!   `playdrop-toast:1:<32 hex token>:<open|show>` and nothing else: no path, no label.
//! - [`parse`] is strict: an exact prefix, exactly 32 lowercase hex digits, one of two
//!   button names, no trailing data. Anything else is refused.
//! - [`Pending::resolve`] answers only the token of the toast whose handler is running,
//!   and only while this daemon has it registered and unanswered; it forgets it on first
//!   use. The path a button acts
//!   on comes from the daemon's own note (the files it reported), never from the argument.
//! - Arguments and tokens are never logged; refusals are logged at debug without them.
//!
//! The buttons ship behind [`SWITCH_ENV`] (off by default, PLAN.md row 7: each later item
//! ships behind a flag until proven), because nobody has clicked one yet.

// Only the Windows toast uses these outside tests.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::{Mutex, PoisonError};

/// Set to `1` to give the Windows "received" toast its "Open" and "Show in folder"
/// buttons. Any other value, or none, keeps the button-less toast.
pub const SWITCH_ENV: &str = "PLAYDROP_TOAST_BUTTONS";

/// The fixed start of every argument string this daemon puts on a toast button.
const PREFIX: &str = "playdrop-toast:1:";

/// Bytes in a token.
const TOKEN_LEN: usize = 16;

/// True when the switch value turns the buttons on: exactly `1`.
pub fn switch_on(value: Option<&OsStr>) -> bool {
    value.is_some_and(|v| v == "1")
}

/// A per-toast random token.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Token([u8; TOKEN_LEN]);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never printed, even at debug (SR-32: tokens are not logged).
        f.write_str("Token(..)")
    }
}

impl Token {
    /// A token from fixed bytes (tests).
    #[cfg(test)]
    pub fn from_bytes(bytes: [u8; TOKEN_LEN]) -> Self {
        Self(bytes)
    }

    /// A fresh token from the system random number generator.
    #[cfg(windows)]
    pub fn random() -> std::io::Result<Self> {
        use windows::Win32::Security::Cryptography::{
            BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        };
        let mut bytes = [0u8; TOKEN_LEN];
        // SAFETY: no algorithm handle with the system-preferred flag, and a valid, writable
        // buffer whose length the binding passes along.
        #[allow(unsafe_code)]
        let status = unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        if status.is_ok() {
            Ok(Self(bytes))
        } else {
            Err(std::io::Error::other(format!(
                "BCryptGenRandom failed: {:#x}",
                status.0
            )))
        }
    }

    fn hex(self) -> String {
        use std::fmt::Write as _;
        self.0.iter().fold(String::with_capacity(32), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }
}

/// A toast button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// "Open".
    Open,
    /// "Show in folder".
    Show,
}

impl Button {
    /// The action ID `notify` uses for this button (`Action::id`).
    pub fn id(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Show => "show",
        }
    }

    /// The button for an action ID, if it is one.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "open" => Some(Self::Open),
            "show" => Some(Self::Show),
            _ => None,
        }
    }
}

/// The argument string for one button of the toast with `token`.
pub fn arguments(token: Token, button: Button) -> String {
    format!("{PREFIX}{}:{}", token.hex(), button.id())
}

/// Why activation arguments were refused. Carries no part of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Not one of ours (another prefix, empty, a toast body click).
    #[error("not a PlayDrop toast argument")]
    Foreign,
    /// Our prefix but not a well-formed token and button.
    #[error("malformed PlayDrop toast argument")]
    Malformed,
}

/// Parses a button's activation arguments. Pure: no lookup, no I/O.
pub fn parse(args: &str) -> Result<(Token, Button), ParseError> {
    let rest = args.strip_prefix(PREFIX).ok_or(ParseError::Foreign)?;
    let (hex, button) = rest.split_once(':').ok_or(ParseError::Malformed)?;
    let button = Button::from_id(button).ok_or(ParseError::Malformed)?;
    if hex.len() != TOKEN_LEN * 2 {
        return Err(ParseError::Malformed);
    }
    let mut bytes = [0u8; TOKEN_LEN];
    for (i, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let hi = nibble(pair[0]).ok_or(ParseError::Malformed)?;
        let lo = nibble(pair[1]).ok_or(ParseError::Malformed)?;
        bytes[i] = (hi << 4) | lo;
    }
    Ok((Token(bytes), button))
}

/// A lowercase hex digit's value; uppercase is refused so each token has one spelling.
fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Toasts that may still be answered, by token, each with what an answer goes to.
#[derive(Debug)]
pub struct Pending<T> {
    map: Mutex<HashMap<Token, T>>,
}

impl<T> Default for Pending<T> {
    fn default() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }
}

impl<T> Pending<T> {
    /// Registers a toast about to be shown.
    pub fn insert(&self, token: Token, value: T) {
        self.lock().insert(token, value);
    }

    /// Forgets a toast (dismissed, failed, or its wait ended). Returns its value.
    pub fn remove(&self, token: Token) -> Option<T> {
        self.lock().remove(&token)
    }

    /// The value and button for a click on the toast with token `expected`, if `args` is
    /// ours, well formed, names `expected` and that token is registered. The token is
    /// forgotten, so each toast answers once. Arguments naming another toast's token are
    /// refused and leave that toast waiting: a click only ever answers its own toast.
    pub fn resolve(&self, expected: Token, args: &str) -> Result<(T, Button), Refused> {
        let (token, button) = parse(args).map_err(Refused::Parse)?;
        if token != expected {
            return Err(Refused::OtherToast);
        }
        let value = self.remove(token).ok_or(Refused::Unknown)?;
        Ok((value, button))
    }

    /// Toasts waiting.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Token, T>> {
        self.map.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Why an activation was ignored. Carries no part of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    /// The arguments did not parse.
    #[error(transparent)]
    Parse(ParseError),
    /// Well formed, but no toast with that token is waiting (answered, expired, never ours).
    #[error("no waiting PlayDrop toast has that token")]
    Unknown,
    /// Well formed, but names a different toast than the one clicked.
    #[error("the arguments name another toast")]
    OtherToast,
}

#[cfg(test)]
mod tests {
    use super::*;

    const T1: Token = Token([0x01; TOKEN_LEN]);
    const T2: Token = Token([0xab; TOKEN_LEN]);

    #[test]
    fn arguments_round_trip() {
        let a = arguments(T2, Button::Show);
        assert_eq!(a, "playdrop-toast:1:abababababababababababababababab:show");
        assert_eq!(parse(&a), Ok((T2, Button::Show)));
        assert_eq!(parse(&arguments(T1, Button::Open)), Ok((T1, Button::Open)));
        assert_eq!(Button::from_id(Button::Open.id()), Some(Button::Open));
    }

    #[test]
    fn valid_token_resolves_once() {
        let p = Pending::default();
        p.insert(T1, "toast one");
        p.insert(T2, "toast two");
        assert_eq!(
            p.resolve(T2, &arguments(T2, Button::Open)),
            Ok(("toast two", Button::Open))
        );
        // Answered: a second click (or a replay) is refused.
        assert_eq!(
            p.resolve(T2, &arguments(T2, Button::Show)),
            Err(Refused::Unknown)
        );
        assert_eq!(p.len(), 1);
        assert_eq!(p.remove(T1), Some("toast one"));
        assert_eq!(p.len(), 0);
    }

    #[test]
    fn unknown_token_is_refused() {
        let p = Pending::default();
        p.insert(T1, ());
        assert_eq!(
            p.resolve(T2, &arguments(T2, Button::Open)),
            Err(Refused::Unknown)
        );
        // The waiting toast is untouched.
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn another_toasts_token_is_refused_and_left_waiting() {
        let p = Pending::default();
        p.insert(T1, "toast one");
        p.insert(T2, "toast two");
        // A click on toast one whose arguments name toast two (registered and waiting).
        assert_eq!(
            p.resolve(T1, &arguments(T2, Button::Open)),
            Err(Refused::OtherToast)
        );
        assert_eq!(p.len(), 2);
        assert_eq!(
            p.resolve(T2, &arguments(T2, Button::Show)),
            Ok(("toast two", Button::Show))
        );
        assert_eq!(p.remove(T1), Some("toast one"));
    }

    #[test]
    fn malformed_input_is_refused() {
        let hex = "01".repeat(TOKEN_LEN);
        for bad in [
            String::new(),
            "open".to_owned(),
            "playdrop-toast:1:".to_owned(),
            format!("playdrop-toast:1:{hex}"),
            format!("playdrop-toast:1:{hex}:"),
            format!("playdrop-toast:1:{hex}:delete"),
            format!("playdrop-toast:1:{hex}:Open"),
            format!("playdrop-toast:1:{}:open", &hex[..30]),
            format!("playdrop-toast:1:{hex}00:open"),
            format!(
                "playdrop-toast:1:{}:open",
                hex.to_uppercase().replace("01", "0A")
            ),
            format!("playdrop-toast:1:{}zz:open", &hex[..30]),
            format!("playdrop-toast:1: {}:open", &hex[1..]),
            "playdrop-toast:1:é".repeat(3),
        ] {
            assert!(parse(&bad).is_err(), "accepted {bad:?}");
        }
        assert_eq!(parse("launch"), Err(ParseError::Foreign));
        assert_eq!(parse("playdrop-toast:2:x:open"), Err(ParseError::Foreign));
        assert_eq!(
            parse(&format!("playdrop-toast:1:{hex}:show:")),
            Err(ParseError::Malformed)
        );
    }

    #[test]
    fn smuggled_paths_are_refused() {
        let hex = "ab".repeat(TOKEN_LEN);
        let p = Pending::default();
        p.insert(T2, ());
        for bad in [
            format!("playdrop-toast:1:{hex}:open:C:\\Windows\\System32\\cmd.exe"),
            format!("playdrop-toast:1:{hex}:show:..\\..\\evil.lnk"),
            format!("playdrop-toast:1:{hex}:C:\\Users\\x\\a.exe"),
            "playdrop-toast:1:C:\\evil.exe:open".to_owned(),
            format!("playdrop-toast:1:{hex}:open\0C:\\evil.exe"),
            format!("playdrop-toast:1:{hex}:open\r\n/tmp/x"),
            "C:\\Users\\x\\Downloads\\PlayDrop\\a.exe".to_owned(),
            "file:///C:/Windows/System32/cmd.exe".to_owned(),
        ] {
            assert!(p.resolve(T2, &bad).is_err(), "accepted {bad:?}");
        }
        // None of those used up the real toast.
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn only_exactly_one_turns_the_switch_on() {
        assert!(switch_on(Some(OsStr::new("1"))));
        for off in ["", "0", "true", "yes", " 1", "1 "] {
            assert!(!switch_on(Some(OsStr::new(off))), "{off:?}");
        }
        assert!(!switch_on(None));
    }

    #[test]
    fn errors_and_tokens_do_not_echo_input() {
        assert_eq!(format!("{T1:?}"), "Token(..)");
        let e = parse("playdrop-toast:1:secret:open").unwrap_err();
        assert!(!e.to_string().contains("secret"));
    }
}

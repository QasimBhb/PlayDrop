//! Start at login inside Flatpak: `org.freedesktop.portal.Background.RequestBackground`.
//!
//! A sandboxed app cannot write `~/.config/autostart`; the portal writes the entry for us
//! (it runs `flatpak run --command=playdropd <app>` at login) after the desktop or the user
//! allows it. Asked with `autostart: false` it removes the entry again.
//!
//! *Choice:* plain `zbus`, like `input/linux/portal.rs`, because the daemon does not build
//! `ashpd`. Spec: <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Background.html>.
//!
//! A refusal is never silent (PLAN.md anti-pattern 18): the caller turns the setting back
//! off, says so in a notification and in `Diagnose`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt as _;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";

/// What the portal runs at login, inside the sandbox.
pub const COMMANDLINE: &str = "playdropd";

/// The reason the desktop shows the user.
pub const REASON: &str = "Start PlayDrop when you log in, so your phone can reach this computer.";

/// How long the desktop's question may stay open.
const ANSWER: Duration = Duration::from_secs(120);

#[zbus::proxy(
    interface = "org.freedesktop.portal.Background",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop",
    gen_blocking = false
)]
trait Background {
    fn request_background(
        &self,
        parent_window: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<OwnedObjectPath>;
}

/// The portal's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The entry is as asked: written for `autostart: true`, removed for `false`.
    Allowed,
    /// The user or the desktop refused, with a reason to show.
    Denied(String),
}

/// The `RequestBackground` options.
pub fn options(handle_token: &str, autostart: bool) -> HashMap<&str, Value<'_>> {
    HashMap::from([
        ("handle_token", Value::from(handle_token)),
        ("reason", Value::from(REASON)),
        ("autostart", Value::from(autostart)),
        ("commandline", Value::from(vec![COMMANDLINE])),
        ("dbus-activatable", Value::from(false)),
    ])
}

/// Reads the `Response` signal: `code` 0 is success, 1 the user cancelled, 2 anything
/// else. A success that reports `autostart: false` after we asked for `true` is a refusal
/// too (the desktop may allow running in the background but not at login).
pub fn interpret(
    code: u32,
    results: &HashMap<String, OwnedValue>,
    autostart: bool,
) -> anyhow::Result<Answer> {
    match code {
        0 => {
            let granted = results
                .get("autostart")
                .and_then(|v| bool::try_from(v).ok());
            if autostart && granted == Some(false) {
                Ok(Answer::Denied(
                    "the desktop allowed PlayDrop to run but not to start at login".to_owned(),
                ))
            } else {
                Ok(Answer::Allowed)
            }
        }
        1 => Ok(Answer::Denied("the request was turned down".to_owned())),
        n => anyhow::bail!("the Background portal ended the request (response {n})"),
    }
}

/// Asks the portal to add (`true`) or remove (`false`) the login entry and waits for the
/// answer. Fails when there is no portal or it never answers.
pub async fn request(autostart: bool) -> anyhow::Result<Answer> {
    let conn = zbus::Connection::session().await?;
    let proxy = BackgroundProxy::new(&conn).await?;
    let sender = conn
        .unique_name()
        .ok_or_else(|| anyhow::anyhow!("no unique bus name"))?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    let token = token();
    // Subscribe before calling: the path is predictable and a fast answer is not missed.
    let expected = OwnedObjectPath::try_from(format!("{DESKTOP_PATH}/request/{sender}/{token}"))?;
    let mut req = zbus::Proxy::new(&conn, DESKTOP, expected.clone(), REQUEST_IFACE).await?;
    let mut responses = req.receive_signal("Response").await?;
    let returned = proxy
        .request_background("", options(&token, autostart))
        .await?;
    if returned != expected {
        req = zbus::Proxy::new(&conn, DESKTOP, returned, REQUEST_IFACE).await?;
        responses = req.receive_signal("Response").await?;
    }
    let Ok(msg) = tokio::time::timeout(ANSWER, responses.next()).await else {
        let _ = req.call_method("Close", &()).await;
        return Ok(Answer::Denied(
            "nobody answered the question in time".to_owned(),
        ));
    };
    let msg = msg.ok_or_else(|| anyhow::anyhow!("the portal closed the request"))?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize()?;
    interpret(code, &results, autostart)
}

/// A fresh `handle_token` (an object path element).
fn token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "playdrop_bg_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn results(autostart: Option<bool>) -> HashMap<String, OwnedValue> {
        let mut m = HashMap::new();
        m.insert("background".to_owned(), OwnedValue::from(true));
        if let Some(a) = autostart {
            m.insert("autostart".to_owned(), OwnedValue::from(a));
        }
        m
    }

    #[test]
    fn options_ask_for_playdropd_at_login() {
        let o = options("tok", true);
        assert_eq!(o["handle_token"], Value::from("tok"));
        assert_eq!(o["autostart"], Value::from(true));
        assert_eq!(o["dbus-activatable"], Value::from(false));
        assert_eq!(o["commandline"], Value::from(vec!["playdropd"]));
        assert!(matches!(&o["reason"], Value::Str(s) if !s.is_empty()));
        assert_eq!(options("tok", false)["autostart"], Value::from(false));
    }

    #[test]
    fn answers_are_read_as_allowed_or_denied() {
        assert_eq!(
            interpret(0, &results(Some(true)), true).unwrap(),
            Answer::Allowed
        );
        // An old portal that leaves the key out: the request itself succeeded.
        assert_eq!(interpret(0, &results(None), true).unwrap(), Answer::Allowed);
        assert!(matches!(
            interpret(0, &results(Some(false)), true).unwrap(),
            Answer::Denied(_)
        ));
        // Removing the entry reports `autostart: false`, which is what we asked for.
        assert_eq!(
            interpret(0, &results(Some(false)), false).unwrap(),
            Answer::Allowed
        );
        assert!(matches!(
            interpret(1, &results(None), true).unwrap(),
            Answer::Denied(_)
        ));
        assert!(interpret(2, &results(None), true).is_err());
    }

    #[test]
    fn tokens_are_path_elements() {
        let (a, b) = (token(), token());
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }
}

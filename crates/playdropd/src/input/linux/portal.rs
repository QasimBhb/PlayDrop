//! The `org.freedesktop.portal.RemoteDesktop` calls the Linux backend needs, over `zbus`
//! directly.
//!
//! *Choice:* plain `zbus` rather than `ashpd`'s `RemoteDesktop` wrapper, because the
//! daemon builds `ashpd` without its `remote_desktop` feature. The surface used here is
//! small (three request calls, five notify calls, two properties) and follows the portal
//! spec: <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html>.
//!
//! Request calls (`CreateSession`, `SelectDevices`, `Start`) answer with a `Request`
//! object path and later a `Response` signal on it. The path is predictable from our
//! unique bus name and the `handle_token` we pass, so we subscribe before calling and
//! cannot miss a fast answer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt as _;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";
const SESSION_IFACE: &str = "org.freedesktop.portal.Session";

/// `AvailableDeviceTypes` bits.
pub(super) const DEVICE_KEYBOARD: u32 = 1;
/// `AvailableDeviceTypes` bits.
pub(super) const DEVICE_POINTER: u32 = 2;
/// `persist_mode` 2: the grant lasts until the user revokes it.
const PERSIST_UNTIL_REVOKED: u32 = 2;
/// Key and button states for the notify calls.
pub(super) const STATE_RELEASED: u32 = 0;
/// Key and button states for the notify calls.
pub(super) const STATE_PRESSED: u32 = 1;
/// `NotifyPointerAxisDiscrete` axes.
pub(super) const AXIS_VERTICAL: u32 = 0;
/// `NotifyPointerAxisDiscrete` axes.
pub(super) const AXIS_HORIZONTAL: u32 = 1;

/// How long the non-interactive requests may take before we give up.
const QUICK_ANSWER: Duration = Duration::from_secs(10);
/// How long `Start` waits for the user to answer the consent dialog.
const DIALOG_ANSWER: Duration = Duration::from_secs(120);

#[zbus::proxy(
    interface = "org.freedesktop.portal.RemoteDesktop",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop",
    gen_blocking = false
)]
trait RemoteDesktop {
    fn create_session(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;

    fn select_devices(
        &self,
        session_handle: &ObjectPath<'_>,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<OwnedObjectPath>;

    fn start(
        &self,
        session_handle: &ObjectPath<'_>,
        parent_window: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<OwnedObjectPath>;

    fn notify_pointer_motion(
        &self,
        session_handle: &ObjectPath<'_>,
        options: HashMap<&str, Value<'_>>,
        dx: f64,
        dy: f64,
    ) -> zbus::Result<()>;

    fn notify_pointer_button(
        &self,
        session_handle: &ObjectPath<'_>,
        options: HashMap<&str, Value<'_>>,
        button: i32,
        state: u32,
    ) -> zbus::Result<()>;

    fn notify_pointer_axis(
        &self,
        session_handle: &ObjectPath<'_>,
        options: HashMap<&str, Value<'_>>,
        dx: f64,
        dy: f64,
    ) -> zbus::Result<()>;

    fn notify_pointer_axis_discrete(
        &self,
        session_handle: &ObjectPath<'_>,
        options: HashMap<&str, Value<'_>>,
        axis: u32,
        steps: i32,
    ) -> zbus::Result<()>;

    fn notify_keyboard_keysym(
        &self,
        session_handle: &ObjectPath<'_>,
        options: HashMap<&str, Value<'_>>,
        keysym: i32,
        state: u32,
    ) -> zbus::Result<()>;

    #[zbus(property)]
    fn available_device_types(&self) -> zbus::Result<u32>;

    #[zbus(property, name = "version")]
    fn version(&self) -> zbus::Result<u32>;
}

/// One call into an open portal session.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Call {
    /// `NotifyPointerMotion`.
    Motion { dx: f64, dy: f64 },
    /// `NotifyPointerButton` with a Linux evdev button code.
    Button { code: i32, state: u32 },
    /// `NotifyPointerAxis`; `finish` marks the end of a scroll sequence.
    Axis { dx: f64, dy: f64, finish: bool },
    /// `NotifyPointerAxisDiscrete`.
    AxisDiscrete { axis: u32, steps: i32 },
    /// `NotifyKeyboardKeysym`.
    Keysym { keysym: i32, state: u32 },
}

/// Why opening a session failed, so the caller can tell a refusal from a fault and
/// decide what to do with the restore token.
#[derive(Debug)]
pub(super) enum OpenError {
    /// The user cancelled or denied the consent dialog.
    Cancelled,
    /// Nobody answered the dialog in time.
    NoAnswer,
    /// `SelectDevices` failed; with a restore token this usually means the token is bad.
    SelectDevices(anyhow::Error),
    /// Anything else.
    Other(anyhow::Error),
}

impl From<zbus::Error> for OpenError {
    fn from(e: zbus::Error) -> Self {
        Self::Other(e.into())
    }
}

/// What the portal advertises, for the support probe.
#[derive(Debug, Clone, Copy)]
pub(super) struct Facts {
    pub(super) version: u32,
    pub(super) device_types: u32,
}

/// Reads the portal's version and device types. Fails when nobody provides the
/// interface.
pub(super) async fn facts(conn: &zbus::Connection) -> zbus::Result<Facts> {
    let proxy = RemoteDesktopProxy::new(conn).await?;
    Ok(Facts {
        version: proxy.version().await?,
        device_types: proxy.available_device_types().await?,
    })
}

/// A started `RemoteDesktop` session.
pub(super) struct Session {
    proxy: RemoteDesktopProxy<'static>,
    path: OwnedObjectPath,
}

/// What [`Session::open`] returns besides the session.
pub(super) struct Opened {
    pub(super) session: Session,
    /// The new restore token, single use: it replaces the one passed in.
    pub(super) restore_token: Option<String>,
    /// The device types the user granted.
    pub(super) devices: u32,
}

/// A fresh token for `handle_token` and `session_handle_token` (an object path element:
/// ASCII letters, digits and `_`). Unique per connection, which is all the spec asks.
fn token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "playdrop_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// `:1.42` becomes `1_42`, as the portal spells our name in request and session paths.
pub(super) fn sender_element(unique_name: &str) -> String {
    unique_name.trim_start_matches(':').replace('.', "_")
}

impl Session {
    /// Creates, configures and starts a session for keyboard and pointer. `Start` shows
    /// the desktop's consent dialog unless `restore_token` is accepted.
    pub(super) async fn open(
        conn: &zbus::Connection,
        restore_token: Option<&str>,
    ) -> Result<Opened, OpenError> {
        let proxy = RemoteDesktopProxy::new(conn).await?;
        let sender = sender_element(
            conn.unique_name()
                .ok_or_else(|| OpenError::Other(anyhow::anyhow!("no unique bus name")))?
                .as_str(),
        );
        let version = proxy.version().await.unwrap_or(1);

        // CreateSession.
        let handle = token();
        let session_token = token();
        let results = request(conn, &sender, &handle, QUICK_ANSWER, async {
            proxy
                .create_session(HashMap::from([
                    ("handle_token", Value::from(handle.as_str())),
                    ("session_handle_token", Value::from(session_token.as_str())),
                ]))
                .await
        })
        .await?;
        let path = match results
            .get("session_handle")
            .and_then(|v| v.downcast_ref::<&str>().ok())
        {
            Some(p) => {
                OwnedObjectPath::try_from(p.to_owned()).map_err(|e| OpenError::Other(e.into()))?
            }
            None => OwnedObjectPath::try_from(format!(
                "{DESKTOP_PATH}/session/{sender}/{session_token}"
            ))
            .map_err(|e| OpenError::Other(e.into()))?,
        };
        let session = Session { proxy, path };

        match session
            .select_and_start(conn, &sender, version, restore_token)
            .await
        {
            Ok((restore_token, devices)) => Ok(Opened {
                session,
                restore_token,
                devices,
            }),
            Err(e) => {
                session.close().await;
                Err(e)
            }
        }
    }

    async fn select_and_start(
        &self,
        conn: &zbus::Connection,
        sender: &str,
        version: u32,
        restore_token: Option<&str>,
    ) -> Result<(Option<String>, u32), OpenError> {
        let handle = token();
        let mut options = HashMap::from([
            ("handle_token", Value::from(handle.as_str())),
            ("types", Value::from(DEVICE_KEYBOARD | DEVICE_POINTER)),
        ]);
        // persist_mode and restore_token arrived in version 2 of the interface.
        if version >= 2 {
            options.insert("persist_mode", Value::from(PERSIST_UNTIL_REVOKED));
            if let Some(t) = restore_token {
                options.insert("restore_token", Value::from(t));
            }
        }
        request(conn, sender, &handle, QUICK_ANSWER, async {
            self.proxy.select_devices(&self.path, options).await
        })
        .await
        .map_err(|e| match e {
            OpenError::Other(e) => OpenError::SelectDevices(e),
            e => e,
        })?;

        let handle = token();
        let results = request(conn, sender, &handle, DIALOG_ANSWER, async {
            self.proxy
                .start(
                    &self.path,
                    "",
                    HashMap::from([("handle_token", Value::from(handle.as_str()))]),
                )
                .await
        })
        .await?;
        let devices = results
            .get("devices")
            .and_then(|v| v.downcast_ref::<u32>().ok())
            .unwrap_or(0);
        let token = results
            .get("restore_token")
            .and_then(|v| v.downcast_ref::<&str>().ok())
            .map(str::to_owned);
        Ok((token, devices))
    }

    /// Sends one call.
    pub(super) async fn send(&self, call: Call) -> zbus::Result<()> {
        let path = &self.path;
        let none = HashMap::new;
        match call {
            Call::Motion { dx, dy } => self.proxy.notify_pointer_motion(path, none(), dx, dy).await,
            Call::Button { code, state } => {
                self.proxy
                    .notify_pointer_button(path, none(), code, state)
                    .await
            }
            Call::Axis { dx, dy, finish } => {
                let mut options = none();
                if finish {
                    options.insert("finish", Value::from(true));
                }
                self.proxy.notify_pointer_axis(path, options, dx, dy).await
            }
            Call::AxisDiscrete { axis, steps } => {
                self.proxy
                    .notify_pointer_axis_discrete(path, none(), axis, steps)
                    .await
            }
            Call::Keysym { keysym, state } => {
                self.proxy
                    .notify_keyboard_keysym(path, none(), keysym, state)
                    .await
            }
        }
    }

    /// Ends the session. Best effort: the portal also ends it when our bus connection
    /// closes.
    pub(super) async fn close(&self) {
        let result = async {
            zbus::Proxy::new(
                self.proxy.inner().connection(),
                DESKTOP,
                self.path.as_ref(),
                SESSION_IFACE,
            )
            .await?
            .call_method("Close", &())
            .await
        }
        .await;
        if let Err(e) = result {
            tracing::debug!(error = %e, "closing the RemoteDesktop session failed");
        }
    }
}

/// Runs one request call and waits for its `Response` signal. Subscribes before calling
/// (the path is known from `handle`), and follows a different path if an old portal
/// returns one.
async fn request<F>(
    conn: &zbus::Connection,
    sender: &str,
    handle: &str,
    wait: Duration,
    call: F,
) -> Result<HashMap<String, OwnedValue>, OpenError>
where
    F: std::future::Future<Output = zbus::Result<OwnedObjectPath>>,
{
    let expected = OwnedObjectPath::try_from(format!("{DESKTOP_PATH}/request/{sender}/{handle}"))
        .map_err(|e| OpenError::Other(e.into()))?;
    let mut req = zbus::Proxy::new(conn, DESKTOP, expected.clone(), REQUEST_IFACE).await?;
    let mut responses = req.receive_signal("Response").await?;
    let returned = call.await?;
    if returned != expected {
        req = zbus::Proxy::new(conn, DESKTOP, returned, REQUEST_IFACE).await?;
        responses = req.receive_signal("Response").await?;
    }
    let msg = if let Ok(m) = tokio::time::timeout(wait, responses.next()).await {
        m.ok_or_else(|| OpenError::Other(anyhow::anyhow!("the portal closed the request")))?
    } else {
        // Take the dialog off the screen; nobody is answering it.
        let _ = req.call_method("Close", &()).await;
        return Err(OpenError::NoAnswer);
    };
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize()?;
    match code {
        0 => Ok(results),
        1 => Err(OpenError::Cancelled),
        n => Err(OpenError::Other(anyhow::anyhow!(
            "the portal ended the request (response {n})"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_element_matches_portal_spelling() {
        assert_eq!(sender_element(":1.42"), "1_42");
        assert_eq!(sender_element(":1.2.3"), "1_2_3");
    }

    #[test]
    fn tokens_are_unique_path_elements() {
        let a = token();
        let b = token();
        assert_ne!(a, b);
        for t in [a, b] {
            assert!(t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
            assert!(ObjectPath::try_from(format!("/x/{t}")).is_ok());
        }
    }
}

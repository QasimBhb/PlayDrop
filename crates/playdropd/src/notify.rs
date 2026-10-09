//! Desktop notifications: freedesktop (`notify-rust`) on Linux, toasts on Windows.
//!
//! What is shown (PLAN.md UX, threat model):
//! - `Paired`: "Paired with <label> (<Short ID>)" (SR-8: a new pairing is never silent).
//! - Receive-side `TransferComplete`: "Received <n> files from <label>" with "Open" and
//!   "Show in folder" (both go through `open.rs`, so SR-23 and the receive folder rule
//!   apply).
//! - `TransferFailed`: "Transfer failed: <reason>".
//! - `Ping`: "Ping from <label>: <message>".
//! - `Find`: a persistent "<label> is looking for this computer", replaced when it stops.
//! - `RemovedByPeer`: "<label> no longer has you paired".
//! - Clipboard content is never shown in a notification, sensitive or not (SR-24); the
//!   daemon only reports a clip it could not apply, without its text.
//!
//! Nothing here blocks the caller: every notification is shown on a blocking-pool thread,
//! and an action click is handled in its own task.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use playdrop_core::host::{HostEvent, TransferDirection};
use tokio::sync::Semaphore;

/// Most notifications at once that wait for an action click, each on a blocking thread.
/// Beyond this, received-file notifications are shown without buttons rather than tying up
/// more threads. *Choice:* 8 covers a burst of transfers; a waiter is released when its
/// notification is clicked, dismissed or expires.
const MAX_ACTION_WAITERS: usize = 8;

/// A notification button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Open the received file (or the folder under SR-23).
    Open(PathBuf),
    /// Reveal the received file in its folder.
    ShowInFolder(PathBuf),
}

impl Action {
    fn id(&self) -> &'static str {
        match self {
            Self::Open(_) => "open",
            Self::ShowInFolder(_) => "show",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Open(_) => "Open",
            Self::ShowInFolder(_) => "Show in folder",
        }
    }
}

/// One notification to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Title line.
    pub summary: String,
    /// Body text (may be empty).
    pub body: String,
    /// Buttons.
    pub actions: Vec<Action>,
    /// Stays until dismissed or replaced.
    pub persistent: bool,
}

impl Note {
    fn plain(summary: String, body: String) -> Self {
        Self {
            summary,
            body,
            actions: Vec::new(),
            persistent: false,
        }
    }
}

/// What to do for one host event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Render {
    /// Show a notification.
    Show(Note),
    /// Show a persistent "find" notification for this peer.
    FindStart(String, Note),
    /// Replace that peer's "find" notification.
    FindStop(String, Note),
}

/// True for the events [`render`] turns into a notification.
pub fn notifies(ev: &HostEvent) -> bool {
    matches!(
        ev,
        HostEvent::Paired { .. }
            | HostEvent::TransferComplete {
                direction: TransferDirection::Receive,
                ..
            }
            | HostEvent::TransferFailed { .. }
            | HostEvent::Ping { .. }
            | HostEvent::Find { .. }
            | HostEvent::RemovedByPeer { .. }
    )
}

/// Words a host event for the user, or `None` for events that do not notify. `label` is
/// the local label of the peer the event is about (or its Short ID).
pub fn render(ev: &HostEvent, label: &str) -> Option<Render> {
    let r = match ev {
        HostEvent::Paired { short_id, name, .. } => Render::Show(Note::plain(
            format!("Paired with {name} ({short_id})"),
            "You can change what it may do in the PlayDrop window.".to_owned(),
        )),
        HostEvent::TransferComplete {
            direction: TransferDirection::Receive,
            saved,
            folder,
            ..
        } => {
            let n = saved.len();
            let files = if n == 1 { "file" } else { "files" };
            let body = match saved.as_slice() {
                [one] => one
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                _ => folder
                    .as_ref()
                    .map(|f| f.display().to_string())
                    .unwrap_or_default(),
            };
            let mut actions = Vec::new();
            match saved.as_slice() {
                [one] => {
                    actions.push(Action::Open(one.clone()));
                    actions.push(Action::ShowInFolder(one.clone()));
                }
                [first, ..] => {
                    if let Some(f) = folder {
                        actions.push(Action::Open(f.clone()));
                    }
                    actions.push(Action::ShowInFolder(first.clone()));
                }
                [] => {}
            }
            Render::Show(Note {
                summary: format!("Received {n} {files} from {label}"),
                body,
                actions,
                persistent: false,
            })
        }
        HostEvent::TransferFailed { reason, .. } => Render::Show(Note::plain(
            format!("Transfer failed: {reason}"),
            label.to_owned(),
        )),
        HostEvent::Ping { message, .. } => Render::Show(Note::plain(
            match message.as_deref().filter(|m| !m.is_empty()) {
                Some(m) => format!("Ping from {label}: {m}"),
                None => format!("Ping from {label}"),
            },
            String::new(),
        )),
        HostEvent::Find { peer, start: true } => Render::FindStart(
            peer.clone(),
            Note {
                summary: format!("{label} is looking for this computer"),
                // TODO(Phase 3): play a ring sound until stopped; no audio backend yet.
                body: "Dismiss this when you have found it.".to_owned(),
                actions: Vec::new(),
                persistent: true,
            },
        ),
        HostEvent::Find { peer, start: false } => Render::FindStop(
            peer.clone(),
            Note::plain(format!("{label} stopped looking"), String::new()),
        ),
        HostEvent::RemovedByPeer { .. } => Render::Show(Note::plain(
            format!("{label} no longer has you paired"),
            "Pair again from the PlayDrop window to reconnect.".to_owned(),
        )),
        _ => return None,
    };
    Some(r)
}

/// Shows notifications when enabled.
pub struct Notifier {
    enabled: AtomicBool,
    receive_dir: Mutex<PathBuf>,
    /// Platform notification IDs of open "find" notifications, by peer.
    finds: Arc<Mutex<HashMap<String, u32>>>,
    waiters: Arc<Semaphore>,
}

impl std::fmt::Debug for Notifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notifier")
            .field("enabled", &self.enabled.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Notifier {
    /// A notifier; `enabled` is the `notifications` setting.
    pub fn new(enabled: bool, receive_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            enabled: AtomicBool::new(enabled),
            receive_dir: Mutex::new(receive_dir),
            finds: Arc::new(Mutex::new(HashMap::new())),
            waiters: Arc::new(Semaphore::new(MAX_ACTION_WAITERS)),
        })
    }

    /// Turns notifications on or off.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
    }

    /// The receive folder actions are confined to (it changes on a settings change).
    pub fn set_receive_dir(&self, dir: PathBuf) {
        *self
            .receive_dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = dir;
    }

    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Notifies about a host event if it is one that notifies. Returns at once. `label`
    /// is called only when a notification is shown, so frequent events (progress) cost
    /// nothing.
    pub fn host_event(&self, ev: &HostEvent, label: impl FnOnce() -> String) {
        if !self.enabled() || !notifies(ev) {
            return;
        }
        match render(ev, &label()) {
            Some(Render::Show(note)) => self.show(note),
            Some(Render::FindStart(peer, note)) => {
                let finds = Arc::clone(&self.finds);
                tokio::spawn(async move {
                    let replaces = lock(&finds).get(&peer).copied();
                    if let Some(id) = platform::show(note, replaces).await {
                        lock(&finds).insert(peer, id);
                    }
                });
            }
            Some(Render::FindStop(peer, note)) => {
                let finds = Arc::clone(&self.finds);
                tokio::spawn(async move {
                    let open = lock(&finds).remove(&peer);
                    if let Some(id) = open {
                        platform::show(note, Some(id)).await;
                    }
                });
            }
            None => {}
        }
    }

    /// Shows a plain notification (tray feedback, clipboard problems).
    pub fn info(&self, summary: impl Into<String>, body: impl Into<String>) {
        if self.enabled() {
            self.show(Note::plain(summary.into(), body.into()));
        }
    }

    fn show(&self, mut note: Note) {
        if note.actions.is_empty() {
            tokio::spawn(async move {
                platform::show(note, None).await;
            });
            return;
        }
        let Ok(permit) = Arc::clone(&self.waiters).try_acquire_owned() else {
            note.actions.clear();
            tokio::spawn(async move {
                platform::show(note, None).await;
            });
            return;
        };
        let receive_dir = self
            .receive_dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        tokio::spawn(async move {
            let actions = note.actions.clone();
            let chosen = platform::show_and_wait(note).await;
            drop(permit);
            let Some(action) = chosen.and_then(|id| actions.into_iter().find(|a| a.id() == id))
            else {
                return;
            };
            let result = match &action {
                Action::Open(p) => crate::open::open_received(p, &receive_dir).map(drop),
                Action::ShowInFolder(p) => crate::open::show_in_folder(p, &receive_dir).await,
            };
            if let Err(e) = result {
                tracing::warn!(error = %e, "notification action failed");
            }
        });
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(target_os = "linux")]
mod platform {
    use notify_rust::{Hint, Notification, Timeout, Urgency};

    use super::Note;

    const APP_NAME: &str = "PlayDrop";
    const ICON: &str = "org.playdrop.PlayDrop";

    fn build(note: &Note, replaces: Option<u32>) -> Notification {
        let mut n = Notification::new();
        n.appname(APP_NAME)
            .summary(&note.summary)
            .body(&note.body)
            .icon(ICON)
            .hint(Hint::DesktopEntry("org.playdrop.PlayDrop".to_owned()));
        for a in &note.actions {
            n.action(a.id(), a.label());
        }
        if note.persistent {
            n.timeout(Timeout::Never)
                .urgency(Urgency::Critical)
                .hint(Hint::Resident(true));
        }
        if let Some(id) = replaces {
            n.id(id);
        }
        n
    }

    /// Shows a notification; returns its server ID.
    pub(super) async fn show(note: Note, replaces: Option<u32>) -> Option<u32> {
        let r = tokio::task::spawn_blocking(move || build(&note, replaces).show().map(|h| h.id()))
            .await;
        match r {
            Ok(Ok(id)) => Some(id),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "notification not shown");
                None
            }
            Err(e) => {
                tracing::debug!(error = %e, "notification task failed");
                None
            }
        }
    }

    /// Shows a notification with buttons and waits until one is clicked or the
    /// notification closes. Returns the clicked action ID.
    pub(super) async fn show_and_wait(note: Note) -> Option<String> {
        let r = tokio::task::spawn_blocking(move || {
            let handle = build(&note, None).show()?;
            let mut chosen = None;
            handle.wait_for_action(|a| {
                if a != "__closed" {
                    chosen = Some(a.to_owned());
                }
            });
            Ok::<_, notify_rust::error::Error>(chosen)
        })
        .await;
        match r {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "notification not shown");
                None
            }
            Err(e) => {
                tracing::debug!(error = %e, "notification task failed");
                None
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    use super::Note;

    /// The AppUserModelID the installer's Start menu shortcut carries; toasts from an
    /// unregistered ID are dropped by Windows.
    const AUMID: &str = "org.playdrop.PlayDrop";

    fn toast(note: &Note) -> windows::core::Result<()> {
        let xml = format!(
            "<toast{}><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
            if note.persistent { " scenario=\"reminder\"" } else { "" },
            super::xml_escape(&note.summary),
            super::xml_escape(&note.body)
        );
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml))?;
        let t = ToastNotification::CreateToastNotification(&doc)?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&t)
    }

    /// Shows a toast. Replacing and closing by ID is not wired yet; returns no ID.
    pub(super) async fn show(note: Note, _replaces: Option<u32>) -> Option<u32> {
        match tokio::task::spawn_blocking(move || toast(&note)).await {
            Ok(Err(e)) => tracing::debug!(error = %e, "toast not shown"),
            Err(e) => tracing::debug!(error = %e, "toast task failed"),
            Ok(Ok(())) => {}
        }
        None
    }

    /// TODO(Phase 2 follow-up): toast buttons need a COM activator registered by the
    /// installer; until then the toast shows without "Open" and "Show in folder".
    pub(super) async fn show_and_wait(mut note: Note) -> Option<String> {
        note.actions.clear();
        show(note, None).await;
        None
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use super::Note;

    #[allow(clippy::unused_async)]
    pub(super) async fn show(_note: Note, _replaces: Option<u32>) -> Option<u32> {
        None
    }

    #[allow(clippy::unused_async)]
    pub(super) async fn show_and_wait(_note: Note) -> Option<String> {
        None
    }
}

/// Escapes text for an XML text node.
#[cfg(any(windows, test))]
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_is_escaped() {
        assert_eq!(xml_escape("a<b>&\"'"), "a&lt;b&gt;&amp;&quot;&apos;");
    }
}

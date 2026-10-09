//! Desktop notifications: freedesktop (`notify-rust`) on Linux, toasts on Windows.
//!
//! What is shown (PLAN.md UX, threat model):
//! - `Paired`: "Paired with <label> (<Short ID>)" (SR-8: a new pairing is never silent).
//! - Receive-side `TransferComplete`: "Received <n> files from <label>" with "Open" and
//!   "Show in folder" (both go through `open.rs`, so SR-23 and the receive folder rule
//!   apply).
//! - `TransferFailed`: "Transfer failed: <reason>".
//! - `Ping`: "Ping from <label>: <message>".
//! - `Find` (PLAN.md "find"): a critical, resident "Find my device" with the freedesktop
//!   `sound-name` hint `phone-incoming-call`, shown again every [`RING_REPEAT`] so the sound
//!   repeats, until the peer sends `start: false`, the user dismisses it, or [`RING_LIMIT`]
//!   passes. Windows shows one reminder toast with the system sound; it cannot tell a
//!   dismissal apart, so it ends on stop or the limit only.
//! - `RemovedByPeer`: "<label> no longer has you paired".
//! - Clipboard content is never shown in a notification, sensitive or not (SR-24); the
//!   daemon only reports a clip it could not apply, without its text.
//!
//! Nothing here blocks the caller: every notification is shown on a blocking-pool thread,
//! and an action click is handled in its own task.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use playdrop_core::host::{HostEvent, TransferDirection};
use tokio::sync::{oneshot, Semaphore};
use tokio::time::Instant;

/// Most notifications at once that wait for an action click, each on a blocking thread.
/// Beyond this, received-file notifications are shown without buttons rather than tying up
/// more threads. *Choice:* 8 covers a burst of transfers; a waiter is released when its
/// notification is clicked, dismissed or expires.
const MAX_ACTION_WAITERS: usize = 8;

/// How often a "find" notification is shown again, which replays its sound.
pub const RING_REPEAT: Duration = Duration::from_secs(3);
/// The longest a "find" rings without a stop or a dismissal.
pub const RING_LIMIT: Duration = Duration::from_mins(1);
/// The freedesktop sound theme name a "find" notification asks for.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const RING_SOUND: &str = "phone-incoming-call";

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

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
    /// Plays the ring sound (critical urgency on Linux, a reminder sound on Windows).
    pub ring: bool,
}

impl Note {
    fn plain(summary: String, body: String) -> Self {
        Self {
            summary,
            body,
            actions: Vec::new(),
            persistent: false,
            ring: false,
        }
    }
}

/// How a ring is paced: [`RING_REPEAT`] and [`RING_LIMIT`] in production, shorter in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RingTiming {
    /// Time between two shows.
    pub repeat: Duration,
    /// Time from the first show to the end.
    pub limit: Duration,
}

impl RingTiming {
    /// The production pacing.
    pub(crate) const DEFAULT: Self = Self {
        repeat: RING_REPEAT,
        limit: RING_LIMIT,
    };
}

/// The next thing a ring does, from when it started and when it last rang.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RingStep {
    /// Show the notification again (and so replay the sound) at this instant.
    Ring(Instant),
    /// Stop ringing at this instant: [`RING_LIMIT`] is reached.
    End(Instant),
}

/// Ring timing, kept pure for tests: every `repeat` after the last ring, until `limit` after
/// the start.
pub(crate) fn next_ring(timing: RingTiming, started: Instant, last: Instant) -> RingStep {
    let next = last + timing.repeat;
    let end = started + timing.limit;
    if next < end {
        RingStep::Ring(next)
    } else {
        RingStep::End(end)
    }
}

/// Why a ring ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RingEnd {
    /// The peer sent `start: false`.
    Stopped,
    /// The user closed the notification.
    Dismissed,
    /// [`RING_LIMIT`] passed.
    TimedOut,
}

/// A ring in progress for one peer.
struct Ring {
    /// Tells the ring task the peer stopped it.
    stop: oneshot::Sender<()>,
    /// Which ring this is, so a finished task only removes its own entry.
    generation: u64,
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
                ring: false,
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
                summary: "Find my device".to_owned(),
                body: format!(
                    "{label} is looking for this computer. Dismiss this to stop the ring."
                ),
                actions: Vec::new(),
                persistent: true,
                ring: true,
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
    /// Rings in progress, by peer.
    rings: Arc<Mutex<HashMap<String, Ring>>>,
    /// The note a stopped ring replaces its notification with, by peer.
    stop_notes: Arc<Mutex<HashMap<String, Note>>>,
    /// Source of [`Ring::generation`].
    ring_generation: AtomicU64,
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
            rings: Arc::new(Mutex::new(HashMap::new())),
            stop_notes: Arc::new(Mutex::new(HashMap::new())),
            ring_generation: AtomicU64::new(0),
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
        // A stop always ends a running ring, even if notifications were turned off since.
        let stop = matches!(ev, HostEvent::Find { start: false, .. });
        if !notifies(ev) || !(self.enabled() || stop) {
            return;
        }
        match render(ev, &label()) {
            Some(Render::Show(note)) => self.show(note),
            Some(Render::FindStart(peer, note)) => self.start_ring(peer, note),
            Some(Render::FindStop(peer, note)) => {
                // The ring task replaces its notification with `note` when it ends.
                let ring = lock(&self.rings).remove(&peer);
                if let Some(ring) = ring {
                    lock(&self.stop_notes).insert(peer, note);
                    let _ = ring.stop.send(());
                }
            }
            None => {}
        }
    }

    /// Starts ringing for `peer` unless a ring for it is already running (a repeated start
    /// keeps the running one and its 60 s limit).
    fn start_ring(&self, peer: String, note: Note) {
        let generation = self.ring_generation.fetch_add(1, Ordering::Relaxed);
        let (stop, stopped) = oneshot::channel();
        {
            let mut rings = lock(&self.rings);
            if rings.contains_key(&peer) {
                tracing::debug!("find already ringing; the running ring continues");
                return;
            }
            rings.insert(peer.clone(), Ring { stop, generation });
        }
        let rings = Arc::clone(&self.rings);
        let stop_notes = Arc::clone(&self.stop_notes);
        tokio::spawn(async move {
            let (end, id) = ring(&PlatformUi, RingTiming::DEFAULT, note, stopped).await;
            {
                let mut rings = lock(&rings);
                if rings.get(&peer).is_some_and(|r| r.generation == generation) {
                    rings.remove(&peer);
                }
            }
            tracing::debug!(?end, "find ring ended");
            let follow_up = match end {
                RingEnd::Stopped => lock(&stop_notes).remove(&peer),
                RingEnd::TimedOut => Some(Note::plain(
                    "Find my device stopped ringing".to_owned(),
                    "It rang for a minute without an answer.".to_owned(),
                )),
                RingEnd::Dismissed => None,
            };
            if let Some(note) = follow_up {
                platform::show(note, id).await;
            }
        });
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

/// The notification calls a ring makes; tests swap in a fake so no real notification shows.
trait RingUi {
    /// Shows `note`, replacing notification `replaces`; returns its ID.
    fn show(&self, note: Note, replaces: Option<u32>) -> BoxFuture<'static, Option<u32>>;
    /// Completes when notification `id` is closed by the user.
    fn closed(&self, id: Option<u32>) -> BoxFuture<'static, ()>;
    /// Whether showing again replays the sound (Linux), or one show is the whole ring.
    fn repeats(&self) -> bool;
}

/// The real notification server.
struct PlatformUi;

impl RingUi for PlatformUi {
    fn show(&self, note: Note, replaces: Option<u32>) -> BoxFuture<'static, Option<u32>> {
        Box::pin(platform::show(note, replaces))
    }

    fn closed(&self, id: Option<u32>) -> BoxFuture<'static, ()> {
        Box::pin(platform::closed(id))
    }

    fn repeats(&self) -> bool {
        platform::RING_REPEATS
    }
}

/// Shows `note` and shows it again every `timing.repeat` (where the platform replays the
/// sound that way) until `stopped` fires, the user dismisses it, or `timing.limit` passes.
/// Returns why it ended and the notification ID to replace.
async fn ring(
    ui: &impl RingUi,
    timing: RingTiming,
    note: Note,
    mut stopped: oneshot::Receiver<()>,
) -> (RingEnd, Option<u32>) {
    let started = Instant::now();
    let id = ui.show(note.clone(), None).await;
    let mut dismissed = ui.closed(id);
    let mut last = started;
    loop {
        let (at, end) = match next_ring(timing, started, last) {
            RingStep::Ring(at) => (at, false),
            RingStep::End(at) => (at, true),
        };
        tokio::select! {
            _ = &mut stopped => return (RingEnd::Stopped, id),
            () = &mut dismissed => return (RingEnd::Dismissed, id),
            () = tokio::time::sleep_until(at) => {
                if end {
                    return (RingEnd::TimedOut, id);
                }
                if ui.repeats() {
                    ui.show(note.clone(), id).await;
                }
                last = at;
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(target_os = "linux")]
mod platform {
    use futures_util::StreamExt as _;
    use notify_rust::{Hint, Notification, Timeout, Urgency};

    use super::Note;

    /// Showing the notification again with the same ID replays its sound.
    pub(super) const RING_REPEATS: bool = true;

    const APP_NAME: &str = "PlayDrop";

    #[zbus::proxy(
        interface = "org.freedesktop.Notifications",
        default_service = "org.freedesktop.Notifications",
        default_path = "/org/freedesktop/Notifications",
        gen_blocking = false
    )]
    trait Notifications {
        #[zbus(signal)]
        fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
    }

    /// Completes when the notification server reports `id` closed (dismissed, expired or
    /// closed by a click). Never completes without an ID or a session bus.
    pub(super) async fn closed(id: Option<u32>) {
        let Some(id) = id else {
            return std::future::pending().await;
        };
        let stream = async {
            let conn = zbus::Connection::session().await?;
            let proxy = NotificationsProxy::new(&conn).await?;
            let stream = proxy.receive_notification_closed().await?;
            Ok::<_, zbus::Error>((conn, stream))
        };
        let (_conn, mut stream) = match stream.await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "notification close signal unavailable");
                return std::future::pending().await;
            }
        };
        while let Some(signal) = stream.next().await {
            if signal.args().is_ok_and(|a| a.id == id) {
                return;
            }
        }
        std::future::pending::<()>().await;
    }
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
        if note.ring {
            n.hint(Hint::SoundName(super::RING_SOUND.to_owned()));
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

    /// The `AppUserModelID` the installer's Start menu shortcut carries; toasts from an
    /// unregistered ID are dropped by Windows.
    const AUMID: &str = "org.playdrop.PlayDrop";

    /// One toast with the system reminder sound is the whole ring on Windows.
    pub(super) const RING_REPEATS: bool = false;

    fn toast(note: &Note) -> windows::core::Result<()> {
        let xml = format!(
            "<toast{}><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{}</toast>",
            if note.persistent { " scenario=\"reminder\"" } else { "" },
            super::xml_escape(&note.summary),
            super::xml_escape(&note.body),
            if note.ring {
                "<audio src=\"ms-winsoundevent:Notification.Reminder\"/>"
            } else {
                ""
            }
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

    /// Toast dismissal is not observed without a COM activator; a ring ends on stop or
    /// the limit.
    pub(super) async fn closed(_id: Option<u32>) {
        std::future::pending::<()>().await;
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use super::Note;

    pub(super) const RING_REPEATS: bool = false;

    pub(super) async fn closed(_id: Option<u32>) {
        std::future::pending::<()>().await;
    }

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

    #[test]
    fn ring_repeats_every_three_seconds_for_a_minute() {
        let t0 = Instant::now();
        let timing = RingTiming::DEFAULT;
        assert_eq!(next_ring(timing, t0, t0), RingStep::Ring(t0 + RING_REPEAT));
        let mut last = t0;
        let mut rings = 0;
        let end = loop {
            match next_ring(timing, t0, last) {
                RingStep::Ring(at) => {
                    assert_eq!(at - last, RING_REPEAT);
                    last = at;
                    rings += 1;
                }
                RingStep::End(at) => break at,
            }
        };
        // Shown at 0 s, then again at 3, 6, ... 57 s: 19 repeats, and the end at 60 s.
        assert_eq!(rings, 19);
        assert_eq!(end - t0, RING_LIMIT);
        assert_eq!(last - t0, Duration::from_secs(57));
    }

    #[test]
    fn find_renders_a_ringing_resident_note() {
        let start = HostEvent::Find {
            peer: "ab".into(),
            start: true,
        };
        let Some(Render::FindStart(peer, note)) = render(&start, "Phone") else {
            panic!("find start should ring");
        };
        assert_eq!(peer, "ab");
        assert_eq!(note.summary, "Find my device");
        assert!(note.body.contains("Phone"));
        assert!(note.persistent && note.ring);
        let stop = HostEvent::Find {
            peer: "ab".into(),
            start: false,
        };
        let Some(Render::FindStop(_, note)) = render(&stop, "Phone") else {
            panic!("find stop should replace the ring");
        };
        assert!(!note.persistent && !note.ring);
    }

    /// Counts shows; "closes" when its receiver fires.
    struct FakeUi {
        shows: Arc<AtomicU64>,
        close: Mutex<Option<oneshot::Receiver<()>>>,
        repeats: bool,
    }

    impl FakeUi {
        fn new(repeats: bool) -> (Self, oneshot::Sender<()>) {
            let (tx, rx) = oneshot::channel();
            let ui = Self {
                shows: Arc::new(AtomicU64::new(0)),
                close: Mutex::new(Some(rx)),
                repeats,
            };
            (ui, tx)
        }
    }

    impl RingUi for FakeUi {
        fn show(&self, note: Note, replaces: Option<u32>) -> BoxFuture<'static, Option<u32>> {
            assert!(note.ring && note.persistent);
            let n = self.shows.fetch_add(1, Ordering::SeqCst);
            // The first show creates ID 7; every repeat replaces it.
            assert_eq!(replaces, (n > 0).then_some(7));
            Box::pin(async { Some(7) })
        }

        fn closed(&self, id: Option<u32>) -> BoxFuture<'static, ()> {
            assert_eq!(id, Some(7));
            let rx = lock(&self.close).take().expect("closed asked once");
            Box::pin(async move {
                if rx.await.is_err() {
                    std::future::pending::<()>().await;
                }
            })
        }

        fn repeats(&self) -> bool {
            self.repeats
        }
    }

    fn ring_note() -> Note {
        let Some(Render::FindStart(_, note)) = render(
            &HostEvent::Find {
                peer: "ab".into(),
                start: true,
            },
            "Phone",
        ) else {
            panic!("find start should ring");
        };
        note
    }

    /// A ring paced 50 times faster than production: 60 ms apart, 1.2 s in all.
    const FAST: RingTiming = RingTiming {
        repeat: Duration::from_millis(60),
        limit: Duration::from_millis(1200),
    };

    #[tokio::test]
    async fn ring_ends_when_the_peer_stops_it() {
        let (ui, _close) = FakeUi::new(true);
        let (stop, stopped) = oneshot::channel();
        let peer_stops = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = stop.send(());
        };
        let started = Instant::now();
        let ((end, id), ()) = tokio::join!(ring(&ui, FAST, ring_note(), stopped), peer_stops);
        assert_eq!((end, id), (RingEnd::Stopped, Some(7)));
        assert!(started.elapsed() < FAST.limit);
        assert!(
            ui.shows.load(Ordering::SeqCst) >= 2,
            "rang again before the stop"
        );
    }

    #[tokio::test]
    async fn ring_ends_when_the_user_dismisses_it() {
        let (ui, close) = FakeUi::new(true);
        let (_stop, stopped) = oneshot::channel();
        let dismiss = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = close.send(());
        };
        let started = Instant::now();
        let ((end, _), ()) = tokio::join!(ring(&ui, FAST, ring_note(), stopped), dismiss);
        assert_eq!(end, RingEnd::Dismissed);
        assert!(started.elapsed() < FAST.limit);
    }

    #[tokio::test]
    async fn ring_gives_up_after_the_limit() {
        // Nobody answers: shown at 0 and every 60 ms up to 1140 ms, then it ends at 1.2 s.
        let (ui, _close) = FakeUi::new(true);
        let (_stop, stopped) = oneshot::channel();
        let started = Instant::now();
        let (end, _) = ring(&ui, FAST, ring_note(), stopped).await;
        assert_eq!(end, RingEnd::TimedOut);
        assert!(started.elapsed() >= FAST.limit);
        assert_eq!(ui.shows.load(Ordering::SeqCst), 20);

        // Windows: one toast, still ended by the limit.
        let (ui, _close) = FakeUi::new(false);
        let (_stop, stopped) = oneshot::channel();
        let (end, _) = ring(&ui, FAST, ring_note(), stopped).await;
        assert_eq!(end, RingEnd::TimedOut);
        assert_eq!(ui.shows.load(Ordering::SeqCst), 1);
    }
}

//! Glue between the core's remote input and media events and the platform backends
//! (`input`, `media`): one injector task per controlling peer, in frame order, and the
//! media feed in both directions (Phase 5; protocol.md 5.9, 5.12; SR-25).
//!
//! The core has already decided what may be applied (grant, session, lock, key cap). This
//! module only keeps order and lifetime: every `Input` event for a peer goes into that
//! peer's unbounded channel, and one task per peer applies them one after another, so a
//! button `down` can never overtake the move before it. The task starts the injector on
//! `session`/`start` (the first time, the desktop's consent dialog appears) and closes it
//! on `session`/`stop`, which the core sends last in every case: peer stop, connection
//! drop, lock, idle timeout and the kill switch (T31: keys and buttons are released there).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use playdrop_core::host::MediaState;
use playdrop_core::session::types::{Input, SessionOp};
use playdrop_ipc::{DaemonEvent, Event};
use tokio::sync::{broadcast, mpsc, Notify};

use crate::input::{self, Injector};
use crate::notify::Notifier;

/// The per-peer injector tasks and the "who controls this desktop" list for the tray and
/// the window.
pub struct InputRunners {
    data_dir: PathBuf,
    notifier: Arc<Notifier>,
    events: broadcast::Sender<Event>,
    tray_dirty: Arc<Notify>,
    runners: Mutex<BTreeMap<String, mpsc::UnboundedSender<Input>>>,
    /// Peers whose injector is open, by hex Device ID, with their label.
    active: Mutex<BTreeMap<String, String>>,
    /// Ends the core's input sessions when injection stops working (the desktop revoked
    /// the portal consent, say), so the controller learns it from a `session`/`stop`.
    ender: Mutex<Option<Ender>>,
}

/// Called with the peer's hex Device ID to end its input session in the core.
pub type Ender = Box<dyn Fn(&str) + Send + Sync>;

impl std::fmt::Debug for InputRunners {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputRunners")
            .field("active", &self.active_labels())
            .finish_non_exhaustive()
    }
}

impl InputRunners {
    /// Builds the registry. Nothing runs until the first `Input` event.
    pub fn new(
        data_dir: PathBuf,
        notifier: Arc<Notifier>,
        events: broadcast::Sender<Event>,
        tray_dirty: Arc<Notify>,
    ) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            notifier,
            events,
            tray_dirty,
            runners: Mutex::new(BTreeMap::new()),
            active: Mutex::new(BTreeMap::new()),
            ender: Mutex::new(None),
        })
    }

    /// Installs the callback that ends a peer's session in the core (set once the daemon
    /// state exists; the registry is built before the host).
    pub fn set_ender(&self, ender: Ender) {
        *lock(&self.ender) = Some(ender);
    }

    /// Routes one core `Input` event to the peer's task, starting the task on
    /// `session`/`start`. Never blocks (the channel is unbounded; the core already caps
    /// keys at 60 per second and coalesces moves to 8 ms).
    pub fn on_input(self: &Arc<Self>, peer: &str, label: String, input: Input) {
        let mut runners = lock(&self.runners);
        let input = match runners.get(peer) {
            Some(tx) => match tx.send(input) {
                Ok(()) => return,
                Err(mpsc::error::SendError(i)) => {
                    runners.remove(peer);
                    i
                }
            },
            None => input,
        };
        if !matches!(
            input,
            Input::Session {
                op: SessionOp::Start
            }
        ) {
            tracing::debug!(peer = short(peer), "input with no runner; dropped");
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        // The task waits for this `start` as its first item; it cannot fail, `rx` is alive.
        let _ = tx.send(input);
        runners.insert(peer.to_owned(), tx);
        drop(runners);
        let me = Arc::clone(self);
        let peer = peer.to_owned();
        tokio::spawn(async move {
            me.run(peer.clone(), label, rx).await;
            lock(&me.runners).remove(&peer);
        });
    }

    /// Labels of the peers with an open injector, for the tray and the window.
    pub fn active_labels(&self) -> Vec<String> {
        lock(&self.active).values().cloned().collect()
    }

    /// Ends every runner now (daemon shutdown). The core's `stop_input_sessions` is the
    /// kill switch while running; this is the last resort when the host is gone.
    pub fn stop_all(&self) {
        let runners = std::mem::take(&mut *lock(&self.runners));
        for tx in runners.values() {
            let _ = tx.send(Input::Session {
                op: SessionOp::Stop,
            });
        }
    }

    async fn run(&self, peer: String, label: String, mut rx: mpsc::UnboundedReceiver<Input>) {
        let Some(first) = rx.recv().await else { return };
        if !matches!(
            first,
            Input::Session {
                op: SessionOp::Start
            }
        ) {
            return;
        }
        tracing::info!(peer = short(&peer), "remote input: starting the injector");
        let mut injector = match Injector::start(&self.data_dir, &label).await {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(peer = short(&peer), error = %e, "remote input could not start");
                self.notifier.info(
                    format!("Remote input from {label} could not start"),
                    e.to_string(),
                );
                // Swallow the rest of the session so the peer's frames do not pile up.
                while let Some(i) = rx.recv().await {
                    if matches!(
                        i,
                        Input::Session {
                            op: SessionOp::Stop
                        }
                    ) {
                        break;
                    }
                }
                return;
            }
        };
        self.set_active(&peer, Some(label.clone()));
        self.notifier.info(
            format!("Remote input from {label}"),
            "This device is controlling the mouse and keyboard. Stop it from the tray or the window.",
        );
        let mut failure: Option<String> = None;
        while let Some(i) = rx.recv().await {
            match i {
                Input::Session {
                    op: SessionOp::Stop,
                } => break,
                Input::Session { .. } => {}
                other => {
                    if let Err(e) = injector.apply(&other).await {
                        // The platform refused (consent revoked, portal gone): release what
                        // is held (T31) and end the session on both sides rather than
                        // dropping frames quietly (anti-pattern 18).
                        tracing::warn!(peer = short(&peer), error = %e, "input not applied; ending the session");
                        injector.release_all().await;
                        failure = Some(e.to_string());
                        break;
                    }
                }
            }
        }
        injector.close().await;
        lock(&self.runners).remove(&peer);
        self.set_active(&peer, None);
        match failure {
            Some(reason) => {
                if let Some(end) = lock(&self.ender).as_ref() {
                    end(&peer);
                }
                self.notifier
                    .info(format!("Remote input from {label} stopped"), reason);
            }
            None => self
                .notifier
                .info(format!("Remote input from {label} ended"), ""),
        }
    }

    fn set_active(&self, peer: &str, label: Option<String>) {
        let active = label.is_some();
        {
            let mut a = lock(&self.active);
            match label.clone() {
                Some(l) => {
                    a.insert(peer.to_owned(), l);
                }
                None => {
                    a.remove(peer);
                }
            }
        }
        let _ = self.events.send(Event::Daemon(DaemonEvent::InputSession {
            peer: peer.to_owned(),
            label: label.unwrap_or_default(),
            active,
        }));
        self.tray_dirty.notify_one();
    }
}

/// Feeds local player states from the media watcher to the host (which coalesces them to
/// 2 frames per second per player and sends only to peers with the `media` grant). `host`
/// is looked up per state so a restarted host keeps receiving.
pub async fn media_feed<F>(mut rx: mpsc::Receiver<MediaState>, host: F)
where
    F: Fn() -> Option<Arc<playdrop_core::host::Host>>,
{
    while let Some(state) = rx.recv().await {
        if let Some(h) = host() {
            h.push_media_state(state);
        }
    }
    tracing::debug!("media feed ended");
}

/// Reports what the two backends can do at start, once, so an unsupported desktop says
/// why (anti-pattern 18) instead of failing quietly later.
pub fn log_support(input: &input::InputSupport, media: &crate::media::MediaSupport) {
    if input.supported {
        tracing::info!(backend = input.backend, "remote input is available");
    } else {
        tracing::info!(
            backend = input.backend,
            reason = input.reason.as_deref().unwrap_or("unsupported"),
            "remote input is not available; the input grant stays hidden"
        );
    }
    if media.supported {
        tracing::info!(backend = media.backend, "media control is available");
    } else {
        tracing::info!(
            backend = media.backend,
            reason = media.reason.as_deref().unwrap_or("unsupported"),
            "media control is not available; peers get no player state from this desktop"
        );
    }
}

/// A weak-reference shape for callers that hold the daemon state behind a `Weak`.
pub fn weak_host<T, F>(
    shared: &Weak<T>,
    host: F,
) -> impl Fn() -> Option<Arc<playdrop_core::host::Host>>
where
    F: Fn(&T) -> Option<Arc<playdrop_core::host::Host>>,
{
    let shared = shared.clone();
    move || shared.upgrade().and_then(|s| host(&s))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn short(peer: &str) -> &str {
    peer.get(..8).unwrap_or(peer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runners() -> (Arc<InputRunners>, broadcast::Receiver<Event>) {
        let (events, rx) = broadcast::channel(16);
        let r = InputRunners::new(
            std::env::temp_dir().join("playdrop-control-test"),
            Notifier::new(false, PathBuf::from("/tmp")),
            events,
            Arc::new(Notify::new()),
        );
        (r, rx)
    }

    /// A frame before `session`/`start` is dropped and starts no task. Starting a real
    /// session is not tested here: on a desktop with the portal it would open the consent
    /// dialog (`cargo test -p playdropd -- --ignored input_portal_live` covers it).
    #[tokio::test]
    async fn frames_before_start_are_dropped() {
        let (r, _events) = runners();
        r.on_input(
            "ab",
            "Phone".into(),
            Input::Pointer(playdrop_core::session::types::PointerInput::Move { dx: 1.0, dy: 1.0 }),
        );
        assert!(lock(&r.runners).is_empty());
        r.on_input(
            "ab",
            "Phone".into(),
            Input::Session {
                op: SessionOp::Stop,
            },
        );
        assert!(lock(&r.runners).is_empty());
        r.stop_all();
        assert_eq!(r.active_labels(), Vec::<String>::new());
    }

    #[test]
    fn active_labels_follow_set_active() {
        let (r, mut events) = runners();
        r.set_active("ab", Some("Phone".into()));
        assert_eq!(r.active_labels(), vec!["Phone".to_owned()]);
        let Ok(Event::Daemon(DaemonEvent::InputSession {
            peer,
            label,
            active,
        })) = events.try_recv()
        else {
            panic!("no event");
        };
        assert_eq!(
            (peer.as_str(), label.as_str(), active),
            ("ab", "Phone", true)
        );
        r.set_active("ab", None);
        assert_eq!(r.active_labels(), Vec::<String>::new());
        let Ok(Event::Daemon(DaemonEvent::InputSession { active, label, .. })) = events.try_recv()
        else {
            panic!("no event");
        };
        assert!(!active);
        assert_eq!(label, "");
    }
}

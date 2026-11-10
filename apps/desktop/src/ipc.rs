//! The bridge between the Slint thread and the daemon.
//!
//! Every IPC call runs on the tokio runtime; its answer is posted back to the Slint event
//! loop with [`slint::invoke_from_event_loop`], so the window never blocks on the daemon.
//! Events from the daemon take the same path, one closure per event.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::StreamExt;
use playdrop_ipc::client::{Client, ClientOptions, ConnectError};
use playdrop_ipc::{ErrorCode, Event, IpcError, Ok, Request};
use tokio::runtime::Handle;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

/// Why a connect attempt failed, already worded for the window.
#[derive(Debug, Clone)]
pub struct ConnectFailure {
    /// No daemon is running (the window offers "Start").
    pub not_running: bool,
    /// Text for the user.
    pub message: String,
}

impl From<&ConnectError> for ConnectFailure {
    fn from(e: &ConnectError) -> Self {
        match e {
            ConnectError::NotRunning => Self {
                not_running: true,
                message: String::new(),
            },
            ConnectError::Version { .. } => Self {
                not_running: false,
                message: format!("{e}. Update PlayDrop so both parts match."),
            },
            _ => Self {
                not_running: false,
                message: e.to_string(),
            },
        }
    }
}

/// What the IPC side tells the window. Delivered on the Slint thread.
#[derive(Debug)]
pub enum IpcMsg {
    /// Connected and verified; the window should load its state.
    Connected,
    /// Connecting failed.
    ConnectFailed(ConnectFailure),
    /// A daemon event.
    Event(Event),
    /// The window fell behind the event stream; reload everything.
    Lagged,
    /// The daemon went away.
    Disconnected,
}

/// Handles IPC messages on the Slint thread.
pub type Deliver = fn(IpcMsg);

/// The window's connection to the daemon.
pub struct Ipc {
    rt: Handle,
    client: Mutex<Option<Arc<Client>>>,
    deliver: Deliver,
    /// Held across one connect attempt and the install of its client, so [`Ipc::connect`]
    /// and [`Ipc::follow`] never both bring up a connection (two event pumps would deliver
    /// every event twice).
    gate: tokio::sync::Mutex<()>,
    /// Whether the [`Ipc::follow`] task is running; there is at most one.
    following: AtomicBool,
}

/// While following: how long to wait before the next try when the daemon's name has an owner
/// that does not answer yet (it is still starting), or when the bus watch itself failed.
const FOLLOW_RETRY_EVERY: Duration = Duration::from_millis(500);
/// While following: how long an owner that does not answer is retried before the failure is
/// shown with "Try again". Measured on the clock from the first such failure, so a daemon
/// that hangs (each attempt then lasts the whole [`CONNECT_TIMEOUT`]) cannot stretch it.
const FOLLOW_PATIENCE: Duration = Duration::from_secs(15);

/// What the follow loop does after one connect attempt that may not start the daemon.
#[derive(Debug, PartialEq, Eq)]
enum FollowStep {
    /// Nobody owns the daemon's name: say so, then wait for the name to appear.
    WaitForName,
    /// An owner exists but did not answer: try again after [`FOLLOW_RETRY_EVERY`].
    RetrySoon,
    /// Stop and show the error (wrong version, unverified owner, or out of patience).
    GiveUp,
}

/// Decides the next step from a failed attempt and how long an owner has gone unanswered
/// (zero on the first such failure).
fn follow_step(error: &ConnectError, unanswered_for: Duration) -> FollowStep {
    match error {
        ConnectError::NotRunning => FollowStep::WaitForName,
        ConnectError::Transport(_) if unanswered_for < FOLLOW_PATIENCE => FollowStep::RetrySoon,
        ConnectError::Transport(_) | ConnectError::Unverified(_) | ConnectError::Version { .. } => {
            FollowStep::GiveUp
        }
    }
}

/// How [`Ipc::follow`] ended.
enum Followed {
    /// It connected; the caller pumps the events.
    Connected(Arc<Client>, tokio_stream::wrappers::BroadcastStream<Event>),
    /// [`Ipc::connect`] got there first.
    ConnectedElsewhere,
    /// It stopped after telling the window why.
    Reported,
}

/// Whether [`Client::wait_for_daemon`] can ever resolve here. Only Linux has a bus name to
/// watch; elsewhere the wait would park the follow task for good.
const CAN_WATCH_FOR_DAEMON: bool = cfg!(target_os = "linux");

/// Longest one connect attempt may take. The bus sets no deadline of its own, so a daemon
/// that owns its name but never answers would otherwise hold the gate for ever and leave
/// "Try again" and "Start" waiting behind it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs one connect attempt under [`CONNECT_TIMEOUT`].
async fn bounded(
    attempt: impl std::future::Future<Output = Result<Client, ConnectError>>,
) -> Result<Client, ConnectError> {
    match tokio::time::timeout(CONNECT_TIMEOUT, attempt).await {
        Result::Ok(outcome) => outcome,
        Result::Err(_) => Err(ConnectError::Transport(format!(
            "the daemon did not answer within {} seconds",
            CONNECT_TIMEOUT.as_secs()
        ))),
    }
}

/// Runs `f` on the Slint thread. Fails only when the event loop has already quit, which
/// happens while the window is closing; the work is then moot.
pub fn post(f: impl FnOnce() + Send + 'static) {
    if slint::invoke_from_event_loop(f).is_err() {
        tracing::debug!("event loop gone, dropping a UI update");
    }
}

fn not_connected() -> IpcError {
    IpcError {
        code: ErrorCode::Internal,
        message: "not connected to the PlayDrop daemon".to_owned(),
    }
}

impl Ipc {
    /// A disconnected bridge that runs its work on `rt` and hands messages to `deliver`.
    pub fn new(rt: Handle, deliver: Deliver) -> Arc<Self> {
        Arc::new(Self {
            rt,
            client: Mutex::new(None),
            deliver,
            gate: tokio::sync::Mutex::new(()),
            following: AtomicBool::new(false),
        })
    }

    /// The tokio runtime, for work that is not an IPC call (file pickers).
    pub fn runtime(&self) -> &Handle {
        &self.rt
    }

    fn current(&self) -> Option<Arc<Client>> {
        self.client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_current(&self, client: Option<Arc<Client>>) {
        *self.client.lock().unwrap_or_else(PoisonError::into_inner) = client;
    }

    /// Whether a verified connection is up.
    pub fn is_connected(&self) -> bool {
        self.current().is_some()
    }

    /// Connects in the background and reports the outcome; on success also starts the
    /// event pump. Does nothing if already connected.
    ///
    /// On Linux this asks the session bus to start the daemon when it is not running, so
    /// it is only for moments the user asked for the app: the window opening, "Start" and
    /// "Try again". After the daemon went away use [`Ipc::follow`], or "Quit" would be
    /// undone at once by the open window.
    pub fn connect(self: &Arc<Self>) {
        if self.is_connected() {
            return;
        }
        let this = Arc::clone(self);
        self.rt.spawn(async move {
            let gate = this.gate.lock().await;
            if this.is_connected() {
                return;
            }
            match bounded(Client::connect()).await {
                Result::Ok(client) => {
                    let (client, events) = this.install(client);
                    drop(gate);
                    this.pump(events, &client).await;
                }
                Result::Err(e) => {
                    tracing::info!("connect to daemon failed: {e}");
                    this.report_failure(&e);
                    drop(gate);
                }
            }
        });
    }

    /// Tells the window a connect attempt failed. Call with the gate held: posts to the
    /// window run in order, so an outcome posted under the gate can never land after the
    /// `Connected` of a later attempt and leave "not running" on screen over a live
    /// connection.
    fn report_failure(&self, error: &ConnectError) {
        let deliver = self.deliver;
        let failure = ConnectFailure::from(error);
        post(move || deliver(IpcMsg::ConnectFailed(failure)));
    }

    /// Reconnects once a daemon is running again, without ever starting one. Used after the
    /// connection was lost or the daemon said it is shutting down: a daemon the user quit
    /// stays quit, and one that was restarted (by the user, the panel applet, the command
    /// line or systemd after a crash) is picked up as soon as its name appears on the bus.
    ///
    /// Reports "not running" straight away when that is the case, so the window can offer
    /// "Start". Does nothing if already connected or already following. On Windows there is
    /// no name to watch (and a connect never starts anything there), so it makes one attempt
    /// and reports it.
    pub fn follow(self: &Arc<Self>) {
        if self.is_connected() || self.following.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = Arc::clone(self);
        self.rt.spawn(async move {
            let outcome = this.follow_until_connected().await;
            this.following.store(false, Ordering::SeqCst);
            match outcome {
                Followed::Connected(client, events) => this.pump(events, &client).await,
                // That other connection may have been lost again before the flag above was
                // cleared, in which case its request to follow was turned away. Ask again:
                // it is a no-op while the connection is up.
                Followed::ConnectedElsewhere => this.follow(),
                Followed::Reported => {}
            }
        });
    }

    /// The follow loop.
    async fn follow_until_connected(&self) -> Followed {
        // When an owner of the name first failed to answer; cleared when the name is free.
        let mut unanswered_since: Option<tokio::time::Instant> = None;
        loop {
            let gate = self.gate.lock().await;
            if self.is_connected() {
                return Followed::ConnectedElsewhere;
            }
            let attempt_started = tokio::time::Instant::now();
            let error = match bounded(Client::connect_without_activation()).await {
                Result::Ok(client) => {
                    let (client, events) = self.install(client);
                    return Followed::Connected(client, events);
                }
                Result::Err(e) => e,
            };
            let unanswered_for = unanswered_since.map_or(Duration::ZERO, |t| t.elapsed());
            match follow_step(&error, unanswered_for) {
                FollowStep::WaitForName => {
                    unanswered_since = None;
                    self.report_failure(&error);
                    drop(gate);
                    if !CAN_WATCH_FOR_DAEMON {
                        return Followed::Reported;
                    }
                    if let Err(e) = Client::wait_for_daemon(&ClientOptions::default()).await {
                        tracing::debug!("watching for the daemon failed: {e}");
                        tokio::time::sleep(FOLLOW_RETRY_EVERY).await;
                    }
                }
                FollowStep::RetrySoon => {
                    drop(gate);
                    // From the start of the attempt: one against a daemon that hangs lasts
                    // the whole timeout, and that time counts.
                    unanswered_since.get_or_insert(attempt_started);
                    tokio::time::sleep(FOLLOW_RETRY_EVERY).await;
                }
                FollowStep::GiveUp => {
                    tracing::info!("reconnect to daemon failed: {error}");
                    self.report_failure(&error);
                    return Followed::Reported;
                }
            }
        }
    }

    /// Makes `client` the current connection and announces it. Call with the gate held.
    fn install(
        &self,
        client: Client,
    ) -> (Arc<Client>, tokio_stream::wrappers::BroadcastStream<Event>) {
        let deliver = self.deliver;
        let client = Arc::new(client);
        // Subscribe before announcing, so no event after `Connected` is missed.
        let events = client.events();
        self.set_current(Some(Arc::clone(&client)));
        post(move || deliver(IpcMsg::Connected));
        (client, events)
    }

    async fn pump(
        self: &Arc<Self>,
        mut events: tokio_stream::wrappers::BroadcastStream<Event>,
        client: &Arc<Client>,
    ) {
        let deliver = self.deliver;
        while let Some(item) = events.next().await {
            match item {
                Result::Ok(event) => post(move || deliver(IpcMsg::Event(event))),
                Result::Err(BroadcastStreamRecvError::Lagged(n)) => {
                    tracing::warn!("window fell {n} events behind; reloading");
                    post(move || deliver(IpcMsg::Lagged));
                }
            }
        }
        // Only report the loss of the connection we pumped for.
        let mut slot = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref().is_some_and(|c| Arc::ptr_eq(c, client)) {
            *slot = None;
            drop(slot);
            post(move || deliver(IpcMsg::Disconnected));
        } else {
            // The window let go first because the daemon said it is shutting down. Its
            // stream has now really ended, so the name is free: wait for the next daemon.
            drop(slot);
            self.follow();
        }
    }

    /// Forgets the connection (the daemon said it is shutting down). When the daemon's event
    /// stream then ends, [`Ipc::follow`] starts by itself.
    pub fn disconnect(&self) {
        self.set_current(None);
    }

    /// Sends `request`; `done` runs on the Slint thread with the answer.
    pub fn request(
        &self,
        request: Request,
        done: impl FnOnce(Result<Ok, IpcError>) + Send + 'static,
    ) {
        let client = self.current();
        self.rt.spawn(async move {
            let result = match client {
                Some(c) => c.request(request).await,
                None => Err(not_connected()),
            };
            post(move || done(result));
        });
    }

    /// Sends files or folders to `peer` (fds on Linux, SR-26); `done` runs on the Slint
    /// thread.
    pub fn send_files(
        &self,
        peer: String,
        paths: Vec<PathBuf>,
        done: impl FnOnce(Result<Ok, IpcError>) + Send + 'static,
    ) {
        let client = self.current();
        self.rt.spawn(async move {
            let result = match client {
                Some(c) => c.send_files(&peer, paths).await,
                None => Err(not_connected()),
            };
            post(move || done(result));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_daemon_is_waited_for_and_never_started() {
        assert_eq!(
            follow_step(&ConnectError::NotRunning, Duration::ZERO),
            FollowStep::WaitForName
        );
        // However long it has been retrying: "not running" always goes back to waiting.
        assert_eq!(
            follow_step(&ConnectError::NotRunning, FOLLOW_PATIENCE * 2),
            FollowStep::WaitForName
        );
    }

    #[test]
    fn a_starting_daemon_is_retried_for_a_bounded_time() {
        let starting = ConnectError::Transport("no reply".to_owned());
        assert_eq!(
            follow_step(&starting, Duration::ZERO),
            FollowStep::RetrySoon
        );
        assert_eq!(
            follow_step(
                &starting,
                FOLLOW_PATIENCE.saturating_sub(Duration::from_millis(1))
            ),
            FollowStep::RetrySoon
        );
        assert_eq!(follow_step(&starting, FOLLOW_PATIENCE), FollowStep::GiveUp);
        // One attempt against a daemon that hangs already uses most of the patience, so the
        // window cannot sit without a button for minutes.
        assert!(CONNECT_TIMEOUT + FOLLOW_RETRY_EVERY < FOLLOW_PATIENCE);
        assert!(FOLLOW_PATIENCE < CONNECT_TIMEOUT * 2);
    }

    #[test]
    fn a_wrong_daemon_is_reported_not_retried() {
        assert_eq!(
            follow_step(
                &ConnectError::Unverified("other user".to_owned()),
                Duration::ZERO
            ),
            FollowStep::GiveUp
        );
        assert_eq!(
            follow_step(
                &ConnectError::Version {
                    daemon: 2,
                    client: 1
                },
                Duration::ZERO
            ),
            FollowStep::GiveUp
        );
    }
}

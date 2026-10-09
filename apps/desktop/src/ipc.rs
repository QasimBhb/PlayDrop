//! The bridge between the Slint thread and the daemon.
//!
//! Every IPC call runs on the tokio runtime; its answer is posted back to the Slint event
//! loop with [`slint::invoke_from_event_loop`], so the window never blocks on the daemon.
//! Events from the daemon take the same path, one closure per event.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::StreamExt;
use playdrop_ipc::client::{Client, ConnectError};
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
    pub fn connect(self: &Arc<Self>) {
        if self.is_connected() {
            return;
        }
        let this = Arc::clone(self);
        self.rt.spawn(async move {
            let deliver = this.deliver;
            match Client::connect().await {
                Result::Ok(client) => {
                    let client = Arc::new(client);
                    // Subscribe before announcing, so no event after `Connected` is missed.
                    let events = client.events();
                    this.set_current(Some(Arc::clone(&client)));
                    post(move || deliver(IpcMsg::Connected));
                    this.pump(events, &client).await;
                }
                Result::Err(e) => {
                    tracing::info!("connect to daemon failed: {e}");
                    let failure = ConnectFailure::from(&e);
                    post(move || deliver(IpcMsg::ConnectFailed(failure)));
                }
            }
        });
    }

    async fn pump(
        &self,
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
        }
    }

    /// Forgets the connection (the daemon said it is shutting down).
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

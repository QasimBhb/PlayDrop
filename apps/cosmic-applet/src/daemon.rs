//! The applet's link to `playdropd`: connect, load a snapshot, stream events, reconnect.
//!
//! One long-lived task (an iced subscription) owns the connection loop and reports every
//! change as a [`Link`] message. The applet never starts the daemon by itself: its own
//! attempts use [`Client::connect_without_activation`], so a daemon the user quit, or chose
//! not to start at login, stays down. While disconnected it waits for the daemon's D-Bus name
//! to gain an owner (one [`DaemonWatch`] kept for the loop's life, so a restart from anywhere is picked up at
//! once), with a doubling back-off poll (1 s up to 30 s) as the fallback. It never spins.
//!
//! Only "Start `PlayDrop`" ([`Wake::Start`]) may start the daemon: it connects with D-Bus
//! activation (which starts it when the service file is installed) and, if nobody answers,
//! runs the `playdropd` binary it finds, then gives it a few seconds to come up.
//!
//! The decisions (what to do after an attempt, what a wake means) live in [`Plan`], which has
//! no I/O and is unit tested.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use cosmic::iced::futures::channel::mpsc::Sender;
use cosmic::iced::futures::SinkExt as _;
use futures_util::StreamExt as _;
use playdrop_core::host::{PeerSummary, TransferSnapshot};
use playdrop_ipc::client::{Activation, Client, ClientOptions, ConnectError, DaemonWatch};
use playdrop_ipc::{ErrorCode, Event, IpcError, Ok as Answer, Request, Status};
use tokio::sync::mpsc;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;

use crate::launch;
use crate::model::ConnectProblem;

/// First wait after a failure.
const FIRST_DELAY: Duration = Duration::from_secs(1);
/// Longest wait between attempts.
const MAX_DELAY: Duration = Duration::from_secs(30);
/// Wait before asking again while the daemon says its host is still starting.
const STARTING_DELAY: Duration = Duration::from_millis(500);
/// Consecutive "starting" answers (about 10 s at [`STARTING_DELAY`]) after which the daemon
/// counts as running but not ready, and the normal back-off applies.
const STARTING_LIMIT: u32 = 20;
/// Longest wait for any one answer from the daemon. zbus has no timeout of its own, so a
/// hung daemon would otherwise leave the popup waiting forever.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// After running `playdropd` by hand, the waits between attempts to reach it (about 8 s).
const START_ATTEMPTS: [Duration; 6] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(3),
];

/// The longest the start button waits for a daemon it started before saying it did not start.
const START_DEADLINE: Duration = Duration::from_secs(20);

/// A shared, verified connection. `Debug` shows nothing about the daemon.
#[derive(Clone)]
pub struct Handle(pub Arc<Client>);

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Handle")
    }
}

/// What a fresh connection loads before it is announced.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// `status`.
    pub status: Box<Status>,
    /// `peers`.
    pub peers: Vec<PeerSummary>,
    /// `transfers`.
    pub transfers: Vec<TransferSnapshot>,
}

/// Something to tell the connection loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// Try now instead of waiting out the back-off (never starts the daemon).
    Retry,
    /// The user asked for the daemon: connect with activation, and run `playdropd` if
    /// nobody answers.
    Start,
}

/// What the connection loop reports.
#[derive(Debug, Clone)]
pub enum Link {
    /// The loop runs; send [`Wake`]s here.
    Ready(mpsc::UnboundedSender<Wake>),
    /// Connected and loaded.
    Connected(Handle, Box<Snapshot>),
    /// An attempt failed.
    ConnectFailed(ConnectProblem),
    /// "Start `PlayDrop`" did not bring the daemon up; the reason is worded for the user.
    StartFailed(String),
    /// A daemon event.
    Event(Box<Event>),
    /// The applet fell behind the event stream; reload everything.
    Lagged,
    /// The daemon went away.
    Disconnected,
}

/// Runs `fut`, answering an error when the daemon does not answer in [`REQUEST_TIMEOUT`].
pub async fn timed<T>(fut: impl Future<Output = Result<T, IpcError>>) -> Result<T, IpcError> {
    tokio::time::timeout(REQUEST_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| {
            Err(IpcError::new(
                ErrorCode::Internal,
                "the PlayDrop daemon did not answer within 15 seconds",
            ))
        })
}

fn problem(e: &ConnectError) -> ConnectProblem {
    match e {
        ConnectError::NotRunning => ConnectProblem::NotRunning,
        ConnectError::Version { .. } => {
            ConnectProblem::Other(format!("{e}. Update PlayDrop so both parts match."))
        }
        other => ConnectProblem::Other(other.to_string()),
    }
}

fn unexpected(op: &str) -> IpcError {
    IpcError::new(ErrorCode::Internal, format!("unexpected answer to {op}"))
}

/// Loads `status`, `peers` and `transfers`.
pub async fn load(client: &Client) -> Result<Snapshot, IpcError> {
    let status = load_status(client).await?;
    let peers = load_peers(client).await?;
    let transfers = load_transfers(client).await?;
    Ok(Snapshot {
        status: Box::new(status),
        peers,
        transfers,
    })
}

/// Loads `peers`, with the request timeout.
pub async fn load_peers(client: &Client) -> Result<Vec<PeerSummary>, IpcError> {
    match timed(client.request(Request::Peers)).await? {
        Answer::Peers(p) => Ok(p),
        _ => Err(unexpected("peers")),
    }
}

/// Loads `transfers`, with the request timeout.
pub async fn load_transfers(client: &Client) -> Result<Vec<TransferSnapshot>, IpcError> {
    match timed(client.request(Request::Transfers)).await? {
        Answer::Transfers(t) => Ok(t),
        _ => Err(unexpected("transfers")),
    }
}

/// Loads `status`, with the request timeout.
pub async fn load_status(client: &Client) -> Result<Status, IpcError> {
    match timed(client.request(Request::Status)).await? {
        Answer::Status(s) => Ok(s),
        _ => Err(unexpected("status")),
    }
}

/// How one connection attempt ended, for [`Plan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A session ran and then the daemon went away (quit, restart or crash).
    Ended,
    /// Nobody owns the daemon's name.
    NotRunning,
    /// The daemon answered "starting; try again" while its host comes up.
    StartingUp,
    /// It kept answering "starting" past [`STARTING_LIMIT`]: its host did not come up.
    NotReady,
    /// Anything else: unverified owner, version mismatch, a load that failed or timed out.
    Failed,
}

/// How to wait before the next attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wait {
    /// Poll again after this long.
    pub delay: Duration,
    /// Also wake as soon as the daemon's bus name gains an owner. Only after the name was
    /// found empty: with an owner that cannot be used (wrong version, unverified) the watch
    /// would fire at once, every time.
    pub watch_name: bool,
}

/// What ended a wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// The back-off delay ran out.
    Timer,
    /// The daemon's bus name gained an owner.
    NameAppeared,
    /// The applet asked for something.
    Wake(Wake),
    /// The applet is gone.
    Closed,
}

/// How to make the next attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// Connect to a running daemon only.
    Auto,
    /// The user's "Start `PlayDrop`": may start the daemon.
    Start,
    /// Stop the loop.
    Exit,
}

/// The connection loop's decisions, without I/O.
#[derive(Debug, Clone)]
pub struct Plan {
    delay: Duration,
    /// Consecutive `StartingUp` outcomes.
    starting: u32,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            delay: FIRST_DELAY,
            starting: 0,
        }
    }
}

impl Plan {
    /// Turns a run of `StartingUp` answers longer than [`STARTING_LIMIT`] into `NotReady`;
    /// any other outcome ends the run. Call it on every attempt's outcome before
    /// [`Plan::after`].
    pub fn classify(&mut self, outcome: Outcome) -> Outcome {
        if outcome == Outcome::StartingUp {
            self.starting = self.starting.saturating_add(1);
            if self.starting > STARTING_LIMIT {
                return Outcome::NotReady;
            }
        } else {
            self.starting = 0;
        }
        outcome
    }

    /// How to wait after an attempt that ended with `outcome`.
    pub fn after(&mut self, outcome: Outcome) -> Wait {
        match outcome {
            Outcome::Ended => {
                // Gone after a session: a restart usually follows within a second or two,
                // and the name watch catches it at once.
                self.delay = FIRST_DELAY;
                Wait {
                    delay: FIRST_DELAY,
                    watch_name: true,
                }
            }
            Outcome::StartingUp => Wait {
                delay: STARTING_DELAY,
                watch_name: false,
            },
            Outcome::NotRunning | Outcome::Failed | Outcome::NotReady => {
                let wait = Wait {
                    delay: self.delay,
                    watch_name: outcome == Outcome::NotRunning,
                };
                self.delay = (self.delay * 2).min(MAX_DELAY);
                wait
            }
        }
    }

    /// What to do about what ended the wait.
    pub fn on(&mut self, next: Next) -> Attempt {
        match next {
            Next::Closed => Attempt::Exit,
            Next::Timer => Attempt::Auto,
            Next::NameAppeared | Next::Wake(Wake::Retry) => {
                self.delay = FIRST_DELAY;
                Attempt::Auto
            }
            Next::Wake(Wake::Start) => {
                self.delay = FIRST_DELAY;
                Attempt::Start
            }
        }
    }

    /// The current back-off delay (tests).
    #[cfg(test)]
    fn delay(&self) -> Duration {
        self.delay
    }
}

/// Folds wakes queued behind `first` into one: a burst of clicks is one attempt, and a
/// `Start` anywhere in it wins.
pub fn collapse(first: Next, rx: &mut mpsc::UnboundedReceiver<Wake>) -> Next {
    let mut next = first;
    while let Ok(w) = rx.try_recv() {
        if w == Wake::Start || next != Next::Wake(Wake::Start) {
            next = Next::Wake(w);
        }
    }
    next
}

/// Drops every queued wake. Called when a session connects and when it ends, so a click
/// made while connecting (a second "Start") cannot fire hours later after a quit.
pub fn drain(rx: &mut mpsc::UnboundedReceiver<Wake>) -> usize {
    let mut n = 0;
    while rx.try_recv().is_ok() {
        n += 1;
    }
    n
}

/// A connected, loaded session ready to pump.
struct Live {
    client: Arc<Client>,
    events: BroadcastStream<Event>,
    snapshot: Snapshot,
}

/// Connects (with or without activation) and loads the snapshot.
async fn attempt(activation: Activation) -> Result<Live, (Outcome, ConnectProblem)> {
    // The connect asks the daemon for its version: a daemon that owns the name but is
    // stopped or stuck must not hang the loop, which would then never read a "Start" click.
    let connect = Client::connect_with_activation(ClientOptions::default(), activation);
    let client = match tokio::time::timeout(REQUEST_TIMEOUT, connect).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            let p = problem(&e);
            let o = if p == ConnectProblem::NotRunning {
                Outcome::NotRunning
            } else {
                Outcome::Failed
            };
            return Err((o, p));
        }
        Err(_) => {
            return Err((
                Outcome::Failed,
                ConnectProblem::Other(
                    "the PlayDrop daemon did not answer within 15 seconds".to_owned(),
                ),
            ))
        }
    };
    let client = Arc::new(client);
    // Subscribe before loading, so nothing between the snapshot and the first event is lost.
    let events = client.events();
    match load(&client).await {
        Ok(snapshot) => Ok(Live {
            client,
            events,
            snapshot,
        }),
        Err(e) if e.code == ErrorCode::InvalidState => {
            // "PlayDrop is starting; try again": the host is coming up.
            Err((Outcome::StartingUp, ConnectProblem::Starting))
        }
        Err(e) => {
            tracing::info!(code = %e.code, "connected but the first answers failed");
            Err((Outcome::Failed, ConnectProblem::Other(e.message)))
        }
    }
}

/// Announces a live session and pumps its events until the daemon goes away.
async fn pump(live: Live, output: &mut Sender<Link>, wakes: &mut mpsc::UnboundedReceiver<Wake>) {
    let Live {
        client,
        mut events,
        snapshot,
    } = live;
    tracing::info!(
        daemon = client.daemon_version(),
        "connected to the PlayDrop daemon"
    );
    drain(wakes);
    let _ = output
        .send(Link::Connected(
            Handle(Arc::clone(&client)),
            Box::new(snapshot),
        ))
        .await;
    while let Some(item) = events.next().await {
        match item {
            Ok(event) => {
                let _ = output.send(Link::Event(Box::new(event))).await;
            }
            Err(BroadcastStreamRecvError::Lagged(n)) => {
                tracing::warn!(missed = n, "fell behind the daemon's events; reloading");
                let _ = output.send(Link::Lagged).await;
            }
        }
    }
    tracing::info!("the PlayDrop daemon went away");
    drain(wakes);
    let _ = output.send(Link::Disconnected).await;
}

/// The user's "Start": connect with activation, else run `playdropd` and wait for it.
async fn start(output: &mut Sender<Link>) -> Result<Live, String> {
    match attempt(Activation::Allow).await {
        Ok(live) => return Ok(live),
        // A daemon owns the name and is still coming up: wait for it, run nothing.
        Err((Outcome::StartingUp, _)) => {}
        Err((Outcome::NotRunning, _)) => {
            // Activation could not start it (no service file): look for the binary now,
            // not at start-up, so an install made while the applet ran is found.
            let Some(bin) = launch::find(launch::DAEMON_BIN) else {
                return Err("playdropd was not found".to_owned());
            };
            tracing::info!("starting playdropd");
            if !launch::spawn(bin, None).await {
                return Err("playdropd could not be run".to_owned());
            }
        }
        Err((_, p)) => return Err(p.text()),
    }
    let _ = output
        .send(Link::ConnectFailed(ConnectProblem::Starting))
        .await;
    let mut last = "it did not answer within a few seconds".to_owned();
    // One limit for the whole wait: each attempt alone may take the full request timeout,
    // and "starting" with no button must not last minutes when the new daemon hangs.
    let deadline = tokio::time::Instant::now() + START_DEADLINE;
    for wait in START_ATTEMPTS {
        tokio::time::sleep(wait).await;
        match tokio::time::timeout_at(deadline, attempt(Activation::Never)).await {
            Ok(Ok(live)) => return Ok(live),
            Ok(Err((Outcome::NotRunning | Outcome::StartingUp, _))) => {}
            Ok(Err((_, p))) => last = p.text(),
            Err(_) => break,
        }
    }
    Err(last)
}

/// Waits as `wait` says, for a wake, or for the daemon's name to appear. `watch` is kept
/// across waits (one bus connection for the loop's life) and made again after it fails.
async fn wait_for(
    wait: Wait,
    watch: &mut Option<DaemonWatch>,
    wakes: &mut mpsc::UnboundedReceiver<Wake>,
) -> Next {
    let name = async {
        if wait.watch_name {
            if watch.is_none() {
                match DaemonWatch::new(&ClientOptions::default()).await {
                    Ok(w) => *watch = Some(w),
                    Err(e) => tracing::debug!(error = %e, "no bus watch; polling only"),
                }
            }
            if let Some(w) = watch.as_mut() {
                match w.appeared().await {
                    Ok(()) => return,
                    Err(e) => {
                        // Fall back to the poll; a new watch is made next time.
                        tracing::debug!(error = %e, "bus watch ended");
                        *watch = None;
                    }
                }
            }
        }
        std::future::pending::<()>().await;
    };
    let next = tokio::select! {
        () = tokio::time::sleep(wait.delay) => Next::Timer,
        () = name => Next::NameAppeared,
        w = wakes.recv() => w.map_or(Next::Closed, Next::Wake),
    };
    collapse(next, wakes)
}

/// The connection loop, run as an iced subscription.
pub fn run() -> impl cosmic::iced::futures::Stream<Item = Link> {
    cosmic::iced::stream::channel(64, async move |mut output: Sender<Link>| {
        let (wake_tx, mut wakes) = mpsc::unbounded_channel::<Wake>();
        let _ = output.send(Link::Ready(wake_tx)).await;
        let mut plan = Plan::default();
        let mut watch: Option<DaemonWatch> = None;
        let mut how = Attempt::Auto;
        // Log the first failure of a run of failures at info, the repeats at debug.
        let mut quiet = false;
        loop {
            let outcome = match how {
                Attempt::Exit => return,
                Attempt::Auto => match attempt(Activation::Never).await {
                    Ok(live) => {
                        quiet = false;
                        pump(live, &mut output, &mut wakes).await;
                        Outcome::Ended
                    }
                    Err((o, p)) => {
                        let o = plan.classify(o);
                        let p = if o == Outcome::NotReady {
                            ConnectProblem::NotReady
                        } else {
                            p
                        };
                        if quiet {
                            tracing::debug!(?p, "could not connect to the PlayDrop daemon");
                        } else {
                            tracing::info!(?p, "could not connect to the PlayDrop daemon");
                            quiet = true;
                        }
                        let _ = output.send(Link::ConnectFailed(p)).await;
                        o
                    }
                },
                Attempt::Start => match start(&mut output).await {
                    Ok(live) => {
                        quiet = false;
                        pump(live, &mut output, &mut wakes).await;
                        Outcome::Ended
                    }
                    Err(reason) => {
                        tracing::info!("PlayDrop did not start");
                        // Clicks made while it tried are answered by this failure. Drained
                        // before the failure is shown: a click made after the button comes
                        // back must not be thrown away, or "starting" would never end.
                        drain(&mut wakes);
                        let _ = output.send(Link::StartFailed(reason)).await;
                        plan.classify(Outcome::NotRunning)
                    }
                },
            };
            let outcome = if outcome == Outcome::Ended {
                plan.classify(outcome)
            } else {
                outcome
            };
            let wait = plan.after(outcome);
            let next = wait_for(wait, &mut watch, &mut wakes).await;
            how = plan.on(next);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_back_off_doubling_up_to_thirty_seconds() {
        let mut plan = Plan::default();
        let delays: Vec<u64> = (0..8)
            .map(|_| plan.after(Outcome::Failed).delay.as_secs())
            .collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[test]
    fn only_an_empty_name_is_watched() {
        let mut plan = Plan::default();
        assert!(plan.after(Outcome::NotRunning).watch_name);
        // A name owned by something unusable would fire the watch at once, forever.
        assert!(!plan.after(Outcome::Failed).watch_name);
        assert!(!plan.after(Outcome::StartingUp).watch_name);
        assert!(plan.after(Outcome::Ended).watch_name);
    }

    #[test]
    fn a_daemon_that_is_starting_is_asked_again_soon_without_growing_the_back_off() {
        let mut plan = Plan::default();
        plan.after(Outcome::NotRunning);
        plan.after(Outcome::NotRunning);
        let before = plan.delay();
        let w = plan.after(Outcome::StartingUp);
        assert_eq!(w.delay, STARTING_DELAY);
        assert_eq!(plan.delay(), before);
    }

    #[test]
    fn a_daemon_stuck_starting_becomes_not_ready_and_backs_off() {
        let mut plan = Plan::default();
        for _ in 0..STARTING_LIMIT {
            let o = plan.classify(Outcome::StartingUp);
            assert_eq!(o, Outcome::StartingUp);
            assert_eq!(plan.after(o).delay, STARTING_DELAY);
        }
        // About 10 s of "starting": now it is "not ready" with the doubling back-off.
        let delays: Vec<u64> = (0..4)
            .map(|_| {
                let o = plan.classify(Outcome::StartingUp);
                assert_eq!(o, Outcome::NotReady);
                let w = plan.after(o);
                assert!(!w.watch_name);
                w.delay.as_secs()
            })
            .collect();
        assert_eq!(delays, [1, 2, 4, 8]);
        // Any other answer ends the run: a later "starting" gets the short retry again.
        assert_eq!(plan.classify(Outcome::NotRunning), Outcome::NotRunning);
        assert_eq!(plan.classify(Outcome::StartingUp), Outcome::StartingUp);
    }

    #[test]
    fn wakes_queued_during_a_failed_start_are_dropped() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(Wake::Start).expect("send");
        tx.send(Wake::Start).expect("send");
        // The start fails; the loop drains what was clicked meanwhile.
        assert_eq!(drain(&mut rx), 2);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_session_that_ends_resets_the_back_off_and_watches_for_a_restart() {
        let mut plan = Plan::default();
        for _ in 0..6 {
            plan.after(Outcome::NotRunning);
        }
        assert_eq!(plan.delay(), MAX_DELAY);
        let w = plan.after(Outcome::Ended);
        assert_eq!(
            w,
            Wait {
                delay: FIRST_DELAY,
                watch_name: true
            }
        );
    }

    #[test]
    fn automatic_attempts_never_start_the_daemon() {
        let mut plan = Plan::default();
        // Timer, a restart seen on the bus, and a popup-open retry all connect only to a
        // running daemon; a quit stays a quit because none of them activates.
        assert_eq!(plan.on(Next::Timer), Attempt::Auto);
        assert_eq!(plan.on(Next::NameAppeared), Attempt::Auto);
        assert_eq!(plan.on(Next::Wake(Wake::Retry)), Attempt::Auto);
        assert_eq!(plan.on(Next::Wake(Wake::Start)), Attempt::Start);
        assert_eq!(plan.on(Next::Closed), Attempt::Exit);
    }

    #[test]
    fn the_name_appearing_resets_the_back_off() {
        let mut plan = Plan::default();
        for _ in 0..4 {
            plan.after(Outcome::NotRunning);
        }
        plan.on(Next::NameAppeared);
        assert_eq!(plan.delay(), FIRST_DELAY);
        // A plain timer keeps the grown delay.
        for _ in 0..3 {
            plan.after(Outcome::NotRunning);
        }
        let grown = plan.delay();
        plan.on(Next::Timer);
        assert_eq!(plan.delay(), grown);
    }

    #[test]
    fn a_burst_of_wakes_is_one_and_start_wins() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        assert_eq!(collapse(Next::Timer, &mut rx), Next::Timer);
        tx.send(Wake::Retry).expect("send");
        tx.send(Wake::Start).expect("send");
        tx.send(Wake::Retry).expect("send");
        assert_eq!(collapse(Next::Timer, &mut rx), Next::Wake(Wake::Start));
        assert!(rx.try_recv().is_err());
        tx.send(Wake::Retry).expect("send");
        assert_eq!(
            collapse(Next::Wake(Wake::Retry), &mut rx),
            Next::Wake(Wake::Retry)
        );
        drop(tx);
        assert_eq!(collapse(Next::Closed, &mut rx), Next::Closed);
    }

    #[test]
    fn a_second_start_click_does_not_survive_the_session() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut plan = Plan::default();
        // First click starts; a second click lands while that start connects.
        tx.send(Wake::Start).expect("send");
        let first = collapse(Next::Closed, &mut rx);
        assert_eq!(plan.on(first), Attempt::Start);
        tx.send(Wake::Start).expect("send");
        // The session connects: queued wakes are dropped.
        assert_eq!(drain(&mut rx), 1);
        // Hours later the user quits the daemon: nothing queued restarts it.
        plan.after(Outcome::Ended);
        assert!(rx.try_recv().is_err());
        assert_eq!(plan.on(Next::Timer), Attempt::Auto);
    }

    #[test]
    fn connect_errors_sort_into_not_running_and_the_rest() {
        assert_eq!(
            problem(&ConnectError::NotRunning),
            ConnectProblem::NotRunning
        );
        match problem(&ConnectError::Version {
            daemon: 2,
            client: 1,
        }) {
            ConnectProblem::Other(m) => assert!(m.contains("Update PlayDrop"), "{m}"),
            other => panic!("version is not {other:?}"),
        }
        assert!(matches!(
            problem(&ConnectError::Unverified("uid".into())),
            ConnectProblem::Other(_)
        ));
    }

    #[tokio::test]
    async fn requests_that_hang_time_out() {
        tokio::time::pause();
        let r: Result<(), IpcError> = timed(std::future::pending()).await;
        let e = r.expect_err("timed out");
        assert!(e.message.contains("did not answer"), "{}", e.message);
    }
}

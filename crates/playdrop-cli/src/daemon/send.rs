//! `playdrop send`: hand files to the daemon, then follow the transfer to its end.
//!
//! The event stream is opened before the send call, so no progress or completion event
//! can slip past between "transfer started" and the first read. Events drive the progress
//! line; once a second the command also asks the daemon for its transfer list, which
//! covers a lagged event stream and fills in the file count.
//!
//! Ctrl-C cancels the transfer. The handler is installed before the first call to the
//! daemon and stays armed for the whole command, so a Ctrl-C that lands while a request is
//! in flight is acted on as soon as that request returns, and one pressed before the daemon has
//! answered "transfer started" cancels the transfer once its ID is known. A second Ctrl-C
//! while cancelling or reading the final state gives up with exit code 1.

use std::future::Future;
use std::io::{IsTerminal as _, Write as _};
use std::pin::Pin;
use std::time::{Duration, Instant};

use futures_util::{Stream, StreamExt as _};
use playdrop_core::host::{HostEvent, TransferDirection, TransferSnapshot, TransferStatus};
use playdrop_ipc::client::Client;
use playdrop_ipc::{DaemonEvent, Event, Ok as Answer, Request};
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

use super::output::{self, Progress};
use super::{absolute_paths, ask, connect, peers, pick_peer, unexpected, Exit, Failure, SendArgs};

/// How many once-a-second list reads may miss a transfer the daemon never listed before
/// the command gives up on it.
const NEVER_LISTED_TICKS: u32 = 10;

/// How the follow loop ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum End {
    /// `TransferComplete` arrived, or the list says so.
    Complete,
    /// `TransferFailed` arrived (failed, declined or cancelled) with its reason.
    Failed(String),
    /// The transfer list shows a finished state.
    Listed(Box<TransferSnapshot>),
    /// The user pressed Ctrl-C and a cancel was requested.
    Interrupted,
}

/// A Ctrl-C listener that is registered as soon as it is created and can be re-armed
/// after it fires, so a second Ctrl-C is caught too.
struct CtrlC(Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>);

impl CtrlC {
    /// Installs the handler and starts listening now: `tokio::signal::ctrl_c` registers
    /// on its first poll, and a Ctrl-C before that would end the process outright.
    async fn armed() -> Self {
        let mut fut: Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>> =
            Box::pin(tokio::signal::ctrl_c());
        // The first poll registers the listener; it cannot already be ready.
        let _ = futures_util::poll!(&mut fut);
        Self(fut)
    }

    /// Waits for the next Ctrl-C, then re-arms for the one after.
    async fn fired(&mut self) {
        let _ = (&mut self.0).await;
        *self = Self::armed().await;
    }
}

/// The failure when the user presses Ctrl-C again while the command is winding down.
fn interrupted_again(id: Option<&str>) -> Failure {
    Failure::new(
        Exit::Failed,
        match id {
            Some(id) => format!("interrupted; check `playdrop transfers` for {id}"),
            None => "interrupted; check `playdrop transfers`".to_owned(),
        },
    )
}

/// The failure when the daemon goes away in the middle of a transfer.
fn daemon_gone(id: &str) -> Failure {
    Failure::new(
        Exit::Failed,
        format!(
            "the daemon went away before transfer {id} finished; \
             it resumes when the daemon is back (`playdrop transfers`)"
        ),
    )
}

/// The failure when the daemon no longer lists a transfer that has not been seen to end.
fn lost(id: &str) -> Failure {
    Failure::new(
        Exit::Failed,
        format!(
            "the daemon does not list transfer {id} and its end was not seen; \
             check `playdrop transfers`"
        ),
    )
}

/// Runs `playdrop send`.
pub async fn run(args: SendArgs) -> Result<(), Failure> {
    let paths = absolute_paths(&args.paths)?;
    // Armed before the first call to the daemon: a Ctrl-C in the first few milliseconds
    // would otherwise end the process by signal (exit 130) instead of cleanly.
    let mut ctrl_c = CtrlC::armed().await;
    let setup = async {
        let client = connect().await?;
        // Subscribe first: events from here on are buffered until read.
        let events = client.events();
        let peers = peers(&client).await?;
        let peer = pick_peer(&peers, args.to.as_deref())?;
        let target = (peer.device_id.clone(), peer.label.clone());
        Ok::<_, Failure>((client, events, target))
    };
    // Nothing has been sent yet, so there is nothing to cancel: stop with a plain message.
    let (client, mut events, (device_id, label)) = tokio::select! {
        r = setup => r?,
        () = ctrl_c.fired() => {
            return Err(Failure::new(Exit::Failed, "interrupted before the transfer started"));
        }
    };

    let started = Instant::now();
    let sending = client.send_files(&device_id, paths);
    tokio::pin!(sending);
    let (answer, interrupted) = tokio::select! {
        r = &mut sending => (r, false),
        () = ctrl_c.fired() => {
            eprintln!("cancelling once the daemon has started the transfer");
            tokio::select! {
                r = &mut sending => (r, true),
                () = ctrl_c.fired() => return Err(interrupted_again(None)),
            }
        }
    };
    let answer = answer.map_err(Failure::from)?;
    let Answer::TransferStarted { id } = &answer else {
        return Err(unexpected("send_files", &answer));
    };
    let id = id.clone();

    let end = if interrupted {
        cancel(&client, &id, &mut ctrl_c).await?;
        End::Interrupted
    } else if args.no_wait {
        if args.json {
            println!("{}", output::json(&answer));
        } else {
            println!("{id}");
        }
        return Ok(());
    } else {
        follow(&client, &mut events, &id, &label, &mut ctrl_c).await?
    };
    let wall = started.elapsed().as_secs_f64();

    let snapshot = tokio::select! {
        r = final_snapshot(&client, &id, end) => r?,
        () = ctrl_c.fired() => return Err(interrupted_again(Some(&id))),
    };
    if args.json {
        println!("{}", output::json(&snapshot));
    }
    if snapshot.state == TransferStatus::Complete {
        if !args.json {
            println!("{}", output::sent_line(&snapshot, &label, wall));
        }
        Ok(())
    } else {
        let why = snapshot.note.as_deref().unwrap_or("no reason given");
        Err(Failure::new(
            Exit::Failed,
            format!(
                "transfer {id} to {label} {}: {why}",
                output::state_name(snapshot.state)
            ),
        ))
    }
}

/// Asks the daemon to cancel `id`. A failed cancel is ignored: the transfer most likely
/// finished at the same moment, and the final snapshot reports what really happened. A
/// second Ctrl-C while the request is in flight gives up.
async fn cancel(client: &Client, id: &str, ctrl_c: &mut CtrlC) -> Result<(), Failure> {
    eprintln!("cancelling {id}");
    tokio::select! {
        _ = ask(client, Request::CancelTransfer { id: id.to_owned() }) => Ok(()),
        () = ctrl_c.fired() => Err(interrupted_again(Some(id))),
    }
}

/// Whether the daemon still knows a transfer, from the once-a-second list reads.
#[derive(Debug, Default)]
struct Listing {
    /// The transfer was in the list at least once.
    seen: bool,
    /// List reads that did not contain it while it was never seen.
    misses: u32,
}

impl Listing {
    /// Records one list read. `false` means give up: the transfer was listed before and
    /// is gone without its end having been seen (the daemon evicted the finished snapshot
    /// while the event stream lagged), or it never showed up in
    /// [`NEVER_LISTED_TICKS`] reads.
    fn observe(&mut self, found: bool) -> bool {
        if found {
            self.seen = true;
            return true;
        }
        if self.seen {
            return false;
        }
        self.misses += 1;
        self.misses < NEVER_LISTED_TICKS
    }
}

/// Follows transfer `id` on the event stream (plus a once-a-second list read) until it
/// ends, drawing the progress line on stderr.
async fn follow<E>(
    client: &Client,
    events: &mut (impl Stream<Item = Result<Event, E>> + Unpin),
    id: &str,
    label: &str,
    ctrl_c: &mut CtrlC,
) -> Result<End, Failure> {
    let mut meter = Meter::new(std::io::stderr().is_terminal());
    let mut progress = Progress::default();
    let mut listing = Listing::default();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let end = loop {
        tokio::select! {
            () = ctrl_c.fired() => {
                meter.clear();
                cancel(client, id, ctrl_c).await?;
                break End::Interrupted;
            }
            ev = events.next() => match ev {
                None => {
                    meter.clear();
                    return Err(daemon_gone(id));
                }
                // Lagged: the once-a-second list read below catches up.
                Some(Err(_)) => {}
                Some(Ok(ev)) => match ev {
                    Event::Host(HostEvent::TransferProgress {
                        id: i, direction: TransferDirection::Send,
                        bytes, total, bytes_per_sec, files_done, ..
                    }) if i == id => {
                        progress.bytes = bytes;
                        progress.total = total;
                        progress.bytes_per_sec = bytes_per_sec;
                        progress.files_done = files_done;
                        // A daemon that still reports zero-byte progress while the
                        // receiver decides must not end the wait; current ones send no
                        // progress until bytes move.
                        progress.waiting &= bytes == 0;
                        meter.event(&output::progress_line(&progress, label));
                    }
                    Event::Host(HostEvent::TransferComplete {
                        id: i, direction: TransferDirection::Send, ..
                    }) if i == id => break End::Complete,
                    Event::Host(HostEvent::TransferFailed {
                        id: i, direction: TransferDirection::Send, reason, ..
                    }) if i == id => break End::Failed(reason),
                    Event::Daemon(DaemonEvent::ShuttingDown) => {
                        meter.clear();
                        return Err(Failure::new(
                            Exit::Failed,
                            format!("the daemon is shutting down; transfer {id} resumes \
                                     when it is back (`playdrop transfers`)"),
                        ));
                    }
                    _ => {}
                },
            },
            _ = tick.tick() => {
                let Ok(found) = listed(client, id).await else {
                    meter.clear();
                    return Err(daemon_gone(id));
                };
                if !listing.observe(found.is_some()) {
                    meter.clear();
                    return Err(lost(id));
                }
                if let Some(t) = found {
                    if t.state.is_finished() {
                        break End::Listed(Box::new(t));
                    }
                    progress.bytes = progress.bytes.max(t.bytes);
                    progress.total = t.total;
                    progress.files = t.file_count;
                    progress.files_done = progress.files_done.max(t.files_done);
                    progress.waiting = t.state == TransferStatus::Offered
                        || (progress.waiting && t.bytes == 0);
                    if progress.bytes_per_sec == 0 || t.bytes_per_sec > 0 {
                        progress.bytes_per_sec = t.bytes_per_sec;
                    }
                }
                meter.tick(&output::progress_line(&progress, label));
            }
        }
    };
    meter.clear();
    Ok(end)
}

/// This transfer as the daemon lists it, if it still does.
async fn listed(client: &Client, id: &str) -> Result<Option<TransferSnapshot>, Failure> {
    match ask(client, Request::Transfers).await? {
        Answer::Transfers(list) => Ok(list
            .into_iter()
            .find(|t| t.id == id && t.direction == TransferDirection::Send)),
        other => Err(unexpected("transfers", &other)),
    }
}

/// The finished snapshot. The event can arrive a moment before the list shows the final
/// state, so look a few times; if the list never catches up, take the event's word for it.
async fn final_snapshot(client: &Client, id: &str, end: End) -> Result<TransferSnapshot, Failure> {
    if let End::Listed(t) = end {
        return Ok(*t);
    }
    let mut last = None;
    for _ in 0..20 {
        if let Some(t) = listed(client, id).await? {
            if t.state.is_finished() {
                return Ok(settle(t, &end));
            }
            last = Some(t);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let Some(t) = last else {
        return Err(lost(id));
    };
    Ok(settle(t, &end))
}

/// Combines the daemon's snapshot with how the follow loop saw the transfer end.
///
/// A finished snapshot keeps its own state; only a missing note is filled from the
/// failure event, so the user always sees the reason that was delivered. A snapshot the
/// list never moved to a finished state takes the event's word for it.
fn settle(mut t: TransferSnapshot, end: &End) -> TransferSnapshot {
    if t.state.is_finished() {
        if let End::Failed(reason) = end {
            if t.state != TransferStatus::Complete && t.note.is_none() {
                t.note = Some(reason.clone());
            }
        }
        return t;
    }
    match end {
        End::Complete => {
            t.state = TransferStatus::Complete;
            t.bytes = t.total;
        }
        End::Failed(reason) => {
            t.state = TransferStatus::Failed;
            t.note = Some(reason.clone());
        }
        End::Interrupted => {
            t.state = TransferStatus::Cancelled;
            t.note = Some("cancelled with Ctrl-C".to_owned());
        }
        End::Listed(_) => {}
    }
    t.bytes_per_sec = 0;
    t
}

/// Columns the in-place progress line may use: one less than the terminal is wide
/// (writing the last column makes some terminals wrap). The width comes from the
/// operating system first, then from `COLUMNS` when the shell exports it, else 79.
fn line_width(os_columns: Option<u16>, columns_env: Option<&str>) -> usize {
    // A width of 0 or 1 is a terminal that does not know its size.
    os_columns
        .map(usize::from)
        .filter(|c| *c > 1)
        .or_else(|| {
            columns_env
                .and_then(|c| c.trim().parse::<usize>().ok())
                .filter(|c| *c > 1)
        })
        .map_or(79, |c| c - 1)
}

/// The width of the terminal on standard error, if it is one.
fn os_columns() -> Option<u16> {
    terminal_size::terminal_size_of(std::io::stderr()).map(|(terminal_size::Width(w), _)| w)
}

/// `line` cut to at most `width` display columns, never inside a character. A wide
/// character that would cross the edge is dropped whole.
fn truncate(line: &str, width: usize) -> &str {
    let mut used = 0;
    for (i, c) in line.char_indices() {
        used += c.width().unwrap_or(0);
        if used > width {
            return &line[..i];
        }
    }
    line
}

/// The progress line on stderr: redrawn in place on a terminal (at most ten times a
/// second, cut to the terminal width so it never wraps), one line per second otherwise,
/// so a log stays readable.
struct Meter {
    tty: bool,
    width: usize,
    last_draw: Option<Instant>,
    drawn_width: usize,
}

impl Meter {
    fn new(tty: bool) -> Self {
        Self {
            tty,
            width: line_width(os_columns(), std::env::var("COLUMNS").ok().as_deref()),
            last_draw: None,
            drawn_width: 0,
        }
    }

    /// A progress event: redraw on a terminal if the last draw is old enough.
    fn event(&mut self, line: &str) {
        if self.tty
            && self
                .last_draw
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(100))
        {
            self.draw(line);
        }
    }

    /// The once-a-second tick: always draw.
    fn tick(&mut self, line: &str) {
        self.draw(line);
    }

    fn draw(&mut self, line: &str) {
        let mut err = std::io::stderr().lock();
        if self.tty {
            let line = truncate(line, self.width);
            let width = line.width();
            let pad = self.drawn_width.saturating_sub(width);
            let _ = write!(err, "\r{line}{}", " ".repeat(pad));
            self.drawn_width = width;
        } else {
            let _ = writeln!(err, "{line}");
        }
        let _ = err.flush();
        self.last_draw = Some(Instant::now());
    }

    /// Wipe the in-place line before the final output.
    fn clear(&mut self) {
        if self.tty && self.drawn_width > 0 {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r{}\r", " ".repeat(self.drawn_width));
            let _ = err.flush();
            self.drawn_width = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(state: TransferStatus, note: Option<&str>) -> TransferSnapshot {
        TransferSnapshot {
            id: "t1".into(),
            peer: "ab".repeat(32),
            short_id: "abab-abab".into(),
            direction: TransferDirection::Send,
            state,
            bytes: 10,
            total: 100,
            bytes_per_sec: 5,
            files_done: 0,
            started_at: 0,
            file_count: 1,
            note: note.map(str::to_owned),
            saved: Vec::new(),
            folder: None,
            delivered: Vec::new(),
            resumable: false,
        }
    }

    #[test]
    fn listing_gives_up_when_a_seen_transfer_disappears() {
        let mut l = Listing::default();
        assert!(l.observe(true));
        assert!(l.observe(true));
        assert!(!l.observe(false));
    }

    #[test]
    fn listing_gives_up_after_ten_reads_without_it() {
        let mut l = Listing::default();
        for _ in 1..NEVER_LISTED_TICKS {
            assert!(l.observe(false));
        }
        assert!(!l.observe(false));
        // Showing up late resets nothing it needs: from then on a miss is fatal.
        let mut l = Listing::default();
        assert!(l.observe(false));
        assert!(l.observe(true));
        assert!(!l.observe(false));
    }

    #[test]
    fn failure_reason_fills_a_missing_note() {
        let end = End::Failed("declined by the receiver".into());
        let t = settle(snapshot(TransferStatus::Cancelled, None), &end);
        assert_eq!(t.state, TransferStatus::Cancelled);
        assert_eq!(t.note.as_deref(), Some("declined by the receiver"));
        // The daemon's own note wins.
        let t = settle(snapshot(TransferStatus::Failed, Some("disk full")), &end);
        assert_eq!(t.note.as_deref(), Some("disk full"));
        // A complete transfer is never given a failure note.
        let t = settle(snapshot(TransferStatus::Complete, None), &end);
        assert_eq!(t.note, None);
    }

    #[test]
    fn unfinished_snapshot_takes_the_event_word() {
        let t = settle(snapshot(TransferStatus::Active, None), &End::Complete);
        assert_eq!(t.state, TransferStatus::Complete);
        assert_eq!((t.bytes, t.bytes_per_sec), (100, 0));
        let t = settle(
            snapshot(TransferStatus::Active, None),
            &End::Failed("peer unreachable".into()),
        );
        assert_eq!(t.state, TransferStatus::Failed);
        assert_eq!(t.note.as_deref(), Some("peer unreachable"));
        let t = settle(snapshot(TransferStatus::Offered, None), &End::Interrupted);
        assert_eq!(t.state, TransferStatus::Cancelled);
    }

    #[test]
    fn progress_line_fits_the_terminal() {
        assert_eq!(line_width(None, None), 79);
        assert_eq!(line_width(None, Some("120")), 119);
        assert_eq!(line_width(None, Some(" 40 ")), 39);
        assert_eq!(line_width(None, Some("wide")), 79);
        assert_eq!(line_width(None, Some("0")), 79);
        // The operating system's answer wins over a stale COLUMNS.
        assert_eq!(line_width(Some(100), Some("40")), 99);
        // A terminal that reports no width (0) falls back to COLUMNS, then 79.
        assert_eq!(line_width(Some(0), Some("40")), 39);
        assert_eq!(line_width(Some(1), None), 79);
        assert_eq!(truncate("abcdef", 4), "abcd");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(truncate("añbñc", 3), "añb");
        assert_eq!(truncate("", 0), "");
        // Wide characters count two columns and are never split.
        assert_eq!(truncate("日本語", 4), "日本");
        assert_eq!(truncate("日本語", 5), "日本");
        assert_eq!(truncate("日本語", 6), "日本語");
        assert_eq!(truncate("a🎉b", 2), "a");
        assert_eq!(truncate("a🎉b", 3), "a🎉");
    }
}

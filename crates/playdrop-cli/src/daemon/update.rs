//! `playdrop update`: the update state, a check, or the update itself
//! (`docs/design/update-check.md`, `docs/ipc.md` 1.9).
//!
//! Without options it shows what the daemon knows and contacts no one. `--check` asks
//! GitHub now, like "Check now" in the window (it skips the once-a-day wait, never the
//! setting). `--install` starts the update and follows it to its end: the system password
//! prompt (or UAC) appears as it would for the window. Ctrl-C during the download cancels
//! it (`update_cancel`); once the system installer has the files it can no longer be
//! stopped from here, and the command says so.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use futures_util::StreamExt as _;
use playdrop_ipc::client::Client;
use playdrop_ipc::{
    DaemonEvent, ErrorCode, Event, IpcError, Ok as Answer, Request, UpdateState, UpdateStatus,
};

use super::{connect, output, unexpected, Exit, Failure};

/// Options of `playdrop update`.
#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("action").args(["check", "install"])))]
pub struct UpdateArgs {
    /// Ask GitHub for the latest version now, even if it was asked today.
    #[arg(long)]
    pub check: bool,
    /// Download, verify and install the offered version, and follow it to the end.
    #[arg(long)]
    pub install: bool,
    /// Print the final update state as one JSON object on stdout.
    #[arg(long)]
    pub json: bool,
}

/// How long to wait for the next word from the daemon while an update runs. The password
/// prompt waits for a person, so this is generous; progress arrives far more often.
const QUIET_LIMIT: Duration = Duration::from_mins(30);

/// Runs `playdrop update`.
pub async fn run(args: UpdateArgs) -> Result<(), Failure> {
    let client = connect().await?;
    if args.install {
        return install(&client, args.json).await;
    }
    let request = if args.check {
        Request::UpdateCheck { force: true }
    } else {
        Request::UpdateState
    };
    let status = status_of(ask_update(&client, request).await?)?;
    print(&status, args.json);
    if let Some(why) = &status.unavailable {
        if !args.json {
            println!("{why}");
        }
        if args.check {
            return Err(Failure::new(Exit::Failed, why.clone()));
        }
    }
    if args.check {
        if status.state == UpdateState::Off {
            return Err(Failure::new(
                Exit::Failed,
                "update checks are off; turn on \"Check for updates\" in the window's Settings",
            ));
        }
        // A check that answered clears the failure, so one that is set is this check's,
        // also when an offer from an earlier check is still shown.
        if let Some(why) = &status.last_failure {
            return Err(Failure::new(Exit::Failed, why.clone()));
        }
    }
    Ok(())
}

/// A Ctrl-C listener registered when it is made (`tokio::signal::ctrl_c` registers on its
/// first poll, and a Ctrl-C before that would end the process without cancelling).
struct CtrlC(Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>);

impl CtrlC {
    async fn armed() -> Self {
        let mut fut: Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>> =
            Box::pin(tokio::signal::ctrl_c());
        let _ = futures_util::poll!(&mut fut);
        Self(fut)
    }

    /// Waits for the next Ctrl-C, then re-arms for the one after.
    async fn fired(&mut self) {
        let _ = (&mut self.0).await;
        *self = Self::armed().await;
    }
}

/// `--install`: start, then follow the events to an end state.
async fn install(client: &Client, json: bool) -> Result<(), Failure> {
    // Subscribe first: events from here on are buffered until read.
    let mut events = client.events();
    let mut ctrl_c = CtrlC::armed().await;
    // The window is not the one asking, so the Windows installer starts only the daemon
    // again.
    let started = status_of(ask_update(client, Request::UpdateInstall { window: false }).await?)?;
    eprintln!("{}", output::update_state_line(&started.state));
    let mut last = started;
    loop {
        let next = tokio::select! {
            next = tokio::time::timeout(QUIET_LIMIT, events.next()) => next,
            () = ctrl_c.fired() => {
                cancel(client).await?;
                continue;
            }
        };
        let status = match next {
            Ok(Some(Ok(Event::Daemon(DaemonEvent::UpdateChanged(s))))) => s,
            // On Windows a successful installer stops the daemon while it is `Installing`:
            // that is the update going through, not a failure.
            Ok(Some(Ok(Event::Daemon(DaemonEvent::ShuttingDown))) | None)
                if cfg!(windows) && matches!(last.state, UpdateState::Installing { .. }) =>
            {
                eprintln!("the installer is replacing PlayDrop and will start it again");
                break;
            }
            // Otherwise the daemon goes away on purpose only after `Restarting`, which ends
            // the loop below before this can be read.
            Ok(Some(Ok(Event::Daemon(DaemonEvent::ShuttingDown))) | None) => {
                return Err(Failure::new(
                    Exit::Failed,
                    "the daemon went away before the update finished; run `playdrop update` once it is back",
                ));
            }
            Ok(Some(_)) => continue,
            Err(_) => {
                return Err(Failure::new(
                    Exit::Failed,
                    "no word from the daemon for 30 minutes; run `playdrop update` to see where it stands",
                ))
            }
        };
        if status.state != last.state {
            eprintln!("{}", output::update_state_line(&status.state));
        }
        last = status;
        match &last.state {
            UpdateState::Restarting { .. } => break,
            UpdateState::Failed { message, .. } => {
                print(&last, json);
                return Err(Failure::new(Exit::Failed, message.clone()));
            }
            _ => {}
        }
    }
    if json {
        println!("{}", output::json(&last));
    }
    Ok(())
}

/// Ctrl-C: asks the daemon to stop the download. Ends the command once it did; when the
/// install is already with the system, says so and keeps following it.
async fn cancel(client: &Client) -> Result<(), Failure> {
    match client.request(Request::UpdateCancel).await {
        Ok(_) => Err(Failure::new(
            Exit::Failed,
            "update cancelled; the downloaded files were removed",
        )),
        Err(IpcError {
            code: ErrorCode::InvalidState,
            message,
            ..
        }) => {
            eprintln!("{message}");
            Ok(())
        }
        Err(e) => Err(Failure::from(e)),
    }
}

fn print(status: &UpdateStatus, json: bool) {
    if json {
        println!("{}", output::json(status));
    } else {
        print!("{}", output::update(status, output::unix_now()));
    }
}

/// One update request. A daemon older than 0.11.0 does not know them and answers
/// `bad_request`: say that plainly instead of "usage error".
async fn ask_update(client: &Client, request: Request) -> Result<Answer, Failure> {
    match client.request(request).await {
        Ok(answer) => Ok(answer),
        Err(IpcError {
            code: ErrorCode::BadRequest,
            ..
        }) => Err(Failure::new(
            Exit::Failed,
            "this PlayDrop daemon has no update check (it is older than 0.11.0)",
        )),
        Err(e) => Err(Failure::from(e)),
    }
}

fn status_of(answer: Answer) -> Result<UpdateStatus, Failure> {
    match answer {
        Answer::Update(s) => Ok(s),
        other => Err(unexpected("update", &other)),
    }
}

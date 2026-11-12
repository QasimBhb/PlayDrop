//! The daemon client commands: `send`, `peers`, `status`, `transfers`, `cancel`, `clip`,
//! `ping`, `outbox`, `diagnose` and `update`.
//!
//! Each one connects to the running `playdropd` through `playdrop-ipc` (D-Bus on Linux,
//! the per-user named pipe on Windows), asks one or two questions and prints the answer.
//! Nothing here is platform-specific: file descriptors, pipes and the daemon identity
//! checks all live in the IPC crate.
//!
//! Layout: [`peer`] resolves `--to`, [`output`] formats, [`commands`] holds the short
//! commands and [`send`] the one that follows a transfer to its end.

mod commands;
pub mod output;
pub mod peer;
mod send;
pub mod update;

use std::path::PathBuf;
use std::process::ExitCode;

use playdrop_ipc::client::{Client, ConnectError};
use playdrop_ipc::types::PeerSummary;
use playdrop_ipc::{ErrorCode, IpcError, Ok as Answer, Request};

/// How a daemon command ended, as the process exit code says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The operation failed, was declined or was cancelled.
    Failed = 1,
    /// Bad arguments, a missing path or no such peer.
    Usage = 2,
    /// The daemon is not running or cannot be reached.
    NoDaemon = 3,
}

/// A failed command: what to print on stderr and which exit code to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Exit code.
    pub exit: Exit,
    /// One or more lines for stderr.
    pub message: String,
}

impl Failure {
    /// A failure with `exit` and `message`.
    pub fn new(exit: Exit, message: impl Into<String>) -> Self {
        Self {
            exit,
            message: message.into(),
        }
    }
}

impl From<IpcError> for Failure {
    fn from(e: IpcError) -> Self {
        let exit = match e.code {
            ErrorCode::BadRequest
            | ErrorCode::NotFound
            | ErrorCode::Ambiguous
            | ErrorCode::TooLarge => Exit::Usage,
            _ => Exit::Failed,
        };
        Self::new(exit, format!("{}: {}", e.code, e.message))
    }
}

/// The one sentence that says how to start the daemon on this platform.
pub const NOT_RUNNING: &str = if cfg!(windows) {
    "the PlayDrop daemon is not running; start PlayDrop from its Start menu shortcut (or start the installed PlayDrop service) and try again"
} else {
    "the PlayDrop daemon is not running; start PlayDrop from the applications menu (or run `playdropd`) and try again"
};

impl From<ConnectError> for Failure {
    fn from(e: ConnectError) -> Self {
        match e {
            ConnectError::NotRunning => Self::new(Exit::NoDaemon, NOT_RUNNING),
            ConnectError::Transport(t) => Self::new(
                Exit::NoDaemon,
                format!("cannot reach the PlayDrop daemon ({t}); {NOT_RUNNING}"),
            ),
            ConnectError::Version { daemon, client } => Self::new(
                Exit::Failed,
                format!(
                    "the daemon speaks IPC version {daemon} and this playdrop speaks {client}; \
                     install matching versions"
                ),
            ),
            ConnectError::Unverified(why) => Self::new(
                Exit::Failed,
                format!("refusing to talk to an unverified daemon: {why}"),
            ),
        }
    }
}

/// `--json` alone.
#[derive(Debug, clap::Args)]
pub struct JsonArgs {
    /// Print the daemon's answer as one JSON object on stdout.
    #[arg(long)]
    pub json: bool,
}

/// Options of `playdrop send`.
#[derive(Debug, clap::Args)]
pub struct SendArgs {
    /// Files or folders to send.
    #[arg(required = true, value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    /// Peer: Short ID or its prefix (hyphens optional) or label. Default: the only online
    /// paired device.
    #[arg(long, value_name = "PEER")]
    pub to: Option<String>,
    /// Return once the transfer has started and print its ID.
    #[arg(long)]
    pub no_wait: bool,
    /// Print the final transfer snapshot (or, with --no-wait, the started ID) as one JSON
    /// object on stdout.
    #[arg(long)]
    pub json: bool,
}

/// Options of `playdrop clip`.
#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("source").args(["text", "stdin"])))]
pub struct ClipArgs {
    /// The text to send. Without it (and without --stdin) the daemon sends what is on this
    /// computer's clipboard now: text, or an image while clipboard images are on.
    pub text: Option<String>,
    /// Read the text from stdin instead (sent exactly as read, newlines included).
    #[arg(long)]
    pub stdin: bool,
    /// Peer: Short ID prefix or label. Default: the only online paired device.
    #[arg(long, value_name = "PEER")]
    pub to: Option<String>,
    /// Mark the clip sensitive (a password), so the other side does not show or keep it
    /// in history. The same when sending this computer's clipboard: the clip goes marked,
    /// and a clip a password manager marked is allowed to go.
    #[arg(long)]
    pub sensitive: bool,
}

/// Options of `playdrop remote`.
#[cfg(feature = "iroh")]
#[derive(Debug, clap::Args)]
pub struct RemoteArgs {
    /// Peer: Short ID prefix or label.
    #[arg(value_name = "PEER")]
    pub peer: String,
    /// `on` or `off`.
    #[arg(value_name = "on|off", value_parser = ["on", "off"])]
    pub state: String,
}

/// Options of `playdrop ping`.
#[derive(Debug, clap::Args)]
pub struct PingArgs {
    /// Peer: Short ID prefix or label. Default: the only online paired device.
    #[arg(long, value_name = "PEER")]
    pub to: Option<String>,
    /// Optional message shown on the other device (cut to 256 bytes).
    pub message: Option<String>,
}

/// Options of `playdrop cancel`.
#[derive(Debug, clap::Args)]
pub struct CancelArgs {
    /// Transfer ID as shown by `playdrop transfers`, or a unique start of it.
    #[arg(value_name = "ID")]
    pub id: String,
}

/// Options of `playdrop outbox`.
#[derive(Debug, clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct OutboxArgs {
    /// `add`; without it, list what the outbox holds.
    #[command(subcommand)]
    pub action: Option<OutboxAction>,
    /// Print the list as one JSON object on stdout.
    #[arg(long)]
    pub json: bool,
}

/// Subcommands of `playdrop outbox`.
#[derive(Debug, clap::Subcommand)]
pub enum OutboxAction {
    /// Queue files or folders for a device to pull when it next opens the app (an iPhone,
    /// which cannot receive in the background).
    Add(OutboxAddArgs),
}

/// Options of `playdrop outbox add`.
#[derive(Debug, clap::Args)]
pub struct OutboxAddArgs {
    /// Files or folders to queue.
    #[arg(required = true, value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    /// Peer the items are for: Short ID prefix or label. It may be offline.
    #[arg(long, value_name = "PEER", required = true)]
    pub to: String,
    /// Print the queued item ID as one JSON object on stdout.
    #[arg(long)]
    pub json: bool,
}

/// A command that talks to the daemon.
#[derive(Debug)]
pub enum DaemonCommand {
    /// `playdrop send`.
    Send(SendArgs),
    /// `playdrop peers`.
    Peers(JsonArgs),
    /// `playdrop remote`.
    #[cfg(feature = "iroh")]
    Remote(RemoteArgs),
    /// `playdrop status`.
    Status(JsonArgs),
    /// `playdrop transfers`.
    Transfers(JsonArgs),
    /// `playdrop cancel`.
    Cancel(CancelArgs),
    /// `playdrop clip`.
    Clip(ClipArgs),
    /// `playdrop ping`.
    Ping(PingArgs),
    /// `playdrop outbox`.
    Outbox(OutboxArgs),
    /// `playdrop diagnose`.
    Diagnose(JsonArgs),
    /// `playdrop update`.
    Update(update::UpdateArgs),
}

/// Runs one daemon command and turns its outcome into the exit code.
pub async fn run(cmd: DaemonCommand) -> ExitCode {
    let result = match cmd {
        DaemonCommand::Send(a) => send::run(a).await,
        DaemonCommand::Peers(a) => commands::peers(a).await,
        #[cfg(feature = "iroh")]
        DaemonCommand::Remote(a) => commands::remote(a).await,
        DaemonCommand::Status(a) => commands::status(a).await,
        DaemonCommand::Transfers(a) => commands::transfers(a).await,
        DaemonCommand::Cancel(a) => commands::cancel(a).await,
        DaemonCommand::Clip(a) => commands::clip(a).await,
        DaemonCommand::Ping(a) => commands::ping(a).await,
        DaemonCommand::Outbox(a) => commands::outbox(a).await,
        DaemonCommand::Diagnose(a) => commands::diagnose(a).await,
        DaemonCommand::Update(a) => update::run(a).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => {
            eprintln!("playdrop: {}", f.message.trim_end());
            ExitCode::from(f.exit as u8)
        }
    }
}

/// Connects to the daemon, verifying it (SR-26, SR-27).
async fn connect() -> Result<Client, Failure> {
    Client::connect().await.map_err(Failure::from)
}

/// One request, one answer.
async fn ask(client: &Client, request: Request) -> Result<Answer, Failure> {
    client.request(request).await.map_err(Failure::from)
}

/// The answer was not the kind this request gets.
fn unexpected(what: &str, answer: &Answer) -> Failure {
    Failure::new(
        Exit::Failed,
        format!(
            "the daemon answered {what} with {}; is it a different PlayDrop version?",
            output::json(answer)
        ),
    )
}

/// The paired devices.
async fn peers(client: &Client) -> Result<Vec<PeerSummary>, Failure> {
    match ask(client, Request::Peers).await? {
        Answer::Peers(p) => Ok(p),
        other => Err(unexpected("peers", &other)),
    }
}

/// Resolves `--to` against the daemon's peer list, or fails with exit code 2 and the list.
fn pick_peer<'a>(peers: &'a [PeerSummary], to: Option<&str>) -> Result<&'a PeerSummary, Failure> {
    peer::resolve(peers, to).map_err(|e| {
        let mut message = e.to_string();
        if !peers.is_empty() {
            message.push_str("\npaired devices:\n");
            message.push_str(&output::peer_choices(peers));
        }
        Failure::new(Exit::Usage, message)
    })
}

/// What a Flatpak user must type for the sandboxed `playdrop` to see a file outside the
/// folders the sandbox shares (packaging/flatpak/README.md, "The command line").
const FLATPAK_FORWARDING_HINT: &str = "playdrop runs inside the Flatpak sandbox and sees only \
     the files handed to it; put the paths between @@ marks and add --file-forwarding: \
     flatpak run --file-forwarding --command=playdrop org.playdrop.PlayDrop send @@ <PATH>... @@";

/// Whether this process runs inside a Flatpak sandbox, which marks itself with this file.
fn in_flatpak() -> bool {
    cfg!(target_os = "linux") && std::path::Path::new("/.flatpak-info").exists()
}

/// Makes every path absolute before it leaves this process, and fails early with exit
/// code 2 on one that does not exist. On Windows the daemon receives the paths as they are
/// and runs in another folder, so a relative path would name the wrong file there.
fn absolute_paths(paths: &[PathBuf]) -> Result<Vec<PathBuf>, Failure> {
    absolute_paths_in(paths, in_flatpak())
}

/// [`absolute_paths`] with the sandbox answer passed in. Inside Flatpak a file that exists
/// on the host is simply absent here, so "not found" gets the sentence that says how to
/// hand the file over instead of leaving the user to guess.
fn absolute_paths_in(paths: &[PathBuf], sandboxed: bool) -> Result<Vec<PathBuf>, Failure> {
    paths
        .iter()
        .map(|p| {
            let abs = std::path::absolute(p)
                .map_err(|e| Failure::new(Exit::Usage, format!("{}: {e}", p.display())))?;
            match std::fs::metadata(&abs) {
                Ok(m) if m.is_file() || m.is_dir() => Ok(abs),
                Ok(_) => Err(Failure::new(
                    Exit::Usage,
                    format!("{} is not a regular file or folder", p.display()),
                )),
                Err(e) if sandboxed && e.kind() == std::io::ErrorKind::NotFound => {
                    Err(Failure::new(
                        Exit::Usage,
                        format!("{}: {e}\n{FLATPAK_FORWARDING_HINT}", p.display()),
                    ))
                }
                Err(e) => Err(Failure::new(Exit::Usage, format!("{}: {e}", p.display()))),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_errors_map_to_exit_codes() {
        let f = Failure::from(IpcError::new(ErrorCode::NotFound, "no such peer"));
        assert_eq!(f.exit, Exit::Usage);
        assert_eq!(f.message, "not_found: no such peer");
        assert_eq!(
            Failure::from(IpcError::new(ErrorCode::Offline, "x")).exit,
            Exit::Failed
        );
        assert_eq!(
            Failure::from(IpcError::new(ErrorCode::Internal, "x")).exit,
            Exit::Failed
        );
        assert_eq!(Failure::from(ConnectError::NotRunning).exit, Exit::NoDaemon);
        assert_eq!(
            Failure::from(ConnectError::Transport("no bus".into())).exit,
            Exit::NoDaemon
        );
    }

    #[test]
    fn not_running_is_one_sentence() {
        assert_eq!(NOT_RUNNING.matches(". ").count(), 0);
        assert!(!NOT_RUNNING.contains('\n'));
    }

    #[test]
    fn missing_peer_lists_the_choices() {
        let peers = [peer::tests::peer("pgry-2xha", "bravo", false)];
        let f = pick_peer(&peers, Some("nope")).expect_err("no match");
        assert_eq!(f.exit, Exit::Usage);
        assert!(f.message.contains("no paired device matches \"nope\""));
        assert!(f.message.contains("pgry-2xha  bravo  offline"));
    }

    #[test]
    fn paths_become_absolute_and_must_exist() {
        let cwd = std::env::current_dir().expect("cwd");
        let got = absolute_paths(&[PathBuf::from("Cargo.toml"), PathBuf::from("src")])
            .expect("both exist");
        assert_eq!(got, vec![cwd.join("Cargo.toml"), cwd.join("src")]);
        let f =
            absolute_paths_in(&[PathBuf::from("no-such-file-here")], false).expect_err("missing");
        assert_eq!(f.exit, Exit::Usage);
        assert!(!f.message.contains("--file-forwarding"));
    }

    #[test]
    fn a_missing_path_inside_flatpak_says_how_to_hand_the_file_over() {
        let f =
            absolute_paths_in(&[PathBuf::from("no-such-file-here")], true).expect_err("missing");
        assert_eq!(f.exit, Exit::Usage);
        assert!(f.message.starts_with("no-such-file-here: "));
        assert!(f.message.contains(
            "flatpak run --file-forwarding --command=playdrop org.playdrop.PlayDrop send @@"
        ));
        // A path that is there needs no hint, sandbox or not.
        assert!(absolute_paths_in(&[PathBuf::from("Cargo.toml")], true).is_ok());
    }
}

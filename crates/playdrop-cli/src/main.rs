//! `playdrop`: the `PlayDrop` command line.
//!
//! Most commands are clients of the running desktop daemon (`playdropd`), so scripts and
//! the shell can send files and drive it without the window: `send`, `peers`, `status`,
//! `transfers`, `clip`, `ping`, `outbox` and `diagnose` (module [`daemon`]). They talk to
//! the daemon through `playdrop-ipc` (D-Bus on Linux, the per-user named pipe on
//! Windows) and exit 0 on success, 1 when the operation failed, 2 on a usage error or an
//! unknown peer, 3 when the daemon is not running.
//!
//! Two commands run without a daemon. `playdrop host` runs a full host (the runtime the
//! daemon embeds) in the foreground, prints its events, and takes commands on stdin, so
//! two hosts in two terminals can pair and send to each other. `playdrop bench` measures
//! HTTP/2 against raw framing on one TLS connection (PLAN.md Phase 1 benchmark).

mod bench;
mod daemon;
mod repl;

use std::net::{IpAddr, Ipv6Addr};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// playdrop: phone-to-computer companion for the local network.
///
/// send, peers, status, transfers, clip, ping, outbox and diagnose talk to the running
/// daemon (playdropd). host runs a standalone host in the foreground (no daemon needed) and
/// bench measures the transport.
///
/// Exit codes: 0 ok, 1 the operation failed (or was declined or cancelled), 2 usage error
/// or no such peer, 3 the daemon is not running.
///
/// A peer is a Short ID (k3qd-7f2m-...) or its prefix, hyphens optional, or the label
/// shown by `playdrop peers`. Without --to, the only online paired device is used.
#[derive(Debug, Parser)]
#[command(name = "playdrop", version, about, verbatim_doc_comment)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Send files or folders and follow the transfer to its end.
    ///
    /// Progress goes to stderr (one line redrawn in place on a terminal, one line per
    /// second otherwise). Exits 0 when every file arrived, 1 when the transfer failed or
    /// was declined or cancelled (the reason goes to stderr). Ctrl-C cancels the transfer.
    ///
    ///   playdrop send photo.jpg
    ///   playdrop send ~/Pictures/trip --to phone
    ///   playdrop send big.iso --to k3qd --json     (final transfer snapshot as JSON)
    ///   playdrop send notes.txt --no-wait           (print the transfer ID and return)
    #[command(verbatim_doc_comment)]
    Send(daemon::SendArgs),
    /// List paired devices: Short ID, label, online state, last seen, last address.
    Peers(daemon::JsonArgs),
    /// Show the daemon's identity, address, version and settings.
    Status(daemon::JsonArgs),
    /// List running transfers and those that finished in the last 10 minutes.
    Transfers(daemon::JsonArgs),
    /// Cancel a transfer, or forget a failed send that is waiting to resume.
    ///
    /// ID is the first column of `playdrop transfers`; a unique start of it is enough.
    /// A send shown as `failed (resumes)` stops resuming and the receiver is told.
    /// Exits 0 when cancelled or forgotten, 1 when the daemon could not, 2 for an unknown
    /// or ambiguous ID, 3 when the daemon is not running.
    ///
    ///   playdrop cancel 7f3a
    #[command(verbatim_doc_comment)]
    Cancel(daemon::CancelArgs),
    /// Send text to a device's clipboard
    ///
    ///   playdrop clip "see you at 6"
    ///   echo -n secret | playdrop clip --stdin --sensitive --to phone
    #[command(verbatim_doc_comment)]
    Clip(daemon::ClipArgs),
    /// Ping a device, with an optional message it shows.
    Ping(daemon::PingArgs),
    /// List the outbox, or queue files for a device with `outbox add`.
    ///
    /// Queued files wait until the device pulls them, the next time it opens the app (an
    /// iPhone cannot receive in the background).
    ///
    ///   playdrop outbox
    ///   playdrop outbox add report.pdf --to iphone
    #[command(verbatim_doc_comment)]
    Outbox(daemon::OutboxArgs),
    /// Check the network and name the layer that fails.
    ///
    /// Covers interfaces, multicast, mDNS, the port probe and each device's last failed
    /// dial. The first lines are the findings, worded for the user.
    Diagnose(daemon::JsonArgs),
    /// Run a host in the foreground until Ctrl-C or `quit`, printing events as lines.
    ///
    /// Type commands on stdin while it runs:
    ///
    ///   peers                         list paired devices
    ///   pair                          open "Add device" for 120 s (the other side pairs)
    ///   pair <ip:port>                pair with that device (SAS shown on both sides)
    ///   pair --qr                     open "Add device" and print a QR pairing URI
    ///   pair --scan <uri>             pair using a QR URI from the other device
    ///   list                          devices seen in "Add device" mode
    ///   y | n                         answer the pairing prompt
    ///   send <peer> <path>...         send files or folders (peer: short ID or label)
    ///   accept <id> | decline <id>    answer an incoming offer
    ///   cancel <id>                   cancel a transfer in either direction
    ///   transfers                     list running and recent transfers
    ///   grants <peer> [+g|-g]...      show or change grants (files, clipboardIn,
    ///                                 clipboardOut, input, media, find)
    ///   ping <peer> [message]         ping a peer
    ///   clip <peer> <text>            push clipboard text
    ///   unpair <peer>                 unpair a peer
    ///   quit                          stop the host
    ///
    /// Two hosts on one machine need different --data-dir, --receive-dir and --port.
    #[command(verbatim_doc_comment)]
    Host(HostArgs),
    /// Benchmark tuned HTTP/2 against raw framing over one TLS connection.
    ///
    /// Run a server, then a client against it, once per mode:
    ///   playdrop bench --role server --addr 127.0.0.1:42200 --mode h2
    ///   playdrop bench --role client --addr 127.0.0.1:42200 --mode h2 --size 1GiB --runs 3
    #[command(verbatim_doc_comment)]
    Bench(bench::BenchArgs),
}

/// Options of `playdrop host`.
#[derive(Debug, clap::Args)]
pub struct HostArgs {
    /// Identity, pins and transfer state (default: the platform data folder).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Where received files go (default: ~/Downloads/PlayDrop).
    #[arg(long)]
    receive_dir: Option<PathBuf>,
    /// TCP port (protocol default 42187).
    #[arg(long, default_value_t = playdrop_core::constants::TCP_PORT)]
    port: u16,
    /// Name shown to other devices (default: the host name).
    #[arg(long)]
    name: Option<String>,
    /// Use the mobile keep-alive (300 s) instead of the desktop one (60 s).
    #[arg(long)]
    mobile: bool,
    /// Print events as JSON lines.
    #[arg(long)]
    json: bool,
    /// Address to bind (default: all interfaces, IPv4 and IPv6).
    #[arg(long, default_value_t = IpAddr::V6(Ipv6Addr::UNSPECIFIED))]
    bind: IpAddr,
    /// Turn off mDNS and the beacon; rely on last-known addresses and manual pairing.
    #[arg(long)]
    no_discovery: bool,
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("playdrop_core=warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() -> ExitCode {
    init_logging();
    let cli = Cli::parse();
    let daemon_command = match cli.command {
        Command::Host(args) => return standalone(repl::run(args).await),
        Command::Bench(args) => return standalone(bench::run(args).await),
        Command::Send(a) => daemon::DaemonCommand::Send(a),
        Command::Peers(a) => daemon::DaemonCommand::Peers(a),
        Command::Status(a) => daemon::DaemonCommand::Status(a),
        Command::Transfers(a) => daemon::DaemonCommand::Transfers(a),
        Command::Cancel(a) => daemon::DaemonCommand::Cancel(a),
        Command::Clip(a) => daemon::DaemonCommand::Clip(a),
        Command::Ping(a) => daemon::DaemonCommand::Ping(a),
        Command::Outbox(a) => daemon::DaemonCommand::Outbox(a),
        Command::Diagnose(a) => daemon::DaemonCommand::Diagnose(a),
    };
    daemon::run(daemon_command).await
}

/// Exit code of `host` and `bench`: 0, or 1 with the error chain on stderr.
fn standalone(result: anyhow::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:?}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    fn parse(args: &[&str]) -> Result<Command, clap::Error> {
        Cli::try_parse_from(std::iter::once("playdrop").chain(args.iter().copied()))
            .map(|c| c.command)
    }

    #[test]
    fn clap_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn send_takes_paths_and_options() {
        let Ok(Command::Send(a)) = parse(&["send", "a.txt", "dir", "--to", "k3qd", "--json"])
        else {
            panic!("send did not parse");
        };
        assert_eq!(a.paths, vec![PathBuf::from("a.txt"), PathBuf::from("dir")]);
        assert_eq!(a.to.as_deref(), Some("k3qd"));
        assert!(a.json && !a.no_wait);
        let Ok(Command::Send(a)) = parse(&["send", "--no-wait", "a.txt"]) else {
            panic!("send --no-wait did not parse");
        };
        assert!(a.no_wait && a.to.is_none());
        assert!(parse(&["send"]).is_err(), "a path is required");
    }

    #[test]
    fn clip_needs_text_or_stdin_but_not_both() {
        assert!(matches!(parse(&["clip", "hi"]), Ok(Command::Clip(_))));
        assert!(matches!(
            parse(&["clip", "--stdin", "--to", "x"]),
            Ok(Command::Clip(_))
        ));
        assert!(parse(&["clip"]).is_err());
        assert!(parse(&["clip", "hi", "--stdin"]).is_err());
    }

    #[test]
    fn ping_message_is_optional() {
        let Ok(Command::Ping(a)) = parse(&["ping", "--to", "bravo", "hello there"]) else {
            panic!("ping did not parse");
        };
        assert_eq!(a.message.as_deref(), Some("hello there"));
        assert!(matches!(parse(&["ping"]), Ok(Command::Ping(_))));
    }

    #[test]
    fn outbox_lists_or_adds() {
        let Ok(Command::Outbox(a)) = parse(&["outbox", "--json"]) else {
            panic!("outbox did not parse");
        };
        assert!(a.json && a.action.is_none());
        let Ok(Command::Outbox(a)) = parse(&["outbox", "add", "f", "--to", "iphone"]) else {
            panic!("outbox add did not parse");
        };
        let Some(daemon::OutboxAction::Add(add)) = a.action else {
            panic!("no add");
        };
        assert_eq!(add.to, "iphone");
        assert!(parse(&["outbox", "add", "f"]).is_err(), "--to is required");
    }

    #[test]
    fn listing_commands_take_json() {
        for cmd in ["peers", "status", "transfers", "diagnose"] {
            assert!(parse(&[cmd]).is_ok(), "{cmd}");
            assert!(parse(&[cmd, "--json"]).is_ok(), "{cmd} --json");
            assert!(parse(&[cmd, "extra"]).is_err(), "{cmd} extra");
        }
    }
}

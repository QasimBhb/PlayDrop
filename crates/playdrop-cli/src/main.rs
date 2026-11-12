//! `playdrop`: the `PlayDrop` command line.
//!
//! Most commands are clients of the running desktop daemon (`playdropd`), so scripts and
//! the shell can send files and drive it without the window: `send`, `peers`, `status`,
//! `transfers`, `clip`, `ping`, `outbox`, `diagnose` and `update` (module [`daemon`]). They talk to
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
/// send, peers, status, transfers, clip, ping, outbox, diagnose and update talk to the
/// running daemon (playdropd). host runs a standalone host in the foreground (no daemon needed) and
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
    /// Give a paired device the `remote` grant, or take it back: reach across networks through
    /// the relay the daemon was started with (`--reach-relay`).
    #[cfg(feature = "iroh")]
    Remote(daemon::RemoteArgs),
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
    /// Send text, or this computer's clipboard, to a device's clipboard
    ///
    ///   playdrop clip "see you at 6"
    ///   echo -n secret | playdrop clip --stdin --sensitive --to phone
    ///   playdrop clip --to phone        (what is on the clipboard now)
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
    /// Show whether a newer `PlayDrop` exists, check now, or install it.
    ///
    /// Without options it shows what the daemon knows and asks no one. --check asks GitHub
    /// now (the same anonymous question as "Check now" in Settings; nothing about this
    /// computer is sent). --install downloads the offered version, checks its signature
    /// and installs it; the system password prompt appears, and the daemon restarts on the
    /// new version. Exits 1 when the check or the update failed, or checks are off.
    ///
    ///   playdrop update
    ///   playdrop update --check
    ///   playdrop update --install
    #[command(verbatim_doc_comment)]
    Update(daemon::update::UpdateArgs),
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
    ///                                 clipboardOut, input, media, find, notificationsIn,
    ///                                 notificationsOut; remote in a build with reach across
    ///                                 networks)
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent command line switches"
)]
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
    /// Reach paired devices across networks through this relay, for those you give the `remote`
    /// grant (`grants <peer> +remote`); give it on both devices, while they can see each other
    /// on a LAN. The relay can see which devices connect, when, and how much data moves, though
    /// not the contents. The device's endpoint ID stays the same until its key changes.
    #[cfg(feature = "iroh")]
    #[arg(long, value_name = "URL")]
    reach_relay: Option<String>,
    /// With --reach-relay: also allow a direct path across networks. Faster, but anyone who has
    /// this device's endpoint ID, paired or not, can then make it send small UDP packets to an
    /// address they name, which also tells them this device's address.
    #[cfg(feature = "iroh")]
    #[arg(long, requires = "reach_relay")]
    reach_direct: bool,
    /// With --reach-relay: PEM file of a private CA to trust for the relay's certificate.
    #[cfg(feature = "iroh")]
    #[arg(long, value_name = "FILE", requires = "reach_relay")]
    reach_relay_ca: Option<PathBuf>,
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("playdrop_core=warn"));
    // SR-40: iroh names the relay at info level.
    #[cfg(feature = "iroh")]
    let filter = tracing_subscriber::EnvFilter::new(playdrop_core::reach::quiet_log_filter(
        &filter.to_string(),
    ));
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
        #[cfg(feature = "iroh")]
        Command::Remote(a) => daemon::DaemonCommand::Remote(a),
        Command::Status(a) => daemon::DaemonCommand::Status(a),
        Command::Transfers(a) => daemon::DaemonCommand::Transfers(a),
        Command::Cancel(a) => daemon::DaemonCommand::Cancel(a),
        Command::Clip(a) => daemon::DaemonCommand::Clip(a),
        Command::Ping(a) => daemon::DaemonCommand::Ping(a),
        Command::Outbox(a) => daemon::DaemonCommand::Outbox(a),
        Command::Diagnose(a) => daemon::DaemonCommand::Diagnose(a),
        Command::Update(a) => daemon::DaemonCommand::Update(a),
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

    /// SR-39: the help text of `--reach-relay` says plainly what the relay can see.
    #[cfg(feature = "iroh")]
    #[test]
    fn reach_relay_help_says_what_the_relay_sees() {
        let mut command = Cli::command();
        let host = command.find_subcommand_mut("host").unwrap();
        let help = host.render_long_help().to_string();
        assert!(help.contains("--reach-relay"), "{help}");
        assert!(help.contains("relay can see which devices"), "{help}");
        // Direct is asked for by name, and its help says what it exposes (T45).
        assert!(help.contains("--reach-direct"), "{help}");
        assert!(help.contains("small UDP packets"), "{help}");
        assert!(!help.contains("--reach-relay-only"), "{help}");
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
    fn clip_takes_text_or_stdin_or_neither_but_not_both() {
        assert!(matches!(parse(&["clip", "hi"]), Ok(Command::Clip(_))));
        assert!(matches!(
            parse(&["clip", "--stdin", "--to", "x"]),
            Ok(Command::Clip(_))
        ));
        // Neither: send this computer's clipboard.
        let Ok(Command::Clip(a)) = parse(&["clip", "--sensitive"]) else {
            panic!("clip with no text did not parse");
        };
        assert!(a.text.is_none() && !a.stdin && a.sensitive);
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
    fn update_checks_or_installs_but_not_both() {
        let Ok(Command::Update(a)) = parse(&["update"]) else {
            panic!("update did not parse");
        };
        assert!(!a.check && !a.install && !a.json);
        let Ok(Command::Update(a)) = parse(&["update", "--check", "--json"]) else {
            panic!("update --check did not parse");
        };
        assert!(a.check && a.json);
        assert!(matches!(
            parse(&["update", "--install"]),
            Ok(Command::Update(_))
        ));
        assert!(parse(&["update", "--check", "--install"]).is_err());
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

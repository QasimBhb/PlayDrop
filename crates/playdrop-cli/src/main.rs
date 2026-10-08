//! `playdrop`: the Phase 1 command line.
//!
//! `playdrop host` runs a full host (the same runtime the desktop daemon will embed) in the
//! foreground, prints its events, and takes commands on stdin, so two hosts in two
//! terminals can pair and send to each other. `playdrop bench` measures HTTP/2 against raw
//! framing on one TLS connection (PLAN.md Phase 1 benchmark).

mod bench;
mod repl;

use std::net::{IpAddr, Ipv6Addr};
use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// `PlayDrop`: phone-to-computer companion for the local network.
#[derive(Debug, Parser)]
#[command(name = "playdrop", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
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
async fn main() -> anyhow::Result<()> {
    init_logging();
    let cli = Cli::parse();
    match cli.command {
        Command::Host(args) => repl::run(args).await,
        Command::Bench(args) => bench::run(args).await,
    }
}

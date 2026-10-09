//! Command line flags. Every flag overrides the persisted setting of the same name for this
//! run only; `--write-settings` makes the override stick.

use std::path::PathBuf;

use clap::Parser;

/// `PlayDrop` desktop daemon.
// Each `--no-*` flag is an independent switch, so a struct of bools is the honest shape.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Parser)]
#[command(name = "playdropd", version, about)]
pub struct Args {
    /// Identity, pins and settings directory (default: the platform data dir).
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    /// Folder received files land in (default: ~/Downloads/PlayDrop).
    #[arg(long)]
    pub receive_dir: Option<PathBuf>,
    /// TCP and UDP port (default: 42187).
    #[arg(long)]
    pub port: Option<u16>,
    /// Advertised device name (default: the hostname).
    #[arg(long)]
    pub name: Option<String>,
    /// Do not show a tray icon.
    #[arg(long)]
    pub no_tray: bool,
    /// Do not watch the clipboard.
    #[arg(long)]
    pub no_clipboard: bool,
    /// Do not show desktop notifications.
    #[arg(long)]
    pub no_notifications: bool,
    /// Turn mDNS and the beacon off (last-known dial only).
    #[arg(long)]
    pub no_discovery: bool,
    /// Never write the XDG autostart entry or the D-Bus activation file (test runs and
    /// packaged installs that ship their own).
    #[arg(long)]
    pub no_autostart: bool,
    /// Persist the flags given on this command line into settings.json.
    #[arg(long)]
    pub write_settings: bool,
    /// Log filter (also `RUST_LOG`). Default: `playdropd=info,playdrop_core=warn`.
    #[arg(long)]
    pub log: Option<String>,
}

/// Installs the tracing subscriber. Logs never carry secrets (SR-32): the core redacts them
/// and the daemon logs file names at debug only.
pub fn init_logging(args: &Args) {
    let filter = args
        .log
        .clone()
        .or_else(|| std::env::var("RUST_LOG").ok())
        .unwrap_or_else(|| "playdropd=info,playdrop_core=warn".to_owned());
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_target(false)
        .init();
}

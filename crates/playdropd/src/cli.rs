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
    /// Reach paired devices across networks through this relay, for those you give the `remote`
    /// grant (`playdrop remote <peer> on`); give it on both devices, while they can see each
    /// other on a LAN. Never saved, so name it at every start. The relay can see which devices
    /// connect, when, and how much data moves, though not the contents. The device's endpoint ID
    /// stays the same until its key changes.
    #[cfg(feature = "iroh")]
    #[arg(long, value_name = "URL")]
    pub reach_relay: Option<String>,
    /// With --reach-relay: also allow a direct path across networks. Faster, but anyone who has
    /// this device's endpoint ID, paired or not, can then make it send small UDP packets to an
    /// address they name, which also tells them this device's address.
    #[cfg(feature = "iroh")]
    #[arg(long, requires = "reach_relay")]
    pub reach_direct: bool,
    /// With --reach-relay: PEM file of a private CA to trust for the relay's certificate.
    #[cfg(feature = "iroh")]
    #[arg(long, value_name = "FILE", requires = "reach_relay")]
    pub reach_relay_ca: Option<PathBuf>,
    /// Persist the flags given on this command line into settings.json.
    #[arg(long)]
    pub write_settings: bool,
    /// Log filter (also `RUST_LOG`). Default: `playdropd=info,playdrop_core=warn`.
    #[arg(long)]
    pub log: Option<String>,
}

#[cfg(feature = "iroh")]
impl Args {
    /// The reach settings these flags ask for.
    pub fn reach_config(&self) -> anyhow::Result<Option<playdrop_core::reach::ReachConfig>> {
        let Some(url) = &self.reach_relay else {
            return Ok(None);
        };
        let mut c = playdrop_core::reach::ReachConfig::new(url.clone());
        c.direct = self.reach_direct;
        if let Some(path) = &self.reach_relay_ca {
            c.add_ca_pem(&std::fs::read_to_string(path)?)?;
        }
        Ok(Some(c))
    }
}

/// Installs the tracing subscriber. Logs never carry secrets (SR-32): the core redacts them
/// and the daemon logs file names at debug only.
pub fn init_logging(args: &Args) {
    let filter = args
        .log
        .clone()
        .or_else(|| std::env::var("RUST_LOG").ok())
        .unwrap_or_else(|| "playdropd=info,playdrop_core=warn".to_owned());
    // SR-40: iroh names the relay at info level.
    #[cfg(feature = "iroh")]
    let filter = playdrop_core::reach::quiet_log_filter(&filter);
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_target(false)
        .init();
}

#[cfg(all(test, feature = "iroh"))]
mod reach_tests {
    use super::*;
    use clap::CommandFactory as _;

    /// SR-39: the help text of `--reach-relay` says plainly what the relay can see.
    #[test]
    fn reach_relay_help_says_what_the_relay_sees() {
        let help = Args::command().render_long_help().to_string();
        assert!(help.contains("--reach-relay"), "{help}");
        assert!(help.contains("relay can see which devices"), "{help}");
        // Direct is asked for by name, and its help says what it exposes (T45).
        assert!(help.contains("--reach-direct"), "{help}");
        assert!(help.contains("small UDP packets"), "{help}");
        assert!(!help.contains("--reach-relay-only"), "{help}");
    }

    #[test]
    fn no_flag_means_no_reach_config() {
        let a = Args::parse_from(["playdropd"]);
        assert!(a.reach_config().unwrap().is_none());
        let a = Args::parse_from(["playdropd", "--reach-relay", "https://relay.example.org"]);
        let c = a.reach_config().unwrap().unwrap();
        assert!(!c.direct, "relay only unless asked");
        let a = Args::parse_from([
            "playdropd",
            "--reach-relay",
            "https://relay.example.org",
            "--reach-direct",
        ]);
        assert!(a.reach_config().unwrap().unwrap().direct);
        assert!(Args::try_parse_from(["playdropd", "--reach-direct"]).is_err());
    }
}

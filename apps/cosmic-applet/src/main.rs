//! `PlayDrop` panel applet for COSMIC: a thin client of `playdropd` over D-Bus.
//!
//! The panel starts it from `org.playdrop.PlayDrop.CosmicApplet.desktop`. It shows the daemon's
//! state, the paired devices with one-click "Send clipboard" and "Send files...", live
//! transfers, and the remote input kill switch. It never opens a network socket: everything
//! goes through `playdrop-ipc` (`docs/ipc.md`).
//!
//! Logs: `RUST_LOG=playdrop_cosmic_applet=debug`. Nothing at info level names a file, a
//! clipboard text or a full Device ID (SR-32).

mod app;
mod clipboard;
mod daemon;
mod launch;
mod model;

fn main() -> cosmic::iced::Result {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,playdrop_cosmic_applet=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting the PlayDrop applet"
    );
    cosmic::applet::run::<app::Applet>(())
}

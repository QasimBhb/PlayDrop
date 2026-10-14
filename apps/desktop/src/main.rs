//! `playdrop-desktop`: the Slint window. A thin client of `playdropd` over `playdrop_ipc`;
//! it never binds a socket (anti-pattern 3).
//!
//! Threads: Slint runs on the main thread; a small tokio runtime runs every IPC call and
//! the file pickers in the background and posts results back to the Slint event loop
//! (`ipc.rs`). View state lives on the Slint thread (`screens/`), and the conversion from
//! IPC types to Slint rows is pure, tested code (`model.rs`).
//!
//! Screens (PLAN.md Phase 2, `docs/phases/phase-1-core.md` section 9 task d): devices
//! with one-click send, add device with the candidate list and QR (SR-6, SR-9), the SAS
//! prompt with no default-focused button (SR-8), transfers, settings, diagnostics.

mod daemon;
mod icon;
mod ipc;
mod model;
mod qr;
mod screens;

/// The code `slint-build` generates from `ui/`. Generated items carry no docs and follow
/// Slint's own style, so the crate lints are relaxed for this module only.
#[allow(missing_docs, clippy::all, clippy::pedantic)]
mod ui {
    slint::include_modules!();
}

use slint::ComponentHandle;
use ui::{
    App, CandidateRow, ConnPhase, ConnState, DeviceRow, DevicesState, DiagSection,
    DiagnosticsState, HelpFocus, HelpState, InputState, MediaRow, Nav, NoticeState, PairingState,
    SettingsState, TransferRow, TransfersState,
};

/// Wayland app ID and X11 class; matches `StartupWMClass` in the packaged `.desktop` file.
const APP_ID: &str = "org.playdrop.PlayDrop";

/// Tab names accepted by `--page`, in tab order. The tray's "Add device" action opens the
/// window on its second tab with `--page add-device`.
const PAGES: [&str; 5] = [
    "devices",
    "add-device",
    "transfers",
    "settings",
    "diagnostics",
];

/// Parses `--page <name>` (or `--page=<name>`) from the command line. Unknown names are
/// reported and ignored, so a stale launcher never blocks the window.
fn page_from_args() -> Option<i32> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let name = if let Some(rest) = arg.strip_prefix("--page=") {
            rest.to_owned()
        } else if arg == "--page" {
            args.next()?
        } else {
            continue;
        };
        let index = PAGES.iter().position(|p| *p == name);
        if index.is_none() {
            tracing::warn!(
                "unknown --page {name}; expected one of {}",
                PAGES.join(", ")
            );
        }
        return index.and_then(|i| i32::try_from(i).ok());
    }
    None
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("playdrop_desktop=info")),
        )
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("playdrop-ipc")
        .enable_all()
        .build()?;

    let app = App::new()?;
    if let Err(e) = slint::set_xdg_app_id(APP_ID) {
        tracing::warn!("could not set the app ID: {e}");
    }
    app.set_app_icon(icon::image());
    if let Some(page) = page_from_args() {
        app.global::<Nav>().set_page(page);
    }

    let ipc = ipc::Ipc::new(rt.handle().clone(), screens::deliver);
    screens::install(&app, ipc);
    install_snapshot(&app);

    app.run()?;

    screens::uninstall();
    rt.shutdown_timeout(std::time::Duration::from_secs(1));
    Ok(())
}

/// Development aid: with `PLAYDROP_SNAPSHOT=<file.png>` set, the window renders itself to
/// that file about two seconds after start and then exits. Lets a headless check (or a
/// compositor that keeps the window behind a terminal) see every `--page` without input
/// tools. Never set in packaged launchers.
fn install_snapshot(app: &App) {
    let Some(path) = std::env::var_os("PLAYDROP_SNAPSHOT") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let weak = app.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(2500), move || {
        let Some(app) = weak.upgrade() else { return };
        match app.window().take_snapshot() {
            Result::Ok(buf) => match snapshot::write_png(&path, &buf) {
                Result::Ok(()) => tracing::info!(path = %path.display(), "snapshot written"),
                Result::Err(e) => tracing::error!("snapshot not written: {e}"),
            },
            Result::Err(e) => tracing::error!("snapshot failed: {e}"),
        }
        let _ = slint::quit_event_loop();
    });
}

mod snapshot {
    //! Minimal PNG writer (stored deflate blocks, no compression) so the snapshot aid needs
    //! no image crate.

    use std::io::Write;
    use std::path::Path;

    use slint::{Rgba8Pixel, SharedPixelBuffer};

    pub fn write_png(path: &Path, buf: &SharedPixelBuffer<Rgba8Pixel>) -> std::io::Result<()> {
        let (w, h) = (buf.width(), buf.height());
        let bytes = buf.as_bytes();
        let stride = w as usize * 4;
        let mut raw = Vec::with_capacity((stride + 1) * h as usize);
        for row in bytes.chunks_exact(stride) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, *b"IHDR", &ihdr);
        chunk(&mut out, *b"IDAT", &zlib_stored(&raw));
        chunk(&mut out, *b"IEND", &[]);
        std::fs::File::create(path)?.write_all(&out)
    }

    fn chunk(out: &mut Vec<u8>, tag: [u8; 4], data: &[u8]) {
        out.extend_from_slice(&(u32::try_from(data.len()).unwrap_or(u32::MAX)).to_be_bytes());
        out.extend_from_slice(&tag);
        out.extend_from_slice(data);
        let mut crc = Crc::new();
        crc.update(&tag);
        crc.update(data);
        out.extend_from_slice(&crc.finish().to_be_bytes());
    }

    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        let mut blocks = data.chunks(65_535).peekable();
        if blocks.peek().is_none() {
            out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
        }
        while let Some(block) = blocks.next() {
            let last = u8::from(blocks.peek().is_none());
            let len = u16::try_from(block.len()).unwrap_or(u16::MAX);
            out.push(last);
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(block);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in data {
            a = (a + u32::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        out.extend_from_slice(&((b << 16) | a).to_be_bytes());
        out
    }

    struct Crc(u32);

    impl Crc {
        fn new() -> Self {
            Self(0xffff_ffff)
        }

        fn update(&mut self, data: &[u8]) {
            for &byte in data {
                self.0 ^= u32::from(byte);
                for _ in 0..8 {
                    let mask = (self.0 & 1).wrapping_neg();
                    self.0 = (self.0 >> 1) ^ (0xedb8_8320 & mask);
                }
            }
        }

        fn finish(self) -> u32 {
            !self.0
        }
    }
}

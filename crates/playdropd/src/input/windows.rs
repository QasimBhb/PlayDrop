//! Windows backend: `SendInput` (see `mod.rs`; protocol.md 5.12, SR-25, T29 to T31).
//!
//! Always supported: `SendInput` needs no permission for the user's own desktop. The
//! daemon never runs elevated or with `uiAccess`, so User Interface Privilege Isolation
//! drops our input into elevated windows and UAC prompts, and the secure desktop (lock
//! screen, Ctrl+Alt+Del) is out of reach by construction (SR-25, T30). The core also stops
//! input sessions while the desktop is locked.
//!
//! The mapping from steps to records lives in `sendinput.rs` (tested on every target);
//! this file only copies records into `INPUT` structs and calls `SendInput` once per
//! frame, so a chord is injected as one uninterrupted sequence that the user's own typing
//! cannot split.

use std::path::Path;

use playdrop_core::session::types::Input;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::WHEEL_DELTA;

use super::plan::{self, Held};
use super::sendinput::{self, Record, Translator};
use super::InputSupport;

// The platform-free flag values must be the real ones.
const _: () = {
    use sendinput::{key, mouse};
    assert!(MOUSEEVENTF_MOVE.0 == mouse::MOVE);
    assert!(MOUSEEVENTF_LEFTDOWN.0 == mouse::LEFTDOWN);
    assert!(MOUSEEVENTF_LEFTUP.0 == mouse::LEFTUP);
    assert!(MOUSEEVENTF_RIGHTDOWN.0 == mouse::RIGHTDOWN);
    assert!(MOUSEEVENTF_RIGHTUP.0 == mouse::RIGHTUP);
    assert!(MOUSEEVENTF_MIDDLEDOWN.0 == mouse::MIDDLEDOWN);
    assert!(MOUSEEVENTF_MIDDLEUP.0 == mouse::MIDDLEUP);
    assert!(MOUSEEVENTF_WHEEL.0 == mouse::WHEEL);
    assert!(MOUSEEVENTF_HWHEEL.0 == mouse::HWHEEL);
    assert!(KEYEVENTF_EXTENDEDKEY.0 == key::EXTENDEDKEY);
    assert!(KEYEVENTF_KEYUP.0 == key::KEYUP);
    assert!(KEYEVENTF_UNICODE.0 == key::UNICODE);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "compile-time check of a small constant"
    )]
    let delta = sendinput::WHEEL_DELTA as u32;
    assert!(WHEEL_DELTA == delta);
};

/// Backend name for diagnostics.
const BACKEND: &str = "send-input";

#[allow(
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    reason = "same signature as the Linux backend"
)]
pub(super) async fn support() -> InputSupport {
    InputSupport {
        supported: true,
        backend: BACKEND,
        reason: None,
    }
}

/// What this injector holds pressed; `SendInput` has no session of its own.
pub(super) struct Injector {
    held: Held,
    translator: Translator,
}

impl Injector {
    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "same signature as the Linux backend"
    )]
    pub(super) async fn start(_data_dir: &Path, peer_label: &str) -> anyhow::Result<Self> {
        tracing::info!(peer = peer_label, "remote input session started");
        Ok(Self {
            held: Held::default(),
            translator: Translator::default(),
        })
    }

    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "same signature as the Linux backend"
    )]
    pub(super) async fn apply(&mut self, input: &Input) -> anyhow::Result<()> {
        let plan = plan::expand(input, &mut self.held);
        if plan.dropped {
            // SR-32: a count, never the value.
            tracing::debug!(count = 1, "dropped an input frame with unknown values");
        }
        let inputs: Vec<INPUT> = plan
            .steps
            .into_iter()
            .flat_map(|s| self.translator.records(s))
            .map(to_input)
            .collect();
        send(&inputs)
    }

    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "same signature as the Linux backend"
    )]
    pub(super) async fn release_all(&mut self) {
        let inputs: Vec<INPUT> = self
            .held
            .release_all()
            .into_iter()
            .flat_map(|s| self.translator.records(s))
            .map(to_input)
            .collect();
        if let Err(e) = send(&inputs) {
            tracing::warn!(error = %e, "could not release held keys and buttons");
        }
    }

    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "same signature as the Linux backend"
    )]
    pub(super) async fn close(self) {}
}

fn to_input(r: Record) -> INPUT {
    match r {
        Record::Mouse {
            dx,
            dy,
            data,
            flags,
        } => INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    // `mouseData` is a DWORD holding a signed wheel delta.
                    mouseData: data.cast_unsigned(),
                    dwFlags: MOUSE_EVENT_FLAGS(flags),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        Record::Key { vk, scan, flags } => INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: if vk == 0 { scan } else { scan_code(vk) },
                    dwFlags: KEYBD_EVENT_FLAGS(flags),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    }
}

/// The scan code for a virtual key on the current layout. Some programs (games, remote
/// desktop clients) read the scan code rather than the virtual key.
fn scan_code(vk: u16) -> u16 {
    #[allow(unsafe_code)]
    // SAFETY: MapVirtualKeyW takes two integers and touches no caller memory.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) };
    u16::try_from(scan).unwrap_or(0)
}

/// Injects `inputs` as one batch.
fn send(inputs: &[INPUT]) -> anyhow::Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let size = i32::try_from(std::mem::size_of::<INPUT>()).expect("INPUT is small");
    #[allow(unsafe_code)]
    // SAFETY: `inputs` is a live, initialised slice of INPUT; the binding passes its exact
    // length and we pass the exact element size, so SendInput reads only that memory.
    let sent = unsafe { SendInput(inputs, size) };
    if usize::try_from(sent).unwrap_or(0) == inputs.len() {
        Ok(())
    } else {
        // SendInput fails when another thread blocked input or the secure desktop is up;
        // UIPI drops events into elevated windows without failing.
        Err(anyhow::anyhow!(
            "Windows accepted {sent} of {} input events: {}",
            inputs.len(),
            windows::core::Error::from_thread()
        ))
    }
}

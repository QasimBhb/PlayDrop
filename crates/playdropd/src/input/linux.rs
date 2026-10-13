//! Linux backend: the `org.freedesktop.portal.RemoteDesktop` portal (see `mod.rs`;
//! protocol.md 5.12, SR-25, T29 to T31).
//!
//! One portal session per [`Injector`], on its own session-bus connection, so a daemon
//! that dies takes its sessions with it (the portal closes a session when its owner's
//! connection goes). The first session shows the desktop's consent dialog; the session
//! asks for `persist_mode` "until revoked" and the restore token it gets back is kept in
//! `<data_dir>/input-restore-token` (0600) so later sessions start without a dialog.
//! Tokens are single use: every successful start replaces the file, and a start that
//! fails after using the token deletes it, so the next start asks the user again rather
//! than retrying a dead token. The token is never logged (SR-32).
//!
//! Frames go through `plan::expand` and each step becomes one portal call
//! ([`Translator`]): pointer motion in pixels, evdev button codes, discrete wheel steps for
//! `line` scrolls and smooth axis motion plus a `finish` for `pixel` scrolls, and X11
//! keysyms for keys so the compositor maps them through the user's layout.

mod portal;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use playdrop_core::session::types::Input;
use tokio::sync::OnceCell;

use self::portal::{
    Call, OpenError, Session, AXIS_HORIZONTAL, AXIS_VERTICAL, DEVICE_KEYBOARD, DEVICE_POINTER,
    STATE_PRESSED, STATE_RELEASED,
};
use super::keys;
use super::plan::{self, Accumulator, Button, Held, Key, Step, Unit};
use super::unsupported::{error, unsupported};
use super::InputSupport;

/// Backend name for diagnostics.
const BACKEND: &str = "remote-desktop-portal";
/// File under the data dir holding the portal restore token.
const TOKEN_FILE: &str = "input-restore-token";
/// Longest the support probe may take, so a hung portal cannot stall the daemon.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

const X11: &str =
    "remote input is not supported on X11 sessions in this version; log in to a Wayland session";
const NO_PORTAL: &str = "this desktop has no RemoteDesktop portal (xdg-desktop-portal with a backend such as xdg-desktop-portal-cosmic or -gnome)";
const NO_DEVICES: &str =
    "the desktop's RemoteDesktop portal does not offer keyboard and pointer control";

/// Linux evdev button codes (`linux/input-event-codes.h`), which the portal expects.
const BTN_LEFT: i32 = 0x110;
const BTN_RIGHT: i32 = 0x111;
const BTN_MIDDLE: i32 = 0x112;

static SUPPORT: OnceCell<InputSupport> = OnceCell::const_new();

/// Probes once per process and caches the answer.
pub(super) async fn support() -> InputSupport {
    SUPPORT.get_or_init(probe).await.clone()
}

async fn probe() -> InputSupport {
    if std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t.eq_ignore_ascii_case("x11")) {
        return unsupported(X11);
    }
    if let Ok(s) = tokio::time::timeout(PROBE_TIMEOUT, probe_portal()).await {
        s
    } else {
        tracing::warn!("the desktop portal did not answer the remote input probe in time");
        unsupported("the desktop portal did not answer within 2 seconds")
    }
}

async fn probe_portal() -> InputSupport {
    let conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            tracing::info!(error = %e, "no session bus; remote input unavailable");
            return unsupported(
                "there is no session D-Bus, so the desktop's RemoteDesktop portal cannot be reached",
            );
        }
    };
    // The portal is D-Bus activated: not running yet is fine as long as it can start.
    if let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await {
        let name = zbus::names::BusName::try_from("org.freedesktop.portal.Desktop")
            .expect("valid bus name");
        let owned = dbus.name_has_owner(name.clone()).await.unwrap_or(false);
        let activatable = owned
            || dbus
                .list_activatable_names()
                .await
                .is_ok_and(|names| names.iter().any(|n| *n == name));
        if !activatable {
            return unsupported(NO_PORTAL);
        }
    }
    match portal::facts(&conn).await {
        Ok(f) => judge(f.version, f.device_types),
        Err(e) => {
            tracing::info!(error = %e, "RemoteDesktop portal properties unreadable");
            unsupported(NO_PORTAL)
        }
    }
}

/// The verdict for a portal that answered.
fn judge(version: u32, device_types: u32) -> InputSupport {
    let needed = DEVICE_KEYBOARD | DEVICE_POINTER;
    if device_types & needed != needed {
        return unsupported(NO_DEVICES);
    }
    if version < 2 {
        // Still works, but every input session shows the consent dialog.
        tracing::info!(
            version,
            "RemoteDesktop portal predates restore tokens; the dialog shows every time"
        );
    }
    InputSupport {
        supported: true,
        backend: BACKEND,
        reason: None,
    }
}

/// Turns steps into portal calls, keeping the fractional part of line scrolls.
#[derive(Debug, Default)]
struct Translator {
    lines_x: Accumulator,
    lines_y: Accumulator,
}

impl Translator {
    fn calls(&mut self, step: Step) -> Vec<Call> {
        match step {
            Step::Motion { dx, dy } => vec![Call::Motion { dx, dy }],
            Step::Button { button, down } => vec![Call::Button {
                code: match button {
                    Button::Left => BTN_LEFT,
                    Button::Right => BTN_RIGHT,
                    Button::Middle => BTN_MIDDLE,
                },
                state: state(down),
            }],
            Step::Scroll {
                dx,
                dy,
                unit: Unit::Line,
            } => {
                let mut calls = Vec::with_capacity(2);
                let v = self.lines_y.take(dy);
                if v != 0 {
                    calls.push(Call::AxisDiscrete {
                        axis: AXIS_VERTICAL,
                        steps: v,
                    });
                }
                let h = self.lines_x.take(dx);
                if h != 0 {
                    calls.push(Call::AxisDiscrete {
                        axis: AXIS_HORIZONTAL,
                        steps: h,
                    });
                }
                calls
            }
            // Smooth scrolling: the motion, then an empty event with `finish` so kinetic
            // scrolling in the client knows the gesture ended.
            Step::Scroll {
                dx,
                dy,
                unit: Unit::Pixel,
            } => vec![
                Call::Axis {
                    dx,
                    dy,
                    finish: false,
                },
                Call::Axis {
                    dx: 0.0,
                    dy: 0.0,
                    finish: true,
                },
            ],
            Step::Key { key, down } => keysym(key)
                .map(|keysym| Call::Keysym {
                    keysym,
                    state: state(down),
                })
                .into_iter()
                .collect(),
        }
    }
}

fn state(down: bool) -> u32 {
    if down {
        STATE_PRESSED
    } else {
        STATE_RELEASED
    }
}

/// The keysym the portal receives, as the `i` its signature asks for. Every keysym,
/// Unicode ones included (`0x0100_0000 + code point`), is below `i32::MAX`.
fn keysym(key: Key) -> Option<i32> {
    let raw = match key {
        Key::Named(e) => e.keysym,
        Key::Char { ch, .. } | Key::Text(ch) => xkeysym::Keysym::from_char(ch).raw(),
        Key::Modifier(m) => keys::modifier_keysym(m).raw(),
    };
    i32::try_from(raw).ok().filter(|k| *k != 0)
}

/// Reads the restore token, if any.
fn read_token(path: &Path) -> Option<String> {
    let t = std::fs::read_to_string(path).ok()?;
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

/// Replaces the restore token file atomically, readable by the owner only.
fn write_token(path: &Path, token: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(token.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

fn delete_token(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => tracing::debug!("forgot the remote input restore token"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, "could not delete the remote input restore token"),
    }
}

/// A portal session plus what it holds pressed.
pub(super) struct Injector {
    session: Session,
    // Kept so the session lives exactly as long as this injector.
    _conn: zbus::Connection,
    held: Held,
    translator: Translator,
}

impl Injector {
    pub(super) async fn start(data_dir: &Path, peer_label: &str) -> anyhow::Result<Self> {
        let support = support().await;
        if !support.supported {
            return Err(error(&support));
        }
        let token_path: PathBuf = data_dir.join(TOKEN_FILE);
        let token = read_token(&token_path);
        let conn = zbus::Connection::session().await?;
        tracing::info!(
            peer = peer_label,
            remembered = token.is_some(),
            "starting a remote input session; the desktop may be showing its consent dialog"
        );

        let mut opened = Session::open(&conn, token.as_deref()).await;
        if token.is_some() {
            if let Err(OpenError::SelectDevices(e)) = &opened {
                // The portal refused the remembered grant; ask the user again.
                tracing::info!(error = %e, "remote input restore token refused; asking again");
                delete_token(&token_path);
                opened = Session::open(&conn, None).await;
            }
        }
        let opened = match opened {
            Ok(o) => o,
            Err(e) => {
                if token.is_some() {
                    delete_token(&token_path);
                }
                return Err(match e {
                    OpenError::Cancelled => {
                        anyhow::anyhow!("remote input was not allowed in the desktop's dialog")
                    }
                    OpenError::NoAnswer => {
                        anyhow::anyhow!("nobody answered the desktop's remote input dialog in time")
                    }
                    OpenError::SelectDevices(e) | OpenError::Other(e) => {
                        e.context("the desktop's RemoteDesktop portal refused the session")
                    }
                });
            }
        };

        match &opened.restore_token {
            Some(t) => {
                if let Err(e) = write_token(&token_path, t) {
                    tracing::warn!(error = %e, "could not save the remote input restore token");
                }
            }
            None => delete_token(&token_path),
        }
        let needed = DEVICE_KEYBOARD | DEVICE_POINTER;
        if opened.devices != 0 && opened.devices & needed != needed {
            tracing::warn!(
                devices = opened.devices,
                "the user granted only some input devices; the rest will fail"
            );
        }
        tracing::info!(peer = peer_label, "remote input session started");
        Ok(Self {
            session: opened.session,
            _conn: conn,
            held: Held::default(),
            translator: Translator::default(),
        })
    }

    pub(super) async fn apply(&mut self, input: &Input) -> anyhow::Result<()> {
        let plan = plan::expand(input, &mut self.held);
        if plan.dropped {
            // SR-32: a count, never the value.
            tracing::debug!(count = 1, "dropped an input frame with unknown values");
        }
        let mut unmapped = 0_usize;
        for step in plan.steps {
            let calls = self.translator.calls(step);
            if calls.is_empty() && matches!(step, Step::Key { .. }) {
                unmapped += 1;
            }
            for call in calls {
                self.session.send(call).await?;
            }
        }
        if unmapped > 0 {
            tracing::debug!(count = unmapped, "dropped keys with no keysym");
        }
        Ok(())
    }

    pub(super) async fn release_all(&mut self) {
        let steps = self.held.release_all();
        if steps.is_empty() {
            return;
        }
        let mut failed = 0_usize;
        for step in steps {
            for call in self.translator.calls(step) {
                if self.session.send(call).await.is_err() {
                    failed += 1;
                }
            }
        }
        if failed > 0 {
            // The session is usually gone already, which releases everything anyway.
            tracing::debug!(count = failed, "some releases did not reach the portal");
        }
    }

    pub(super) async fn close(self) {
        self.session.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_calls(t: &mut Translator, key: Key) -> Vec<Call> {
        t.calls(Step::Key { key, down: true })
    }

    #[test]
    fn judge_requires_keyboard_and_pointer() {
        assert!(judge(2, 7).supported);
        assert_eq!(judge(2, 7).backend, BACKEND);
        assert!(judge(1, 3).supported);
        for types in [0, 1, 2, 4, 5, 6] {
            let s = judge(2, types);
            assert!(!s.supported);
            assert_eq!(s.reason.as_deref(), Some(NO_DEVICES));
        }
    }

    #[test]
    fn buttons_use_evdev_codes() {
        let mut t = Translator::default();
        let calls: Vec<_> = [Button::Left, Button::Right, Button::Middle]
            .into_iter()
            .flat_map(|button| t.calls(Step::Button { button, down: true }))
            .collect();
        assert_eq!(
            calls,
            [
                Call::Button {
                    code: 0x110,
                    state: STATE_PRESSED
                },
                Call::Button {
                    code: 0x111,
                    state: STATE_PRESSED
                },
                Call::Button {
                    code: 0x112,
                    state: STATE_PRESSED
                },
            ]
        );
        assert_eq!(
            t.calls(Step::Button {
                button: Button::Left,
                down: false
            }),
            [Call::Button {
                code: 0x110,
                state: STATE_RELEASED
            }]
        );
    }

    #[test]
    fn line_scrolls_become_discrete_steps_with_carry() {
        let mut t = Translator::default();
        let line = |dx, dy| Step::Scroll {
            dx,
            dy,
            unit: Unit::Line,
        };
        assert_eq!(
            t.calls(line(-1.0, 3.0)),
            [
                Call::AxisDiscrete {
                    axis: AXIS_VERTICAL,
                    steps: 3
                },
                Call::AxisDiscrete {
                    axis: AXIS_HORIZONTAL,
                    steps: -1
                },
            ]
        );
        assert_eq!(t.calls(line(0.0, 0.5)), Vec::<Call>::new());
        assert_eq!(
            t.calls(line(0.0, 0.5)),
            [Call::AxisDiscrete {
                axis: AXIS_VERTICAL,
                steps: 1
            }]
        );
    }

    #[test]
    fn pixel_scrolls_end_with_finish() {
        let mut t = Translator::default();
        assert_eq!(
            t.calls(Step::Scroll {
                dx: 0.0,
                dy: 12.0,
                unit: Unit::Pixel
            }),
            [
                Call::Axis {
                    dx: 0.0,
                    dy: 12.0,
                    finish: false
                },
                Call::Axis {
                    dx: 0.0,
                    dy: 0.0,
                    finish: true
                },
            ]
        );
    }

    #[test]
    fn keys_become_keysyms() {
        use xkeysym::key;
        let mut t = Translator::default();
        let ks = |calls: Vec<Call>| match calls.as_slice() {
            [Call::Keysym { keysym, .. }] => u32::try_from(*keysym).unwrap(),
            other => panic!("{other:?}"),
        };
        let enter = Key::Named(keys::lookup("Enter").unwrap());
        assert_eq!(ks(key_calls(&mut t, enter)), key::Return);
        assert_eq!(
            ks(key_calls(&mut t, Key::Modifier(keys::Mod::Meta))),
            key::Super_L
        );
        assert_eq!(
            ks(key_calls(
                &mut t,
                Key::Char {
                    ch: 'q',
                    prefer_vk: true
                }
            )),
            key::q
        );
        // Text uses Unicode keysyms per character.
        assert_eq!(ks(key_calls(&mut t, Key::Text('ж'))), key::Cyrillic_zhe);
        assert_eq!(ks(key_calls(&mut t, Key::Text('😀'))), 0x0101_F600);
    }

    #[test]
    fn text_frame_maps_char_by_char() {
        let mut held = Held::default();
        let mut t = Translator::default();
        let frame = Input::Key(playdrop_core::session::types::KeyInput {
            key: None,
            text: Some("Hi".into()),
            mods: vec![],
            action: playdrop_core::session::types::KeyAction::Press,
        });
        let calls: Vec<Call> = plan::expand(&frame, &mut held)
            .steps
            .into_iter()
            .flat_map(|s| t.calls(s))
            .collect();
        let k = |c: u32, s| Call::Keysym {
            keysym: i32::try_from(c).unwrap(),
            state: s,
        };
        assert_eq!(
            calls,
            [
                k(xkeysym::key::H, STATE_PRESSED),
                k(xkeysym::key::H, STATE_RELEASED),
                k(xkeysym::key::i, STATE_PRESSED),
                k(xkeysym::key::i, STATE_RELEASED),
            ]
        );
    }

    #[test]
    fn token_file_round_trip_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TOKEN_FILE);
        assert_eq!(read_token(&path), None);
        write_token(&path, "abc-123").unwrap();
        assert_eq!(read_token(&path).as_deref(), Some("abc-123"));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        write_token(&path, "next").unwrap();
        assert_eq!(read_token(&path).as_deref(), Some("next"));
        delete_token(&path);
        assert_eq!(read_token(&path), None);
        delete_token(&path);
    }

    /// Opens a real portal session, moves the pointer 20 px right and back, types
    /// nothing, and closes. The desktop shows its consent dialog unless a grant is
    /// remembered; approve it on screen. Run with
    /// `cargo test -p playdropd -- --ignored input_portal_live`.
    #[tokio::test]
    #[ignore = "needs a desktop session and a person to approve the portal dialog"]
    async fn input_portal_live() {
        use playdrop_core::session::types::PointerInput;
        let support = support().await;
        assert!(support.supported, "{:?}", support.reason);
        let dir = tempfile::tempdir().unwrap();
        let mut inj = Injector::start(dir.path(), "live test").await.unwrap();
        for dx in [20.0, -20.0] {
            inj.apply(&Input::Pointer(PointerInput::Move { dx, dy: 0.0 }))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        inj.release_all().await;
        inj.close().await;
    }
}

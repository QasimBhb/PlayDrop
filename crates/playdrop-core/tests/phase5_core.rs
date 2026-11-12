//! Phase 5 core over loopback (`docs/core-api.md` section 9, protocol.md 5.12, SR-25):
//! capabilities decide who may be controlled, the `input` grant decides who may control,
//! frames apply only inside a session, moves are coalesced, keys are capped, and every
//! way a session ends gives both sides a final signal; the controller gets exactly one
//! `InputSessionEnded` with a machine-readable reason per `start`, from a new target and
//! from one before the reason field (0.10.0).

mod common;

use std::time::Duration;

use common::{pair, start_host, start_host_with, temp_root, TestHost};
use playdrop_core::host::{HostEvent, InputEndReason, LockFlag};
use playdrop_core::session::types::{
    ButtonAction, Input, KeyAction, KeyInput, PointerButton, PointerInput, SessionOp,
};
use playdrop_core::session::{ErrorCode, Frame};
use playdrop_core::Error;

const T: Duration = Duration::from_secs(20);

fn start() -> Input {
    Input::Session {
        op: SessionOp::Start,
    }
}

fn stop() -> Input {
    Input::Session {
        op: SessionOp::Stop,
    }
}

fn click() -> Input {
    Input::Pointer(PointerInput::Button {
        button: PointerButton::Left,
        action: ButtonAction::Click,
    })
}

fn is_start(e: &HostEvent) -> bool {
    matches!(
        e,
        HostEvent::Input {
            input: Input::Session {
                op: SessionOp::Start
            },
            ..
        }
    )
}

/// Waits for the controller's next `InputSessionEnded` and returns its code and text.
async fn ended(h: &mut TestHost) -> (InputEndReason, String) {
    let ev = h
        .events
        .wait_for(T, |e| matches!(e, HostEvent::InputSessionEnded { .. }))
        .await;
    let HostEvent::InputSessionEnded { code, reason, .. } = ev else {
        unreachable!()
    };
    (code, reason)
}

/// How many `InputSessionEnded` the controller has seen.
fn ended_count(h: &mut TestHost) -> usize {
    h.events.drain();
    h.events
        .seen
        .iter()
        .filter(|e| matches!(e, HostEvent::InputSessionEnded { .. }))
        .count()
}

/// Sends a ping from `from` and waits until `to` has it: frames on one session arrive in
/// order, so everything `from` sent before has been handled by `to` too.
async fn barrier(from: &TestHost, to: &mut TestHost, tag: &str) {
    from.host
        .ping(&to.host.device_id(), Some(tag))
        .expect("ping");
    to.events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::Ping { message: Some(m), .. } if m == tag),
        )
        .await;
}

/// The phone opens a fresh session on the desk and the desk has it. The desk first ends
/// any session the phone still holds there, and the phone has that answer before its new
/// `start`, so the answer cannot end the new session.
async fn open_session(phone: &mut TestHost, desk: &mut TestHost) {
    desk.host.stop_input_sessions();
    barrier(desk, phone, "before-open").await;
    phone.events.clear();
    desk.events.clear();
    phone
        .host
        .send_input(&desk.host.device_id(), start())
        .unwrap();
    desk.events.wait_for(T, is_start).await;
}

async fn set_input_grant(h: &TestHost, peer: &playdrop_core::identity::DeviceId, on: bool) {
    let mut g = h.host.grants(peer).unwrap();
    g.input = on;
    h.host.set_grants(peer, g).await.unwrap();
}

fn is_stop(e: &HostEvent) -> bool {
    matches!(
        e,
        HostEvent::Input {
            input: Input::Session {
                op: SessionOp::Stop
            },
            ..
        }
    )
}

/// A desktop (`accepts_input`) and a phone: the phone sees `input.*` on the desktop and
/// not the other way round; a frame without the `input` grant is refused with
/// `forbidden` naming the frame; inside a session moves arrive coalesced and in order
/// before a click, keys stop at 60 per second, and `stop` ends it on both sides.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scripted session, read top to bottom"
)]
async fn remote_input_grant_capability_session_and_caps() {
    common::init_logs();
    let root = temp_root();
    let mut desk = start_host_with(root.path(), "desk", 0, |c| c.accepts_input = true).await;
    let mut phone = start_host(root.path(), "phone", 0).await;
    pair(&mut phone, &mut desk).await;
    let desk_id = desk.host.device_id();
    let phone_id = phone.host.device_id();

    // Capabilities (anti-pattern 18: the phone can tell what the desktop accepts).
    let seen_by_phone = phone.host.peers().remove(0);
    assert!(seen_by_phone
        .capabilities
        .iter()
        .any(|c| c == "input.pointer"));
    let seen_by_desk = desk.host.peers().remove(0);
    assert!(!seen_by_desk
        .capabilities
        .iter()
        .any(|c| c.starts_with("input.")));
    // The desktop cannot control the phone: no capability, refused locally, and the
    // desktop's UI hears why from the same event as every other end.
    assert!(matches!(
        desk.host.send_input(&phone_id, start()),
        Err(Error::Forbidden(_))
    ));
    assert_eq!(ended(&mut desk).await.0, InputEndReason::NotAccepted);

    // The desktop's `input` grant for the phone is off by default (SR-14): the `start` is
    // dropped there and answered with its reason; a later frame gets the one `forbidden`
    // naming the frame.
    phone.host.send_input(&desk_id, start()).unwrap();
    assert_eq!(ended(&mut phone).await.0, InputEndReason::GrantOff);
    phone.host.send_input(&desk_id, click()).unwrap();
    let ev = phone
        .events
        .wait_for(T, |e| matches!(e, HostEvent::PeerRefused { .. }))
        .await;
    let HostEvent::PeerRefused {
        frame_type, code, ..
    } = ev
    else {
        unreachable!()
    };
    assert_eq!(frame_type.as_deref(), Some("input"));
    assert_eq!(code, "forbidden");
    assert_eq!(desk.host.input_sessions(), []);
    // The `forbidden` for a frame after the refused `start` does not end anything twice.
    assert_eq!(ended_count(&mut phone), 1);
    phone.events.clear();

    // Grant it from the desktop's own UI (SR-14), then a session opens.
    let mut g = desk.host.grants(&phone_id).unwrap();
    g.input = true;
    desk.host.set_grants(&phone_id, g).await.unwrap();
    // A move outside a session is dropped (SR-25).
    phone
        .host
        .send_input(
            &desk_id,
            Input::Pointer(PointerInput::Move { dx: 5.0, dy: 5.0 }),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    desk.events.drain();
    assert!(!desk.events.seen.iter().any(|e| matches!(
        e,
        HostEvent::Input {
            input: Input::Pointer(_),
            ..
        }
    )));

    phone.host.send_input(&desk_id, start()).unwrap();
    desk.events
        .wait_for(T, |e| {
            matches!(
                e,
                HostEvent::Input {
                    input: Input::Session {
                        op: SessionOp::Start
                    },
                    ..
                }
            )
        })
        .await;
    assert_eq!(desk.host.input_sessions(), vec![phone_id]);
    desk.events.clear();

    // Ten moves in a burst, then a click: fewer than ten move frames arrive, their deltas
    // sum to the total, and the click comes after every move (5.12 coalescing).
    for _ in 0..10 {
        phone
            .host
            .send_input(
                &desk_id,
                Input::Pointer(PointerInput::Move { dx: 1.5, dy: -1.0 }),
            )
            .unwrap();
    }
    phone
        .host
        .send_input(
            &desk_id,
            Input::Pointer(PointerInput::Button {
                button: PointerButton::Left,
                action: ButtonAction::Click,
            }),
        )
        .unwrap();
    desk.events
        .wait_for(T, |e| {
            matches!(
                e,
                HostEvent::Input {
                    input: Input::Pointer(PointerInput::Button { .. }),
                    ..
                }
            )
        })
        .await;
    let inputs: Vec<&Input> = desk
        .events
        .seen
        .iter()
        .filter_map(|e| match e {
            HostEvent::Input { input, .. } => Some(input),
            _ => None,
        })
        .collect();
    let moves: Vec<(f64, f64)> = inputs
        .iter()
        .filter_map(|i| match i {
            Input::Pointer(PointerInput::Move { dx, dy }) => Some((*dx, *dy)),
            _ => None,
        })
        .collect();
    assert!(!moves.is_empty() && moves.len() < 10, "moves: {moves:?}");
    let (sx, sy) = moves
        .iter()
        .fold((0.0, 0.0), |(x, y), (dx, dy)| (x + dx, y + dy));
    assert!(
        (sx - 15.0).abs() < 1e-9 && (sy + 10.0).abs() < 1e-9,
        "sum {sx} {sy}"
    );
    let last_move = inputs
        .iter()
        .rposition(|i| matches!(i, Input::Pointer(PointerInput::Move { .. })))
        .unwrap();
    let click = inputs
        .iter()
        .position(|i| matches!(i, Input::Pointer(PointerInput::Button { .. })))
        .unwrap();
    assert!(last_move < click, "click overtook a move");
    desk.events.clear();

    // 100 key presses in a burst: the target keeps 60 (5.12).
    for _ in 0..100 {
        phone
            .host
            .send_input(
                &desk_id,
                Input::Key(KeyInput {
                    key: Some("a".into()),
                    text: None,
                    mods: vec![],
                    action: KeyAction::Press,
                }),
            )
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    desk.events.drain();
    let keys = desk
        .events
        .seen
        .iter()
        .filter(|e| {
            matches!(
                e,
                HostEvent::Input {
                    input: Input::Key(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(keys, 60, "key cap");
    desk.events.clear();

    // The phone stops: the desktop gets its final `stop` and lists no session.
    phone.host.send_input(&desk_id, stop()).unwrap();
    desk.events.wait_for(T, is_stop).await;
    assert_eq!(desk.host.input_sessions(), []);

    // The desktop's kill switch (SR-25): both sides hear about it with the reason.
    phone.host.send_input(&desk_id, start()).unwrap();
    desk.events
        .wait_for(T, |e| {
            matches!(
                e,
                HostEvent::Input {
                    input: Input::Session {
                        op: SessionOp::Start
                    },
                    ..
                }
            )
        })
        .await;
    desk.events.clear();
    assert_eq!(desk.host.stop_input_sessions(), vec![phone_id]);
    desk.events.wait_for(T, is_stop).await;
    let ev = phone
        .events
        .wait_for(T, |e| matches!(e, HostEvent::InputSessionEnded { .. }))
        .await;
    let HostEvent::InputSessionEnded { reason, code, .. } = ev else {
        unreachable!()
    };
    assert_eq!(reason, "stopped on the desktop");
    assert_eq!(code, InputEndReason::StoppedByUser);
    assert_eq!(desk.host.input_sessions(), []);

    desk.host.shutdown().await;
    phone.host.shutdown().await;
}

/// The controller's connection drops mid-session: the target gets its final `stop` so the
/// backend releases every key and button (T31).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_session_ends_when_the_controller_drops() {
    common::init_logs();
    let root = temp_root();
    let mut desk = start_host_with(root.path(), "desk", 0, |c| c.accepts_input = true).await;
    let mut phone = start_host(root.path(), "phone", 0).await;
    pair(&mut phone, &mut desk).await;
    let desk_id = desk.host.device_id();
    let phone_id = phone.host.device_id();
    let mut g = desk.host.grants(&phone_id).unwrap();
    g.input = true;
    desk.host.set_grants(&phone_id, g).await.unwrap();
    phone.host.send_input(&desk_id, start()).unwrap();
    desk.events
        .wait_for(T, |e| {
            matches!(
                e,
                HostEvent::Input {
                    input: Input::Session {
                        op: SessionOp::Start
                    },
                    ..
                }
            )
        })
        .await;
    desk.events.clear();
    phone.host.shutdown().await;
    desk.events.wait_for(T, is_stop).await;
    assert_eq!(desk.host.input_sessions(), []);
    desk.host.shutdown().await;
}

/// PLAN.md row 5 gate, loopback stand-in for the Windows LAN number: the time from
/// `send_input` on the controller to the `Input` event on the target, for a click (not
/// coalesced), median over 50, must stay well under 30 ms. Prints the numbers with
/// `--nocapture`; the Wi-Fi figure comes from the phone test in the handoff.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_input_latency_on_loopback() {
    common::init_logs();
    let root = temp_root();
    let mut desk = start_host_with(root.path(), "desk", 0, |c| c.accepts_input = true).await;
    let mut phone = start_host(root.path(), "phone", 0).await;
    pair(&mut phone, &mut desk).await;
    let desk_id = desk.host.device_id();
    let phone_id = phone.host.device_id();
    let mut g = desk.host.grants(&phone_id).unwrap();
    g.input = true;
    desk.host.set_grants(&phone_id, g).await.unwrap();
    phone.host.send_input(&desk_id, start()).unwrap();
    desk.events
        .wait_for(T, |e| {
            matches!(
                e,
                HostEvent::Input {
                    input: Input::Session {
                        op: SessionOp::Start
                    },
                    ..
                }
            )
        })
        .await;
    let mut samples = Vec::with_capacity(50);
    for _ in 0..50 {
        desk.events.clear();
        let t0 = std::time::Instant::now();
        phone
            .host
            .send_input(
                &desk_id,
                Input::Pointer(PointerInput::Button {
                    button: PointerButton::Left,
                    action: ButtonAction::Click,
                }),
            )
            .unwrap();
        desk.events
            .wait_for(T, |e| {
                matches!(
                    e,
                    HostEvent::Input {
                        input: Input::Pointer(PointerInput::Button { .. }),
                        ..
                    }
                )
            })
            .await;
        samples.push(t0.elapsed());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    samples.sort();
    let median = samples[samples.len() / 2];
    let p95 = samples[samples.len() * 95 / 100];
    println!(
        "input latency on loopback: median {:.2} ms, p95 {:.2} ms, max {:.2} ms",
        median.as_secs_f64() * 1e3,
        p95.as_secs_f64() * 1e3,
        samples.last().unwrap().as_secs_f64() * 1e3
    );
    assert!(median < Duration::from_millis(30), "median {median:?}");
    phone.host.send_input(&desk_id, stop()).unwrap();
    desk.host.shutdown().await;
    phone.host.shutdown().await;
}

/// Every way a `start` is refused or a session we control ends gives the controller exactly
/// one `InputSessionEnded` with its reason: grant off (on every try, also on one
/// connection), grant turned off mid-session, locked, a target that hides remote input,
/// the target's Stop (in the test above), and the connection lost. Our own `stop` gives
/// none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scripted exchange, read top to bottom"
)]
async fn the_controller_hears_why_every_session_ended() {
    common::init_logs();
    let root = temp_root();
    let lock = LockFlag::new();
    let lock2 = lock.clone();
    let mut desk = start_host_with(root.path(), "desk", 0, move |c| {
        c.accepts_input = true;
        c.lock_state = Box::new(lock2);
    })
    .await;
    let mut phone = start_host(root.path(), "phone", 0).await;
    pair(&mut phone, &mut desk).await;
    let (desk_id, phone_id) = (desk.host.device_id(), phone.host.device_id());

    // (a) Grant off: each `start` on the same connection is answered, so opening the
    // trackpad a second time says why again.
    for _ in 0..2 {
        phone.events.clear();
        phone.host.send_input(&desk_id, start()).unwrap();
        let (code, text) = ended(&mut phone).await;
        assert_eq!(code, InputEndReason::GrantOff);
        assert_eq!(text, InputEndReason::GrantOff.message());
    }
    assert_eq!(desk.host.input_sessions(), []);

    // The grant turned off mid-session ends it on both sides (SR-25).
    set_input_grant(&desk, &phone_id, true).await;
    phone.events.clear();
    phone.host.send_input(&desk_id, start()).unwrap();
    desk.events.wait_for(T, is_start).await;
    desk.events.clear();
    set_input_grant(&desk, &phone_id, false).await;
    desk.events.wait_for(T, is_stop).await;
    assert_eq!(ended(&mut phone).await.0, InputEndReason::GrantOff);
    assert_eq!(desk.host.input_sessions(), []);
    set_input_grant(&desk, &phone_id, true).await;

    // (b) Locked: a `start` is answered, and a session open at lock ends.
    lock.set_locked(true);
    phone.events.clear();
    phone.host.send_input(&desk_id, start()).unwrap();
    assert_eq!(ended(&mut phone).await.0, InputEndReason::Locked);
    lock.set_locked(false);
    phone.events.clear();
    phone.host.send_input(&desk_id, start()).unwrap();
    desk.events.wait_for(T, is_start).await;
    lock.set_locked(true);
    phone.host.send_input(&desk_id, click()).unwrap();
    assert_eq!(ended(&mut phone).await.0, InputEndReason::Locked);
    lock.set_locked(false);

    // Our own `stop` raises nothing.
    phone.events.clear();
    phone.host.send_input(&desk_id, start()).unwrap();
    desk.events.wait_for(T, is_start).await;
    phone.host.send_input(&desk_id, stop()).unwrap();
    desk.events.wait_for(T, is_stop).await;
    barrier(&desk, &mut phone, "after-own-stop").await;
    assert_eq!(ended_count(&mut phone), 0);

    // Connection lost: the target goes away mid-session.
    phone.host.send_input(&desk_id, start()).unwrap();
    desk.events.wait_for(T, is_start).await;
    desk.host.shutdown().await;
    assert_eq!(ended(&mut phone).await.0, InputEndReason::ConnectionLost);
    // A `start` with no connection fails and says so the same way.
    phone.events.clear();
    assert!(matches!(
        phone.host.send_input(&desk_id, start()),
        Err(Error::NotFound)
    ));
    assert_eq!(ended(&mut phone).await.0, InputEndReason::ConnectionLost);
    phone.host.shutdown().await;
}

/// (c, d) A target that hides remote input (`accepts_input` off, or a build with no
/// backend) lists no `input.*`: the controller refuses locally and says
/// [`InputEndReason::NotAccepted`]. A `start` that arrives anyway (a controller that ignores
/// the lists) opens nothing, even with the grant on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_target_that_does_not_accept_input_refuses_it() {
    common::init_logs();
    let root = temp_root();
    let mut desk = start_host(root.path(), "desk", 0).await;
    let mut phone = start_host(root.path(), "phone", 0).await;
    pair(&mut phone, &mut desk).await;
    let (desk_id, phone_id) = (desk.host.device_id(), phone.host.device_id());
    set_input_grant(&desk, &phone_id, true).await;

    assert!(matches!(
        phone.host.send_input(&desk_id, start()),
        Err(Error::Forbidden(_))
    ));
    let (code, text) = ended(&mut phone).await;
    assert_eq!(code, InputEndReason::NotAccepted);
    assert_eq!(text, "the device does not accept remote input");

    assert!(phone
        .host
        .send_frame_for_test(&desk_id, Frame::Input(start()), None));
    assert!(phone
        .host
        .send_frame_for_test(&desk_id, Frame::Input(click()), None));
    barrier(&phone, &mut desk, "after-hidden").await;
    assert!(!desk
        .events
        .seen
        .iter()
        .any(|e| matches!(e, HostEvent::Input { .. })));
    assert_eq!(desk.host.input_sessions(), []);
    // The answer to that `start` names no session of ours, so nothing more is raised.
    barrier(&desk, &mut phone, "after-answer").await;
    assert_eq!(ended_count(&mut phone), 1);

    desk.host.shutdown().await;
    phone.host.shutdown().await;
}

/// A target from before the reason field (0.10.0) still yields a reason: its one
/// `forbidden` for an `input` frame reads as grant off, its three known texts read as what
/// they say, and any other text as [`InputEndReason::Other`] with the text kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_older_target_still_yields_a_reason() {
    common::init_logs();
    let root = temp_root();
    let mut desk = start_host_with(root.path(), "desk", 0, |c| c.accepts_input = true).await;
    let mut phone = start_host(root.path(), "phone", 0).await;
    pair(&mut phone, &mut desk).await;
    let (desk_id, phone_id) = (desk.host.device_id(), phone.host.device_id());
    set_input_grant(&desk, &phone_id, true).await;

    // The desk opens each session; then answers as 0.10.0 would have.
    open_session(&mut phone, &mut desk).await;
    let seq = phone
        .host
        .last_sent_seq_for_test(&desk_id, "input")
        .expect("start written");
    assert!(desk.host.send_frame_for_test(
        &phone_id,
        Frame::error(
            ErrorCode::Forbidden,
            Some("input is not allowed for this device")
        ),
        Some(seq)
    ));
    phone
        .events
        .wait_for(T, |e| matches!(e, HostEvent::PeerRefused { .. }))
        .await;
    assert_eq!(ended(&mut phone).await.0, InputEndReason::GrantOff);

    for (text, want) in [
        ("the desktop is locked", InputEndReason::Locked),
        ("stopped on the desktop", InputEndReason::StoppedByUser),
        ("no input for 10 minutes", InputEndReason::IdleTimeout),
        ("the cat sat on the keyboard", InputEndReason::Other),
    ] {
        open_session(&mut phone, &mut desk).await;
        assert!(desk.host.send_frame_for_test(
            &phone_id,
            Frame::error(ErrorCode::InputSessionEnded, Some(text)),
            None
        ));
        let (code, got) = ended(&mut phone).await;
        assert_eq!((code, got.as_str()), (want, text));
    }
    // No message at all.
    open_session(&mut phone, &mut desk).await;
    assert!(desk.host.send_frame_for_test(
        &phone_id,
        Frame::error(ErrorCode::InputSessionEnded, None),
        None
    ));
    assert_eq!(
        ended(&mut phone).await,
        (
            InputEndReason::Other,
            InputEndReason::Other.message().to_owned()
        )
    );

    desk.host.shutdown().await;
    phone.host.shutdown().await;
}

fn button_down() -> Input {
    Input::Pointer(PointerInput::Button {
        button: PointerButton::Left,
        action: ButtonAction::Down,
    })
}

/// SR-25: unpairing during a session with a button held ends it at once on both sides,
/// whichever side unpairs: the target's host gets its final `stop` (so the backend releases
/// the button) and the controller gets exactly one `InputSessionEnded`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unpair_during_a_session_ends_it_on_both_sides() {
    common::init_logs();
    for target_unpairs in [true, false] {
        let root = temp_root();
        let mut desk = start_host_with(root.path(), "desk", 0, |c| c.accepts_input = true).await;
        let mut phone = start_host(root.path(), "phone", 0).await;
        pair(&mut phone, &mut desk).await;
        let (desk_id, phone_id) = (desk.host.device_id(), phone.host.device_id());
        set_input_grant(&desk, &phone_id, true).await;
        open_session(&mut phone, &mut desk).await;
        phone.host.send_input(&desk_id, button_down()).unwrap();
        desk.events
            .wait_for(T, |e| {
                matches!(
                    e,
                    HostEvent::Input {
                        input: Input::Pointer(PointerInput::Button {
                            action: ButtonAction::Down,
                            ..
                        }),
                        ..
                    }
                )
            })
            .await;
        desk.events.clear();
        phone.events.clear();
        if target_unpairs {
            desk.host.unpair(&phone_id).await.unwrap();
        } else {
            phone.host.unpair(&desk_id).await.unwrap();
        }
        // The stop comes before `Unpaired` on the side that unpaired, and at once on the
        // other, not after the 10 minute idle sweep.
        desk.events.wait_for(T, is_stop).await;
        assert_eq!(desk.host.input_sessions(), []);
        let (code, _) = ended(&mut phone).await;
        assert!(matches!(code, InputEndReason::ConnectionLost), "{code:?}");
        phone
            .events
            .wait_for(T, |e| matches!(e, HostEvent::Unpaired { .. }))
            .await;
        desk.events
            .wait_for(T, |e| matches!(e, HostEvent::Unpaired { .. }))
            .await;
        // The connections close after unpair; nothing raises a second end.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(
            ended_count(&mut phone),
            1,
            "target_unpairs {target_unpairs}"
        );
        let stops = desk.events.seen.iter().filter(|e| is_stop(e)).count();
        assert_eq!(stops, 1, "target_unpairs {target_unpairs}");
        desk.host.shutdown().await;
        phone.host.shutdown().await;
    }
}

/// An answer that names an earlier `start` (its `re`) does not end the session a newer
/// `start` opened; an answer that names the current one does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_answer_to_an_earlier_start_does_not_end_a_newer_session() {
    common::init_logs();
    let root = temp_root();
    let mut desk = start_host_with(root.path(), "desk", 0, |c| c.accepts_input = true).await;
    let mut phone = start_host(root.path(), "phone", 0).await;
    pair(&mut phone, &mut desk).await;
    let (desk_id, phone_id) = (desk.host.device_id(), phone.host.device_id());
    set_input_grant(&desk, &phone_id, true).await;

    open_session(&mut phone, &mut desk).await;
    let first = phone
        .host
        .last_sent_seq_for_test(&desk_id, "input")
        .expect("start written");
    phone.host.send_input(&desk_id, stop()).unwrap();
    desk.events.wait_for(T, is_stop).await;
    open_session(&mut phone, &mut desk).await;
    let second = phone
        .host
        .last_sent_seq_for_test(&desk_id, "input")
        .expect("start written");
    assert!(second > first);

    for code in [ErrorCode::InputSessionEnded, ErrorCode::Forbidden] {
        assert!(desk.host.send_frame_for_test(
            &phone_id,
            Frame::error_with_reason(code, "late", "grant_off"),
            Some(first)
        ));
    }
    barrier(&desk, &mut phone, "after-late").await;
    assert_eq!(ended_count(&mut phone), 0);

    assert!(desk.host.send_frame_for_test(
        &phone_id,
        Frame::error_with_reason(ErrorCode::InputSessionEnded, "now", "grant_off"),
        Some(second)
    ));
    assert_eq!(ended(&mut phone).await.0, InputEndReason::GrantOff);

    desk.host.shutdown().await;
    phone.host.shutdown().await;
}

/// A target older than 0.11.0 answers its first refusal per connection with `forbidden`
/// and then drops every `input` frame unanswered: a later `start` to it ends at once
/// locally with `GrantOff`, and is not sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_to_an_older_target_that_refused_once_ends_at_once() {
    common::init_logs();
    let root = temp_root();
    let mut desk = start_host_with(root.path(), "desk", 0, |c| c.accepts_input = true).await;
    let mut phone = start_host(root.path(), "phone", 0).await;
    pair(&mut phone, &mut desk).await;
    let (desk_id, phone_id) = (desk.host.device_id(), phone.host.device_id());
    set_input_grant(&desk, &phone_id, true).await;
    assert!(phone
        .host
        .set_peer_app_for_test(&desk_id, "playdrop-core/0.10.0"));

    // A `start` the desk opens, refused the way 0.10.0 would after its grant went off.
    open_session(&mut phone, &mut desk).await;
    let seq = phone
        .host
        .last_sent_seq_for_test(&desk_id, "input")
        .expect("start written");
    assert!(desk.host.send_frame_for_test(
        &phone_id,
        Frame::error(
            ErrorCode::Forbidden,
            Some("input is not allowed for this device")
        ),
        Some(seq)
    ));
    assert_eq!(ended(&mut phone).await.0, InputEndReason::GrantOff);
    desk.host.stop_input_sessions();
    barrier(&desk, &mut phone, "after-stop").await;
    desk.events.clear();
    phone.events.clear();

    phone.host.send_input(&desk_id, start()).unwrap();
    assert_eq!(ended(&mut phone).await.0, InputEndReason::GrantOff);
    barrier(&phone, &mut desk, "after-local-end").await;
    assert!(
        !desk.events.seen.iter().any(is_start),
        "the start was not sent"
    );
    assert_eq!(desk.host.input_sessions(), []);
    assert_eq!(ended_count(&mut phone), 1);

    desk.host.shutdown().await;
    phone.host.shutdown().await;
}

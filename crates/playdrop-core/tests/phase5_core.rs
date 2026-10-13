//! Phase 5 core over loopback (`docs/core-api.md` section 9, protocol.md 5.12, SR-25):
//! capabilities decide who may be controlled, the `input` grant decides who may control,
//! frames apply only inside a session, moves are coalesced, keys are capped, and every
//! way a session ends gives both sides a final signal.

mod common;

use std::time::Duration;

use common::{pair, start_host, start_host_with, temp_root};
use playdrop_core::host::HostEvent;
use playdrop_core::session::types::{
    ButtonAction, Input, KeyAction, KeyInput, PointerButton, PointerInput, SessionOp,
};
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
    // The desktop cannot control the phone: no capability, refused locally.
    assert!(matches!(
        desk.host.send_input(&phone_id, start()),
        Err(Error::Forbidden(_))
    ));

    // The desktop's `input` grant for the phone is off by default (SR-14): the frame is
    // dropped there and the phone learns why.
    phone.host.send_input(&desk_id, start()).unwrap();
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
    let HostEvent::InputSessionEnded { reason, .. } = ev else {
        unreachable!()
    };
    assert_eq!(reason, "stopped on the desktop");
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

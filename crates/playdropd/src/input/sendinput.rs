//! Platform-free half of the Windows backend: [`Step`]s from `plan.rs` become plain
//! `SendInput` records ([`Record`]) that `windows.rs` copies into `INPUT` structs. Kept
//! out of `windows.rs` so the mapping is unit-tested on Linux too; `windows.rs` asserts at
//! compile time that the flag values below match the `windows` crate's.
//!
//! - **Moves** are relative `MOUSEEVENTF_MOVE` in whole pixels; the fraction carries to
//!   the next frame so slow trackpad motion is not lost. Windows applies the user's
//!   pointer acceleration ("Enhance pointer precision") to relative moves, as it does to
//!   a physical mouse.
//! - **Scroll:** `line` is [`WHEEL_DELTA`] (120) per line. `pixel` is scaled by
//!   [`WHEEL_PER_PIXEL`]: Windows scrolls 3 lines per 120 by default and a line is about
//!   16 px, so 120 per 48 px. The wheel's positive direction is away from the user (up),
//!   the protocol's positive `dy` is down, so the vertical sign flips; `MOUSEEVENTF_HWHEEL`
//!   is positive to the right, like the protocol.
//! - **Keys:** named keys and modifiers send their virtual key, with
//!   `KEYEVENTF_EXTENDEDKEY` where the table says so. A character from `key` that is part
//!   of a Ctrl, Alt or Meta chord sends its US-layout virtual key (so Ctrl+C fires);
//!   every other character, and all of `text`, is typed with `KEYEVENTF_UNICODE` per
//!   UTF-16 unit, which is layout independent.

use super::keys;
use super::plan::{Accumulator, Button, Key, Step, Unit};

/// `MOUSEINPUT.dwFlags` values (winuser.h).
pub(super) mod mouse {
    pub const MOVE: u32 = 0x0001;
    pub const LEFTDOWN: u32 = 0x0002;
    pub const LEFTUP: u32 = 0x0004;
    pub const RIGHTDOWN: u32 = 0x0008;
    pub const RIGHTUP: u32 = 0x0010;
    pub const MIDDLEDOWN: u32 = 0x0020;
    pub const MIDDLEUP: u32 = 0x0040;
    pub const WHEEL: u32 = 0x0800;
    pub const HWHEEL: u32 = 0x1000;
}

/// `KEYBDINPUT.dwFlags` values (winuser.h).
pub(super) mod key {
    pub const EXTENDEDKEY: u32 = 0x0001;
    pub const KEYUP: u32 = 0x0002;
    pub const UNICODE: u32 = 0x0004;
}

/// One wheel notch.
pub(super) const WHEEL_DELTA: f64 = 120.0;
/// Wheel units per pixel of `pixel` scroll: 120 per 3 lines of 16 px.
pub(super) const WHEEL_PER_PIXEL: f64 = WHEEL_DELTA / 48.0;

/// One `INPUT` record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Record {
    /// `INPUT_MOUSE`; `data` is `mouseData` (signed wheel delta, 0 otherwise).
    Mouse {
        dx: i32,
        dy: i32,
        data: i32,
        flags: u32,
    },
    /// `INPUT_KEYBOARD`; `scan` is the UTF-16 unit when `flags` has `UNICODE`, else 0
    /// (`windows.rs` fills in the scan code for a virtual key).
    Key { vk: u16, scan: u16, flags: u32 },
}

/// Turns steps into records, carrying fractions of moves and scrolls between frames.
#[derive(Debug, Default)]
pub(super) struct Translator {
    move_x: Accumulator,
    move_y: Accumulator,
    wheel_v: Accumulator,
    wheel_h: Accumulator,
}

impl Translator {
    pub(super) fn records(&mut self, step: Step) -> Vec<Record> {
        match step {
            Step::Motion { dx, dy } => {
                let (dx, dy) = (self.move_x.take(dx), self.move_y.take(dy));
                if dx == 0 && dy == 0 {
                    Vec::new()
                } else {
                    vec![Record::Mouse {
                        dx,
                        dy,
                        data: 0,
                        flags: mouse::MOVE,
                    }]
                }
            }
            Step::Button { button, down } => {
                let flags = match (button, down) {
                    (Button::Left, true) => mouse::LEFTDOWN,
                    (Button::Left, false) => mouse::LEFTUP,
                    (Button::Right, true) => mouse::RIGHTDOWN,
                    (Button::Right, false) => mouse::RIGHTUP,
                    (Button::Middle, true) => mouse::MIDDLEDOWN,
                    (Button::Middle, false) => mouse::MIDDLEUP,
                };
                vec![Record::Mouse {
                    dx: 0,
                    dy: 0,
                    data: 0,
                    flags,
                }]
            }
            Step::Scroll { dx, dy, unit } => {
                let scale = match unit {
                    Unit::Line => WHEEL_DELTA,
                    Unit::Pixel => WHEEL_PER_PIXEL,
                };
                let mut out = Vec::with_capacity(2);
                let v = self.wheel_v.take(-dy * scale);
                if v != 0 {
                    out.push(Record::Mouse {
                        dx: 0,
                        dy: 0,
                        data: v,
                        flags: mouse::WHEEL,
                    });
                }
                let h = self.wheel_h.take(dx * scale);
                if h != 0 {
                    out.push(Record::Mouse {
                        dx: 0,
                        dy: 0,
                        data: h,
                        flags: mouse::HWHEEL,
                    });
                }
                out
            }
            Step::Key { key, down } => key_records(key, down),
        }
    }
}

fn key_records(k: Key, down: bool) -> Vec<Record> {
    let up = if down { 0 } else { key::KEYUP };
    let vk = |vk: u16, extended: bool| {
        vec![Record::Key {
            vk,
            scan: 0,
            flags: up | if extended { key::EXTENDEDKEY } else { 0 },
        }]
    };
    match k {
        Key::Named(e) => vk(e.vk, e.extended),
        Key::Modifier(m) => {
            let (code, ext) = keys::modifier_vk(m);
            vk(code, ext)
        }
        Key::Char { ch, prefer_vk } => match keys::char_vk(ch).filter(|_| prefer_vk) {
            Some(code) => vk(code, false),
            None => unicode(ch, up),
        },
        Key::Text(ch) => unicode(ch, up),
    }
}

/// `KEYEVENTF_UNICODE` records for `ch`, one per UTF-16 unit.
fn unicode(ch: char, up: u32) -> Vec<Record> {
    let mut units = [0_u16; 2];
    ch.encode_utf16(&mut units)
        .iter()
        .map(|u| Record::Key {
            vk: 0,
            scan: *u,
            flags: key::UNICODE | up,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::plan::{expand, Held};
    use playdrop_core::session::types::{
        ButtonAction, Input, KeyAction, KeyInput, Modifier, PointerButton, PointerInput,
    };

    fn run(t: &mut Translator, held: &mut Held, input: &Input) -> Vec<Record> {
        expand(input, held)
            .steps
            .into_iter()
            .flat_map(|s| t.records(s))
            .collect()
    }

    fn k(vk: u16, flags: u32) -> Record {
        Record::Key { vk, scan: 0, flags }
    }
    fn u(scan: u16, flags: u32) -> Record {
        Record::Key {
            vk: 0,
            scan,
            flags: key::UNICODE | flags,
        }
    }

    #[test]
    fn moves_round_and_carry() {
        let mut t = Translator::default();
        let mv = |dx, dy| Step::Motion { dx, dy };
        assert_eq!(
            t.records(mv(3.6, -2.0)),
            [Record::Mouse {
                dx: 3,
                dy: -2,
                data: 0,
                flags: mouse::MOVE
            }]
        );
        assert_eq!(t.records(mv(0.3, 0.0)), Vec::<Record>::new());
        assert_eq!(
            t.records(mv(0.2, 0.0)),
            [Record::Mouse {
                dx: 1,
                dy: 0,
                data: 0,
                flags: mouse::MOVE
            }]
        );
    }

    #[test]
    fn double_click_is_four_records() {
        let mut t = Translator::default();
        let mut held = Held::default();
        let recs = run(
            &mut t,
            &mut held,
            &Input::Pointer(PointerInput::Button {
                button: PointerButton::Right,
                action: ButtonAction::Double,
            }),
        );
        let flags: Vec<u32> = recs
            .iter()
            .map(|r| match r {
                Record::Mouse { flags, .. } => *flags,
                Record::Key { .. } => panic!("key record"),
            })
            .collect();
        assert_eq!(
            flags,
            [
                mouse::RIGHTDOWN,
                mouse::RIGHTUP,
                mouse::RIGHTDOWN,
                mouse::RIGHTUP
            ]
        );
    }

    #[test]
    fn wheel_signs_and_scales() {
        let mut t = Translator::default();
        // One line down is one notch towards the user: negative.
        assert_eq!(
            t.records(Step::Scroll {
                dx: 2.0,
                dy: 1.0,
                unit: Unit::Line
            }),
            [
                Record::Mouse {
                    dx: 0,
                    dy: 0,
                    data: -120,
                    flags: mouse::WHEEL
                },
                Record::Mouse {
                    dx: 0,
                    dy: 0,
                    data: 240,
                    flags: mouse::HWHEEL
                },
            ]
        );
        // 48 px up is one notch away from the user.
        assert_eq!(
            t.records(Step::Scroll {
                dx: 0.0,
                dy: -48.0,
                unit: Unit::Pixel
            }),
            [Record::Mouse {
                dx: 0,
                dy: 0,
                data: 120,
                flags: mouse::WHEEL
            }]
        );
    }

    #[test]
    fn chords_use_virtual_keys_with_extended_flags() {
        let mut t = Translator::default();
        let mut held = Held::default();
        let frame = Input::Key(KeyInput {
            key: Some("c".into()),
            text: None,
            mods: vec![Modifier::Ctrl],
            action: KeyAction::Press,
        });
        assert_eq!(
            run(&mut t, &mut held, &frame),
            [
                k(0x11, 0),
                k(0x43, 0),
                k(0x43, key::KEYUP),
                k(0x11, key::KEYUP)
            ]
        );
        let frame = Input::Key(KeyInput {
            key: Some("ArrowLeft".into()),
            text: None,
            mods: vec![Modifier::Meta],
            action: KeyAction::Press,
        });
        let ext = key::EXTENDEDKEY;
        assert_eq!(
            run(&mut t, &mut held, &frame),
            [
                k(0x5B, ext),
                k(0x25, ext),
                k(0x25, ext | key::KEYUP),
                k(0x5B, ext | key::KEYUP)
            ]
        );
    }

    #[test]
    fn text_and_plain_chars_are_unicode_per_utf16_unit() {
        let mut t = Translator::default();
        let mut held = Held::default();
        let frame = Input::Key(KeyInput {
            key: None,
            text: Some("a😀".into()),
            mods: vec![],
            action: KeyAction::Press,
        });
        assert_eq!(
            run(&mut t, &mut held, &frame),
            [
                u(0x61, 0),
                u(0x61, key::KEYUP),
                u(0xD83D, 0),
                u(0xDE00, 0),
                u(0xD83D, key::KEYUP),
                u(0xDE00, key::KEYUP),
            ]
        );
        // A shifted character without a chord types the character itself.
        let frame = Input::Key(KeyInput {
            key: Some("A".into()),
            text: None,
            mods: vec![],
            action: KeyAction::Press,
        });
        assert_eq!(
            run(&mut t, &mut held, &frame),
            [u(0x41, 0), u(0x41, key::KEYUP)]
        );
        // A chord with a character the US layout lacks falls back to Unicode.
        let frame = Input::Key(KeyInput {
            key: Some("é".into()),
            text: None,
            mods: vec![Modifier::Alt],
            action: KeyAction::Press,
        });
        assert_eq!(
            run(&mut t, &mut held, &frame),
            [
                k(0x12, 0),
                u(0xE9, 0),
                u(0xE9, key::KEYUP),
                k(0x12, key::KEYUP)
            ]
        );
    }

    #[test]
    fn presenter_blackout_is_the_b_key() {
        let mut t = Translator::default();
        let mut held = Held::default();
        let frame = Input::Presenter {
            action: playdrop_core::session::types::PresenterAction::Blackout,
        };
        assert_eq!(
            run(&mut t, &mut held, &frame),
            [k(0x42, 0), k(0x42, key::KEYUP)]
        );
    }
}

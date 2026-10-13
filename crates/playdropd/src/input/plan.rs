//! Platform-free expansion of one `input` frame (protocol.md 5.12) into a list of press,
//! release, move and scroll steps, plus the held-state bookkeeping that makes
//! `release_all` possible (T31). Both backends run frames through [`expand`] and only
//! translate the resulting [`Step`]s into portal calls or `INPUT` records, so the rules
//! below are written and tested once:
//!
//! - **Modifiers** in `mods` are pressed before the key, in the order of [`Mod::ALL`],
//!   and released after it in reverse. A modifier already held (from an earlier `down`)
//!   is left alone, so a chord never releases a modifier the phone is holding.
//! - **`press`** is down then up. **`down`** presses the modifiers and the key and keeps
//!   them held; **`up`** releases the key and then those modifiers. An `up` for something
//!   this injector does not hold sends nothing: it cannot be stuck by us, and a stray
//!   release could cancel a key the local user is holding.
//! - **`text`** types each character as down then up (the action is ignored; holding a
//!   string has no meaning). `\r`, `\n` and `\r\n` become Enter and `\t` becomes Tab so
//!   both backends send the real keys rather than control characters.
//! - **Buttons:** `click` is down then up, `double` is two clicks.
//! - **Presenter:** `next` is Right, `previous` is Left, `blackout` is `b`, `pointerOn`
//!   and `pointerOff` do nothing (no OS-level laser pointer; a later phase may draw one).
//! - **Unknown values** (a button, action, unit, modifier or key this version does not
//!   know, protocol.md section 4) drop the whole frame and count it; the backend logs the
//!   count only (SR-32). Non-finite numbers are dropped too, and magnitudes are clamped so
//!   one frame cannot fling the pointer or scroll a document by thousands of pages.

use playdrop_core::session::types::{
    ButtonAction, Input, KeyAction, KeyInput, PointerButton, PointerInput, PresenterAction,
    ScrollUnit,
};

use super::keys::{self, KeyEntry, Mod};

/// Largest pointer move per frame, in pixels on each axis. Moves are coalesced to one
/// per 8 ms (protocol.md 5.12), so this is far above any real swipe.
pub(super) const MAX_MOVE: f64 = 4096.0;
/// Largest scroll per frame, in lines on each axis.
pub(super) const MAX_SCROLL_LINES: f64 = 100.0;
/// Largest scroll per frame, in pixels on each axis.
pub(super) const MAX_SCROLL_PIXELS: f64 = 8192.0;

/// A pointer button as a `Copy` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Button {
    Left,
    Right,
    Middle,
}

/// Scroll units as a `Copy` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unit {
    Line,
    Pixel,
}

/// A key the backends know how to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Key {
    /// A table entry (Enter, arrows, media keys, ...).
    Named(&'static KeyEntry),
    /// One printable character from a `key` value. `prefer_vk` is set when the key is
    /// part of a Ctrl, Alt or Meta chord (or a presenter key): Windows then sends the
    /// virtual key so the shortcut fires, otherwise it types the character itself.
    Char { ch: char, prefer_vk: bool },
    /// One character of a `text` value.
    Text(char),
    /// A modifier, from `mods` or a bare `Control`/`Alt`/`Shift`/`Meta` key.
    Modifier(Mod),
}

/// One OS-level action.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Step {
    /// Relative pointer move in pixels.
    Motion { dx: f64, dy: f64 },
    /// Button press (`down: true`) or release.
    Button { button: Button, down: bool },
    /// Scroll; positive `dy` scrolls down, positive `dx` scrolls right (the protocol's
    /// sign, which is also the portal's and libei's).
    Scroll { dx: f64, dy: f64, unit: Unit },
    /// Key press (`down: true`) or release.
    Key { key: Key, down: bool },
}

/// Something this injector holds pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeldItem {
    Button(Button),
    Key(Key),
}

/// The keys, modifiers and buttons an injector holds, in press order.
#[derive(Debug, Default)]
pub(super) struct Held {
    items: Vec<HeldItem>,
}

impl Held {
    fn contains(&self, item: HeldItem) -> bool {
        self.items.contains(&item)
    }

    fn insert(&mut self, item: HeldItem) {
        if !self.contains(item) {
            self.items.push(item);
        }
    }

    /// Removes `item`; `true` when it was held.
    fn remove(&mut self, item: HeldItem) -> bool {
        if let Some(i) = self.items.iter().position(|h| *h == item) {
            self.items.remove(i);
            true
        } else {
            false
        }
    }

    /// Number of held items.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.items.len()
    }

    /// Releases everything held, last pressed first (T31), and forgets it.
    pub(super) fn release_all(&mut self) -> Vec<Step> {
        self.items
            .drain(..)
            .rev()
            .map(|item| match item {
                HeldItem::Button(button) => Step::Button {
                    button,
                    down: false,
                },
                HeldItem::Key(key) => Step::Key { key, down: false },
            })
            .collect()
    }
}

/// What one frame expands to.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Plan {
    /// The steps to send, in order.
    pub(super) steps: Vec<Step>,
    /// `true` when the frame was dropped because it held a value this version does not
    /// know (logged as a count only, SR-32).
    pub(super) dropped: bool,
}

impl Plan {
    fn dropped() -> Self {
        Self {
            steps: Vec::new(),
            dropped: true,
        }
    }

    fn steps(steps: Vec<Step>) -> Self {
        Self {
            steps,
            dropped: false,
        }
    }
}

/// Expands one frame and updates `held` to match the steps it returns.
pub(super) fn expand(input: &Input, held: &mut Held) -> Plan {
    match input {
        // The daemon opens and closes injectors itself.
        Input::Session { .. } => Plan::default(),
        Input::Pointer(p) => pointer(p, held),
        Input::Key(k) => key(k, held),
        Input::Presenter { action } => presenter(action, held),
    }
}

fn button_of(b: &PointerButton) -> Option<Button> {
    match b {
        PointerButton::Left => Some(Button::Left),
        PointerButton::Right => Some(Button::Right),
        PointerButton::Middle => Some(Button::Middle),
        PointerButton::Unknown(_) => None,
    }
}

/// `v` clamped to `±limit`, or `None` when it is not a finite number.
fn finite(v: f64, limit: f64) -> Option<f64> {
    v.is_finite().then(|| v.clamp(-limit, limit))
}

fn pointer(p: &PointerInput, held: &mut Held) -> Plan {
    match p {
        PointerInput::Move { dx, dy } => match (finite(*dx, MAX_MOVE), finite(*dy, MAX_MOVE)) {
            (Some(dx), Some(dy)) => Plan::steps(vec![Step::Motion { dx, dy }]),
            _ => Plan::dropped(),
        },
        PointerInput::Button { button, action } => {
            let Some(button) = button_of(button) else {
                return Plan::dropped();
            };
            let item = HeldItem::Button(button);
            let press = Step::Button { button, down: true };
            let release = Step::Button {
                button,
                down: false,
            };
            let steps = match action {
                ButtonAction::Down => {
                    held.insert(item);
                    vec![press]
                }
                ButtonAction::Up => {
                    if held.remove(item) {
                        vec![release]
                    } else {
                        Vec::new()
                    }
                }
                ButtonAction::Click | ButtonAction::Double => {
                    // A click while the same button is held would leave the OS confused
                    // about the press count; release the held press first.
                    let mut steps = Vec::with_capacity(5);
                    if held.remove(item) {
                        steps.push(release);
                    }
                    let clicks = if matches!(action, ButtonAction::Double) {
                        2
                    } else {
                        1
                    };
                    for _ in 0..clicks {
                        steps.push(press);
                        steps.push(release);
                    }
                    steps
                }
                ButtonAction::Unknown(_) => return Plan::dropped(),
            };
            Plan::steps(steps)
        }
        PointerInput::Scroll { dx, dy, unit } => {
            let (unit, limit) = match unit {
                None | Some(ScrollUnit::Line) => (Unit::Line, MAX_SCROLL_LINES),
                Some(ScrollUnit::Pixel) => (Unit::Pixel, MAX_SCROLL_PIXELS),
                Some(ScrollUnit::Unknown(_)) => return Plan::dropped(),
            };
            match (finite(*dx, limit), finite(*dy, limit)) {
                (Some(0.0), Some(0.0)) => Plan::default(),
                (Some(dx), Some(dy)) => Plan::steps(vec![Step::Scroll { dx, dy, unit }]),
                _ => Plan::dropped(),
            }
        }
    }
}

/// Resolves a `key` value; `chord` is true when Ctrl, Alt or Meta is part of the frame.
fn resolve(name: &str, chord: bool) -> Option<Key> {
    if let Some(m) = Mod::from_key_name(name) {
        return Some(Key::Modifier(m));
    }
    if let Some(e) = keys::lookup(name) {
        return Some(Key::Named(e));
    }
    keys::single_char(name).map(|ch| Key::Char {
        ch,
        prefer_vk: chord,
    })
}

fn named(name: &str) -> Key {
    Key::Named(keys::lookup(name).expect("name is in the key table"))
}

/// The characters of `text` as keys, with line breaks and tabs as their named keys.
fn text_keys(text: &str) -> Vec<Key> {
    let mut out = Vec::with_capacity(text.len());
    let mut after_cr = false;
    for ch in text.chars() {
        match ch {
            '\n' if after_cr => {}
            '\r' | '\n' => out.push(named("Enter")),
            '\t' => out.push(named("Tab")),
            c if c.is_control() => {}
            c => out.push(Key::Text(c)),
        }
        after_cr = ch == '\r';
    }
    out
}

fn key(k: &KeyInput, held: &mut Held) -> Plan {
    let mut mods = Vec::with_capacity(k.mods.len());
    for m in &k.mods {
        match Mod::from_wire(m) {
            Some(m) => mods.push(m),
            None => return Plan::dropped(),
        }
    }
    // Press order is fixed, whatever order the frame lists them in.
    let mods: Vec<Mod> = Mod::ALL.into_iter().filter(|m| mods.contains(m)).collect();
    let chord = mods
        .iter()
        .any(|m| matches!(m, Mod::Ctrl | Mod::Alt | Mod::Meta));

    if let Some(text) = &k.text {
        return Plan::steps(chorded(&mods, &text_keys(text), held));
    }
    let Some(name) = &k.key else {
        return Plan::default();
    };
    let Some(key) = resolve(name, chord) else {
        return Plan::dropped();
    };
    match &k.action {
        KeyAction::Press => Plan::steps(chorded(&mods, &[key], held)),
        KeyAction::Down => {
            let mut steps = Vec::with_capacity(mods.len() + 1);
            for m in &mods {
                let item = HeldItem::Key(Key::Modifier(*m));
                if !held.contains(item) {
                    held.insert(item);
                    steps.push(Step::Key {
                        key: Key::Modifier(*m),
                        down: true,
                    });
                }
            }
            held.insert(HeldItem::Key(key));
            steps.push(Step::Key { key, down: true });
            Plan::steps(steps)
        }
        KeyAction::Up => {
            let mut steps = Vec::with_capacity(mods.len() + 1);
            if held.remove(HeldItem::Key(key)) {
                steps.push(Step::Key { key, down: false });
            }
            for m in mods.iter().rev() {
                if held.remove(HeldItem::Key(Key::Modifier(*m))) {
                    steps.push(Step::Key {
                        key: Key::Modifier(*m),
                        down: false,
                    });
                }
            }
            Plan::steps(steps)
        }
        KeyAction::Unknown(_) => Plan::dropped(),
    }
}

/// Presses the modifiers not already held, presses and releases each key, then releases
/// those modifiers in reverse. Held state is unchanged afterwards.
fn chorded(mods: &[Mod], keys: &[Key], held: &Held) -> Vec<Step> {
    if keys.is_empty() {
        return Vec::new();
    }
    let fresh: Vec<Mod> = mods
        .iter()
        .copied()
        .filter(|m| !held.contains(HeldItem::Key(Key::Modifier(*m))))
        .collect();
    let mut steps = Vec::with_capacity(fresh.len() * 2 + keys.len() * 2);
    for m in &fresh {
        steps.push(Step::Key {
            key: Key::Modifier(*m),
            down: true,
        });
    }
    for key in keys {
        // A key the phone is holding is released first so this press registers.
        if held.contains(HeldItem::Key(*key)) {
            steps.push(Step::Key {
                key: *key,
                down: false,
            });
        }
        steps.push(Step::Key {
            key: *key,
            down: true,
        });
        steps.push(Step::Key {
            key: *key,
            down: false,
        });
        if held.contains(HeldItem::Key(*key)) {
            steps.push(Step::Key {
                key: *key,
                down: true,
            });
        }
    }
    for m in fresh.iter().rev() {
        steps.push(Step::Key {
            key: Key::Modifier(*m),
            down: false,
        });
    }
    steps
}

fn presenter(action: &PresenterAction, held: &Held) -> Plan {
    let key = match action {
        PresenterAction::Next => named("ArrowRight"),
        PresenterAction::Previous => named("ArrowLeft"),
        // `b` is the usual slideshow blackout key; send the key itself on Windows too,
        // since slideshow apps read the key, not the character.
        PresenterAction::Blackout => Key::Char {
            ch: 'b',
            prefer_vk: true,
        },
        PresenterAction::PointerOn | PresenterAction::PointerOff => return Plan::default(),
        PresenterAction::Unknown(_) => return Plan::dropped(),
    };
    Plan::steps(chorded(&[], &[key], held))
}

/// Carries the fractional part of a stream of deltas so slow trackpad motion and small
/// scrolls add up instead of rounding away. One per axis and unit.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Accumulator {
    rest: f64,
}

impl Accumulator {
    /// Adds `v` and returns the whole part now due, keeping the remainder.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "inputs are clamped far below i32::MAX by expand()"
    )]
    pub(super) fn take(&mut self, v: f64) -> i32 {
        let total = self.rest + v;
        let whole = total.trunc();
        self.rest = total - whole;
        whole as i32
    }
}

#[cfg(test)]
mod tests {
    use playdrop_core::session::types::Modifier;

    use super::*;

    fn kd(key: Key) -> Step {
        Step::Key { key, down: true }
    }
    fn ku(key: Key) -> Step {
        Step::Key { key, down: false }
    }
    fn m(m: Mod) -> Key {
        Key::Modifier(m)
    }
    fn key_frame(key: &str, mods: Vec<Modifier>, action: KeyAction) -> Input {
        Input::Key(KeyInput {
            key: Some(key.into()),
            text: None,
            mods,
            action,
        })
    }
    fn button(b: PointerButton, action: ButtonAction) -> Input {
        Input::Pointer(PointerInput::Button { button: b, action })
    }
    fn bd(b: Button) -> Step {
        Step::Button {
            button: b,
            down: true,
        }
    }
    fn bu(b: Button) -> Step {
        Step::Button {
            button: b,
            down: false,
        }
    }

    #[test]
    fn press_with_mods_presses_in_order_and_releases_in_reverse() {
        let mut held = Held::default();
        // Listed out of order on purpose: press order is fixed.
        let frame = key_frame(
            "Enter",
            vec![Modifier::Shift, Modifier::Ctrl],
            KeyAction::Press,
        );
        let plan = expand(&frame, &mut held);
        let enter = named("Enter");
        assert_eq!(
            plan.steps,
            [
                kd(m(Mod::Ctrl)),
                kd(m(Mod::Shift)),
                kd(enter),
                ku(enter),
                ku(m(Mod::Shift)),
                ku(m(Mod::Ctrl)),
            ]
        );
        assert_eq!(held.len(), 0);
    }

    #[test]
    fn down_and_up_keep_held_state() {
        let mut held = Held::default();
        let down = key_frame("a", vec![Modifier::Ctrl], KeyAction::Down);
        let a = Key::Char {
            ch: 'a',
            prefer_vk: true,
        };
        assert_eq!(expand(&down, &mut held).steps, [kd(m(Mod::Ctrl)), kd(a)]);
        assert_eq!(held.len(), 2);
        // A second down does not press Ctrl again.
        assert_eq!(expand(&down, &mut held).steps, [kd(a)]);
        assert_eq!(held.len(), 2);
        let up = key_frame("a", vec![Modifier::Ctrl], KeyAction::Up);
        assert_eq!(expand(&up, &mut held).steps, [ku(a), ku(m(Mod::Ctrl))]);
        assert_eq!(held.len(), 0);
        // An up for something not held sends nothing.
        assert_eq!(expand(&up, &mut held).steps, Vec::<Step>::new());
    }

    #[test]
    fn chord_leaves_a_held_modifier_alone() {
        let mut held = Held::default();
        expand(&key_frame("Shift", vec![], KeyAction::Down), &mut held);
        let plan = expand(
            &key_frame(
                "Tab",
                vec![Modifier::Shift, Modifier::Alt],
                KeyAction::Press,
            ),
            &mut held,
        );
        let tab = named("Tab");
        assert_eq!(
            plan.steps,
            [kd(m(Mod::Alt)), kd(tab), ku(tab), ku(m(Mod::Alt))]
        );
        assert_eq!(held.release_all(), [ku(m(Mod::Shift))]);
    }

    #[test]
    fn release_all_is_reverse_press_order_and_empties() {
        let mut held = Held::default();
        expand(&button(PointerButton::Left, ButtonAction::Down), &mut held);
        expand(
            &key_frame("ArrowUp", vec![Modifier::Meta], KeyAction::Down),
            &mut held,
        );
        expand(&button(PointerButton::Right, ButtonAction::Down), &mut held);
        let up = named("ArrowUp");
        assert_eq!(
            held.release_all(),
            [
                bu(Button::Right),
                ku(up),
                ku(m(Mod::Meta)),
                bu(Button::Left)
            ]
        );
        assert_eq!(held.release_all(), Vec::<Step>::new());
    }

    #[test]
    fn click_and_double_expand() {
        let mut held = Held::default();
        let click = expand(
            &button(PointerButton::Middle, ButtonAction::Click),
            &mut held,
        );
        assert_eq!(click.steps, [bd(Button::Middle), bu(Button::Middle)]);
        let double = expand(
            &button(PointerButton::Left, ButtonAction::Double),
            &mut held,
        );
        assert_eq!(
            double.steps,
            [
                bd(Button::Left),
                bu(Button::Left),
                bd(Button::Left),
                bu(Button::Left)
            ]
        );
        assert_eq!(held.len(), 0);
        // A click while the button is held releases the hold first.
        expand(&button(PointerButton::Left, ButtonAction::Down), &mut held);
        let click = expand(&button(PointerButton::Left, ButtonAction::Click), &mut held);
        assert_eq!(
            click.steps,
            [bu(Button::Left), bd(Button::Left), bu(Button::Left)]
        );
        assert_eq!(held.len(), 0);
        // Up without down sends nothing.
        let up = expand(&button(PointerButton::Right, ButtonAction::Up), &mut held);
        assert!(up.steps.is_empty() && !up.dropped);
    }

    #[test]
    fn text_types_each_char_with_line_breaks_as_keys() {
        let mut held = Held::default();
        let frame = Input::Key(KeyInput {
            key: None,
            text: Some("hé\r\n€\t".into()),
            mods: vec![],
            action: KeyAction::Down,
        });
        let enter = named("Enter");
        let tab = named("Tab");
        assert_eq!(
            expand(&frame, &mut held).steps,
            [
                kd(Key::Text('h')),
                ku(Key::Text('h')),
                kd(Key::Text('é')),
                ku(Key::Text('é')),
                kd(enter),
                ku(enter),
                kd(Key::Text('€')),
                ku(Key::Text('€')),
                kd(tab),
                ku(tab),
            ]
        );
        assert_eq!(held.len(), 0);
    }

    #[test]
    fn unknown_values_drop_the_frame() {
        let mut held = Held::default();
        let frames = [
            key_frame("Hyper", vec![], KeyAction::Press),
            key_frame(
                "a",
                vec![Modifier::Unknown("hyper".into())],
                KeyAction::Press,
            ),
            key_frame("a", vec![], KeyAction::Unknown("tap".into())),
            button(PointerButton::Unknown("back".into()), ButtonAction::Click),
            button(PointerButton::Left, ButtonAction::Unknown("triple".into())),
            Input::Pointer(PointerInput::Scroll {
                dx: 0.0,
                dy: 1.0,
                unit: Some(ScrollUnit::Unknown("page".into())),
            }),
            Input::Pointer(PointerInput::Move {
                dx: f64::NAN,
                dy: 0.0,
            }),
            Input::Presenter {
                action: PresenterAction::Unknown("laser".into()),
            },
        ];
        for f in &frames {
            let plan = expand(f, &mut held);
            assert!(plan.dropped && plan.steps.is_empty(), "{f:?}");
        }
        assert_eq!(held.len(), 0);
    }

    #[test]
    fn pointer_moves_and_scrolls_are_clamped() {
        let mut held = Held::default();
        let mv = expand(
            &Input::Pointer(PointerInput::Move { dx: 1e9, dy: -2.5 }),
            &mut held,
        );
        assert_eq!(
            mv.steps,
            [Step::Motion {
                dx: MAX_MOVE,
                dy: -2.5
            }]
        );
        let sc = expand(
            &Input::Pointer(PointerInput::Scroll {
                dx: 0.0,
                dy: -1e6,
                unit: None,
            }),
            &mut held,
        );
        assert_eq!(
            sc.steps,
            [Step::Scroll {
                dx: 0.0,
                dy: -MAX_SCROLL_LINES,
                unit: Unit::Line
            }]
        );
        let zero = expand(
            &Input::Pointer(PointerInput::Scroll {
                dx: 0.0,
                dy: 0.0,
                unit: Some(ScrollUnit::Pixel),
            }),
            &mut held,
        );
        assert!(zero.steps.is_empty() && !zero.dropped);
    }

    #[test]
    fn presenter_maps_to_keys() {
        let mut held = Held::default();
        let p = |a| Input::Presenter { action: a };
        let right = named("ArrowRight");
        let left = named("ArrowLeft");
        let b = Key::Char {
            ch: 'b',
            prefer_vk: true,
        };
        assert_eq!(
            expand(&p(PresenterAction::Next), &mut held).steps,
            [kd(right), ku(right)]
        );
        assert_eq!(
            expand(&p(PresenterAction::Previous), &mut held).steps,
            [kd(left), ku(left)]
        );
        assert_eq!(
            expand(&p(PresenterAction::Blackout), &mut held).steps,
            [kd(b), ku(b)]
        );
        for a in [PresenterAction::PointerOn, PresenterAction::PointerOff] {
            let plan = expand(&p(a), &mut held);
            assert!(plan.steps.is_empty() && !plan.dropped);
        }
    }

    #[test]
    fn session_frames_are_ignored() {
        let mut held = Held::default();
        let plan = expand(
            &Input::Session {
                op: playdrop_core::session::types::SessionOp::Stop,
            },
            &mut held,
        );
        assert_eq!(plan, Plan::default());
    }

    #[test]
    fn char_keys_prefer_vk_only_in_chords() {
        let mut held = Held::default();
        let plain = expand(
            &key_frame("A", vec![Modifier::Shift], KeyAction::Press),
            &mut held,
        );
        assert!(plain.steps.contains(&kd(Key::Char {
            ch: 'A',
            prefer_vk: false
        })));
        let chord = expand(
            &key_frame("c", vec![Modifier::Ctrl], KeyAction::Press),
            &mut held,
        );
        assert!(chord.steps.contains(&kd(Key::Char {
            ch: 'c',
            prefer_vk: true
        })));
    }

    #[test]
    fn accumulator_carries_fractions() {
        let mut acc = Accumulator::default();
        assert_eq!(acc.take(0.4), 0);
        assert_eq!(acc.take(0.4), 0);
        assert_eq!(acc.take(0.4), 1);
        assert_eq!(acc.take(-0.7), 0);
        assert_eq!(acc.take(-0.6), -1);
        assert_eq!(acc.take(3.0), 2);
    }
}

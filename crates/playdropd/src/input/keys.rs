//! The protocol's key vocabulary (protocol.md 5.12: W3C UI Events `key` values plus
//! `ctrl`, `alt`, `shift`, `meta`) mapped to what each backend needs: X11 keysyms for the
//! portal's `NotifyKeyboardKeysym`, Windows virtual-key codes for `SendInput`.
//!
//! Every named key lives in one table ([`NAMED`]) with its keysym and virtual key side by
//! side, so a key added for one platform cannot be forgotten on the other; the tests walk
//! the table both ways. The Windows codes are plain `u16` values (not the `windows` crate
//! constants) so the table compiles and is tested on Linux too. The keysym column exists
//! only on Linux, the one target that links `xkeysym`.
//!
//! Single printable characters (`"a"`, `"A"`, `"!"`) are not in the table: on Linux they map
//! through the Unicode keysym rule (`Keysym::from_char`), so the compositor applies the
//! user's layout; on Windows they map to the US-layout virtual key that produces them
//! ([`char_vk`]). *Known limitation:* on a non-US Windows layout a punctuation key sent with
//! `ctrl`, `alt` or `meta` may land on a different key; plain characters are typed with
//! `KEYEVENTF_UNICODE` instead and are layout independent.
//!
//! Unknown names map to `None`; the backend drops the key with a debug log that carries a
//! count, never the key itself (SR-32).

use playdrop_core::session::types::Modifier;

/// One named key: its W3C UI Events `key` value and what each backend sends for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEntry {
    /// The W3C UI Events `key` value, case-sensitive as the spec writes it.
    pub name: &'static str,
    /// The X11 keysym the portal receives.
    #[cfg(target_os = "linux")]
    pub keysym: u32,
    /// The Windows virtual-key code.
    pub vk: u16,
    /// Whether `SendInput` must set `KEYEVENTF_EXTENDEDKEY` (keys with an `E0` scan code
    /// prefix: arrows, the navigation block, media keys). Without it Windows reads the
    /// numeric keypad twin of an arrow or Home key when Num Lock is off.
    pub extended: bool,
}

/// The four protocol modifiers as a `Copy` value; the wire enum also carries `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mod {
    /// `ctrl`
    Ctrl,
    /// `alt`
    Alt,
    /// `shift`
    Shift,
    /// `meta` (Super on Linux, the Windows key on Windows)
    Meta,
}

impl Mod {
    /// Every modifier, in the order a chord presses them.
    pub const ALL: [Self; 4] = [Self::Ctrl, Self::Alt, Self::Shift, Self::Meta];

    /// The modifier a wire value names; `None` for a value this version does not know.
    pub fn from_wire(m: &Modifier) -> Option<Self> {
        match m {
            Modifier::Ctrl => Some(Self::Ctrl),
            Modifier::Alt => Some(Self::Alt),
            Modifier::Shift => Some(Self::Shift),
            Modifier::Meta => Some(Self::Meta),
            Modifier::Unknown(_) => None,
        }
    }

    /// The modifier a W3C `key` value names (`Control`, `Alt`, `Shift`, `Meta`), so a key
    /// frame for a bare modifier shares held-state bookkeeping with `mods`.
    pub fn from_key_name(name: &str) -> Option<Self> {
        match name {
            "Control" => Some(Self::Ctrl),
            "Alt" => Some(Self::Alt),
            "Shift" => Some(Self::Shift),
            "Meta" => Some(Self::Meta),
            _ => None,
        }
    }
}

/// Windows virtual-key codes used by the table (winuser.h values).
#[cfg_attr(
    not(any(windows, test)),
    allow(dead_code, reason = "only Windows sends virtual keys")
)]
mod vk {
    pub const BACK: u16 = 0x08;
    pub const TAB: u16 = 0x09;
    pub const RETURN: u16 = 0x0D;
    pub const SHIFT: u16 = 0x10;
    pub const CONTROL: u16 = 0x11;
    pub const MENU: u16 = 0x12;
    pub const CAPITAL: u16 = 0x14;
    pub const ESCAPE: u16 = 0x1B;
    pub const SPACE: u16 = 0x20;
    pub const PRIOR: u16 = 0x21;
    pub const NEXT: u16 = 0x22;
    pub const END: u16 = 0x23;
    pub const HOME: u16 = 0x24;
    pub const LEFT: u16 = 0x25;
    pub const UP: u16 = 0x26;
    pub const RIGHT: u16 = 0x27;
    pub const DOWN: u16 = 0x28;
    pub const SNAPSHOT: u16 = 0x2C;
    pub const INSERT: u16 = 0x2D;
    pub const DELETE: u16 = 0x2E;
    pub const KEY_0: u16 = 0x30;
    pub const KEY_A: u16 = 0x41;
    pub const LWIN: u16 = 0x5B;
    pub const F1: u16 = 0x70;
    pub const BROWSER_BACK: u16 = 0xA6;
    pub const BROWSER_FORWARD: u16 = 0xA7;
    pub const VOLUME_MUTE: u16 = 0xAD;
    pub const VOLUME_DOWN: u16 = 0xAE;
    pub const VOLUME_UP: u16 = 0xAF;
    pub const MEDIA_NEXT_TRACK: u16 = 0xB0;
    pub const MEDIA_PREV_TRACK: u16 = 0xB1;
    pub const MEDIA_PLAY_PAUSE: u16 = 0xB3;
    pub const OEM_1: u16 = 0xBA;
    pub const OEM_PLUS: u16 = 0xBB;
    pub const OEM_COMMA: u16 = 0xBC;
    pub const OEM_MINUS: u16 = 0xBD;
    pub const OEM_PERIOD: u16 = 0xBE;
    pub const OEM_2: u16 = 0xBF;
    pub const OEM_3: u16 = 0xC0;
    pub const OEM_4: u16 = 0xDB;
    pub const OEM_5: u16 = 0xDC;
    pub const OEM_6: u16 = 0xDD;
    pub const OEM_7: u16 = 0xDE;
}

/// Builds [`NAMED`]; the keysym column is dropped off Linux.
macro_rules! key_table {
    ($( $name:literal => $keysym:ident, $vk:expr, $ext:literal; )+) => {
        /// Every named key the protocol vocabulary supports.
        pub const NAMED: &[KeyEntry] = &[
            $( KeyEntry {
                name: $name,
                #[cfg(target_os = "linux")]
                keysym: xkeysym::key::$keysym,
                vk: $vk,
                extended: $ext,
            }, )+
        ];
    };
}

key_table! {
    "Enter" => Return, vk::RETURN, false;
    "Tab" => Tab, vk::TAB, false;
    "Backspace" => BackSpace, vk::BACK, false;
    "Delete" => Delete, vk::DELETE, true;
    "Escape" => Escape, vk::ESCAPE, false;
    "Space" => space, vk::SPACE, false;
    " " => space, vk::SPACE, false;
    "ArrowUp" => Up, vk::UP, true;
    "ArrowDown" => Down, vk::DOWN, true;
    "ArrowLeft" => Left, vk::LEFT, true;
    "ArrowRight" => Right, vk::RIGHT, true;
    "Home" => Home, vk::HOME, true;
    "End" => End, vk::END, true;
    "PageUp" => Page_Up, vk::PRIOR, true;
    "PageDown" => Page_Down, vk::NEXT, true;
    "Insert" => Insert, vk::INSERT, true;
    "F1" => F1, vk::F1, false;
    "F2" => F2, vk::F1 + 1, false;
    "F3" => F3, vk::F1 + 2, false;
    "F4" => F4, vk::F1 + 3, false;
    "F5" => F5, vk::F1 + 4, false;
    "F6" => F6, vk::F1 + 5, false;
    "F7" => F7, vk::F1 + 6, false;
    "F8" => F8, vk::F1 + 7, false;
    "F9" => F9, vk::F1 + 8, false;
    "F10" => F10, vk::F1 + 9, false;
    "F11" => F11, vk::F1 + 10, false;
    "F12" => F12, vk::F1 + 11, false;
    "CapsLock" => Caps_Lock, vk::CAPITAL, false;
    "PrintScreen" => Print, vk::SNAPSHOT, true;
    "AudioVolumeUp" => XF86_AudioRaiseVolume, vk::VOLUME_UP, true;
    "AudioVolumeDown" => XF86_AudioLowerVolume, vk::VOLUME_DOWN, true;
    "AudioVolumeMute" => XF86_AudioMute, vk::VOLUME_MUTE, true;
    // XF86AudioPlay is the play/pause toggle on every Linux desktop.
    "MediaPlayPause" => XF86_AudioPlay, vk::MEDIA_PLAY_PAUSE, true;
    "MediaTrackNext" => XF86_AudioNext, vk::MEDIA_NEXT_TRACK, true;
    "MediaTrackPrevious" => XF86_AudioPrev, vk::MEDIA_PREV_TRACK, true;
    "BrowserBack" => XF86_Back, vk::BROWSER_BACK, true;
    "BrowserForward" => XF86_Forward, vk::BROWSER_FORWARD, true;
}

/// The table entry for a named key.
pub fn lookup(name: &str) -> Option<&'static KeyEntry> {
    NAMED.iter().find(|e| e.name == name)
}

/// The only character in `name` when it is exactly one printable character.
pub fn single_char(name: &str) -> Option<char> {
    let mut chars = name.chars();
    let ch = chars.next()?;
    (chars.next().is_none() && !ch.is_control()).then_some(ch)
}

/// The keysym for a `key` value: a named key, a modifier name, or one printable character
/// (Unicode keysym, so the compositor maps it through the user's layout).
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the backend resolves keys through plan.rs; kept as the table's public lookup"
    )
)]
pub fn keysym_for(name: &str) -> Option<xkeysym::Keysym> {
    if let Some(m) = Mod::from_key_name(name) {
        return Some(modifier_keysym(m));
    }
    if let Some(e) = lookup(name) {
        return Some(xkeysym::Keysym::new(e.keysym));
    }
    single_char(name).map(xkeysym::Keysym::from_char)
}

/// The virtual key and extended flag for a `key` value: a named key, a modifier name, or
/// one character the US layout produces ([`char_vk`]).
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the backends resolve keys through plan.rs; kept as the table's public lookup"
    )
)]
pub fn vk_for(name: &str) -> Option<(u16, bool)> {
    if let Some(m) = Mod::from_key_name(name) {
        return Some(modifier_vk(m));
    }
    if let Some(e) = lookup(name) {
        return Some((e.vk, e.extended));
    }
    single_char(name).and_then(char_vk).map(|vk| (vk, false))
}

/// The left-hand keysym for a modifier (`Control_L`, `Alt_L`, `Shift_L`, `Super_L`).
#[cfg(target_os = "linux")]
pub fn modifier_keysym(m: Mod) -> xkeysym::Keysym {
    use xkeysym::key;
    xkeysym::Keysym::new(match m {
        Mod::Ctrl => key::Control_L,
        Mod::Alt => key::Alt_L,
        Mod::Shift => key::Shift_L,
        Mod::Meta => key::Super_L,
    })
}

/// The virtual key and extended flag for a modifier (`VK_CONTROL`, `VK_MENU`, `VK_SHIFT`,
/// `VK_LWIN`; the Windows key has an `E0` scan code).
#[cfg_attr(
    not(any(windows, test)),
    allow(dead_code, reason = "only Windows sends virtual keys")
)]
pub fn modifier_vk(m: Mod) -> (u16, bool) {
    match m {
        Mod::Ctrl => (vk::CONTROL, false),
        Mod::Alt => (vk::MENU, false),
        Mod::Shift => (vk::SHIFT, false),
        Mod::Meta => (vk::LWIN, true),
    }
}

/// The US-layout virtual key that produces `ch` (with or without Shift), for chords such
/// as Ctrl+C where Windows needs the key, not the character. Letters map to `VK_A`..,
/// digits and their shifted symbols to `VK_0`.., punctuation to the `VK_OEM_*` codes.
#[cfg_attr(
    not(any(windows, test)),
    allow(dead_code, reason = "only Windows sends virtual keys")
)]
pub fn char_vk(ch: char) -> Option<u16> {
    let c = u8::try_from(ch).ok()?;
    let code = match c {
        b'a'..=b'z' => vk::KEY_A + u16::from(c - b'a'),
        b'A'..=b'Z' => vk::KEY_A + u16::from(c - b'A'),
        b'0'..=b'9' => vk::KEY_0 + u16::from(c - b'0'),
        b')' => vk::KEY_0,
        b'!' => vk::KEY_0 + 1,
        b'@' => vk::KEY_0 + 2,
        b'#' => vk::KEY_0 + 3,
        b'$' => vk::KEY_0 + 4,
        b'%' => vk::KEY_0 + 5,
        b'^' => vk::KEY_0 + 6,
        b'&' => vk::KEY_0 + 7,
        b'*' => vk::KEY_0 + 8,
        b'(' => vk::KEY_0 + 9,
        b' ' => vk::SPACE,
        b';' | b':' => vk::OEM_1,
        b'=' | b'+' => vk::OEM_PLUS,
        b',' | b'<' => vk::OEM_COMMA,
        b'-' | b'_' => vk::OEM_MINUS,
        b'.' | b'>' => vk::OEM_PERIOD,
        b'/' | b'?' => vk::OEM_2,
        b'`' | b'~' => vk::OEM_3,
        b'[' | b'{' => vk::OEM_4,
        b'\\' | b'|' => vk::OEM_5,
        b']' | b'}' => vk::OEM_6,
        b'\'' | b'"' => vk::OEM_7,
        _ => return None,
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_names_are_unique_and_resolve_to_themselves() {
        for (i, e) in NAMED.iter().enumerate() {
            assert!(
                NAMED[..i].iter().all(|o| o.name != e.name),
                "duplicate {}",
                e.name
            );
            assert_eq!(lookup(e.name), Some(e));
            assert_eq!(vk_for(e.name), Some((e.vk, e.extended)));
            assert_ne!(e.vk, 0, "{} has no virtual key", e.name);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn table_keysyms_walk_both_ways() {
        for e in NAMED {
            assert_ne!(e.keysym, 0, "{} has no keysym", e.name);
            assert_eq!(keysym_for(e.name).map(xkeysym::Keysym::raw), Some(e.keysym));
            // Back from keysym to name: every keysym belongs to one name, except the two
            // spellings of the space bar.
            let names: Vec<_> = NAMED
                .iter()
                .filter(|o| o.keysym == e.keysym)
                .map(|o| o.name)
                .collect();
            if e.keysym == xkeysym::key::space {
                assert_eq!(names, ["Space", " "]);
            } else {
                assert_eq!(names, [e.name]);
            }
        }
    }

    #[test]
    fn table_virtual_keys_walk_both_ways() {
        for e in NAMED {
            let names: Vec<_> = NAMED
                .iter()
                .filter(|o| o.vk == e.vk)
                .map(|o| o.name)
                .collect();
            if e.vk == vk::SPACE {
                assert_eq!(names, ["Space", " "]);
            } else {
                assert_eq!(names, [e.name]);
            }
        }
    }

    #[test]
    fn required_vocabulary_is_present() {
        let mut names = vec![
            "Enter",
            "Tab",
            "Backspace",
            "Delete",
            "Escape",
            "Space",
            " ",
            "ArrowUp",
            "ArrowDown",
            "ArrowLeft",
            "ArrowRight",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "Insert",
            "CapsLock",
            "PrintScreen",
            "AudioVolumeUp",
            "AudioVolumeDown",
            "AudioVolumeMute",
            "MediaPlayPause",
            "MediaTrackNext",
            "MediaTrackPrevious",
            "BrowserBack",
            "BrowserForward",
        ];
        let f: Vec<String> = (1..=12).map(|n| format!("F{n}")).collect();
        names.extend(f.iter().map(String::as_str));
        for n in names {
            assert!(lookup(n).is_some(), "{n} missing");
        }
    }

    #[test]
    fn spot_values() {
        assert_eq!(vk_for("Enter"), Some((0x0D, false)));
        assert_eq!(vk_for("ArrowLeft"), Some((0x25, true)));
        assert_eq!(vk_for("F12"), Some((0x7B, false)));
        assert_eq!(vk_for("MediaPlayPause"), Some((0xB3, true)));
        assert_eq!(vk_for("a"), Some((0x41, false)));
        assert_eq!(vk_for("Z"), Some((0x5A, false)));
        assert_eq!(vk_for("7"), Some((0x37, false)));
        assert_eq!(vk_for("!"), Some((0x31, false)));
        assert_eq!(vk_for("/"), Some((0xBF, false)));
        assert_eq!(vk_for("Control"), Some((0x11, false)));
        assert_eq!(vk_for("Meta"), Some((0x5B, true)));
        assert_eq!(vk_for("é"), None);
        assert_eq!(vk_for("Hyper"), None);
        assert_eq!(vk_for(""), None);
        assert_eq!(vk_for("\u{7}"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn spot_keysyms() {
        use xkeysym::key;
        let raw = |n: &str| keysym_for(n).map(xkeysym::Keysym::raw);
        assert_eq!(raw("Enter"), Some(key::Return));
        assert_eq!(raw("ArrowRight"), Some(key::Right));
        assert_eq!(raw("AudioVolumeMute"), Some(key::XF86_AudioMute));
        assert_eq!(raw("a"), Some(key::a));
        assert_eq!(raw("A"), Some(key::A));
        assert_eq!(raw("é"), Some(key::eacute));
        // Legacy keysyms win where xkb has one; the rest use the Unicode range.
        assert_eq!(raw("€"), Some(key::EuroSign));
        assert_eq!(raw("😀"), Some(0x0101_F600));
        assert_eq!(raw("Shift"), Some(key::Shift_L));
        assert_eq!(raw("Unidentified"), None);
        assert_eq!(raw(""), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn modifier_keysyms() {
        use xkeysym::key;
        let got: Vec<u32> = Mod::ALL.iter().map(|m| modifier_keysym(*m).raw()).collect();
        assert_eq!(
            got,
            [key::Control_L, key::Alt_L, key::Shift_L, key::Super_L]
        );
    }

    #[test]
    fn modifiers_from_wire_and_names() {
        assert_eq!(Mod::from_wire(&Modifier::Ctrl), Some(Mod::Ctrl));
        assert_eq!(Mod::from_wire(&Modifier::Meta), Some(Mod::Meta));
        assert_eq!(Mod::from_wire(&Modifier::Unknown("hyper".into())), None);
        assert_eq!(Mod::from_key_name("Alt"), Some(Mod::Alt));
        assert_eq!(Mod::from_key_name("alt"), None);
        let vks: Vec<_> = Mod::ALL.iter().map(|m| modifier_vk(*m)).collect();
        assert_eq!(
            vks,
            [(0x11, false), (0x12, false), (0x10, false), (0x5B, true)]
        );
    }

    #[test]
    fn every_ascii_printable_has_a_us_virtual_key() {
        for c in ' '..='~' {
            assert!(char_vk(c).is_some(), "{c:?}");
        }
        assert_eq!(char_vk('\n'), None);
        assert_eq!(char_vk('ß'), None);
    }
}

//! The keyboard: which keys go to the Mac as keys and which as text, every down followed by its up,
//! the modifier keys' own events, and everything let go when the window loses the keyboard. It follows
//! iOSClient/KeyChords.swift (`ForwardedKeys`) and TextComposition.swift (`KeyTyping`); the host
//! (KeyStrokes.swift) turns these into the Mac's events.
//!
//! The host repeats a held key when its window list says `keyRepeat` 1 or more: then one down goes
//! out, one up, and the OS's own autorepeat events are dropped. Without it (a Mac before key repeat)
//! the characters are typed as text (the OS's repeats typing again, as iPadOS's text system does) and
//! the keys that are always keys (arrows, F-keys…) go once; they do not repeat on such a Mac.

use crate::keymap::hid_usage;
use crate::mods::{flag_for_usage, ModifierMap};
use sill_proto::{modifiers, InputEvent};
use winit::keyboard::KeyCode;

/// What the session says about where typing goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Typing {
    /// Something streams; nothing else is sent (the host drops it).
    pub streaming: bool,
    /// The window list says `keyRepeat` >= 1: the Mac types the characters, as keys.
    pub host_repeats: bool,
}

/// Keys the text system never delivers, which go as keys whatever is held (`ForwardedKeys.keyUsages`).
fn always_key(usage: u16) -> bool {
    matches!(usage, 0x4F..=0x52 | 0x29 | 0x4C | 0x4A | 0x4D | 0x4B | 0x4E | 0x3A..=0x45 | 0xE0 | 0xE2 | 0xE3 | 0xE4 | 0xE6 | 0xE7)
}

/// Keys that type characters, sent as keys while the Mac types (`characterUsages`): letters, digits,
/// Return, Delete (backspace), Tab, Space, the punctuation keys every layout has.
fn character(usage: u16) -> bool {
    matches!(usage, 0x04..=0x28 | 0x2A..=0x2C | 0x2D..=0x31 | 0x33..=0x38)
}

fn goes_as_key(usage: u16, flags: u64, host_repeats: bool) -> bool {
    always_key(usage)
        || flags & (modifiers::CONTROL | modifiers::OPTION | modifiers::COMMAND) != 0
        || (matches!(usage, 0x28 | 0x2B) && flags & modifiers::SHIFT != 0)
        || (host_repeats && character(usage))
}

/// A letter typed with Caps Lock on carries Shift (unless it is a shortcut).
fn carried(usage: u16, flags: u64, caps: bool) -> u64 {
    let shortcut = flags & (modifiers::CONTROL | modifiers::OPTION | modifiers::COMMAND) != 0;
    if (0x04..=0x1D).contains(&usage) && caps && !shortcut {
        flags | modifiers::SHIFT
    } else {
        flags
    }
}

/// The text a key types, for the fallback: what winit's `KeyEvent::text` says, with Return as "\n".
fn typed(text: &str) -> Option<String> {
    let t: String = text.chars().map(|c| if c == '\r' { '\n' } else { c }).collect();
    let ok = !t.is_empty() && t.chars().all(|c| matches!(c, '\n' | '\t' | '\u{8}') || !c.is_control());
    ok.then_some(t)
}

#[derive(Clone, Debug)]
struct Held {
    code: KeyCode,
    usage: Option<u16>,
    /// Its down went to the Mac as a key; its up must.
    sent: bool,
    /// The modifier flag, for a modifier key (sent or not: shortcuts count it).
    flag: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Keyboard {
    pub map: ModifierMap,
    held: Vec<Held>,
    caps: bool,
}

impl Keyboard {
    pub fn new(map: ModifierMap) -> Keyboard {
        Keyboard { map, held: vec![], caps: false }
    }

    /// Flags of every modifier key held, including those the Mac was not sent (Shift while it does not type).
    fn all_flags(&self) -> u64 {
        self.held.iter().filter_map(|h| h.flag).fold(0, |a, f| a | f)
    }

    /// Flags of the modifier keys down on the Mac (`ForwardedKeys.held`).
    fn mac_flags(&self) -> u64 {
        self.held.iter().filter(|h| h.sent).filter_map(|h| h.flag).fold(0, |a, f| a | f)
    }

    /// Keys whose down went to the Mac and whose up has not (in the order they went down).
    pub fn down_on_mac(&self) -> Vec<u16> {
        self.held.iter().filter(|h| h.sent).filter_map(|h| h.usage).collect()
    }

    pub fn is_idle(&self) -> bool {
        self.held.is_empty()
    }

    /// A key went down. `text` is what winit says it types, `repeat` its OS autorepeat.
    pub fn press(&mut self, code: KeyCode, text: Option<&str>, repeat: bool, t: Typing) -> Vec<InputEvent> {
        if code == KeyCode::CapsLock {
            // The Mac's caps lock is never turned on: a letter typed with it carries Shift instead.
            if !repeat {
                self.caps = !self.caps;
            }
            return vec![];
        }
        if let Some(h) = self.held.iter().find(|h| h.code == code) {
            // The OS repeating a key. A key sent as a key is repeated by the host; one typed as text
            // is typed again, as iPadOS's text system types it.
            if repeat && !h.sent && h.flag.is_none() && t.streaming {
                return text.and_then(typed).map(|s| vec![InputEvent::Text(s)]).unwrap_or_default();
            }
            return vec![];
        }
        if let Some(usage) = self.map.usage(code) {
            let flag = flag_for_usage(usage);
            let sent = t.streaming && (t.host_repeats || flag != Some(modifiers::SHIFT));
            self.held.push(Held { code, usage: Some(usage), sent, flag });
            return if sent { vec![InputEvent::Key { hid_usage: usage, down: true, modifiers: self.mac_flags() }] } else { vec![] };
        }
        if !t.streaming {
            return vec![];
        }
        let usage = hid_usage(code);
        let flags = self.all_flags();
        match usage {
            Some(u) if goes_as_key(u, flags, t.host_repeats) => {
                self.held.push(Held { code, usage, sent: true, flag: None });
                vec![InputEvent::Key { hid_usage: u, down: true, modifiers: carried(u, flags, self.caps) }]
            }
            _ => match text.and_then(typed) {
                Some(s) => {
                    self.held.push(Held { code, usage, sent: false, flag: None });
                    vec![InputEvent::Text(s)]
                }
                None => vec![],
            },
        }
    }

    /// A key came up: its up, when its down went to the Mac, carrying the modifier keys still down there.
    pub fn release(&mut self, code: KeyCode) -> Vec<InputEvent> {
        let Some(i) = self.held.iter().position(|h| h.code == code) else { return vec![] };
        let h = self.held.remove(i);
        match (h.sent, h.usage) {
            (true, Some(usage)) => vec![InputEvent::Key { hid_usage: usage, down: false, modifiers: self.mac_flags() }],
            _ => vec![],
        }
    }

    /// Every key still down on the Mac goes up, the last pressed first, each with what is still held
    /// after it: focus lost, the window closing, the session ending.
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        let mut out = vec![];
        while let Some(h) = self.held.last().map(|h| h.code) {
            out.extend(self.release(h));
        }
        out
    }

    /// An input method's commit: text, once.
    pub fn ime_commit(&mut self, text: &str, t: Typing) -> Vec<InputEvent> {
        if t.streaming && !text.is_empty() {
            vec![InputEvent::Text(text.to_string())]
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use KeyCode::*;

    const KEYS: Typing = Typing { streaming: true, host_repeats: true };
    const TEXT: Typing = Typing { streaming: true, host_repeats: false };
    fn key(u: u16, down: bool, m: u64) -> InputEvent {
        InputEvent::Key { hid_usage: u, down, modifiers: m }
    }

    #[test]
    fn a_letter_is_one_down_and_one_up_and_the_oses_repeats_are_dropped() {
        let mut k = Keyboard::default();
        assert_eq!(k.press(KeyA, Some("a"), false, KEYS), vec![key(4, true, 0)]);
        assert!(k.press(KeyA, Some("a"), true, KEYS).is_empty());
        assert!(k.press(KeyA, Some("a"), true, KEYS).is_empty());
        assert_eq!(k.release(KeyA), vec![key(4, false, 0)]);
        assert!(k.is_idle());
        assert!(k.release(KeyA).is_empty());
    }

    #[test]
    fn super_c_is_command_around_c_as_a_keyboard_sends_it() {
        let mut k = Keyboard::default();
        let mut got = k.press(SuperLeft, None, false, KEYS);
        got.extend(k.press(KeyC, Some("c"), false, KEYS));
        got.extend(k.release(KeyC));
        got.extend(k.release(SuperLeft));
        let cmd = modifiers::COMMAND;
        assert_eq!(got, vec![key(0xE3, true, cmd), key(6, true, cmd), key(6, false, cmd), key(0xE3, false, 0)]);
    }

    #[test]
    fn shift_is_a_key_the_mac_hears_only_when_it_types_the_characters() {
        let mut k = Keyboard::default();
        assert_eq!(k.press(ShiftLeft, None, false, KEYS), vec![key(0xE1, true, modifiers::SHIFT)]);
        assert_eq!(k.press(KeyA, Some("A"), false, KEYS), vec![key(4, true, modifiers::SHIFT)]);
        assert_eq!(k.release(ShiftLeft), vec![key(0xE1, false, 0)]);
        assert_eq!(k.release(KeyA), vec![key(4, false, 0)]);
        // Without key repeat Shift is the text's: nothing sent, the character arrives shifted.
        let mut k = Keyboard::default();
        assert!(k.press(ShiftLeft, None, false, TEXT).is_empty());
        assert_eq!(k.press(KeyA, Some("A"), false, TEXT), vec![InputEvent::Text("A".into())]);
        assert!(k.release(ShiftLeft).is_empty());
        // ... but ⌘⇧S still carries Shift.
        k.press(ShiftLeft, None, false, TEXT);
        k.press(SuperLeft, None, false, TEXT);
        let want = key(22, true, modifiers::SHIFT | modifiers::COMMAND);
        assert_eq!(k.press(KeyS, Some("s"), false, TEXT), vec![want]);
    }

    #[test]
    fn modifier_remapping_changes_the_shortcut() {
        use crate::mods::MacMod;
        let mut k = Keyboard::new(ModifierMap { ctrl: MacMod::Command, ..Default::default() });
        assert_eq!(k.press(ControlLeft, None, false, KEYS), vec![key(0xE3, true, modifiers::COMMAND)]);
        assert_eq!(k.press(KeyV, Some("\u{16}"), false, KEYS), vec![key(25, true, modifiers::COMMAND)]);
    }

    #[test]
    fn without_key_repeat_characters_are_text_and_repeat_as_text() {
        let mut k = Keyboard::default();
        assert_eq!(k.press(KeyP, Some("p"), false, TEXT), vec![InputEvent::Text("p".into())]);
        assert_eq!(k.press(KeyP, Some("p"), true, TEXT), vec![InputEvent::Text("p".into())]);
        assert!(k.release(KeyP).is_empty());
        assert_eq!(k.press(Enter, Some("\r"), false, TEXT), vec![InputEvent::Text("\n".into())]);
        // The always-keys go once either way.
        assert_eq!(k.press(ArrowLeft, None, false, TEXT), vec![key(80, true, 0)]);
        assert!(k.press(ArrowLeft, None, true, TEXT).is_empty());
        assert_eq!(k.release(ArrowLeft), vec![key(80, false, 0)]);
        // A shortcut is a key.
        k.press(ControlLeft, None, false, TEXT);
        assert_eq!(k.press(KeyA, Some("\u{1}"), false, TEXT), vec![key(4, true, modifiers::CONTROL)]);
    }

    #[test]
    fn caps_lock_stays_local_and_capitalises_letters() {
        let mut k = Keyboard::default();
        assert!(k.press(CapsLock, None, false, KEYS).is_empty());
        assert!(k.release(CapsLock).is_empty());
        assert_eq!(k.press(KeyA, Some("A"), false, KEYS), vec![key(4, true, modifiers::SHIFT)]);
        k.release(KeyA);
        // not a shortcut's letter
        k.press(SuperLeft, None, false, KEYS);
        assert_eq!(k.press(KeyA, None, false, KEYS), vec![key(4, true, modifiers::COMMAND)]);
        k.release_all();
        k.press(CapsLock, None, false, KEYS);
        assert_eq!(k.press(KeyA, Some("a"), false, KEYS), vec![key(4, true, 0)]);
    }

    #[test]
    fn nothing_goes_down_while_nothing_streams_and_ups_still_follow() {
        let mut k = Keyboard::default();
        let idle = Typing::default();
        assert!(k.press(KeyA, Some("a"), false, idle).is_empty());
        assert!(k.release(KeyA).is_empty());
        // Held when the source stops: released by release_all.
        k.press(KeyB, Some("b"), false, KEYS);
        assert_eq!(k.release_all(), vec![key(5, false, 0)]);
    }

    #[test]
    fn release_all_lets_go_last_pressed_first_with_what_is_still_held() {
        let mut k = Keyboard::default();
        k.press(SuperLeft, None, false, KEYS);
        k.press(ShiftLeft, None, false, KEYS);
        k.press(KeyS, Some("S"), false, KEYS);
        let cmd = modifiers::COMMAND;
        assert_eq!(k.release_all(), vec![key(22, false, cmd | modifiers::SHIFT), key(0xE1, false, cmd), key(0xE3, false, 0)]);
        assert!(k.is_idle());
        assert!(k.release_all().is_empty());
    }

    #[test]
    fn keys_without_usage_or_text_send_nothing_and_ime_commits_are_text() {
        let mut k = Keyboard::default();
        assert!(k.press(AudioVolumeUp, None, false, KEYS).is_empty());
        assert!(k.press(Fn, None, false, KEYS).is_empty());
        assert_eq!(k.ime_commit("你好", KEYS), vec![InputEvent::Text("你好".into())]);
        assert!(k.ime_commit("你好", Typing::default()).is_empty());
        // The keypad is not a key the host answers to: typed as text.
        assert_eq!(k.press(Numpad5, Some("5"), false, KEYS), vec![InputEvent::Text("5".into())]);
    }

    #[test]
    fn left_and_right_modifiers_keep_the_flag_until_both_are_up() {
        let mut k = Keyboard::default();
        k.press(AltLeft, None, false, KEYS);
        assert_eq!(k.press(AltRight, None, false, KEYS), vec![key(0xE6, true, modifiers::OPTION)]);
        assert_eq!(k.release(AltLeft), vec![key(0xE2, false, modifiers::OPTION)]);
        assert_eq!(k.release(AltRight), vec![key(0xE6, false, 0)]);
    }
}

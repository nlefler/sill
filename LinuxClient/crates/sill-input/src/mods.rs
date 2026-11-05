//! Which Mac modifier a Linux modifier key stands for, and the HID usage its own key event carries.

use sill_proto::modifiers;
use winit::keyboard::KeyCode;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MacMod {
    Shift,
    Control,
    Option,
    Command,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl MacMod {
    /// The CGEventFlags bit (UIKeyModifierFlags has the same positions).
    pub fn flag(self) -> u64 {
        match self {
            MacMod::Shift => modifiers::SHIFT,
            MacMod::Control => modifiers::CONTROL,
            MacMod::Option => modifiers::OPTION,
            MacMod::Command => modifiers::COMMAND,
        }
    }

    /// The modifier's own key: left or right Control 0xE0/0xE4, Shift 0xE1/0xE5, Option 0xE2/0xE6,
    /// Command 0xE3/0xE7.
    pub fn usage(self, side: Side) -> u16 {
        let base = match self {
            MacMod::Control => 0xE0,
            MacMod::Shift => 0xE1,
            MacMod::Option => 0xE2,
            MacMod::Command => 0xE3,
        };
        base + if side == Side::Right { 4 } else { 0 }
    }
}

/// The flag a modifier key's usage (0xE0 to 0xE7) stands for.
pub fn flag_for_usage(usage: u16) -> Option<u64> {
    match usage {
        0xE0 | 0xE4 => Some(modifiers::CONTROL),
        0xE1 | 0xE5 => Some(modifiers::SHIFT),
        0xE2 | 0xE6 => Some(modifiers::OPTION),
        0xE3 | 0xE7 => Some(modifiers::COMMAND),
        _ => None,
    }
}

/// What each Linux modifier is on the Mac. Left and right keys stay left and right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModifierMap {
    pub shift: MacMod,
    pub ctrl: MacMod,
    pub alt: MacMod,
    /// The Super (logo, Windows) key.
    pub logo: MacMod,
}

impl Default for ModifierMap {
    fn default() -> Self {
        ModifierMap { shift: MacMod::Shift, ctrl: MacMod::Control, alt: MacMod::Option, logo: MacMod::Command }
    }
}

impl ModifierMap {
    /// The Mac modifier and side of a physical modifier key, `None` for any other key.
    pub fn resolve(&self, code: KeyCode) -> Option<(MacMod, Side)> {
        use KeyCode::*;
        Some(match code {
            ShiftLeft => (self.shift, Side::Left),
            ShiftRight => (self.shift, Side::Right),
            ControlLeft => (self.ctrl, Side::Left),
            ControlRight => (self.ctrl, Side::Right),
            AltLeft => (self.alt, Side::Left),
            AltRight => (self.alt, Side::Right),
            SuperLeft => (self.logo, Side::Left),
            SuperRight => (self.logo, Side::Right),
            _ => return None,
        })
    }

    /// The usage the modifier key's own event carries.
    pub fn usage(&self, code: KeyCode) -> Option<u16> {
        self.resolve(code).map(|(m, s)| m.usage(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_mac_layout() {
        let m = ModifierMap::default();
        assert_eq!(m.usage(KeyCode::SuperLeft), Some(0xE3));
        assert_eq!(m.usage(KeyCode::SuperRight), Some(0xE7));
        assert_eq!(m.usage(KeyCode::AltLeft), Some(0xE2));
        assert_eq!(m.usage(KeyCode::AltRight), Some(0xE6));
        assert_eq!(m.usage(KeyCode::ControlLeft), Some(0xE0));
        assert_eq!(m.usage(KeyCode::ShiftRight), Some(0xE5));
        assert_eq!(m.usage(KeyCode::KeyA), None);
    }

    #[test]
    fn swapping_ctrl_and_super() {
        let m = ModifierMap { ctrl: MacMod::Command, logo: MacMod::Control, ..Default::default() };
        assert_eq!(m.usage(KeyCode::ControlLeft), Some(0xE3));
        assert_eq!(m.usage(KeyCode::SuperRight), Some(0xE4));
    }

    #[test]
    fn flags_and_usages_agree() {
        for m in [MacMod::Shift, MacMod::Control, MacMod::Option, MacMod::Command] {
            for s in [Side::Left, Side::Right] {
                assert_eq!(flag_for_usage(m.usage(s)), Some(m.flag()));
            }
        }
        assert_eq!(MacMod::Command.flag(), 0x10_0000);
        assert_eq!(flag_for_usage(4), None);
    }
}

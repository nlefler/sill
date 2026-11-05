//! winit's physical keys as USB HID usages (keyboard page 0x07), the numbers kind 8's `key` carries.

use winit::keyboard::KeyCode as K;

/// Every key with a HID equivalent, once.
pub const TABLE: &[(K, u16)] = &[
    (K::KeyA, 4), (K::KeyB, 5), (K::KeyC, 6), (K::KeyD, 7), (K::KeyE, 8), (K::KeyF, 9), (K::KeyG, 10),
    (K::KeyH, 11), (K::KeyI, 12), (K::KeyJ, 13), (K::KeyK, 14), (K::KeyL, 15), (K::KeyM, 16), (K::KeyN, 17),
    (K::KeyO, 18), (K::KeyP, 19), (K::KeyQ, 20), (K::KeyR, 21), (K::KeyS, 22), (K::KeyT, 23), (K::KeyU, 24),
    (K::KeyV, 25), (K::KeyW, 26), (K::KeyX, 27), (K::KeyY, 28), (K::KeyZ, 29),
    (K::Digit1, 30), (K::Digit2, 31), (K::Digit3, 32), (K::Digit4, 33), (K::Digit5, 34), (K::Digit6, 35),
    (K::Digit7, 36), (K::Digit8, 37), (K::Digit9, 38), (K::Digit0, 39),
    (K::Enter, 40), (K::Escape, 41), (K::Backspace, 42), (K::Tab, 43), (K::Space, 44),
    (K::Minus, 45), (K::Equal, 46), (K::BracketLeft, 47), (K::BracketRight, 48), (K::Backslash, 49),
    (K::Semicolon, 51), (K::Quote, 52), (K::Backquote, 53), (K::Comma, 54), (K::Period, 55), (K::Slash, 56),
    (K::CapsLock, 57),
    (K::F1, 58), (K::F2, 59), (K::F3, 60), (K::F4, 61), (K::F5, 62), (K::F6, 63), (K::F7, 64), (K::F8, 65),
    (K::F9, 66), (K::F10, 67), (K::F11, 68), (K::F12, 69),
    (K::PrintScreen, 70), (K::ScrollLock, 71), (K::Pause, 72), (K::Insert, 73), (K::Home, 74), (K::PageUp, 75),
    (K::Delete, 76), (K::End, 77), (K::PageDown, 78),
    (K::ArrowRight, 79), (K::ArrowLeft, 80), (K::ArrowDown, 81), (K::ArrowUp, 82),
    (K::NumLock, 83), (K::NumpadDivide, 84), (K::NumpadMultiply, 85), (K::NumpadSubtract, 86),
    (K::NumpadAdd, 87), (K::NumpadEnter, 88),
    (K::Numpad1, 89), (K::Numpad2, 90), (K::Numpad3, 91), (K::Numpad4, 92), (K::Numpad5, 93), (K::Numpad6, 94),
    (K::Numpad7, 95), (K::Numpad8, 96), (K::Numpad9, 97), (K::Numpad0, 98), (K::NumpadDecimal, 99),
    (K::IntlBackslash, 100), (K::ContextMenu, 101), (K::NumpadEqual, 103),
    (K::F13, 104), (K::F14, 105), (K::F15, 106), (K::F16, 107), (K::F17, 108), (K::F18, 109), (K::F19, 110),
    (K::F20, 111), (K::F21, 112), (K::F22, 113), (K::F23, 114), (K::F24, 115),
    (K::Open, 116), (K::Help, 117), (K::Select, 119), (K::Again, 121), (K::Undo, 122), (K::Cut, 123),
    (K::Copy, 124), (K::Paste, 125), (K::Find, 126),
    (K::NumpadComma, 133), (K::IntlRo, 135), (K::KanaMode, 136), (K::IntlYen, 137), (K::Convert, 138),
    (K::NonConvert, 139), (K::Lang1, 144), (K::Lang2, 145), (K::Lang3, 146), (K::Lang4, 147), (K::Lang5, 148),
    (K::NumpadParenLeft, 182), (K::NumpadParenRight, 183), (K::NumpadBackspace, 187), (K::NumpadHash, 204),
    (K::NumpadMemoryStore, 208), (K::NumpadMemoryRecall, 209), (K::NumpadMemoryClear, 210),
    (K::NumpadMemoryAdd, 211), (K::NumpadMemorySubtract, 212), (K::NumpadClear, 216), (K::NumpadClearEntry, 217),
    (K::ControlLeft, 224), (K::ShiftLeft, 225), (K::AltLeft, 226), (K::SuperLeft, 227),
    (K::ControlRight, 228), (K::ShiftRight, 229), (K::AltRight, 230), (K::SuperRight, 231),
];

/// The HID usage of a physical key, `None` for a key with none (media keys, Fn, power…).
pub fn hid_usage(code: K) -> Option<u16> {
    TABLE.iter().find(|(c, _)| *c == code).map(|(_, u)| *u)
}

/// Keys with no usage on the keyboard page, or only an alias of another key's (`NumpadStar` is
/// `NumpadMultiply`). Together with [`TABLE`] they are every `KeyCode` winit 0.30 has.
pub const UNMAPPED: &[K] = &[
    K::Fn, K::FnLock, K::NumpadStar, K::BrowserBack, K::BrowserFavorites, K::BrowserForward, K::BrowserHome,
    K::BrowserRefresh, K::BrowserSearch, K::BrowserStop, K::Eject, K::LaunchApp1, K::LaunchApp2, K::LaunchMail,
    K::MediaPlayPause, K::MediaSelect, K::MediaStop, K::MediaTrackNext, K::MediaTrackPrevious, K::Power, K::Sleep,
    K::AudioVolumeDown, K::AudioVolumeMute, K::AudioVolumeUp, K::WakeUp, K::Meta, K::Hyper, K::Turbo, K::Abort,
    K::Resume, K::Suspend, K::Props, K::Hiragana, K::Katakana, K::F25, K::F26, K::F27, K::F28, K::F29, K::F30,
    K::F31, K::F32, K::F33, K::F34, K::F35,
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Every variant of winit 0.30's `KeyCode` (it is non-exhaustive, so the compiler cannot say).
    const ALL: &[K] = &[
        K::Backquote, K::Backslash, K::BracketLeft, K::BracketRight, K::Comma, K::Digit0, K::Digit1, K::Digit2,
        K::Digit3, K::Digit4, K::Digit5, K::Digit6, K::Digit7, K::Digit8, K::Digit9, K::Equal, K::IntlBackslash,
        K::IntlRo, K::IntlYen, K::KeyA, K::KeyB, K::KeyC, K::KeyD, K::KeyE, K::KeyF, K::KeyG, K::KeyH, K::KeyI,
        K::KeyJ, K::KeyK, K::KeyL, K::KeyM, K::KeyN, K::KeyO, K::KeyP, K::KeyQ, K::KeyR, K::KeyS, K::KeyT, K::KeyU,
        K::KeyV, K::KeyW, K::KeyX, K::KeyY, K::KeyZ, K::Minus, K::Period, K::Quote, K::Semicolon, K::Slash,
        K::AltLeft, K::AltRight, K::Backspace, K::CapsLock, K::ContextMenu, K::ControlLeft, K::ControlRight,
        K::Enter, K::SuperLeft, K::SuperRight, K::ShiftLeft, K::ShiftRight, K::Space, K::Tab, K::Convert,
        K::KanaMode, K::Lang1, K::Lang2, K::Lang3, K::Lang4, K::Lang5, K::NonConvert, K::Delete, K::End, K::Help,
        K::Home, K::Insert, K::PageDown, K::PageUp, K::ArrowDown, K::ArrowLeft, K::ArrowRight, K::ArrowUp,
        K::NumLock, K::Numpad0, K::Numpad1, K::Numpad2, K::Numpad3, K::Numpad4, K::Numpad5, K::Numpad6,
        K::Numpad7, K::Numpad8, K::Numpad9, K::NumpadAdd, K::NumpadBackspace, K::NumpadClear,
        K::NumpadClearEntry, K::NumpadComma, K::NumpadDecimal, K::NumpadDivide, K::NumpadEnter, K::NumpadEqual,
        K::NumpadHash, K::NumpadMemoryAdd, K::NumpadMemoryClear, K::NumpadMemoryRecall, K::NumpadMemoryStore,
        K::NumpadMemorySubtract, K::NumpadMultiply, K::NumpadParenLeft, K::NumpadParenRight, K::NumpadStar,
        K::NumpadSubtract, K::Escape, K::Fn, K::FnLock, K::PrintScreen, K::ScrollLock, K::Pause, K::BrowserBack,
        K::BrowserFavorites, K::BrowserForward, K::BrowserHome, K::BrowserRefresh, K::BrowserSearch,
        K::BrowserStop, K::Eject, K::LaunchApp1, K::LaunchApp2, K::LaunchMail, K::MediaPlayPause, K::MediaSelect,
        K::MediaStop, K::MediaTrackNext, K::MediaTrackPrevious, K::Power, K::Sleep, K::AudioVolumeDown,
        K::AudioVolumeMute, K::AudioVolumeUp, K::WakeUp, K::Meta, K::Hyper, K::Turbo, K::Abort, K::Resume,
        K::Suspend, K::Again, K::Copy, K::Cut, K::Find, K::Open, K::Paste, K::Props, K::Select, K::Undo,
        K::Hiragana, K::Katakana, K::F1, K::F2, K::F3, K::F4, K::F5, K::F6, K::F7, K::F8, K::F9, K::F10, K::F11,
        K::F12, K::F13, K::F14, K::F15, K::F16, K::F17, K::F18, K::F19, K::F20, K::F21, K::F22, K::F23, K::F24,
        K::F25, K::F26, K::F27, K::F28, K::F29, K::F30, K::F31, K::F32, K::F33, K::F34, K::F35,
    ];

    #[test]
    fn every_key_is_mapped_or_listed_as_having_no_usage() {
        let mapped: HashSet<K> = TABLE.iter().map(|(c, _)| *c).collect();
        let unmapped: HashSet<K> = UNMAPPED.iter().copied().collect();
        assert_eq!(mapped.len(), TABLE.len(), "a key twice in the table");
        assert_eq!(unmapped.len(), UNMAPPED.len());
        assert!(mapped.is_disjoint(&unmapped));
        for k in ALL {
            assert!(mapped.contains(k) || unmapped.contains(k), "{k:?} is neither");
        }
        assert_eq!(mapped.len() + unmapped.len(), ALL.iter().collect::<HashSet<_>>().len());
    }

    #[test]
    fn no_two_keys_share_a_usage_and_all_are_keyboard_page_numbers() {
        let usages: HashSet<u16> = TABLE.iter().map(|(_, u)| *u).collect();
        assert_eq!(usages.len(), TABLE.len());
        assert!(TABLE.iter().all(|(_, u)| (4..=231).contains(u)));
    }

    #[test]
    fn well_known_usages() {
        assert_eq!(hid_usage(K::KeyA), Some(4));
        assert_eq!(hid_usage(K::KeyZ), Some(29));
        assert_eq!(hid_usage(K::Digit1), Some(30));
        assert_eq!(hid_usage(K::Digit0), Some(39));
        assert_eq!(hid_usage(K::Enter), Some(40));
        assert_eq!(hid_usage(K::Escape), Some(41));
        assert_eq!(hid_usage(K::Space), Some(44));
        assert_eq!(hid_usage(K::F1), Some(58));
        assert_eq!(hid_usage(K::F12), Some(69));
        assert_eq!(hid_usage(K::ArrowRight), Some(79));
        assert_eq!(hid_usage(K::ArrowUp), Some(82));
        assert_eq!(hid_usage(K::SuperLeft), Some(227));
        assert_eq!(hid_usage(K::AudioVolumeUp), None);
    }
}

//! Port of `LockedMac` (Sources/StreamProtocol/Switcher.swift): the device's words for a locked Mac.

use sill_proto::WindowList;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    ScreenLocked,
    OtherUser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shade {
    Locked(Why),
    Unlocking,
}

pub struct LockedMac;

impl LockedMac {
    pub const UNLOCKING_TIMEOUT: f64 = 4.0;

    pub fn why(l: &WindowList) -> Option<Why> {
        (l.locked == Some(true)).then(|| if l.off_console == Some(true) { Why::OtherUser } else { Why::ScreenLocked })
    }

    /// The cover after a window list that says `why`: another user's while it says so; this session's own
    /// screen lock shows no cover (the Mac streams its lock screen); after a cover, "Unlocking..." until a keyframe.
    pub fn shade_after_list(was: Option<Shade>, why: Option<Why>) -> Option<Shade> {
        if why == Some(Why::OtherUser) {
            return Some(Shade::Locked(Why::OtherUser));
        }
        was.map(|_| Shade::Unlocking)
    }

    /// A keyframe ends "Unlocking...".
    pub fn shade_after_frame(was: Option<Shade>, keyframe: bool) -> Option<Shade> {
        if was == Some(Shade::Unlocking) && keyframe { None } else { was }
    }

    fn name(mac: &str) -> &str {
        if mac.is_empty() { "The Mac" } else { mac }
    }

    /// The switcher's line where the windows would be.
    pub fn line(mac: &str, why: Why) -> String {
        match why {
            Why::ScreenLocked => format!("{} is locked. Unlock it to see its windows.", Self::name(mac)),
            Why::OtherUser => "Another user started controlling this Mac. Log in to your account to reactivate screen sharing with Sill.".into(),
        }
    }

    pub fn title(mac: &str, shade: Shade) -> String {
        match shade {
            Shade::Locked(Why::ScreenLocked) => format!("{} is locked", Self::name(mac)),
            Shade::Locked(Why::OtherUser) => "Another user started controlling this Mac.".into(),
            Shade::Unlocking => "Unlocking\u{2026}".into(),
        }
    }

    pub fn detail(shade: Shade, paired: Option<bool>) -> Option<String> {
        match shade {
            Shade::Locked(Why::ScreenLocked) => Some(if paired == Some(true) {
                "To unlock it from here, type its password and press Return. Its windows come back once it is unlocked.".into()
            } else {
                "Its windows come back once it is unlocked.".into()
            }),
            Shade::Locked(Why::OtherUser) => Some("Log in to your account to reactivate screen sharing with Sill.".into()),
            Shade::Unlocking => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(locked: Option<bool>, off: Option<bool>) -> WindowList {
        WindowList { mac_name: "Mini".into(), windows: vec![], active: sill_proto::StreamSource::None, launch_id: None, host_version: None, protocol: None, gestures: None, clipboard: None, key_repeat: None, locked, off_console: off }
    }

    #[test]
    fn why_and_shade() {
        assert_eq!(LockedMac::why(&list(None, None)), None);
        assert_eq!(LockedMac::why(&list(Some(true), None)), Some(Why::ScreenLocked));
        assert_eq!(LockedMac::why(&list(Some(true), Some(true))), Some(Why::OtherUser));
        let s = LockedMac::shade_after_list(None, Some(Why::OtherUser));
        assert_eq!(s, Some(Shade::Locked(Why::OtherUser)));
        // the screen lock shows no cover; after a cover, Unlocking until a keyframe.
        assert_eq!(LockedMac::shade_after_list(None, Some(Why::ScreenLocked)), None);
        let u = LockedMac::shade_after_list(s, None);
        assert_eq!(u, Some(Shade::Unlocking));
        assert_eq!(LockedMac::shade_after_frame(u, false), u);
        assert_eq!(LockedMac::shade_after_frame(u, true), None);
    }

    #[test]
    fn words() {
        assert_eq!(LockedMac::title("Mini", Shade::Locked(Why::ScreenLocked)), "Mini is locked");
        assert_eq!(LockedMac::title("", Shade::Locked(Why::ScreenLocked)), "The Mac is locked");
        assert_eq!(LockedMac::title("Mini", Shade::Locked(Why::OtherUser)), "Another user started controlling this Mac.");
        assert!(LockedMac::line("Mini", Why::ScreenLocked).starts_with("Mini is locked."));
        assert!(LockedMac::detail(Shade::Locked(Why::ScreenLocked), Some(true)).unwrap().contains("password"));
        assert!(!LockedMac::detail(Shade::Locked(Why::ScreenLocked), None).unwrap().contains("password"));
        assert_eq!(LockedMac::detail(Shade::Unlocking, None), None);
    }
}

//! Port of `DiscoveryPolicy.autoDesktop` / `autoDesktopRecheck` plus the glue StreamClient keeps
//! (`askForDesktopByItself`, `recheckDesktop`, the closed window's wait): the device starts on the
//! Desktop, never on an empty panel, and never asks faster than every 10 s.
//!
//! * nothing watched, or the Desktop watched and the Mac stopped it: ask at once when 10 s have passed
//!   since the last automatic ask; else set one re-check for when the spacing ends, unless the person
//!   chose something since (a pick, a launch);
//! * a window watched and no longer listed: ask after `2 s + the worst round trip`, unless the person
//!   picked or launched meanwhile; a window still listed is the person's to retry (a failed pick);
//! * the re-check asks only while nothing streams still, nobody chose, and no other automatic ask went out.

use sill_proto::{StreamSource, WindowList};

pub const SPACING: f64 = 10.0;
/// The closed window's wait before the Desktop is asked for (plus the worst round trip).
pub const CLOSED_WAIT: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Watched {
    Nothing,
    Desktop,
    Window { listed: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decision {
    Ask,
    AskAfterWait,
    AskIn(f64),
    Leave,
}

/// `DiscoveryPolicy.autoDesktop`.
pub fn auto_desktop(watched: Watched, since_last_auto: f64, chose_since_last_auto: bool) -> Decision {
    let spaced = since_last_auto > SPACING;
    match watched {
        Watched::Nothing | Watched::Desktop => {
            if spaced {
                Decision::Ask
            } else if chose_since_last_auto {
                Decision::Leave
            } else {
                Decision::AskIn(SPACING.min(SPACING - since_last_auto))
            }
        }
        Watched::Window { listed } => {
            if !spaced || listed { Decision::Leave } else { Decision::AskAfterWait }
        }
    }
}

/// `DiscoveryPolicy.autoDesktopRecheck`.
pub fn recheck(nothing_streams: bool, chose_since: bool, asked_since: bool) -> bool {
    nothing_streams && !chose_since && !asked_since
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Due {
    /// The spacing's re-check.
    Recheck,
    /// The closed window's wait.
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    Nothing,
    Desktop,
    Window(u32),
}

#[derive(Clone, Debug)]
pub struct AutoDesktop {
    watched: Seen,
    last_auto: Option<f64>,
    chose_since: bool,
    nothing_streams: bool,
    due: Option<(f64, Due)>,
}

impl Default for AutoDesktop {
    fn default() -> Self {
        AutoDesktop { watched: Seen::Nothing, last_auto: None, chose_since: false, nothing_streams: true, due: None }
    }
}

impl AutoDesktop {
    pub fn new() -> AutoDesktop {
        AutoDesktop::default()
    }

    /// A new connection: nothing is watched and nothing is owed.
    pub fn reset(&mut self) {
        *self = AutoDesktop::default();
    }

    /// The person picked, launched or moved something: a pending automatic ask is off.
    pub fn chose(&mut self) {
        self.chose_since = true;
        self.due = None;
    }

    /// The Desktop was asked for by this logic at `now`.
    fn asked(&mut self, now: f64) {
        self.last_auto = Some(now);
        self.chose_since = false;
        self.due = None;
    }

    /// A window list arrived. `Some(true)` when the Desktop should be asked for right now.
    pub fn on_list(&mut self, now: f64, l: &WindowList, worst_rtt_s: Option<f64>) -> bool {
        match l.active {
            StreamSource::Desktop => {
                self.watched = Seen::Desktop;
                self.nothing_streams = false;
                self.due = None;
                false
            }
            StreamSource::Window(id) => {
                self.watched = Seen::Window(id);
                self.nothing_streams = false;
                self.due = None;
                false
            }
            StreamSource::None => {
                self.nothing_streams = true;
                let watched = match self.watched {
                    Seen::Nothing => Watched::Nothing,
                    Seen::Desktop => Watched::Desktop,
                    Seen::Window(id) => Watched::Window { listed: l.windows.iter().any(|w| w.id == id) },
                };
                let since = self.last_auto.map_or(f64::INFINITY, |t| (now - t).max(0.0));
                match auto_desktop(watched, since, self.chose_since) {
                    Decision::Ask => {
                        self.asked(now);
                        true
                    }
                    Decision::AskAfterWait => {
                        self.due = Some((now + CLOSED_WAIT + worst_rtt_s.unwrap_or(0.0), Due::Closed));
                        false
                    }
                    Decision::AskIn(s) => {
                        self.due = Some((now + s.max(0.0), Due::Recheck));
                        false
                    }
                    Decision::Leave => {
                        self.due = None;
                        false
                    }
                }
            }
        }
    }

    /// The time the next ask is due, if any.
    pub fn next_due(&self) -> Option<f64> {
        self.due.map(|(t, _)| t)
    }

    /// Call as time passes: `true` when the Desktop should be asked for now.
    pub fn poll(&mut self, now: f64) -> bool {
        let Some((t, kind)) = self.due else { return false };
        if now < t {
            return false;
        }
        self.due = None;
        let ok = match kind {
            // asked_since: another automatic ask after this was set would have cleared `due`.
            Due::Recheck => recheck(self.nothing_streams, self.chose_since, false),
            Due::Closed => self.nothing_streams && !self.chose_since,
        };
        if ok {
            self.asked(now);
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sill_proto::WindowInfo;

    fn list(active: StreamSource, windows: &[u32]) -> WindowList {
        WindowList {
            mac_name: "M".into(),
            windows: windows.iter().map(|&id| WindowInfo { id, title: "t".into(), app_name: "a".into(), bundle_id: "b".into(), width: 1, height: 1 }).collect(),
            active,
            launch_id: None, host_version: None, protocol: None, gestures: None, clipboard: None, key_repeat: None, locked: None, off_console: None,
        }
    }

    #[test]
    fn the_pure_rule_matches_the_swift_cases() {
        assert_eq!(auto_desktop(Watched::Nothing, 11.0, false), Decision::Ask);
        assert_eq!(auto_desktop(Watched::Desktop, 11.0, true), Decision::Ask);
        assert_eq!(auto_desktop(Watched::Nothing, 0.2, false), Decision::AskIn(9.8));
        assert_eq!(auto_desktop(Watched::Nothing, 3.0, true), Decision::Leave);
        assert_eq!(auto_desktop(Watched::Window { listed: true }, 11.0, false), Decision::Leave);
        assert_eq!(auto_desktop(Watched::Window { listed: false }, 11.0, false), Decision::AskAfterWait);
        assert_eq!(auto_desktop(Watched::Window { listed: false }, 5.0, false), Decision::Leave);
        assert!(recheck(true, false, false));
        assert!(!recheck(false, false, false) && !recheck(true, true, false) && !recheck(true, false, true));
    }

    #[test]
    fn asks_at_once_then_rechecks_when_the_spacing_ends() {
        let mut a = AutoDesktop::new();
        let nothing = list(StreamSource::None, &[]);
        assert!(a.on_list(100.0, &nothing, None), "the first list asks at once");
        // Lists within the spacing set one re-check at the spacing's end.
        assert!(!a.on_list(100.3, &nothing, None));
        assert!(!a.on_list(100.4, &nothing, None));
        assert_eq!(a.next_due(), Some(110.0));
        assert!(!a.poll(109.9));
        assert!(a.poll(110.0), "re-check while nothing streams still");
        // And again 10 s later, never faster.
        assert!(!a.on_list(110.1, &nothing, None));
        assert_eq!(a.next_due(), Some(120.0));
    }

    #[test]
    fn a_choice_cancels_the_recheck() {
        let mut a = AutoDesktop::new();
        let nothing = list(StreamSource::None, &[]);
        assert!(a.on_list(0.0, &nothing, None));
        a.on_list(0.5, &nothing, None);
        a.chose();
        assert_eq!(a.next_due(), None);
        assert!(!a.poll(20.0));
        // Having chosen since the last ask, a list within the spacing leaves it.
        assert!(!a.on_list(5.0, &nothing, None));
        assert_eq!(a.next_due(), None);
    }

    #[test]
    fn a_recheck_finds_something_streaming() {
        let mut a = AutoDesktop::new();
        let nothing = list(StreamSource::None, &[]);
        a.on_list(0.0, &nothing, None);
        a.on_list(1.0, &nothing, None);
        a.on_list(5.0, &list(StreamSource::Desktop, &[]), None);
        assert_eq!(a.next_due(), None, "streaming clears it");
        assert!(!a.poll(10.0));
    }

    #[test]
    fn a_closed_window_waits_two_seconds_plus_the_round_trip() {
        let mut a = AutoDesktop::new();
        // Streaming window 7, the person's pick long ago.
        a.on_list(0.0, &list(StreamSource::None, &[]), None);
        a.on_list(1.0, &list(StreamSource::Window(7), &[7]), None);
        // The window closes: nothing streams, 7 no longer listed.
        assert!(!a.on_list(30.0, &list(StreamSource::None, &[]), Some(0.5)));
        assert_eq!(a.next_due(), Some(32.5));
        assert!(a.poll(32.5));
    }

    #[test]
    fn a_pick_during_the_closed_windows_wait_cancels_it() {
        let mut a = AutoDesktop::new();
        a.on_list(1.0, &list(StreamSource::Window(7), &[7]), None);
        a.on_list(30.0, &list(StreamSource::None, &[]), None);
        a.chose();
        assert!(!a.poll(40.0));
    }

    #[test]
    fn a_window_still_listed_is_left_as_a_failed_pick() {
        let mut a = AutoDesktop::new();
        a.on_list(1.0, &list(StreamSource::Window(7), &[7]), None);
        assert!(!a.on_list(30.0, &list(StreamSource::None, &[7]), None));
        assert_eq!(a.next_due(), None);
    }
}

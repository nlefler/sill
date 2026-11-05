//! The pure controller: window intents and the host's messages in, session commands out. It owns the
//! settings ledger, the switcher model, the Desktop auto-ask and the locked-Mac cover, with time
//! injected as seconds, and publishes a [`Model`] for the window.
//!
//! Connection intents (pairing, dialing, forgetting) are the hub's: they come back as
//! [`Effect::Hub`].

use crate::auto_desktop::AutoDesktop;
use crate::clipboard::ClipboardSync;
use crate::gestures::GestureGate;
use crate::menus::MenuState;
use crate::ledger::{self, Ledger};
use crate::locked::{LockedMac, Shade};
use crate::model::*;
use crate::switcher::Switcher;
use sill_net::Command;
use sill_proto::{QUALITY_PRESETS, AppInfo, HostSettingsChange, HostSettingsState, StreamSource, WindowCommand, WindowList};

#[derive(Debug)]
pub enum Effect {
    Send(Command),
    Hub(UiIntent),
    /// Put this text on the local clipboard (the Mac's copy).
    WriteClipboard(String),
}

/// How long a notice ("kept its setting") stays up.
pub const NOTICE_SECS: f64 = 6.0;

/// `SettingsChoices` (HostSettings.swift): what a device may pick besides the bitrate presets.
pub const MAX_FPS_CHOICES: [i64; 2] = [60, 120];
pub const CAPTURE_SCALE_CHOICES: [f64; 2] = [2.0, 1.0];

#[derive(Debug)]
pub struct Controller {
    pub switcher: Switcher,
    pub ledger: Ledger,
    auto: AutoDesktop,
    shade: Option<Shade>,
    unlocking_since: Option<f64>,
    pending_pick: Option<StreamSource>,
    notice: Option<(String, f64)>,
    paired: Option<bool>,
    worst_rtt: Option<f64>,
    pub clip: ClipboardSync,
    pub menus: MenuState,
    pub gestures: GestureGate,
}

impl Default for Controller {
    fn default() -> Self {
        Controller {
            switcher: Switcher::default(),
            ledger: Ledger::default(),
            auto: AutoDesktop::default(),
            shade: None,
            unlocking_since: None,
            pending_pick: None,
            notice: None,
            paired: None,
            worst_rtt: None,
            clip: ClipboardSync::new(true),
            menus: MenuState::new(),
            gestures: GestureGate::default(),
        }
    }
}

impl Controller {
    pub fn new() -> Controller {
        Controller::default()
    }

    /// A new session: everything of the old one is forgotten (the ledger is never persisted).
    pub fn session_started(&mut self) {
        // The clipboard switch is the person's and outlives a session.
        let share = self.clip.switch_on();
        *self = Controller { clip: ClipboardSync::new(share), ..Controller::default() };
    }

    pub fn session_ended(&mut self) {
        self.session_started();
    }

    pub fn set_worst_rtt(&mut self, rtt_s: Option<f64>) {
        self.worst_rtt = rtt_s;
    }

    fn mac(&self) -> &str {
        if self.switcher.mac_name.is_empty() { "The Mac" } else { &self.switcher.mac_name }
    }

    fn say(&mut self, now: f64, text: String) {
        self.notice = Some((text, now + NOTICE_SECS));
    }

    pub fn on_window_list(&mut self, now: f64, l: &WindowList) -> Vec<Effect> {
        self.switcher.on_list(l);
        self.pending_pick = None;
        let was = self.shade;
        self.shade = LockedMac::shade_after_list(self.shade, LockedMac::why(l));
        if self.shade == Some(Shade::Unlocking) && was != Some(Shade::Unlocking) {
            self.unlocking_since = Some(now);
        }
        if self.shade.is_none() {
            self.unlocking_since = None;
        }
        let mut out = vec![];
        if let Some(c) = self.clip.on_host_list(now, l.clipboard) {
            out.push(Effect::Send(Command::Clipboard(c)));
        }
        self.gestures.on_host_list(l.gestures);
        if let Some(f) = self.menus.subscribe() {
            out.push(Effect::Send(Command::FetchMenu(f)));
        }
        // A locked Mac lists no windows but streams its lock screen (or a placeholder) for a Desktop pick, as iOS
        // asks for it: without the ask a locked Mac shows no frames.
        if self.auto.on_list(now, l, self.worst_rtt) {
            out.push(Effect::Send(Command::SelectSource(StreamSource::Desktop)));
        }
        out
    }

    pub fn on_settings(&mut self, now: f64, s: &HostSettingsState) {
        self.paired = s.session_paired;
        let refused = self.ledger.receive(s);
        if !refused.is_empty() {
            let m = format!("{} kept its setting.", self.mac());
            self.say(now, m);
        }
    }

    /// The Mac's kind 32.
    pub fn on_clipboard(&mut self, clip: &sill_proto::Clipboard) -> Vec<Effect> {
        self.clip.on_host_text(clip).map(Effect::WriteClipboard).into_iter().collect()
    }

    /// A look at the local clipboard (the app polls while [`ClipboardSync::looking`]).
    pub fn on_local_clipboard(&mut self, now: f64, text: Option<&str>) -> Vec<Effect> {
        self.clip.on_local(now, text).map(|c| Effect::Send(Command::Clipboard(c))).into_iter().collect()
    }

    /// The Mac's kind 24. `true` when the window should redraw.
    pub fn on_menu(&mut self, m: &sill_proto::MacMenu) -> bool {
        self.menus.on_menu(m)
    }

    pub fn on_thumbnail(&mut self, window: u32, jpeg: &[u8]) {
        self.switcher.on_thumbnail(window, jpeg);
    }

    pub fn on_icon(&mut self, bundle: &str, png: &[u8]) {
        self.switcher.on_icon(bundle, png);
    }

    pub fn on_apps(&mut self, apps: Vec<AppInfo>) {
        self.switcher.on_apps(apps);
    }

    /// A video frame arrived (a keyframe ends "Unlocking...").
    pub fn on_frame(&mut self, keyframe: bool) {
        if self.shade == Some(Shade::Unlocking) {
            self.shade = LockedMac::shade_after_frame(self.shade, keyframe);
            if self.shade.is_none() {
                self.unlocking_since = None;
            }
        }
    }

    /// Time passes: pending picks expire, the Desktop is asked for, "Unlocking..." gives up.
    pub fn tick(&mut self, now: f64) -> Vec<Effect> {
        let mut out = vec![];
        let timeout = ledger::pick_timeout(self.worst_rtt);
        if !self.ledger.expire(now, timeout).is_empty() {
            let m = format!("{} didn\u{2019}t answer. Try again.", self.mac());
            self.say(now, m);
        }
        if let Some((_, until)) = &self.notice {
            if now >= *until {
                self.notice = None;
            }
        }
        if self.unlocking_since.is_some_and(|t| now - t >= LockedMac::UNLOCKING_TIMEOUT) {
            self.shade = None;
            self.unlocking_since = None;
        }
        if self.auto.poll(now) {
            out.push(Effect::Send(Command::SelectSource(StreamSource::Desktop)));
        }
        out
    }

    /// When [`tick`](Self::tick) next has something to do.
    pub fn next_deadline(&self) -> Option<f64> {
        let timeout = ledger::pick_timeout(self.worst_rtt);
        [
            self.ledger.next_expiry(timeout),
            self.auto.next_due(),
            self.notice.as_ref().map(|(_, t)| *t),
            self.unlocking_since.map(|t| t + LockedMac::UNLOCKING_TIMEOUT),
        ]
        .into_iter()
        .flatten()
        .fold(None, |a: Option<f64>, t| Some(a.map_or(t, |a| a.min(t))))
    }

    pub fn intent(&mut self, now: f64, i: UiIntent) -> Vec<Effect> {
        match i {
            UiIntent::Pick(src) => {
                // No duplicate pick: the source already streaming, or one already asked for.
                if self.switcher.active == Some(src) || self.pending_pick == Some(src) {
                    return vec![];
                }
                self.pending_pick = Some(src);
                self.auto.chose();
                vec![Effect::Send(Command::SelectSource(src))]
            }
            UiIntent::Launch(bundle) => {
                self.auto.chose();
                vec![Effect::Send(Command::LaunchApp(bundle))]
            }
            UiIntent::WindowButton { id, action } => {
                if !self.switcher.windows.iter().any(|w| w.id == id) {
                    return vec![];
                }
                vec![Effect::Send(Command::WindowCommand(WindowCommand { id, action }))]
            }
            UiIntent::Setting(p) => self.setting(now, p),
            UiIntent::SetClipboard(on) => self.clip.set_switch(now, on).map(|c| Effect::Send(Command::Clipboard(c))).into_iter().collect(),
            UiIntent::OpenMenu { id, title } => self.menus.open(now, &id, &title).map(|f| Effect::Send(Command::FetchMenu(f))).into_iter().collect(),
            UiIntent::PressMenu { id, title } => self.menus.press(now, &id, &title).map(|p| Effect::Send(Command::PressMenu(p))).into_iter().collect(),
            UiIntent::Gesture { name, fingers } => self.gestures.send(now, &name, fingers).map(|g| Effect::Send(Command::Gesture(g))).into_iter().collect(),
            other => vec![Effect::Hub(other)],
        }
    }

    fn setting(&mut self, now: f64, p: SettingPick) -> Vec<Effect> {
        let mut c = HostSettingsChange::default();
        match p {
            SettingPick::Quality(b) if QUALITY_PRESETS.iter().any(|p| p.1 == b) => c.bitrate = Some(b),
            SettingPick::FrameRate(f) if MAX_FPS_CHOICES.contains(&f) => c.max_fps = Some(f),
            SettingPick::Scale(s) if CAPTURE_SCALE_CHOICES.contains(&s) => c.capture_scale = Some(s),
            SettingPick::PrioritizeSpeed(v) => c.prioritize_speed = Some(v),
            SettingPick::VirtualDisplay(v) => {
                // The panel disables the switch when the Mac cannot make one; turning it off is always allowed.
                if v && !self.ledger.host().is_some_and(|h| h.virtual_display_available) {
                    return vec![];
                }
                c.virtual_display = Some(v)
            }
            _ => return vec![],
        }
        let token = ledger::next_token();
        match self.ledger.pick(&c, token, now) {
            Some(out) => vec![Effect::Send(Command::ChangeSettings(out))],
            None => vec![],
        }
    }

    pub fn model(&self) -> Model {
        let sw = &self.switcher;
        let windows = sw
            .windows
            .iter()
            .map(|w| WindowEntry {
                id: w.id,
                title: Switcher::label(w).to_string(),
                app: w.app_name.clone(),
                bundle_id: w.bundle_id.clone(),
                width: w.width,
                height: w.height,
                thumb: sw.thumbs.get(w.id),
                icon: sw.icon(&w.bundle_id),
            })
            .collect();
        let apps = sw.apps.iter().map(|a| AppEntry { name: a.name.clone(), bundle_id: a.bundle_id.clone(), icon: sw.icon(&a.bundle_id) }).collect();
        let mac = sw.mac_name.clone();
        let title = match (sw.active_window(), mac.is_empty()) {
            (Some(w), false) => format!("{} - {}", Switcher::label(w), mac),
            (Some(w), true) => Switcher::label(w).to_string(),
            (None, false) => format!("Sill - {mac}"),
            (None, true) => "Sill".into(),
        };
        let cover = self.shade.map(|s| Cover { title: LockedMac::title(&mac, s), detail: LockedMac::detail(s, self.paired), shade: s });
        let locked_line = sw.locked.map(|w| LockedMac::line(&mac, w));
        let settings = self.ledger.host().map(|h| SettingsView {
            shown: self.ledger.displayed().unwrap_or_else(|| h.settings.clone()),
            pending: self.ledger.pending_fields(),
            persistent: h.persistent,
            virtual_display_available: h.virtual_display_available,
            virtual_display_note: h.virtual_display_note.clone(),
            software_encoder: h.software_encoder,
            running: h.stream.as_ref().map(|s| format!("{} x {}, {} fps, {} Mbps", s.width, s.height, s.fps, s.mbps)),
        });
        Model {
            conn: ConnState::Connected,
            mac_name: mac,
            active: sw.active.unwrap_or(StreamSource::None),
            windows,
            apps,
            title,
            cover,
            locked_line,
            settings,
            notice: self.notice.as_ref().map(|(t, _)| t.clone()),
            macs: vec![],
            pairing: PairingView::default(),
            clipboard: ClipboardView { offered: self.clip.offered(), on: self.clip.switch_on() },
            menus: self.menus.view().cloned(),
            gestures: self.gestures.available(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::state;
    use sill_proto::{WindowAction, WindowInfo};

    fn list(active: StreamSource, ids: &[u32]) -> WindowList {
        WindowList {
            mac_name: "Mini".into(),
            windows: ids.iter().map(|&id| WindowInfo { id, title: format!("Doc {id}"), app_name: "Notes".into(), bundle_id: "com.apple.Notes".into(), width: 800, height: 600 }).collect(),
            active,
            launch_id: None, host_version: None, protocol: None, gestures: None, clipboard: None, key_repeat: None, locked: None, off_console: None,
        }
    }

    fn sent(e: Vec<Effect>) -> Vec<Command> {
        e.into_iter().filter_map(|e| if let Effect::Send(c) = e { Some(c) } else { None }).filter(|c| !matches!(c, Command::FetchMenu(_))).collect()
    }

    #[test]
    fn the_first_list_asks_for_the_desktop_once() {
        let mut c = Controller::new();
        let e = sent(c.on_window_list(0.0, &list(StreamSource::None, &[1])));
        assert!(matches!(e.as_slice(), [Command::SelectSource(StreamSource::Desktop)]));
        assert!(sent(c.on_window_list(0.2, &list(StreamSource::None, &[1]))).is_empty(), "inside the spacing");
        assert_eq!(c.next_deadline(), Some(10.0));
        assert!(sent(c.tick(9.9)).is_empty());
        assert!(matches!(sent(c.tick(10.0)).as_slice(), [Command::SelectSource(StreamSource::Desktop)]));
    }

    #[test]
    fn a_pick_sends_kind_6_once_and_the_active_source_is_not_picked_again() {
        let mut c = Controller::new();
        c.on_window_list(0.0, &list(StreamSource::Desktop, &[1, 2]));
        let e = sent(c.intent(1.0, UiIntent::Pick(StreamSource::Window(2))));
        assert!(matches!(e.as_slice(), [Command::SelectSource(StreamSource::Window(2))]));
        assert!(sent(c.intent(1.1, UiIntent::Pick(StreamSource::Window(2)))).is_empty(), "already asked for");
        assert!(sent(c.intent(1.1, UiIntent::Pick(StreamSource::Desktop))).is_empty(), "the Desktop is the active source");
        c.on_window_list(1.5, &list(StreamSource::Window(2), &[1, 2]));
        assert!(sent(c.intent(2.0, UiIntent::Pick(StreamSource::Window(2)))).is_empty());
        assert_eq!(sent(c.intent(2.0, UiIntent::Pick(StreamSource::Desktop))).len(), 1);
    }

    #[test]
    fn launch_and_window_buttons_map_to_kinds_7_and_15() {
        let mut c = Controller::new();
        c.on_window_list(0.0, &list(StreamSource::Desktop, &[1]));
        assert!(matches!(sent(c.intent(0.0, UiIntent::Launch("com.apple.Notes".into()))).as_slice(), [Command::LaunchApp(b)] if b == "com.apple.Notes"));
        let e = sent(c.intent(0.0, UiIntent::WindowButton { id: 1, action: WindowAction::Minimize }));
        assert!(matches!(e.as_slice(), [Command::WindowCommand(w)] if w.id == 1 && w.action == WindowAction::Minimize));
        assert!(sent(c.intent(0.0, UiIntent::WindowButton { id: 99, action: WindowAction::Close })).is_empty(), "not a listed window");
    }

    #[test]
    fn settings_picks_carry_increasing_tokens_and_show_at_once() {
        let mut c = Controller::new();
        assert!(sent(c.intent(0.0, UiIntent::Setting(SettingPick::FrameRate(120)))).is_empty(), "no kind 16, nothing sent");
        c.on_settings(0.0, &state(60, 15_000_000));
        let t = |e: Vec<Effect>| match sent(e).pop() {
            Some(Command::ChangeSettings(ch)) => ch,
            other => panic!("{other:?}"),
        };
        let a = t(c.intent(1.0, UiIntent::Setting(SettingPick::FrameRate(120))));
        let b = t(c.intent(1.1, UiIntent::Setting(SettingPick::Quality(40_000_000))));
        assert!(b.token.unwrap() > a.token.unwrap());
        assert_eq!((a.max_fps, a.bitrate, b.bitrate, b.max_fps), (Some(120), None, Some(40_000_000), None));
        let m = c.model();
        assert_eq!(m.settings.as_ref().unwrap().shown.max_fps, 120);
        assert_eq!(m.settings.as_ref().unwrap().pending.len(), 2);
        // Values outside the choices are ignored.
        assert!(sent(c.intent(1.2, UiIntent::Setting(SettingPick::FrameRate(90)))).is_empty());
        assert!(sent(c.intent(1.2, UiIntent::Setting(SettingPick::Scale(3.0)))).is_empty());
    }

    #[test]
    fn a_refused_setting_goes_back_with_words() {
        let mut c = Controller::new();
        c.on_window_list(0.0, &list(StreamSource::Desktop, &[]));
        c.on_settings(0.0, &state(60, 1));
        let Some(Command::ChangeSettings(ch)) = sent(c.intent(1.0, UiIntent::Setting(SettingPick::FrameRate(120)))).pop() else { panic!() };
        let mut a = state(60, 1);
        a.answering = ch.token;
        c.on_settings(1.1, &a);
        let m = c.model();
        assert_eq!(m.settings.unwrap().shown.max_fps, 60);
        assert_eq!(m.notice.as_deref(), Some("Mini kept its setting."));
        c.tick(1.1 + NOTICE_SECS);
        assert!(c.model().notice.is_none());
    }

    #[test]
    fn an_unanswered_setting_goes_back_after_four_seconds() {
        let mut c = Controller::new();
        c.on_window_list(0.0, &list(StreamSource::Desktop, &[]));
        c.on_settings(0.0, &state(60, 1));
        c.intent(10.0, UiIntent::Setting(SettingPick::PrioritizeSpeed(true)));
        assert_eq!(c.next_deadline(), Some(14.0));
        c.tick(13.9);
        assert!(c.model().settings.unwrap().shown.prioritize_speed);
        c.tick(14.1);
        let m = c.model();
        assert!(!m.settings.unwrap().shown.prioritize_speed);
        assert_eq!(m.notice.as_deref(), Some("Mini didn\u{2019}t answer. Try again."));
        // A slow link waits longer: 4 x the worst round trip.
        let mut c = Controller::new();
        c.on_settings(0.0, &state(60, 1));
        c.set_worst_rtt(Some(2.0));
        c.intent(0.0, UiIntent::Setting(SettingPick::PrioritizeSpeed(true)));
        c.tick(7.0);
        assert!(c.model().settings.unwrap().shown.prioritize_speed);
        c.tick(8.1);
        assert!(!c.model().settings.unwrap().shown.prioritize_speed);
    }

    #[test]
    fn virtual_display_is_not_sent_when_the_mac_cannot_make_one() {
        let mut c = Controller::new();
        let mut s = state(60, 1);
        s.virtual_display_available = false;
        s.virtual_display_note = Some("Needs Screen Recording".into());
        c.on_settings(0.0, &s);
        assert!(sent(c.intent(0.0, UiIntent::Setting(SettingPick::VirtualDisplay(true)))).is_empty());
        assert_eq!(c.model().settings.unwrap().virtual_display_note.as_deref(), Some("Needs Screen Recording"));
    }

    #[test]
    fn a_session_end_forgets_the_ledger_but_not_the_token_order() {
        let mut c = Controller::new();
        c.on_settings(0.0, &state(60, 1));
        let Some(Command::ChangeSettings(a)) = sent(c.intent(0.0, UiIntent::Setting(SettingPick::FrameRate(120)))).pop() else { panic!() };
        c.session_ended();
        assert!(c.model().settings.is_none());
        c.on_settings(0.0, &state(60, 1));
        let Some(Command::ChangeSettings(b)) = sent(c.intent(0.0, UiIntent::Setting(SettingPick::FrameRate(120)))).pop() else { panic!() };
        assert!(b.token > a.token);
    }

    #[test]
    fn other_users_cover_and_the_unlocking_wait() {
        let mut c = Controller::new();
        let mut l = list(StreamSource::None, &[]);
        l.locked = Some(true);
        l.off_console = Some(true);
        assert!(sent(c.on_window_list(0.0, &l)).iter().any(|m| matches!(m, Command::SelectSource(StreamSource::Desktop))), "a locked Mac is asked for the Desktop: it streams the lock screen or a placeholder");
        let m = c.model();
        let cover = m.cover.unwrap();
        assert_eq!(cover.title, "Another user started controlling this Mac.");
        assert!(m.locked_line.unwrap().starts_with("Another user"));
        // Unlock: "Unlocking..." until a keyframe.
        c.on_window_list(5.0, &list(StreamSource::Desktop, &[]));
        assert_eq!(c.model().cover.unwrap().title, "Unlocking\u{2026}");
        c.on_frame(false);
        assert!(c.model().cover.is_some());
        c.on_frame(true);
        assert!(c.model().cover.is_none());
        // Or the wait gives up.
        c.on_window_list(6.0, &l);
        c.on_window_list(7.0, &list(StreamSource::Desktop, &[]));
        assert!(c.model().cover.is_some());
        c.tick(10.9);
        assert!(c.model().cover.is_some());
        c.tick(11.1);
        assert!(c.model().cover.is_none());
    }

    #[test]
    fn a_screen_lock_shows_the_line_but_no_cover() {
        let mut c = Controller::new();
        let mut l = list(StreamSource::Desktop, &[]);
        l.locked = Some(true);
        c.on_window_list(0.0, &l);
        let m = c.model();
        assert!(m.cover.is_none());
        assert_eq!(m.locked_line.as_deref(), Some("Mini is locked. Unlock it to see its windows."));
    }

    #[test]
    fn the_title_follows_the_streamed_window() {
        let mut c = Controller::new();
        c.on_window_list(0.0, &list(StreamSource::Window(2), &[1, 2]));
        assert_eq!(c.model().title, "Doc 2 - Mini");
        c.on_window_list(1.0, &list(StreamSource::Desktop, &[1, 2]));
        assert_eq!(c.model().title, "Sill - Mini");
    }

    #[test]
    fn an_older_mac_with_no_kind_16_has_no_settings_and_sends_none() {
        let mut c = Controller::new();
        c.on_window_list(0.0, &list(StreamSource::Desktop, &[1]));
        assert!(c.model().settings.is_none());
        assert!(sent(c.intent(0.0, UiIntent::Setting(SettingPick::Quality(40_000_000)))).is_empty());
    }

    #[test]
    fn connection_intents_go_to_the_hub() {
        let mut c = Controller::new();
        assert!(matches!(c.intent(0.0, UiIntent::PairLink("x".into())).as_slice(), [Effect::Hub(UiIntent::PairLink(_))]));
    }
}

#[cfg(test)]
mod m8_tests {
    use super::*;
    use sill_proto::{Clipboard, MacMenu, MacMenuItem, WindowInfo};

    fn list(clipboard: Option<i64>, gestures: Option<i64>) -> WindowList {
        WindowList {
            mac_name: "Mini".into(),
            windows: vec![WindowInfo { id: 1, title: "Doc".into(), app_name: "Notes".into(), bundle_id: "n".into(), width: 8, height: 6 }],
            active: StreamSource::Desktop,
            launch_id: None, host_version: None, protocol: None, gestures, clipboard, key_repeat: None, locked: None, off_console: None,
        }
    }
    fn sent(e: Vec<Effect>) -> Vec<Command> {
        e.into_iter().filter_map(|e| if let Effect::Send(c) = e { Some(c) } else { None }).collect()
    }

    #[test]
    fn the_first_list_subscribes_to_the_clipboard_and_the_menus() {
        let mut c = Controller::new();
        let e = sent(c.on_window_list(0.0, &list(Some(1), Some(1))));
        assert!(e.iter().any(|c| matches!(c, Command::Clipboard(x) if *x == Clipboard::subscription())));
        assert!(e.iter().any(|c| matches!(c, Command::FetchMenu(f) if f.id.is_none())));
        let again = sent(c.on_window_list(1.0, &list(Some(1), Some(1))));
        assert!(!again.iter().any(|c| matches!(c, Command::Clipboard(_) | Command::FetchMenu(_))), "once");
        let m = c.model();
        assert!(m.clipboard.offered && m.clipboard.on && m.gestures && m.menus.is_none());
    }

    #[test]
    fn an_older_mac_gets_no_clipboard_and_no_gestures() {
        let mut c = Controller::new();
        let e = sent(c.on_window_list(0.0, &list(None, None)));
        assert!(!e.iter().any(|c| matches!(c, Command::Clipboard(_))));
        assert!(sent(c.intent(0.0, UiIntent::Gesture { name: "swipeUp".into(), fingers: 3 })).is_empty());
        assert!(!c.model().clipboard.offered && !c.model().gestures);
    }

    #[test]
    fn copies_travel_both_ways_and_the_switch_stops_them() {
        let mut c = Controller::new();
        c.on_window_list(0.0, &list(Some(1), None));
        assert!(c.on_local_clipboard(1.0, Some("first")).is_empty(), "the first look is not sent");
        assert!(matches!(sent(c.on_local_clipboard(1.5, Some("copied"))).as_slice(), [Command::Clipboard(x)] if x.text.as_deref() == Some("copied")));
        let w = c.on_clipboard(&Clipboard::text("from the Mac").unwrap());
        assert!(matches!(w.as_slice(), [Effect::WriteClipboard(t)] if t == "from the Mac"));
        assert!(c.on_local_clipboard(2.0, Some("from the Mac")).is_empty(), "no echo");
        let stop = sent(c.intent(3.0, UiIntent::SetClipboard(false)));
        assert!(matches!(stop.as_slice(), [Command::Clipboard(x)] if x.stop == Some(true)));
        assert!(c.on_local_clipboard(4.0, Some("later")).is_empty());
        assert!(c.on_clipboard(&Clipboard::text("x").unwrap()).is_empty());
        assert!(!c.model().clipboard.on);
    }

    #[test]
    fn the_switch_outlives_a_session() {
        let mut c = Controller::new();
        c.on_window_list(0.0, &list(Some(1), None));
        c.intent(1.0, UiIntent::SetClipboard(false));
        c.session_ended();
        assert!(!c.model().clipboard.on);
        assert!(sent(c.on_window_list(0.0, &list(Some(1), None))).iter().all(|c| !matches!(c, Command::Clipboard(_))), "off: no subscription");
    }

    #[test]
    fn menus_open_and_press_through_the_controller() {
        let mut c = Controller::new();
        c.on_window_list(0.0, &list(None, None));
        let top = MacMenu { version: Some(1), app: Some("Notes".into()), menus: Some(vec![MacMenuItem { id: Some("1".into()), title: Some("File".into()), submenu: Some(true), ..Default::default() }]), ..Default::default() };
        assert!(c.on_menu(&top));
        assert!(c.model().menus.is_some());
        let f = sent(c.intent(1.0, UiIntent::OpenMenu { id: "1".into(), title: "File".into() }));
        assert!(matches!(f.as_slice(), [Command::FetchMenu(f)] if f.id.as_deref() == Some("1") && f.token == Some(1)));
        let items = MacMenu { answering: Some(1), menu: Some("1".into()), items: Some(vec![MacMenuItem { id: Some("1.0".into()), title: Some("New".into()), ..Default::default() }]), ..Default::default() };
        assert!(c.on_menu(&items));
        let p = sent(c.intent(2.0, UiIntent::PressMenu { id: "1.0".into(), title: "New".into() }));
        assert!(matches!(p.as_slice(), [Command::PressMenu(p)] if p.id.as_deref() == Some("1.0") && p.version == Some(1)));
        c.session_ended();
        assert!(c.model().menus.is_none(), "a new connection subscribes afresh");
    }

    #[test]
    fn gestures_only_when_the_list_says_so() {
        let mut c = Controller::new();
        c.on_window_list(0.0, &list(None, Some(1)));
        let g = sent(c.intent(1.0, UiIntent::Gesture { name: "swipeUp".into(), fingers: 3 }));
        assert!(matches!(g.as_slice(), [Command::Gesture(g)] if g.gesture == "swipeUp" && g.fingers == Some(3)));
        assert!(sent(c.intent(1.0, UiIntent::Gesture { name: "nonsense".into(), fingers: 3 })).is_empty());
    }
}

//! Port of `iOSClient/HostSettingsLedger.swift` (`SettingsLedger`): the Mac's last word on this
//! connection with this device's unanswered picks laid over it, field by field.
//!
//! Rules (the Swift file numbers them): no state, nothing sent (1); a pick sends only fields whose
//! value differs from what is shown (2); each sent field is held under the pick's token (3);
//! broadcasts never clear a pick, an answer clears exactly the picks carrying its token (4); a cleared
//! pick that differs from the answer's settings was refused (5); an unanswered pick goes back after the
//! timeout (6); reset on tear-down (7); a field the Mac did not report is never sent (9); Direct
//! Wireless is never sent on a session the Mac says is not paired (10).

use sill_proto::{HostSettingsChange, HostSettingsState, StreamSettings};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    MaxFps,
    Bitrate,
    CaptureScale,
    PrioritizeSpeed,
    VirtualDisplay,
    DirectWireless,
    SendAudio,
}

static TOKEN: AtomicI64 = AtomicI64::new(0);

/// The next token: strictly increasing for the life of the process (a reconnect never reuses one).
pub fn next_token() -> i64 {
    TOKEN.fetch_add(1, Ordering::SeqCst) + 1
}

/// The seconds a pick waits for its answer: `max(4, 4 x the worst recent round trip)`.
pub fn pick_timeout(worst_rtt_s: Option<f64>) -> f64 {
    (4.0f64).max(4.0 * worst_rtt_s.unwrap_or(0.0))
}

pub fn fields(c: &HostSettingsChange) -> Vec<Field> {
    let mut f = vec![];
    if c.max_fps.is_some() { f.push(Field::MaxFps) }
    if c.bitrate.is_some() { f.push(Field::Bitrate) }
    if c.capture_scale.is_some() { f.push(Field::CaptureScale) }
    if c.prioritize_speed.is_some() { f.push(Field::PrioritizeSpeed) }
    if c.virtual_display.is_some() { f.push(Field::VirtualDisplay) }
    if c.direct_wireless.is_some() { f.push(Field::DirectWireless) }
    if c.send_audio.is_some() { f.push(Field::SendAudio) }
    f
}

pub fn only(c: &HostSettingsChange, field: Field) -> HostSettingsChange {
    let mut o = HostSettingsChange::default();
    match field {
        Field::MaxFps => o.max_fps = c.max_fps,
        Field::Bitrate => o.bitrate = c.bitrate,
        Field::CaptureScale => o.capture_scale = c.capture_scale,
        Field::PrioritizeSpeed => o.prioritize_speed = c.prioritize_speed,
        Field::VirtualDisplay => o.virtual_display = c.virtual_display,
        Field::DirectWireless => o.direct_wireless = c.direct_wireless,
        Field::SendAudio => o.send_audio = c.send_audio,
    }
    o
}

pub fn is_empty(c: &HostSettingsChange) -> bool {
    fields(c).is_empty()
}

/// `HostSettingsChange.applied(to:)`.
pub fn applied(c: &HostSettingsChange, s: &StreamSettings) -> StreamSettings {
    let mut r = s.clone();
    if let Some(v) = c.max_fps { r.max_fps = v }
    if let Some(v) = c.bitrate { r.bitrate = v }
    if let Some(v) = c.capture_scale { r.capture_scale = v }
    if let Some(v) = c.prioritize_speed { r.prioritize_speed = v }
    if let Some(v) = c.virtual_display { r.virtual_display = v }
    if let Some(v) = c.direct_wireless { r.direct_wireless = Some(v) }
    if let Some(v) = c.send_audio { r.send_audio = Some(v) }
    r
}

fn adding(base: &mut HostSettingsChange, o: &HostSettingsChange) {
    base.max_fps = o.max_fps.or(base.max_fps);
    base.bitrate = o.bitrate.or(base.bitrate);
    base.capture_scale = o.capture_scale.or(base.capture_scale);
    base.prioritize_speed = o.prioritize_speed.or(base.prioritize_speed);
    base.virtual_display = o.virtual_display.or(base.virtual_display);
    base.direct_wireless = o.direct_wireless.or(base.direct_wireless);
    base.send_audio = o.send_audio.or(base.send_audio);
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub change: HostSettingsChange,
    pub token: i64,
    pub sent_at: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Ledger {
    host: Option<HostSettingsState>,
    pending: HashMap<Field, Entry>,
}

impl Ledger {
    pub fn host(&self) -> Option<&HostSettingsState> {
        self.host.as_ref()
    }

    /// `host.settings` with every pending pick applied; `None` while there is no state.
    pub fn displayed(&self) -> Option<StreamSettings> {
        let h = self.host.as_ref()?;
        let mut s = h.settings.clone();
        for e in self.pending.values() {
            s = applied(&e.change, &s);
        }
        Some(s)
    }

    pub fn pending_since(&self, f: Field) -> Option<f64> {
        self.pending.get(&f).map(|e| e.sent_at)
    }

    pub fn pending_fields(&self) -> Vec<Field> {
        self.pending.keys().copied().collect()
    }

    /// Whether this Mac takes Direct Wireless on this session (rule 10).
    pub fn takes_direct_wireless(&self) -> bool {
        self.host.as_ref().is_none_or(|h| h.session_paired != Some(false))
    }

    /// A control changed `change`'s fields: the message to send (carrying `token`), or `None`.
    pub fn pick(&mut self, change: &HostSettingsChange, token: i64, now: f64) -> Option<HostSettingsChange> {
        let shown = self.displayed()?;
        let takes_dw = self.takes_direct_wireless();
        let mut out = HostSettingsChange { token: Some(token), ..Default::default() };
        for field in fields(change) {
            if field == Field::DirectWireless && shown.direct_wireless.is_none() { continue }
            if field == Field::SendAudio && shown.send_audio.is_none() { continue }
            if field == Field::DirectWireless && !takes_dw { continue }
            let single = only(change, field);
            if applied(&single, &shown) == shown { continue }
            self.pending.insert(field, Entry { change: single.clone(), token, sent_at: now });
            adding(&mut out, &single);
        }
        (!is_empty(&out)).then_some(out)
    }

    /// A state arrived. Returns the fields this answer refused (empty for a broadcast).
    pub fn receive(&mut self, state: &HostSettingsState) -> Vec<Field> {
        let mut s = state.clone();
        s.answering = None;
        let settings = s.settings.clone();
        self.host = Some(s);
        let Some(token) = state.answering else { return vec![] };
        let mut refused = vec![];
        let ours: Vec<Field> = self.pending.iter().filter(|(_, e)| e.token == token).map(|(f, _)| *f).collect();
        for f in ours {
            let e = self.pending.remove(&f).unwrap();
            if applied(&e.change, &settings) != settings {
                refused.push(f);
            }
        }
        refused.sort_by_key(|f| *f as u8 as usize);
        refused
    }

    /// Drops the picks older than `timeout` seconds; returns their fields.
    pub fn expire(&mut self, now: f64, timeout: f64) -> Vec<Field> {
        let gone: Vec<Field> = self.pending.iter().filter(|(_, e)| now - e.sent_at > timeout).map(|(f, _)| *f).collect();
        for f in &gone {
            self.pending.remove(f);
        }
        gone
    }

    /// When the oldest pick expires.
    pub fn next_expiry(&self, timeout: f64) -> Option<f64> {
        self.pending.values().map(|e| e.sent_at + timeout).fold(None, |a: Option<f64>, t| Some(a.map_or(t, |a| a.min(t))))
    }

    pub fn reset(&mut self) {
        self.host = None;
        self.pending.clear();
    }
}

#[cfg(test)]
pub(crate) fn state(max_fps: i64, bitrate: i64) -> HostSettingsState {
    HostSettingsState {
        settings: StreamSettings { max_fps, bitrate, capture_scale: 2.0, prioritize_speed: false, virtual_display: false, direct_wireless: None, send_audio: None },
        persistent: false,
        virtual_display_available: true,
        virtual_display_note: None,
        software_encoder: false,
        stream: None,
        answering: None,
        away: None,
        link: None,
        audio_note: None,
        session_paired: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fps(v: i64) -> HostSettingsChange {
        HostSettingsChange { max_fps: Some(v), ..Default::default() }
    }

    #[test]
    fn no_state_sends_nothing() {
        let mut l = Ledger::default();
        assert!(l.pick(&fps(120), 1, 0.0).is_none());
    }

    #[test]
    fn picking_what_is_shown_sends_nothing_and_a_pick_sends_only_changed_fields() {
        let mut l = Ledger::default();
        l.receive(&state(60, 15_000_000));
        assert!(l.pick(&fps(60), 1, 0.0).is_none());
        let both = HostSettingsChange { max_fps: Some(120), bitrate: Some(15_000_000), ..Default::default() };
        let sent = l.pick(&both, 2, 0.0).unwrap();
        assert_eq!((sent.token, sent.max_fps, sent.bitrate), (Some(2), Some(120), None));
        assert_eq!(l.displayed().unwrap().max_fps, 120);
        // The same pick again is what is shown now.
        assert!(l.pick(&fps(120), 3, 0.1).is_none());
    }

    #[test]
    fn an_answer_clears_only_its_token() {
        // 60 -> 120 -> 60: the answer to the first finds the field held by the third.
        let mut l = Ledger::default();
        l.receive(&state(60, 1));
        l.pick(&fps(120), 1, 0.0).unwrap();
        l.pick(&fps(60), 2, 0.1).unwrap();
        let mut a = state(120, 1);
        a.answering = Some(1);
        assert!(l.receive(&a).is_empty(), "token 1 matches nothing now");
        assert_eq!(l.displayed().unwrap().max_fps, 60, "never shows 120");
        // A broadcast never clears.
        l.receive(&state(120, 1));
        assert_eq!(l.pending_fields(), vec![Field::MaxFps]);
        let mut a = state(60, 1);
        a.answering = Some(2);
        assert!(l.receive(&a).is_empty());
        assert!(l.pending_fields().is_empty());
    }

    #[test]
    fn a_refused_pick_goes_back() {
        let mut l = Ledger::default();
        l.receive(&state(60, 1));
        l.pick(&fps(120), 7, 0.0).unwrap();
        let mut a = state(60, 1);
        a.answering = Some(7);
        assert_eq!(l.receive(&a), vec![Field::MaxFps]);
        assert_eq!(l.displayed().unwrap().max_fps, 60);
    }

    #[test]
    fn an_unanswered_pick_expires_after_the_timeout_and_a_late_answer_still_applies() {
        let mut l = Ledger::default();
        l.receive(&state(60, 1));
        l.pick(&fps(120), 1, 10.0).unwrap();
        assert_eq!(l.next_expiry(4.0), Some(14.0));
        assert!(l.expire(13.9, 4.0).is_empty());
        assert_eq!(l.expire(14.1, 4.0), vec![Field::MaxFps]);
        assert_eq!(l.displayed().unwrap().max_fps, 60);
        let mut a = state(120, 1);
        a.answering = Some(1);
        l.receive(&a);
        assert_eq!(l.displayed().unwrap().max_fps, 120);
    }

    #[test]
    fn timeout_follows_the_round_trip() {
        assert_eq!(pick_timeout(None), 4.0);
        assert_eq!(pick_timeout(Some(0.5)), 4.0);
        assert_eq!(pick_timeout(Some(2.0)), 8.0);
    }

    #[test]
    fn fields_the_mac_did_not_report_are_never_sent() {
        let mut l = Ledger::default();
        l.receive(&state(60, 1));
        let c = HostSettingsChange { direct_wireless: Some(true), send_audio: Some(true), ..Default::default() };
        assert!(l.pick(&c, 1, 0.0).is_none());
        let mut s = state(60, 1);
        s.settings.direct_wireless = Some(false);
        s.session_paired = Some(false);
        l.receive(&s);
        let c = HostSettingsChange { direct_wireless: Some(true), ..Default::default() };
        assert!(l.pick(&c, 2, 0.0).is_none(), "rule 10");
    }

    #[test]
    fn reset_forgets_everything_and_tokens_only_grow() {
        let mut l = Ledger::default();
        l.receive(&state(60, 1));
        l.pick(&fps(120), 1, 0.0);
        l.reset();
        assert!(l.displayed().is_none() && l.pending_fields().is_empty());
        let (a, b) = (next_token(), next_token());
        assert!(b > a);
    }
}

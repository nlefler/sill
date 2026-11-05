//! The device's side of the clipboard (kind 32): ports of `ClipboardPolicy`'s device rules. Pure, time
//! injected. A subscription is sent when the Mac's window list says `clipboard` 1 or more and the switch
//! is on, a stop when the switch goes off; text goes to the Mac when the local clipboard changes (the
//! first look only records what is there, as the host never sends what was on the clipboard when the
//! first device subscribed); the Mac's text is written locally and recorded as seen, so the next look
//! never sends it back.

use sill_proto::Clipboard;

/// The host rates subscriptions and texts at 4 a second per connection.
pub const PER_SECOND: usize = 4;
/// How often the app looks at the local clipboard (`ClipboardPolicy.lookInterval`).
pub const LOOK_INTERVAL: f64 = 0.25;

/// `ClipboardPolicy.hostTakes`.
pub fn host_takes(host_clipboard: Option<i64>) -> bool {
    host_clipboard.unwrap_or(0) >= Clipboard::GENERATION
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subscription {
    Subscribe,
    Stop,
    None,
}

/// `ClipboardPolicy.deviceSubscription`: what to send and the new subscribed state.
pub fn device_subscription(host_clipboard: Option<i64>, switch_on: bool, subscribed: bool) -> (Subscription, bool) {
    if !host_takes(host_clipboard) {
        return (Subscription::None, false);
    }
    if switch_on && !subscribed {
        return (Subscription::Subscribe, true);
    }
    if !switch_on && subscribed {
        return (Subscription::Stop, false);
    }
    (Subscription::None, subscribed)
}

/// `ClipboardPolicy.admits`: at most [`PER_SECOND`] in any second.
pub fn admits(arrivals: &[f64], now: f64) -> (bool, Vec<f64>) {
    let mut recent: Vec<f64> = arrivals.iter().copied().filter(|t| now - t < 1.0).collect();
    if recent.len() >= PER_SECOND {
        return (false, recent);
    }
    recent.push(now);
    (true, recent)
}

#[derive(Debug)]
pub struct ClipboardSync {
    switch_on: bool,
    host: Option<i64>,
    subscribed: bool,
    /// The local clipboard text as last looked at; `None` before the first look.
    observed: Option<Option<String>>,
    arrivals: Vec<f64>,
}

impl ClipboardSync {
    pub fn new(switch_on: bool) -> ClipboardSync {
        ClipboardSync { switch_on, host: None, subscribed: false, observed: None, arrivals: vec![] }
    }

    pub fn switch_on(&self) -> bool {
        self.switch_on
    }

    /// The Mac offers the clipboard (the panel shows the switch).
    pub fn offered(&self) -> bool {
        host_takes(self.host)
    }

    /// Whether the app should be looking at the local clipboard.
    pub fn looking(&self) -> bool {
        self.switch_on && self.subscribed
    }

    fn subscription_message(&mut self, now: f64) -> Option<Clipboard> {
        let (send, subscribed) = device_subscription(self.host, self.switch_on, self.subscribed);
        self.subscribed = subscribed;
        match send {
            Subscription::Subscribe => {
                // A stop is never rated; a subscription is.
                let (ok, kept) = admits(&self.arrivals, now);
                self.arrivals = kept;
                if !ok {
                    self.subscribed = false; // try again at the next call
                    return None;
                }
                self.observed = None; // what is there now is not sent
                Some(Clipboard::subscription())
            }
            Subscription::Stop => Some(Clipboard::stopping()),
            Subscription::None => None,
        }
    }

    /// The window list's `clipboard`.
    pub fn on_host_list(&mut self, now: f64, host_clipboard: Option<i64>) -> Option<Clipboard> {
        self.host = host_clipboard;
        self.subscription_message(now)
    }

    /// The switch in Settings.
    pub fn set_switch(&mut self, now: f64, on: bool) -> Option<Clipboard> {
        self.switch_on = on;
        self.subscription_message(now)
    }

    /// A look at the local clipboard (`None`: no text on it). Returns the message to send when the
    /// text is new, valid (non-empty, at most 256 KB) and the rate admits it.
    pub fn on_local(&mut self, now: f64, text: Option<&str>) -> Option<Clipboard> {
        if !self.looking() {
            return None;
        }
        let cur = text.map(str::to_string);
        match &self.observed {
            None => {
                self.observed = Some(cur);
                return None;
            }
            Some(seen) if *seen == cur => return None,
            _ => {}
        }
        let Some(t) = cur.as_deref() else {
            self.observed = Some(cur);
            return None;
        };
        let Some(msg) = Clipboard::text(t) else {
            // Empty or too long: never sent, and not looked at again until it changes.
            self.observed = Some(cur);
            return None;
        };
        let (ok, kept) = admits(&self.arrivals, now);
        self.arrivals = kept;
        if !ok {
            return None; // seen again at the next look
        }
        self.observed = Some(cur);
        Some(msg)
    }

    /// The Mac's kind 32. `Some(text)` to write to the local clipboard (`ClipboardPolicy.deviceWrites`).
    pub fn on_host_text(&mut self, clip: &Clipboard) -> Option<String> {
        if !self.switch_on || !host_takes(self.host) || !self.subscribed {
            return None;
        }
        let text = clip.text.as_deref().filter(|t| !t.is_empty() && t.len() <= Clipboard::MAX_TEXT_BYTES)?;
        // Recorded as seen: the next look finds it and sends nothing.
        self.observed = Some(Some(text.to_string()));
        Some(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> ClipboardSync {
        let mut c = ClipboardSync::new(true);
        assert!(c.on_host_list(0.0, Some(1)).is_some());
        c
    }

    #[test]
    fn subscription_rules() {
        assert_eq!(device_subscription(None, true, false), (Subscription::None, false));
        assert_eq!(device_subscription(Some(0), true, true), (Subscription::None, false));
        assert_eq!(device_subscription(Some(1), true, false), (Subscription::Subscribe, true));
        assert_eq!(device_subscription(Some(1), true, true), (Subscription::None, true));
        assert_eq!(device_subscription(Some(2), false, true), (Subscription::Stop, false));
        assert_eq!(device_subscription(Some(1), false, false), (Subscription::None, false));
    }

    #[test]
    fn subscribes_once_and_stops_when_the_switch_goes_off() {
        let mut c = ClipboardSync::new(true);
        assert!(c.on_host_list(0.0, None).is_none(), "a Mac without the clipboard");
        assert_eq!(c.on_host_list(0.0, Some(1)), Some(Clipboard::subscription()));
        assert!(c.on_host_list(1.0, Some(1)).is_none(), "every list says it; one subscription");
        assert_eq!(c.set_switch(2.0, false), Some(Clipboard::stopping()));
        assert!(!c.looking());
        assert!(c.set_switch(2.1, false).is_none());
        assert_eq!(c.set_switch(3.0, true), Some(Clipboard::subscription()));
    }

    #[test]
    fn a_switch_that_starts_off_sends_nothing() {
        let mut c = ClipboardSync::new(false);
        assert!(c.on_host_list(0.0, Some(1)).is_none());
        assert!(c.on_local(0.0, Some("x")).is_none());
        assert!(c.on_host_text(&Clipboard::text("y").unwrap()).is_none());
        assert_eq!(c.set_switch(1.0, true), Some(Clipboard::subscription()));
    }

    #[test]
    fn the_first_look_is_not_sent_changes_are() {
        let mut c = on();
        assert!(c.on_local(1.0, Some("old")).is_none());
        assert!(c.on_local(1.25, Some("old")).is_none());
        assert_eq!(c.on_local(1.5, Some("new")), Clipboard::text("new"));
        assert!(c.on_local(1.75, Some("new")).is_none(), "once");
        assert!(c.on_local(2.0, None).is_none(), "cleared or not text: nothing");
        assert_eq!(c.on_local(2.25, Some("new")), Clipboard::text("new"), "copied again after a gap");
    }

    #[test]
    fn the_hosts_own_text_is_never_sent_back() {
        let mut c = on();
        c.on_local(1.0, Some("mine"));
        let w = c.on_host_text(&Clipboard::text("theirs").unwrap());
        assert_eq!(w.as_deref(), Some("theirs"));
        assert!(c.on_local(1.25, Some("theirs")).is_none(), "the write is found and not echoed");
        assert_eq!(c.on_local(1.5, Some("mine again")), Clipboard::text("mine again"));
    }

    #[test]
    fn host_text_is_held_to_the_rules() {
        let mut c = on();
        assert!(c.on_host_text(&Clipboard::default()).is_none());
        assert!(c.on_host_text(&Clipboard::stopping()).is_none());
        let big = Clipboard { text: Some("x".repeat(Clipboard::MAX_TEXT_BYTES + 1)), ..Default::default() };
        assert!(c.on_host_text(&big).is_none());
        c.set_switch(1.0, false);
        assert!(c.on_host_text(&Clipboard::text("a").unwrap()).is_none(), "switch off: written nowhere");
    }

    #[test]
    fn four_a_second_and_a_refused_one_is_seen_again() {
        let mut c = on(); // the subscription took one of the second's four
        c.on_local(0.0, Some("a"));
        let mut sent = 0;
        for (i, t) in ["b", "c", "d", "e", "f"].iter().enumerate() {
            if c.on_local(0.1 + i as f64 * 0.01, Some(t)).is_some() {
                sent += 1;
            }
        }
        assert_eq!(sent, 3, "4 per second including the subscription");
        // The refused text goes out once the second is over.
        assert_eq!(c.on_local(1.2, Some("f")), Clipboard::text("f"));
    }

    #[test]
    fn oversize_and_empty_text_are_never_sent() {
        let mut c = on();
        c.on_local(0.0, Some("a"));
        assert!(c.on_local(1.0, Some("")).is_none());
        assert!(c.on_local(2.0, Some(&"x".repeat(Clipboard::MAX_TEXT_BYTES + 1))).is_none());
        assert!(c.on_local(3.0, Some(&"x".repeat(Clipboard::MAX_TEXT_BYTES + 1))).is_none());
        assert!(c.on_local(4.0, Some(&"x".repeat(Clipboard::MAX_TEXT_BYTES))).is_some());
    }

    #[test]
    fn a_new_subscription_forgets_what_was_there() {
        let mut c = on();
        c.on_local(0.0, Some("a"));
        c.set_switch(1.0, false);
        c.set_switch(2.0, true);
        assert!(c.on_local(2.1, Some("b")).is_none(), "copied while off: not sent");
        assert!(c.on_local(2.4, Some("c")).is_some());
    }
}

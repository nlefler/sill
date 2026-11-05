//! The automatic reconnect: a pure state machine ([`Supervisor`], time injected as seconds) and a
//! thin async driver ([`reconnect`]).

use crate::connect::ConnectOutcome;
use crate::policy::{quit_hold, reconnect_line, redial_row, Redial};
use sill_net::session::{CloseReason, Session};
use std::future::Future;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// `StreamClient.reconnectDialWait`: a dial not ready in 5 s is let go.
pub const DIAL_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Poll {
    /// Dial the listed row (or, after a tap, the saved addresses) now.
    Dial,
    /// Look again at this time.
    WaitUntil(f64),
    /// Nothing to do until something changes.
    Idle,
}

#[derive(Debug)]
pub struct Supervisor {
    mac: String,
    foreground: bool,
    wanted: bool,
    failed: u32,
    last_dial_at: Option<f64>,
    dialing: bool,
    quit_at: Option<f64>,
    row_since: Option<f64>,
    forced: bool,
}

impl Supervisor {
    pub fn new(mac_name: &str) -> Supervisor {
        Supervisor {
            mac: mac_name.into(),
            foreground: true,
            wanted: false,
            failed: 0,
            last_dial_at: None,
            dialing: false,
            quit_at: None,
            row_since: None,
            forced: false,
        }
    }

    /// The session ended at `now`. Whether a reconnect starts follows `CloseReason::reconnects`
    /// (removed, pairingRequired, update notices and a close by this side never reconnect).
    pub fn session_ended(&mut self, now: f64, reason: &CloseReason) {
        self.wanted = reason.reconnects();
        self.failed = 0;
        self.last_dial_at = None;
        self.dialing = false;
        self.forced = false;
        self.quit_at = match reason {
            CloseReason::Goodbye(g) if g.reason == "quit" => Some(now),
            _ => None,
        };
    }

    /// The row of this Mac is listed (afresh) at `now`: the count starts again.
    pub fn row_listed(&mut self, now: f64) {
        if self.row_since.is_none() {
            self.row_since = Some(now);
            self.failed = 0;
            self.last_dial_at = None;
        }
    }

    pub fn row_gone(&mut self) {
        self.row_since = None;
    }

    /// The app came forward (count starts again) or went away (no dials).
    pub fn set_foreground(&mut self, fg: bool) {
        if fg && !self.foreground {
            self.failed = 0;
            self.last_dial_at = None;
        }
        self.foreground = fg;
    }

    /// The person tapped the Mac: try now, from the start, row listed or not.
    pub fn tap(&mut self, _now: f64) {
        self.wanted = true;
        self.failed = 0;
        self.last_dial_at = None;
        self.quit_at = None;
        self.forced = true;
    }

    pub fn poll(&self, now: f64) -> Poll {
        if !self.wanted || self.dialing {
            return Poll::Idle;
        }
        if self.forced {
            return Poll::Dial;
        }
        let Some(since) = self.row_since else { return Poll::Idle };
        if let Some(t) = quit_hold(self.quit_at, since, now) {
            return Poll::WaitUntil(t);
        }
        match redial_row(self.failed, self.last_dial_at, self.foreground, now) {
            Redial::Now => Poll::Dial,
            Redial::At(t) => Poll::WaitUntil(t),
            Redial::No => Poll::Idle,
        }
    }

    pub fn dial_started(&mut self, now: f64) {
        self.dialing = true;
        self.forced = false;
        self.last_dial_at = Some(now);
    }

    /// A dial that did not connect (including one let go after `DIAL_WAIT`).
    pub fn dial_failed(&mut self) {
        self.dialing = false;
        self.failed += 1;
    }

    /// Connected, or ended for good (a new key): nothing more to do.
    pub fn done(&mut self) {
        self.dialing = false;
        self.wanted = false;
    }

    pub fn failed(&self) -> u32 {
        self.failed
    }

    /// The status line while a reconnect is wanted.
    pub fn line(&self) -> Option<String> {
        self.wanted.then(|| reconnect_line(&self.mac, self.failed))
    }
}

pub enum ReconnectEnd {
    Connected { session: Session, address: (String, u16) },
    /// The Mac answered with another key: stop, pair again.
    NewKey,
    NotAllowed,
    /// An input channel closed.
    Cancelled,
}

/// Inputs of the driver.
pub struct ReconnectInputs {
    /// The row's dialable addresses, `None` while the Mac is not listed.
    pub row: watch::Receiver<Option<Vec<(String, u16)>>>,
    pub foreground: watch::Receiver<bool>,
    /// A tap on the Mac.
    pub taps: mpsc::Receiver<()>,
    /// Status lines, sent whenever the line changes.
    pub lines: mpsc::UnboundedSender<String>,
}

/// Drive `sup` (after `session_ended`) until a dial connects or ends for good. `dial` gets the
/// listed addresses (empty after a tap with no row). Time is tokio's clock, so tests can pause it.
pub async fn reconnect<D, Fut>(mut sup: Supervisor, mut inp: ReconnectInputs, mut dial: D) -> ReconnectEnd
where
    D: FnMut(Vec<(String, u16)>) -> Fut,
    Fut: Future<Output = ConnectOutcome>,
{
    let t0 = tokio::time::Instant::now();
    let now = |_: ()| t0.elapsed().as_secs_f64();
    let mut last_line = None::<String>;
    let mut taps_open = true;
    sup.set_foreground(*inp.foreground.borrow());
    if inp.row.borrow().is_some() {
        sup.row_listed(now(()));
    }
    loop {
        if let Some(l) = sup.line() {
            if last_line.as_ref() != Some(&l) {
                let _ = inp.lines.send(l.clone());
                last_line = Some(l);
            }
        }
        match sup.poll(now(())) {
            Poll::Dial => {
                let addrs = inp.row.borrow().clone().unwrap_or_default();
                sup.dial_started(now(()));
                let out = tokio::time::timeout(DIAL_WAIT, dial(addrs)).await;
                match out {
                    Ok(ConnectOutcome::Connected { session, address }) => {
                        sup.done();
                        return ReconnectEnd::Connected { session, address };
                    }
                    Ok(ConnectOutcome::NewKey { .. }) => {
                        sup.done();
                        return ReconnectEnd::NewKey;
                    }
                    Ok(ConnectOutcome::NotAllowed) => {
                        sup.done();
                        return ReconnectEnd::NotAllowed;
                    }
                    Ok(ConnectOutcome::Unreachable(_)) | Err(_) => sup.dial_failed(),
                }
            }
            p => {
                let wake = async {
                    match p {
                        Poll::WaitUntil(t) => {
                            tokio::time::sleep_until(t0 + Duration::from_secs_f64(t.max(0.0))).await
                        }
                        _ => std::future::pending().await,
                    }
                };
                tokio::select! {
                    r = inp.row.changed() => {
                        if r.is_err() { return ReconnectEnd::Cancelled }
                        if inp.row.borrow_and_update().is_some() { sup.row_listed(now(())) } else { sup.row_gone() }
                    }
                    r = inp.foreground.changed() => {
                        if r.is_err() { return ReconnectEnd::Cancelled }
                        let fg = *inp.foreground.borrow_and_update();
                        sup.set_foreground(fg);
                    }
                    t = inp.taps.recv(), if taps_open => {
                        match t { Some(()) => { sup.tap(now(())); last_line = None; } None => taps_open = false }
                    }
                    _ = wake => {}
                }
            }
        }
    }
}

impl std::fmt::Debug for ReconnectEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReconnectEnd::Connected { address, .. } => write!(f, "Connected({address:?})"),
            ReconnectEnd::NewKey => write!(f, "NewKey"),
            ReconnectEnd::NotAllowed => write!(f, "NotAllowed"),
            ReconnectEnd::Cancelled => write!(f, "Cancelled"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sill_net::policy::GoodbyeOutcome;

    fn gb(reason: &str, reconnect: bool, notice: bool) -> CloseReason {
        CloseReason::Goodbye(GoodbyeOutcome { reason: reason.into(), message: None, reconnect, is_notice: notice })
    }

    /// Run the machine against an always failing dial and return the dial times.
    fn dial_times(sup: &mut Supervisor, until: f64) -> Vec<f64> {
        let mut t = 0.0;
        let mut out = vec![];
        while t <= until {
            match sup.poll(t) {
                Poll::Dial => {
                    out.push(t);
                    sup.dial_started(t);
                    sup.dial_failed();
                }
                Poll::WaitUntil(w) => t = w,
                Poll::Idle => break,
            }
        }
        out
    }

    #[test]
    fn ladder_and_stop_after_twelve() {
        let mut s = Supervisor::new("M");
        s.row_listed(0.0);
        s.session_ended(0.0, &CloseReason::Eof);
        let d = dial_times(&mut s, 10_000.0);
        // 0, +5, +10, +20, then every 30; twelve tries.
        let want = [0.0, 5.0, 15.0, 35.0, 65.0, 95.0, 125.0, 155.0, 185.0, 215.0, 245.0, 275.0];
        assert_eq!(d, want);
        assert_eq!(s.poll(1e9), Poll::Idle);
        assert_eq!(s.line().unwrap(), "Stopped trying to reach M. Tap it to try again.");
    }

    #[test]
    fn lines_follow_failures() {
        let mut s = Supervisor::new("M");
        s.row_listed(0.0);
        s.session_ended(0.0, &CloseReason::Silent);
        assert_eq!(s.line().unwrap(), "Reconnecting to M\u{2026}");
        for _ in 0..3 {
            s.dial_started(0.0);
            s.dial_failed();
        }
        assert!(s.line().unwrap().contains("isn\u{2019}t answering"));
    }

    #[test]
    fn tap_resets_after_giving_up() {
        let mut s = Supervisor::new("M");
        s.row_listed(0.0);
        s.session_ended(0.0, &CloseReason::Eof);
        dial_times(&mut s, 1e6);
        assert_eq!(s.poll(1e6), Poll::Idle);
        s.tap(1e6);
        assert_eq!(s.poll(1e6), Poll::Dial);
        s.dial_started(1e6);
        s.dial_failed();
        assert_eq!(s.failed(), 1);
        assert_eq!(s.poll(1e6 + 1.0), Poll::WaitUntil(1e6 + 5.0));
        assert_eq!(s.line().unwrap(), "Reconnecting to M\u{2026}");
    }

    #[test]
    fn fresh_listing_and_foreground_restart_the_count() {
        let mut s = Supervisor::new("M");
        s.row_listed(0.0);
        s.session_ended(0.0, &CloseReason::Eof);
        dial_times(&mut s, 1e6);
        assert_eq!(s.poll(1e6), Poll::Idle);
        s.row_gone();
        s.row_listed(2e6);
        assert_eq!(s.poll(2e6), Poll::Dial);
        // Background: no dials; back: starts again.
        let mut s = Supervisor::new("M");
        s.row_listed(0.0);
        s.session_ended(0.0, &CloseReason::Eof);
        s.set_foreground(false);
        assert_eq!(s.poll(1.0), Poll::Idle);
        s.set_foreground(true);
        assert_eq!(s.poll(1.0), Poll::Dial);
    }

    #[test]
    fn no_row_no_dial_until_listed() {
        let mut s = Supervisor::new("M");
        s.session_ended(0.0, &CloseReason::Eof);
        assert_eq!(s.poll(1.0), Poll::Idle);
        s.row_listed(2.0);
        assert_eq!(s.poll(2.0), Poll::Dial);
    }

    #[test]
    fn quit_waits_three_seconds_for_an_old_row() {
        let mut s = Supervisor::new("M");
        s.row_listed(0.0);
        s.session_ended(10.0, &gb("quit", true, false));
        assert_eq!(s.poll(10.5), Poll::WaitUntil(13.0));
        assert_eq!(s.poll(12.99), Poll::WaitUntil(13.0));
        assert_eq!(s.poll(13.0), Poll::Dial);
        // The row listed again after the quit (Sill is back) is taken at once.
        let mut s = Supervisor::new("M");
        s.row_listed(0.0);
        s.session_ended(10.0, &gb("quit", true, false));
        s.row_gone();
        s.row_listed(11.0);
        assert_eq!(s.poll(11.0), Poll::Dial);
        // A tap is the person's choice.
        let mut s = Supervisor::new("M");
        s.row_listed(0.0);
        s.session_ended(10.0, &gb("quit", true, false));
        s.tap(10.1);
        assert_eq!(s.poll(10.1), Poll::Dial);
    }

    #[test]
    fn goodbyes_that_never_reconnect() {
        for (r, n) in [("removed", false), ("pairingRequired", false), ("update", true)] {
            let mut s = Supervisor::new("M");
            s.row_listed(0.0);
            s.session_ended(0.0, &gb(r, false, n));
            assert_eq!(s.poll(100.0), Poll::Idle, "{r}");
            assert!(s.line().is_none());
        }
        let mut s = Supervisor::new("M");
        s.row_listed(0.0);
        s.session_ended(0.0, &CloseReason::Local);
        assert_eq!(s.poll(1.0), Poll::Idle);
        // busy and remoteOff reconnect.
        s.session_ended(0.0, &gb("busy", true, false));
        assert_eq!(s.poll(1.0), Poll::Dial);
    }
}

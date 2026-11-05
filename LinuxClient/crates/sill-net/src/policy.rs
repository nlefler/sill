//! The session's decisions as pure functions: liveness, goodbye, stats, the keyframe rate. No clock
//! of its own, no I/O; callers pass seconds so tests need no sleeping.

use sill_proto::{ClientStats, Goodbye};

/// No byte for `max(min_silence, 4 x the worst recent round trip)` ends the session
/// (CLAUDE.md, "Remote pacing": liveness is by bytes, not complete messages).
pub fn liveness_timeout(min_silence: f64, worst_rtt: f64) -> f64 {
    min_silence.max(4.0 * worst_rtt)
}

/// What a finished session means for reconnecting, per `GoodbyePolicy.swift`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoodbyeOutcome {
    pub reason: String,
    /// The Mac's own words, cleaned; present only for notices.
    pub message: Option<String>,
    pub reconnect: bool,
    /// A reason this build has no words of its own for ("update", or one a later host adds).
    pub is_notice: bool,
}

const KNOWN: [&str; 6] = ["quit", "removed", "remoteOff", "internetOff", "busy", "pairingRequired"];

/// `removed` and `pairingRequired` never reconnect; the other known reasons do; a notice reconnects
/// only on an explicit `true` (before, a reason nobody knew was redialled in a loop).
pub fn goodbye_outcome(g: &Goodbye) -> GoodbyeOutcome {
    let is_notice = !KNOWN.contains(&g.reason.as_str());
    let reconnect = if is_notice {
        g.reconnect == Some(true)
    } else {
        !matches!(g.reason.as_str(), "removed" | "pairingRequired")
    };
    let message = if is_notice { g.message.as_deref().and_then(|m| clean_message(m, 300)) } else { None };
    GoodbyeOutcome { reason: g.reason.clone(), message, reconnect, is_notice }
}

/// The status line for a finished session (`GoodbyePolicy.outcome`); `device` is "iPad", "iPhone" or here "computer".
pub fn goodbye_text(g: &GoodbyeOutcome, mac: &str, device: &str) -> String {
    match g.reason.as_str() {
        "quit" => format!("{mac} quit Sill. This {device} reconnects when it\u{2019}s back."),
        "removed" => format!("{mac} removed this {device}. To use it again, pair it again."),
        "remoteOff" => format!("{mac} turned off Remote Access."),
        "internetOff" => format!("{mac} stopped accepting connections from the internet. Connect through your VPN."),
        "busy" => format!("{mac} is already serving 8 devices."),
        "pairingRequired" => format!("{mac} now asks devices to pair. Tap it to pair this {device}."),
        _ => g.message.clone().unwrap_or_else(|| {
            if g.reconnect { format!("{mac} closed the connection. Sill will reconnect when it can.") } else { format!("{mac} closed the connection. Tap it to try again.") }
        }),
    }
}

/// One line, no control or bidirectional-override characters, at most `limit` characters.
pub fn clean_message(s: &str, limit: usize) -> Option<String> {
    let line = s.lines().next().unwrap_or("");
    let t: String = line
        .chars()
        .filter(|c| !c.is_control() && !matches!(*c as u32, 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069))
        .take(limit)
        .collect();
    let t = t.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// Kind 44 at most once per 2 s (`KeyframeRecovery`'s spacing, so the host's one-a-second limit never
/// refuses a device's own ask).
#[derive(Debug, Default)]
pub struct KeyframeLimiter {
    last: Option<f64>,
}

impl KeyframeLimiter {
    pub const SPACING: f64 = 2.0;

    pub fn allow(&mut self, now: f64) -> bool {
        match self.last {
            Some(t) if now - t < Self::SPACING && now >= t => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

/// One second of samples, closed into a kind 12 report: median (with max beside it), -1 for no sample.
#[derive(Debug, Default)]
pub struct StatsWindow {
    ages_ms: Vec<f64>,
    rtts_ms: Vec<f64>,
    frames: i64,
}

impl StatsWindow {
    pub fn frame(&mut self, age_ms: f64) {
        self.frames += 1;
        self.ages_ms.push(age_ms);
    }
    pub fn rtt(&mut self, rtt_ms: f64) {
        self.rtts_ms.push(rtt_ms);
    }
    pub fn worst_rtt_s(&self) -> Option<f64> {
        self.rtts_ms.iter().cloned().reduce(f64::max).map(|m| m / 1000.0)
    }

    /// Close the window; it starts empty again.
    pub fn close(&mut self, device: &str) -> ClientStats {
        let w = std::mem::take(self);
        let (age, age_max) = med_max(w.ages_ms);
        let (rtt, rtt_max) = med_max(w.rtts_ms);
        ClientStats {
            fps: w.frames,
            frame_age_ms: age,
            rtt_ms: rtt,
            device: device.to_string(),
            frame_age_max_ms: Some(age_max),
            rtt_max_ms: Some(rtt_max),
            audio_behind_ms: None,
            audio_late: None,
        }
    }
}

fn med_max(mut v: Vec<f64>) -> (i64, i64) {
    if v.is_empty() {
        return (-1, -1);
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    let med = if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 };
    (med.round() as i64, v[n - 1].round() as i64)
}

/// Ping payload: the client's monotonic clock in seconds, 8 bytes big-endian (the host echoes it).
pub fn ping_payload(clock: f64) -> Vec<u8> {
    clock.to_be_bytes().to_vec()
}

/// The round trip a pong's payload gives at clock `now`; `None` for anything that is not our ping.
pub fn rtt_from_pong(payload: &[u8], now: f64) -> Option<f64> {
    let sent = f64::from_be_bytes(payload.try_into().ok()?);
    let rtt = now - sent;
    if sent.is_finite() && (0.0..3600.0).contains(&rtt) { Some(rtt) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(reason: &str, reconnect: Option<bool>) -> Goodbye {
        Goodbye { reason: reason.into(), message: Some("hi\nsecond".into()), minimum_version: None, reconnect }
    }

    #[test]
    fn liveness_is_six_seconds_or_four_round_trips() {
        assert_eq!(liveness_timeout(6.0, 0.1), 6.0);
        assert_eq!(liveness_timeout(6.0, 2.0), 8.0);
    }

    #[test]
    fn reconnect_decisions() {
        assert!(goodbye_outcome(&g("quit", None)).reconnect);
        assert!(goodbye_outcome(&g("busy", None)).reconnect);
        assert!(goodbye_outcome(&g("remoteOff", None)).reconnect);
        assert!(!goodbye_outcome(&g("removed", Some(true))).reconnect);
        assert!(!goodbye_outcome(&g("pairingRequired", None)).reconnect);
        // Notices: only an explicit true.
        let u = goodbye_outcome(&g("update", None));
        assert!(u.is_notice && !u.reconnect && u.message.as_deref() == Some("hi"));
        assert!(!goodbye_outcome(&g("update", Some(false))).reconnect);
        assert!(goodbye_outcome(&g("update", Some(true))).reconnect);
        assert!(!goodbye_outcome(&g("somethingNew", None)).reconnect);
        assert!(goodbye_outcome(&g("", Some(true))).reconnect);
    }

    #[test]
    fn message_is_cleaned() {
        assert_eq!(clean_message("a\u{202E}b\u{7}c", 300).as_deref(), Some("abc"));
        assert_eq!(clean_message("  \n", 300), None);
        assert_eq!(clean_message(&"x".repeat(500), 300).unwrap().len(), 300);
    }

    #[test]
    fn keyframe_spacing() {
        let mut l = KeyframeLimiter::default();
        assert!(l.allow(10.0));
        assert!(!l.allow(11.9));
        assert!(l.allow(12.0));
        assert!(!l.allow(12.5));
    }

    #[test]
    fn stats_medians_and_empty() {
        let mut w = StatsWindow::default();
        let s = w.close("d");
        assert_eq!((s.fps, s.frame_age_ms, s.rtt_ms), (0, -1, -1));
        for a in [10.0, 30.0, 20.0] {
            w.frame(a);
        }
        w.rtt(4.0);
        w.rtt(8.0);
        let s = w.close("d");
        assert_eq!((s.fps, s.frame_age_ms, s.frame_age_max_ms), (3, 20, Some(30)));
        assert_eq!((s.rtt_ms, s.rtt_max_ms), (6, Some(8)));
    }

    #[test]
    fn pong_rtt() {
        let p = ping_payload(10.0);
        assert!((rtt_from_pong(&p, 10.25).unwrap() - 0.25).abs() < 1e-9);
        assert_eq!(rtt_from_pong(&[1, 2, 3], 1.0), None);
        assert_eq!(rtt_from_pong(&ping_payload(20.0), 10.0), None);
    }
}

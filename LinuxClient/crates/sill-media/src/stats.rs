//! Frame-age, fps and round-trip statistics over a sliding second: what the HUD shows, and what a kind 12
//! report holds. Times are seconds on a monotonic clock the caller supplies.

use sill_proto::ClientStats;
use std::collections::VecDeque;

const WINDOW: f64 = 1.0;

#[derive(Debug, Default)]
pub struct StatsCollector {
    decoded: VecDeque<(f64, f64)>,   // (when, decode ms)
    presented: VecDeque<(f64, f64)>, // (when, frame age ms at presentation)
    rtts: VecDeque<(f64, f64)>,      // (when, ms)
    dropped: VecDeque<f64>,
    total_presented: u64,
    total_dropped: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub decoded_fps: i64,
    pub presented_fps: i64,
    /// Median / max frame age (host encode to presentation) in the last second; `None` without a sample.
    pub frame_age_ms: Option<(i64, i64)>,
    pub rtt_ms: Option<(i64, i64)>,
    pub decode_ms_avg: Option<f64>,
    pub dropped_per_s: i64,
    pub total_presented: u64,
    pub total_dropped: u64,
}

fn trim<T>(q: &mut VecDeque<T>, now: f64, when: impl Fn(&T) -> f64) {
    while q.front().is_some_and(|x| now - when(x) >= WINDOW) {
        q.pop_front();
    }
}

fn med_max(mut v: Vec<f64>) -> Option<(i64, i64)> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    let med = if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 };
    Some((med.round() as i64, v[n - 1].round() as i64))
}

impl StatsCollector {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn record_decoded(&mut self, now: f64, decode_ms: f64) {
        self.decoded.push_back((now, decode_ms));
    }
    /// A picture reached the screen; `age_ms` is host-encode-to-now by the wall clock (negative clamps to 0).
    pub fn record_presented(&mut self, now: f64, age_ms: f64) {
        self.total_presented += 1;
        self.presented.push_back((now, age_ms.max(0.0)));
    }
    /// A decoded picture was replaced by a newer one before it was shown.
    pub fn record_dropped(&mut self, now: f64) {
        self.total_dropped += 1;
        self.dropped.push_back(now);
    }
    pub fn record_rtt(&mut self, now: f64, rtt_ms: f64) {
        self.rtts.push_back((now, rtt_ms));
    }

    pub fn snapshot(&mut self, now: f64) -> Snapshot {
        trim(&mut self.decoded, now, |x| x.0);
        trim(&mut self.presented, now, |x| x.0);
        trim(&mut self.rtts, now, |x| x.0);
        trim(&mut self.dropped, now, |x| *x);
        Snapshot {
            decoded_fps: self.decoded.len() as i64,
            presented_fps: self.presented.len() as i64,
            frame_age_ms: med_max(self.presented.iter().map(|x| x.1).collect()),
            rtt_ms: med_max(self.rtts.iter().map(|x| x.1).collect()),
            decode_ms_avg: (!self.decoded.is_empty()).then(|| self.decoded.iter().map(|x| x.1).sum::<f64>() / self.decoded.len() as f64),
            dropped_per_s: self.dropped.len() as i64,
            total_presented: self.total_presented,
            total_dropped: self.total_dropped,
        }
    }

    /// The last second as a kind 12 report: -1 for no sample.
    pub fn to_client_stats(&mut self, now: f64, device: &str) -> ClientStats {
        let s = self.snapshot(now);
        let (age, age_max) = s.frame_age_ms.unwrap_or((-1, -1));
        let (rtt, rtt_max) = s.rtt_ms.unwrap_or((-1, -1));
        ClientStats {
            fps: s.presented_fps,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_second_has_no_samples() {
        let mut s = StatsCollector::new();
        let st = s.snapshot(5.0);
        assert_eq!((st.decoded_fps, st.presented_fps, st.frame_age_ms, st.rtt_ms), (0, 0, None, None));
        let c = s.to_client_stats(5.0, "d");
        assert_eq!((c.fps, c.frame_age_ms, c.rtt_ms, c.frame_age_max_ms), (0, -1, -1, Some(-1)));
    }

    #[test]
    fn sliding_window_medians_and_counts() {
        let mut s = StatsCollector::new();
        for i in 0..60 {
            let t = i as f64 / 60.0;
            s.record_decoded(t, 2.0);
            s.record_presented(t, 10.0 + (i % 3) as f64 * 10.0);
        }
        s.record_rtt(0.5, 4.0);
        s.record_rtt(0.6, 8.0);
        let st = s.snapshot(0.999);
        assert_eq!((st.decoded_fps, st.presented_fps), (60, 60));
        assert_eq!(st.frame_age_ms, Some((20, 30)));
        assert_eq!(st.rtt_ms, Some((6, 8)));
        assert_eq!(st.decode_ms_avg, Some(2.0));
        // Half a second later only the newer half is counted.
        let st = s.snapshot(1.7);
        assert!(st.presented_fps <= 20 && st.presented_fps >= 17, "{}", st.presented_fps);
        assert_eq!(st.rtt_ms, None);
        assert_eq!(st.total_presented, 60);
    }

    #[test]
    fn drops_and_negative_age() {
        let mut s = StatsCollector::new();
        s.record_dropped(0.1);
        s.record_dropped(0.2);
        s.record_presented(0.3, -50.0); // clock skew
        let st = s.snapshot(0.5);
        assert_eq!((st.dropped_per_s, st.frame_age_ms), (2, Some((0, 0))));
        assert_eq!(s.snapshot(2.0).dropped_per_s, 0);
        assert_eq!(s.snapshot(2.0).total_dropped, 2);
    }
}

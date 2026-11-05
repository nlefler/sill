//! When the client asks the Mac for a keyframe (kind 44) after its picture was interrupted: a port of
//! `iOSClient/KeyframeRecovery.swift`, rule for rule. Pure: the caller passes the clock (seconds on a
//! monotonic clock) and what is active.
//!
//! - One ask per interruption while its keyframe is due; an interruption runs from the picture lost to
//!   the next keyframe shown.
//! - An ask whose keyframe has not come within `wait` (max(4 s, 4 worst round trips)) is owed again, as
//!   a repeat: 4, 8, 16 s later, then every 32 s.
//! - Only while the window is active and something streams.
//! - At least the spacing (2 s) after the last ask; a picture that fails again within `CALM` (10 s) of
//!   its keyframe, or an unanswered ask, doubles the spacing (2, 4, 8, 16, 32 s at most).

pub const SPACING: f64 = 2.0;
pub const LONGEST_SPACING: f64 = 32.0;
pub const CALM: f64 = 10.0;
pub const SHORTEST_WAIT: f64 = 4.0;

/// How long an ask waits for its keyframe: 4 s, or four of the worst round trips of the last second that
/// measured one; none measured counts as 4 s.
pub fn answer_wait(worst_rtt_ms: Option<i64>) -> f64 {
    SHORTEST_WAIT.max(4.0 * worst_rtt_ms.unwrap_or(0) as f64 / 1000.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decision {
    /// No interruption, window not active, or nothing streams.
    Nothing,
    /// Ask now (`consider` has counted the ask).
    AskNow,
    /// Owed, held back by the spacing: consider again at this time.
    AskAt(f64),
    /// The ask is out and its keyframe is due by this time: consider again then.
    KeyframeDue(f64),
}

#[derive(Clone, Debug, Default)]
pub struct KeyframeRecovery {
    open: bool,
    asked: bool,
    repeats: u32,
    last_ask: Option<f64>,
    back_at: Option<f64>,
    went_away: bool,
}

impl KeyframeRecovery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn repeats(&self) -> u32 {
        self.repeats
    }

    /// The spacing the next ask keeps from the last one.
    pub fn spacing_now(&self) -> f64 {
        (SPACING * 2f64.powi(self.repeats.min(30) as i32)).min(LONGEST_SPACING)
    }

    /// The decoder flushed and waits for a keyframe. Opens an interruption, or is part of the open one.
    pub fn interrupted(&mut self, now: f64) {
        if self.open {
            return;
        }
        self.open = true;
        self.asked = false;
        self.repeats = match self.back_at {
            Some(back) if !self.went_away && now >= back && now - back < CALM => self.repeats + 1,
            _ => 0,
        };
    }

    /// A keyframe ended the wait. `active`: the window is active. A keyframe shown while away is the
    /// background's: the trip away still counts, so the next interruption is not a repeat.
    pub fn picture_back(&mut self, now: f64, active: bool) {
        if !self.open {
            return;
        }
        self.open = false;
        self.back_at = Some(now);
        if active {
            self.went_away = false;
        }
    }

    /// The window is no longer active (minimised, hidden). An ask that was out is owed again and the
    /// spacing starts at 2 s again.
    pub fn left_foreground(&mut self) {
        self.went_away = true;
        self.asked = false;
        self.repeats = 0;
    }

    /// Whether to ask now. Counts the ask when it says `AskNow`.
    pub fn consider(&mut self, now: f64, active: bool, streaming: bool, wait: f64) -> Decision {
        if !(self.open && active && streaming) {
            return Decision::Nothing;
        }
        if self.asked {
            if let Some(last) = self.last_ask {
                if now >= last && now < last + wait {
                    return Decision::KeyframeDue(last + wait);
                }
            }
            // None came: owed again, as a repeat, so a Mac that never answers is asked ever less often.
            self.asked = false;
            self.repeats += 1;
        }
        // Against the time it is due, not `now - last < spacing`, which can round below it and hold an
        // ask at the very time it was held until.
        if let Some(last) = self.last_ask {
            if now >= last && now < last + self.spacing_now() {
                return Decision::AskAt(last + self.spacing_now());
            }
        }
        self.asked = true;
        self.last_ask = Some(now);
        Decision::AskNow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Decision::*;

    const W: f64 = 4.0;

    #[test]
    fn nothing_without_interruption_or_activity() {
        let mut k = KeyframeRecovery::new();
        assert_eq!(k.consider(0.0, true, true, W), Nothing);
        k.interrupted(1.0);
        assert_eq!(k.consider(1.0, false, true, W), Nothing);
        assert_eq!(k.consider(1.0, true, false, W), Nothing);
        assert_eq!(k.consider(1.0, true, true, W), AskNow);
    }

    #[test]
    fn one_ask_per_interruption_then_due_time() {
        let mut k = KeyframeRecovery::new();
        k.interrupted(10.0);
        assert_eq!(k.consider(10.0, true, true, W), AskNow);
        assert_eq!(k.consider(10.5, true, true, W), KeyframeDue(14.0));
        assert_eq!(k.consider(13.99, true, true, W), KeyframeDue(14.0));
        // Part of the same interruption (the layer reads interrupted at every frame).
        k.interrupted(11.0);
        assert_eq!(k.consider(11.0, true, true, W), KeyframeDue(14.0));
    }

    #[test]
    fn unanswered_asks_back_off_4_8_16_then_32() {
        let mut k = KeyframeRecovery::new();
        k.interrupted(0.0);
        let mut t = 0.0;
        assert_eq!(k.consider(t, true, true, W), AskNow);
        // wait 4 s, then repeats=1 (spacing 4): asked again at 4 s
        let mut gaps = vec![];
        for _ in 0..6 {
            let mut now = t;
            loop {
                match k.consider(now, true, true, W) {
                    AskNow => break,
                    AskAt(x) | KeyframeDue(x) => now = x,
                    Nothing => panic!(),
                }
            }
            gaps.push(now - t);
            t = now;
        }
        // 4 (wait 4, spacing 4), 8, 16, 32, 32, 32
        assert_eq!(gaps, vec![4.0, 8.0, 16.0, 32.0, 32.0, 32.0]);
    }

    #[test]
    fn a_slow_link_waits_four_round_trips() {
        assert_eq!(answer_wait(None), 4.0);
        assert_eq!(answer_wait(Some(200)), 4.0);
        assert_eq!(answer_wait(Some(2500)), 10.0);
        let mut k = KeyframeRecovery::new();
        k.interrupted(0.0);
        assert_eq!(k.consider(0.0, true, true, 10.0), AskNow);
        assert_eq!(k.consider(9.9, true, true, 10.0), KeyframeDue(10.0));
    }

    #[test]
    fn spacing_holds_back_a_new_interruption_two_seconds() {
        let mut k = KeyframeRecovery::new();
        k.interrupted(0.0);
        assert_eq!(k.consider(0.0, true, true, W), AskNow);
        k.picture_back(0.5, true);
        // Fails again 1 s later, within CALM of the keyframe: a repeat, spacing 4 s.
        k.interrupted(1.5);
        assert_eq!(k.repeats(), 1);
        assert_eq!(k.consider(1.5, true, true, W), AskAt(4.0));
        assert_eq!(k.consider(3.99, true, true, W), AskAt(4.0));
        assert_eq!(k.consider(4.0, true, true, W), AskNow);
    }

    #[test]
    fn a_calm_picture_starts_afresh() {
        let mut k = KeyframeRecovery::new();
        k.interrupted(0.0);
        k.consider(0.0, true, true, W);
        k.picture_back(1.0, true);
        k.interrupted(11.0); // exactly CALM later
        assert_eq!(k.repeats(), 0);
        assert_eq!(k.consider(11.0, true, true, W), AskNow);
    }

    #[test]
    fn picture_back_without_interruption_is_ignored() {
        let mut k = KeyframeRecovery::new();
        k.picture_back(1.0, true);
        k.interrupted(2.0);
        assert_eq!(k.repeats(), 0);
    }

    #[test]
    fn leaving_the_foreground_owes_the_ask_again_and_resets_spacing() {
        let mut k = KeyframeRecovery::new();
        k.interrupted(0.0);
        assert_eq!(k.consider(0.0, true, true, W), AskNow);
        k.left_foreground();
        assert_eq!(k.consider(0.5, false, true, W), Nothing);
        // back at 1 s: ask is owed, spacing 2 s from the last ask at 0
        assert_eq!(k.consider(1.0, true, true, W), AskAt(2.0));
        assert_eq!(k.consider(2.0, true, true, W), AskNow);
    }

    #[test]
    fn a_keyframe_shown_while_away_never_makes_a_repeat() {
        let mut k = KeyframeRecovery::new();
        k.interrupted(0.0);
        k.consider(0.0, true, true, W);
        k.left_foreground();
        k.picture_back(1.0, false);
        k.interrupted(2.0);
        assert_eq!(k.repeats(), 0);
    }

    #[test]
    fn a_keyframe_while_active_after_a_trip_clears_the_trip() {
        let mut k = KeyframeRecovery::new();
        k.interrupted(0.0);
        k.left_foreground();
        k.picture_back(1.0, true);
        k.interrupted(2.0); // active throughout since: a repeat
        assert_eq!(k.repeats(), 1);
    }

    #[test]
    fn clock_going_back_never_loops_or_panics() {
        let mut k = KeyframeRecovery::new();
        k.interrupted(100.0);
        assert_eq!(k.consider(100.0, true, true, W), AskNow);
        // now < last: not "due", not held: asked again as owed
        assert_eq!(k.consider(50.0, true, true, W), AskNow);
    }

    #[test]
    fn spacing_is_capped() {
        let mut k = KeyframeRecovery::new();
        k.repeats = 40;
        assert_eq!(k.spacing_now(), 32.0);
    }
}

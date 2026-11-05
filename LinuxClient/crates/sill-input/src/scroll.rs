//! Scrolling: a wheel's notches as plain `scroll` events, a touchpad's two-finger scroll as a phased
//! gesture (`scrollGesture` began, deltas, ended) with the coast the Mac will not make generated here
//! (momentumBegan, decaying deltas, momentumEnded), as iOSClient/InputOverlay.swift does for a finger.
//! Deltas are fractions of the picture; positive `dy` moves the content down (the direction winit
//! reports, which the OS has already turned by its natural-scrolling setting). Every event of one
//! scroll, its coast included, is at the place the pointer was when it began.

use crate::pointer::Rect;
use sill_proto::{InputEvent, ScrollPhase};

#[derive(Clone, Copy, Debug)]
pub struct ScrollConfig {
    /// Turn every scroll around (Reverse Scrolling, ScrollDirection.swift; there only for fingers on
    /// the glass, here for any source: the OS's own setting is the first turn).
    pub reverse: bool,
    /// Coast after a touchpad flick.
    pub momentum: bool,
    /// Logical points one wheel notch scrolls (iOS measured 3 pt per notch from a mouse; a Mac's
    /// wheel scrolls about ten).
    pub line_points: f64,
}

impl Default for ScrollConfig {
    fn default() -> Self {
        ScrollConfig { reverse: false, momentum: true, line_points: 10.0 }
    }
}

/// Where the picture is and how big a logical point is, for turning pixels into fractions.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub rect: Rect,
    pub scale: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Started,
    Moved,
    Ended,
    Cancelled,
}

/// Lift-off speed (points a second) below which a pan just stops (ScrollMomentum.minimumSpeed).
pub const MIN_SPEED: f64 = 150.0;
const TICK: f64 = 1.0 / 60.0;
/// Velocity kept per 1/60 s (ScrollMomentum.decayPerTick).
const DECAY: f64 = 0.96;
/// Stop once a tick moves less than this many points.
const STOP_BELOW: f64 = 0.3;
/// A gesture with no event for this long is over (some platforms never say "ended").
pub const IDLE_END: f64 = 0.15;
/// Samples this recent before the lift-off count toward its velocity.
const VELOCITY_WINDOW: f64 = 0.1;

#[derive(Debug)]
enum State {
    Idle,
    Gesture { at: (f64, f64), last: f64, samples: Vec<(f64, f64, f64)> },
    Momentum { at: (f64, f64), vx: f64, vy: f64, last: f64 },
}

#[derive(Debug)]
pub struct Scroller {
    pub cfg: ScrollConfig,
    state: State,
}

impl Scroller {
    pub fn new(cfg: ScrollConfig) -> Scroller {
        Scroller { cfg, state: State::Idle }
    }

    fn sign(&self) -> f64 {
        if self.cfg.reverse { -1.0 } else { 1.0 }
    }

    fn scroll(at: (f64, f64), dx: f64, dy: f64) -> InputEvent {
        InputEvent::Scroll { x: at.0, y: at.1, dx, dy }
    }

    fn gesture(phase: ScrollPhase, at: (f64, f64)) -> InputEvent {
        InputEvent::ScrollGesture { phase, x: at.0, y: at.1 }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// A coast in progress ends (a new scroll, a click, a key…): its end goes out.
    pub fn stop_momentum(&mut self) -> Vec<InputEvent> {
        if let State::Momentum { at, .. } = self.state {
            self.state = State::Idle;
            return vec![Self::gesture(ScrollPhase::MomentumEnded, at)];
        }
        vec![]
    }

    /// Everything open ends at once, with no coast (focus lost, the session ending).
    pub fn stop_all(&mut self) -> Vec<InputEvent> {
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Gesture { at, .. } => vec![Self::gesture(ScrollPhase::Ended, at)],
            State::Momentum { at, .. } => vec![Self::gesture(ScrollPhase::MomentumEnded, at)],
            State::Idle => vec![],
        }
    }

    /// A wheel's notches (or a touch of lines): plain deltas, no gesture, no coast.
    pub fn wheel(&mut self, v: View, at: (f64, f64), lines_x: f64, lines_y: f64) -> Vec<InputEvent> {
        let mut out = self.stop_momentum();
        if v.rect.is_empty() {
            return out;
        }
        let px = self.cfg.line_points * v.scale * self.sign();
        let (dx, dy) = (lines_x * px / v.rect.w, lines_y * px / v.rect.h);
        if dx != 0.0 || dy != 0.0 {
            out.push(Self::scroll(at, dx, dy));
        }
        out
    }

    /// A touchpad's pixel deltas with their phase.
    pub fn pixels(&mut self, now: f64, v: View, at: (f64, f64), phase: Phase, dx_px: f64, dy_px: f64) -> Vec<InputEvent> {
        let mut out = vec![];
        if v.rect.is_empty() {
            return self.stop_all();
        }
        let (dx_px, dy_px) = (dx_px * self.sign(), dy_px * self.sign());
        let open = matches!(self.state, State::Gesture { .. });
        match phase {
            Phase::Started | Phase::Moved => {
                if phase == Phase::Started && open {
                    out.extend(self.stop_all()); // a new gesture closes the last
                }
                if !matches!(self.state, State::Gesture { .. }) {
                    out.extend(self.stop_momentum());
                    out.push(Self::gesture(ScrollPhase::Began, at));
                    self.state = State::Gesture { at, last: now, samples: vec![] };
                }
                out.extend(self.step(now, v, dx_px, dy_px));
            }
            Phase::Ended | Phase::Cancelled => {
                if !open {
                    return out;
                }
                if phase == Phase::Ended {
                    out.extend(self.step(now, v, dx_px, dy_px));
                }
                out.extend(self.end_gesture(now, v, phase == Phase::Ended));
            }
        }
        out
    }

    fn step(&mut self, now: f64, v: View, dx_px: f64, dy_px: f64) -> Vec<InputEvent> {
        let State::Gesture { at, last, samples } = &mut self.state else { return vec![] };
        *last = now;
        if dx_px == 0.0 && dy_px == 0.0 {
            return vec![];
        }
        samples.push((now, dx_px, dy_px));
        vec![Self::scroll(*at, dx_px / v.rect.w, dy_px / v.rect.h)]
    }

    fn end_gesture(&mut self, now: f64, v: View, lift: bool) -> Vec<InputEvent> {
        let State::Gesture { at, samples, .. } = std::mem::replace(&mut self.state, State::Idle) else { return vec![] };
        let mut out = vec![Self::gesture(ScrollPhase::Ended, at)];
        if lift && self.cfg.momentum {
            let recent: Vec<_> = samples.iter().filter(|s| now - s.0 <= VELOCITY_WINDOW).collect();
            if let (Some(first), Some(last)) = (recent.first(), recent.last()) {
                let dt = last.0 - first.0;
                if recent.len() >= 2 && dt > 0.0 {
                    let (sx, sy) = recent.iter().skip(1).fold((0.0, 0.0), |a, s| (a.0 + s.1, a.1 + s.2));
                    let (vx, vy) = (sx / dt, sy / dt);
                    if vx.hypot(vy) / v.scale > MIN_SPEED {
                        out.push(Self::gesture(ScrollPhase::MomentumBegan, at));
                        out.push(Self::scroll(at, vx * TICK / v.rect.w, vy * TICK / v.rect.h));
                        self.state = State::Momentum { at, vx, vy, last: now };
                    }
                }
            }
        }
        out
    }

    /// Time passes: a gesture nobody ended is ended, and a coast takes its steps (about 60 a second).
    pub fn poll(&mut self, now: f64, v: View) -> Vec<InputEvent> {
        match &mut self.state {
            State::Idle => vec![],
            State::Gesture { last, .. } => {
                if now - *last >= IDLE_END {
                    let at_last = *last;
                    self.end_gesture(at_last, v, true)
                } else {
                    vec![]
                }
            }
            State::Momentum { at, vx, vy, last } => {
                if v.rect.is_empty() {
                    return self.stop_momentum();
                }
                let dt = (now - *last).min(0.1);
                if dt < TICK * 0.999 {
                    return vec![];
                }
                *last = now;
                let k = DECAY.powf(dt / TICK);
                *vx *= k;
                *vy *= k;
                if vx.hypot(*vy) / v.scale * TICK < STOP_BELOW {
                    return self.stop_momentum();
                }
                vec![Self::scroll(*at, *vx * dt / v.rect.w, *vy * dt / v.rect.h)]
            }
        }
    }

    pub fn next_deadline(&self) -> Option<f64> {
        match &self.state {
            State::Idle => None,
            State::Gesture { last, .. } => Some(last + IDLE_END),
            State::Momentum { last, .. } => Some(last + TICK),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ScrollPhase as P;

    fn view() -> View {
        View { rect: Rect { x: 0.0, y: 0.0, w: 1000.0, h: 500.0 }, scale: 1.0 }
    }
    const AT: (f64, f64) = (0.25, 0.75);

    fn gest(p: P) -> InputEvent {
        InputEvent::ScrollGesture { phase: p, x: AT.0, y: AT.1 }
    }
    fn sc(dx: f64, dy: f64) -> InputEvent {
        InputEvent::Scroll { x: AT.0, y: AT.1, dx, dy }
    }

    #[test]
    fn wheel_notches_are_plain_scrolls_in_fractions_of_the_picture() {
        let mut s = Scroller::new(ScrollConfig::default());
        // One notch up (winit: +1 line, content moves down): 10 points of 500.
        assert_eq!(s.wheel(view(), AT, 0.0, 1.0), vec![sc(0.0, 10.0 / 500.0)]);
        assert_eq!(s.wheel(view(), AT, -2.0, 0.0), vec![sc(-20.0 / 1000.0, 0.0)]);
        assert!(s.wheel(view(), AT, 0.0, 0.0).is_empty());
        // HiDPI: a logical point is two pixels.
        let v = View { scale: 2.0, ..view() };
        assert_eq!(s.wheel(v, AT, 0.0, 1.0), vec![sc(0.0, 20.0 / 500.0)]);
    }

    #[test]
    fn reverse_scrolling_turns_everything_around() {
        let mut s = Scroller::new(ScrollConfig { reverse: true, ..Default::default() });
        assert_eq!(s.wheel(view(), AT, 0.0, 1.0), vec![sc(0.0, -10.0 / 500.0)]);
        let out = s.pixels(0.0, view(), AT, Phase::Started, 0.0, 5.0);
        assert_eq!(out, vec![gest(P::Began), sc(0.0, -5.0 / 500.0)]);
    }

    #[test]
    fn a_slow_touchpad_scroll_is_began_deltas_ended_with_no_coast() {
        let mut s = Scroller::new(ScrollConfig::default());
        let mut out = s.pixels(0.00, view(), AT, Phase::Started, 0.0, 2.0);
        out.extend(s.pixels(0.05, view(), AT, Phase::Moved, 0.0, 2.0));
        out.extend(s.pixels(0.10, view(), AT, Phase::Moved, 0.0, 2.0));
        out.extend(s.pixels(0.15, view(), AT, Phase::Ended, 0.0, 0.0));
        let d = 2.0 / 500.0;
        assert_eq!(out, vec![gest(P::Began), sc(0.0, d), sc(0.0, d), sc(0.0, d), gest(P::Ended)]);
        assert!(!s.is_active());
        assert_eq!(s.next_deadline(), None);
    }

    #[test]
    fn a_flick_coasts_from_the_place_it_began_with_decaying_steps_and_ends() {
        let mut s = Scroller::new(ScrollConfig::default());
        let mut out = vec![];
        // 20 px every 10 ms = 2000 px/s.
        out.extend(s.pixels(0.00, view(), AT, Phase::Started, 0.0, 20.0));
        for i in 1..=5 {
            out.extend(s.pixels(i as f64 * 0.01, view(), AT, Phase::Moved, 0.0, 20.0));
        }
        let lift = s.pixels(0.06, view(), AT, Phase::Ended, 0.0, 0.0);
        assert_eq!(lift[0], gest(P::Ended));
        assert_eq!(lift[1], gest(P::MomentumBegan));
        let InputEvent::Scroll { dy: first, x, y, .. } = lift[2] else { panic!("{lift:?}") };
        assert_eq!((x, y), AT);
        assert!((first - 2000.0 / 60.0 / 500.0).abs() < 1e-9, "the first step is a tick at the lift-off speed: {first}");
        // The coast: one step per tick, each smaller, all at AT, then its end.
        let mut t = 0.06;
        let mut steps = vec![];
        let mut ended = false;
        for _ in 0..1000 {
            t += 1.0 / 60.0;
            for e in s.poll(t, view()) {
                match e {
                    InputEvent::Scroll { dy, x, y, .. } => {
                        assert_eq!((x, y), AT);
                        steps.push(dy);
                    }
                    InputEvent::ScrollGesture { phase: P::MomentumEnded, x, y } => {
                        assert_eq!((x, y), AT);
                        ended = true;
                    }
                    e => panic!("{e:?}"),
                }
            }
            if ended {
                break;
            }
        }
        assert!(ended && steps.len() > 30, "{} steps", steps.len());
        assert!(steps.windows(2).all(|w| w[1] < w[0]), "decaying");
        assert!(!s.is_active());
        let total: f64 = steps.iter().sum::<f64>() * 500.0 + first * 500.0;
        assert!(total > 600.0 && total < 1000.0, "total coast {total} px");
    }

    #[test]
    fn a_new_scroll_stops_the_coast_first() {
        let mut s = Scroller::new(ScrollConfig::default());
        s.pixels(0.00, view(), AT, Phase::Started, 0.0, 20.0);
        s.pixels(0.01, view(), AT, Phase::Moved, 0.0, 20.0);
        s.pixels(0.02, view(), AT, Phase::Ended, 0.0, 0.0);
        assert!(s.is_active());
        let out = s.pixels(0.05, view(), (0.9, 0.9), Phase::Started, 0.0, 1.0);
        assert_eq!(out[0], gest(P::MomentumEnded));
        assert_eq!(out[1], InputEvent::ScrollGesture { phase: P::Began, x: 0.9, y: 0.9 });
        // A wheel stops it too.
        let mut s = Scroller::new(ScrollConfig::default());
        s.pixels(0.00, view(), AT, Phase::Started, 0.0, 20.0);
        s.pixels(0.01, view(), AT, Phase::Moved, 0.0, 20.0);
        s.pixels(0.02, view(), AT, Phase::Ended, 0.0, 0.0);
        assert_eq!(s.wheel(view(), AT, 0.0, 1.0)[0], gest(P::MomentumEnded));
    }

    #[test]
    fn momentum_can_be_off_and_a_cancel_never_coasts() {
        let mut s = Scroller::new(ScrollConfig { momentum: false, ..Default::default() });
        s.pixels(0.00, view(), AT, Phase::Started, 0.0, 20.0);
        s.pixels(0.01, view(), AT, Phase::Moved, 0.0, 20.0);
        assert_eq!(s.pixels(0.02, view(), AT, Phase::Ended, 0.0, 0.0), vec![gest(P::Ended)]);
        let mut s = Scroller::new(ScrollConfig::default());
        s.pixels(0.00, view(), AT, Phase::Started, 0.0, 20.0);
        s.pixels(0.01, view(), AT, Phase::Moved, 0.0, 20.0);
        assert_eq!(s.pixels(0.02, view(), AT, Phase::Cancelled, 0.0, 0.0), vec![gest(P::Ended)]);
    }

    #[test]
    fn a_gesture_nobody_ends_is_ended_after_a_pause_and_a_stale_flick_does_not_coast() {
        let mut s = Scroller::new(ScrollConfig::default());
        // Moved without a Started begins one; a platform that never says "ended".
        let out = s.pixels(0.0, view(), AT, Phase::Moved, 0.0, 1.0);
        assert_eq!(out[0], gest(P::Began));
        assert_eq!(s.next_deadline(), Some(IDLE_END));
        assert!(s.poll(0.1, view()).is_empty());
        assert_eq!(s.poll(0.2, view()), vec![gest(P::Ended)]);
        assert!(!s.is_active());
        // A fast stroke that stopped before the lift: no coast.
        let mut s = Scroller::new(ScrollConfig::default());
        s.pixels(0.00, view(), AT, Phase::Started, 0.0, 30.0);
        s.pixels(0.01, view(), AT, Phase::Moved, 0.0, 30.0);
        assert_eq!(s.pixels(0.50, view(), AT, Phase::Ended, 0.0, 0.0), vec![gest(P::Ended)]);
    }

    #[test]
    fn stop_all_closes_whatever_is_open() {
        let mut s = Scroller::new(ScrollConfig::default());
        s.pixels(0.0, view(), AT, Phase::Started, 0.0, 1.0);
        assert_eq!(s.stop_all(), vec![gest(P::Ended)]);
        assert!(s.stop_all().is_empty());
    }
}

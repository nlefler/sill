//! The pointer: window pixels to fractions of the video frame (through the letterbox), the buttons,
//! and moves coalesced to the newest per [`MOVE_INTERVAL`]. iOSClient/InputOverlay.swift's
//! `normalized(_:clamped:)` and `sendPointer`: a point in the black bars is not sent, except while a
//! button is held (a drag, and its release, are clamped to the frame so the button never sticks).

use sill_proto::{InputEvent, PointerAction};

/// The least time between two moves sent (coalescing: the newest position wins).
pub const MOVE_INTERVAL: f64 = 0.008;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The video aspect-fit in a window, centred (the same as `sill_media::cursor::video_rect`).
pub fn video_rect(win_w: f64, win_h: f64, frame_w: f64, frame_h: f64) -> Rect {
    if !(win_w > 0.0 && win_h > 0.0 && frame_w > 0.0 && frame_h > 0.0) {
        return Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
    }
    let s = (win_w / frame_w).min(win_h / frame_h);
    let (w, h) = (frame_w * s, frame_h * s);
    Rect { x: (win_w - w) / 2.0, y: (win_h - h) / 2.0, w, h }
}

impl Rect {
    pub fn shifted_y(self, dy: f64) -> Rect {
        Rect { y: self.y + dy, ..self }
    }

    pub fn is_empty(&self) -> bool {
        !(self.w > 0.0 && self.h > 0.0)
    }

    /// `(x, y)` in window pixels as fractions of the picture; `None` outside it (or with no picture),
    /// or clamped onto it when `clamped`.
    pub fn fraction(&self, x: f64, y: f64, clamped: bool) -> Option<(f64, f64)> {
        if self.is_empty() || !x.is_finite() || !y.is_finite() {
            return None;
        }
        let (fx, fy) = ((x - self.x) / self.w, (y - self.y) / self.h);
        if !(0.0..=1.0).contains(&fx) || !(0.0..=1.0).contains(&fy) {
            return clamped.then(|| (fx.clamp(0.0, 1.0), fy.clamp(0.0, 1.0)));
        }
        Some((fx, fy))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    /// Middle and the rest: the host has no event for them (iOS sends none either).
    Other,
}

#[derive(Debug, Default)]
pub struct Pointer {
    /// The cursor in window pixels, while it is over the window.
    at: Option<(f64, f64)>,
    /// The newest position sent, and the newest held back.
    sent: Option<(f64, f64)>,
    pending: Option<(f64, f64)>,
    last_move_at: f64,
    left: bool,
    right: bool,
}

impl Pointer {
    /// The last position the cursor had, as a fraction clamped onto the picture.
    pub fn last_fraction(&self, rect: &Rect) -> Option<(f64, f64)> {
        self.at.and_then(|(x, y)| rect.fraction(x, y, true))
    }

    pub fn buttons_down(&self) -> bool {
        self.left || self.right
    }

    fn set(&mut self, b: Button, v: bool) {
        match b {
            Button::Left => self.left = v,
            Button::Right => self.right = v,
            Button::Other => {}
        }
    }

    fn drag(&self) -> bool {
        self.left || self.right
    }

    /// The cursor moved to window pixel `(x, y)`.
    pub fn moved(&mut self, now: f64, rect: &Rect, x: f64, y: f64, streaming: bool) -> Vec<InputEvent> {
        self.at = Some((x, y));
        if !streaming {
            return vec![];
        }
        let Some(p) = rect.fraction(x, y, self.drag()) else {
            self.pending = None;
            return vec![];
        };
        if self.sent == Some(p) {
            self.pending = None;
            return vec![];
        }
        if now - self.last_move_at >= MOVE_INTERVAL || self.sent.is_none() {
            self.pending = None;
            self.emit_move(now, p)
        } else {
            self.pending = Some(p);
            vec![]
        }
    }

    fn emit_move(&mut self, now: f64, p: (f64, f64)) -> Vec<InputEvent> {
        self.sent = Some(p);
        self.last_move_at = now;
        vec![InputEvent::Pointer { action: PointerAction::Move, x: p.0, y: p.1 }]
    }

    /// The held-back move, once its interval is over.
    pub fn poll(&mut self, now: f64) -> Vec<InputEvent> {
        match self.pending {
            Some(p) if now - self.last_move_at >= MOVE_INTERVAL => {
                self.pending = None;
                self.emit_move(now, p)
            }
            _ => vec![],
        }
    }

    /// When [`poll`](Self::poll) next has something.
    pub fn next_deadline(&self) -> Option<f64> {
        self.pending.map(|_| self.last_move_at + MOVE_INTERVAL)
    }

    /// The cursor left the window: held-back moves are dropped (a button held keeps its state).
    pub fn left_window(&mut self) {
        self.at = None;
        self.pending = None;
    }

    /// A button changed. A down needs a point on the picture and a move to it first (the Mac's cursor
    /// is where the last event left it); an up is sent only for a down that was, clamped to the picture.
    pub fn button(&mut self, now: f64, rect: &Rect, b: Button, down: bool, streaming: bool) -> Vec<InputEvent> {
        let (act_down, act_up, cur) = match b {
            Button::Left => (PointerAction::LeftDown, PointerAction::LeftUp, self.left),
            Button::Right => (PointerAction::RightDown, PointerAction::RightUp, self.right),
            Button::Other => return vec![],
        };
        let at = self.at;
        if down {
            if cur || !streaming {
                return vec![];
            }
            let Some(p) = at.and_then(|(x, y)| rect.fraction(x, y, false)) else { return vec![] };
            self.set(b, true);
            self.pending = None;
            let mut out = vec![];
            if self.sent != Some(p) {
                out.extend(self.emit_move(now, p));
            }
            out.push(InputEvent::Pointer { action: act_down, x: p.0, y: p.1 });
            out
        } else {
            if !cur {
                return vec![];
            }
            self.set(b, false);
            // Held-back moves are superseded by the release's own position.
            self.pending = None;
            let p = at.and_then(|(x, y)| rect.fraction(x, y, true)).or(self.sent).unwrap_or((0.5, 0.5));
            self.sent = Some(p);
            vec![InputEvent::Pointer { action: act_up, x: p.0, y: p.1 }]
        }
    }

    /// Every button still down goes up (focus lost, the session ending), where the cursor last was.
    pub fn release_all(&mut self, rect: &Rect) -> Vec<InputEvent> {
        let mut out = vec![];
        for b in [Button::Left, Button::Right] {
            out.extend(self.button(0.0, rect, b, false, true));
        }
        self.pending = None;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1000x500 window showing a 16:9 frame: the picture is 888.9 x 500 at x = 55.5.
    fn rect() -> Rect {
        video_rect(1000.0, 500.0, 1600.0, 900.0)
    }

    fn mv(x: f64, y: f64) -> InputEvent {
        InputEvent::Pointer { action: PointerAction::Move, x, y }
    }

    #[test]
    fn the_letterbox_is_left_out() {
        let r = rect();
        assert!((r.x - 55.555).abs() < 0.01 && r.h == 500.0);
        assert_eq!(r.fraction(10.0, 100.0, false), None);
        assert_eq!(r.fraction(10.0, 100.0, true), Some((0.0, 0.2)));
        let (x, y) = r.fraction(500.0, 250.0, false).unwrap();
        assert!((x - 0.5).abs() < 1e-9 && (y - 0.5).abs() < 1e-9);
        assert_eq!(Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }.fraction(1.0, 1.0, true), None);
    }

    #[test]
    fn moves_are_coalesced_to_the_newest_per_interval() {
        let (r, mut p) = (rect(), Pointer::default());
        assert_eq!(p.moved(1.0, &r, 500.0, 250.0, true).len(), 1);
        assert!(p.moved(1.002, &r, 510.0, 250.0, true).is_empty());
        assert!(p.moved(1.004, &r, 520.0, 250.0, true).is_empty());
        assert_eq!(p.next_deadline(), Some(1.008));
        assert!(p.poll(1.005).is_empty());
        let out = p.poll(1.009);
        assert_eq!(out.len(), 1);
        let InputEvent::Pointer { x, .. } = out[0] else { panic!() };
        assert!((x - (520.0 - r.x) / r.w).abs() < 1e-9, "the newest wins");
        assert!(p.poll(2.0).is_empty());
        assert_eq!(p.next_deadline(), None);
        // A later move after the interval goes at once.
        assert_eq!(p.moved(1.1, &r, 530.0, 250.0, true).len(), 1);
    }

    #[test]
    fn a_click_is_a_move_then_the_button() {
        let (r, mut p) = (rect(), Pointer::default());
        p.moved(0.0, &r, 300.0, 100.0, true);
        p.moved(0.001, &r, 400.0, 200.0, true); // held back
        let down = p.button(0.002, &r, Button::Left, true, true);
        let (fx, fy) = r.fraction(400.0, 200.0, false).unwrap();
        assert_eq!(
            down,
            vec![mv(fx, fy), InputEvent::Pointer { action: PointerAction::LeftDown, x: fx, y: fy }]
        );
        let up = p.button(0.1, &r, Button::Left, false, true);
        assert_eq!(up, vec![InputEvent::Pointer { action: PointerAction::LeftUp, x: fx, y: fy }]);
        assert!(p.button(0.2, &r, Button::Left, false, true).is_empty(), "no up without a down");
    }

    #[test]
    fn a_click_in_the_black_bars_is_not_sent_nor_its_release() {
        let (r, mut p) = (rect(), Pointer::default());
        p.moved(0.0, &r, 10.0, 100.0, true);
        assert!(p.button(0.1, &r, Button::Left, true, true).is_empty());
        assert!(p.button(0.2, &r, Button::Left, false, true).is_empty());
    }

    #[test]
    fn a_drag_out_of_the_picture_is_clamped_and_the_release_outside_is_sent() {
        let (r, mut p) = (rect(), Pointer::default());
        p.moved(0.0, &r, 500.0, 250.0, true);
        p.button(0.01, &r, Button::Left, true, true);
        // Dragging into the left bar: clamped to the edge, not dropped.
        let out = p.moved(0.1, &r, 5.0, 250.0, true);
        assert_eq!(out, vec![mv(0.0, 0.5)]);
        let out = p.moved(0.2, &r, 5.0, 900.0, true);
        assert_eq!(out, vec![mv(0.0, 1.0)]);
        // Released outside the window altogether.
        p.left_window();
        let up = p.button(0.3, &r, Button::Left, false, true);
        assert_eq!(up.len(), 1, "the up is sent so the button never sticks");
        assert!(matches!(up[0], InputEvent::Pointer { action: PointerAction::LeftUp, .. }));
    }

    #[test]
    fn right_button_other_buttons_and_idle() {
        let (r, mut p) = (rect(), Pointer::default());
        p.moved(0.0, &r, 500.0, 250.0, true);
        assert!(p.button(0.1, &r, Button::Other, true, true).is_empty());
        let d = p.button(0.1, &r, Button::Right, true, true);
        assert!(matches!(d.last(), Some(InputEvent::Pointer { action: PointerAction::RightDown, .. })));
        assert!(p.buttons_down());
        assert!(p.moved(0.5, &r, 600.0, 250.0, false).is_empty(), "nothing streams");
        assert!(p.button(0.6, &r, Button::Left, true, false).is_empty());
        let u = p.button(0.7, &r, Button::Right, false, false);
        assert!(matches!(u[0], InputEvent::Pointer { action: PointerAction::RightUp, .. }), "an up is sent even then");
    }

    #[test]
    fn release_all_lets_go_of_both_buttons() {
        let (r, mut p) = (rect(), Pointer::default());
        p.moved(0.0, &r, 500.0, 250.0, true);
        p.button(0.1, &r, Button::Left, true, true);
        p.button(0.1, &r, Button::Right, true, true);
        assert_eq!(p.release_all(&r).len(), 2);
        assert!(!p.buttons_down());
        assert!(p.release_all(&r).is_empty());
    }

    #[test]
    fn unchanged_positions_are_not_resent() {
        let (r, mut p) = (rect(), Pointer::default());
        assert_eq!(p.moved(0.0, &r, 500.0, 250.0, true).len(), 1);
        assert!(p.moved(1.0, &r, 500.0, 250.0, true).is_empty());
    }
}

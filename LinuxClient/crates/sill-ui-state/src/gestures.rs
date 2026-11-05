//! Kind 28, the six trackpad gestures by name, sent only to a host whose window list says `gestures` 1
//! or more. The host takes four a second per connection; the device holds itself to that.

use crate::clipboard::admits;
use sill_proto::TrackpadGesture;

pub const PER_SECOND: usize = 4;

#[derive(Debug, Default)]
pub struct GestureGate {
    host: Option<i64>,
    arrivals: Vec<f64>,
}

impl GestureGate {
    pub fn on_host_list(&mut self, gestures: Option<i64>) {
        self.host = gestures;
    }

    /// The Mac takes gestures (the panel shows them).
    pub fn available(&self) -> bool {
        self.host.unwrap_or(0) >= TrackpadGesture::GENERATION
    }

    pub fn send(&mut self, now: f64, name: &str, fingers: i64) -> Option<TrackpadGesture> {
        if !self.available() {
            return None;
        }
        let g = TrackpadGesture::named(name, fingers)?;
        let (ok, kept) = admits(&self.arrivals, now);
        self.arrivals = kept;
        ok.then_some(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_to_a_host_that_takes_them() {
        let mut g = GestureGate::default();
        assert!(g.send(0.0, "swipeUp", 3).is_none());
        g.on_host_list(Some(0));
        assert!(g.send(0.0, "swipeUp", 3).is_none());
        g.on_host_list(Some(1));
        assert!(g.available());
        assert_eq!(g.send(0.0, "swipeUp", 3).unwrap().gesture, "swipeUp");
        assert!(g.send(0.1, "rotate", 3).is_none());
        assert!(g.send(0.1, "pinch", 2).is_none());
    }

    #[test]
    fn four_a_second() {
        let mut g = GestureGate::default();
        g.on_host_list(Some(1));
        let n = (0..6).filter(|i| g.send(*i as f64 * 0.05, "swipeLeft", 3).is_some()).count();
        assert_eq!(n, PER_SECOND);
        assert!(g.send(1.2, "swipeLeft", 4).is_some());
    }
}

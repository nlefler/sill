//! Kind 28, a three- or four-finger trackpad gesture by name (Gesture.swift). Sent only to a host whose
//! window list says `gestures` 1 or more.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackpadGesture {
    pub gesture: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingers: Option<i64>,
}

impl TrackpadGesture {
    pub const SWIPE_UP: &'static str = "swipeUp";
    pub const SWIPE_DOWN: &'static str = "swipeDown";
    pub const SWIPE_LEFT: &'static str = "swipeLeft";
    pub const SWIPE_RIGHT: &'static str = "swipeRight";
    pub const PINCH: &'static str = "pinch";
    pub const SPREAD: &'static str = "spread";
    pub const NAMES: [&'static str; 6] = [Self::SWIPE_UP, Self::SWIPE_DOWN, Self::SWIPE_LEFT, Self::SWIPE_RIGHT, Self::PINCH, Self::SPREAD];
    /// The window list's `gestures` this build understands.
    pub const GENERATION: i64 = 1;

    /// `None` for a name the Mac does not take; `fingers` is 3 or 4.
    pub fn named(name: &str, fingers: i64) -> Option<TrackpadGesture> {
        (Self::NAMES.contains(&name) && (3..=4).contains(&fingers)).then(|| TrackpadGesture { gesture: name.to_string(), fingers: Some(fingers) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode_json;

    #[test]
    fn six_names_three_or_four_fingers() {
        assert_eq!(encode_json(&TrackpadGesture::named("swipeUp", 3).unwrap()), br#"{"gesture":"swipeUp","fingers":3}"#);
        assert!(TrackpadGesture::named("rotate", 3).is_none());
        assert!(TrackpadGesture::named("pinch", 2).is_none());
        assert!(TrackpadGesture::named("spread", 5).is_none());
        assert_eq!(TrackpadGesture::NAMES.len(), 6);
    }
}

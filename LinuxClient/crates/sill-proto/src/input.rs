use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerAction {
    #[serde(rename = "move")]
    Move,
    #[serde(rename = "leftDown")]
    LeftDown,
    #[serde(rename = "leftUp")]
    LeftUp,
    #[serde(rename = "rightDown")]
    RightDown,
    #[serde(rename = "rightUp")]
    RightUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollPhase {
    #[serde(rename = "began")]
    Began,
    #[serde(rename = "ended")]
    Ended,
    #[serde(rename = "momentumBegan")]
    MomentumBegan,
    #[serde(rename = "momentumEnded")]
    MomentumEnded,
}

/// Modifier bits as CGEventFlags / UIKeyModifierFlags have them.
pub mod modifiers {
    pub const SHIFT: u64 = 0x2_0000;
    pub const CONTROL: u64 = 0x4_0000;
    pub const OPTION: u64 = 0x8_0000;
    pub const COMMAND: u64 = 0x10_0000;
}

/// Client → host input (kind 8). Positions are fractions 0…1 of the streamed frame. Swift's
/// synthesized Codable writes a case's unlabeled first value as `_0` and its labeled ones by name:
/// `{"pointer":{"_0":"move","x":0.5,"y":0.5}}`, `{"scroll":{"x":..,"y":..,"dx":..,"dy":..}}`,
/// `{"text":{"_0":"abc"}}`, `{"scrollGesture":{"_0":"began","x":..,"y":..}}`,
/// `{"key":{"hidUsage":4,"down":true,"modifiers":0}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "InputRepr", into = "InputRepr")]
pub enum InputEvent {
    Pointer { action: PointerAction, x: f64, y: f64 },
    /// Natural-scrolling sign: positive `dy` moves the content down. Fractions of the frame.
    Scroll { x: f64, y: f64, dx: f64, dy: f64 },
    /// Text typed (an input method's commit). "\n", "\t" and "\u{8}" arrive as keys on the host.
    Text(String),
    ScrollGesture { phase: ScrollPhase, x: f64, y: f64 },
    /// A key by USB HID usage with [`modifiers`] bits.
    Key { hid_usage: u16, down: bool, modifiers: u64 },
}

#[derive(Serialize, Deserialize)]
struct PointerArgs {
    #[serde(rename = "_0")]
    action: PointerAction,
    x: f64,
    y: f64,
}

#[derive(Serialize, Deserialize)]
struct ScrollArgs {
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
}

#[derive(Serialize, Deserialize)]
struct TextArgs {
    #[serde(rename = "_0")]
    text: String,
}

#[derive(Serialize, Deserialize)]
struct GestureArgs {
    #[serde(rename = "_0")]
    phase: ScrollPhase,
    x: f64,
    y: f64,
}

#[derive(Serialize, Deserialize)]
struct KeyArgs {
    #[serde(rename = "hidUsage")]
    hid_usage: u16,
    down: bool,
    modifiers: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct InputRepr {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pointer: Option<PointerArgs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scroll: Option<ScrollArgs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<TextArgs>,
    #[serde(rename = "scrollGesture", default, skip_serializing_if = "Option::is_none")]
    scroll_gesture: Option<GestureArgs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<KeyArgs>,
}

impl From<InputEvent> for InputRepr {
    fn from(e: InputEvent) -> InputRepr {
        let mut r = InputRepr::default();
        match e {
            InputEvent::Pointer { action, x, y } => r.pointer = Some(PointerArgs { action, x, y }),
            InputEvent::Scroll { x, y, dx, dy } => r.scroll = Some(ScrollArgs { x, y, dx, dy }),
            InputEvent::Text(text) => r.text = Some(TextArgs { text }),
            InputEvent::ScrollGesture { phase, x, y } => r.scroll_gesture = Some(GestureArgs { phase, x, y }),
            InputEvent::Key { hid_usage, down, modifiers } => {
                r.key = Some(KeyArgs { hid_usage, down, modifiers })
            }
        }
        r
    }
}

impl TryFrom<InputRepr> for InputEvent {
    type Error = &'static str;
    fn try_from(r: InputRepr) -> Result<InputEvent, &'static str> {
        match (r.pointer, r.scroll, r.text, r.scroll_gesture, r.key) {
            (Some(p), None, None, None, None) => Ok(InputEvent::Pointer { action: p.action, x: p.x, y: p.y }),
            (None, Some(s), None, None, None) => Ok(InputEvent::Scroll { x: s.x, y: s.y, dx: s.dx, dy: s.dy }),
            (None, None, Some(t), None, None) => Ok(InputEvent::Text(t.text)),
            (None, None, None, Some(g), None) => Ok(InputEvent::ScrollGesture { phase: g.phase, x: g.x, y: g.y }),
            (None, None, None, None, Some(k)) => {
                Ok(InputEvent::Key { hid_usage: k.hid_usage, down: k.down, modifiers: k.modifiers })
            }
            _ => Err("an InputEvent has exactly one case"),
        }
    }
}

//! The Sill client's input, as the host's kind 8: keys (winit's physical keys as USB HID usages, the
//! Mac's modifiers, held keys let go on focus loss), the pointer (window pixels to fractions of the
//! video frame through the letterbox, coalesced moves, buttons that never stick) and scrolling (wheel
//! notches, touchpad gestures, a generated coast). Pure state machines with an injected clock; the
//! window (`sill-app`) feeds them winit's events and sends what they return.

pub mod engine;
pub mod feed;
pub mod keyboard;
pub mod keymap;
pub mod mods;
pub mod pointer;
pub mod scroll;

pub use engine::{moves_pointer, Delta, InputConfig, InputEngine};
pub use feed::PointerFeed;
pub use keyboard::{Keyboard, Typing};
pub use keymap::hid_usage;
pub use mods::{MacMod, ModifierMap, Side};
pub use pointer::{video_rect, Button, Pointer, Rect, MOVE_INTERVAL};
pub use scroll::{Phase, ScrollConfig, Scroller};

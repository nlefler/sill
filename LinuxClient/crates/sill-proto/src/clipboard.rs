//! Kind 32, the clipboard, both ways (Clipboard.swift). `{}` subscribes (text alone), `{"stop":true}`
//! ends it, `text` becomes the receiver's clipboard. `image` is reserved for a later generation; it is
//! kept as the base64 string Swift's `Data` encodes to and never produced here.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Clipboard {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<bool>,
}

impl Clipboard {
    /// Text at most 256 KB of UTF-8.
    pub const MAX_TEXT_BYTES: usize = 256 * 1024;
    /// `ClipboardPolicy.generation`: the window list's `clipboard` this build understands.
    pub const GENERATION: i64 = 1;

    /// Plain text; `None` for an empty string or one over the cap (`ClipboardPolicy.payload`).
    pub fn text(text: &str) -> Option<Clipboard> {
        if text.is_empty() || text.len() > Self::MAX_TEXT_BYTES {
            return None;
        }
        Some(Clipboard { text: Some(text.to_string()), ..Default::default() })
    }
    /// `ClipboardPolicy.subscription`: `{}`.
    pub fn subscription() -> Clipboard {
        Clipboard::default()
    }
    /// `ClipboardPolicy.stopping`: `{"stop":true}`.
    pub fn stopping() -> Clipboard {
        Clipboard { stop: Some(true), ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_json, encode_json};

    #[test]
    fn wire_shapes() {
        assert_eq!(encode_json(&Clipboard::subscription()), b"{}");
        assert_eq!(encode_json(&Clipboard::stopping()), br#"{"stop":true}"#);
        assert_eq!(encode_json(&Clipboard::text("hi").unwrap()), br#"{"text":"hi"}"#);
        let c: Clipboard = decode_json(br#"{"text":"a","image":"AAAA","future":1}"#).unwrap();
        assert_eq!(c.text.as_deref(), Some("a"));
    }

    #[test]
    fn the_cap_and_empty() {
        assert!(Clipboard::text("").is_none());
        assert!(Clipboard::text(&"x".repeat(Clipboard::MAX_TEXT_BYTES)).is_some());
        assert!(Clipboard::text(&"x".repeat(Clipboard::MAX_TEXT_BYTES + 1)).is_none());
        // Bytes, not characters.
        assert!(Clipboard::text(&"\u{e9}".repeat(Clipboard::MAX_TEXT_BYTES / 2 + 1)).is_none());
    }
}

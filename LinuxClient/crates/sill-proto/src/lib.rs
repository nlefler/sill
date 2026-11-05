//! Sill's wire format: the 14-byte header, the message kinds and the JSON payloads.
//!
//! Port of `Sources/StreamProtocol` (the Swift side is the source of truth). No I/O, no async:
//! a session layer feeds bytes to [`Decoder`] and writes what [`Message::serialize`] gives.
//!
//! Rules carried over from the Swift side (CLAUDE.md, "Compatibility floor"): fields added later are
//! optional, a kind this build does not know is skipped and never fatal, unknown JSON keys are
//! ignored, and every size cap is held in both directions.

mod blobs;
mod clipboard;
mod gesture;
mod menu;
mod compat;
mod header;
mod input;
mod pointer;
mod remote;
mod settings;
mod switcher;
mod viewport;

pub use blobs::*;
pub use clipboard::*;
pub use gesture::*;
pub use menu::*;
pub use compat::*;
pub use header::*;
pub use input::*;
pub use pointer::*;
pub use remote::*;
pub use settings::*;
pub use switcher::*;
pub use viewport::*;

use serde::{de::DeserializeOwned, Serialize};

/// Encode a JSON payload. Swift's encoder omits nil optionals; every optional field here is
/// `skip_serializing_if = "Option::is_none"`, so the bytes carry the same keys.
pub fn encode_json<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

/// Decode a JSON payload; `None` for anything that does not decode (the caller skips it).
pub fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    serde_json::from_slice(bytes).ok()
}

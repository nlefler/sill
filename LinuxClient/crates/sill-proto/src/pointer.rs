use serde::{Deserialize, Serialize};

/// Kind 26, host → device: where the Mac's pointer is while this device is not moving it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MacPointer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inside: Option<bool>,
    /// Input messages (kind 8) the host had read on this connection when it sent this. A device that
    /// has sent more since drops the report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<i64>,
}

impl MacPointer {
    /// Where to draw the Mac's pointer: only `inside`, with finite x and y.
    pub fn position(&self) -> Option<(f64, f64)> {
        match (self.inside, self.x, self.y) {
            (Some(true), Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some((x, y)),
            _ => None,
        }
    }
}

use serde::{Deserialize, Serialize};

/// `SillProtocol.current`.
pub const PROTOCOL: i64 = 1;

/// Kind 23: the first message of every session connection, never on a pairing connection. Every
/// field optional. `audio` stays unset: this client plays no sound (v2), so a host sends no kind 43.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    #[serde(rename = "appVersion", default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(rename = "protocol", default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<Vec<String>>,
}

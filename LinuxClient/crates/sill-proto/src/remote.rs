use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacAddress {
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<i64>,
    /// "vpn", "lan" or "internet".
    pub kind: String,
    pub via: String,
}

/// What kind 18 signs: who this Mac is and how to reach it from afar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MacInfo {
    pub v: i64,
    #[serde(rename = "macID")]
    pub mac_id: String,
    pub name: String,
    #[serde(rename = "issuedAt")]
    pub issued_at: f64,
    #[serde(rename = "remoteAccess")]
    pub remote_access: bool,
    #[serde(rename = "remotePort")]
    pub remote_port: i64,
    pub internet: bool,
    pub addresses: Vec<MacAddress>,
}

/// Kind 18: `info` is base64 of the MacInfo JSON, `key` base64 of the Mac's 65-byte X9.63 P-256 point,
/// `sig` base64 of a DER ECDSA-SHA256 signature over the info bytes. Verifying it is the session
/// layer's job (it needs a P-256 implementation); this only carries the fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedMacInfo {
    pub info: String,
    pub key: String,
    pub sig: String,
}

/// Kind 19: the one message of a pairing connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRequest {
    pub v: i64,
    /// "qr" or "code" (a proof for the pairing window); "ask" and "cancel" are not used by this client.
    pub method: String,
    /// base64url(proof_D); "" with "ask".
    pub proof: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cable: Option<bool>,
}

impl PairRequest {
    pub const VERSION: i64 = 1;
    pub const QR: &'static str = "qr";
    pub const CODE: &'static str = "code";
}

/// Kind 20.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PairResult {
    pub ok: bool,
    /// ok: base64url(proof_M), checked before anything is saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
    #[serde(rename = "macID", default, skip_serializing_if = "Option::is_none")]
    pub mac_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "recognitionKey", default, skip_serializing_if = "Option::is_none")]
    pub recognition_key: Option<String>,
    /// "code", "closed", "expired", "stopped", "busy", "shown", "openOnMac", "locked", or one a later
    /// host adds (then `message` says it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(rename = "triesLeft", default, skip_serializing_if = "Option::is_none")]
    pub tries_left: Option<i64>,
    #[serde(rename = "retryAfter", default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl PairResult {
    pub const KNOWN_REASONS: [&'static str; 8] =
        ["code", "closed", "expired", "stopped", "busy", "shown", "openOnMac", "locked"];

    /// For a refusal with a reason this build does not know: the host's `message`, to show as it is.
    pub fn unknown_reason_message(&self) -> Option<&str> {
        if self.ok {
            return None;
        }
        if let Some(r) = &self.reason {
            if Self::KNOWN_REASONS.contains(&r.as_str()) {
                return None;
            }
        }
        self.message.as_deref().filter(|m| !m.trim().is_empty())
    }
}

/// Kind 22, host → device: why the host is about to close this session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goodbye {
    /// "removed", "remoteOff", "internetOff", "quit", "busy", "pairingRequired", "update", or one a
    /// later host adds: a notice, shown as `message` says.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(rename = "minimumVersion", default, skip_serializing_if = "Option::is_none")]
    pub minimum_version: Option<String>,
    /// Whether to reconnect; only an explicit true reconnects after a notice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnect: Option<bool>,
}

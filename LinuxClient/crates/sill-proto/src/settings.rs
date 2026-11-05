use serde::{Deserialize, Serialize};

/// The settings the Mac's menu shows (kinds 16 and 17; HostSettings.swift).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamSettings {
    #[serde(rename = "maxFPS")]
    pub max_fps: i64,
    /// Bits per second per 60 fps.
    pub bitrate: i64,
    #[serde(rename = "captureScale")]
    pub capture_scale: f64,
    #[serde(rename = "prioritizeSpeed")]
    pub prioritize_speed: bool,
    #[serde(rename = "virtualDisplay")]
    pub virtual_display: bool,
    #[serde(rename = "directWireless", default, skip_serializing_if = "Option::is_none")]
    pub direct_wireless: Option<bool>,
    #[serde(rename = "sendAudio", default, skip_serializing_if = "Option::is_none")]
    pub send_audio: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunningStream {
    pub width: i64,
    pub height: i64,
    pub fps: i64,
    pub mbps: i64,
    #[serde(rename = "onVirtualDisplay")]
    pub on_virtual_display: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AwayQuality {
    #[serde(rename = "homeBitrate")]
    pub home_bitrate: i64,
    #[serde(rename = "homeCaptureScale")]
    pub home_capture_scale: f64,
    #[serde(rename = "awayBitrate")]
    pub away_bitrate: i64,
    #[serde(rename = "awayCaptureScale")]
    pub away_capture_scale: f64,
    #[serde(rename = "thisConnectionAway")]
    pub this_connection_away: bool,
    #[serde(rename = "awayRunning")]
    pub away_running: bool,
}

/// `state` is a string ("behind", "stalled"); one this build does not know reads as keeping up.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkReport {
    pub state: String,
    #[serde(rename = "withheldPerSecond")]
    pub withheld_per_second: i64,
    pub bitrate: i64,
    #[serde(rename = "carriedKbps", default, skip_serializing_if = "Option::is_none")]
    pub carried_kbps: Option<i64>,
    #[serde(rename = "suggestedBitrate", default, skip_serializing_if = "Option::is_none")]
    pub suggested_bitrate: Option<i64>,
    #[serde(rename = "suggestedCaptureScale", default, skip_serializing_if = "Option::is_none")]
    pub suggested_capture_scale: Option<f64>,
}

/// Kind 16, host → device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostSettingsState {
    pub settings: StreamSettings,
    pub persistent: bool,
    #[serde(rename = "virtualDisplayAvailable")]
    pub virtual_display_available: bool,
    #[serde(rename = "virtualDisplayNote", default, skip_serializing_if = "Option::is_none")]
    pub virtual_display_note: Option<String>,
    #[serde(rename = "softwareEncoder")]
    pub software_encoder: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<RunningStream>,
    /// Set in the reply to one change: that change's token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answering: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub away: Option<AwayQuality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkReport>,
    #[serde(rename = "audioNote", default, skip_serializing_if = "Option::is_none")]
    pub audio_note: Option<String>,
    /// False for an unpaired session at an open door (Direct Wireless is then refused).
    #[serde(rename = "sessionPaired", default, skip_serializing_if = "Option::is_none")]
    pub session_paired: Option<bool>,
}

/// Kind 17, device → host: only the fields one control changed, absolute values, a token that
/// increases per device process.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostSettingsChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<i64>,
    #[serde(rename = "maxFPS", default, skip_serializing_if = "Option::is_none")]
    pub max_fps: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<i64>,
    #[serde(rename = "captureScale", default, skip_serializing_if = "Option::is_none")]
    pub capture_scale: Option<f64>,
    #[serde(rename = "prioritizeSpeed", default, skip_serializing_if = "Option::is_none")]
    pub prioritize_speed: Option<bool>,
    #[serde(rename = "virtualDisplay", default, skip_serializing_if = "Option::is_none")]
    pub virtual_display: Option<bool>,
    #[serde(rename = "directWireless", default, skip_serializing_if = "Option::is_none")]
    pub direct_wireless: Option<bool>,
    #[serde(rename = "sendAudio", default, skip_serializing_if = "Option::is_none")]
    pub send_audio: Option<bool>,
}

/// The quality presets, bits per second per 60 fps (`QualityPreset`).
pub const QUALITY_PRESETS: [(&str, i64); 7] = [
    ("Low", 4_000_000),
    ("Efficient", 8_000_000),
    ("Balanced", 15_000_000),
    ("High", 25_000_000),
    ("Pro", 40_000_000),
    ("Ultra", 80_000_000),
    ("Extreme", 150_000_000),
];

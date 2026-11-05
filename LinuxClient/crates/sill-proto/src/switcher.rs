use serde::{Deserialize, Serialize};

/// What the host streams (kind 6; `WindowList.active`). Swift's synthesized Codable writes
/// `{"none":{}}`, `{"desktop":{}}` and `{"window":{"_0":ID}}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "SourceRepr", into = "SourceRepr")]
pub enum StreamSource {
    None,
    Window(u32),
    Desktop,
}

#[derive(Serialize, Deserialize)]
struct Empty {}

#[derive(Serialize, Deserialize)]
struct WindowArg {
    #[serde(rename = "_0")]
    id: u32,
}

#[derive(Serialize, Deserialize, Default)]
struct SourceRepr {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    none: Option<Empty>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window: Option<WindowArg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    desktop: Option<Empty>,
}

impl From<StreamSource> for SourceRepr {
    fn from(s: StreamSource) -> SourceRepr {
        let mut r = SourceRepr::default();
        match s {
            StreamSource::None => r.none = Some(Empty {}),
            StreamSource::Window(id) => r.window = Some(WindowArg { id }),
            StreamSource::Desktop => r.desktop = Some(Empty {}),
        }
        r
    }
}

impl TryFrom<SourceRepr> for StreamSource {
    type Error = &'static str;
    fn try_from(r: SourceRepr) -> Result<StreamSource, &'static str> {
        match (r.none, r.window, r.desktop) {
            (Some(_), None, None) => Ok(StreamSource::None),
            (None, Some(w), None) => Ok(StreamSource::Window(w.id)),
            (None, None, Some(_)) => Ok(StreamSource::Desktop),
            _ => Err("a StreamSource has exactly one case"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    #[serde(rename = "appName")]
    pub app_name: String,
    #[serde(rename = "bundleID")]
    pub bundle_id: String,
    /// Points.
    pub width: i64,
    pub height: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    #[serde(rename = "bundleID")]
    pub bundle_id: String,
}

/// Kind 2. Every field after `active` is optional: nil from a host that predates it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowList {
    #[serde(rename = "macName")]
    pub mac_name: String,
    pub windows: Vec<WindowInfo>,
    pub active: StreamSource,
    #[serde(rename = "launchID", default, skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<String>,
    #[serde(rename = "hostVersion", default, skip_serializing_if = "Option::is_none")]
    pub host_version: Option<String>,
    #[serde(rename = "protocol", default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<i64>,
    /// Trackpad gestures (kind 28): 1 or more means the host takes them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gestures: Option<i64>,
    /// Clipboard (kind 32): 1 is text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard: Option<i64>,
    /// 1 or more: the host repeats a key held between its down and its up.
    #[serde(rename = "keyRepeat", default, skip_serializing_if = "Option::is_none")]
    pub key_repeat: Option<i64>,
    /// The Mac is locked, or another user has its console: no windows are listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    #[serde(rename = "offConsole", default, skip_serializing_if = "Option::is_none")]
    pub off_console: Option<bool>,
}

impl WindowList {
    /// Whether this host repeats held keys, so a hardware keyboard's characters go as keys.
    pub fn host_repeats_keys(&self) -> bool {
        self.key_repeat.unwrap_or(0) >= 1
    }
}

/// Kind 7.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchApp {
    #[serde(rename = "bundleID")]
    pub bundle_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowAction {
    #[serde(rename = "close")]
    Close,
    #[serde(rename = "minimize")]
    Minimize,
    #[serde(rename = "fullScreen")]
    FullScreen,
}

/// Kind 15: a window's traffic lights.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowCommand {
    pub id: u32,
    pub action: WindowAction,
}

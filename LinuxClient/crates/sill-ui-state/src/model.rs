//! What the window draws: a snapshot the session side publishes after every change, and the intents
//! the window sends back. Both are plain data (no egui, no wgpu).

use crate::locked::Shade;
use crate::switcher::Image;
use sill_proto::{StreamSettings, StreamSource};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub enum ConnState {
    /// Nothing is connected (the Mac list shows).
    Idle,
    Connecting(String),
    Connected,
    /// The ladder's line: "Reconnecting to Mac...", "Mac isn't answering...", "Stopped trying...".
    Reconnecting(String),
    /// The session ended for good; the text says why.
    Ended(String),
}

#[derive(Clone, Debug)]
pub struct WindowEntry {
    pub id: u32,
    pub title: String,
    pub app: String,
    pub bundle_id: String,
    pub width: i64,
    pub height: i64,
    pub thumb: Option<Arc<Image>>,
    pub icon: Option<Arc<Image>>,
}

#[derive(Clone, Debug)]
pub struct AppEntry {
    pub name: String,
    pub bundle_id: String,
    pub icon: Option<Arc<Image>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cover {
    pub title: String,
    pub detail: Option<String>,
    pub shade: Shade,
}

/// The Settings panel: what is shown (the Mac's word with this device's pending picks over it).
#[derive(Clone, Debug)]
pub struct SettingsView {
    pub shown: StreamSettings,
    pub pending: Vec<crate::ledger::Field>,
    pub persistent: bool,
    pub virtual_display_available: bool,
    pub virtual_display_note: Option<String>,
    pub software_encoder: bool,
    /// "1512 x 982, 60 fps, 15 Mbps" while something runs.
    pub running: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MacRow {
    /// A stable key: the saved Mac ID, else the Bonjour name.
    pub key: String,
    /// "host:port" of its first address while listed (prefills the pairing dialog).
    pub address: Option<String>,
    /// An open door (`p=0`) that is not saved: a click connects with any key.
    pub open_door: bool,
    pub name: String,
    /// "Paired", "Not paired" or "Update Sill on the Mac" (discovery's [`RowClass::word`]).
    pub word: String,
    pub saved: bool,
    pub listed: bool,
    pub dialable: bool,
    pub needs_pairing: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PairingView {
    pub busy: bool,
    pub error: Option<String>,
    /// For a pairing done: "Paired with X."
    pub done: Option<String>,
}

/// One snapshot for the window.
#[derive(Clone, Debug)]
pub struct Model {
    pub conn: ConnState,
    pub mac_name: String,
    pub active: StreamSource,
    pub windows: Vec<WindowEntry>,
    pub apps: Vec<AppEntry>,
    /// "Mac - window title" for the OS window.
    pub title: String,
    pub cover: Option<Cover>,
    /// The switcher's line where the windows would be (locked Mac).
    pub locked_line: Option<String>,
    /// `None` when the host sent no kind 16 (an older Mac): the panel says "Update Sill on <Mac>".
    pub settings: Option<SettingsView>,
    /// A transient line: a refused or unanswered setting.
    pub notice: Option<String>,
    pub macs: Vec<MacRow>,
    pub pairing: PairingView,
    /// Settings > This computer: the clipboard switch, shown while the Mac takes the clipboard.
    pub clipboard: ClipboardView,
    /// The Mac's menus, once a top level has arrived.
    pub menus: Option<crate::menus::MenuView>,
    /// The Mac takes trackpad gestures (kind 28).
    pub gestures: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipboardView {
    pub offered: bool,
    pub on: bool,
}

impl Default for Model {
    fn default() -> Self {
        Model {
            conn: ConnState::Idle,
            mac_name: String::new(),
            active: StreamSource::None,
            windows: vec![],
            apps: vec![],
            title: "Sill".into(),
            cover: None,
            locked_line: None,
            settings: None,
            notice: None,
            macs: vec![],
            pairing: PairingView::default(),
            clipboard: ClipboardView::default(),
            menus: None,
            gestures: false,
        }
    }
}

impl Model {
    /// The one status line for the bar: the ladder's words, why it ended, or the Mac's name.
    pub fn status_line(&self) -> String {
        match &self.conn {
            ConnState::Idle => "Not connected".into(),
            ConnState::Connecting(l) | ConnState::Reconnecting(l) | ConnState::Ended(l) => l.clone(),
            ConnState::Connected => match &self.notice {
                Some(n) => n.clone(),
                None => format!("Connected to {}", if self.mac_name.is_empty() { "the Mac" } else { &self.mac_name }),
            },
        }
    }
}

/// A setting the panel changed. Absolute values; the ledger decides whether anything is sent.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingPick {
    Quality(i64),
    FrameRate(i64),
    /// 2 is Retina, 1 is Standard.
    Scale(f64),
    PrioritizeSpeed(bool),
    VirtualDisplay(bool),
}

/// What the window asks for. Connection intents are the hub's; the rest the controller's.
#[derive(Clone, Debug, PartialEq)]
pub enum UiIntent {
    Pick(StreamSource),
    Launch(String),
    WindowButton { id: u32, action: sill_proto::WindowAction },
    Setting(SettingPick),
    /// Connect to a listed or saved Mac by its row key.
    ConnectMac(String),
    /// A typed address, `host` or `host:port`.
    ConnectAddress(String),
    /// A pasted `sill://pair` link.
    PairLink(String),
    /// 12 digits and the Mac's address.
    PairCode { address: String, code: String },
    ForgetMac(String),
    /// Try the reconnect now.
    Retry,
    /// Leave the session and show the Mac list.
    Disconnect,
    DismissPairing,
    /// The clipboard switch in Settings.
    SetClipboard(bool),
    /// A Mac menu opened: fetch its items (id and the title shown).
    OpenMenu { id: String, title: String },
    /// A Mac menu leaf chosen.
    PressMenu { id: String, title: String },
    /// A three- or four-finger gesture by name.
    Gesture { name: String, fingers: i64 },
}

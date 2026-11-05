//! Window-independent UI state for the Sill client: ports of the Swift rules that decide what the
//! switcher and the Settings panel show and what goes to the Mac (`HostSettingsLedger`,
//! `DiscoveryPolicy.autoDesktop`, `LockedMac`), the switcher model, and the [`controller::Controller`]
//! that turns UI intents and session events into commands. No window, no I/O, time injected.

pub mod auto_desktop;
pub mod clipboard;
pub mod controller;
pub mod gestures;
pub mod ledger;
pub mod locked;
pub mod menus;
pub mod model;
pub mod switcher;

pub use auto_desktop::{AutoDesktop, Watched};
pub use controller::{Controller, Effect};
pub use clipboard::ClipboardSync;
pub use gestures::GestureGate;
pub use menus::{MenuState, MenuView};
pub use ledger::{Field, Ledger};
pub use locked::{LockedMac, Shade, Why};
pub use model::*;
pub use switcher::{Switcher, ThumbCache};

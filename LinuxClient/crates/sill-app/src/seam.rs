//! The seam between the window and the session. M6 (input) and M7 (sources, settings) plug in here:
//! the window sends [`AppCommand`]s (any session [`Command`] passes straight through) and receives
//! [`UiEvent`]s (the host's lists and state).

use sill_media::cursor::CursorImage;
use sill_media::Panel;
use sill_net::Command;
use sill_proto::WindowList;
use std::sync::Arc;

#[derive(Debug)]
pub enum UiEvent {
    /// A decoded picture is in the [`crate::pipeline::FrameSlot`].
    FrameReady,
    /// One line for the status area / title.
    Status(String),
    /// The Mac's cursor shape (kind 14); `None` when it does not decode.
    Cursor(Option<CursorImage>),
    /// The Mac's pointer as fractions of the video frame, `None` off the stream.
    Pointer(Option<(f64, f64)>),
    /// Kind 2, for the switcher (M7).
    WindowList(WindowList),
    /// The session ended; the text says why.
    Closed(String),
    /// Everything the panels draw, after any change.
    Model(Box<sill_ui_state::Model>),
}

#[derive(Debug)]
pub enum AppCommand {
    /// The window's drawable area changed (or the monitor's refresh rate did).
    Panel { panel: Panel, refresh_hz: Option<f64> },
    /// The window became visible/focused or not (keyframe recovery asks only while active).
    Active(bool),
    /// Anything else the session takes: input (M6), source picks, settings (M7).
    Session(Command),
    /// A window intent (a pick, a launch, a setting, pairing...), handled by the controller or the hub.
    Intent(sill_ui_state::UiIntent),
    Quit,
}

/// How the session side reaches whoever draws: a closure (the winit proxy, or a channel in tests).
pub type UiSink = Arc<dyn Fn(UiEvent) + Send + Sync>;

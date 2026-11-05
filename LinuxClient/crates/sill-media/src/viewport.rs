//! What the client tells the host about its window (kind 9), as `StreamClient+Viewport.swift` does for
//! the iPad: the panel in points, the frame rate the screen can show, the local-cursor flag. Held to
//! what a host takes (`ViewportBounds::held`), debounced while a window is dragged to a new size, and
//! sent again only when it changed.

use sill_proto::{Viewport, ViewportBounds};

/// The least time a new size must hold still before it is sent (the first viewport goes at once).
pub const DEBOUNCE: f64 = 0.15;

/// The window's drawable area in physical pixels and the monitor's scale factor (physical per logical).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Panel {
    pub width_px: u32,
    pub height_px: u32,
    pub scale_factor: f64,
}

/// The rate to ask the host for: the monitor's refresh rounded, 30 to 120, 60 when unknown, and at most
/// 60 away from home (`RemoteRoute.capsFrameRate`).
pub fn wanted_fps(refresh_hz: Option<f64>, remote: bool) -> i64 {
    let hz = refresh_hz.filter(|h| h.is_finite() && *h > 0.0).map_or(60, |h| h.round() as i64).clamp(30, 120);
    if remote { hz.min(60) } else { hz }
}

/// The viewport for a panel, held to the host's bounds; `None` while the panel is not measured.
pub fn viewport_for(panel: Panel, refresh_hz: Option<f64>, remote: bool, text_scale: Option<f64>) -> Option<Viewport> {
    let f = if panel.scale_factor.is_finite() && panel.scale_factor > 0.0 { panel.scale_factor } else { 1.0 };
    ViewportBounds::held(&Viewport {
        width: panel.width_px as f64 / f,
        height: panel.height_px as f64 / f,
        scale: text_scale,
        local_cursor: Some(true), // this client draws the Mac's pointer itself
        fps: Some(wanted_fps(refresh_hz, remote)),
    })
}

#[derive(Debug, Default)]
pub struct ViewportSender {
    last_sent: Option<Viewport>,
    /// The latest wanted viewport and when it last changed.
    wanted: Option<(Viewport, f64)>,
}

impl ViewportSender {
    pub fn new() -> Self {
        Self::default()
    }

    /// The window, the monitor or the text scale changed (or is merely restated: no harm).
    pub fn set(&mut self, now: f64, panel: Panel, refresh_hz: Option<f64>, remote: bool, text_scale: Option<f64>) {
        match viewport_for(panel, refresh_hz, remote, text_scale) {
            Some(v) => {
                if self.wanted.as_ref().map(|(w, _)| w) != Some(&v) {
                    self.wanted = Some((v, now));
                }
            }
            None => self.wanted = None,
        }
    }

    /// What to send now, if anything. Counts it as sent.
    pub fn poll(&mut self, now: f64) -> Option<Viewport> {
        let (v, changed) = self.wanted?;
        if self.last_sent == Some(v) {
            return None;
        }
        if self.last_sent.is_some() && now < changed + DEBOUNCE {
            return None;
        }
        self.last_sent = Some(v);
        Some(v)
    }

    /// When `poll` next has something to say, for a timer.
    pub fn next_deadline(&self) -> Option<f64> {
        let (v, changed) = self.wanted?;
        (self.last_sent != Some(v)).then(|| if self.last_sent.is_none() { changed } else { changed + DEBOUNCE })
    }

    /// A new connection: the host knows nothing, so the next viewport goes at once.
    pub fn reset(&mut self) {
        self.last_sent = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(w: u32, h: u32, s: f64) -> Panel {
        Panel { width_px: w, height_px: h, scale_factor: s }
    }

    #[test]
    fn points_are_pixels_over_scale_and_fps_from_monitor() {
        let v = viewport_for(panel(2560, 1440, 2.0), Some(144.0), false, None).unwrap();
        assert_eq!((v.width, v.height, v.fps, v.local_cursor, v.scale), (1280.0, 720.0, Some(120), Some(true), None));
        let v = viewport_for(panel(1000, 600, 1.0), Some(59.94), false, None).unwrap();
        assert_eq!(v.fps, Some(60));
        assert_eq!(viewport_for(panel(1000, 600, 1.0), None, false, None).unwrap().fps, Some(60));
        assert_eq!(viewport_for(panel(1000, 600, 1.0), Some(144.0), true, None).unwrap().fps, Some(60));
        assert_eq!(viewport_for(panel(1000, 600, 1.0), Some(24.0), false, None).unwrap().fps, Some(30));
    }

    #[test]
    fn held_to_the_hosts_bounds() {
        assert!(viewport_for(panel(0, 600, 1.0), None, false, None).is_none());
        let v = viewport_for(panel(100_000, 600, 1.0), None, false, Some(10.0)).unwrap();
        assert_eq!((v.width, v.scale), (16384.0, Some(4.0)));
        assert!(ViewportBounds::refusal(&v).is_none());
        // A nonsense scale factor counts as 1.
        assert_eq!(viewport_for(panel(800, 600, 0.0), None, false, None).unwrap().width, 800.0);
        assert_eq!(viewport_for(panel(800, 600, f64::NAN), None, false, None).unwrap().width, 800.0);
    }

    #[test]
    fn first_goes_at_once_then_debounced_and_only_on_change() {
        let mut s = ViewportSender::new();
        assert_eq!(s.poll(0.0), None);
        s.set(0.0, panel(1000, 600, 1.0), Some(60.0), false, None);
        assert_eq!(s.next_deadline(), Some(0.0));
        let first = s.poll(0.0).unwrap();
        assert_eq!(first.width, 1000.0);
        assert_eq!(s.poll(0.1), None);
        assert_eq!(s.next_deadline(), None);
        // Same again: nothing.
        s.set(1.0, panel(1000, 600, 1.0), Some(60.0), false, None);
        assert_eq!(s.poll(2.0), None);
        // A drag: sizes in quick succession, one viewport after it settles.
        s.set(3.0, panel(1100, 600, 1.0), Some(60.0), false, None);
        s.set(3.05, panel(1200, 600, 1.0), Some(60.0), false, None);
        s.set(3.10, panel(1300, 600, 1.0), Some(60.0), false, None);
        assert_eq!(s.poll(3.2), None);
        assert!((s.next_deadline().unwrap() - 3.25).abs() < 1e-9);
        assert_eq!(s.poll(3.26).unwrap().width, 1300.0);
        assert_eq!(s.poll(10.0), None);
        // Back to an earlier size is a change.
        s.set(11.0, panel(1000, 600, 1.0), Some(60.0), false, None);
        assert_eq!(s.poll(11.2).unwrap().width, 1000.0);
    }

    #[test]
    fn monitor_change_alone_resends_and_reset_resends() {
        let mut s = ViewportSender::new();
        s.set(0.0, panel(800, 600, 1.0), Some(60.0), false, None);
        s.poll(0.0).unwrap();
        s.set(1.0, panel(800, 600, 1.0), Some(120.0), false, None);
        assert_eq!(s.poll(1.2).unwrap().fps, Some(120));
        s.reset();
        assert_eq!(s.poll(1.3).unwrap().fps, Some(120));
    }
}

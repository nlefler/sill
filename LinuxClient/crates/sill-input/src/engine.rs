//! All of a window's input in one place: winit-shaped calls in, kind 8 [`InputEvent`]s out, with the
//! clock injected (`now`, seconds). The caller sends what each call returns, in order, and calls
//! [`InputEngine::poll`] when [`InputEngine::next_deadline`] passes.

use crate::keyboard::{Keyboard, Typing};
use crate::mods::ModifierMap;
use crate::pointer::{video_rect, Button, Pointer, Rect};
use crate::scroll::{Phase, ScrollConfig, Scroller, View};
use sill_proto::InputEvent;
use winit::keyboard::KeyCode;

#[derive(Clone, Copy, Debug, Default)]
pub struct InputConfig {
    pub modifiers: ModifierMap,
    pub scroll: ScrollConfig,
}

/// A scroll delta as winit gives it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Delta {
    Lines(f64, f64),
    Pixels(f64, f64),
}

#[derive(Debug)]
pub struct InputEngine {
    keyboard: Keyboard,
    pointer: Pointer,
    scroller: Scroller,
    typing: Typing,
    /// Window size in pixels, the frame size, the monitor scale.
    window: (f64, f64),
    /// Where the video area starts in the window (the panels around it take the rest).
    origin: (f64, f64),
    frame: Option<(f64, f64)>,
    scale: f64,
    /// The window has the keyboard and pointer; without it nothing is sent.
    focused: bool,
}

/// Whether an event is a pointer or scroll (the Mac's own pointer sprite gives way to it).
pub fn moves_pointer(e: &InputEvent) -> bool {
    matches!(e, InputEvent::Pointer { .. } | InputEvent::Scroll { .. } | InputEvent::ScrollGesture { .. })
}

impl InputEngine {
    pub fn new(cfg: InputConfig) -> InputEngine {
        InputEngine {
            keyboard: Keyboard::new(cfg.modifiers),
            pointer: Pointer::default(),
            scroller: Scroller::new(cfg.scroll),
            typing: Typing::default(),
            window: (0.0, 0.0),
            origin: (0.0, 0.0),
            frame: None,
            scale: 1.0,
            focused: true,
        }
    }

    pub fn scroll_config_mut(&mut self) -> &mut ScrollConfig {
        &mut self.scroller.cfg
    }

    pub fn set_modifier_map(&mut self, m: ModifierMap) {
        self.keyboard.map = m;
    }

    fn rect(&self) -> Rect {
        self.frame.map_or(Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }, |(fw, fh)| {
            let r = video_rect(self.window.0, self.window.1, fw, fh);
            Rect { x: r.x + self.origin.0, ..r }.shifted_y(self.origin.1)
        })
    }

    fn view(&self) -> View {
        View { rect: self.rect(), scale: self.scale.max(0.1) }
    }

    fn live(&self) -> bool {
        self.focused && self.typing.streaming
    }

    /// The window's size in pixels and the monitor's scale factor changed.
    pub fn set_window(&mut self, width_px: f64, height_px: f64, scale_factor: f64) {
        self.window = (width_px, height_px);
        self.scale = if scale_factor.is_finite() && scale_factor > 0.0 { scale_factor } else { 1.0 };
    }

    /// The video area is not the whole window: its top-left corner in window pixels (the size is `set_window`'s).
    pub fn set_origin(&mut self, x: f64, y: f64) {
        self.origin = (x, y);
    }

    /// The decoded picture's size (`None` before the first one).
    pub fn set_frame(&mut self, frame: Option<(u32, u32)>) {
        self.frame = frame.map(|(w, h)| (w as f64, h as f64));
    }

    /// What the host's window list says: something streams, and the host repeats held keys. When the
    /// source goes away everything held is let go (the host takes the ups).
    pub fn set_host(&mut self, streaming: bool, host_repeats: bool) -> Vec<InputEvent> {
        let was = self.typing.streaming;
        self.typing = Typing { streaming, host_repeats };
        if was && !streaming {
            return self.release_all();
        }
        vec![]
    }

    /// Everything held goes up: the window lost focus or closes, or the session ended.
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        let rect = self.rect();
        let mut out = self.keyboard.release_all();
        out.extend(self.pointer.release_all(&rect));
        out.extend(self.scroller.stop_all());
        out
    }

    /// Keyboard focus gained or lost. Losing it releases everything and silences the engine.
    pub fn set_focused(&mut self, focused: bool) -> Vec<InputEvent> {
        if self.focused == focused {
            return vec![];
        }
        self.focused = focused;
        if focused { vec![] } else { self.release_all() }
    }

    fn flush_pointer(&mut self, now: f64) -> Vec<InputEvent> {
        self.pointer.poll(now.max(self.pointer.next_deadline().unwrap_or(0.0)))
    }

    pub fn cursor_moved(&mut self, now: f64, x: f64, y: f64) -> Vec<InputEvent> {
        let rect = self.rect();
        let live = self.live();
        self.pointer.moved(now, &rect, x, y, live || (self.focused && self.pointer.buttons_down()))
    }

    pub fn cursor_left(&mut self) {
        self.pointer.left_window();
    }

    pub fn button(&mut self, now: f64, b: Button, pressed: bool) -> Vec<InputEvent> {
        let rect = self.rect();
        let mut out = self.scroller.stop_momentum();
        out.extend(self.pointer.button(now, &rect, b, pressed, self.live()));
        out
    }

    fn scroll_place(&self) -> (f64, f64) {
        self.pointer.last_fraction(&self.rect()).unwrap_or((0.5, 0.5))
    }

    pub fn wheel(&mut self, now: f64, delta: Delta, phase: Phase) -> Vec<InputEvent> {
        if !self.live() {
            // An open gesture still ends.
            return if matches!(phase, Phase::Ended | Phase::Cancelled) { self.pixels_end(now, phase) } else { vec![] };
        }
        let at = self.scroll_place();
        let mut out = self.flush_pointer(now);
        match delta {
            Delta::Lines(x, y) => out.extend(self.scroller.wheel(self.view(), at, x, y)),
            Delta::Pixels(x, y) => out.extend(self.scroller.pixels(now, self.view(), at, phase, x, y)),
        }
        out
    }

    fn pixels_end(&mut self, now: f64, phase: Phase) -> Vec<InputEvent> {
        let at = self.scroll_place();
        self.scroller.pixels(now, self.view(), at, phase, 0.0, 0.0)
    }

    /// A key. `text` is winit's `KeyEvent::text`, `repeat` its OS autorepeat. A synthetic press (winit
    /// replays held keys when the window gains focus) is the caller's to leave out.
    pub fn key(&mut self, now: f64, code: KeyCode, pressed: bool, text: Option<&str>, repeat: bool) -> Vec<InputEvent> {
        let mut out = self.flush_pointer(now);
        if !pressed {
            out.extend(self.keyboard.release(code));
        } else if self.focused {
            out.extend(self.scroller.stop_momentum());
            out.extend(self.keyboard.press(code, text, repeat, self.typing));
        }
        out
    }

    pub fn ime_commit(&mut self, text: &str) -> Vec<InputEvent> {
        if self.focused { self.keyboard.ime_commit(text, self.typing) } else { vec![] }
    }

    /// Time passed: held-back moves, the end of a gesture nobody ended, a coast's steps.
    pub fn poll(&mut self, now: f64) -> Vec<InputEvent> {
        let mut out = self.pointer.poll(now);
        let view = self.view();
        out.extend(self.scroller.poll(now, view));
        out
    }

    /// When [`poll`](Self::poll) next has something to do.
    pub fn next_deadline(&self) -> Option<f64> {
        [self.pointer.next_deadline(), self.scroller.next_deadline()].into_iter().flatten().reduce(f64::min)
    }

    pub fn keyboard(&self) -> &Keyboard {
        &self.keyboard
    }
}

//! The window: winit's event loop, the wgpu surface, and the glue from [`UiEvent`]s to the renderer.

use crate::client::Client;
use crate::gpu::Gpu;
use crate::hud;
use crate::renderer::Renderer;
use crate::seam::{AppCommand, UiEvent};
use crate::ui::Gui;
use sill_ui_state::Model;
use sill_input::{moves_pointer, Button, Delta, InputConfig, InputEngine, Phase};
use sill_media::Panel;
use sill_net::Command;
use sill_proto::{InputEvent, StreamSource};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key, NamedKey, PhysicalKey};
use winit::window::{Window, WindowId};

pub struct Options {
    pub hud: bool,
    /// Quit after this many pictures were shown (for scripted runs).
    pub exit_after: Option<u64>,
    /// With `exit_after`: write the last picture, as drawn, to this PNG.
    pub dump: Option<std::path::PathBuf>,
    pub title: String,
}

struct Surface {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    renderer: Renderer,
    gui: Gui,
    egui_state: egui_winit::State,
}

pub struct App {
    client: Client,
    opts: Options,
    s: Option<Surface>,
    pointer: Option<(f64, f64)>,
    input: InputEngine,
    hud_on: bool,
    hud_at: Option<Instant>,
    presented: u64,
    model: Model,
    /// The video area in window pixels (x, y, w, h), after the panels.
    area: Option<(f64, f64, f64, f64)>,
    /// A press began over the picture: its moves and release are the Mac's.
    video_drag: bool,
    repaint_at: Option<Instant>,
    shown_title: String,
    exit_error: Option<String>,
}

impl App {
    pub fn new(client: Client, opts: Options) -> App {
        let hud_on = opts.hud;
        App { client, opts, s: None, pointer: None, input: InputEngine::new(InputConfig::default()), hud_on, hud_at: None, presented: 0, model: Model::default(), area: None, video_drag: false, repaint_at: None, shown_title: String::new(), exit_error: None }
    }

    pub fn error(&self) -> Option<&str> {
        self.exit_error.as_deref()
    }

    fn send_panel(&mut self) {
        let Some(s) = &self.s else { return };
        let size = s.window.inner_size();
        // The viewport is the video area the panels leave, not the whole window.
        let (ox, oy, w, h) = self.area.unwrap_or((0.0, 0.0, size.width as f64, size.height as f64));
        self.input.set_window(w, h, s.window.scale_factor());
        self.input.set_origin(ox, oy);
        let hz = s.window.current_monitor().and_then(|m| m.refresh_rate_millihertz()).map(|m| m as f64 / 1000.0);
        let _ = self.client.cmds.send(AppCommand::Panel {
            panel: Panel { width_px: w.round().max(1.0) as u32, height_px: h.round().max(1.0) as u32, scale_factor: s.window.scale_factor() },
            refresh_hz: hz,
        });
    }

    fn now(&self) -> f64 {
        self.client.clock.elapsed().as_secs_f64()
    }

    /// Send what the input engine made, in order. Input from this device takes the Mac pointer's sprite away.
    fn send_input(&mut self, events: Vec<InputEvent>) {
        if events.is_empty() {
            return;
        }
        if events.iter().any(moves_pointer) && self.pointer.take().is_some() {
            if let Some(s) = &self.s {
                s.window.request_redraw();
            }
        }
        for e in events {
            let _ = self.client.cmds.send(AppCommand::Session(Command::Input(e)));
        }
    }

    fn set_title(&mut self) {
        let t = if self.model.title.is_empty() { self.opts.title.clone() } else { self.model.title.clone() };
        if t != self.shown_title {
            if let Some(s) = &self.s {
                s.window.set_title(&t);
            }
            self.shown_title = t;
        }
    }

    /// The panels changed what is left for the picture: tell the renderer, the input engine and the Mac.
    fn set_area(&mut self, px: Option<(f64, f64, f64, f64)>) {
        if self.area == px {
            return;
        }
        self.area = px;
        if let Some(s) = self.s.as_mut() {
            s.renderer.set_area(px);
        }
        self.send_panel();
    }

    fn refresh_hud(&mut self) {
        let Some(s) = self.s.as_mut() else { return };
        if !self.hud_on {
            s.renderer.set_hud(None);
            return;
        }
        let now = self.client.clock.elapsed().as_secs_f64();
        let snap = self.client.stats.lock().unwrap().snapshot(now);
        let size = s.renderer.video_size().map_or("-".to_string(), |(w, h)| format!("{w}x{h}"));
        let pair = |v: Option<(i64, i64)>| v.map_or("-".into(), |(m, x)| format!("{m}/{x}"));
        let lines = vec![
            format!("decoder: {}", self.client.info.get()),
            format!("gpu: {}  present: {:?}", s.gpu.info.name, s.config.present_mode),
            format!("video {size}  fps {} (decoded {})", snap.presented_fps, snap.decoded_fps),
            format!("frame age {} ms  rtt {} ms", pair(snap.frame_age_ms), pair(snap.rtt_ms)),
            format!("decode {} ms  dropped {}/s", snap.decode_ms_avg.map_or("-".into(), |d| format!("{d:.1}")), snap.dropped_per_s),
        ];
        s.renderer.set_hud(Some(hud::render_text(&lines, 2)));
        self.hud_at = Some(Instant::now());
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(s) = self.s.as_mut() else { return };
        if let Some(f) = self.client.slot.take() {
            let now = self.client.clock.elapsed().as_secs_f64();
            let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
            self.client.stats.lock().unwrap().record_presented(now, (wall - f.timestamp) * 1000.0);
            s.renderer.upload(&f);
            self.input.set_frame(s.renderer.video_size());
            self.presented += 1;
        }
        if self.hud_on && self.hud_at.is_none_or(|t| t.elapsed() > Duration::from_millis(250)) {
            self.refresh_hud();
        }
        // The panels: egui lays them out against the model; what they leave is the picture's area.
        let s = self.s.as_mut().unwrap();
        let raw = s.egui_state.take_egui_input(&s.window);
        let out = s.gui.run(raw, &self.model);
        s.egui_state.handle_platform_output(&s.window, std::mem::take(&mut { out.platform.clone() }));
        let ppp = out.pixels_per_point as f64;
        let area = Some((out.area[0] as f64 * ppp, out.area[1] as f64 * ppp, out.area[2] as f64 * ppp, out.area[3] as f64 * ppp)).filter(|a| a.2 > 0.0 && a.3 > 0.0);
        let intents = out.intents.clone();
        self.repaint_at = (out.repaint_after < Duration::from_secs(3600)).then(|| Instant::now() + out.repaint_after);
        self.set_area(area);
        self.set_title();
        for i in intents {
            let _ = self.client.cmds.send(AppCommand::Intent(i));
        }
        let s = self.s.as_mut().unwrap();
        let frame = match s.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                s.surface.configure(&s.gpu.device, &s.config);
                s.window.request_redraw();
                return;
            }
            Err(wgpu::SurfaceError::Timeout) => return,
            Err(e) => {
                self.exit_error = Some(format!("surface: {e}"));
                event_loop.exit();
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let size = (s.config.width, s.config.height);
        {
            let (gui, device, queue) = (&mut s.gui, &s.gpu.device, &s.gpu.queue);
            s.renderer.render_with(&view, size, s.window.scale_factor(), self.pointer, &mut |v| gui.paint(device, queue, v, size, &out));
        }
        s.window.pre_present_notify();
        frame.present();
        if self.opts.exit_after.is_some_and(|n| self.presented >= n) {
            if let Some(p) = &self.opts.dump {
                let (gui, device, queue) = (&mut s.gui, &s.gpu.device, &s.gpu.queue);
                let img = s.renderer.render_to_image_with(size, s.window.scale_factor(), self.pointer, &mut |v| gui.paint(device, queue, v, size, &out));
                if let Err(e) = write_png(p, size, &img) {
                    self.exit_error = Some(e);
                }
            }
            let _ = self.client.cmds.send(AppCommand::Quit);
            event_loop.exit();
        }
    }
}

pub fn write_png(path: &std::path::Path, size: (u32, u32), rgba: &[u8]) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), size.0, size.1);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().and_then(|mut w| w.write_image_data(rgba)).map_err(|e| e.to_string())
}

impl ApplicationHandler<UiEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.s.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title(self.opts.title.clone()).with_inner_size(PhysicalSize::new(1280, 720));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.exit_error = Some(format!("window: {e}"));
                event_loop.exit();
                return;
            }
        };
        let instance = Gpu::instance();
        let surface = match instance.create_surface(window.clone()) {
            Ok(s) => s,
            Err(e) => {
                self.exit_error = Some(format!("surface: {e}"));
                event_loop.exit();
                return;
            }
        };
        let gpu = match Gpu::request_for_app(instance, Some(&surface)) {
            Ok(g) => g,
            Err(e) => {
                self.exit_error = Some(e);
                event_loop.exit();
                return;
            }
        };
        let caps = surface.get_capabilities(&gpu.adapter);
        // The picture is gamma-encoded already: write it as it is, to a non-sRGB format.
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).or(caps.formats.first().copied());
        let Some(format) = format else {
            self.exit_error = Some("the surface has no formats".into());
            event_loop.exit();
            return;
        };
        let present_mode = [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
            .into_iter()
            .find(|m| caps.present_modes.contains(m))
            .unwrap_or(wgpu::PresentMode::Fifo);
        window.set_ime_allowed(true);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes.first().copied().unwrap_or(wgpu::CompositeAlphaMode::Auto),
            view_formats: vec![],
        };
        surface.configure(&gpu.device, &config);
        eprintln!("gpu: {}; surface {:?}, present mode {:?}", gpu.describe(), format, present_mode);
        let mut renderer = Renderer::new(gpu.device.clone(), gpu.queue.clone(), format);
        renderer.set_importer(gpu.importer.clone());
        let mut gui = Gui::new();
        gui.init_renderer(&gpu.device, format);
        let egui_state = egui_winit::State::new(gui.ctx.clone(), egui::ViewportId::ROOT, &*window, Some(window.scale_factor() as f32), None, None);
        self.s = Some(Surface { window, surface, config, gpu, renderer, gui, egui_state });
        event_loop.set_control_flow(ControlFlow::Wait);
        self.send_panel();
    }

    fn user_event(&mut self, _: &ActiveEventLoop, ev: UiEvent) {
        match ev {
            UiEvent::FrameReady => {
                if let Some(s) = &self.s {
                    s.window.request_redraw();
                }
            }
            UiEvent::Cursor(c) => {
                if let Some(s) = self.s.as_mut() {
                    s.renderer.set_cursor(c);
                }
            }
            UiEvent::Pointer(p) => {
                self.pointer = p;
                if let Some(s) = &self.s {
                    s.window.request_redraw();
                }
            }
            UiEvent::Status(_) => {}
            UiEvent::Model(m) => {
                self.model = *m;
                self.set_title();
                if let Some(s) = &self.s {
                    s.window.request_redraw();
                }
            }
            UiEvent::WindowList(l) => {
                let streaming = input_live(&l);
                let ups = self.input.set_host(streaming, l.host_repeats_keys());
                self.send_input(ups);
            }
            UiEvent::Closed(_) => {
                self.input.release_all();
                self.input.set_host(false, false);
                self.video_drag = false;
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = self.now();
        let evs = self.input.poll(now);
        self.send_input(evs);
        let input = self.input.next_deadline().map(|d| self.client.clock + Duration::from_secs_f64(d.max(now)));
        match input.into_iter().chain(self.repaint_at).min() {
            Some(t) => event_loop.set_control_flow(ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
        if self.repaint_at.is_some_and(|t| t <= Instant::now()) {
            self.repaint_at = None;
            if let Some(s) = &self.s {
                s.window.request_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        // The panels see every event first. What they use is not the Mac's: input over a panel, or while a
        // text field has the keyboard, is not forwarded (a release of something the Mac holds always is).
        let mut ui_used = false;
        let (mut ui_pointer, mut ui_keys) = (false, false);
        if let Some(s) = self.s.as_mut() {
            let r = s.egui_state.on_window_event(&s.window, &event);
            if r.repaint {
                s.window.request_redraw();
            }
            ui_used = r.consumed;
            ui_pointer = s.gui.ctx.wants_pointer_input();
            ui_keys = s.gui.ctx.wants_keyboard_input();
        }
        match event {
            WindowEvent::CloseRequested => {
                let ups = self.input.release_all();
                self.send_input(ups);
                let _ = self.client.cmds.send(AppCommand::Quit);
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(s) = self.s.as_mut() {
                    s.config.width = size.width.max(1);
                    s.config.height = size.height.max(1);
                    s.surface.configure(&s.gpu.device, &s.config);
                    s.window.request_redraw();
                }
                self.send_panel();
            }
            WindowEvent::ScaleFactorChanged { .. } => self.send_panel(),
            WindowEvent::Focused(f) => {
                let ups = self.input.set_focused(f);
                self.send_input(ups);
                let _ = self.client.cmds.send(AppCommand::Active(f));
            }
            WindowEvent::Occluded(o) => {
                let _ = self.client.cmds.send(AppCommand::Active(!o));
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat && event.logical_key == Key::Named(NamedKey::F3) =>
            {
                    self.hud_on = !self.hud_on;
                    self.hud_at = None;
                    self.refresh_hud();
                    if let Some(s) = &self.s {
                        s.window.request_redraw();
                    }
            }
            WindowEvent::KeyboardInput { event, .. } if event.logical_key == Key::Named(NamedKey::F3) => {}
            WindowEvent::KeyboardInput { event, is_synthetic, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if let PhysicalKey::Code(code) = event.physical_key {
                    if !(pressed && (is_synthetic || ui_keys || ui_used)) {
                        let now = self.now();
                        let evs = self.input.key(now, code, pressed, event.text.as_deref(), event.repeat);
                        self.send_input(evs);
                    }
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) if !(ui_keys || ui_used) => {
                let evs = self.input.ime_commit(&text);
                self.send_input(evs);
            }
            WindowEvent::CursorMoved { position, .. } => {
                if !self.video_drag && (ui_pointer || ui_used) {
                    self.input.cursor_left();
                    return;
                }
                let now = self.now();
                let evs = self.input.cursor_moved(now, position.x, position.y);
                self.send_input(evs);
            }
            WindowEvent::CursorLeft { .. } => self.input.cursor_left(),
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                if pressed {
                    if ui_pointer || ui_used {
                        return;
                    }
                    self.video_drag = true;
                } else if !std::mem::take(&mut self.video_drag) {
                    return;
                }
                let b = match button {
                    MouseButton::Left => Button::Left,
                    MouseButton::Right => Button::Right,
                    _ => Button::Other,
                };
                let now = self.now();
                let evs = self.input.button(now, b, state == ElementState::Pressed);
                self.send_input(evs);
            }
            WindowEvent::MouseWheel { delta, phase, .. } if !(ui_pointer || ui_used) => {
                let phase = match phase {
                    TouchPhase::Started => Phase::Started,
                    TouchPhase::Moved => Phase::Moved,
                    TouchPhase::Ended => Phase::Ended,
                    TouchPhase::Cancelled => Phase::Cancelled,
                };
                let delta = match delta {
                    MouseScrollDelta::LineDelta(x, y) => Delta::Lines(x as f64, y as f64),
                    MouseScrollDelta::PixelDelta(p) => Delta::Pixels(p.x, p.y),
                };
                let now = self.now();
                let evs = self.input.wheel(now, delta, phase);
                self.send_input(evs);
            }
            WindowEvent::RedrawRequested => self.draw(event_loop),
            _ => {}
        }
    }
}

/// Whether the input engine may forward input for this window list. A locked screen still takes input from
/// a paired session (the owner types the password at the lock screen), so the host decides there; only
/// another user on the console (`offConsole`) takes none from anyone, so nothing is sent.
fn input_live(l: &sill_proto::WindowList) -> bool {
    if l.off_console.unwrap_or(false) {
        return false;
    }
    l.active != StreamSource::None || l.locked.unwrap_or(false)
}

#[cfg(test)]
mod input_live_tests {
    use super::*;
    fn list(active: StreamSource, locked: Option<bool>, off: Option<bool>) -> sill_proto::WindowList {
        sill_proto::WindowList { mac_name: "M".into(), windows: vec![], active, launch_id: None, host_version: None, protocol: None, gestures: None, clipboard: None, key_repeat: None, locked, off_console: off }
    }
    #[test]
    fn a_locked_screen_takes_input_but_another_user_does_not() {
        assert!(input_live(&list(StreamSource::Desktop, None, None)));
        assert!(!input_live(&list(StreamSource::None, None, None)));
        assert!(input_live(&list(StreamSource::Desktop, Some(true), None)));
        assert!(input_live(&list(StreamSource::None, Some(true), None)));
        assert!(!input_live(&list(StreamSource::Desktop, Some(true), Some(true))));
    }
}

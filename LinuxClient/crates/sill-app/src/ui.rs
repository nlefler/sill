//! The panels, drawn with egui over the video: the switcher (the Desktop and the host's windows), the Apps
//! drawer, the host's Settings, the Mac list with pairing, and the status line. The panels read a
//! [`Model`] and emit [`UiIntent`]s; they hold only view state (what is open, what is typed).
//!
//! The video is drawn first by the renderer into the area the panels leave ([`FrameOut::area`]); egui
//! paints on top. Nothing here knows the window system: the app feeds a `RawInput` and paints the result.

use egui::{Color32, RichText};
use sill_proto::{StreamSource, WindowAction, QUALITY_PRESETS};
use sill_ui_state::{ConnState, Model, SettingPick, UiIntent};
use std::collections::HashMap;
use std::sync::Arc;

pub const SIDE_WIDTH: f32 = 240.0;
const THUMB_WIDTH: f32 = 200.0;

/// What is open and what is typed.
pub struct Local {
    pub side_open: bool,
    pub apps_open: bool,
    pub settings_open: bool,
    /// The Mac's menus window, and the path of submenus opened in it (id, title).
    pub menus_window: bool,
    pub menu_path: Vec<(String, String)>,
    /// The Mac list; opens by itself while nothing is connected.
    pub macs_open: bool,
    pub macs_auto: bool,
    pub search: String,
    pub add_address: String,
    pub pair_link: String,
    pub pair_address: String,
    pub pair_code: String,
}

impl Default for Local {
    fn default() -> Self {
        Local {
            side_open: true,
            apps_open: false,
            settings_open: false,
            menus_window: false,
            menu_path: vec![],
            macs_open: true,
            macs_auto: true,
            search: String::new(),
            add_address: String::new(),
            pair_link: String::new(),
            pair_address: String::new(),
            pair_code: String::new(),
        }
    }
}

pub struct FrameOut {
    pub primitives: Vec<egui::ClippedPrimitive>,
    pub textures: egui::TexturesDelta,
    pub platform: egui::PlatformOutput,
    pub intents: Vec<UiIntent>,
    /// The video area in points (x, y, w, h): what the panels leave of the window.
    pub area: [f32; 4],
    pub pixels_per_point: f32,
    pub repaint_after: std::time::Duration,
}

pub struct Gui {
    pub ctx: egui::Context,
    pub local: Local,
    renderer: Option<egui_wgpu::Renderer>,
    textures: HashMap<u64, egui::TextureHandle>,
    used: Vec<u64>,
}

impl Default for Gui {
    fn default() -> Self {
        Gui::new()
    }
}

impl Gui {
    pub fn new() -> Gui {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        Gui { ctx, local: Local::default(), renderer: None, textures: HashMap::new(), used: vec![] }
    }

    fn texture(&mut self, img: &Arc<sill_ui_state::switcher::Image>) -> egui::TextureId {
        self.used.push(img.id);
        if let Some(t) = self.textures.get(&img.id) {
            return t.id();
        }
        let ci = egui::ColorImage::from_rgba_unmultiplied([img.w as usize, img.h as usize], &img.rgba);
        let h = self.ctx.load_texture(format!("img{}", img.id), ci, egui::TextureOptions::LINEAR);
        let id = h.id();
        self.textures.insert(img.id, h);
        id
    }

    /// Lay the panels out for one frame.
    pub fn run(&mut self, raw: egui::RawInput, model: &Model) -> FrameOut {
        let mut intents = vec![];
        let mut area = egui::Rect::NOTHING;
        self.used.clear();
        let ctx = self.ctx.clone();
        // Take the view state out so the closure can borrow `self` for textures.
        let mut local = std::mem::take(&mut self.local);
        let out = ctx.run(raw, |ctx| {
            area = self.panels(ctx, model, &mut local, &mut intents);
        });
        self.local = local;
        let used = std::mem::take(&mut self.used);
        self.textures.retain(|id, _| used.contains(id));
        self.used = used;
        let primitives = ctx.tessellate(out.shapes, out.pixels_per_point);
        let vp = out.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.repaint_delay).unwrap_or(std::time::Duration::MAX);
        FrameOut {
            primitives,
            textures: out.textures_delta,
            platform: out.platform_output,
            intents,
            area: [area.min.x, area.min.y, area.width(), area.height()],
            pixels_per_point: out.pixels_per_point,
            repaint_after: vp,
        }
    }

    fn panels(&mut self, ctx: &egui::Context, m: &Model, l: &mut Local, intents: &mut Vec<UiIntent>) -> egui::Rect {
        let connected = m.conn == ConnState::Connected;
        if l.macs_auto {
            l.macs_open = !connected;
        }
        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if connected && ui.selectable_label(l.side_open, "Windows").clicked() {
                    l.side_open = !l.side_open;
                }
                if connected && ui.selectable_label(l.apps_open, "Apps").clicked() {
                    l.apps_open = !l.apps_open;
                }
                if connected && ui.selectable_label(l.settings_open, "Settings").clicked() {
                    l.settings_open = !l.settings_open;
                }
                if connected && m.menus.is_some() && ui.selectable_label(l.menus_window, "Menus").clicked() {
                    l.menus_window = !l.menus_window;
                    l.menu_path.clear();
                }
                if ui.selectable_label(l.macs_open && !l.macs_auto, "Macs").clicked() {
                    l.macs_auto = false;
                    l.macs_open = !l.macs_open;
                }
                ui.separator();
                let line = m.status_line();
                if m.notice.is_some() {
                    ui.colored_label(Color32::from_rgb(255, 200, 90), line);
                } else {
                    ui.label(line);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if connected {
                        if ui.button("Disconnect").clicked() {
                            intents.push(UiIntent::Disconnect);
                        }
                    } else if matches!(m.conn, ConnState::Reconnecting(_)) {
                        if ui.button("Cancel").clicked() {
                            intents.push(UiIntent::Disconnect);
                        }
                        if ui.button("Try now").clicked() {
                            intents.push(UiIntent::Retry);
                        }
                    }
                });
            });
        });
        if connected && l.side_open {
            egui::SidePanel::left("switcher").exact_width(SIDE_WIDTH).resizable(false).show(ctx, |ui| self.switcher(ui, m, intents));
        }
        // No CentralPanel: it would count as an egui area and take the pointer from the picture. What the
        // panels leave is the picture's.
        let r = ctx.available_rect();
        {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("cover")));
            let ui = &painter;
            if let Some(c) = &m.cover {
                ui.rect_filled(r, 0.0, Color32::from_black_alpha(235));
                ui.text(r.center() - egui::vec2(0.0, 14.0), egui::Align2::CENTER_CENTER, &c.title, egui::FontId::proportional(24.0), Color32::WHITE);
                if let Some(d) = &c.detail {
                    ui.text(r.center() + egui::vec2(0.0, 18.0), egui::Align2::CENTER_TOP, d, egui::FontId::proportional(14.0), Color32::LIGHT_GRAY);
                }
            } else if !connected {
                ui.rect_filled(r, 0.0, Color32::BLACK);
            }
        }
        if connected && l.apps_open {
            self.apps(ctx, m, l, intents);
        }
        if connected && l.settings_open {
            settings(ctx, m, l, intents);
        }
        if connected && l.menus_window {
            match &m.menus {
                Some(_) => mac_menus(ctx, m, l, intents),
                None => l.menus_window = false,
            }
        }
        if l.macs_open {
            self.macs(ctx, m, l, intents);
        }
        r
    }

    fn switcher(&mut self, ui: &mut egui::Ui, m: &Model, intents: &mut Vec<UiIntent>) {
        ui.add_space(4.0);
        if let Some(line) = &m.locked_line {
            ui.label(RichText::new(line).color(Color32::LIGHT_GRAY));
            return;
        }
        let desktop = m.active == StreamSource::Desktop;
        if ui.add_sized([ui.available_width(), 28.0], egui::Button::new("Desktop").selected(desktop)).clicked() {
            intents.push(UiIntent::Pick(StreamSource::Desktop));
        }
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if m.windows.is_empty() {
                ui.label(RichText::new("No windows open on the Mac.").color(Color32::GRAY));
            }
            for w in &m.windows {
                let active = m.active == StreamSource::Window(w.id);
                let frame = egui::Frame::group(ui.style()).fill(if active { Color32::from_rgb(30, 60, 100) } else { Color32::TRANSPARENT });
                let r = frame
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            if let Some(icon) = &w.icon {
                                let id = self.texture(icon);
                                ui.image(egui::load::SizedTexture::new(id, egui::vec2(20.0, 20.0)));
                            }
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&w.title).strong());
                                ui.label(RichText::new(&w.app).small().color(Color32::GRAY));
                            });
                        });
                        if let Some(t) = &w.thumb {
                            let id = self.texture(t);
                            let h = THUMB_WIDTH * t.h as f32 / t.w.max(1) as f32;
                            ui.image(egui::load::SizedTexture::new(id, egui::vec2(THUMB_WIDTH, h.min(150.0))));
                        }
                    })
                    .response
                    .interact(egui::Sense::click());
                if r.clicked() {
                    intents.push(UiIntent::Pick(StreamSource::Window(w.id)));
                }
                r.context_menu(|ui| {
                    for (label, action) in [("Close", WindowAction::Close), ("Minimize", WindowAction::Minimize), ("Full Screen", WindowAction::FullScreen)] {
                        if ui.button(label).clicked() {
                            intents.push(UiIntent::WindowButton { id: w.id, action });
                            ui.close();
                        }
                    }
                });
            }
        });
    }

    fn apps(&mut self, ctx: &egui::Context, m: &Model, l: &mut Local, intents: &mut Vec<UiIntent>) {
        let mut open = l.apps_open;
        egui::Window::new("Apps").open(&mut open).default_width(280.0).default_pos([SIDE_WIDTH + 16.0, 48.0]).show(ctx, |ui| {
            if let Some(line) = &m.locked_line {
                ui.label(line);
                return;
            }
            ui.add(egui::TextEdit::singleline(&mut l.search).hint_text("Search apps"));
            let q = l.search.trim().to_lowercase();
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for a in m.apps.iter().filter(|a| q.is_empty() || a.name.to_lowercase().contains(&q)) {
                    ui.horizontal(|ui| {
                        if let Some(icon) = &a.icon {
                            let id = self.texture(icon);
                            ui.image(egui::load::SizedTexture::new(id, egui::vec2(20.0, 20.0)));
                        }
                        if ui.button(&a.name).clicked() {
                            intents.push(UiIntent::Launch(a.bundle_id.clone()));
                            l.apps_open = false;
                        }
                    });
                }
            });
        });
        if !open {
            l.apps_open = false;
        }
    }

    fn macs(&mut self, ctx: &egui::Context, m: &Model, l: &mut Local, intents: &mut Vec<UiIntent>) {
        let mut open = l.macs_open;
        let closable = !l.macs_auto;
        egui::Window::new("Macs").open(&mut open).collapsible(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).default_width(420.0).show(ctx, |ui| {
            if m.macs.is_empty() {
                ui.label(RichText::new("Looking for Macs running Sill...").color(Color32::GRAY));
            }
            for r in &m.macs {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&r.name).strong());
                        ui.label(RichText::new(&r.word).small().color(Color32::GRAY));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if r.saved && !r.listed && ui.small_button("Forget").clicked() {
                            intents.push(UiIntent::ForgetMac(r.key.clone()));
                        }
                        if r.needs_pairing {
                            if ui.button("Pair...").clicked() {
                                l.pair_address = r.address.clone().unwrap_or_default();
                            }
                        } else if r.dialable && (r.saved || r.open_door) && ui.button("Connect").clicked() {
                            intents.push(UiIntent::ConnectMac(r.key.clone()));
                        }
                    });
                });
                ui.separator();
            }
            ui.label(RichText::new("Connect by address").strong());
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut l.add_address).hint_text("100.101.102.103 or mac.example.net").desired_width(260.0));
                if ui.button("Connect").clicked() && !l.add_address.trim().is_empty() {
                    intents.push(UiIntent::ConnectAddress(l.add_address.trim().to_string()));
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new("Pair with a Mac").strong());
            ui.label(RichText::new("On the Mac: Sill > Pair iPhone or iPad. Paste its link, or type its address and the 12-digit code.").small().color(Color32::GRAY));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut l.pair_link).hint_text("sill://pair?...").desired_width(260.0));
                if ui.add_enabled(!m.pairing.busy && !l.pair_link.trim().is_empty(), egui::Button::new("Pair")).clicked() {
                    intents.push(UiIntent::PairLink(l.pair_link.trim().to_string()));
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut l.pair_address).hint_text("address").desired_width(150.0));
                ui.add(egui::TextEdit::singleline(&mut l.pair_code).hint_text("12-digit code").desired_width(110.0));
                if ui.add_enabled(!m.pairing.busy && !l.pair_code.trim().is_empty(), egui::Button::new("Pair")).clicked() {
                    intents.push(UiIntent::PairCode { address: l.pair_address.trim().to_string(), code: l.pair_code.trim().to_string() });
                }
            });
            if m.pairing.busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Pairing...");
                });
            }
            if let Some(e) = &m.pairing.error {
                ui.colored_label(Color32::from_rgb(255, 120, 110), e);
            }
            if let Some(d) = &m.pairing.done {
                ui.colored_label(Color32::from_rgb(120, 220, 140), d);
            }
        });
        if !open && closable {
            l.macs_open = false;
        }
    }

    // ------------------------------------------------------------------ painting

    pub fn init_renderer(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        self.renderer = Some(egui_wgpu::Renderer::new(device, format, None, 1, false));
    }

    /// Paint `out` over `view`'s picture (the picture's pass must have been submitted).
    pub fn paint(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView, size_px: (u32, u32), out: &FrameOut) {
        let Some(r) = self.renderer.as_mut() else { return };
        for (id, delta) in &out.textures.set {
            r.update_texture(device, queue, *id, delta);
        }
        let sd = egui_wgpu::ScreenDescriptor { size_in_pixels: [size_px.0, size_px.1], pixels_per_point: out.pixels_per_point };
        let mut enc = device.create_command_encoder(&Default::default());
        let extra = r.update_buffers(device, queue, &mut enc, &out.primitives, &sd);
        {
            let pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            let mut pass = pass.forget_lifetime();
            r.render(&mut pass, &out.primitives, &sd);
        }
        queue.submit(extra.into_iter().chain(Some(enc.finish())));
        for id in &out.textures.free {
            r.free_texture(id);
        }
    }
}

fn settings(ctx: &egui::Context, m: &Model, l: &mut Local, intents: &mut Vec<UiIntent>) {
    let mut open = l.settings_open;
    let mut open2 = l.settings_open;
    let mac = if m.mac_name.is_empty() { "the Mac".to_string() } else { m.mac_name.clone() };
    egui::Window::new(format!("Settings - {mac}")).open(&mut open).default_width(300.0).anchor(egui::Align2::RIGHT_TOP, [-12.0, 48.0]).show(ctx, |ui| {
        let Some(s) = &m.settings else {
            ui.label(format!("Update Sill on {mac} to change its settings from here."));
            return;
        };
        let pending = |f: sill_ui_state::Field| s.pending.contains(&f);
        if let Some(run) = &s.running {
            ui.label(RichText::new(run).color(Color32::LIGHT_GRAY));
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Quality").strong());
        ui.horizontal_wrapped(|ui| {
            for (name, bitrate) in QUALITY_PRESETS {
                if ui.selectable_label(s.shown.bitrate == bitrate, name).clicked() {
                    intents.push(UiIntent::Setting(SettingPick::Quality(bitrate)));
                }
            }
            if pending(sill_ui_state::Field::Bitrate) {
                ui.spinner();
            }
        });
        if !QUALITY_PRESETS.iter().any(|p| p.1 == s.shown.bitrate) {
            ui.label(RichText::new(format!("Custom: {} Mbps", s.shown.bitrate / 1_000_000)).small());
        }
        ui.label(RichText::new("Frame rate").strong());
        ui.horizontal(|ui| {
            for fps in [60, 120] {
                if ui.selectable_label(s.shown.max_fps == fps, format!("{fps} fps")).clicked() {
                    intents.push(UiIntent::Setting(SettingPick::FrameRate(fps)));
                }
            }
            if pending(sill_ui_state::Field::MaxFps) {
                ui.spinner();
            }
        });
        ui.label(RichText::new("Resolution").strong());
        ui.horizontal(|ui| {
            for (name, scale) in [("Retina", 2.0), ("Standard", 1.0)] {
                if ui.selectable_label(s.shown.capture_scale == scale, name).clicked() {
                    intents.push(UiIntent::Setting(SettingPick::Scale(scale)));
                }
            }
            if pending(sill_ui_state::Field::CaptureScale) {
                ui.spinner();
            }
        });
        ui.add_space(4.0);
        let mut speed = s.shown.prioritize_speed;
        if ui.checkbox(&mut speed, "Prioritize speed").changed() {
            intents.push(UiIntent::Setting(SettingPick::PrioritizeSpeed(speed)));
        }
        let mut vd = s.shown.virtual_display;
        let r = ui.add_enabled(s.virtual_display_available || vd, egui::Checkbox::new(&mut vd, "Virtual Display"));
        if r.changed() {
            intents.push(UiIntent::Setting(SettingPick::VirtualDisplay(vd)));
        }
        if let (false, Some(note)) = (s.virtual_display_available, &s.virtual_display_note) {
            ui.label(RichText::new(note).small().color(Color32::GRAY));
        }
        if s.software_encoder {
            ui.label(RichText::new(format!("{mac}\u{2019}s hardware encoder is busy or not responding, so streams run at up to 60 fps at Standard.")).small().color(Color32::from_rgb(255, 200, 90)));
        }
        if !s.persistent {
            ui.label(RichText::new("These last until Sill quits on the Mac.").small().color(Color32::GRAY));
        }
    });
    egui::Window::new("This computer").id(egui::Id::new("local-settings")).open(&mut open2).default_width(300.0).anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -12.0]).show(ctx, |ui| {
        if m.clipboard.offered {
            let mut on = m.clipboard.on;
            if ui.checkbox(&mut on, "Share clipboard with the Mac").on_hover_text("Text you copy here can be pasted on the Mac, and the other way round.").changed() {
                intents.push(UiIntent::SetClipboard(on));
            }
        }
        if m.gestures {
            ui.label(RichText::new("Trackpad gestures").strong());
            ui.horizontal_wrapped(|ui| {
                for (name, label) in GESTURES {
                    if ui.button(label).clicked() {
                        intents.push(UiIntent::Gesture { name: name.to_string(), fingers: 3 });
                    }
                }
            });
        }
    });
    if !open || !open2 {
        l.settings_open = false;
    }
}

/// The six gestures the Mac takes (kind 28) and what each does by default on the Mac.
const GESTURES: [(&str, &str); 6] = [
    ("swipeUp", "Mission Control"),
    ("swipeDown", "App Exposé"),
    ("swipeLeft", "Space right"),
    ("swipeRight", "Space left"),
    ("pinch", "Show Desktop"),
    ("spread", "Apps"),
];

/// The Mac's menu bar as a window: the top level in a row, the opened menu's items below (a submenu
/// opens in place, a crumb goes back), a leaf is pressed on the Mac and the window folds.
fn mac_menus(ctx: &egui::Context, m: &Model, l: &mut Local, intents: &mut Vec<UiIntent>) {
    let Some(menus) = &m.menus else { return };
    let mut open = true;
    let title = if menus.app.is_empty() { "Menus".to_string() } else { format!("{} menus", menus.app) };
    egui::Window::new(title).id(egui::Id::new("mac-menus")).open(&mut open).collapsible(false).default_pos([SIDE_WIDTH + 16.0, 48.0]).default_width(280.0).show(ctx, |ui| {
        ui.horizontal_wrapped(|ui| {
            for it in &menus.top {
                let (Some(id), Some(t)) = (&it.id, &it.title) else { continue };
                let on = l.menu_path.first().is_some_and(|p| &p.0 == id);
                if ui.add_enabled(it.enabled != Some(false), egui::Button::selectable(on, t)).clicked() {
                    l.menu_path = vec![(id.clone(), t.clone())];
                    intents.push(UiIntent::OpenMenu { id: id.clone(), title: t.clone() });
                }
            }
        });
        if let Some(n) = &menus.note {
            ui.colored_label(Color32::from_rgb(255, 200, 90), n);
        }
        let Some((cur, _)) = l.menu_path.last().cloned() else { return };
        ui.separator();
        if l.menu_path.len() > 1 {
            let trail: Vec<&str> = l.menu_path.iter().map(|p| p.1.as_str()).collect();
            if ui.small_button(format!("\u{2039} {}", trail.join(" \u{203a} "))).clicked() {
                l.menu_path.pop();
            }
        }
        let Some(items) = menus.submenus.get(&cur) else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Asking the Mac\u{2026}");
            });
            return;
        };
        let mut pressed = false;
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for it in items {
                if it.separator == Some(true) {
                    ui.separator();
                    continue;
                }
                let (Some(id), Some(t)) = (&it.id, &it.title) else { continue };
                let mark = it.mark.as_deref().unwrap_or("");
                let label = if it.submenu == Some(true) { format!("{mark} {t}  \u{203a}") } else { format!("{mark} {t}") };
                ui.horizontal(|ui| {
                    let r = ui.add_enabled(it.enabled != Some(false), egui::Button::new(label.trim()).frame(false));
                    if let Some(k) = &it.key {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(RichText::new(k).small().color(Color32::GRAY)));
                    }
                    if r.clicked() {
                        if it.submenu == Some(true) {
                            l.menu_path.push((id.clone(), t.clone()));
                            intents.push(UiIntent::OpenMenu { id: id.clone(), title: t.clone() });
                        } else {
                            intents.push(UiIntent::PressMenu { id: id.clone(), title: t.clone() });
                            pressed = true;
                        }
                    }
                });
            }
        });
        if pressed {
            l.menu_path.clear();
        }
    });
    if !open {
        l.menus_window = false;
        l.menu_path.clear();
    }
}

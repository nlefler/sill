//! The whole path without a window: session, decode and (when a GPU exists) the renderer drawing offscreen.
//! Used by the end-to-end test and `sill --headless`.

use crate::client::{self, Client};
use crate::gpu::Gpu;
use crate::hud;
use crate::renderer::Renderer;
use crate::seam::{AppCommand, UiEvent, UiSink};
use sill_media::{DecoderPref, Panel};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct Report {
    pub presented: u64,
    pub rendered: u64,
    pub decoder: String,
    pub gpu: Option<String>,
    pub last_size: Option<(u32, u32)>,
    pub closed: Option<String>,
    pub cursor_seen: bool,
    pub pointer_seen: bool,
    pub max_age_ms: i64,
    /// Seconds from the first picture taken to the last, and the rate that makes.
    pub seconds: f64,
    pub fps: f64,
    /// The last rendered image (RGBA, size) when a GPU was available.
    pub image: Option<((u32, u32), Vec<u8>)>,
}

pub struct Options {
    pub frames: u64,
    pub timeout: Duration,
    pub pref: DecoderPref,
    pub panel: (u32, u32),
    pub render: bool,
}

/// Run `client` (already started with `ui`'s receiver) until `frames` pictures were taken or time ran out.
pub fn drive(client: &Client, rx: std::sync::mpsc::Receiver<UiEvent>, opts: &Options) -> Report {
    let mut rep = Report::default();
    let mut gpu_renderer = None;
    if !opts.render {
        sill_media::zerocopy::set_state(sill_media::zerocopy::State::Off);
    }
    if opts.render {
        match Gpu::request_for_app(Gpu::instance(), None) {
            Ok(g) => {
                rep.gpu = Some(g.describe());
                let mut r = Renderer::new(g.device.clone(), g.queue.clone(), wgpu::TextureFormat::Rgba8Unorm);
                r.set_importer(g.importer.clone());
                gpu_renderer = Some(r);
            }
            Err(e) => {
                sill_media::zerocopy::set_state(sill_media::zerocopy::State::Off);
                eprintln!("headless: no GPU, decoding only ({e})")
            }
        }
    }
    let _ = client.cmds.send(AppCommand::Panel {
        panel: Panel { width_px: opts.panel.0, height_px: opts.panel.1, scale_factor: 1.0 },
        refresh_hz: Some(60.0),
    });
    let mut pointer = None;
    let deadline = Instant::now() + opts.timeout;
    let mut first_at: Option<Instant> = None;
    while rep.presented < opts.frames && Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(UiEvent::FrameReady) => {
                if let Some(f) = client.slot.take() {
                    let now = client.clock.elapsed().as_secs_f64();
                    let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
                    let age = (wall - f.timestamp) * 1000.0;
                    rep.max_age_ms = rep.max_age_ms.max(age as i64);
                    client.stats.lock().unwrap().record_presented(now, age);
                    rep.presented += 1;
                    let t0 = *first_at.get_or_insert_with(Instant::now);
                    rep.seconds = t0.elapsed().as_secs_f64();
                    if rep.presented > 1 {
                        rep.fps = (rep.presented - 1) as f64 / rep.seconds.max(1e-9);
                    }
                    rep.last_size = Some((f.width, f.height));
                    if let Some(r) = gpu_renderer.as_mut() {
                        r.upload(&f);
                        if rep.presented == 1 {
                            let snap = client.stats.lock().unwrap().snapshot(now);
                            r.set_hud(Some(hud::render_text(&[format!("decoder: {}", client.info.get()), format!("fps {}", snap.presented_fps)], 2)));
                        }
                        rep.image = Some((opts.panel, r.render_to_image(opts.panel, 1.0, pointer)));
                        rep.rendered += 1;
                    }
                }
            }
            Ok(UiEvent::Cursor(c)) => {
                rep.cursor_seen |= c.is_some();
                if let Some(r) = gpu_renderer.as_mut() {
                    r.set_cursor(c);
                }
            }
            Ok(UiEvent::Pointer(p)) => {
                rep.pointer_seen |= p.is_some();
                pointer = p;
            }
            Ok(UiEvent::Closed(why)) => {
                rep.closed = Some(why);
                break;
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    rep.decoder = client.info.get();
    let _ = client.cmds.send(AppCommand::Quit);
    rep
}

/// Connect to `target`, start the pipeline and drive it headlessly.
pub fn run(rt: &tokio::runtime::Runtime, target: client::Target, opts: Options) -> Result<(Report, Option<sill_mockhost::MockHost>), String> {
    let conn = rt.block_on(client::connect(target))?;
    let (tx, rx) = channel();
    let tx = std::sync::Mutex::new(tx);
    let ui: UiSink = Arc::new(move |e| {
        let _ = tx.lock().unwrap().send(e);
    });
    let c = client::start(rt.handle(), conn.session, opts.pref, ui, false);
    let rep = drive(&c, rx, &opts);
    Ok((rep, conn.mock))
}

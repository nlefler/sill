//! Connecting (a saved Mac, an address, or the in-process mock host) and starting the session's tasks.

use crate::control::{self, Control};
use crate::pipeline::{self, DecoderInfo, FrameSlot};
use crate::seam::{AppCommand, UiSink};
use sill_discovery::{connect_by_address, connect_saved, ConnectOutcome, DialConfig};
use sill_media::{clip, DecoderPref, StatsCollector};
use sill_mockhost::{MockConfig, MockHost, MockVideo};
use sill_net::identity::{self, Identity};
use sill_net::saved::SavedMacs;
use sill_net::{ConnectOptions, PinMode, Session, SessionConfig};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

#[derive(Clone, Debug)]
pub enum Target {
    /// The in-process mock host streaming the embedded clip.
    Mock { corrupt_frame: Option<usize> },
    Address { host: String, port: u16, tofu: bool },
}

pub struct Connected {
    pub session: Session,
    pub label: String,
    /// The saved Mac the session is pinned to.
    pub mac_id: Option<String>,
    /// Keeps the mock host alive.
    pub mock: Option<MockHost>,
}

fn device_name() -> String {
    std::fs::read_to_string("/etc/hostname").ok().map(|h| h.trim().to_string()).filter(|h| !h.is_empty()).unwrap_or_else(|| "Linux".into())
}

pub fn connect_options(identity: Identity, device: &str, pin: PinMode, config: SessionConfig) -> ConnectOptions {
    ConnectOptions { identity, pin, device: device.into(), app_version: env!("CARGO_PKG_VERSION").into(), build: "1".into(), config }
}

fn options(identity: Identity, pin: PinMode) -> ConnectOptions {
    connect_options(identity, &device_name(), pin, SessionConfig::default())
}

pub fn device_name_pub() -> String {
    device_name()
}

/// A cursor shape for the mock: a 24x24 px white ring with a black outline, hotspot in the middle.
pub fn mock_cursor_payload() -> Vec<u8> {
    let n = 24u32;
    let mut px = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            let d = ((x as f32 - 11.5).powi(2) + (y as f32 - 11.5).powi(2)).sqrt();
            let c = if (8.0..10.5).contains(&d) { Some(255) } else if (6.0..8.0).contains(&d) || (10.5..12.0).contains(&d) { Some(0) } else { None };
            if let Some(c) = c {
                let i = ((y * n + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&[c, c, c, 255]);
            }
        }
    }
    let mut png_bytes = vec![];
    {
        let mut e = png::Encoder::new(&mut png_bytes, n, n);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&px).unwrap();
    }
    sill_proto::CursorShape { hotspot: (12.0, 12.0), size: (24.0, 24.0), png: png_bytes }.encode()
}

pub async fn start_mock(corrupt_frame: Option<usize>) -> Result<(MockHost, Identity), String> {
    // TEST ONLY: `SILL_MOCK_CLIP=file.hevc` (Annex B) streams that clip, `SILL_MOCK_FPS` sets its rate.
    let c = match std::env::var("SILL_MOCK_CLIP") {
        Ok(p) => clip::parse_annex_b(&std::fs::read(&p).map_err(|e| format!("SILL_MOCK_CLIP {p}: {e}"))?)?,
        Err(_) => clip::embedded_720p(),
    };
    let fps = std::env::var("SILL_MOCK_FPS").ok().and_then(|v| v.parse().ok()).unwrap_or(60.0);
    let video = MockVideo {
        params: c.params.clone(),
        frames: c.frames.iter().map(|f| (f.keyframe, f.payload.clone())).collect(),
        fps,
        corrupt_frame,
        cursor_payload: Some(mock_cursor_payload()),
    };
    let host = MockHost::start(MockConfig { video: Some(video), ..Default::default() }).await.map_err(|e| e.to_string())?;
    let id = Identity::generate_in_memory()?;
    Ok((host, id))
}

pub async fn connect(target: Target) -> Result<Connected, String> {
    match target {
        Target::Mock { corrupt_frame } => {
            let (host, id) = start_mock(corrupt_frame).await?;
            let opts = options(id, PinMode::Pinned(host.fingerprint));
            let s = sill_net::connect(("127.0.0.1", host.port()), opts).await.map_err(|e| e.to_string())?;
            Ok(Connected { session: s, label: "Mock Mac".into(), mac_id: None, mock: Some(host) })
        }
        Target::Address { host, port, tofu } => {
            let dir = identity::default_dir().ok_or("no data directory")?;
            let id = Identity::load_or_create(&dir)?;
            let saved = SavedMacs::load(&SavedMacs::default_path().ok_or("no data directory")?)?;
            let found = saved.macs.iter().find(|m| m.addresses.iter().any(|a| a.host == host && a.port == port));
            let outcome = match (found, tofu) {
                (Some(mac), _) => connect_saved(mac, &[], options(id, PinMode::TrustOnFirstUse), &DialConfig::default()).await,
                (None, true) => connect_by_address(&host, port, PinMode::TrustOnFirstUse, options(id, PinMode::TrustOnFirstUse), &DialConfig::default()).await,
                (None, false) => return Err(format!("no saved Mac at {host}:{port}; pair it first, or pass --tofu for a Mac that lets any device in")),
            };
            match outcome {
                ConnectOutcome::Connected { session, address } => {
                    Ok(Connected { session, label: found.map_or_else(|| format!("{}:{}", address.0, address.1), |m| m.name.clone()), mac_id: found.map(|m| m.mac_id.clone()), mock: None })
                }
                other => Err(other.words(&found.map_or(host.clone(), |m| m.name.clone())).unwrap_or_else(|| format!("{other:?}"))),
            }
        }
    }
}

/// Everything running for one session.
pub struct Client {
    pub cmds: UnboundedSender<AppCommand>,
    pub slot: FrameSlot,
    pub info: DecoderInfo,
    pub stats: Arc<Mutex<StatsCollector>>,
    pub clock: Instant,
    _decode: std::thread::JoinHandle<()>,
}

/// Spawn the decode thread and the control task for `session`.
pub fn start(rt: &tokio::runtime::Handle, session: Session, pref: DecoderPref, ui: UiSink, remote: bool) -> Client {
    start_with(rt, Some(session), None, pref, ui, remote)
}

/// As [`start`], with no session yet (the Mac list shows) and/or a [`control::Link`] to dial, pair, browse and reconnect with.
pub fn start_with(rt: &tokio::runtime::Handle, session: Option<Session>, link: Option<control::Link>, pref: DecoderPref, ui: UiSink, remote: bool) -> Client {
    let clock = Instant::now();
    let stats = Arc::new(Mutex::new(StatsCollector::new()));
    let (note_tx, note_rx) = unbounded_channel();
    let dt = pipeline::spawn(pref, stats.clone(), clock, note_tx, ui.clone());
    let (cmd_tx, cmd_rx) = unbounded_channel();
    rt.spawn(control::run(Control { session, cmds: cmd_rx, notes: note_rx, decode: dt.tx.clone(), stats: stats.clone(), ui, clock, remote, link, clipboard: crate::clipboard::system() }));
    Client { cmds: cmd_tx, slot: dt.slot, info: dt.info, stats, clock, _decode: dt.handle }
}

/// The real link: this device's identity, the saved Macs, and Bonjour.
pub fn real_link(current: Option<String>) -> Result<control::Link, String> {
    let dir = identity::default_dir().ok_or("no data directory")?;
    let identity = Identity::load_or_create(&dir)?;
    let saved = SavedMacs::load(&SavedMacs::default_path().ok_or("no data directory")?)?;
    Ok(control::Link {
        identity,
        device: device_name(),
        saved,
        browser: Some(Arc::new(sill_discovery::MdnsBrowser::new(sill_discovery::browser::DEFAULT_SERVICE_TYPE))),
        dial: DialConfig::default(),
        session_config: SessionConfig::default(),
        pair_rounds: None,
        current,
    })
}

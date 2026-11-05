//! The session's side of the client, a hub: reads events, feeds the decoder, keeps the viewport and the
//! keyframe asks, runs the controller (picks, settings, the Desktop auto-ask), dials and pairs, browses for
//! Macs, runs the reconnect ladder, and publishes a [`Model`] for the window.

use crate::pipeline::{DecodeMsg, DecodeNote};
use crate::seam::{AppCommand, UiEvent, UiSink};
use sill_discovery::policy::reconnect_matches;
use sill_discovery::{classify, connect_by_address, connect_saved, reconnect, Browser, BrowserEvent, ConnectOutcome, DialConfig, Row, RowClass, RowTable};
use sill_discovery::{ReconnectEnd, ReconnectInputs, Supervisor};
use sill_input::PointerFeed;
use sill_media::cursor::decode_cursor;
use sill_media::recovery::{self, Decision, KeyframeRecovery};
use sill_media::{Panel, StatsCollector, ViewportSender};
use sill_net::pairing::{pair_with_code, pair_with_link, PairLink, PairOptions};
use sill_net::saved::{SavedMac, SavedMacs};
use sill_net::{CloseReason, Command, ConnectOptions, Event, Fingerprint, Identity, PairError, PinMode, Session, SessionConfig, SessionHandle};
use sill_proto::StreamSource;
use sill_ui_state::{ConnState, Controller, Effect, MacRow, Model, PairingView, UiIntent};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::{mpsc, watch};

/// The port a typed address gets when it names none (the Mac's remote door).
pub const DEFAULT_PORT: u16 = 7455;

/// What the hub needs to dial, pair, browse and reconnect. Without one the session just runs.
pub struct Link {
    pub identity: Identity,
    pub device: String,
    pub saved: SavedMacs,
    pub browser: Option<Arc<dyn Browser>>,
    pub dial: DialConfig,
    pub session_config: SessionConfig,
    /// PBKDF2 rounds for a typed code (tests lower it).
    pub pair_rounds: Option<u32>,
    /// The saved Mac the starting session belongs to.
    pub current: Option<String>,
}

pub struct Control {
    pub session: Option<Session>,
    pub cmds: UnboundedReceiver<AppCommand>,
    pub notes: UnboundedReceiver<DecodeNote>,
    pub decode: Sender<DecodeMsg>,
    pub stats: Arc<Mutex<StatsCollector>>,
    pub ui: UiSink,
    pub clock: Instant,
    /// The session is away from home (a VPN or the internet): 60 fps at most.
    pub remote: bool,
    pub link: Option<Link>,
    /// The local clipboard; `None` leaves the clipboard unshared.
    pub clipboard: Option<Box<dyn crate::clipboard::ClipboardBackend>>,
}

enum Job {
    Line(String),
    Reconnected(ReconnectEnd),
    Dialed(Result<(Session, String, Option<String>), String>),
    Paired(Result<SavedMac, PairError>),
    ClipboardLook(Option<String>),
}

struct Rc {
    row_tx: watch::Sender<Option<Vec<(String, u16)>>>,
    fg_tx: watch::Sender<bool>,
    tap_tx: mpsc::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

struct Hub {
    ui: UiSink,
    clock: Instant,
    remote: bool,
    decode: Sender<DecodeMsg>,
    stats: Arc<Mutex<StatsCollector>>,
    handle: Option<SessionHandle>,
    mac_fp: Option<Fingerprint>,
    ctl: Controller,
    viewport: ViewportSender,
    recovery: KeyframeRecovery,
    feed: PointerFeed,
    last_panel: Option<(Panel, Option<f64>)>,
    active: bool,
    streaming: bool,
    got_frames: bool,
    conn: ConnState,
    pairing: PairingView,
    link: Option<Link>,
    rows: RowTable,
    current: Option<String>,
    label: String,
    rc: Option<Rc>,
    jobs: UnboundedSender<Job>,
    clip: Option<crate::clipboard::ClipboardWorker>,
    last_look: f64,
    dirty: bool,
}

pub fn options(link: &Link, pin: PinMode) -> ConnectOptions {
    crate::client::connect_options(link.identity.clone(), &link.device, pin, link.session_config.clone())
}

fn row_addrs(r: &Row) -> Vec<(String, u16)> {
    r.addrs.iter().map(|a| (a.to_string(), r.port)).collect()
}

pub async fn run(mut c: Control) {
    let (jobs_tx, mut jobs) = unbounded_channel::<Job>();
    let (browser_tx, mut browser_rx) = unbounded_channel::<BrowserEvent>();
    if let Some(b) = c.link.as_ref().and_then(|l| l.browser.clone()) {
        tokio::spawn(sill_discovery::run_browser(b, browser_tx.clone(), |d| Box::pin(tokio::time::sleep(d))));
    }
    let current = c.link.as_mut().and_then(|l| l.current.take());
    let mut hub = Hub {
        ui: c.ui.clone(),
        clock: c.clock,
        remote: c.remote,
        decode: c.decode.clone(),
        stats: c.stats.clone(),
        handle: None,
        mac_fp: None,
        ctl: Controller::new(),
        viewport: ViewportSender::new(),
        recovery: KeyframeRecovery::new(),
        feed: PointerFeed::new(),
        last_panel: None,
        active: true,
        streaming: false,
        got_frames: false,
        conn: ConnState::Idle,
        pairing: PairingView::default(),
        link: c.link.take(),
        rows: RowTable::default(),
        current,
        label: String::new(),
        rc: None,
        jobs: jobs_tx.clone(),
        clip: c.clipboard.take().map(|b| {
            let tx = jobs_tx;
            crate::clipboard::ClipboardWorker::spawn(b, move |t| {
                let _ = tx.send(Job::ClipboardLook(t));
            })
        }),
        last_look: 0.0,
        dirty: true,
    };
    let mut session = c.session.take();
    if let Some(s) = &session {
        hub.install(s, "Mac".into());
    }
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    hub.publish();
    loop {
        let ev = async {
            match session.as_mut() {
                Some(s) => s.events.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            ev = ev => {
                match ev {
                    Some(ev) => { if hub.on_event(ev) { session = None; } }
                    None => { session = None; hub.handle = None; }
                }
            }
            cmd = c.cmds.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    AppCommand::Quit => {
                        if let Some(h) = &hub.handle { h.close(); }
                        break;
                    }
                    other => hub.on_cmd(other, session.is_some()),
                }
            }
            note = c.notes.recv() => {
                let now = hub.now();
                match note {
                    Some(DecodeNote::Interrupted(why)) => { if std::env::var_os("SILL_DEBUG").is_some() { eprintln!("interrupted: {why}"); } hub.recovery.interrupted(now) }
                    Some(DecodeNote::PictureBack) => hub.recovery.picture_back(now, hub.active),
                    None => {}
                }
            }
            job = jobs.recv() => {
                if let Some(job) = job {
                    if let Some(s) = hub.on_job(job) { session = Some(s); }
                }
            }
            b = browser_rx.recv() => {
                if let Some(b) = b { hub.on_browser(b); }
            }
            _ = tick.tick() => hub.on_tick(),
        }
        if hub.dirty {
            hub.publish();
        }
    }
    if let Some(h) = &hub.handle {
        h.close();
    }
    if let Some(rc) = hub.rc.take() {
        rc.task.abort();
    }
}

impl Hub {
    fn now(&self) -> f64 {
        self.clock.elapsed().as_secs_f64()
    }

    fn send(&self, cmd: Command) -> bool {
        self.handle.as_ref().is_some_and(|h| h.send(cmd))
    }

    /// A session is up: forget the last one's state and start this one's.
    fn install(&mut self, s: &Session, label: String) {
        let now = self.now();
        self.handle = Some(s.handle.clone());
        self.mac_fp = Some(s.mac_fingerprint);
        self.ctl.session_started();
        self.viewport = ViewportSender::new();
        if let Some((p, hz)) = self.last_panel {
            self.viewport.set(now, p, hz, self.remote, None);
        }
        self.recovery = KeyframeRecovery::new();
        self.feed = PointerFeed::new();
        self.streaming = false;
        self.got_frames = false;
        self.label = label;
        self.conn = ConnState::Connected;
        if let Some(rc) = self.rc.take() {
            rc.task.abort();
        }
        if let (Some(link), Some(id)) = (self.link.as_mut(), self.current.clone()) {
            if let Some(m) = link.saved.get_mut(&id) {
                m.last_connected_at = Some(wall());
                let _ = link.saved.save();
            }
        }
        self.dirty = true;
    }

    /// One session event; `true` when the session is over.
    fn on_event(&mut self, ev: Event) -> bool {
        let now = self.now();
        match ev {
            Event::WindowList(l) => {
                self.streaming = l.active != StreamSource::None || self.got_frames;
                self.ctl.set_worst_rtt(self.stats.lock().unwrap().snapshot(now).rtt_ms.map(|(_, max)| max as f64 / 1000.0));
                let effects = self.ctl.on_window_list(now, &l);
                self.apply(effects);
                (self.ui)(UiEvent::WindowList(l));
                self.dirty = true;
            }
            Event::HostSettings(s) => {
                self.ctl.on_settings(now, &s);
                self.dirty = true;
            }
            Event::Thumbnail { window_id, jpeg } => {
                self.ctl.on_thumbnail(window_id, &jpeg);
                self.dirty = true;
            }
            Event::AppIcon { bundle_id, png } => {
                self.ctl.on_icon(&bundle_id, &png);
                self.dirty = true;
            }
            Event::AppList(apps) => {
                self.ctl.on_apps(apps);
                self.dirty = true;
            }
            Event::MacInfo(check) => {
                if let (Some(link), Some(id), Some(fp)) = (self.link.as_mut(), self.current.clone(), self.mac_fp) {
                    if let Some(m) = link.saved.get_mut(&id) {
                        if m.apply_mac_info(&check, &fp) {
                            let _ = link.saved.save();
                        }
                    }
                }
            }
            Event::ParameterSets(ps) => {
                let _ = self.decode.send(DecodeMsg::Params(ps));
            }
            Event::Frame { timestamp, is_keyframe, payload } => {
                self.got_frames = true;
                self.streaming = true;
                self.ctl.on_frame(is_keyframe);
                let _ = self.decode.send(DecodeMsg::Frame { payload, timestamp, keyframe: is_keyframe });
            }
            Event::CursorShape(s) => (self.ui)(UiEvent::Cursor(decode_cursor(&s))),
            Event::MacPointer(p) => {
                // A report built before the host read this device's latest input is stale.
                if let Some(pos) = self.feed.accept(&p) {
                    (self.ui)(UiEvent::Pointer(pos));
                }
            }
            Event::Clipboard(c) => {
                let effects = self.ctl.on_clipboard(&c);
                self.apply(effects);
            }
            Event::MacMenu(m) => {
                if self.ctl.on_menu(&m) {
                    self.dirty = true;
                }
            }
            Event::Pong(rtt) => self.stats.lock().unwrap().record_rtt(now, rtt * 1000.0),
            Event::Goodbye(g) => {
                if let (Some(link), Some(id)) = (self.link.as_mut(), self.current.clone()) {
                    if let Some(m) = link.saved.get_mut(&id) {
                        if m.apply_goodbye(&g).is_some() {
                            let _ = link.saved.save();
                        }
                    }
                }
                self.dirty = true;
            }
            Event::Closed(reason) => {
                self.on_closed(reason);
                return true;
            }
            Event::Unknown(_) => {}
        }
        false
    }

    fn on_closed(&mut self, reason: CloseReason) {
        let now = self.now();
        let words = close_text(&reason, &self.label);
        (self.ui)(UiEvent::Closed(words.clone()));
        self.handle = None;
        self.ctl.session_ended();
        let mac = self.current.clone().and_then(|id| self.link.as_ref().and_then(|l| l.saved.get(&id).cloned()));
        match mac {
            Some(m) if reason.reconnects() && m.can_connect() => {
                let mut sup = Supervisor::new(&m.name);
                sup.session_ended(now, &reason);
                self.conn = ConnState::Reconnecting(sup.line().unwrap_or_else(|| format!("Reconnecting to {}\u{2026}", m.name)));
                self.start_reconnect(sup, m);
            }
            _ if reason == CloseReason::Local => {
                self.conn = ConnState::Idle;
                self.current = None;
            }
            _ => self.conn = ConnState::Ended(words),
        }
        self.dirty = true;
    }

    fn start_reconnect(&mut self, sup: Supervisor, mac: SavedMac) {
        let Some(link) = self.link.as_ref() else { return };
        let (row_tx, row) = watch::channel(self.row_for(&mac));
        let (fg_tx, foreground) = watch::channel(self.active);
        let (tap_tx, taps) = mpsc::channel(4);
        let (lines_tx, mut lines) = unbounded_channel::<String>();
        let jobs = self.jobs.clone();
        let jobs2 = self.jobs.clone();
        tokio::spawn(async move {
            while let Some(l) = lines.recv().await {
                if jobs2.send(Job::Line(l)).is_err() {
                    return;
                }
            }
        });
        let (identity, device, cfg, scfg) = (link.identity.clone(), link.device.clone(), link.dial.clone(), link.session_config.clone());
        let task = tokio::spawn(async move {
            let inp = ReconnectInputs { row, foreground, taps, lines: lines_tx };
            let end = reconnect(sup, inp, move |addrs| {
                let (mac, cfg) = (mac.clone(), cfg.clone());
                let opts = crate::client::connect_options(identity.clone(), &device, PinMode::TrustOnFirstUse, scfg.clone());
                async move { connect_saved(&mac, &addrs, opts, &cfg).await }
            })
            .await;
            let _ = jobs.send(Job::Reconnected(end));
        });
        self.rc = Some(Rc { row_tx, fg_tx, tap_tx, task });
    }

    /// The listed row of `mac`, as the ladder wants it: its dialable addresses, `None` while not listed.
    fn row_for(&self, mac: &SavedMac) -> Option<Vec<(String, u16)>> {
        let link = self.link.as_ref()?;
        self.rows.rows().iter().find(|r| reconnect_matches(mac, r, &link.saved.macs)).map(row_addrs)
    }

    fn refresh_row(&mut self) {
        let Some(id) = self.current.clone() else { return };
        let Some(mac) = self.link.as_ref().and_then(|l| l.saved.get(&id).cloned()) else { return };
        let row = self.row_for(&mac);
        if let Some(rc) = &self.rc {
            rc.row_tx.send_if_modified(|cur| {
                if *cur != row {
                    *cur = row.clone();
                    true
                } else {
                    false
                }
            });
        }
    }

    fn on_browser(&mut self, e: BrowserEvent) {
        self.rows.apply(&e);
        self.refresh_row();
        self.dirty = true;
    }

    fn on_tick(&mut self) {
        let now = self.now();
        if self.handle.is_none() {
            return;
        }
        let s = self.stats.lock().unwrap().snapshot(now);
        self.ctl.set_worst_rtt(s.rtt_ms.map(|(_, max)| max as f64 / 1000.0));
        let before = self.ctl.next_deadline();
        let effects = self.ctl.tick(now);
        self.apply(effects);
        if before.is_some_and(|t| t <= now) {
            self.dirty = true;
        }
        if self.ctl.clip.looking() && now - self.last_look >= sill_ui_state::clipboard::LOOK_INTERVAL {
            self.last_look = now;
            if let Some(w) = &self.clip {
                w.look();
            }
        }
        if let Some(v) = self.viewport.poll(now) {
            self.send(Command::Viewport(v));
        }
        let worst = s.rtt_ms.map(|(_, max)| max);
        if self.recovery.consider(now, self.active, self.streaming, recovery::answer_wait(worst)) == Decision::AskNow {
            self.send(Command::RequestKeyframe);
        }
    }

    fn apply(&mut self, effects: Vec<Effect>) {
        for e in effects {
            match e {
                Effect::Send(cmd) => {
                    self.send(cmd);
                }
                Effect::Hub(i) => self.hub_intent(i),
                Effect::WriteClipboard(t) => {
                    if let Some(w) = &self.clip {
                        w.write(t);
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn on_cmd(&mut self, cmd: AppCommand, connected: bool) {
        let now = self.now();
        match cmd {
            AppCommand::Panel { panel: p, refresh_hz } => {
                self.last_panel = Some((p, refresh_hz));
                self.viewport.set(now, p, refresh_hz, self.remote, None);
            }
            AppCommand::Active(a) => {
                if self.active && !a {
                    self.recovery.left_foreground();
                }
                self.active = a;
                if let Some(rc) = &self.rc {
                    let _ = rc.fg_tx.send(a);
                }
            }
            AppCommand::Session(cmd) => {
                let is_input = matches!(cmd, Command::Input(_));
                if self.send(cmd) && is_input {
                    self.feed.input_sent();
                }
            }
            AppCommand::Intent(i) => {
                if connected {
                    let effects = self.ctl.intent(now, i);
                    self.apply(effects);
                } else {
                    self.hub_intent(i);
                }
            }
            AppCommand::Quit => {}
        }
    }

    fn model(&self) -> Model {
        let mut m = if self.handle.is_some() { self.ctl.model() } else { Model::default() };
        m.conn = self.conn.clone();
        if self.handle.is_none() {
            m.mac_name = self.label.clone();
        }
        m.macs = self.mac_rows();
        m.pairing = self.pairing.clone();
        m
    }

    fn publish(&mut self) {
        self.dirty = false;
        (self.ui)(UiEvent::Model(Box::new(self.model())));
    }

    fn mac_rows(&self) -> Vec<MacRow> {
        let Some(link) = &self.link else { return vec![] };
        let saved = &link.saved.macs;
        let mut out: Vec<MacRow> = vec![];
        let mut shown: Vec<String> = vec![];
        for row in self.rows.rows() {
            let class = classify(&row, saved);
            let address = row.addrs.first().map(|a| format!("{a}:{}", row.port));
            let (key, is_saved) = match &class {
                RowClass::Paired { mac_id, .. } => (mac_id.clone(), true),
                RowClass::NotPaired { was_saved: Some(id), .. } => (id.clone(), false),
                _ => (row.name.clone(), false),
            };
            shown.push(key.clone());
            out.push(MacRow {
                key,
                address,
                open_door: class == RowClass::OpenDoor,
                name: row.name.clone(),
                word: class.word().into(),
                saved: is_saved,
                listed: true,
                dialable: class.dialable(),
                needs_pairing: matches!(class, RowClass::NotPaired { .. }),
            });
        }
        for m in saved {
            if shown.contains(&m.mac_id) {
                continue;
            }
            let (word, needs_pairing) = match m.standing {
                sill_net::Standing::Paired => ("Paired", false),
                sill_net::Standing::Revoked => ("Removed this computer. Pair it again.", true),
                sill_net::Standing::NeedsPairing => ("Asks to pair. Pair it again.", true),
            };
            out.push(MacRow {
                key: m.mac_id.clone(),
                address: m.dial_order(None).first().map(|a| format!("{}:{}", a.host, a.port)),
                open_door: false,
                name: m.name.clone(),
                word: word.into(),
                saved: true,
                listed: false,
                dialable: !needs_pairing,
                needs_pairing,
            });
        }
        out
    }

    // ------------------------------------------------------------------ connecting and pairing

    fn hub_intent(&mut self, i: UiIntent) {
        match i {
            UiIntent::Retry => {
                if let Some(rc) = &self.rc {
                    let _ = rc.tap_tx.try_send(());
                }
            }
            UiIntent::Disconnect => {
                if let Some(h) = &self.handle {
                    h.close();
                } else if let Some(rc) = self.rc.take() {
                    rc.task.abort();
                    self.conn = ConnState::Idle;
                    self.current = None;
                }
            }
            UiIntent::DismissPairing => self.pairing = PairingView::default(),
            UiIntent::ForgetMac(id) => {
                if let Some(link) = self.link.as_mut() {
                    link.saved.remove(&id);
                    let _ = link.saved.save();
                }
            }
            UiIntent::ConnectMac(key) => self.connect_mac(&key),
            UiIntent::ConnectAddress(text) => self.connect_address(&text),
            UiIntent::PairLink(text) => self.pair_link(&text),
            UiIntent::PairCode { address, code } => self.pair_code(&address, &code),
            _ => {}
        }
        self.dirty = true;
    }

    fn spawn_dial<F>(&mut self, label: String, mac_id: Option<String>, f: F)
    where
        F: std::future::Future<Output = ConnectOutcome> + Send + 'static,
    {
        self.conn = ConnState::Connecting(format!("Connecting to {label}\u{2026}"));
        let jobs = self.jobs.clone();
        tokio::spawn(async move {
            let r = match f.await {
                ConnectOutcome::Connected { session, .. } => Ok((session, label, mac_id)),
                other => Err(other.words(&label).unwrap_or_else(|| format!("{other:?}"))),
            };
            let _ = jobs.send(Job::Dialed(r));
        });
    }

    fn connect_mac(&mut self, key: &str) {
        if self.handle.is_some() {
            return;
        }
        let Some(link) = self.link.as_ref() else { return };
        if let Some(mac) = link.saved.get(key).cloned() {
            let discovered = self.rows.rows().iter().find(|r| reconnect_matches(&mac, r, &link.saved.macs)).map(row_addrs).unwrap_or_default();
            let (opts, cfg) = (options(link, PinMode::TrustOnFirstUse), link.dial.clone());
            let (name, id) = (mac.name.clone(), mac.mac_id.clone());
            self.spawn_dial(name, Some(id), async move { connect_saved(&mac, &discovered, opts, &cfg).await });
            return;
        }
        // A listed open door that is not saved: any key will do.
        if let Some(row) = self.rows.get(key).cloned() {
            if let (RowClass::OpenDoor, Some(addr)) = (classify(&row, &link.saved.macs), row_addrs(&row).first().cloned()) {
                self.dial_address(&row.name, addr.0, addr.1);
            }
        }
    }

    fn connect_address(&mut self, text: &str) {
        if self.handle.is_some() {
            return;
        }
        match sill_net::addr::parse(text) {
            Ok(a) => {
                let port = a.port.unwrap_or(DEFAULT_PORT);
                self.dial_address(&a.host.clone(), a.host, port);
            }
            Err(_) => self.conn = ConnState::Ended("That doesn\u{2019}t look like an address. Try 100.101.102.103 or mac.example.net.".into()),
        }
    }

    /// Dial `host:port`: a saved Mac with that address pinned, anything else trust-on-first-use (which an
    /// open door admits and a door that asks devices to pair refuses).
    fn dial_address(&mut self, label: &str, host: String, port: u16) {
        let Some(link) = self.link.as_ref() else { return };
        let saved = link.saved.macs.iter().find(|m| m.addresses.iter().any(|a| a.host == host && a.port == port)).cloned();
        let (opts, cfg) = (options(link, PinMode::TrustOnFirstUse), link.dial.clone());
        match saved {
            Some(mac) => {
                let (name, id) = (mac.name.clone(), mac.mac_id.clone());
                self.spawn_dial(name, Some(id), async move { connect_saved(&mac, &[], opts, &cfg).await });
            }
            None => self.spawn_dial(label.to_string(), None, async move { connect_by_address(&host, port, PinMode::TrustOnFirstUse, opts, &cfg).await }),
        }
    }

    fn pair_options(&self) -> Option<PairOptions> {
        let link = self.link.as_ref()?;
        let mut o = PairOptions::new(link.identity.clone(), &link.device);
        if let Some(r) = link.pair_rounds {
            o.rounds = r;
        }
        Some(o)
    }

    fn pair_link(&mut self, text: &str) {
        let Some(opts) = self.pair_options() else { return };
        let link = match PairLink::parse(text) {
            Ok(l) => l,
            Err(f) => {
                self.pairing = PairingView { busy: false, error: Some(f.text().into()), done: None };
                return;
            }
        };
        self.pairing = PairingView { busy: true, error: None, done: None };
        let jobs = self.jobs.clone();
        tokio::spawn(async move {
            let _ = jobs.send(Job::Paired(pair_with_link(&link, &opts).await));
        });
    }

    fn pair_code(&mut self, address: &str, code: &str) {
        let Some(opts) = self.pair_options() else { return };
        let a = match sill_net::addr::parse(address) {
            Ok(a) => a,
            Err(_) => {
                self.pairing = PairingView { busy: false, error: Some(PairError::NoAddress.text("the Mac")), done: None };
                return;
            }
        };
        self.pairing = PairingView { busy: true, error: None, done: None };
        let (code, jobs) = (code.to_string(), self.jobs.clone());
        let port = a.port.unwrap_or(DEFAULT_PORT);
        tokio::spawn(async move {
            let _ = jobs.send(Job::Paired(pair_with_code(&code, &a.host, port, None, &opts).await));
        });
    }

    fn on_job(&mut self, job: Job) -> Option<Session> {
        match job {
            Job::ClipboardLook(t) => {
                if self.handle.is_some() {
                    let now = self.now();
                    let effects = self.ctl.on_local_clipboard(now, t.as_deref());
                    self.apply(effects);
                }
                None
            }
            Job::Line(l) => {
                if matches!(self.conn, ConnState::Reconnecting(_)) {
                    self.conn = ConnState::Reconnecting(l);
                }
                self.dirty = true;
                None
            }
            Job::Reconnected(end) => {
                self.dirty = true;
                match end {
                    ReconnectEnd::Connected { session, .. } => {
                        let label = self.link.as_ref().zip(self.current.as_ref()).and_then(|(l, id)| l.saved.get(id)).map_or("Mac".into(), |m| m.name.clone());
                        self.rc = None;
                        self.install(&session, label);
                        Some(session)
                    }
                    ReconnectEnd::NewKey => {
                        let name = self.label.clone();
                        self.rc = None;
                        self.conn = ConnState::Ended(format!("{name} has a new key since this computer paired. Pair it again."));
                        None
                    }
                    ReconnectEnd::NotAllowed => {
                        let name = self.label.clone();
                        self.rc = None;
                        self.conn = ConnState::Ended(format!("{name} no longer takes this computer. Pair it again."));
                        None
                    }
                    ReconnectEnd::Cancelled => None,
                }
            }
            Job::Dialed(r) => {
                self.dirty = true;
                match r {
                    Ok((session, label, mac_id)) => {
                        self.current = mac_id;
                        self.install(&session, label);
                        Some(session)
                    }
                    Err(words) => {
                        self.conn = ConnState::Ended(words);
                        None
                    }
                }
            }
            Job::Paired(r) => {
                self.dirty = true;
                match r {
                    Ok(mac) => {
                        let (name, id) = (mac.name.clone(), mac.mac_id.clone());
                        if let Some(link) = self.link.as_mut() {
                            link.saved.upsert(mac);
                            if let Err(e) = link.saved.save() {
                                eprintln!("sill: {e}");
                            }
                        }
                        self.pairing = PairingView { busy: false, error: None, done: Some(format!("Paired with {name}.")) };
                        // Pairing done: connect, as the iOS client does.
                        self.connect_mac(&id);
                    }
                    Err(e) => self.pairing = PairingView { busy: false, error: Some(e.text("the Mac")), done: None },
                }
                None
            }
        }
    }
}

fn wall() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

pub fn close_text(r: &CloseReason, mac: &str) -> String {
    let mac = if mac.is_empty() { "The Mac" } else { mac };
    match r {
        CloseReason::Goodbye(g) => sill_net::policy::goodbye_text(g, mac, "computer"),
        CloseReason::Silent => format!("{mac} stopped answering."),
        CloseReason::Eof => format!("{mac} closed the connection."),
        CloseReason::ReadError(e) => format!("Connection lost: {e}"),
        CloseReason::Protocol(e) => format!("{mac} sent something this client cannot read: {e}"),
        CloseReason::Local => "Disconnected.".into(),
    }
}

//! A session over one TLS connection: Hello first, a reader that turns bytes into [`Event`]s, a writer
//! fed by a channel, and the timers (ping, stats, liveness). Decisions live in [`crate::policy`].

use crate::identity::Identity;
use crate::pin::Fingerprint;
use crate::policy::{self, GoodbyeOutcome, KeyframeLimiter, StatsWindow};
use crate::tls::{self, Alpn, Observed, PinMode};
use sill_proto::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_rustls::TlsConnector;

#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// The floor of the liveness rule (6 s); tests lower it.
    pub min_silence: Duration,
    pub ping_interval: Duration,
    pub connect_timeout: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            min_silence: Duration::from_secs(6),
            ping_interval: Duration::from_millis(250),
            connect_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConnectOptions {
    pub identity: Identity,
    pub pin: PinMode,
    /// Name in the Hello and in stats ("Linux", the host name…).
    pub device: String,
    pub app_version: String,
    pub build: String,
    pub config: SessionConfig,
}

#[derive(Debug)]
pub enum ConnectError {
    Io(String),
    Timeout,
    /// The Mac presented another key than the pin: nothing was sent.
    PinMismatch { expected: Fingerprint, got: Fingerprint },
    Tls(String),
    Alpn(Option<Vec<u8>>),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Io(e) => write!(f, "connection failed: {e}"),
            ConnectError::Timeout => write!(f, "connection timed out"),
            ConnectError::PinMismatch { .. } => write!(f, "the Mac presented a different key than the one paired"),
            ConnectError::Tls(e) => write!(f, "TLS failed: {e}"),
            ConnectError::Alpn(a) => write!(f, "unexpected ALPN {a:?}"),
        }
    }
}
impl std::error::Error for ConnectError {}

#[derive(Clone, Debug, PartialEq)]
pub enum CloseReason {
    Goodbye(GoodbyeOutcome),
    /// No byte for the liveness timeout.
    Silent,
    /// The peer closed without a goodbye.
    Eof,
    ReadError(String),
    /// The stream broke the rules (a header over its cap); it cannot be resynchronised.
    Protocol(String),
    /// This side closed it.
    Local,
}

impl CloseReason {
    /// Whether the app should redial by itself (M4's schedule decides when).
    pub fn reconnects(&self) -> bool {
        match self {
            CloseReason::Goodbye(g) => g.reconnect,
            CloseReason::Local | CloseReason::Protocol(_) => false,
            CloseReason::Silent | CloseReason::Eof | CloseReason::ReadError(_) => true,
        }
    }
}

#[derive(Debug)]
pub enum Event {
    WindowList(WindowList),
    HostSettings(HostSettingsState),
    /// Kind 18, checked against the session's own handshake key: only `Verified` names the Mac.
    MacInfo(crate::macinfo::MacInfoCheck),
    ParameterSets(ParameterSets),
    Frame { timestamp: f64, is_keyframe: bool, payload: Vec<u8> },
    Thumbnail { window_id: u32, jpeg: Vec<u8> },
    AppIcon { bundle_id: String, png: Vec<u8> },
    AppList(Vec<AppInfo>),
    CursorShape(CursorShape),
    MacPointer(MacPointer),
    /// Kind 32 from the Mac: its clipboard text (or a stop it never sends).
    Clipboard(Clipboard),
    /// Kind 24: a top level, or the answer to one fetch or press.
    MacMenu(MacMenu),
    /// Round trip in seconds.
    Pong(f64),
    Goodbye(GoodbyeOutcome),
    /// A kind this build does not surface (unknown, not yet handled, or its JSON did not decode):
    /// skipped, never fatal. The wire number of an unknown kind is not kept by the decoder.
    Unknown(Kind),
    /// Always the last event.
    Closed(CloseReason),
}

/// What the app can ask of a session.
#[derive(Clone, Debug)]
pub enum Command {
    Input(InputEvent),
    SelectSource(StreamSource),
    /// Sent through [`ViewportBounds::held`]; one that cannot be held (no panel yet) sends nothing.
    Viewport(Viewport),
    LaunchApp(String),
    WindowCommand(WindowCommand),
    ChangeSettings(HostSettingsChange),
    /// Rate limited to one per 2 s.
    /// Kind 32: a subscription (`{}`), a stop or text.
    Clipboard(Clipboard),
    /// Kind 27 (no id: the subscription to the top level).
    FetchMenu(FetchMenu),
    /// Kind 25.
    PressMenu(PressMenuItem),
    /// Kind 28.
    Gesture(TrackpadGesture),
    RequestKeyframe,
    Raw(Message),
}

#[derive(Clone)]
pub struct SessionHandle {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    stop: Arc<watch::Sender<bool>>,
    keyframes: Arc<Mutex<KeyframeLimiter>>,
    start: Instant,
}

impl SessionHandle {
    /// Queue a command; `false` when it was dropped (rate limit, unholdable viewport, closed session).
    pub fn send(&self, c: Command) -> bool {
        let msg = match c {
            Command::Input(e) => Message::client(Kind::Input, encode_json(&e)),
            Command::SelectSource(s) => Message::client(Kind::SelectSource, encode_json(&s)),
            Command::Viewport(v) => match ViewportBounds::held(&v) {
                Some(v) => Message::client(Kind::Viewport, encode_json(&v)),
                None => return false,
            },
            Command::LaunchApp(bundle_id) => Message::client(Kind::LaunchApp, encode_json(&LaunchApp { bundle_id })),
            Command::WindowCommand(w) => Message::client(Kind::WindowCommand, encode_json(&w)),
            Command::ChangeSettings(c) => Message::client(Kind::ChangeSettings, encode_json(&c)),
            Command::Clipboard(c) => Message::client(Kind::Clipboard, encode_json(&c)),
            Command::FetchMenu(f) => Message::client(Kind::FetchMenu, encode_json(&f)),
            Command::PressMenu(p) => Message::client(Kind::PressMenuItem, encode_json(&p)),
            Command::Gesture(g) => Message::client(Kind::Gesture, encode_json(&g)),
            Command::RequestKeyframe => {
                if !self.keyframes.lock().unwrap().allow(self.start.elapsed().as_secs_f64()) {
                    return false;
                }
                Message::client(Kind::KeyframeRequest, Vec::new())
            }
            Command::Raw(m) => m,
        };
        self.tx.send(msg.serialize()).is_ok()
    }

    pub fn close(&self) {
        let _ = self.stop.send(true);
    }
}

pub struct Session {
    pub events: mpsc::Receiver<Event>,
    pub handle: SessionHandle,
    /// The Mac's key as the handshake saw it.
    pub mac_fingerprint: Fingerprint,
}

/// A TLS stream to `host:port` with the session's ALPN, handshake finished, nothing sent yet.
/// Pairing (M3) calls this with [`Alpn::Pairing`] and [`PinMode::TrustOnFirstUse`].
pub async fn connect_tls(
    addr: (&str, u16),
    identity: &Identity,
    alpn: Alpn,
    pin: PinMode,
    timeout: Duration,
) -> Result<(tokio_rustls::client::TlsStream<TcpStream>, Fingerprint), ConnectError> {
    let (cfg, observed): (_, Observed) = tls::client_config(identity, alpn, pin.clone()).map_err(ConnectError::Tls)?;
    let work = async {
        let tcp = TcpStream::connect(addr).await.map_err(|e| ConnectError::Io(e.to_string()))?;
        let _ = tcp.set_nodelay(true);
        let name = rustls::pki_types::ServerName::try_from(tls::SNI).expect("static name");
        match TlsConnector::from(cfg).connect(name, tcp).await {
            Ok(s) => Ok(s),
            Err(e) => match (&pin, observed.get()) {
                (PinMode::Pinned(want), Some(got)) if *want != got => {
                    Err(ConnectError::PinMismatch { expected: *want, got })
                }
                _ => Err(ConnectError::Tls(e.to_string())),
            },
        }
    };
    let stream = tokio::time::timeout(timeout, work).await.map_err(|_| ConnectError::Timeout)??;
    let got = stream.get_ref().1.alpn_protocol().map(|a| a.to_vec());
    if got.as_deref() != Some(alpn.wire()) {
        return Err(ConnectError::Alpn(got));
    }
    let fp = observed.get().ok_or_else(|| ConnectError::Tls("no server certificate".into()))?;
    Ok((stream, fp))
}

pub async fn connect(addr: (&str, u16), opts: ConnectOptions) -> Result<Session, ConnectError> {
    let (stream, fp) =
        connect_tls(addr, &opts.identity, Alpn::Session, opts.pin.clone(), opts.config.connect_timeout).await?;
    let (rd, wr) = tokio::io::split(stream);
    let (tx, wrx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (stop, stop_rx) = watch::channel(false);
    let stop = Arc::new(stop);
    let start = Instant::now();
    let handle = SessionHandle { tx, stop: stop.clone(), keyframes: Arc::default(), start };

    // Hello is queued before anything else can be: it is the first message on the wire.
    let hello = Hello {
        app_version: Some(opts.app_version.clone()),
        build: Some(opts.build.clone()),
        protocol: Some(PROTOCOL),
        device: Some(opts.device.clone()),
        audio: None,
    };
    let _ = handle.tx.send(Message::client(Kind::Hello, encode_json(&hello)).serialize());

    tokio::spawn(writer(wr, wrx, stop_rx.clone()));
    let (etx, events) = mpsc::channel(1024);
    tokio::spawn(reader(rd, etx, handle.clone(), stop_rx, opts, fp));
    Ok(Session { events, handle, mac_fingerprint: fp })
}

async fn writer<W: tokio::io::AsyncWrite + Unpin>(
    mut wr: W,
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            m = rx.recv() => match m {
                Some(bytes) => if wr.write_all(&bytes).await.is_err() || wr.flush().await.is_err() { return },
                None => break,
            },
            _ = stop.changed() => break,
        }
    }
    let _ = wr.shutdown().await;
}

fn wall_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// What a message becomes as an event; `None` for ones only liveness cares about.
fn to_event(m: Message, mac_fp: &Fingerprint) -> Option<Event> {
    let bad = |k| Some(Event::Unknown(k));
    Some(match m.kind {
        Kind::WindowList => json_event(&m, Event::WindowList),
        Kind::HostSettings => json_event(&m, Event::HostSettings),
        Kind::MacInfo => match decode_json::<SignedMacInfo>(&m.payload) {
            Some(s) => Event::MacInfo(crate::macinfo::check(&s, mac_fp)),
            None => return bad(m.kind),
        },
        Kind::MacPointer => json_event(&m, Event::MacPointer),
        Kind::Clipboard => json_event(&m, Event::Clipboard),
        Kind::MacMenu => json_event(&m, Event::MacMenu),
        Kind::AppList => json_event(&m, Event::AppList),
        Kind::ParameterSets => match ParameterSets::decode(&m.payload) {
            Some(p) => Event::ParameterSets(p),
            None => return bad(m.kind),
        },
        Kind::Frame => Event::Frame { timestamp: m.timestamp, is_keyframe: m.is_keyframe, payload: m.payload },
        Kind::Thumbnail => match decode_thumbnail(&m.payload) {
            Some((window_id, jpeg)) => Event::Thumbnail { window_id, jpeg: jpeg.to_vec() },
            None => return bad(m.kind),
        },
        Kind::AppIcon => match decode_icon(&m.payload) {
            Some((b, png)) => Event::AppIcon { bundle_id: b.to_string(), png: png.to_vec() },
            None => return bad(m.kind),
        },
        Kind::CursorShape => match CursorShape::decode(&m.payload) {
            Some(c) => Event::CursorShape(c),
            None => return bad(m.kind),
        },
        Kind::Tick | Kind::Pong | Kind::Goodbye => return None,
        k => Event::Unknown(k),
    })
}

async fn reader<R: tokio::io::AsyncRead + Unpin>(
    mut rd: R,
    etx: mpsc::Sender<Event>,
    handle: SessionHandle,
    mut stop: watch::Receiver<bool>,
    opts: ConnectOptions,
    mac_fp: Fingerprint,
) {
    let reason = run(&mut rd, &etx, &handle, &mut stop, &opts, mac_fp).await;
    let _ = etx.send(Event::Closed(reason)).await;
    // Ends the writer too (it sends close_notify on the way out).
    let _ = handle.stop.send(true);
}

async fn run<R: tokio::io::AsyncRead + Unpin>(
    rd: &mut R,
    etx: &mpsc::Sender<Event>,
    handle: &SessionHandle,
    stop: &mut watch::Receiver<bool>,
    opts: &ConnectOptions,
    mac_fp: Fingerprint,
) -> CloseReason {
    let start = handle.start;
    let clock = || start.elapsed().as_secs_f64();
    let mut dec = Decoder::new(Direction::HostToClient);
    let mut buf = vec![0u8; 64 * 1024];
    let mut window = StatsWindow::default();
    let mut last_worst_rtt = 0.0f64;
    let mut last_rx = Instant::now();
    let mut ping = tokio::time::interval(opts.config.ping_interval);
    let mut stats = tokio::time::interval(Duration::from_secs(1));
    let mut live = tokio::time::interval(Duration::from_millis(50));
    stats.reset();
    loop {
        tokio::select! {
            r = rd.read(&mut buf) => {
                let n = match r {
                    Ok(0) => return CloseReason::Eof,
                    Ok(n) => n,
                    Err(e) => return CloseReason::ReadError(e.to_string()),
                };
                last_rx = Instant::now(); // any byte is a sign of life, even half a frame
                dec.push(&buf[..n]);
                loop {
                    let m = match dec.next_message() {
                        Ok(Some(m)) => m,
                        Ok(None) => break,
                        Err(e) => return CloseReason::Protocol(e.to_string()),
                    };
                    match m.kind {
                        Kind::Pong => {
                            if let Some(rtt) = policy::rtt_from_pong(&m.payload, clock()) {
                                window.rtt(rtt * 1000.0);
                                if etx.send(Event::Pong(rtt)).await.is_err() { return CloseReason::Local }
                            }
                            continue;
                        }
                        Kind::Frame => window.frame(((wall_now() - m.timestamp) * 1000.0).max(0.0)),
                        Kind::Goodbye => {
                            let g = decode_json::<Goodbye>(&m.payload).unwrap_or(Goodbye {
                                reason: String::new(), message: None, minimum_version: None, reconnect: None,
                            });
                            let out = policy::goodbye_outcome(&g);
                            let _ = etx.send(Event::Goodbye(out.clone())).await;
                            return CloseReason::Goodbye(out);
                        }
                        _ => {}
                    }
                    if let Some(ev) = to_event(m, &mac_fp) {
                        if etx.send(ev).await.is_err() { return CloseReason::Local }
                    }
                }
            }
            _ = ping.tick() => {
                let _ = handle.tx.send(Message::client(Kind::Ping, policy::ping_payload(clock())).serialize());
            }
            _ = stats.tick() => {
                if let Some(w) = window.worst_rtt_s() { last_worst_rtt = w; }
                let s = window.close(&opts.device);
                let _ = handle.tx.send(Message::client(Kind::ClientStats, encode_json(&s)).serialize());
            }
            _ = live.tick() => {
                let worst = window.worst_rtt_s().unwrap_or(0.0).max(last_worst_rtt);
                let limit = policy::liveness_timeout(opts.config.min_silence.as_secs_f64(), worst);
                if last_rx.elapsed().as_secs_f64() > limit { return CloseReason::Silent }
            }
            _ = stop.changed() => return CloseReason::Local,
        }
    }
}

/// A JSON payload as its event, or `Unknown(kind)` when it does not decode (skipped, never fatal).
fn json_event<T: serde::de::DeserializeOwned>(m: &Message, make: fn(T) -> Event) -> Event {
    decode_json(&m.payload).map(make).unwrap_or(Event::Unknown(m.kind))
}

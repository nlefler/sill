//! A TLS 1.3 host that speaks Sill's host side well enough to test a client without a Mac.
//!
//! It records every message it receives, answers the Hello with the usual opening (window list,
//! settings, Mac info, parameter sets, a few frames), echoes pings, and can be told to send a goodbye,
//! raw bytes (unknown kinds, oversized headers) or to fall silent.

use rustls::client::danger::HandshakeSignatureValid;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::{DigitallySignedStruct, DistinguishedName, Error, ServerConfig, SignatureScheme};
use sill_net::identity::Identity;
use sill_net::pairing::{self, PairLink};
use sill_net::pin::{self, Fingerprint};
use sill_net::{b64, macinfo};
use sill_proto::*;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Notify};
use tokio_rustls::TlsAcceptor;

#[derive(Clone, Debug)]
pub struct MockConfig {
    /// Refuse a client whose key does not hash to this; `None` admits any key (an open door).
    pub require_client_pin: Option<Fingerprint>,
    pub identity: Option<Identity>,
    pub alpns: Vec<Vec<u8>>,
    /// How many synthetic frames follow the opening.
    pub frames: usize,
    /// Speak `sill-pair/1` too: a pairing window with a code and a QR secret.
    pub pairing: Option<MockPairing>,
    /// Sign kind 18 with this key instead of the host's own (a relay or another Mac).
    pub macinfo_signer: Option<Identity>,
    /// Send kind 18 with a flipped byte in the signed info.
    pub macinfo_tamper: bool,
    /// Stream a looping clip after the client picks a source (kind 6), and answer kind 44.
    pub video: Option<MockVideo>,
}

/// A clip as the host sends it: kind 0, then kind 1 frames at `fps`, looping.
#[derive(Clone, Debug)]
pub struct MockVideo {
    pub params: ParameterSets,
    /// (keyframe, length-prefixed NALs)
    pub frames: Vec<(bool, Vec<u8>)>,
    pub fps: f64,
    /// Corrupt the n-th frame sent (counting across restarts), once: its payload is cut short.
    pub corrupt_frame: Option<usize>,
    /// A kind 14 payload sent when streaming starts, and then a Mac pointer on a circle with every frame.
    pub cursor_payload: Option<Vec<u8>>,
}

/// What a pairing window holds, and how the host may misbehave.
#[derive(Clone, Debug)]
pub struct MockPairing {
    pub code: String,
    pub secret: [u8; 16],
    pub max_tries: i64,
    pub name: String,
    pub recognition_key: [u8; 32],
    pub rounds: u32,
    pub fault: PairFault,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PairFault {
    None,
    /// proof_M with a flipped bit.
    TamperProofM,
    /// Read the request, never answer, keep the connection open.
    Silent,
    /// Answer with this instead of judging the proof.
    Reply(Box<PairResult>),
    /// ok with a proof computed under a key derived from the wrong code (a Mac that does not know it).
    WrongKey,
}

impl MockPairing {
    pub fn new(code: &str) -> MockPairing {
        MockPairing { code: code.into(), secret: [0x5a; 16], max_tries: 5, name: "Mock Mac".into(), recognition_key: [0x33; 32], rounds: pairing::ROUNDS, fault: PairFault::None }
    }
}

impl Default for MockConfig {
    fn default() -> Self {
        MockConfig { require_client_pin: None, identity: None, alpns: vec![b"sill/1".to_vec()], frames: 3, pairing: None, macinfo_signer: None, macinfo_tamper: false, video: None }
    }
}

#[derive(Default, Debug)]
struct Shared {
    pairing: Mutex<PairState>,
    paired: Mutex<Vec<Fingerprint>>,
    received: Mutex<Vec<Message>>,
    clients: Mutex<Vec<Fingerprint>>,
    handshake_failures: Mutex<usize>,
    out: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    silent: Mutex<bool>,
    changed: Notify,
}

pub struct MockHost {
    pairing_cfg: Option<MockPairing>,
    pub addr: SocketAddr,
    /// The key this host presents; pin it from the client.
    pub fingerprint: Fingerprint,
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct AnyClient {
    pin: Option<Fingerprint>,
    shared: Arc<Shared>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ClientCertVerifier for AnyClient {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(&self, end: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: UnixTime) -> Result<ClientCertVerified, Error> {
        let fp = pin::fingerprint_of_cert(end).ok_or(Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
        if self.pin.is_some_and(|p| p != fp) {
            return Err(Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure));
        }
        self.shared.clients.lock().unwrap().push(fp);
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::General("TLS 1.2 is not spoken".into()))
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        pin::verify_signature(&self.provider, m, c, d)?;
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn opening(frames: usize, info: SignedMacInfo, video: bool) -> Vec<Message> {
    let list = WindowList {
        mac_name: "Mock Mac".into(),
        windows: vec![WindowInfo { id: 7, title: "Notes".into(), app_name: "Notes".into(), bundle_id: "com.apple.Notes".into(), width: 800, height: 600 }],
        active: if video { StreamSource::None } else { StreamSource::Desktop },
        launch_id: Some("mock-launch".into()),
        host_version: Some("0.6.0".into()),
        protocol: Some(PROTOCOL),
        gestures: Some(1),
        clipboard: Some(1),
        key_repeat: Some(1),
        locked: None,
        off_console: None,
    };
    let settings = HostSettingsState {
        settings: StreamSettings {
            max_fps: 60, bitrate: 15_000_000, capture_scale: 2.0, prioritize_speed: false, virtual_display: false,
            direct_wireless: None, send_audio: None,
        },
        persistent: false,
        virtual_display_available: false,
        virtual_display_note: None,
        software_encoder: true,
        stream: None,
        answering: None,
        away: None,
        link: None,
        audio_note: None,
        session_paired: Some(true),
    };
    let mut v = vec![
        Message::new(Kind::WindowList, 0.0, false, encode_json(&list)),
        Message::new(Kind::HostSettings, 0.0, false, encode_json(&settings)),
        Message::new(Kind::MacInfo, 0.0, false, encode_json(&info)),
    ];
    if !video {
        v.push(Message::new(Kind::ParameterSets, 0.0, false, ParameterSets { nal_unit_header_length: 4, sets: vec![vec![0x40, 1], vec![0x42, 1], vec![0x44, 1]] }.encode()));
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    for i in 0..if video { 0 } else { frames } {
        v.push(Message::new(Kind::Frame, now, i == 0, vec![0, 0, 0, 2, 0x26, i as u8]));
    }
    v
}

impl MockHost {
    pub async fn start(cfg: MockConfig) -> std::io::Result<MockHost> {
        let identity = match cfg.identity.clone() {
            Some(i) => i,
            None => Identity::generate_in_memory().map_err(std::io::Error::other)?,
        };
        let shared = Arc::new(Shared::default());
        let provider = pin::provider();
        let verifier = Arc::new(AnyClient { pin: cfg.require_client_pin, shared: shared.clone(), provider: provider.clone() });
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der.clone()));
        let mut sc = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(std::io::Error::other)?
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![CertificateDer::from(identity.cert_der.clone())], key)
            .map_err(std::io::Error::other)?;
        let mut alpns = cfg.alpns.clone();
        if cfg.pairing.is_some() {
            alpns.push(b"sill-pair/1".to_vec());
        }
        sc.alpn_protocols = alpns;
        sc.send_tls13_tickets = 0;
        let acceptor = TlsAcceptor::from(Arc::new(sc));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let sh = shared.clone();
        let frames = cfg.frames;
        let video = cfg.video.clone();
        let info = macinfo_for(&identity, &cfg);
        *shared.pairing.lock().unwrap() = PairState { tries_left: cfg.pairing.as_ref().map_or(0, |p| p.max_tries), closed: None };
        let pairing = cfg.pairing.clone().map(|p| (p, identity.fingerprint));
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else { return };
                let (acceptor, sh, info, pairing, video) = (acceptor.clone(), sh.clone(), info.clone(), pairing.clone(), video.clone());
                tokio::spawn(async move {
                    match acceptor.accept(tcp).await {
                        Ok(tls) => {
                            if tls.get_ref().1.alpn_protocol() == Some(b"sill-pair/1".as_slice()) {
                                if let Some((p, fp)) = pairing {
                                    serve_pairing(tls, sh, p, fp).await;
                                }
                            } else {
                                serve(tls, sh, frames, info, video).await
                            }
                        }
                        Err(_) => {
                            *sh.handshake_failures.lock().unwrap() += 1;
                            sh.changed.notify_waiters();
                        }
                    }
                });
            }
        });
        Ok(MockHost { pairing_cfg: cfg.pairing, addr, fingerprint: identity.fingerprint, shared })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Everything the client has sent, in order, across connections.
    pub fn received(&self) -> Vec<Message> {
        self.shared.received.lock().unwrap().clone()
    }

    pub fn received_kinds(&self) -> Vec<Kind> {
        self.received().iter().map(|m| m.kind).collect()
    }

    /// Client certificate fingerprints seen in handshakes.
    pub fn client_fingerprints(&self) -> Vec<Fingerprint> {
        self.shared.clients.lock().unwrap().clone()
    }

    /// Handshakes that failed on the host's side (e.g. the client aborted over a wrong pin).
    pub fn handshake_failures(&self) -> usize {
        *self.shared.handshake_failures.lock().unwrap()
    }

    /// Wait until `pred` holds over what was received, or `timeout` passes.
    pub async fn wait_for(&self, timeout: Duration, pred: impl Fn(&[Message]) -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if pred(&self.shared.received.lock().unwrap()) {
                return true;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            let _ = tokio::time::timeout(left.min(Duration::from_millis(20)), self.shared.changed.notified()).await;
        }
    }

    /// Send a message on the current connection.
    pub fn send(&self, m: &Message) -> bool {
        self.send_raw(m.serialize())
    }

    /// Send bytes as they are: an unknown kind, or a header announcing too much.
    pub fn send_raw(&self, bytes: Vec<u8>) -> bool {
        self.shared.out.lock().unwrap().as_ref().is_some_and(|t| t.send(bytes).is_ok())
    }

    pub fn send_goodbye(&self, g: &Goodbye) -> bool {
        self.send(&Message::new(Kind::Goodbye, 0.0, false, encode_json(g)))
    }

    /// From now on nothing is sent, pongs included: the connection stays open.
    pub fn go_silent(&self) {
        *self.shared.silent.lock().unwrap() = true;
    }
}

async fn serve(tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>, sh: Arc<Shared>, frames: usize, info: SignedMacInfo, video: Option<MockVideo>) {
    let (mut rd, mut wr) = tokio::io::split(tls);
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    *sh.out.lock().unwrap() = Some(tx.clone());
    *sh.silent.lock().unwrap() = false;
    let shw = sh.clone();
    tokio::spawn(async move {
        while let Some(b) = rx.recv().await {
            if *shw.silent.lock().unwrap() {
                continue;
            }
            if wr.write_all(&b).await.is_err() {
                return;
            }
        }
    });
    let mut dec = Decoder::new(Direction::ClientToHost);
    let mut buf = vec![0u8; 16 * 1024];
    let mut hello_seen = false;
    let restart = Arc::new(Notify::new());
    let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut streamer: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        let Ok(n) = rd.read(&mut buf).await else { return };
        if n == 0 {
            return;
        }
        dec.push(&buf[..n]);
        while let Ok(Some(m)) = dec.next_message() {
            if m.kind == Kind::Ping {
                let _ = tx.send(Message::new(Kind::Pong, 0.0, false, m.payload.clone()).serialize());
            }
            if m.kind == Kind::Hello && !hello_seen {
                hello_seen = true;
                for o in opening(frames, info.clone(), video.is_some()) {
                    let _ = tx.send(o.serialize());
                }
            }
            if let Some(v) = &video {
                match m.kind {
                    Kind::SelectSource if streamer.is_none() => {
                        streamer = Some(tokio::spawn(stream_video(v.clone(), tx.clone(), restart.clone(), sent.clone())));
                    }
                    Kind::KeyframeRequest => restart.notify_one(),
                    _ => {}
                }
            }
            sh.received.lock().unwrap().push(m);
            sh.changed.notify_waiters();
        }
    }
}

#[derive(Debug, Default)]
struct PairState {
    tries_left: i64,
    /// Why the window is closed ("closed", "expired", "stopped"); `None` while open.
    closed: Option<&'static str>,
}

fn macinfo_for(identity: &Identity, cfg: &MockConfig) -> SignedMacInfo {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    let info = MacInfo {
        v: 1,
        mac_id: pairing::mac_id(&identity.fingerprint),
        name: "Mock Mac".into(),
        issued_at: now,
        remote_access: true,
        remote_port: 7455,
        internet: false,
        addresses: vec![MacAddress { host: "192.0.2.7".into(), port: None, kind: "lan".into(), via: "en0".into() }],
    };
    let signer = cfg.macinfo_signer.as_ref().unwrap_or(identity);
    let mut s = macinfo::sign(signer, &info).expect("sign kind 18");
    if cfg.macinfo_tamper {
        let mut b = b64::std_decode(&s.info).unwrap();
        b[12] ^= 1;
        s.info = b64::std_encode(&b);
    }
    s
}

impl MockHost {
    /// The `sill://pair` link the Mac's window would show, with this host's loopback address.
    pub fn pair_link(&self) -> PairLink {
        let p = self.pairing_cfg.as_ref().expect("a host started with pairing");
        PairLink::new(self.fingerprint, p.secret, &p.name, self.port(), vec![sill_net::addr::parse(&format!("127.0.0.1:{}", self.port())).unwrap()])
    }

    pub fn pairing_code(&self) -> String {
        self.pairing_cfg.as_ref().expect("a host started with pairing").code.clone()
    }

    /// Close the pairing window: "closed", "expired" or "stopped".
    pub fn close_pairing(&self, reason: &'static str) {
        self.shared.pairing.lock().unwrap().closed = Some(reason);
    }

    pub fn pairing_tries_left(&self) -> i64 {
        self.shared.pairing.lock().unwrap().tries_left
    }

    /// Keys the host paired (a proof that checked).
    pub fn paired_devices(&self) -> Vec<Fingerprint> {
        self.shared.paired.lock().unwrap().clone()
    }
}

async fn serve_pairing(mut tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>, sh: Arc<Shared>, p: MockPairing, mac_fp: Fingerprint) {
    let Some(dev_fp) = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).and_then(|c| pin::fingerprint_of_cert(c)) else { return };
    let mut head = [0u8; HEADER_LENGTH];
    if tls.read_exact(&mut head).await.is_err() {
        return;
    }
    let Some(h) = Header::parse(&head) else { return };
    if h.payload_length > MAX_PAIRING_PAYLOAD {
        return;
    }
    let mut body = vec![0u8; h.payload_length];
    if tls.read_exact(&mut body).await.is_err() {
        return;
    }
    sh.received.lock().unwrap().push(Message::new(h.kind, h.timestamp, h.is_keyframe, body.clone()));
    sh.changed.notify_waiters();
    let refuse = |reason: &str| PairResult { ok: false, reason: Some(reason.into()), ..Default::default() };
    let result = match (h.kind, decode_json::<PairRequest>(&body)) {
        (Kind::PairRequest, Some(req)) => judge(&sh, &p, &req, &dev_fp, &mac_fp),
        _ => refuse("closed"),
    };
    if p.fault == PairFault::Silent {
        tokio::time::sleep(Duration::from_secs(30)).await;
        return;
    }
    let reply = Message::new(Kind::PairResult, 0.0, false, encode_json(&result)).serialize();
    let _ = tls.write_all(&reply).await;
    let _ = tls.shutdown().await;
}

fn judge(sh: &Shared, p: &MockPairing, req: &PairRequest, dev_fp: &Fingerprint, mac_fp: &Fingerprint) -> PairResult {
    let refuse = |reason: &str| PairResult { ok: false, reason: Some(reason.into()), ..Default::default() };
    if let PairFault::Reply(r) = &p.fault {
        return (**r).clone();
    }
    let mut st = sh.pairing.lock().unwrap();
    if req.v != PairRequest::VERSION || !matches!(req.method.as_str(), "qr" | "code") {
        return refuse("closed");
    }
    if let Some(reason) = st.closed {
        return refuse(reason);
    }
    let key: Vec<u8> = match req.method.as_str() {
        "qr" => p.secret.to_vec(),
        _ => pairing::code_key(&p.code, mac_fp, p.rounds).unwrap().to_vec(),
    };
    let proof = b64::url_decode(&req.proof).unwrap_or_default();
    if !pairing::is_valid_device_proof(&proof, &key, dev_fp, mac_fp) {
        st.tries_left -= 1;
        let left = st.tries_left.max(0);
        if st.tries_left <= 0 {
            st.closed = Some("stopped");
        }
        return PairResult { ok: false, reason: Some("code".into()), tries_left: Some(left), ..Default::default() };
    }
    let mut proof_m = match p.fault {
        PairFault::WrongKey => pairing::mac_proof(b"not the key", mac_fp, dev_fp),
        _ => pairing::mac_proof(&key, mac_fp, dev_fp),
    };
    if p.fault == PairFault::TamperProofM {
        proof_m[0] ^= 1;
    }
    st.closed = Some("closed"); // single use
    sh.paired.lock().unwrap().push(*dev_fp);
    PairResult {
        ok: true,
        proof: Some(b64::url_encode(&proof_m)),
        mac_id: Some(pairing::mac_id(mac_fp)),
        name: Some(p.name.clone()),
        recognition_key: Some(b64::url_encode(&p.recognition_key)),
        ..Default::default()
    }
}

/// The clip, forever: parameter sets, then a frame every `1/fps`. A keyframe request starts it over
/// from its first frame (a keyframe) with the parameter sets again, as the host's encoder answers one.
async fn stream_video(v: MockVideo, tx: mpsc::UnboundedSender<Vec<u8>>, restart: Arc<Notify>, sent: Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::Ordering;
    if let Some(c) = &v.cursor_payload {
        let _ = tx.send(Message::new(Kind::CursorShape, 0.0, false, c.clone()).serialize());
    }
    let mut next = 0usize;
    let mut first = true;
    let period = Duration::from_secs_f64(1.0 / v.fps.max(1.0));
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = restart.notified() => { next = 0; first = true; }
        }
        if next >= v.frames.len() {
            next = 0;
            first = true; // looping starts on a keyframe: tell the decoder its sets again
        }
        if first {
            let _ = tx.send(Message::new(Kind::ParameterSets, 0.0, false, v.params.encode()).serialize());
            first = false;
        }
        let (key, payload) = &v.frames[next];
        let n = sent.fetch_add(1, Ordering::SeqCst);
        let mut payload = payload.clone();
        if v.corrupt_frame == Some(n) {
            payload.truncate(payload.len() / 2);
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
        if tx.send(Message::new(Kind::Frame, now, *key, payload).serialize()).is_err() {
            return;
        }
        if v.cursor_payload.is_some() {
            let a = n as f64 / 90.0 * std::f64::consts::TAU;
            let p = MacPointer { x: Some(0.5 + 0.3 * a.cos()), y: Some(0.5 + 0.3 * a.sin()), inside: Some(true), seen: Some(0) };
            let _ = tx.send(Message::new(Kind::MacPointer, 0.0, false, encode_json(&p)).serialize());
        }
        next += 1;
    }
}

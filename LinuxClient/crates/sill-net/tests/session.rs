//! The session against `sill-mockhost`, on loopback.

use sill_mockhost::{MockConfig, MockHost};
use sill_net::session::ConnectError;
use sill_net::*;
use sill_proto::*;
use std::time::Duration;

const T: Duration = Duration::from_secs(5);

fn opts(_host: &MockHost, id: &Identity, pin: PinMode) -> ConnectOptions {
    ConnectOptions {
        identity: id.clone(),
        pin,
        device: "Test Linux".into(),
        app_version: "0.1.0".into(),
        build: "1".into(),
        config: SessionConfig { min_silence: Duration::from_millis(700), ..Default::default() },
    }
}

async fn open(host: &MockHost) -> (Session, Identity) {
    let id = Identity::generate_in_memory().unwrap();
    let s = connect(("127.0.0.1", host.port()), opts(host, &id, PinMode::Pinned(host.fingerprint))).await.unwrap();
    (s, id)
}

async fn next(s: &mut Session) -> Event {
    tokio::time::timeout(T, s.events.recv()).await.expect("event in time").expect("channel open")
}

async fn until(s: &mut Session, pred: impl Fn(&Event) -> bool) -> Event {
    loop {
        let e = next(s).await;
        if pred(&e) {
            return e;
        }
    }
}

#[tokio::test]
async fn handshake_with_pin_hello_first_then_opening() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let (mut s, id) = open(&host).await;
    assert_eq!(s.mac_fingerprint, host.fingerprint);
    let mut seen = vec![];
    let mut frames = 0;
    while frames < 3 {
        match next(&mut s).await {
            Event::Frame { is_keyframe, .. } => {
                frames += 1;
                seen.push(if is_keyframe { "key" } else { "delta" });
            }
            Event::WindowList(l) => {
                assert!(l.host_repeats_keys());
                seen.push("list");
            }
            Event::HostSettings(_) => seen.push("settings"),
            Event::MacInfo(_) => seen.push("macinfo"),
            Event::ParameterSets(p) => {
                assert_eq!(p.sets.len(), 3);
                seen.push("ps")
            }
            _ => {}
        }
    }
    assert_eq!(seen, ["list", "settings", "macinfo", "ps", "key", "delta", "delta"]);
    let got = host.received();
    assert_eq!(got[0].kind, Kind::Hello, "Hello is the first message");
    let h: Hello = decode_json(&got[0].payload).unwrap();
    assert_eq!(h.protocol, Some(1));
    assert_eq!(h.device.as_deref(), Some("Test Linux"));
    assert_eq!(h.audio, None);
    assert_eq!(host.client_fingerprints(), vec![id.fingerprint]);
}

#[tokio::test]
async fn wrong_pin_is_refused_before_any_application_data() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let id = Identity::generate_in_memory().unwrap();
    let r = connect(("127.0.0.1", host.port()), opts(&host, &id, PinMode::Pinned([9; 32]))).await;
    match r {
        Err(ConnectError::PinMismatch { expected, got }) => {
            assert_eq!(expected, [9; 32]);
            assert_eq!(got, host.fingerprint);
        }
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("connected with a wrong pin"),
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(host.received().is_empty());
}

#[tokio::test]
async fn trust_on_first_use_reports_the_key() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let id = Identity::generate_in_memory().unwrap();
    let s = connect(("127.0.0.1", host.port()), opts(&host, &id, PinMode::TrustOnFirstUse)).await.unwrap();
    assert_eq!(s.mac_fingerprint, host.fingerprint);
}

#[tokio::test]
async fn wrong_alpn_is_refused() {
    let host = MockHost::start(MockConfig { alpns: vec![b"sill-pair/1".to_vec()], ..Default::default() }).await.unwrap();
    let id = Identity::generate_in_memory().unwrap();
    assert!(connect(("127.0.0.1", host.port()), opts(&host, &id, PinMode::TrustOnFirstUse)).await.is_err());
}

#[tokio::test]
async fn ping_pong_gives_rtt() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let (mut s, _) = open(&host).await;
    match until(&mut s, |e| matches!(e, Event::Pong(_))).await {
        Event::Pong(rtt) => assert!((0.0..1.0).contains(&rtt), "rtt {rtt}"),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn stats_once_a_second() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let (_s, _) = open(&host).await;
    assert!(host.wait_for(Duration::from_secs(3), |m| m.iter().any(|x| x.kind == Kind::ClientStats)).await);
    let m = host.received().into_iter().find(|x| x.kind == Kind::ClientStats).unwrap();
    let st: ClientStats = decode_json(&m.payload).unwrap();
    assert_eq!(st.device, "Test Linux");
    assert!(st.rtt_ms >= 0 && st.frame_age_ms >= -1);
}

#[tokio::test]
async fn silent_host_trips_liveness() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let (mut s, _) = open(&host).await;
    until(&mut s, |e| matches!(e, Event::Pong(_))).await;
    host.go_silent();
    match until(&mut s, |e| matches!(e, Event::Closed(_))).await {
        Event::Closed(r) => {
            assert_eq!(r, CloseReason::Silent);
            assert!(r.reconnects());
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn goodbye_is_surfaced_with_reconnect_decision() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let (mut s, _) = open(&host).await;
    until(&mut s, |e| matches!(e, Event::Frame { .. })).await;
    let g = Goodbye { reason: "update".into(), message: Some("Update Sill".into()), minimum_version: Some("1.2".into()), reconnect: None };
    assert!(host.send_goodbye(&g));
    let Event::Goodbye(o) = until(&mut s, |e| matches!(e, Event::Goodbye(_))).await else { unreachable!() };
    assert!(o.is_notice && !o.reconnect);
    assert_eq!(o.message.as_deref(), Some("Update Sill"));
    let Event::Closed(r) = next(&mut s).await else { panic!("closed expected") };
    assert!(!r.reconnects());
}

#[tokio::test]
async fn quit_goodbye_reconnects() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let (mut s, _) = open(&host).await;
    until(&mut s, |e| matches!(e, Event::Frame { .. })).await;
    host.send_goodbye(&Goodbye { reason: "quit".into(), message: None, minimum_version: None, reconnect: None });
    let Event::Closed(r) = until(&mut s, |e| matches!(e, Event::Closed(_))).await else { unreachable!() };
    assert!(r.reconnects());
}

#[tokio::test]
async fn unknown_kinds_and_bad_json_are_skipped() {
    let host = MockHost::start(MockConfig { frames: 0, ..Default::default() }).await.unwrap();
    let (mut s, _) = open(&host).await;
    until(&mut s, |e| matches!(e, Event::ParameterSets(_))).await;
    // Kind 200 with 3 bytes, then a window list that is not JSON, then a good Thumbnail.
    let mut raw = Message::new(Kind::Tick, 0.0, false, vec![1, 2, 3]).serialize();
    raw[0] = 200;
    host.send_raw(raw);
    host.send(&Message::new(Kind::WindowList, 0.0, false, b"{nope".to_vec()));
    host.send(&Message::new(Kind::Thumbnail, 0.0, false, encode_thumbnail(7, b"jpg")));
    assert!(matches!(until(&mut s, |e| matches!(e, Event::Unknown(Kind::Unknown))).await, Event::Unknown(_)));
    assert!(matches!(next(&mut s).await, Event::Unknown(Kind::WindowList)));
    match next(&mut s).await {
        Event::Thumbnail { window_id, jpeg } => assert_eq!((window_id, jpeg.as_slice()), (7, &b"jpg"[..])),
        e => panic!("{e:?}"),
    }
}

#[tokio::test]
async fn oversized_header_closes_the_session() {
    let host = MockHost::start(MockConfig { frames: 0, ..Default::default() }).await.unwrap();
    let (mut s, _) = open(&host).await;
    until(&mut s, |e| matches!(e, Event::ParameterSets(_))).await;
    let mut h = Message::new(Kind::MacInfo, 0.0, false, vec![]).serialize();
    h[10..14].copy_from_slice(&(5u32 << 20).to_be_bytes()); // 5 MiB of a 4 MiB cap, no body follows
    host.send_raw(h);
    let Event::Closed(r) = until(&mut s, |e| matches!(e, Event::Closed(_))).await else { unreachable!() };
    assert!(matches!(r, CloseReason::Protocol(_)), "{r:?}");
    assert!(!r.reconnects());
}

#[tokio::test]
async fn keyframe_requests_are_rate_limited() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let (s, _) = open(&host).await;
    let sent: Vec<bool> = (0..3).map(|_| s.handle.send(Command::RequestKeyframe)).collect();
    assert_eq!(sent, [true, false, false]);
    assert!(host.wait_for(T, |m| m.iter().any(|x| x.kind == Kind::KeyframeRequest)).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(host.received_kinds().iter().filter(|k| **k == Kind::KeyframeRequest).count(), 1);
}

#[tokio::test]
async fn commands_arrive_as_sent() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let (s, _) = open(&host).await;
    let ev = InputEvent::Key { hid_usage: 4, down: true, modifiers: modifiers::COMMAND };
    assert!(s.handle.send(Command::Input(ev.clone())));
    assert!(s.handle.send(Command::SelectSource(StreamSource::Window(7))));
    // A viewport is held to the host's bounds on the way out; one with no size is not sent.
    assert!(s.handle.send(Command::Viewport(Viewport { width: 1e9, height: 600.0, scale: None, local_cursor: None, fps: Some(60) })));
    assert!(!s.handle.send(Command::Viewport(Viewport { width: 0.0, height: 600.0, scale: None, local_cursor: None, fps: None })));
    assert!(host.wait_for(T, |m| m.iter().any(|x| x.kind == Kind::Viewport)).await);
    let got = host.received();
    let input = got.iter().find(|m| m.kind == Kind::Input).unwrap();
    assert_eq!(decode_json::<InputEvent>(&input.payload), Some(ev));
    let sel = got.iter().find(|m| m.kind == Kind::SelectSource).unwrap();
    assert_eq!(decode_json::<StreamSource>(&sel.payload), Some(StreamSource::Window(7)));
    let vp = got.iter().find(|m| m.kind == Kind::Viewport).unwrap();
    assert_eq!(decode_json::<Viewport>(&vp.payload).unwrap().width, 16384.0);
}

#[tokio::test]
async fn local_close_ends_with_local() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let (mut s, _) = open(&host).await;
    s.handle.close();
    let Event::Closed(r) = until(&mut s, |e| matches!(e, Event::Closed(_))).await else { unreachable!() };
    assert_eq!(r, CloseReason::Local);
}

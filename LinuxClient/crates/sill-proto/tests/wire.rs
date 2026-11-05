use serde_json::json;
use sill_proto::*;

fn v(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap()
}

#[test]
fn header_layout_is_14_bytes_big_endian() {
    let m = Message::new(Kind::Frame, 1.5, true, vec![9, 8, 7]);
    let b = m.serialize();
    assert_eq!(b.len(), 17);
    assert_eq!(b[0], 1);
    assert_eq!(&b[1..9], &1.5f64.to_bits().to_be_bytes());
    assert_eq!(b[9], 1);
    assert_eq!(&b[10..14], &[0, 0, 0, 3]);
    assert_eq!(&b[14..], &[9, 8, 7]);
    let h = Header::parse(&b).unwrap();
    assert_eq!((h.kind, h.timestamp, h.is_keyframe, h.payload_length), (Kind::Frame, 1.5, true, 3));
}

#[test]
fn every_known_kind_round_trips_its_number() {
    for raw in 0u8..=255 {
        let k = Kind::from_u8(raw);
        match k.as_u8() {
            Some(n) => assert_eq!(n, raw),
            None => assert_eq!(k, Kind::Unknown),
        }
    }
    // 29 to 31 and 33 to 42 are free (pull request #66's), and are unknown here.
    for raw in [29u8, 30, 31, 33, 40, 42, 45, 255] {
        assert_eq!(Kind::from_u8(raw), Kind::Unknown, "{raw}");
    }
    assert_eq!(Kind::from_u8(43), Kind::Audio);
    assert_eq!(Kind::from_u8(44), Kind::KeyframeRequest);
}

#[test]
fn header_needs_14_bytes() {
    assert!(Header::parse(&[0; 13]).is_none());
    assert!(Header::parse(&[0; 14]).is_some());
}

#[test]
fn decoder_reassembles_split_and_coalesced_input() {
    let a = Message::new(Kind::Tick, 2.0, false, vec![]).serialize();
    let b = Message::new(Kind::Pong, 3.0, false, vec![1, 2, 3, 4, 5, 6, 7, 8]).serialize();
    let mut all = a.clone();
    all.extend_from_slice(&b);
    for chunk in [1usize, 3, 14, 100] {
        let mut d = Decoder::new(Direction::HostToClient);
        let mut got = Vec::new();
        for piece in all.chunks(chunk) {
            d.push(piece);
            while let Some(m) = d.next_message().unwrap() {
                got.push(m);
            }
        }
        assert_eq!(got.len(), 2, "chunk {chunk}");
        assert_eq!(got[0].kind, Kind::Tick);
        assert_eq!(got[1].payload, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }
}

#[test]
fn decoder_returns_unknown_kinds_without_failing() {
    let mut bytes = Message::new(Kind::Tick, 0.0, false, vec![1, 2]).serialize();
    bytes[0] = 99;
    let mut d = Decoder::new(Direction::HostToClient);
    d.push(&bytes);
    let m = d.next_message().unwrap().unwrap();
    assert_eq!(m.kind, Kind::Unknown);
    assert_eq!(m.payload, vec![1, 2]);
    d.push(&Message::new(Kind::Tick, 0.0, false, vec![]).serialize());
    assert_eq!(d.next_message().unwrap().unwrap().kind, Kind::Tick);
}

fn header_announcing(kind: u8, len: u32) -> Vec<u8> {
    let mut h = vec![kind];
    h.extend_from_slice(&0f64.to_bits().to_be_bytes());
    h.push(0);
    h.extend_from_slice(&len.to_be_bytes());
    h
}

#[test]
fn decoder_enforces_caps_before_waiting_for_the_bytes() {
    // A frame may be 32 MiB, anything else from the host 4 MiB, a client message 1 MiB.
    let mut d = Decoder::new(Direction::HostToClient);
    d.push(&header_announcing(1, MAX_FRAME_PAYLOAD as u32));
    assert!(d.next_message().unwrap().is_none());

    let mut d = Decoder::new(Direction::HostToClient);
    d.push(&header_announcing(1, MAX_FRAME_PAYLOAD as u32 + 1));
    assert!(matches!(d.next_message(), Err(DecodeError::TooBig { kind: Kind::Frame, .. })));
    assert!(d.next_message().is_err(), "an error is final");

    let mut d = Decoder::new(Direction::HostToClient);
    d.push(&header_announcing(2, MAX_OTHER_HOST_PAYLOAD as u32 + 1));
    assert!(d.next_message().is_err());

    // An SSH banner parses as a 1.7 GB payload; it must close, not wait.
    let mut d = Decoder::new(Direction::HostToClient);
    d.push(b"SSH-2.0-OpenSSH_9.6\r\n");
    assert!(d.next_message().is_err());

    let mut d = Decoder::new(Direction::ClientToHost);
    d.push(&header_announcing(8, MAX_CLIENT_PAYLOAD as u32 + 1));
    assert!(d.next_message().is_err());
}

#[test]
fn parameter_sets_round_trip_and_reject_truncation() {
    let ps = ParameterSets { nal_unit_header_length: 4, sets: vec![vec![0x40, 1, 2], vec![0x42], vec![0x44, 9, 9, 9]] };
    let bytes = ps.encode();
    assert_eq!(&bytes[..2], &[4, 3]);
    assert_eq!(&bytes[2..5], &[0, 3, 0x40]);
    assert_eq!(ParameterSets::decode(&bytes), Some(ps));
    assert_eq!(ParameterSets::decode(&bytes[..bytes.len() - 1]), None);
    assert_eq!(ParameterSets::decode(&[4]), None);
}

#[test]
fn stream_source_uses_swifts_enum_shape() {
    assert_eq!(v(&encode_json(&StreamSource::Desktop)), json!({"desktop": {}}));
    assert_eq!(v(&encode_json(&StreamSource::None)), json!({"none": {}}));
    assert_eq!(v(&encode_json(&StreamSource::Window(42))), json!({"window": {"_0": 42}}));
    assert_eq!(decode_json::<StreamSource>(br#"{"window":{"_0":7}}"#), Some(StreamSource::Window(7)));
    assert_eq!(decode_json::<StreamSource>(br#"{"desktop":{}}"#), Some(StreamSource::Desktop));
    assert_eq!(decode_json::<StreamSource>(br#"{}"#), None);
    assert_eq!(decode_json::<StreamSource>(br#"{"desktop":{},"none":{}}"#), None);
    assert_eq!(decode_json::<StreamSource>(br#"{"tablet":{}}"#), None);
}

#[test]
fn input_events_use_the_shapes_sillclient_sends() {
    let e = InputEvent::Pointer { action: PointerAction::LeftDown, x: 0.25, y: 0.75 };
    assert_eq!(v(&encode_json(&e)), json!({"pointer": {"_0": "leftDown", "x": 0.25, "y": 0.75}}));
    let e = InputEvent::Key { hid_usage: 44, down: true, modifiers: modifiers::COMMAND };
    assert_eq!(v(&encode_json(&e)), json!({"key": {"hidUsage": 44, "down": true, "modifiers": 1048576}}));
    let e = InputEvent::ScrollGesture { phase: ScrollPhase::MomentumBegan, x: 0.5, y: 0.5 };
    assert_eq!(v(&encode_json(&e)), json!({"scrollGesture": {"_0": "momentumBegan", "x": 0.5, "y": 0.5}}));
    let e = InputEvent::Scroll { x: 0.1, y: 0.2, dx: 0.0, dy: -0.01 };
    assert_eq!(v(&encode_json(&e)), json!({"scroll": {"x": 0.1, "y": 0.2, "dx": 0.0, "dy": -0.01}}));
    let e = InputEvent::Text("héllo".into());
    assert_eq!(v(&encode_json(&e)), json!({"text": {"_0": "héllo"}}));
    for e in [
        InputEvent::Pointer { action: PointerAction::RightUp, x: 0.0, y: 1.0 },
        InputEvent::Text("a\n".into()),
        InputEvent::Key { hid_usage: 4, down: false, modifiers: 0 },
    ] {
        assert_eq!(decode_json::<InputEvent>(&encode_json(&e)), Some(e));
    }
    assert_eq!(decode_json::<InputEvent>(br#"{"pointer":{"_0":"teleport","x":0,"y":0}}"#), None);
}

#[test]
fn modifier_bits_match_cgeventflags() {
    assert_eq!(modifiers::SHIFT, 0x20000);
    assert_eq!(modifiers::CONTROL, 0x40000);
    assert_eq!(modifiers::OPTION, 0x80000);
    assert_eq!(modifiers::COMMAND, 0x100000);
}

#[test]
fn window_list_reads_old_and_new_hosts() {
    let old = br#"{"macName":"Mac","windows":[{"id":5,"title":"t","appName":"A","bundleID":"b.c","width":800,"height":600}],"active":{"desktop":{}}}"#;
    let l: WindowList = decode_json(old).unwrap();
    assert_eq!(l.active, StreamSource::Desktop);
    assert_eq!(l.windows[0].bundle_id, "b.c");
    assert!(!l.host_repeats_keys());
    let new = br#"{"macName":"Mac","windows":[],"active":{"window":{"_0":5}},"launchID":"x","hostVersion":"0.6.0","protocol":1,"gestures":1,"clipboard":1,"keyRepeat":1,"locked":true,"offConsole":true,"someFutureField":[1,2]}"#;
    let l: WindowList = decode_json(new).unwrap();
    assert!(l.host_repeats_keys());
    assert_eq!(l.active, StreamSource::Window(5));
    assert_eq!((l.locked, l.off_console, l.protocol), (Some(true), Some(true), Some(1)));
    // Nil optionals are left out, as Swift's encoder leaves them.
    assert_eq!(v(&encode_json(&decode_json::<WindowList>(old).unwrap())).as_object().unwrap().len(), 3);
}

#[test]
fn hello_leaves_audio_out() {
    let h = Hello { app_version: Some("0.1.0".into()), build: Some("1".into()), protocol: Some(PROTOCOL), device: Some("Linux".into()), audio: None };
    assert_eq!(v(&encode_json(&h)), json!({"appVersion": "0.1.0", "build": "1", "protocol": 1, "device": "Linux"}));
    assert_eq!(decode_json::<Hello>(b"{}"), Some(Hello::default()));
}

#[test]
fn viewport_bounds() {
    let ok = Viewport { width: 1280.0, height: 800.0, scale: Some(1.5), local_cursor: None, fps: Some(60) };
    assert_eq!(ViewportBounds::refusal(&ok), None);
    let bad = Viewport { width: 1e300, height: 0.5, scale: Some(1e-300), local_cursor: None, fps: None };
    assert_eq!(
        ViewportBounds::refusal(&bad).unwrap(),
        "width 1e+300 (1 to 16384 points), height 0.5 (1 to 16384 points), scale 1e-300 (0.25 to 4)"
    );
    let nan = Viewport { width: f64::NAN, height: 10.0, scale: None, local_cursor: None, fps: None };
    assert_eq!(ViewportBounds::refusal(&nan).unwrap(), "width nan (1 to 16384 points)");
    let neg = Viewport { width: -1.0, height: 16385.0, scale: None, local_cursor: None, fps: None };
    assert_eq!(ViewportBounds::refusal(&neg).unwrap(), "width -1 (1 to 16384 points), height 16385 (1 to 16384 points)");
    for (w, h) in [(0.0, 10.0), (-1.0, 10.0), (f64::NAN, 10.0), (10.0, f64::INFINITY)] {
        assert_eq!(ViewportBounds::held(&Viewport { width: w, height: h, scale: None, local_cursor: None, fps: None }), None);
    }
    let held = ViewportBounds::held(&Viewport { width: 99999.0, height: 0.2, scale: Some(9.0), local_cursor: None, fps: Some(120) }).unwrap();
    assert_eq!((held.width, held.height, held.scale, held.fps), (16384.0, 1.0, Some(4.0), Some(120)));
    let held = ViewportBounds::held(&Viewport { width: 10.0, height: 10.0, scale: Some(-1.0), local_cursor: None, fps: None }).unwrap();
    assert_eq!(held.scale, None);
    assert_eq!(ViewportBounds::refusal(&held), None);
}

#[test]
fn blobs() {
    let t = encode_thumbnail(0x01020304, b"jpg");
    assert_eq!(&t[..4], &[1, 2, 3, 4]);
    assert_eq!(decode_thumbnail(&t), Some((0x01020304, &b"jpg"[..])));
    assert_eq!(decode_thumbnail(&[0, 0, 0, 1]), None);
    let i = encode_icon("com.apple.Safari", b"png");
    assert_eq!(decode_icon(&i), Some(("com.apple.Safari", &b"png"[..])));
    assert_eq!(decode_icon(&[0, 5, b'a']), None);
    let c = CursorShape { hotspot: (1.0, 2.0), size: (16.0, 24.0), png: vec![1, 2, 3] };
    assert_eq!(CursorShape::decode(&c.encode()), Some(c));
    assert_eq!(CursorShape::decode(&[0; 16]), None);
}

#[test]
fn mac_pointer_position() {
    let p: MacPointer = decode_json(br#"{"inside":true,"seen":12,"x":0.4213,"y":0.1873}"#).unwrap();
    assert_eq!(p.position(), Some((0.4213, 0.1873)));
    assert_eq!(p.seen, Some(12));
    let p: MacPointer = decode_json(br#"{"inside":false,"seen":12}"#).unwrap();
    assert_eq!(p.position(), None);
    assert_eq!(decode_json::<MacPointer>(b"{}").unwrap().position(), None);
}

#[test]
fn settings_state_and_change() {
    let state = br#"{"settings":{"maxFPS":60,"bitrate":40000000,"captureScale":2,"prioritizeSpeed":false,"virtualDisplay":true,"directWireless":false},
        "persistent":true,"virtualDisplayAvailable":true,"softwareEncoder":false,
        "stream":{"width":3024,"height":1964,"fps":60,"mbps":40,"onVirtualDisplay":false},
        "answering":3,"sessionPaired":true,"future":{}}"#;
    let s: HostSettingsState = decode_json(state).unwrap();
    assert_eq!(s.settings.bitrate, 40_000_000);
    assert_eq!(s.answering, Some(3));
    assert_eq!(s.stream.as_ref().unwrap().width, 3024);
    let c = HostSettingsChange { token: Some(1), bitrate: Some(15_000_000), ..Default::default() };
    assert_eq!(v(&encode_json(&c)), json!({"token": 1, "bitrate": 15000000}));
    assert_eq!(QUALITY_PRESETS[4], ("Pro", 40_000_000));
}

#[test]
fn pairing_payloads() {
    let r = PairRequest { v: PairRequest::VERSION, method: PairRequest::CODE.into(), proof: "abc".into(), name: "Linux".into(), model: None, cable: None };
    assert_eq!(v(&encode_json(&r)), json!({"v": 1, "method": "code", "proof": "abc", "name": "Linux"}));
    let res: PairResult = decode_json(br#"{"ok":false,"reason":"code","triesLeft":4}"#).unwrap();
    assert_eq!(res.tries_left, Some(4));
    assert_eq!(res.unknown_reason_message(), None);
    let res: PairResult = decode_json(br#"{"ok":false,"reason":"newthing","message":"Try later"}"#).unwrap();
    assert_eq!(res.unknown_reason_message(), Some("Try later"));
    let ok: PairResult = decode_json(br#"{"ok":true,"proof":"p","macID":"m","name":"Mac","recognitionKey":"k","method":"cable"}"#).unwrap();
    assert!(ok.ok && ok.recognition_key.as_deref() == Some("k"));
}

#[test]
fn goodbye_and_mac_info() {
    let g: Goodbye = decode_json(br#"{"reason":"update","message":"Update Sill","minimumVersion":"1.2","reconnect":false}"#).unwrap();
    assert_eq!(g.reconnect, Some(false));
    assert_eq!(decode_json::<Goodbye>(br#"{"reason":"quit"}"#).unwrap().reconnect, None);
    let info = br#"{"v":1,"macID":"ABCD","name":"Mac","issuedAt":1.7e9,"remoteAccess":true,"remotePort":7455,"internet":false,"addresses":[{"host":"100.1.2.3","port":7455,"kind":"vpn","via":"tailscale"}]}"#;
    let m: MacInfo = decode_json(info).unwrap();
    assert_eq!(m.addresses[0].kind, "vpn");
    assert_eq!(m.remote_port, 7455);
}

#[test]
fn client_stats_keys() {
    let s = ClientStats { fps: 60, frame_age_ms: 9, rtt_ms: 7, device: "Linux".into(), frame_age_max_ms: Some(24), rtt_max_ms: None, audio_behind_ms: None, audio_late: None };
    assert_eq!(v(&encode_json(&s)), json!({"fps": 60, "frameAgeMs": 9, "rttMs": 7, "device": "Linux", "frameAgeMaxMs": 24}));
}

//! Pairing, saved Macs, kind 18 and goodbyes against `sill-mockhost`, end to end on loopback.

use sill_mockhost::{MockConfig, MockHost, MockPairing, PairFault};
use sill_net::pairing::{self, CodeProblem, PairError, PairLink, PairOptions};
use sill_net::*;
use sill_proto::*;
use std::time::Duration;

const CODE: &str = "482913557208";

async fn host_with(f: impl FnOnce(&mut MockPairing)) -> MockHost {
    let mut p = MockPairing::new(CODE);
    f(&mut p);
    MockHost::start(MockConfig { pairing: Some(p), ..Default::default() }).await.unwrap()
}

fn opts() -> PairOptions {
    let mut o = PairOptions::new(Identity::generate_in_memory().unwrap(), "Test Linux");
    o.model = Some("linux-test".into());
    o
}

fn pair_kinds(h: &MockHost) -> usize {
    h.received_kinds().iter().filter(|k| **k == Kind::PairRequest).count()
}

#[tokio::test]
async fn pairs_by_typed_code() {
    let h = host_with(|_| {}).await;
    let o = opts();
    let mac = pairing::pair_with_code("4829 1355 7208", "127.0.0.1", h.port(), None, &o).await.unwrap();
    assert_eq!(mac.fingerprint, h.fingerprint); // learned from the TLS session
    assert_eq!(mac.mac_id, pairing::mac_id(&h.fingerprint));
    assert_eq!((mac.name.as_str(), mac.recognition_key, mac.method.as_str()), ("Mock Mac", [0x33; 32], "code"));
    assert_eq!(mac.addresses[0].host, "127.0.0.1");
    assert_eq!(h.received_kinds(), vec![Kind::PairRequest], "exactly one kind 19");
    let req: PairRequest = decode_json(&h.received()[0].payload).unwrap();
    assert_eq!((req.v, req.method.as_str(), req.name.as_str(), req.model.as_deref()), (1, "code", "Test Linux", Some("linux-test")));
    assert_eq!(h.paired_devices(), vec![o.identity.fingerprint]);
}

#[tokio::test]
async fn pairs_by_link_then_session_uses_the_pin() {
    let h = host_with(|_| {}).await;
    let o = opts();
    let link = PairLink::parse(&h.pair_link().url()).unwrap();
    let mac = pairing::pair_with_link(&link, &o).await.unwrap();
    assert_eq!((mac.method.as_str(), mac.fingerprint, mac.remote_port), ("qr", h.fingerprint, h.port()));
    assert_eq!(pair_kinds(&h), 1);
    // The saved record is what a session pins to.
    let s = connect(("127.0.0.1", h.port()), session_opts(&o.identity, PinMode::Pinned(mac.fingerprint))).await.unwrap();
    assert_eq!(s.mac_fingerprint, mac.fingerprint);
    // The window is single use.
    let again = pairing::pair_with_link(&link, &opts()).await;
    assert_eq!(again, Err(PairError::NotPairing));
}

#[tokio::test]
async fn real_rounds_and_lowered_rounds_agree() {
    let h = host_with(|p| p.rounds = 1000).await;
    let mut o = opts();
    o.rounds = 1000;
    assert!(pairing::pair_with_code(CODE, "127.0.0.1", h.port(), None, &o).await.is_ok());
}

#[tokio::test]
async fn wrong_code_counts_tries_and_stops() {
    let h = host_with(|p| p.max_tries = 3).await;
    let o = opts();
    for left in [2, 1] {
        let e = pairing::pair_with_code("123456789018", "127.0.0.1", h.port(), None, &o).await.unwrap_err();
        assert_eq!(e, PairError::WrongCode { tries_left: left });
    }
    let e = pairing::pair_with_code("123456789018", "127.0.0.1", h.port(), None, &o).await.unwrap_err();
    assert_eq!(e, PairError::WrongCode { tries_left: 0 });
    // Now the window is stopped, even for the right code.
    let e = pairing::pair_with_code(CODE, "127.0.0.1", h.port(), None, &o).await.unwrap_err();
    assert_eq!(e, PairError::Stopped);
    assert!(h.paired_devices().is_empty());
}

#[tokio::test]
async fn typo_and_length_never_reach_the_mac() {
    let h = host_with(|_| {}).await;
    let o = opts();
    assert_eq!(pairing::pair_with_code("482913557209", "127.0.0.1", h.port(), None, &o).await, Err(PairError::Code(CodeProblem::Typo)));
    assert_eq!(pairing::pair_with_code("4829", "127.0.0.1", h.port(), None, &o).await, Err(PairError::Code(CodeProblem::Length)));
    assert_eq!(pair_kinds(&h), 0);
    assert!(h.client_fingerprints().is_empty(), "not even a connection");
}

#[tokio::test]
async fn link_to_the_wrong_mac_sends_nothing() {
    let h = host_with(|_| {}).await;
    let mut link = h.pair_link();
    let other = Identity::generate_in_memory().unwrap();
    link = PairLink::new(other.fingerprint, link.secret, &link.name, link.port, link.addresses.clone());
    let e = pairing::pair_with_link(&link, &opts()).await.unwrap_err();
    assert!(matches!(e, PairError::WrongMac(_)), "{e:?}");
    assert_eq!(pair_kinds(&h), 0, "the handshake failed before kind 19");
    assert!(h.paired_devices().is_empty());
}

#[tokio::test]
async fn second_address_is_tried_when_the_first_is_dead() {
    let h = host_with(|_| {}).await;
    let mut link = h.pair_link();
    link.addresses.insert(0, sill_net::addr::parse("127.0.0.1:1").unwrap());
    let mac = pairing::pair_with_link(&link, &opts()).await.unwrap();
    assert_eq!(mac.addresses.len(), 2);
    // Nothing at all answers:
    let mut dead = h.pair_link();
    dead.addresses = vec![sill_net::addr::parse("127.0.0.1:1").unwrap()];
    assert!(matches!(pairing::pair_with_link(&dead, &opts()).await, Err(PairError::NothingAnswered(_))));
    dead.addresses.clear();
    assert_eq!(pairing::pair_with_link(&dead, &opts()).await, Err(PairError::NoAddress));
}

#[tokio::test]
async fn a_mac_that_cannot_prove_it_is_not_saved() {
    for fault in [PairFault::TamperProofM, PairFault::WrongKey] {
        let h = host_with(|p| p.fault = fault.clone()).await;
        let by_code = pairing::pair_with_code(CODE, "127.0.0.1", h.port(), None, &opts()).await;
        assert_eq!(by_code, Err(PairError::ProofFailed), "{fault:?}");
        let by_link = pairing::pair_with_link(&h.pair_link(), &opts()).await;
        // The first attempt used up the single-use window on the mock; either way nothing is saved.
        assert!(by_link.is_err(), "{fault:?}");
    }
}

#[tokio::test]
async fn pinned_code_pairing_refuses_another_key() {
    let h = host_with(|_| {}).await;
    let e = pairing::pair_with_code(CODE, "127.0.0.1", h.port(), Some([9; 32]), &opts()).await.unwrap_err();
    assert!(matches!(e, PairError::WrongMac(_)));
    assert_eq!(pair_kinds(&h), 0);
}

#[tokio::test]
async fn silence_times_out() {
    let h = host_with(|p| p.fault = PairFault::Silent).await;
    let mut o = opts();
    o.timeout = Duration::from_millis(500);
    let t = std::time::Instant::now();
    assert_eq!(pairing::pair_with_code(CODE, "127.0.0.1", h.port(), None, &o).await, Err(PairError::Timeout));
    assert!(t.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn every_refusal_reason_has_its_error() {
    let cases: Vec<(PairResult, PairError)> = vec![
        (PairResult { ok: false, reason: Some("expired".into()), ..Default::default() }, PairError::Expired),
        (PairResult { ok: false, reason: Some("stopped".into()), ..Default::default() }, PairError::Stopped),
        (PairResult { ok: false, reason: Some("busy".into()), retry_after: Some(1.5), ..Default::default() }, PairError::Busy { retry_after: Some(1.5) }),
        (PairResult { ok: false, reason: Some("closed".into()), ..Default::default() }, PairError::NotPairing),
        (PairResult { ok: false, reason: Some("neverHeard".into()), message: Some("Come back at noon".into()), ..Default::default() }, PairError::MacSaid("Come back at noon".into())),
    ];
    for (reply, want) in cases {
        let h = host_with(|p| p.fault = PairFault::Reply(Box::new(reply.clone()))).await;
        assert_eq!(pairing::pair_with_code(CODE, "127.0.0.1", h.port(), None, &opts()).await, Err(want));
    }
    // The window closed on the Mac's side reads as the reason it was closed for.
    let h = host_with(|_| {}).await;
    h.close_pairing("expired");
    assert_eq!(pairing::pair_with_link(&h.pair_link(), &opts()).await, Err(PairError::Expired));
}

// ---- kind 18 and goodbyes, in a session

fn session_opts(id: &Identity, pin: PinMode) -> ConnectOptions {
    ConnectOptions {
        identity: id.clone(),
        pin,
        device: "Test Linux".into(),
        app_version: "0.1.0".into(),
        build: "1".into(),
        config: SessionConfig { min_silence: Duration::from_millis(700), ..Default::default() },
    }
}

async fn mac_info_event(h: &MockHost) -> MacInfoCheck {
    let id = Identity::generate_in_memory().unwrap();
    let mut s = connect(("127.0.0.1", h.port()), session_opts(&id, PinMode::Pinned(h.fingerprint))).await.unwrap();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), s.events.recv()).await.unwrap().unwrap() {
            Event::MacInfo(c) => return c,
            Event::Closed(r) => panic!("closed before kind 18: {r:?}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn kind_18_signed_by_the_session_key_updates_the_saved_mac() {
    let h = host_with(|_| {}).await;
    let mut mac = pairing::pair_with_link(&h.pair_link(), &opts()).await.unwrap();
    let check = mac_info_event(&h).await;
    assert!(matches!(check, MacInfoCheck::Verified(_)), "{check:?}");
    assert!(mac.apply_mac_info(&check, &h.fingerprint));
    assert!(mac.addresses.iter().any(|a| a.host == "192.0.2.7" && a.learned));
    assert!(mac.addresses.iter().any(|a| a.host == "127.0.0.1" && !a.learned));
    assert!(mac.info_issued_at > 0.0);
}

#[tokio::test]
async fn kind_18_signed_by_another_key_is_foreign_and_changes_nothing() {
    let signer = Identity::generate_in_memory().unwrap();
    let h = MockHost::start(MockConfig { macinfo_signer: Some(signer.clone()), pairing: Some(MockPairing::new(CODE)), ..Default::default() })
        .await
        .unwrap();
    let mut mac = pairing::pair_with_link(&h.pair_link(), &opts()).await.unwrap();
    let before = mac.clone();
    let check = mac_info_event(&h).await;
    // Valid signature, but the macID in it is not the signing key's, so it does not even verify.
    assert!(matches!(check, MacInfoCheck::Invalid | MacInfoCheck::Foreign { .. }), "{check:?}");
    assert!(!mac.apply_mac_info(&check, &h.fingerprint));
    assert_eq!(mac, before);
}

#[tokio::test]
async fn tampered_kind_18_is_invalid() {
    let h = MockHost::start(MockConfig { macinfo_tamper: true, ..Default::default() }).await.unwrap();
    assert_eq!(mac_info_event(&h).await, MacInfoCheck::Invalid);
}

#[tokio::test]
async fn goodbye_removed_and_pairing_required_end_with_typed_outcomes() {
    for (reason, standing, words) in [
        ("removed", Standing::Revoked, "Mock Mac removed this computer. To use it again, pair it again."),
        ("pairingRequired", Standing::NeedsPairing, "Mock Mac now asks devices to pair. Tap it to pair this computer."),
    ] {
        let h = host_with(|_| {}).await;
        let mut mac = pairing::pair_with_link(&h.pair_link(), &opts()).await.unwrap();
        let id = Identity::generate_in_memory().unwrap();
        let mut s = connect(("127.0.0.1", h.port()), session_opts(&id, PinMode::Pinned(mac.fingerprint))).await.unwrap();
        // Wait for the opening, then the Mac says goodbye.
        loop {
            if let Event::WindowList(_) = tokio::time::timeout(Duration::from_secs(5), s.events.recv()).await.unwrap().unwrap() {
                break;
            }
        }
        assert!(h.send_goodbye(&Goodbye { reason: reason.into(), message: None, minimum_version: None, reconnect: None }));
        let outcome = loop {
            if let Event::Closed(r) = tokio::time::timeout(Duration::from_secs(5), s.events.recv()).await.unwrap().unwrap() {
                    assert!(!r.reconnects(), "{reason} never reconnects");
                    match r {
                        CloseReason::Goodbye(g) => break g,
                        other => panic!("{other:?}"),
                    }
            }
        };
        assert_eq!(mac.apply_goodbye(&outcome), Some(standing));
        assert!(!mac.can_connect());
        assert_eq!(sill_net::policy::goodbye_text(&outcome, &mac.name, "computer"), words);
    }
}

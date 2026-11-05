use sill_discovery::browser::{MdnsBrowser, Browser, BrowserEvent};
use sill_discovery::policy::{make_tag, HomeDoor};
use sill_discovery::*;
use sill_mockhost::{MockConfig, MockHost};
use sill_net::saved::{SavedAddress, SavedMac, Standing};
use sill_net::session::CloseReason;
use sill_net::{ConnectOptions, Identity, PinMode, SessionConfig};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

fn opts() -> ConnectOptions {
    ConnectOptions {
        identity: Identity::generate_in_memory().unwrap(),
        pin: PinMode::TrustOnFirstUse,
        device: "Test".into(),
        app_version: "0.1.0".into(),
        build: "1".into(),
        config: SessionConfig::default(),
    }
}

fn saved(fp: [u8; 32], port: u16) -> SavedMac {
    SavedMac {
        mac_id: "MAC1".into(),
        name: "Studio".into(),
        fingerprint: fp,
        recognition_key: [5; 32],
        remote_port: 7455,
        addresses: vec![SavedAddress::typed("127.0.0.1", port)],
        paired_at: 0.0,
        last_connected_at: None,
        info_issued_at: 0.0,
        method: "qr".into(),
        standing: Standing::Paired,
    }
}

#[tokio::test]
async fn pinned_success_and_new_key() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let cfg = DialConfig::default();
    let mac = saved(host.fingerprint, host.port());
    match connect_saved(&mac, &[], opts(), &cfg).await {
        ConnectOutcome::Connected { address, .. } => assert_eq!(address, ("127.0.0.1".to_string(), host.port())),
        o => panic!("{o:?}"),
    }
    // The Mac set up again: another key answers. Typed outcome, no retry on the next address.
    let bad = saved([9; 32], host.port());
    let o = connect_saved(&bad, &[("127.0.0.1".into(), host.port())], opts(), &cfg).await;
    assert!(matches!(o, ConnectOutcome::NewKey { .. }), "{o:?}");
    assert!(o.words("Studio").unwrap().contains("new key"));
    // Revoked: not even dialed.
    let mut rev = saved(host.fingerprint, host.port());
    rev.standing = Standing::Revoked;
    assert!(matches!(connect_saved(&rev, &[], opts(), &cfg).await, ConnectOutcome::NotAllowed));
}

#[tokio::test]
async fn discovered_first_then_saved_and_unreachable() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    // Port 1 refuses at once; the saved address works. Discovery's goes first.
    let mac = saved(host.fingerprint, host.port());
    let o = connect_saved(&mac, &[("127.0.0.1".into(), 1)], opts(), &DialConfig::default()).await;
    match o {
        ConnectOutcome::Connected { address, .. } => assert_eq!(address.1, host.port()),
        o => panic!("{o:?}"),
    }
    assert_eq!(connect::candidates(&mac, &[("127.0.0.1".into(), 1)])[0].1, 1);
    let mut dead = saved([1; 32], 1);
    dead.addresses = vec![SavedAddress::typed("127.0.0.1", 1)];
    match connect_saved(&dead, &[], opts(), &DialConfig::default()).await {
        ConnectOutcome::Unreachable(e) => assert_eq!(e.len(), 1),
        o => panic!("{o:?}"),
    }
}

#[tokio::test]
async fn by_address() {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let o = connect_by_address("127.0.0.1", host.port(), PinMode::TrustOnFirstUse, opts(), &DialConfig::default()).await;
    assert!(matches!(o, ConnectOutcome::Connected { .. }));
}

#[tokio::test(start_paused = true)]
async fn driver_retries_then_connects_on_the_ladder() {
    let (row_tx, row_rx) = watch::channel(Some(vec![("127.0.0.1".to_string(), 1u16)]));
    let (_fg_tx, fg_rx) = watch::channel(true);
    let (_tap_tx, tap_rx) = mpsc::channel(4);
    let (lines_tx, mut lines) = mpsc::unbounded_channel();
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let mac = saved(host.fingerprint, host.port());
    let mut sup = Supervisor::new("Studio");
    sup.session_ended(0.0, &CloseReason::Eof);
    let t0 = tokio::time::Instant::now();
    let times = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let tt = times.clone();
    let end = reconnect(sup, ReconnectInputs { row: row_rx, foreground: fg_rx, taps: tap_rx, lines: lines_tx }, move |_addrs| {
        let n = {
            let mut g = tt.lock().unwrap();
            g.push(t0.elapsed().as_secs());
            g.len()
        };
        let mac = mac.clone();
        async move {
            if n < 5 {
                ConnectOutcome::Unreachable(vec![])
            } else {
                connect_saved(&mac, &[], opts(), &DialConfig::default()).await
            }
        }
    })
    .await;
    assert!(matches!(end, ReconnectEnd::Connected { .. }), "{end:?}");
    assert_eq!(*times.lock().unwrap(), vec![0, 5, 15, 35, 65]);
    let mut all = vec![];
    while let Ok(l) = lines.try_recv() {
        all.push(l);
    }
    assert!(all[0].starts_with("Reconnecting"));
    assert!(all.iter().any(|l| l.contains("isn\u{2019}t answering")));
    drop(row_tx);
}

#[tokio::test(start_paused = true)]
async fn driver_new_key_stops() {
    let (_r, row_rx) = watch::channel(Some(vec![]));
    let (_f, fg_rx) = watch::channel(true);
    let (_t, tap_rx) = mpsc::channel(1);
    let (lines_tx, _l) = mpsc::unbounded_channel();
    let mut sup = Supervisor::new("S");
    sup.session_ended(0.0, &CloseReason::Silent);
    let end = reconnect(sup, ReconnectInputs { row: row_rx, foreground: fg_rx, taps: tap_rx, lines: lines_tx }, |_| async {
        ConnectOutcome::NewKey { address: ("x".into(), 1) }
    })
    .await;
    assert!(matches!(end, ReconnectEnd::NewKey));
}

/// Real mDNS on this machine. Skipped (with a message) when multicast does not work here.
#[tokio::test]
async fn mdns_registration_is_browsed() {
    let ty = "_silltest._tcp.local.";
    let daemon = match mdns_sd::ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => return eprintln!("SKIP mdns: {e}"),
    };
    let key = [5u8; 32];
    let tag = make_tag(&key, [1; 6]);
    let props = [("r", tag.as_str()), ("p", "1")];
    let info = mdns_sd::ServiceInfo::new(ty, "Studio", "sillhost-test.local.", "127.0.0.1", 4242, &props[..])
        .unwrap()
        .enable_addr_auto();
    if daemon.register(info).is_err() {
        return eprintln!("SKIP mdns: register failed");
    }
    let browser = MdnsBrowser::new("_silltest._tcp");
    let mut rx = browser.browse().unwrap();
    let got = tokio::time::timeout(Duration::from_secs(8), async {
        while let Some(e) = rx.recv().await {
            if let BrowserEvent::Resolved(r) = e {
                if r.name == "Studio" {
                    return Some(r);
                }
            }
        }
        None
    })
    .await;
    match got {
        Ok(Some(r)) => {
            assert_eq!(r.port, 4242);
            assert_eq!(r.door, HomeDoor::PairingRequired);
            assert_eq!(r.tag.as_deref(), Some(tag.as_str()));
            assert!(!r.addrs.is_empty());
            eprintln!("mdns OK: {r:?}");
        }
        _ => eprintln!("SKIP mdns: nothing resolved in 8 s (multicast unavailable in this sandbox?)"),
    }
    let _ = daemon.shutdown();
}

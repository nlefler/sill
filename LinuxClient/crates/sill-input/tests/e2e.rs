//! Synthetic winit-like events through the engine, over a real session, into the mock host; the exact
//! kind 8 sequence the host recorded is asserted.

use sill_input::*;
use sill_mockhost::{MockConfig, MockHost};
use sill_net::identity::Identity;
use sill_net::{Command, ConnectOptions, PinMode, SessionConfig};
use sill_proto::{decode_json, InputEvent, Kind, PointerAction as A, ScrollPhase as P};
use std::time::Duration;
use winit::keyboard::KeyCode as K;

fn engine(host_repeats: bool) -> InputEngine {
    let mut e = InputEngine::new(InputConfig::default());
    e.set_window(1000.0, 500.0, 1.0);
    e.set_frame(Some((1600, 900)));
    e.set_host(true, host_repeats);
    e
}

/// Run `script` against a fresh engine, send everything it returns, and give back what the host recorded.
async fn recorded(host_repeats: bool, script: impl FnOnce(&mut InputEngine) -> Vec<InputEvent>) -> (Vec<InputEvent>, Vec<InputEvent>) {
    let host = MockHost::start(MockConfig::default()).await.unwrap();
    let opts = ConnectOptions {
        identity: Identity::generate_in_memory().unwrap(),
        pin: PinMode::Pinned(host.fingerprint),
        device: "test".into(),
        app_version: "0".into(),
        build: "1".into(),
        config: SessionConfig::default(),
    };
    let session = sill_net::connect(("127.0.0.1", host.port()), opts).await.unwrap();
    let mut e = engine(host_repeats);
    let sent = script(&mut e);
    for ev in &sent {
        assert!(session.handle.send(Command::Input(ev.clone())));
    }
    let n = sent.len();
    let ok = host.wait_for(Duration::from_secs(5), |m| m.iter().filter(|x| x.kind == Kind::Input).count() >= n).await;
    assert!(ok, "the host did not receive {n} inputs");
    let got = host.received().iter().filter(|m| m.kind == Kind::Input).map(|m| decode_json::<InputEvent>(&m.payload).unwrap()).collect();
    (sent, got)
}

fn mv(x: f64, y: f64) -> InputEvent {
    InputEvent::Pointer { action: A::Move, x, y }
}
fn ptr(a: A, x: f64, y: f64) -> InputEvent {
    InputEvent::Pointer { action: a, x, y }
}
fn key(u: u16, down: bool, m: u64) -> InputEvent {
    InputEvent::Key { hid_usage: u, down, modifiers: m }
}

// The 16:9 frame in the 1000x500 window: picture 888.89 x 500 at x = 55.56.
fn frac(x: f64, y: f64) -> (f64, f64) {
    let r = video_rect(1000.0, 500.0, 1600.0, 900.0);
    ((x - r.x) / r.w, (y - r.y) / r.h)
}

#[tokio::test]
async fn pointer_keys_and_scroll_arrive_as_the_host_decodes_them() {
    let (sent, got) = recorded(true, |e| {
        let mut out = vec![];
        let mut t = 0.0;
        // Hover, click, right click.
        out.extend(e.cursor_moved(t, 500.0, 250.0));
        t += 0.1;
        out.extend(e.button(t, Button::Left, true));
        out.extend(e.button(t + 0.05, Button::Left, false));
        t += 0.2;
        out.extend(e.button(t, Button::Right, true));
        out.extend(e.button(t, Button::Right, false));
        // Middle button: nothing.
        out.extend(e.button(t, Button::Other, true));
        // Super+C, then Shift+A held with the OS repeating A.
        t += 0.1;
        out.extend(e.key(t, K::SuperLeft, true, None, false));
        out.extend(e.key(t, K::KeyC, true, Some("c"), false));
        out.extend(e.key(t, K::KeyC, false, None, false));
        out.extend(e.key(t, K::SuperLeft, false, None, false));
        out.extend(e.key(t, K::KeyA, true, Some("a"), false));
        out.extend(e.key(t, K::KeyA, true, Some("a"), true));
        out.extend(e.key(t, K::KeyA, true, Some("a"), true));
        out.extend(e.key(t, K::KeyA, false, None, false));
        // A wheel notch, then a slow touchpad scroll.
        out.extend(e.wheel(t, Delta::Lines(0.0, 1.0), Phase::Moved));
        out.extend(e.wheel(t, Delta::Pixels(0.0, 4.0), Phase::Started));
        out.extend(e.wheel(t + 0.05, Delta::Pixels(0.0, 4.0), Phase::Moved));
        out.extend(e.wheel(t + 0.10, Delta::Pixels(0.0, 0.0), Phase::Ended));
        out
    })
    .await;
    assert_eq!(got, sent, "the host got exactly what was sent, in order");
    let (cx, cy) = frac(500.0, 250.0);
    let cmd = sill_proto::modifiers::COMMAND;
    let at = (cx, cy);
    assert_eq!(
        got,
        vec![
            mv(cx, cy),
            ptr(A::LeftDown, cx, cy),
            ptr(A::LeftUp, cx, cy),
            ptr(A::RightDown, cx, cy),
            ptr(A::RightUp, cx, cy),
            key(0xE3, true, cmd),
            key(6, true, cmd),
            key(6, false, cmd),
            key(0xE3, false, 0),
            key(4, true, 0),
            key(4, false, 0),
            InputEvent::Scroll { x: at.0, y: at.1, dx: 0.0, dy: 10.0 / 500.0 },
            InputEvent::ScrollGesture { phase: P::Began, x: at.0, y: at.1 },
            InputEvent::Scroll { x: at.0, y: at.1, dx: 0.0, dy: 4.0 / 500.0 },
            InputEvent::Scroll { x: at.0, y: at.1, dx: 0.0, dy: 4.0 / 500.0 },
            InputEvent::ScrollGesture { phase: P::Ended, x: at.0, y: at.1 },
        ]
    );
}

#[tokio::test]
async fn focus_loss_lets_go_of_keys_buttons_and_an_open_scroll() {
    let (_, got) = recorded(true, |e| {
        let mut out = vec![];
        out.extend(e.cursor_moved(0.0, 500.0, 250.0));
        out.extend(e.key(0.1, K::ControlLeft, true, None, false));
        out.extend(e.key(0.1, K::ArrowLeft, true, None, false));
        out.extend(e.button(0.2, Button::Left, true));
        out.extend(e.wheel(0.3, Delta::Pixels(0.0, 3.0), Phase::Started));
        out.extend(e.set_focused(false));
        // While unfocused nothing goes, and a late key up for a released key is nothing.
        out.extend(e.key(0.4, K::KeyB, true, Some("b"), false));
        out.extend(e.cursor_moved(0.4, 600.0, 250.0));
        out.extend(e.key(0.5, K::ArrowLeft, false, None, false));
        out
    })
    .await;
    let (cx, cy) = frac(500.0, 250.0);
    let ctl = sill_proto::modifiers::CONTROL;
    // The exact release order: keys (last first), buttons, then the scroll gesture's end.
    assert_eq!(
        got[got.len() - 4..],
        [key(80, false, ctl), key(0xE0, false, 0), ptr(A::LeftUp, cx, cy), InputEvent::ScrollGesture { phase: P::Ended, x: cx, y: cy }]
    );
    assert_eq!(got.len(), 10, "{got:?}");
    assert!(!got.iter().any(|e| matches!(e, InputEvent::Key { hid_usage: 5, .. })), "no key while unfocused");
}

#[tokio::test]
async fn a_button_released_outside_the_picture_is_still_sent() {
    let (_, got) = recorded(true, |e| {
        let mut out = vec![];
        out.extend(e.cursor_moved(0.0, 500.0, 250.0));
        out.extend(e.button(0.1, Button::Left, true));
        // Dragged out of the picture into the black bar, then out of the window.
        out.extend(e.cursor_moved(0.2, 20.0, 250.0));
        e.cursor_left();
        out.extend(e.button(0.3, Button::Left, false));
        out
    })
    .await;
    let (cx, cy) = frac(500.0, 250.0);
    assert_eq!(got, vec![mv(cx, cy), ptr(A::LeftDown, cx, cy), mv(0.0, cy), ptr(A::LeftUp, 0.0, cy)]);
}

#[tokio::test]
async fn a_click_in_the_letterbox_sends_nothing_at_all() {
    let (sent, _) = recorded(true, |e| {
        let mut out = vec![];
        out.extend(e.cursor_moved(0.0, 20.0, 250.0));
        out.extend(e.button(0.1, Button::Left, true));
        out.extend(e.button(0.2, Button::Left, false));
        // A marker so the recorder has something to wait for.
        out.extend(e.key(0.3, K::Escape, true, None, false));
        out
    })
    .await;
    assert_eq!(sent, vec![key(41, true, 0)]);
}

#[tokio::test]
async fn a_host_without_key_repeat_gets_text_for_characters_and_ime_commits() {
    let (_, got) = recorded(false, |e| {
        let mut out = vec![];
        out.extend(e.key(0.0, K::KeyH, true, Some("h"), false));
        out.extend(e.key(0.0, K::KeyH, false, None, false));
        out.extend(e.key(0.1, K::Enter, true, Some("\r"), false));
        out.extend(e.ime_commit("你好"));
        out
    })
    .await;
    assert_eq!(got, vec![InputEvent::Text("h".into()), InputEvent::Text("\n".into()), InputEvent::Text("你好".into())]);
}

#[tokio::test]
async fn moves_are_coalesced_by_the_clock_and_flushed_by_poll() {
    let (_, got) = recorded(true, |e| {
        let mut out = vec![];
        out.extend(e.cursor_moved(0.000, 300.0, 100.0));
        out.extend(e.cursor_moved(0.002, 310.0, 100.0));
        out.extend(e.cursor_moved(0.004, 320.0, 100.0));
        assert_eq!(e.next_deadline(), Some(0.008));
        out.extend(e.poll(0.010));
        out
    })
    .await;
    let (a, y) = frac(300.0, 100.0);
    let (b, _) = frac(320.0, 100.0);
    assert_eq!(got, vec![mv(a, y), mv(b, y)]);
}

#[tokio::test]
async fn the_session_end_releases_what_is_held() {
    // The source going away (window list says nothing streams) lets keys go too.
    let (_, got) = recorded(true, |e| {
        let mut out = e.key(0.0, K::KeyQ, true, Some("q"), false);
        out.extend(e.set_host(false, true));
        out
    })
    .await;
    assert_eq!(got, vec![key(20, true, 0), key(20, false, 0)]);
}

#[test]
fn the_pointer_feed_counts_what_the_session_sent() {
    let mut f = PointerFeed::new();
    f.input_sent();
    let stale = sill_proto::MacPointer { x: Some(0.1), y: Some(0.1), inside: Some(true), seen: Some(0) };
    let fresh = sill_proto::MacPointer { seen: Some(1), ..stale.clone() };
    assert_eq!(f.accept(&stale), None);
    assert_eq!(f.accept(&fresh), Some(Some((0.1, 0.1))));
}

//! M7 end to end: the hub and controller against the in-process assert!(r.host.received().iter().filter(|m| m.kind == Kind::SelectSource).count() >= desktops_before, "a locked Mac is asked for the Desktop like any other");ock host (picks, launches, window buttons,
//! settings with their tokens and reverts, the locked-Mac cover, the switcher's pictures, the reconnect
//! ladder, pairing, the Mac list), and the egui panels (layout, the video area, the input gate, an
//! offscreen render when a GPU exists).

use sill_app::client::{self, Client};
use sill_app::control::Link;
use sill_app::seam::{AppCommand, UiEvent, UiSink};
use sill_app::ui::Gui;
use sill_discovery::{BrowserEvent, DialConfig, FakeBrowser, HomeDoor, Row};
use sill_media::DecoderPref;
use sill_mockhost::{MockConfig, MockHost, MockPairing};
use sill_net::saved::{SavedAddress, SavedMac, SavedMacs, Standing};
use sill_net::{Identity, PinMode, SessionConfig};
use sill_proto::*;
use sill_ui_state::{ConnState, Model, SettingPick, UiIntent};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap()
}

struct Rig {
    rt: tokio::runtime::Runtime,
    host: MockHost,
    client: Client,
    rx: Receiver<UiEvent>,
    model: Model,
    path: std::path::PathBuf,
}

fn temp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("sill-ui-{}-{}-{}", name, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&d).unwrap();
    d.join("saved-macs.json")
}

fn saved_for(host: &MockHost) -> SavedMac {
    SavedMac {
        mac_id: sill_net::pairing::mac_id(&host.fingerprint),
        name: "Mock Mac".into(),
        fingerprint: host.fingerprint,
        recognition_key: [0x33; 32],
        remote_port: 7455,
        addresses: vec![SavedAddress::typed("127.0.0.1", host.port())],
        paired_at: 1.0,
        last_connected_at: None,
        info_issued_at: 0.0,
        method: "code".into(),
        standing: Standing::Paired,
    }
}

fn quick_link(identity: Identity, saved: SavedMacs, current: Option<String>, browser: Option<Arc<FakeBrowser>>) -> Link {
    Link {
        identity,
        device: "test".into(),
        saved,
        browser: browser.map(|b| b as Arc<dyn sill_discovery::Browser>),
        dial: DialConfig { stagger: Duration::from_millis(100), timeout: Duration::from_secs(3) },
        session_config: SessionConfig { min_silence: Duration::from_millis(700), ping_interval: Duration::from_millis(100), connect_timeout: Duration::from_secs(3) },
        pair_rounds: Some(1000),
        current,
    }
}

impl Rig {
    /// A session to a fresh mock host; `with_link` gives it a saved record of that host.
    fn new(cfg: MockConfig, with_link: bool, browser: Option<Arc<FakeBrowser>>) -> Rig {
        let rt = rt();
        let host = rt.block_on(MockHost::start(MockConfig { frames: 0, ..cfg })).unwrap();
        let id = Identity::generate_in_memory().unwrap();
        let path = temp("rig");
        let mut saved = SavedMacs::load(&path).unwrap();
        let mac = saved_for(&host);
        let mac_id = mac.mac_id.clone();
        if with_link {
            saved.upsert(mac);
            saved.save().unwrap();
        }
        let link = quick_link(id.clone(), saved, with_link.then_some(mac_id), browser);
        let opts = client::connect_options(id, "test", PinMode::Pinned(host.fingerprint), link.session_config.clone());
        let session = rt.block_on(sill_net::connect(("127.0.0.1", host.port()), opts)).unwrap();
        let (tx, rx) = channel();
        let tx = std::sync::Mutex::new(tx);
        let ui: UiSink = Arc::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        });
        let client = client::start_with(rt.handle(), Some(session), Some(link), DecoderPref::Software, ui, false);
        Rig { rt, host, client, rx, model: Model::default(), path }
    }

    fn intent(&self, i: UiIntent) {
        let _ = self.client.cmds.send(AppCommand::Intent(i));
    }

    /// Drain the window's events until the newest model satisfies `pred`.
    fn until(&mut self, secs: f64, pred: impl Fn(&Model) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        loop {
            if pred(&self.model) {
                return true;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            if let Ok(UiEvent::Model(m)) = self.rx.recv_timeout(left.min(Duration::from_millis(50))) {
                self.model = *m;
            }
        }
    }

    fn host_got(&self, secs: f64, kind: Kind) -> Vec<Message> {
        self.rt.block_on(self.host.wait_for(Duration::from_secs_f64(secs), |m| m.iter().any(|x| x.kind == kind)));
        self.host.received().into_iter().filter(|m| m.kind == kind).collect()
    }

    fn send<T: serde::Serialize>(&self, kind: Kind, v: &T) {
        assert!(self.host.send(&Message::new(kind, 0.0, false, encode_json(v))));
    }
}

fn list(active: StreamSource, ids: &[u32]) -> WindowList {
    WindowList {
        mac_name: "Mock Mac".into(),
        windows: ids.iter().map(|&id| WindowInfo { id, title: format!("Doc {id}"), app_name: "Notes".into(), bundle_id: "com.apple.Notes".into(), width: 800, height: 600 }).collect(),
        active,
        launch_id: None, host_version: None, protocol: None, gestures: None, clipboard: None, key_repeat: Some(1), locked: None, off_console: None,
    }
}

fn state(max_fps: i64) -> HostSettingsState {
    HostSettingsState {
        settings: StreamSettings { max_fps, bitrate: 15_000_000, capture_scale: 2.0, prioritize_speed: false, virtual_display: false, direct_wireless: None, send_audio: None },
        persistent: true,
        virtual_display_available: true,
        virtual_display_note: None,
        software_encoder: false,
        stream: Some(RunningStream { width: 1512, height: 982, fps: 60, mbps: 15, on_virtual_display: false }),
        answering: None,
        away: None,
        link: None,
        audio_note: None,
        session_paired: Some(true),
    }
}

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    use image::ImageEncoder;
    let mut out = vec![];
    image::codecs::jpeg::JpegEncoder::new(&mut out).write_image(&vec![90u8; (w * h * 3) as usize], w, h, image::ExtendedColorType::Rgb8).unwrap();
    out
}

fn png(w: u32, h: u32) -> Vec<u8> {
    use image::ImageEncoder;
    let mut out = vec![];
    image::codecs::png::PngEncoder::new(&mut out).write_image(&vec![200u8; (w * h * 4) as usize], w, h, image::ExtendedColorType::Rgba8).unwrap();
    out
}

#[test]
fn a_pick_a_launch_and_a_window_button_reach_the_host() {
    let mut r = Rig::new(MockConfig::default(), false, None);
    assert!(r.until(5.0, |m| m.windows.len() == 1 && m.conn == ConnState::Connected), "the mock's list shows");
    // The Desktop is the active source already: picking it sends nothing, picking the window does.
    r.intent(UiIntent::Pick(StreamSource::Desktop));
    r.intent(UiIntent::Pick(StreamSource::Window(7)));
    r.intent(UiIntent::Pick(StreamSource::Window(7)));
    r.intent(UiIntent::Launch("com.apple.TextEdit".into()));
    r.intent(UiIntent::WindowButton { id: 7, action: WindowAction::Minimize });
    r.intent(UiIntent::WindowButton { id: 99, action: WindowAction::Close });
    let launches = r.host_got(5.0, Kind::LaunchApp);
    assert_eq!(decode_json::<LaunchApp>(&launches[0].payload).unwrap().bundle_id, "com.apple.TextEdit");
    let btn = r.host_got(5.0, Kind::WindowCommand);
    assert_eq!(btn.len(), 1, "an unlisted window gets no button press");
    assert_eq!(decode_json::<WindowCommand>(&btn[0].payload).unwrap(), WindowCommand { id: 7, action: WindowAction::Minimize });
    let picks: Vec<StreamSource> = r.host_got(5.0, Kind::SelectSource).iter().map(|m| decode_json(&m.payload).unwrap()).collect();
    assert_eq!(picks, vec![StreamSource::Window(7)], "one kind 6, for the window, once");
}

#[test]
fn the_switcher_gets_pictures_icons_apps_and_drops_closed_windows() {
    let mut r = Rig::new(MockConfig::default(), false, None);
    assert!(r.until(5.0, |m| m.windows.len() == 1));
    r.host.send(&Message::new(Kind::Thumbnail, 0.0, false, encode_thumbnail(7, &jpeg(64, 40))));
    r.host.send(&Message::new(Kind::AppIcon, 0.0, false, encode_icon("com.apple.Notes", &png(32, 32))));
    r.send(Kind::AppList, &vec![AppInfo { name: "Safari".into(), bundle_id: "com.apple.Safari".into() }, AppInfo { name: "Notes".into(), bundle_id: "com.apple.Notes".into() }]);
    assert!(r.until(5.0, |m| m.windows.first().is_some_and(|w| w.thumb.is_some() && w.icon.is_some()) && m.apps.len() == 2), "{:?}", r.model.windows);
    assert_eq!(r.model.apps[0].name, "Notes");
    assert!(r.model.apps.iter().any(|a| a.icon.is_some()));
    // The window closes on the Mac: its picture goes with it.
    r.send(Kind::WindowList, &list(StreamSource::Desktop, &[]));
    assert!(r.until(5.0, |m| m.windows.is_empty()));
}

#[test]
fn settings_carry_increasing_tokens_show_at_once_and_a_refusal_goes_back() {
    let mut r = Rig::new(MockConfig::default(), false, None);
    assert!(r.until(5.0, |m| m.settings.is_some()), "the mock sends kind 16 after its list");
    r.intent(UiIntent::Setting(SettingPick::FrameRate(120)));
    r.intent(UiIntent::Setting(SettingPick::Quality(40_000_000)));
    r.intent(UiIntent::Setting(SettingPick::FrameRate(120)));
    assert!(r.until(5.0, |m| m.settings.as_ref().is_some_and(|s| s.shown.max_fps == 120 && s.shown.bitrate == 40_000_000)), "shown before the Mac answers");
    r.rt.block_on(r.host.wait_for(Duration::from_secs(5), |m| m.iter().filter(|x| x.kind == Kind::ChangeSettings).count() >= 2));
    let changes: Vec<HostSettingsChange> = r.host.received().iter().filter(|m| m.kind == Kind::ChangeSettings).map(|m| decode_json(&m.payload).unwrap()).collect();
    assert_eq!(changes.len(), 2, "the repeated pick sends nothing");
    assert_eq!((changes[0].max_fps, changes[0].bitrate), (Some(120), None));
    assert_eq!((changes[1].max_fps, changes[1].bitrate), (None, Some(40_000_000)));
    assert!(changes[1].token.unwrap() > changes[0].token.unwrap());
    // The Mac takes the first and refuses the second: its answer says 60 fps stays... here, the bitrate stays.
    let mut a = state(120);
    a.answering = changes[1].token;
    r.send(Kind::HostSettings, &a);
    assert!(r.until(5.0, |m| m.settings.as_ref().is_some_and(|s| s.shown.bitrate == 15_000_000) && m.notice.as_deref() == Some("Mock Mac kept its setting.")), "{:?}", r.model.notice);
    assert_eq!(r.model.settings.as_ref().unwrap().shown.max_fps, 120, "the other pick still waits for its own answer");
}

#[test]
fn a_setting_the_mac_never_answers_goes_back_after_four_seconds_with_words() {
    let mut r = Rig::new(MockConfig::default(), false, None);
    assert!(r.until(5.0, |m| m.settings.is_some()));
    let t = Instant::now();
    r.intent(UiIntent::Setting(SettingPick::PrioritizeSpeed(true)));
    assert!(r.until(3.0, |m| m.settings.as_ref().is_some_and(|s| s.shown.prioritize_speed)));
    assert!(r.until(6.0, |m| m.settings.as_ref().is_some_and(|s| !s.shown.prioritize_speed)), "reverted");
    let took = t.elapsed().as_secs_f64();
    assert!((3.9..6.5).contains(&took), "after the 4 s timeout, took {took}");
    assert_eq!(r.model.notice.as_deref(), Some("Mock Mac didn\u{2019}t answer. Try again."));
}

#[test]
fn a_locked_mac_shows_the_cover_and_the_unlock_ends_it_on_a_keyframe() {
    let mut r = Rig::new(MockConfig::default(), false, None);
    assert!(r.until(5.0, |m| m.settings.is_some()));
    let mut l = list(StreamSource::None, &[]);
    l.locked = Some(true);
    l.off_console = Some(true);
    r.send(Kind::WindowList, &l);
    assert!(r.until(5.0, |m| m.cover.as_ref().is_some_and(|c| c.title == "Another user started controlling this Mac.")));
    assert!(r.model.locked_line.as_deref().unwrap().starts_with("Another user"));
    let desktops_before = r.host.received().iter().filter(|m| m.kind == Kind::SelectSource).count();
    r.send(Kind::WindowList, &list(StreamSource::Desktop, &[]));
    assert!(r.until(5.0, |m| m.cover.as_ref().is_some_and(|c| c.title == "Unlocking\u{2026}")));
    assert_eq!(r.host.received().iter().filter(|m| m.kind == Kind::SelectSource).count(), desktops_before, "no Desktop ask while locked");
    r.host.send(&Message::new(Kind::Frame, 0.0, true, vec![0, 0, 0, 2, 0x26, 1]));
    assert!(r.until(5.0, |m| m.cover.is_none()), "a keyframe ends Unlocking");
}

#[test]
fn a_verified_kind_18_teaches_the_saved_mac_an_address() {
    let mut r = Rig::new(MockConfig::default(), true, None);
    assert!(r.until(5.0, |m| m.conn == ConnState::Connected));
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let text = std::fs::read_to_string(&r.path).unwrap_or_default();
        if text.contains("192.0.2.7") {
            break;
        }
        assert!(Instant::now() < end, "kind 18's address never reached the record: {text}");
        r.until(0.1, |_| false);
    }
}

#[test]
fn a_silent_mac_runs_the_ladder_and_the_session_comes_back() {
    let mut r = Rig::new(MockConfig::default(), true, None);
    assert!(r.until(5.0, |m| m.settings.is_some()));
    r.host.go_silent();
    assert!(r.until(10.0, |m| matches!(&m.conn, ConnState::Reconnecting(l) if l.contains("Mock Mac"))), "{:?}", r.model.conn);
    // Nothing lists the Mac (no browser here): a tap dials its saved addresses.
    r.intent(UiIntent::Retry);
    assert!(r.until(10.0, |m| m.conn == ConnState::Connected && m.settings.is_some()), "{:?}", r.model.conn);
    let hellos = r.host.received().iter().filter(|m| m.kind == Kind::Hello).count();
    assert_eq!(hellos, 2, "a second session said hello");
    // The new session streams its own state from scratch and still takes intents.
    r.intent(UiIntent::Launch("com.apple.Notes".into()));
    assert_eq!(r.host_got(5.0, Kind::LaunchApp).len(), 1);
}

#[test]
fn removed_means_pair_again_and_no_ladder() {
    let mut r = Rig::new(MockConfig::default(), true, None);
    assert!(r.until(5.0, |m| m.settings.is_some()));
    r.host.send_goodbye(&Goodbye { reason: "removed".into(), message: None, minimum_version: None, reconnect: None });
    assert!(r.until(5.0, |m| matches!(&m.conn, ConnState::Ended(t) if t.contains("removed this computer"))), "{:?}", r.model.conn);
    let saved = SavedMacs::load(&r.path).unwrap();
    assert_eq!(saved.macs[0].standing, Standing::Revoked, "the record says pair again");
    assert!(r.until(5.0, |m| m.macs.iter().any(|x| x.needs_pairing && !x.listed)));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(r.host.received().iter().filter(|m| m.kind == Kind::Hello).count(), 1, "no redial");
}

#[test]
fn pairing_by_link_saves_the_mac_and_connects() {
    let rt0 = rt();
    let mut cfg = MockConfig { frames: 0, ..Default::default() };
    let mut p = MockPairing::new("123456789012");
    p.rounds = 1000;
    cfg.pairing = Some(p);
    let host = rt0.block_on(MockHost::start(cfg)).unwrap();
    let url = host.pair_link().url();
    let path = temp("pair");
    let id = Identity::generate_in_memory().unwrap();
    let link = quick_link(id, SavedMacs::load(&path).unwrap(), None, None);
    let (tx, rx) = channel();
    let tx = std::sync::Mutex::new(tx);
    let ui: UiSink = Arc::new(move |e| {
        let _ = tx.lock().unwrap().send(e);
    });
    let client = client::start_with(rt0.handle(), None, Some(link), DecoderPref::Software, ui, false);
    let mut model = Model::default();
    let mut wait = |pred: &dyn Fn(&Model) -> bool| {
        let end = Instant::now() + Duration::from_secs(10);
        while !pred(&model) && Instant::now() < end {
            if let Ok(UiEvent::Model(m)) = rx.recv_timeout(Duration::from_millis(50)) {
                model = *m;
            }
        }
        pred(&model)
    };
    assert!(wait(&|m| m.conn == ConnState::Idle));
    // A junk link is refused with words and nothing is dialed.
    let _ = client.cmds.send(AppCommand::Intent(UiIntent::PairLink("not a link".into())));
    assert!(wait(&|m| m.pairing.error.is_some()));
    let _ = client.cmds.send(AppCommand::Intent(UiIntent::PairLink(url)));
    assert!(wait(&|m| m.pairing.done.as_deref() == Some("Paired with Mock Mac.")), "{:?}", model.pairing);
    assert!(wait(&|m| m.conn == ConnState::Connected && m.settings.is_some()), "{:?}", model.conn);
    let saved = SavedMacs::load(&path).unwrap();
    assert_eq!(saved.macs.len(), 1);
    assert_eq!(saved.macs[0].fingerprint, host.fingerprint);
}

#[test]
fn a_wrong_code_shows_the_pairing_error_words() {
    let rt0 = rt();
    let mut p = MockPairing::new("123456789012");
    p.rounds = 1000;
    let host = rt0.block_on(MockHost::start(MockConfig { frames: 0, pairing: Some(p), ..Default::default() })).unwrap();
    let id = Identity::generate_in_memory().unwrap();
    let link = quick_link(id, SavedMacs::load(&temp("code")).unwrap(), None, None);
    let (tx, rx) = channel();
    let tx = std::sync::Mutex::new(tx);
    let ui: UiSink = Arc::new(move |e| {
        let _ = tx.lock().unwrap().send(e);
    });
    let client = client::start_with(rt0.handle(), None, Some(link), DecoderPref::Software, ui, false);
    let _ = client.cmds.send(AppCommand::Intent(UiIntent::PairCode { address: format!("127.0.0.1:{}", host.port()), code: "000000000000".into() }));
    let end = Instant::now() + Duration::from_secs(10);
    let mut err = None;
    while err.is_none() && Instant::now() < end {
        if let Ok(UiEvent::Model(m)) = rx.recv_timeout(Duration::from_millis(50)) {
            err = m.pairing.error.clone();
        }
    }
    let err = err.expect("an error");
    assert!(!err.is_empty() && !err.contains("PairError"), "{err}");
}

#[test]
fn the_mac_list_classifies_discovered_rows_and_saved_macs() {
    let fake = Arc::new(FakeBrowser::default());
    let mut r = Rig::new(MockConfig::default(), true, Some(fake.clone()));
    let end = Instant::now() + Duration::from_secs(5);
    while fake.starts() == 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    let row = |name: &str, door| Row { name: name.into(), addrs: vec!["127.0.0.1".parse().unwrap()], port: 4000, tag: None, door };
    fake.push(BrowserEvent::Resolved(row("Studio", HomeDoor::PairingRequired)));
    fake.push(BrowserEvent::Resolved(row("Old Mac", HomeDoor::Plain)));
    fake.push(BrowserEvent::Resolved(row("Guest", HomeDoor::Open)));
    assert!(r.until(5.0, |m| m.macs.len() == 4), "{:?}", r.model.macs);
    let by = |n: &str| r.model.macs.iter().find(|m| m.name == n).unwrap().clone();
    assert_eq!((by("Studio").word.as_str(), by("Studio").needs_pairing, by("Studio").address.as_deref()), ("Not paired", true, Some("127.0.0.1:4000")));
    assert_eq!((by("Old Mac").word.as_str(), by("Old Mac").dialable), ("Update Sill on the Mac", false));
    assert!(by("Guest").open_door && by("Guest").dialable);
    let saved = by("Mock Mac");
    assert!((saved.word.as_str(), saved.saved, saved.listed) == ("Paired", true, false));
    fake.push(BrowserEvent::Removed("Studio".into()));
    assert!(r.until(5.0, |m| m.macs.len() == 3));
}

// ---------------------------------------------------------------------------- the panels

fn raw(w: f32, h: f32, events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))), events, ..Default::default() }
}

#[allow(clippy::field_reassign_with_default)]
fn connected_model() -> Model {
    let mut m = Model::default();
    m.conn = ConnState::Connected;
    m.mac_name = "Mock Mac".into();
    m.active = StreamSource::Desktop;
    let img = |w: u32, h: u32| {
        let d = sill_ui_state::switcher::decode_image(&jpeg(w, h)).unwrap();
        Arc::new(d)
    };
    m.windows = (1..=3)
        .map(|i| sill_ui_state::WindowEntry { id: i, title: format!("Document {i}"), app: "Notes".into(), bundle_id: "com.apple.Notes".into(), width: 800, height: 600, thumb: Some(img(160, 100)), icon: Some(img(32, 32)) })
        .collect();
    m.apps = vec![sill_ui_state::AppEntry { name: "Safari".into(), bundle_id: "com.apple.Safari".into(), icon: None }];
    m.settings = Some(sill_ui_state::SettingsView {
        shown: state(60).settings,
        pending: vec![],
        persistent: true,
        virtual_display_available: false,
        virtual_display_note: Some("Needs Screen Recording".into()),
        software_encoder: false,
        running: Some("1512 x 982, 60 fps, 15 Mbps".into()),
    });
    m
}

#[test]
fn the_video_area_is_what_the_panels_leave() {
    let mut g = Gui::new();
    let m = connected_model();
    let out = g.run(raw(1280.0, 720.0, vec![]), &m);
    let [x, y, w, h] = out.area;
    assert_eq!(x, sill_app::ui::SIDE_WIDTH, "the switcher takes its width on the left");
    assert!(y > 10.0 && y < 60.0, "the bar takes the top: {y}");
    assert!((w - (1280.0 - x)).abs() < 1.0 && (h - (720.0 - y)).abs() < 1.0, "{:?}", out.area);
    // With the switcher folded the picture gets the full width.
    g.local.side_open = false;
    let out = g.run(raw(1280.0, 720.0, vec![]), &m);
    assert_eq!(out.area[0], 0.0);
    // Not connected: the Mac list is a window over the area, which stays the whole window under the bar.
    let out = g.run(raw(1280.0, 720.0, vec![]), &Model::default());
    assert_eq!(out.area[0], 0.0);
}

#[test]
fn panels_take_the_pointer_and_the_picture_does_not() {
    let mut g = Gui::new();
    let m = connected_model();
    let at = |p: egui::Pos2| vec![egui::Event::PointerMoved(p)];
    g.run(raw(1280.0, 720.0, at(egui::pos2(120.0, 200.0))), &m);
    g.run(raw(1280.0, 720.0, at(egui::pos2(120.0, 201.0))), &m);
    assert!(g.ctx.wants_pointer_input(), "over the switcher: not the Mac's");
    g.run(raw(1280.0, 720.0, at(egui::pos2(800.0, 400.0))), &m);
    g.run(raw(1280.0, 720.0, at(egui::pos2(801.0, 400.0))), &m);
    assert!(!g.ctx.wants_pointer_input(), "over the picture: the Mac's");
    g.run(raw(1280.0, 720.0, at(egui::pos2(300.0, 8.0))), &m);
    assert!(g.ctx.wants_pointer_input(), "over the bar");
    // A text field with the keyboard keeps keys from the Mac.
    g.local.apps_open = true;
    let mut found = false;
    for _ in 0..6 {
        g.run(raw(1280.0, 720.0, vec![]), &m);
        found |= g.ctx.wants_keyboard_input();
    }
    assert!(!found, "nothing has the keyboard until a field is clicked");
}

fn click(g: &mut Gui, m: &Model, p: egui::Pos2) -> Vec<UiIntent> {
    let btn = |pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    let mut all = vec![];
    for events in [vec![egui::Event::PointerMoved(p)], vec![], vec![btn(true)], vec![btn(false)], vec![]] {
        all.extend(g.run(raw(1280.0, 720.0, events), m).intents);
    }
    all
}

#[test]
fn clicking_the_panels_emits_intents() {
    let mut g = Gui::new();
    let mut m = connected_model();
    m.active = StreamSource::Window(1);
    // Desktop is the first entry of the switcher, the windows follow.
    let intents = click(&mut g, &m, egui::pos2(120.0, 41.0));
    assert!(intents.contains(&UiIntent::Pick(StreamSource::Desktop)), "{intents:?}");
    let intents = click(&mut g, &m, egui::pos2(120.0, 300.0));
    assert!(intents.contains(&UiIntent::Pick(StreamSource::Window(2))), "{intents:?}");
    // Settings: the frame rate buttons.
    let mut g = Gui::new();
    g.local.settings_open = true;
    // Let the window settle where it anchors, then press "120 fps".
    g.run(raw(1280.0, 720.0, vec![]), &m);
    let intents = click(&mut g, &m, egui::pos2(1055.0, 194.0));
    assert!(intents.contains(&UiIntent::Setting(SettingPick::FrameRate(120))), "{intents:?}");
    let intents = click(&mut g, &m, egui::pos2(1203.0, 135.0));
    assert!(intents.contains(&UiIntent::Setting(SettingPick::Quality(40_000_000))), "{intents:?}");
}

fn gpu_image(m: &Model, w: u32, h: u32, setup: impl FnOnce(&mut Gui)) -> Option<Vec<u8>> {
    let g = sill_app::gpu::Gpu::headless().ok()?;
    let fmt = wgpu::TextureFormat::Rgba8Unorm;
    let renderer = sill_app::renderer::Renderer::new(g.device.clone(), g.queue.clone(), fmt);
    let mut gui = Gui::new();
    gui.init_renderer(&g.device, fmt);
    setup(&mut gui);
    // Two frames: egui settles sizes on the second.
    let first = gui.run(raw(w as f32, h as f32, vec![]), m);
    let out = gui.run(raw(w as f32, h as f32, vec![]), m);
    Some(renderer.render_to_image_with((w, h), 1.0, None, &mut |v| {
        // The first frame's texture uploads were in the first run's delta: paint both so the font atlas exists.
        gui.paint(&g.device, &g.queue, v, (w, h), &first);
        gui.paint(&g.device, &g.queue, v, (w, h), &out);
    }))
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn the_panels_render_offscreen() {
    let m = connected_model();
    let Some(img) = gpu_image(&m, 1280, 720, |g| {
        g.local.settings_open = true;
        g.local.apps_open = true;
    }) else {
        eprintln!("no GPU adapter: skipping the offscreen render");
        return;
    };
    let lit = img.chunks_exact(4).filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 120).count();
    assert!(lit > 2000, "text and widgets are drawn ({lit} lit pixels)");
    if let Some(dir) = std::env::var_os("SILL_UI_DUMP") {
        let d = std::path::Path::new(&dir);
        sill_app::app::write_png(&d.join("panels.png"), (1280, 720), &img).unwrap();
        let mut idle = Model::default();
        idle.macs = vec![
            sill_ui_state::MacRow { key: "a".into(), address: Some("192.168.1.5:47000".into()), open_door: false, name: "Studio".into(), word: "Paired".into(), saved: true, listed: true, dialable: true, needs_pairing: false },
            sill_ui_state::MacRow { key: "b".into(), address: Some("192.168.1.6:47000".into()), open_door: false, name: "Old Mac".into(), word: "Update Sill on the Mac".into(), saved: false, listed: true, dialable: false, needs_pairing: false },
            sill_ui_state::MacRow { key: "c".into(), address: None, open_door: false, name: "Mini".into(), word: "Not paired".into(), saved: false, listed: true, dialable: true, needs_pairing: true },
        ];
        idle.pairing.error = Some("Nothing answered at 10.0.0.9. Check the address, and that Sill is open on your Mac.".into());
        let img = gpu_image(&idle, 1000, 640, |_| {}).unwrap();
        sill_app::app::write_png(&d.join("macs.png"), (1000, 640), &img).unwrap();
        let mut locked = connected_model();
        locked.cover = Some(sill_ui_state::Cover { title: "Another user started controlling this Mac.".into(), detail: Some("Log in to your account to reactivate screen sharing with Sill.".into()), shade: sill_ui_state::Shade::Locked(sill_ui_state::Why::OtherUser) });
        let img = gpu_image(&locked, 1000, 640, |_| {}).unwrap();
        sill_app::app::write_png(&d.join("locked.png"), (1000, 640), &img).unwrap();
    }
}

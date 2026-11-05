//! `sill --connect HOST:PORT` or `sill --mock`. See `sill --help`.

use sill_app::app::{App, Options};
use sill_app::client::{self, Target};
use sill_app::headless;
use sill_app::seam::{UiEvent, UiSink};
use sill_media::DecoderPref;
use std::sync::Arc;
use std::time::Duration;
use winit::event_loop::EventLoop;

const HELP: &str = "\
sill - stream a Mac's windows

  sill                                no arguments: list the Macs on the network, pair and connect
  sill --connect HOST:PORT [--tofu]   a saved Mac; --tofu trusts the key of a Mac that lets any device in
  sill --mock                         an in-process test host streaming a generated clip

options:
  --decoder auto|sw|vaapi   (or SILL_DECODER)
  (SILL_ZEROCOPY=0 turns off the VAAPI -> Vulkan dmabuf path and uses the CPU upload)
  --hud                     show the debug overlay (toggle with F3; or SILL_HUD=1)
  --exit-after N            quit after N pictures were shown
  --dump FILE.png           with --exit-after: save the last picture as drawn
  --headless                no window: decode, render offscreen (needs --exit-after N)
  --corrupt N               with --mock: cut the N-th frame short once (the keyframe recovery path)
";

struct Args {
    target: Option<Target>,
    pref: DecoderPref,
    hud: bool,
    exit_after: Option<u64>,
    dump: Option<std::path::PathBuf>,
    headless: bool,
}

fn parse() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let (mut connect, mut mock, mut tofu, mut corrupt) = (None, false, false, None);
    let mut a = Args { target: None, pref: DecoderPref::from_env(), hud: std::env::var_os("SILL_HUD").is_some_and(|v| v != "0"), exit_after: None, dump: None, headless: false };
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--connect" => connect = Some(val("--connect")?),
            "--mock" => mock = true,
            "--tofu" => tofu = true,
            "--decoder" => a.pref = DecoderPref::parse(&val("--decoder")?).ok_or("--decoder takes auto, sw or vaapi")?,
            "--hud" => a.hud = true,
            "--exit-after" => a.exit_after = Some(val("--exit-after")?.parse().map_err(|_| "--exit-after takes a number")?),
            "--dump" => a.dump = Some(val("--dump")?.into()),
            "--headless" => a.headless = true,
            "--corrupt" => corrupt = Some(val("--corrupt")?.parse().map_err(|_| "--corrupt takes a number")?),
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    a.target = match (connect, mock) {
        (None, false) => None,
        (Some(c), false) => {
            let (h, p) = sill_net::addr::parse(&c).map(|x| (x.host, x.port.unwrap_or(0))).map_err(|_| "--connect takes HOST:PORT")?;
            if p == 0 {
                return Err("--connect takes HOST:PORT".into());
            }
            Some(Target::Address { host: h, port: p, tofu })
        }
        (None, true) => Some(Target::Mock { corrupt_frame: corrupt }),
        _ => return Err("give at most one of --connect and --mock".into()),
    };
    if a.headless && a.target.is_none() {
        return Err("--headless needs --connect or --mock".into());
    }
    if a.headless && a.exit_after.is_none() {
        return Err("--headless needs --exit-after N".into());
    }
    Ok(a)
}

fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("sill: {e}\n");
            }
            eprint!("{HELP}");
            std::process::exit(if e.is_empty() { 0 } else { 2 });
        }
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
    if args.headless {
        let opts = headless::Options { frames: args.exit_after.unwrap(), timeout: Duration::from_secs(30), pref: args.pref, panel: (1280, 720), render: true };
        match headless::run(&rt, args.target.clone().expect("--headless needs --connect or --mock"), opts) {
            Ok((rep, _host)) => {
                println!("{rep:?}");
                if let (Some(p), Some((size, img))) = (&args.dump, &rep.image) {
                    if let Err(e) = sill_app::app::write_png(p, *size, img) {
                        eprintln!("sill: {e}");
                    }
                }
            }
            Err(e) => {
                eprintln!("sill: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let event_loop = match EventLoop::<UiEvent>::with_user_event().build() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("sill: no window system: {e}");
            std::process::exit(1);
        }
    };
    let proxy = event_loop.create_proxy();
    let ui: UiSink = Arc::new(move |e| {
        let _ = proxy.send_event(e);
    });
    let mut label = "Sill".to_string();
    let (mut session, mut mock, mut current) = (None, None, None);
    let mock_run = matches!(args.target, Some(Target::Mock { .. }));
    if let Some(target) = args.target {
        eprintln!("connecting...");
        match rt.block_on(client::connect(target)) {
            Ok(c) => {
                label = format!("Sill - {}", c.label);
                session = Some(c.session);
                current = c.mac_id;
                mock = c.mock;
            }
            Err(e) => {
                eprintln!("sill: {e}");
                std::process::exit(1);
            }
        }
    }
    let _mock = mock;
    // A mock Mac has no record to redial; everything else gets the saved Macs, Bonjour and the reconnect ladder.
    let link = if mock_run {
        None
    } else {
        match client::real_link(current) {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("sill: {e}");
                None
            }
        }
    };
    let c = client::start_with(rt.handle(), session, link, args.pref, ui, false);
    let mut app = App::new(c, Options { hud: args.hud, exit_after: args.exit_after, dump: args.dump, title: label });
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("sill: {e}");
        std::process::exit(1);
    }
    if let Some(e) = app.error() {
        eprintln!("sill: {e}");
        std::process::exit(1);
    }
}

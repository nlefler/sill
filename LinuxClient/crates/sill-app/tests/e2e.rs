//! The whole path against the in-process mock host: connect, request the Desktop, decode the stream, draw
//! it offscreen (when a GPU exists), recover from a damaged frame by asking for a keyframe.
use sill_app::client::Target;
use sill_app::headless::{self, Options};
use sill_media::DecoderPref;
use sill_proto::{decode_json, Kind, StreamSource, Viewport};
use std::time::Duration;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap()
}

fn opts(frames: u64, pref: DecoderPref) -> Options {
    Options { frames, timeout: Duration::from_secs(30), pref, panel: (960, 540), render: true }
}

#[test]
fn streams_decodes_and_draws_n_frames() {
    for pref in [DecoderPref::Software, DecoderPref::Auto] {
        let rt = rt();
        let (rep, host) = headless::run(&rt, Target::Mock { corrupt_frame: None }, opts(150, pref)).unwrap();
        let host = host.unwrap();
        eprintln!("{pref:?}: presented {} rendered {} decoder {} gpu {:?} max age {} ms", rep.presented, rep.rendered, rep.decoder, rep.gpu, rep.max_age_ms);
        assert!(rep.presented >= 150, "presented {} ({:?})", rep.presented, rep.closed);
        assert_eq!(rep.last_size, Some((1280, 720)));
        assert!(rep.cursor_seen && rep.pointer_seen, "cursor kinds 14 and 26 arrive");
        if rep.gpu.is_some() {
            assert!(rep.rendered >= 150);
            let ((w, h), img) = rep.image.as_ref().unwrap();
            // testsrc has colour bars along the top: the picture is not black and not flat.
            let lit = img.chunks_exact(4).filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 60).count();
            assert!(lit > (w * h / 4) as usize, "picture drawn ({lit} lit pixels)");
        }
        // What the client said to the host.
        let got = host.received();
        let kinds: Vec<Kind> = got.iter().map(|m| m.kind).collect();
        assert_eq!(kinds[0], Kind::Hello);
        let sel = got.iter().find(|m| m.kind == Kind::SelectSource).expect("asked for the Desktop");
        assert_eq!(decode_json::<StreamSource>(&sel.payload), Some(StreamSource::Desktop));
        let vp: Viewport = decode_json(&got.iter().find(|m| m.kind == Kind::Viewport).expect("sent a viewport").payload).unwrap();
        assert_eq!((vp.width, vp.height, vp.fps, vp.local_cursor), (960.0, 540.0, Some(60), Some(true)));
        assert!(!kinds.contains(&Kind::KeyframeRequest), "a clean stream asks for no keyframe");
        assert_eq!(got.iter().filter(|m| m.kind == Kind::SelectSource).count(), 1);
    }
}

#[test]
fn a_damaged_frame_flushes_asks_for_a_keyframe_and_the_picture_comes_back() {
    let rt = rt();
    let (rep, host) = headless::run(&rt, Target::Mock { corrupt_frame: Some(10) }, opts(200, DecoderPref::Software)).unwrap();
    let host = host.unwrap();
    assert!(rep.presented >= 200, "presented {} ({:?})", rep.presented, rep.closed);
    let asks = host.received_kinds().iter().filter(|k| **k == Kind::KeyframeRequest).count();
    assert_eq!(asks, 1, "one ask for one interruption");
}

#[test]
fn an_unknown_saved_mac_is_refused_with_words() {
    let rt = rt();
    let e = rt.block_on(sill_app::client::connect(Target::Address { host: "127.0.0.1".into(), port: 9, tofu: false }));
    match e {
        Err(m) => assert!(m.contains("pair") || m.contains("data directory"), "{m}"),
        Ok(_) => panic!("connected to nothing"),
    }
}

#[test]
fn the_input_crates_letterbox_is_the_renderers() {
    for (w, h, fw, fh) in [(1000.0, 500.0, 1600.0, 900.0), (500.0, 900.0, 1280.0, 720.0), (800.0, 800.0, 800.0, 800.0), (0.0, 5.0, 1.0, 1.0)] {
        let a = sill_input::video_rect(w, h, fw, fh);
        let b = sill_media::cursor::video_rect(w, h, fw, fh);
        assert_eq!((a.x, a.y, a.w, a.h), (b.x, b.y, b.w, b.h));
    }
}

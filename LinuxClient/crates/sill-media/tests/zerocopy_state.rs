//! The decoder follows the process-wide zero-copy state: dmabufs while it is Active, pictures read back
//! to memory as soon as it is Off (what the renderer sets when an import fails). Skips without VAAPI.
use sill_media::zerocopy::{self as zc, State};
use sill_media::{clip, Decoder, DecoderPref};

#[test]
fn decoder_exports_while_active_and_reads_back_when_off() {
    let Ok(mut d) = Decoder::new(DecoderPref::Vaapi) else {
        return eprintln!("SKIP: no VAAPI");
    };
    let c = clip::embedded_720p();
    d.set_parameter_sets(&c.params);
    zc::set_state(State::Active);
    let first = d.decode(&c.frames[0].payload, 0.0, true).frame.expect("first picture");
    if first.prime.is_none() {
        return eprintln!("SKIP: the driver did not export a dmabuf ({})", d.path_name());
    }
    assert!(first.y.is_empty() && first.uv.is_empty());
    assert_eq!((first.width, first.height), (1280, 720));
    assert!(d.path_name().ends_with("zero-copy dmabuf to Vulkan"), "{}", d.path_name());
    let mut held = vec![];
    // The budget: with MAX_LIVE_PICTURES held, the next picture is not exported (and is not shown).
    held.push(first);
    for (i, f) in c.frames.iter().enumerate().skip(1) {
        match d.decode(&f.payload, i as f64 / 60.0, f.keyframe).frame {
            Some(p) => {
                assert!(p.prime.is_some());
                held.push(p);
            }
            None => break,
        }
        if held.len() > zc::MAX_LIVE_PICTURES + 1 {
            panic!("exported more pictures than the budget allows");
        }
    }
    assert_eq!(held.len(), zc::MAX_LIVE_PICTURES, "the budget stops the export");
    held.clear();
    zc::set_state(State::Off);
    // A fresh keyframe, read back now.
    let p = d.decode(&c.frames[0].payload, 9.0, true).frame.expect("picture");
    assert!(p.prime.is_none() && !p.y.is_empty());
    assert!(d.path_name().contains("read back"), "{}", d.path_name());
    zc::set_state(State::Unknown);
}

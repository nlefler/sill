//! `bench FILE.hevc [sw|vaapi|auto] [loops] [nodl|prime]`: decode an Annex B clip as fast as possible, print fps.
//! Frames are converted to the wire's shape first, so the timing covers what the app does per frame.
use sill_media::clip::parse_annex_b;
use sill_media::{Decoder, DecoderPref};
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&a[1]).expect("read clip");
    let pref = a.get(2).and_then(|s| DecoderPref::parse(s)).unwrap_or(DecoderPref::Auto);
    let loops: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
    let clip = parse_annex_b(&data).expect("parse");
    let mut d = match Decoder::new(pref) {
        Ok(d) => d,
        Err(e) => {
            println!("{}x{} {pref:?}: unavailable ({e})", clip.width, clip.height);
            return;
        }
    };
    d.set_parameter_sets(&clip.params);
    let nodl = a.get(4).is_some_and(|s| s == "nodl");
    // "prime": export each picture as a dmabuf (what zero-copy does on the decode side) and drop it.
    let prime = a.get(4).is_some_and(|s| s == "prime");
    if prime {
        sill_media::zerocopy::set_state(sill_media::zerocopy::State::Active);
    }
    d.set_discard_output(nodl);
    let mut n = 0u64;
    let mut errs = 0;
    let t = Instant::now();
    for _ in 0..loops {
        for (i, f) in clip.frames.iter().enumerate() {
            let r = d.decode(&f.payload, i as f64 / 30.0, f.keyframe);
            if r.frame.is_some() || (nodl && r.interruption.is_none()) {
                n += 1;
            }
            if let Some(e) = r.interruption {
                errs += 1;
                if errs == 1 {
                    eprintln!("interruption: {e}");
                }
            }
        }
    }
    let s = t.elapsed().as_secs_f64();
    println!(
        "{}x{} {:?}{}: {} frames in {:.2}s = {:.1} fps ({} errors{})",
        clip.width, clip.height, d.kind(), if nodl { " (read-back every 30th)" } else if prime { " (dmabuf export)" } else { "" }, n, s, n as f64 / s, errs,
        d.fallback_reason().map(|r| format!(", fallback: {r}")).unwrap_or_default()
    );
}

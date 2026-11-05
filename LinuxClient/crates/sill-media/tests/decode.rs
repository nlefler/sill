//! Decoder tests over the embedded clip. VAAPI cases skip with a message when the device or driver
//! is missing; software cases always run.
use sill_media::clip::embedded_720p;
use sill_media::*;

fn run(pref: DecoderPref) -> Option<(Decoder, usize, VideoFrame)> {
    let clip = embedded_720p();
    let mut d = match Decoder::new(pref) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("SKIP {pref:?}: {e}");
            return None;
        }
    };
    d.set_parameter_sets(&clip.params);
    let (mut n, mut last) = (0, None);
    for (i, f) in clip.frames.iter().enumerate() {
        let r = d.decode(&f.payload, i as f64 / 30.0, f.keyframe);
        assert!(r.interruption.is_none(), "frame {i}: {:?}", r.interruption);
        if let Some(fr) = r.frame {
            n += 1;
            last = Some(fr);
        }
    }
    Some((d, n, last?))
}

fn check_picture(f: &VideoFrame) {
    assert_eq!((f.width, f.height), (1280, 720));
    assert_eq!(f.y.len(), 1280 * 720);
    assert_eq!(f.uv.len(), 640 * 360 * 2);
    assert_eq!(f.color, ColorInfo { matrix: Matrix::Bt709, full_range: false });
    // testsrc is not black, not flat.
    let min = f.y.iter().min().unwrap();
    let max = f.y.iter().max().unwrap();
    assert!(*max > *min + 100, "{min}..{max}");
}

#[test]
fn software_decodes_every_frame_in_order() {
    let (d, n, last) = run(DecoderPref::Software).unwrap();
    assert_eq!(d.kind(), DecoderKind::Software);
    assert_eq!(n, 60, "one picture per packet: no reorder delay");
    check_picture(&last);
    assert!((last.timestamp - 59.0 / 30.0).abs() < 1e-6);
}

#[test]
fn vaapi_decodes_the_same_or_skips() {
    let Some((d, n, last)) = run(DecoderPref::Vaapi) else { return };
    assert_eq!(d.kind(), DecoderKind::Vaapi);
    assert_eq!(n, 60);
    check_picture(&last);
}

#[test]
fn vaapi_and_software_agree_closely() {
    let (Some((_, _, a)), Some((_, _, b))) = (run(DecoderPref::Software), run(DecoderPref::Vaapi)) else { return };
    let diff: u64 = a.y.iter().zip(&b.y).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64).sum();
    assert!((diff as f64 / a.y.len() as f64) < 1.0, "mean luma difference {}", diff as f64 / a.y.len() as f64);
}

#[test]
fn auto_always_yields_a_decoder() {
    let (d, n, _) = run(DecoderPref::Auto).unwrap();
    eprintln!("auto chose {:?} (fallback: {:?})", d.kind(), d.fallback_reason());
    assert_eq!(n, 60);
}

#[test]
fn delta_before_any_keyframe_interrupts_once_and_drops_until_a_keyframe() {
    let clip = embedded_720p();
    let mut d = Decoder::new(DecoderPref::Software).unwrap();
    d.set_parameter_sets(&clip.params);
    let r = d.decode(&clip.frames[1].payload, 0.0, false);
    assert!(r.interruption.is_some() && r.frame.is_none() && r.dropped);
    let r = d.decode(&clip.frames[2].payload, 0.0, false);
    assert!(r.interruption.is_none() && r.dropped, "reported once per loss");
    let r = d.decode(&clip.frames[30].payload, 1.0, true);
    assert!(r.frame.is_some() && r.picture_back && r.interruption.is_none());
    let r = d.decode(&clip.frames[31].payload, 1.03, false);
    assert!(r.frame.is_some() && !r.picture_back && !r.dropped);
}

#[test]
fn garbage_and_missing_sets_flush_and_report() {
    let clip = embedded_720p();
    let mut d = Decoder::new(DecoderPref::Software).unwrap();
    let r = d.decode(&clip.frames[0].payload, 0.0, true);
    assert!(r.interruption.is_some(), "no parameter sets yet");
    d.set_parameter_sets(&clip.params);
    assert!(d.decode(&clip.frames[0].payload, 0.0, true).frame.is_some());
    // A frame whose NAL lengths are wrong.
    let r = d.decode(&[0, 0, 0, 50, 1, 2, 3], 0.1, false);
    assert!(r.interruption.is_some() && r.frame.is_none());
    assert!(d.is_waiting_for_keyframe());
    assert!(d.decode(&clip.frames[1].payload, 0.2, false).dropped);
    let r = d.decode(&clip.frames[30].payload, 0.3, true);
    assert!(r.picture_back && r.frame.is_some());
}

#[test]
fn a_truncated_frame_never_panics_and_recovers() {
    let clip = embedded_720p();
    let mut d = Decoder::new(DecoderPref::Software).unwrap();
    d.set_parameter_sets(&clip.params);
    d.decode(&clip.frames[0].payload, 0.0, true);
    let f = &clip.frames[1].payload;
    let _ = d.decode(&f[..f.len() / 2], 0.1, false);
    let r = d.decode(&clip.frames[30].payload, 1.0, true);
    assert!(r.frame.is_some());
}

#[test]
fn hvcc_extradata_is_accepted_by_ffmpeg_with_length_prefixed_packets() {
    use ffmpeg_next as ff;
    let clip = embedded_720p();
    let extra = sill_media::nal::hvcc_extradata(&clip.params).unwrap();
    ff::init().unwrap();
    let codec = ff::decoder::find(ff::codec::Id::HEVC).unwrap();
    let mut ctx = ff::codec::Context::new_with_codec(codec);
    unsafe {
        let p = ctx.as_mut_ptr();
        let buf = ffmpeg_next::ffi::av_mallocz(extra.len() + 64) as *mut u8;
        std::ptr::copy_nonoverlapping(extra.as_ptr(), buf, extra.len());
        (*p).extradata = buf;
        (*p).extradata_size = extra.len() as i32;
    }
    let mut dec = ctx.decoder().video().unwrap();
    dec.send_packet(&ff::Packet::copy(&clip.frames[0].payload)).unwrap();
    let mut f = ff::frame::Video::empty();
    dec.receive_frame(&mut f).unwrap();
    assert_eq!((f.width(), f.height()), (1280, 720));
}

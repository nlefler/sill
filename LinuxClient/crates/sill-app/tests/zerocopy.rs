//! VAAPI -> Vulkan zero-copy on this machine: a picture decoded by VAAPI, exported as a dmabuf and
//! imported by the Vulkan device must draw the same as the same picture decoded in software and uploaded
//! from memory. Skips (with a message) where there is no VAAPI, no Vulkan, or no dmabuf import.
use sill_app::gpu::Gpu;
use sill_app::renderer::Renderer;
use sill_media::zerocopy as zc;
use sill_media::{clip, Decoder, DecoderPref};

#[test]
fn imported_dmabuf_draws_like_the_uploaded_picture() {
    let gpu = match Gpu::request_with(Gpu::instance(), None, true) {
        Ok(g) => g,
        Err(e) => return eprintln!("SKIP zero-copy test: {e}"),
    };
    let Some(importer) = gpu.importer.clone() else {
        return eprintln!("SKIP zero-copy test: {}", gpu.zero_copy_note);
    };
    let Ok(mut hw) = Decoder::new(DecoderPref::Vaapi) else {
        return eprintln!("SKIP zero-copy test: no VAAPI");
    };
    zc::set_state(zc::State::Active);
    let c = clip::embedded_720p();
    let mut sw = Decoder::new(DecoderPref::Software).expect("software decoder");
    hw.set_parameter_sets(&c.params);
    sw.set_parameter_sets(&c.params);
    let mk = || {
        let mut r = Renderer::new(gpu.device.clone(), gpu.queue.clone(), wgpu::TextureFormat::Rgba8Unorm);
        r.set_importer(Some(importer.clone()));
        r
    };
    let (mut r_zc, mut r_cpu) = (mk(), mk());
    let size = (1280, 720);
    let mut compared = 0;
    for (i, f) in c.frames.iter().enumerate().take(40) {
        let a = hw.decode(&f.payload, i as f64 / 60.0, f.keyframe).frame.expect("vaapi frame");
        let b = sw.decode(&f.payload, i as f64 / 60.0, f.keyframe).frame.expect("software frame");
        if i == 0 && a.prime.is_none() {
            return eprintln!("SKIP zero-copy test: the decoder did not export a dmabuf ({})", hw.path_name());
        }
        assert!(a.prime.is_some(), "frame {i} was read back");
        assert!(r_zc.upload(&a), "frame {i}: import failed");
        assert!(r_cpu.upload(&b));
        let (ia, ib) = (r_zc.render_to_image(size, 1.0, None), r_cpu.render_to_image(size, 1.0, None));
        let worst = ia.iter().zip(&ib).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap();
        assert!(worst <= 2, "frame {i}: zero-copy and CPU upload differ by {worst}");
        compared += 1;
    }
    assert_eq!(compared, 40);
    assert!(hw.zero_copy_active());
    assert!(hw.path_name().contains("zero-copy"), "{}", hw.path_name());
    // Everything dropped: no exported picture stays alive (the renderers hold at most the last one).
    drop((r_zc, r_cpu));
    let _ = gpu.device.poll(wgpu::PollType::Wait);
    zc::set_state(zc::State::Unknown);
}

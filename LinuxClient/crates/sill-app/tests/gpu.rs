//! The renderer on a real Vulkan device, offscreen. Skips with a message when there is no adapter.
use sill_app::gpu::Gpu;
use sill_app::renderer::Renderer;
use sill_media::cursor::CursorImage;
use sill_media::{ColorInfo, Matrix, VideoFrame};

fn renderer() -> Option<Renderer> {
    match Gpu::headless() {
        Ok(g) => {
            eprintln!("GPU test on {}", g.describe());
            Some(Renderer::new(g.device.clone(), g.queue.clone(), wgpu::TextureFormat::Rgba8Unorm))
        }
        Err(e) => {
            eprintln!("SKIP gpu test: {e}");
            None
        }
    }
}

/// RGB (0..1) to 8-bit Y, Cb, Cr under `kr`, `kb`.
fn encode(rgb: [f32; 3], kr: f32, kb: f32, full: bool) -> (u8, u8, u8) {
    let kg = 1.0 - kr - kb;
    let y = kr * rgb[0] + kg * rgb[1] + kb * rgb[2];
    let cb = (rgb[2] - y) / (2.0 * (1.0 - kb));
    let cr = (rgb[0] - y) / (2.0 * (1.0 - kr));
    let q = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    if full {
        (q(y * 255.0), q(cb * 255.0 + 128.0), q(cr * 255.0 + 128.0))
    } else {
        (q(16.0 + 219.0 * y), q(128.0 + 224.0 * cb), q(128.0 + 224.0 * cr))
    }
}

fn solid(w: u32, h: u32, yuv: (u8, u8, u8), color: ColorInfo) -> VideoFrame {
    VideoFrame {
        width: w,
        height: h,
        y: vec![yuv.0; (w * h) as usize],
        uv: [yuv.1, yuv.2].repeat((w.div_ceil(2) * h.div_ceil(2)) as usize),
        timestamp: 0.0,
        color,
        prime: None,
    }
}

fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * w + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2]]
}

fn near(a: [u8; 3], b: [u8; 3], tol: i32) -> bool {
    a.iter().zip(&b).all(|(x, y)| (*x as i32 - *y as i32).abs() <= tol)
}

#[test]
fn yuv_to_rgb_matches_the_matrices_and_ranges() {
    let Some(mut r) = renderer() else { return };
    let colors: [[f32; 3]; 7] = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.], [1., 1., 1.], [0., 0., 0.], [0.5, 0.5, 0.5], [0.8, 0.4, 0.1]];
    for (matrix, kr, kb) in [(Matrix::Bt709, 0.2126, 0.0722), (Matrix::Bt601, 0.299, 0.114), (Matrix::Bt2020, 0.2627, 0.0593)] {
        for full in [false, true] {
            for c in colors {
                let f = solid(32, 32, encode(c, kr, kb, full), ColorInfo { matrix, full_range: full });
                r.upload(&f);
                let img = r.render_to_image((32, 32), 1.0, None);
                let want = [(c[0] * 255.0).round() as u8, (c[1] * 255.0).round() as u8, (c[2] * 255.0).round() as u8];
                let got = px(&img, 32, 16, 16);
                assert!(near(got, want, 4), "{matrix:?} full={full} {c:?}: want {want:?} got {got:?}");
            }
        }
    }
}

#[test]
fn limited_range_black_and_white_are_exact_ends() {
    let Some(mut r) = renderer() else { return };
    r.upload(&solid(16, 16, (16, 128, 128), ColorInfo::default()));
    assert_eq!(px(&r.render_to_image((16, 16), 1.0, None), 16, 8, 8), [0, 0, 0]);
    r.upload(&solid(16, 16, (235, 128, 128), ColorInfo::default()));
    assert!(near(px(&r.render_to_image((16, 16), 1.0, None), 16, 8, 8), [255, 255, 255], 1));
    // Below black and above white clamp, they do not wrap.
    r.upload(&solid(16, 16, (0, 128, 128), ColorInfo::default()));
    assert_eq!(px(&r.render_to_image((16, 16), 1.0, None), 16, 8, 8), [0, 0, 0]);
    r.upload(&solid(16, 16, (255, 128, 128), ColorInfo::default()));
    assert_eq!(px(&r.render_to_image((16, 16), 1.0, None), 16, 8, 8), [255, 255, 255]);
}

#[test]
fn the_picture_is_letterboxed_aspect_fit() {
    let Some(mut r) = renderer() else { return };
    // A 2:1 white picture in a 64x64 window: 64x32 in the middle, black above and below.
    r.upload(&solid(64, 32, (235, 128, 128), ColorInfo::default()));
    let img = r.render_to_image((64, 64), 1.0, None);
    assert_eq!(px(&img, 64, 32, 4), [0, 0, 0]);
    assert_eq!(px(&img, 64, 32, 59), [0, 0, 0]);
    assert!(near(px(&img, 64, 32, 32), [255, 255, 255], 1));
    assert!(near(px(&img, 64, 1, 17), [255, 255, 255], 1), "just inside the top-left corner of the picture");
    assert_eq!(px(&img, 64, 1, 14), [0, 0, 0], "just above it");
    // And a 1:2 picture is pillarboxed.
    r.upload(&solid(32, 64, (235, 128, 128), ColorInfo::default()));
    let img = r.render_to_image((64, 64), 1.0, None);
    assert_eq!(px(&img, 64, 4, 32), [0, 0, 0]);
    assert!(near(px(&img, 64, 32, 32), [255, 255, 255], 1));
}

#[test]
fn a_picture_that_changes_size_is_shown_at_its_new_size() {
    let Some(mut r) = renderer() else { return };
    r.upload(&solid(32, 32, (16, 128, 128), ColorInfo::default()));
    r.upload(&solid(48, 24, (235, 128, 128), ColorInfo::default()));
    assert_eq!(r.video_size(), Some((48, 24)));
    assert!(near(px(&r.render_to_image((48, 24), 1.0, None), 48, 24, 12), [255, 255, 255], 1));
}

#[test]
fn odd_sized_pictures_upload_without_trouble() {
    let Some(mut r) = renderer() else { return };
    r.upload(&solid(33, 17, (235, 128, 128), ColorInfo::default()));
    assert!(near(px(&r.render_to_image((33, 17), 1.0, None), 33, 16, 8), [255, 255, 255], 1));
}

#[test]
fn the_pointer_sprite_sits_with_its_hotspot_on_the_tip_and_blends() {
    let Some(mut r) = renderer() else { return };
    r.upload(&solid(64, 64, (16, 128, 128), ColorInfo::default())); // black picture
    // 4x4 points, opaque green, hotspot at (1,1); the window is at scale 2 (sprite is 8x8 px).
    let rgba: Vec<u8> = [0u8, 255, 0, 255].repeat(16);
    r.set_cursor(Some(CursorImage { width: 4, height: 4, rgba, hotspot: (1.0, 1.0), size: (4.0, 4.0) }));
    let img = r.render_to_image((64, 64), 2.0, Some((0.5, 0.5)));
    // The tip is at 32,32; the sprite covers 30..38.
    assert_eq!(px(&img, 64, 31, 31), [0, 255, 0]);
    assert_eq!(px(&img, 64, 37, 37), [0, 255, 0]);
    assert_eq!(px(&img, 64, 29, 29), [0, 0, 0]);
    assert_eq!(px(&img, 64, 38, 38), [0, 0, 0]);
    // No pointer, no sprite.
    assert_eq!(px(&r.render_to_image((64, 64), 2.0, None), 64, 32, 32), [0, 0, 0]);
    // A half-transparent sprite over white blends.
    r.upload(&solid(64, 64, (235, 128, 128), ColorInfo::default()));
    r.set_cursor(Some(CursorImage { width: 1, height: 1, rgba: vec![0, 0, 0, 128], hotspot: (0.0, 0.0), size: (8.0, 8.0) }));
    let g = px(&r.render_to_image((64, 64), 1.0, Some((0.5, 0.5))), 64, 34, 34);
    assert!((120..=135).contains(&g[0]), "{g:?}");
}

#[test]
fn the_hud_draws_in_the_top_left_corner() {
    let Some(mut r) = renderer() else { return };
    r.upload(&solid(64, 64, (235, 128, 128), ColorInfo::default()));
    let (w, h, rgba) = sill_app::hud::render_text(&["fps 60".into()], 1);
    r.set_hud(Some((w, h, rgba)));
    let img = r.render_to_image((128, 64), 1.0, None);
    // The HUD plate (translucent black) darkens the corner 8 px in; far from it the picture is untouched.
    let near_hud = px(&img, 128, 12, 12);
    assert!(near_hud[0] < 200, "{near_hud:?}");
    assert!(near(px(&img, 128, 100, 40), [255, 255, 255], 1) || px(&img, 128, 100, 40) == [0, 0, 0]);
    r.set_hud(None);
    let img = r.render_to_image((64, 64), 1.0, None);
    assert!(near(px(&img, 64, 12, 12), [255, 255, 255], 1));
}

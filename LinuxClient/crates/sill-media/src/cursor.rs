//! The Mac's cursor (kind 14) and pointer (kind 26) as the renderer needs them: the shape decoded to
//! RGBA, the letterboxed video rectangle, and where the sprite's top-left goes in the window.

use sill_proto::{CursorShape, MacPointer};

/// A decoded cursor image with its hotspot and size in logical points (the Mac's own cursor points).
#[derive(Clone, Debug, PartialEq)]
pub struct CursorImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Hotspot in points, from the sprite's top-left.
    pub hotspot: (f32, f32),
    /// Size in points; the PNG is usually 2x that.
    pub size: (f32, f32),
}

/// Decode a kind-14 payload's PNG to straight 8-bit RGBA. `None` for a PNG that does not decode or a
/// shape with nonsense numbers.
pub fn decode_cursor(shape: &CursorShape) -> Option<CursorImage> {
    let ok = |v: f32| v.is_finite() && (0.0..=1024.0).contains(&v);
    if !(ok(shape.hotspot.0) && ok(shape.hotspot.1) && ok(shape.size.0) && ok(shape.size.1)) || shape.size.0 < 1.0 || shape.size.1 < 1.0 {
        return None;
    }
    let mut dec = png::Decoder::new(std::io::Cursor::new(&shape.png));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w == 0 || h == 0 || w > 1024 || h > 1024 {
        return None;
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => data.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::Indexed => return None, // EXPAND turns these into Rgb(a)
    };
    if rgba.len() != (w * h * 4) as usize {
        return None;
    }
    Some(CursorImage { width: w, height: h, rgba, hotspot: shape.hotspot, size: shape.size })
}

/// The arrow drawn before the Mac sends a shape: a white-outlined black arrow, 12 x 19 points.
pub fn fallback_arrow() -> CursorImage {
    const ART: [&str; 19] = [
        "X...........", "XX..........", "XOX.........", "XOOX........", "XOOOX.......", "XOOOOX......", "XOOOOOX.....",
        "XOOOOOOX....", "XOOOOOOOX...", "XOOOOOOOOX..", "XOOOOOXXXXX.", "XOOXOOX.....", "XOX.XOOX....", "XX..XOOX....",
        "X....XOOX...", ".....XOOX...", "......XOOX..", "......XOOX..", ".......XX...",
    ];
    let (w, h) = (12u32, 19u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for (y, row) in ART.iter().enumerate() {
        for (x, c) in row.bytes().enumerate() {
            let px = match c {
                b'X' => [0, 0, 0, 255],
                b'O' => [255, 255, 255, 255],
                _ => continue,
            };
            let i = (y * w as usize + x) * 4;
            rgba[i..i + 4].copy_from_slice(&px);
        }
    }
    CursorImage { width: w, height: h, rgba, hotspot: (0.0, 0.0), size: (w as f32, h as f32) }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The video aspect-fit in a window, centred: the letterboxed rectangle. Empty when either is.
pub fn video_rect(win_w: f64, win_h: f64, frame_w: f64, frame_h: f64) -> Rect {
    if !(win_w > 0.0 && win_h > 0.0 && frame_w > 0.0 && frame_h > 0.0) {
        return Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
    }
    let s = (win_w / frame_w).min(win_h / frame_h);
    let (w, h) = (frame_w * s, frame_h * s);
    Rect { x: (win_w - w) / 2.0, y: (win_h - h) / 2.0, w, h }
}

/// Where the Mac's pointer tip is in the window (pixels), or `None` when it is off the stream or the
/// report is not usable.
pub fn pointer_in_window(p: &MacPointer, rect: Rect) -> Option<(f64, f64)> {
    let (x, y) = p.position()?;
    if !(0.0..1.0).contains(&x) || !(0.0..1.0).contains(&y) || rect.w <= 0.0 {
        return None;
    }
    Some((rect.x + x * rect.w, rect.y + y * rect.h))
}

/// The sprite's top-left in window pixels for a tip at `tip`, its hotspot in points and the scale
/// factor (pixels per point).
pub fn sprite_origin(tip: (f64, f64), img: &CursorImage, scale_factor: f64) -> (f64, f64) {
    (tip.0 - img.hotspot.0 as f64 * scale_factor, tip.1 - img.hotspot.1 as f64 * scale_factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(color: png::ColorType, w: u32, h: u32, data: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(color);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(data).unwrap();
        out
    }

    #[test]
    fn decodes_rgba_rgb_and_gray_alpha() {
        let shape = |png| CursorShape { hotspot: (1.0, 2.0), size: (2.0, 1.0), png };
        let c = decode_cursor(&shape(png_bytes(png::ColorType::Rgba, 2, 1, &[1, 2, 3, 4, 5, 6, 7, 8]))).unwrap();
        assert_eq!((c.width, c.height, c.rgba.clone()), (2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]));
        assert_eq!(c.hotspot, (1.0, 2.0));
        let c = decode_cursor(&shape(png_bytes(png::ColorType::Rgb, 2, 1, &[1, 2, 3, 4, 5, 6]))).unwrap();
        assert_eq!(c.rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        let c = decode_cursor(&shape(png_bytes(png::ColorType::GrayscaleAlpha, 2, 1, &[9, 100, 8, 0]))).unwrap();
        assert_eq!(c.rgba, vec![9, 9, 9, 100, 8, 8, 8, 0]);
    }

    #[test]
    fn bad_shapes_are_none() {
        let good = png_bytes(png::ColorType::Rgba, 1, 1, &[0, 0, 0, 0]);
        let mk = |h: (f32, f32), s: (f32, f32), png: Vec<u8>| CursorShape { hotspot: h, size: s, png };
        assert!(decode_cursor(&mk((0.0, 0.0), (1.0, 1.0), good.clone())).is_some());
        assert!(decode_cursor(&mk((0.0, 0.0), (1.0, 1.0), vec![1, 2, 3])).is_none());
        assert!(decode_cursor(&mk((f32::NAN, 0.0), (1.0, 1.0), good.clone())).is_none());
        assert!(decode_cursor(&mk((0.0, 0.0), (0.0, 1.0), good.clone())).is_none());
        assert!(decode_cursor(&mk((0.0, 0.0), (1e9, 1.0), good.clone())).is_none());
        assert!(decode_cursor(&mk((0.0, 0.0), (1.0, 1.0), good[..20].to_vec())).is_none());
    }

    #[test]
    fn fallback_arrow_has_tip_at_origin() {
        let a = fallback_arrow();
        assert_eq!(a.rgba.len(), (a.width * a.height * 4) as usize);
        assert_eq!(&a.rgba[..4], &[0, 0, 0, 255]);
        assert_eq!(a.hotspot, (0.0, 0.0));
    }

    #[test]
    fn letterbox_rect_and_pointer() {
        let r = video_rect(1000.0, 1000.0, 1920.0, 1080.0);
        assert!((r.w - 1000.0).abs() < 1e-6 && (r.h - 562.5).abs() < 1e-6 && (r.y - 218.75).abs() < 1e-6 && r.x.abs() < 1e-6);
        let r2 = video_rect(1920.0, 540.0, 1920.0, 1080.0);
        assert!((r2.w - 960.0).abs() < 1e-9 && (r2.x - 480.0).abs() < 1e-9 && r2.y.abs() < 1e-6);
        assert_eq!(video_rect(0.0, 5.0, 1.0, 1.0).w, 0.0);
        let p = MacPointer { x: Some(0.5), y: Some(0.5), inside: Some(true), seen: None };
        let (px, py) = pointer_in_window(&p, r).unwrap();
        assert!((px - 500.0).abs() < 1e-6 && (py - 500.0).abs() < 1e-6);
        let off = MacPointer { x: Some(0.5), y: Some(0.5), inside: Some(false), seen: None };
        assert_eq!(pointer_in_window(&off, r), None);
        let edge = MacPointer { x: Some(1.0), y: Some(0.5), inside: Some(true), seen: None };
        assert_eq!(pointer_in_window(&edge, r), None);
        let nan = MacPointer { x: Some(f64::NAN), y: Some(0.5), inside: Some(true), seen: None };
        assert_eq!(pointer_in_window(&nan, r), None);
        assert_eq!(pointer_in_window(&p, Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }), None);
    }

    #[test]
    fn sprite_origin_puts_the_hotspot_on_the_tip() {
        let mut img = fallback_arrow();
        img.hotspot = (4.0, 6.0);
        assert_eq!(sprite_origin((100.0, 100.0), &img, 2.0), (92.0, 88.0));
    }
}

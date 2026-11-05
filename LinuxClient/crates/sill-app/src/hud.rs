//! The debug HUD's text as an RGBA image: an 8x8 bitmap font on a translucent plate. Pure.

use font8x8::legacy::BASIC_LEGACY;

/// `(width, height, rgba)` for `lines`, each glyph drawn `scale` times as large.
pub fn render_text(lines: &[String], scale: u32) -> (u32, u32, Vec<u8>) {
    let scale = scale.max(1);
    let pad = 4 * scale;
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).max(1) as u32;
    let w = cols * 8 * scale + 2 * pad;
    let h = lines.len().max(1) as u32 * 9 * scale + 2 * pad;
    let mut img = vec![0u8; (w * h * 4) as usize];
    for px in img.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 0, 0, 170]);
    }
    for (row, line) in lines.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            let glyph = BASIC_LEGACY.get(ch as usize).unwrap_or(&BASIC_LEGACY['?' as usize]);
            for (gy, bits) in glyph.iter().enumerate() {
                for gx in 0..8u32 {
                    if bits >> gx & 1 == 0 {
                        continue;
                    }
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let x = pad + (col as u32 * 8 + gx) * scale + sx;
                            let y = pad + (row as u32 * 9 + gy as u32) * scale + sy;
                            let i = ((y * w + x) * 4) as usize;
                            img[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                        }
                    }
                }
            }
        }
    }
    (w, h, img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_follows_text_and_glyphs_are_drawn() {
        let (w, h, img) = render_text(&["fps 60".into(), "x".into()], 2);
        assert_eq!((w, h), (6 * 8 * 2 + 16, 2 * 9 * 2 + 16));
        assert_eq!(img.len(), (w * h * 4) as usize);
        assert!(img.chunks_exact(4).any(|p| p == [255, 255, 255, 255]));
        assert_eq!(&img[..4], &[0, 0, 0, 170]);
    }

    #[test]
    fn empty_and_non_ascii_do_not_panic() {
        let (w, h, _) = render_text(&[], 1);
        assert!(w > 0 && h > 0);
        let _ = render_text(&["ünï ✓ 日本".into()], 1);
    }
}

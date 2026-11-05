/// Kind 3: windowID(4, BE) + JPEG. Needs at least one byte of image, as the Swift reader does.
pub fn decode_thumbnail(d: &[u8]) -> Option<(u32, &[u8])> {
    if d.len() <= 4 {
        return None;
    }
    Some((u32::from_be_bytes(d[..4].try_into().ok()?), &d[4..]))
}

pub fn encode_thumbnail(window_id: u32, jpeg: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(4 + jpeg.len());
    d.extend_from_slice(&window_id.to_be_bytes());
    d.extend_from_slice(jpeg);
    d
}

/// Kind 4: bundleIDLength(2, BE) + bundleID UTF-8 + PNG.
pub fn decode_icon(d: &[u8]) -> Option<(&str, &[u8])> {
    if d.len() <= 2 {
        return None;
    }
    let len = u16::from_be_bytes(d[..2].try_into().ok()?) as usize;
    if d.len() <= 2 + len {
        return None;
    }
    Some((std::str::from_utf8(&d[2..2 + len]).ok()?, &d[2 + len..]))
}

pub fn encode_icon(bundle_id: &str, png: &[u8]) -> Vec<u8> {
    let id = bundle_id.as_bytes();
    let mut d = Vec::with_capacity(2 + id.len() + png.len());
    d.extend_from_slice(&(id.len() as u16).to_be_bytes());
    d.extend_from_slice(id);
    d.extend_from_slice(png);
    d
}

/// Kind 14: hotspot x, y and size w, h as four big-endian f32, then a PNG.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorShape {
    pub hotspot: (f32, f32),
    pub size: (f32, f32),
    pub png: Vec<u8>,
}

impl CursorShape {
    pub fn decode(d: &[u8]) -> Option<CursorShape> {
        if d.len() <= 16 {
            return None;
        }
        let f = |i: usize| f32::from_bits(u32::from_be_bytes(d[i * 4..i * 4 + 4].try_into().unwrap()));
        Some(CursorShape { hotspot: (f(0), f(1)), size: (f(2), f(3)), png: d[16..].to_vec() })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut d = Vec::with_capacity(16 + self.png.len());
        for v in [self.hotspot.0, self.hotspot.1, self.size.0, self.size.1] {
            d.extend_from_slice(&v.to_bits().to_be_bytes());
        }
        d.extend_from_slice(&self.png);
        d
    }
}

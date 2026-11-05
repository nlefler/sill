//! base64 in the two forms Sill uses: standard with padding (kind 18's fields) and canonical
//! unpadded base64url (everything in pairing). Strictness follows `Base64URL.decode` in
//! RemoteIdentity.swift: a string that does not re-encode to itself is refused.

const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn encode_with(data: &[u8], table: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        let chars = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (i, v) in chars.iter().enumerate() {
            if i <= c.len() {
                out.push(table[*v as usize] as char);
            } else if pad {
                out.push('=');
            }
        }
    }
    out
}

fn decode_with(text: &str, table: &[u8; 64]) -> Option<Vec<u8>> {
    let mut buf = 0u32;
    let mut bits = 0u32;
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for b in text.bytes() {
        let v = table.iter().position(|t| *t == b)? as u32;
        buf = buf << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Unpadded base64url.
pub fn url_encode(data: &[u8]) -> String {
    encode_with(data, URL, false)
}

/// Canonical unpadded base64url only: no padding, no standard alphabet, no stray bits.
pub fn url_decode(text: &str) -> Option<Vec<u8>> {
    if !text.is_ascii() || text.len() % 4 == 1 {
        return None;
    }
    let d = decode_with(text, URL)?;
    (url_encode(&d) == text).then_some(d)
}

/// Standard base64 with padding.
pub fn std_encode(data: &[u8]) -> String {
    encode_with(data, STD, true)
}

/// Standard base64, padding required (as Foundation's `Data(base64Encoded:)`), canonical.
pub fn std_decode(text: &str) -> Option<Vec<u8>> {
    if !text.is_ascii() || !text.len().is_multiple_of(4) {
        return None;
    }
    let d = decode_with(text.trim_end_matches('='), STD)?;
    (std_encode(&d) == text).then_some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        for (raw, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(std_encode(raw.as_bytes()), enc);
            assert_eq!(std_decode(enc).unwrap(), raw.as_bytes());
            assert_eq!(url_encode(raw.as_bytes()), enc.trim_end_matches('='));
            assert_eq!(url_decode(enc.trim_end_matches('=')).unwrap(), raw.as_bytes());
        }
    }

    #[test]
    fn url_is_strict() {
        assert!(url_decode("Zm8=").is_none()); // padding
        assert!(url_decode("Zm9+").is_none()); // standard alphabet
        assert!(url_decode("Zm8").is_some());
        assert!(url_decode("Z").is_none()); // length 1 mod 4
        assert!(url_decode("Zh").is_none()); // non-canonical trailing bits
        assert_eq!(url_encode(&[0xfb, 0xff]), "-_8");
        assert!(url_decode(" Zm9v").is_none());
    }

    #[test]
    fn std_is_strict() {
        assert!(std_decode("Zm8").is_none());
        assert!(std_decode("Zm9v\n").is_none());
        assert!(std_decode("Zh==").is_none());
    }
}

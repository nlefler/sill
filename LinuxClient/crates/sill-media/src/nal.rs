//! The wire's HEVC framing as the decoder needs it. Kind 0 carries the NAL length-field size and the
//! VPS/SPS/PPS; kind 1 carries the access unit as length-prefixed NALs (hvcC style). Pure functions.

use sill_proto::ParameterSets;

pub const NAL_VPS: u8 = 32;
pub const NAL_SPS: u8 = 33;
pub const NAL_PPS: u8 = 34;
pub const START_CODE: [u8; 4] = [0, 0, 0, 1];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NalError {
    /// The length field size must be 1 to 4.
    BadLengthSize(u8),
    /// A length field or NAL runs past the payload.
    Truncated,
    /// A NAL of length 0 (no header byte).
    Empty,
}

impl std::fmt::Display for NalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NalError::BadLengthSize(n) => write!(f, "NAL length field of {n} bytes"),
            NalError::Truncated => write!(f, "NAL runs past the end of the frame"),
            NalError::Empty => write!(f, "empty NAL"),
        }
    }
}
impl std::error::Error for NalError {}

pub fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|b| (b >> 1) & 0x3f)
}

/// Iterate the NALs of a length-prefixed payload.
pub fn split_length_prefixed(payload: &[u8], len_size: u8) -> Result<Vec<&[u8]>, NalError> {
    if !(1..=4).contains(&len_size) {
        return Err(NalError::BadLengthSize(len_size));
    }
    let n = len_size as usize;
    let mut out = vec![];
    let mut i = 0;
    while i < payload.len() {
        let field = payload.get(i..i + n).ok_or(NalError::Truncated)?;
        let len = field.iter().fold(0usize, |a, b| (a << 8) | *b as usize);
        i += n;
        if len == 0 {
            return Err(NalError::Empty);
        }
        out.push(payload.get(i..i + len).ok_or(NalError::Truncated)?);
        i += len;
    }
    Ok(out)
}

/// Length-prefixed NALs to Annex B (4-byte start codes).
pub fn to_annex_b(payload: &[u8], len_size: u8) -> Result<Vec<u8>, NalError> {
    let nals = split_length_prefixed(payload, len_size)?;
    let mut out = Vec::with_capacity(payload.len() + nals.len() * 4);
    for n in nals {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(n);
    }
    Ok(out)
}

/// The parameter sets as Annex B, in the order the host sent them.
pub fn param_sets_annex_b(ps: &ParameterSets) -> Vec<u8> {
    let mut out = vec![];
    for s in ps.sets.iter().filter(|s| !s.is_empty()) {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(s);
    }
    out
}

/// Annex B to NALs (3- or 4-byte start codes).
pub fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = vec![];
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = vec![];
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(data.len(), |n| n - 3);
        while e > s && data[e - 1] == 0 {
            e -= 1; // trailing_zero_8bits / the zero of a 4-byte start code
        }
        if e > s {
            out.push(&data[s..e]);
        }
    }
    out
}

/// Remove emulation prevention bytes (00 00 03 -> 00 00).
pub fn rbsp(nal_body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal_body.len());
    let mut zeros = 0;
    for &b in nal_body {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let b = *self.d.get(self.pos / 8)?;
        let v = (b >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(v as u32)
    }
    fn bits(&mut self, n: usize) -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.pos += n;
        (self.pos <= self.d.len() * 8).then_some(())
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some(((1u64 << zeros) - 1 + self.bits(zeros)? as u64) as u32)
    }
}

/// What the SPS says that the container (hvcC) and the renderer need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpsInfo {
    /// general_profile_space(2) tier(1) profile_idc(5).
    pub profile_byte: u8,
    pub compat_flags: [u8; 4],
    pub constraint_flags: [u8; 6],
    pub level_idc: u8,
    pub chroma_format_idc: u8,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    /// Cropped picture size (the conformance window applied).
    pub width: u32,
    pub height: u32,
}

/// Parse an SPS NAL (with its 2-byte header). `None` when it is not an SPS or is cut short.
pub fn parse_sps(nal: &[u8]) -> Option<SpsInfo> {
    if nal_type(nal)? != NAL_SPS || nal.len() < 3 {
        return None;
    }
    let r = rbsp(&nal[2..]);
    let max_sub_layers_minus1 = ((*r.first()? >> 1) & 7) as usize;
    let ptl = r.get(1..13)?;
    let mut b = Bits { d: &r, pos: 13 * 8 };
    let mut profile_present = [false; 8];
    let mut level_present = [false; 8];
    for i in 0..max_sub_layers_minus1 {
        profile_present[i] = b.bit()? == 1;
        level_present[i] = b.bit()? == 1;
    }
    if max_sub_layers_minus1 > 0 {
        b.skip(2 * (8 - max_sub_layers_minus1))?;
    }
    for i in 0..max_sub_layers_minus1 {
        if profile_present[i] {
            b.skip(88)?;
        }
        if level_present[i] {
            b.skip(8)?;
        }
    }
    let _sps_id = b.ue()?;
    let chroma = b.ue()?;
    if chroma == 3 {
        b.skip(1)?;
    }
    let (w, h) = (b.ue()?, b.ue()?);
    let (mut cl, mut cr, mut ct, mut cb) = (0, 0, 0, 0);
    if b.bit()? == 1 {
        cl = b.ue()?;
        cr = b.ue()?;
        ct = b.ue()?;
        cb = b.ue()?;
    }
    let bd_l = b.ue()? + 8;
    let bd_c = b.ue()? + 8;
    let (sw, sh) = match chroma {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    Some(SpsInfo {
        profile_byte: ptl[0],
        compat_flags: ptl[1..5].try_into().ok()?,
        constraint_flags: ptl[5..11].try_into().ok()?,
        level_idc: ptl[11],
        chroma_format_idc: chroma as u8,
        bit_depth_luma: bd_l as u8,
        bit_depth_chroma: bd_c as u8,
        width: w.checked_sub((cl + cr) * sw)?,
        height: h.checked_sub((ct + cb) * sh)?,
    })
}

/// An hvcC `HEVCDecoderConfigurationRecord` for the parameter sets, as an MP4 container (and ffmpeg's
/// `extradata`) holds it. `None` when there is no parseable SPS.
pub fn hvcc_extradata(ps: &ParameterSets) -> Option<Vec<u8>> {
    let sps = ps.sets.iter().find(|s| nal_type(s) == Some(NAL_SPS)).and_then(|s| parse_sps(s))?;
    let mut d = vec![1, sps.profile_byte];
    d.extend_from_slice(&sps.compat_flags);
    d.extend_from_slice(&sps.constraint_flags);
    d.push(sps.level_idc);
    d.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc 0
    d.push(0xFC); // parallelismType unknown
    d.push(0xFC | sps.chroma_format_idc);
    d.push(0xF8 | (sps.bit_depth_luma - 8));
    d.push(0xF8 | (sps.bit_depth_chroma - 8));
    d.extend_from_slice(&[0, 0]); // avgFrameRate
    // constantFrameRate 0, numTemporalLayers 1, temporalIdNested 1, lengthSizeMinusOne
    d.push((1 << 3) | (1 << 2) | (ps.nal_unit_header_length.clamp(1, 4) - 1));
    let kinds = [NAL_VPS, NAL_SPS, NAL_PPS];
    let present: Vec<u8> = kinds.iter().copied().filter(|k| ps.sets.iter().any(|s| nal_type(s) == Some(*k))).collect();
    d.push(present.len() as u8);
    for k in present {
        let of: Vec<&Vec<u8>> = ps.sets.iter().filter(|s| nal_type(s) == Some(k)).collect();
        d.push(0x80 | k);
        d.extend_from_slice(&(of.len() as u16).to_be_bytes());
        for s in of {
            d.extend_from_slice(&(s.len() as u16).to_be_bytes());
            d.extend_from_slice(s);
        }
    }
    Some(d)
}

/// Whether the NAL types of an access unit include an IRAP picture (16 to 23): a keyframe.
pub fn is_irap(nal: &[u8]) -> bool {
    nal_type(nal).is_some_and(|t| (16..=23).contains(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lp(len_size: usize, nals: &[&[u8]]) -> Vec<u8> {
        let mut v = vec![];
        for n in nals {
            let l = (n.len() as u64).to_be_bytes();
            v.extend_from_slice(&l[8 - len_size..]);
            v.extend_from_slice(n);
        }
        v
    }

    #[test]
    fn length_prefixed_to_annex_b_for_each_field_size() {
        let a: &[u8] = &[0x26, 1, 0xAA, 0xBB];
        let b: &[u8] = &[0x02, 1, 0xCC];
        for n in 1..=4u8 {
            let out = to_annex_b(&lp(n as usize, &[a, b]), n).unwrap();
            assert_eq!(out, [&START_CODE[..], a, &START_CODE[..], b].concat());
        }
    }

    #[test]
    fn malformed_payloads_are_errors_not_panics() {
        assert_eq!(to_annex_b(&[0, 0, 0, 9, 1, 2], 4), Err(NalError::Truncated));
        assert_eq!(to_annex_b(&[0, 0], 4), Err(NalError::Truncated));
        assert_eq!(to_annex_b(&[0, 0, 0, 0], 4), Err(NalError::Empty));
        assert_eq!(to_annex_b(&[1, 2], 0), Err(NalError::BadLengthSize(0)));
        assert_eq!(to_annex_b(&[1, 2], 5), Err(NalError::BadLengthSize(5)));
        assert_eq!(to_annex_b(&[], 4), Ok(vec![]));
    }

    #[test]
    fn annex_b_round_trip_and_trailing_zeros() {
        let a: &[u8] = &[0x40, 1, 0x0C];
        let b: &[u8] = &[0x42, 1, 5, 0, 0, 3, 1];
        let mut s = vec![0, 0, 1];
        s.extend_from_slice(a);
        s.extend_from_slice(&[0, 0, 0, 1]);
        s.extend_from_slice(b);
        s.extend_from_slice(&[0, 0]);
        assert_eq!(split_annex_b(&s), vec![a, b]);
    }

    #[test]
    fn parameter_sets_to_annex_b() {
        let ps = ParameterSets { nal_unit_header_length: 4, sets: vec![vec![0x40, 1], vec![], vec![0x44, 1, 9]] };
        assert_eq!(param_sets_annex_b(&ps), vec![0, 0, 0, 1, 0x40, 1, 0, 0, 0, 1, 0x44, 1, 9]);
    }

    #[test]
    fn emulation_prevention_removed() {
        assert_eq!(rbsp(&[1, 0, 0, 3, 1, 0, 0, 3, 0, 0, 3]), vec![1, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(rbsp(&[0, 0, 4]), vec![0, 0, 4]);
    }

    #[test]
    fn nal_types() {
        assert_eq!(nal_type(&[0x40, 1]), Some(NAL_VPS));
        assert_eq!(nal_type(&[0x42, 1]), Some(NAL_SPS));
        assert_eq!(nal_type(&[0x44, 1]), Some(NAL_PPS));
        assert!(is_irap(&[0x26, 1]) && !is_irap(&[0x02, 1]) && !is_irap(&[]));
    }

    /// A hand-made 64x64 4:2:0 8-bit Main SPS: bits written by hand below.
    fn tiny_sps() -> Vec<u8> {
        // header 0x42 0x01; rbsp: vps 0, max_sub_layers 0, nesting 1 => 0x01
        let mut v = vec![0x42, 0x01, 0x01];
        v.extend_from_slice(&[0x01, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 93]); // PTL: Main, level 3.1
        // sps_id ue(0)=1, chroma ue(1)=010, w ue(64)=0000001000001, h same, conf 0, bd 1,1
        let bits = concat!("1", "010", "0000001000001", "0000001000001", "0", "1", "1");
        let mut byte = 0u8;
        let mut n = 0;
        for c in bits.chars() {
            byte = (byte << 1) | (c == '1') as u8;
            n += 1;
            if n == 8 {
                v.push(byte);
                byte = 0;
                n = 0;
            }
        }
        if n > 0 {
            v.push(byte << (8 - n));
        }
        v
    }

    #[test]
    fn sps_fields_and_hvcc() {
        let sps = tiny_sps();
        let info = parse_sps(&sps).unwrap();
        assert_eq!((info.width, info.height, info.chroma_format_idc, info.bit_depth_luma), (64, 64, 1, 8));
        assert_eq!((info.profile_byte, info.level_idc), (1, 93));
        let ps = ParameterSets { nal_unit_header_length: 4, sets: vec![vec![0x40, 1, 0xC], sps.clone(), vec![0x44, 1, 0xC0]] };
        let h = hvcc_extradata(&ps).unwrap();
        assert_eq!(h[0], 1);
        assert_eq!(h[1], 1);
        assert_eq!(h[12], 93);
        assert_eq!(h[21] & 3, 3, "length size minus one");
        assert_eq!(h[22], 3, "three arrays");
        assert_eq!(h[23], 0x80 | NAL_VPS);
        assert_eq!(&h[h.len() - 3..], &[0x44, 1, 0xC0]);
        assert!(hvcc_extradata(&ParameterSets { nal_unit_header_length: 4, sets: vec![vec![0x40, 1]] }).is_none());
    }

    #[test]
    fn truncated_sps_is_none() {
        let sps = tiny_sps();
        for n in 0..sps.len() - 1 {
            let _ = parse_sps(&sps[..n]); // never panics
        }
        assert!(parse_sps(&sps[..10]).is_none());
        assert!(parse_sps(&[0x40, 1, 1]).is_none());
    }
}

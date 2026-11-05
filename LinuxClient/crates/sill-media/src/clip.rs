//! A short HEVC clip as the host would send it: parameter sets, then length-prefixed access units with
//! their keyframe flags. For tests, the benchmark and `sill --mock`.

use crate::nal;
use sill_proto::ParameterSets;

#[derive(Clone, Debug)]
pub struct ClipFrame {
    pub keyframe: bool,
    /// Length-prefixed NALs, 4-byte fields.
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Clip {
    pub params: ParameterSets,
    pub frames: Vec<ClipFrame>,
    pub width: u32,
    pub height: u32,
}

/// Split an Annex B elementary stream (as `ffmpeg -f hevc` writes) into the wire's shape. Parameter
/// sets and SEI are lifted out (the host sends the sets in kind 0 and no SEI).
pub fn parse_annex_b(data: &[u8]) -> Result<Clip, String> {
    let nals = nal::split_annex_b(data);
    let (mut vps, mut sps, mut pps) = (None, None, None);
    let mut frames: Vec<ClipFrame> = vec![];
    for n in nals {
        let Some(t) = nal::nal_type(n) else { continue };
        match t {
            nal::NAL_VPS => vps = vps.or(Some(n.to_vec())),
            nal::NAL_SPS => sps = sps.or(Some(n.to_vec())),
            nal::NAL_PPS => pps = pps.or(Some(n.to_vec())),
            0..=9 | 16..=21 => {
                let first = n.get(2).is_some_and(|b| b & 0x80 != 0);
                if first || frames.is_empty() {
                    frames.push(ClipFrame { keyframe: nal::is_irap(n), payload: vec![] });
                }
                let f = frames.last_mut().unwrap();
                f.payload.extend_from_slice(&(n.len() as u32).to_be_bytes());
                f.payload.extend_from_slice(n);
            }
            _ => {}
        }
    }
    let (vps, sps, pps) = (vps.ok_or("no VPS")?, sps.ok_or("no SPS")?, pps.ok_or("no PPS")?);
    let info = nal::parse_sps(&sps).ok_or("SPS does not parse")?;
    if frames.is_empty() {
        return Err("no pictures".into());
    }
    Ok(Clip {
        params: ParameterSets { nal_unit_header_length: 4, sets: vec![vps, sps, pps] },
        frames,
        width: info.width,
        height: info.height,
    })
}

const CLIP_720P: &[u8] = include_bytes!("../tests/fixtures/clip_1280x720.hevc");

/// 1280x720, 60 frames at 30 fps, a keyframe every 30, no B-frames, BT.709 limited: the `testsrc` pattern.
pub fn embedded_720p() -> Clip {
    parse_annex_b(CLIP_720P).expect("embedded clip parses")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_clip_shape() {
        let c = embedded_720p();
        assert_eq!((c.width, c.height), (1280, 720));
        assert_eq!(c.frames.len(), 60);
        let keys: Vec<usize> = c.frames.iter().enumerate().filter(|(_, f)| f.keyframe).map(|(i, _)| i).collect();
        assert_eq!(keys, vec![0, 30]);
        assert_eq!(c.params.sets.len(), 3);
        for f in &c.frames {
            assert!(nal::split_length_prefixed(&f.payload, 4).is_ok());
        }
    }

    #[test]
    fn rejects_streams_without_sets_or_pictures() {
        assert!(parse_annex_b(&[]).is_err());
        assert!(parse_annex_b(&[0, 0, 0, 1, 0x26, 1, 0x80]).is_err());
    }
}

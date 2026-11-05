use serde::{Deserialize, Serialize};

/// Kind 9: the device's stream panel, Aa's scale and its wanted frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    pub width: f64,
    pub height: f64,
    /// Device points per Mac point; none leaves the Mac window alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(rename = "localCursor", default, skip_serializing_if = "Option::is_none")]
    pub local_cursor: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<i64>,
}

/// What a host takes of a viewport: each side 1 to 16384 points, a text scale 0.25 to 4 or none. Any
/// other viewport is refused whole (security scan, finding 4), so a client holds what it sends to them.
pub struct ViewportBounds;

impl ViewportBounds {
    pub const SIDES: (f64, f64) = (1.0, 16384.0);
    pub const SCALES: (f64, f64) = (0.25, 4.0);

    fn in_range(v: f64, (lo, hi): (f64, f64)) -> bool {
        v >= lo && v <= hi // false for NaN
    }

    /// Why a host would refuse `v`; `None` when it takes it.
    pub fn refusal(v: &Viewport) -> Option<String> {
        let mut why = Vec::new();
        if !Self::in_range(v.width, Self::SIDES) {
            why.push(format!("width {} (1 to 16384 points)", number(v.width)));
        }
        if !Self::in_range(v.height, Self::SIDES) {
            why.push(format!("height {} (1 to 16384 points)", number(v.height)));
        }
        if let Some(s) = v.scale {
            if !Self::in_range(s, Self::SCALES) {
                why.push(format!("scale {} (0.25 to 4)", number(s)));
            }
        }
        if why.is_empty() { None } else { Some(why.join(", ")) }
    }

    /// What to send for `v`: sides clamped, a scale that is not a positive number dropped. `None` while
    /// a side is not a positive number (a panel not measured yet): send nothing.
    pub fn held(v: &Viewport) -> Option<Viewport> {
        if !(v.width.is_finite() && v.height.is_finite() && v.width > 0.0 && v.height > 0.0) {
            return None;
        }
        let mut h = *v;
        h.width = v.width.clamp(Self::SIDES.0, Self::SIDES.1);
        h.height = v.height.clamp(Self::SIDES.0, Self::SIDES.1);
        h.scale = v.scale.and_then(|s| {
            if s.is_finite() && s > 0.0 { Some(s.clamp(Self::SCALES.0, Self::SCALES.1)) } else { None }
        });
        Some(h)
    }
}

/// A number as the Swift side prints it in a refusal: whole ones without ".0".
fn number(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let a = x.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        // Rust gives "1e300" / "1e-5"; Swift gives "1e+300" / "1e-05" (a sign, two digits at least).
        let s = format!("{x:e}");
        let (mant, exp) = s.split_once('e').unwrap();
        let exp: i32 = exp.parse().unwrap();
        return format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    if x == x.round() {
        return format!("{}", x as i64);
    }
    format!("{x}")
}

/// Kind 12, once a second. The first four fields carry the second's median, -1 for no sample.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientStats {
    pub fps: i64,
    #[serde(rename = "frameAgeMs")]
    pub frame_age_ms: i64,
    #[serde(rename = "rttMs")]
    pub rtt_ms: i64,
    pub device: String,
    #[serde(rename = "frameAgeMaxMs", default, skip_serializing_if = "Option::is_none")]
    pub frame_age_max_ms: Option<i64>,
    #[serde(rename = "rttMaxMs", default, skip_serializing_if = "Option::is_none")]
    pub rtt_max_ms: Option<i64>,
    #[serde(rename = "audioBehindMs", default, skip_serializing_if = "Option::is_none")]
    pub audio_behind_ms: Option<i64>,
    #[serde(rename = "audioLate", default, skip_serializing_if = "Option::is_none")]
    pub audio_late: Option<i64>,
}

//! VAAPI -> Vulkan zero-copy, the parts that need neither FFmpeg nor a GPU: whether it is wanted, the
//! process-wide state the renderer sets and the decoder reads, how many exported pictures may be alive
//! at once, and the import plan for a DRM PRIME picture (format, modifier, plane layout).
//!
//! The decoder exports a decoded VAAPI surface as a dmabuf (DRM PRIME); the window's Vulkan device
//! imports it as an NV12 image and samples its two planes, so the picture never visits the CPU. Any
//! failure puts the state to `Off` and the decoder goes back to reading pictures into memory.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;

/// `SILL_ZEROCOPY=0|off|false|no` turns it off; anything else (or nothing) leaves it wanted.
pub fn wanted_from_env(v: Option<&str>) -> bool {
    !matches!(v.map(|s| s.trim().to_ascii_lowercase()).as_deref(), Some("0" | "off" | "false" | "no"))
}

pub fn wanted() -> bool {
    wanted_from_env(std::env::var("SILL_ZEROCOPY").ok().as_deref())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// The window's GPU has not said yet (or there is none): pictures are read back.
    Unknown,
    /// The GPU device can import dmabufs: the decoder exports them.
    Active,
    /// Not wanted, not possible, or an import failed.
    Off,
}

static STATE: AtomicU8 = AtomicU8::new(0);

pub fn state() -> State {
    match STATE.load(Ordering::Acquire) {
        1 => State::Active,
        2 => State::Off,
        _ => State::Unknown,
    }
}

pub fn set_state(s: State) {
    STATE.store(
        match s {
            State::Unknown => 0,
            State::Active => 1,
            State::Off => 2,
        },
        Ordering::Release,
    );
}

pub fn active() -> bool {
    state() == State::Active
}

/// Exported pictures alive at once (the one waiting in the slot, the ones the GPU still reads). The
/// decoder's surface pool is sized for this many beyond its own references.
pub const MAX_LIVE_PICTURES: usize = 6;

/// A count of exported pictures that are alive; a picture holds a [`LiveGuard`] until it is dropped.
#[derive(Clone, Default, Debug)]
pub struct LiveFrames(Arc<AtomicUsize>);

#[derive(Debug)]
pub struct LiveGuard(Arc<AtomicUsize>);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl LiveFrames {
    /// A guard when fewer than `limit` pictures are alive.
    pub fn try_acquire(&self, limit: usize) -> Option<LiveGuard> {
        let mut cur = self.0.load(Ordering::Acquire);
        loop {
            if cur >= limit {
                return None;
            }
            match self.0.compare_exchange_weak(cur, cur + 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Some(LiveGuard(self.0.clone())),
                Err(c) => cur = c,
            }
        }
    }
    pub fn live(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }
}

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    (s[0] as u32) | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24
}
pub const DRM_FORMAT_NV12: u32 = fourcc(b"NV12");
pub const DRM_FORMAT_R8: u32 = fourcc(b"R8  ");
pub const DRM_FORMAT_GR88: u32 = fourcc(b"GR88");
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimeObject {
    pub fd: i32,
    pub size: u64,
    pub modifier: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrimePlane {
    pub object: usize,
    pub offset: u64,
    pub pitch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimeLayer {
    pub format: u32,
    pub planes: Vec<PrimePlane>,
}

/// What FFmpeg's `AVDRMFrameDescriptor` says about a picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimeDesc {
    pub objects: Vec<PrimeObject>,
    pub layers: Vec<PrimeLayer>,
}

/// What to give Vulkan for one NV12 picture in one dmabuf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportPlan {
    pub fd: i32,
    pub modifier: u64,
    pub size: u64,
    pub coded: (u32, u32),
    /// Luma, then chroma: offset and row pitch in bytes.
    pub planes: [(u64, u64); 2],
}

/// The picture as an image Vulkan can import, or why not (the reason goes in the log; the decoder
/// then reads pictures back instead). Accepts NV12 as one layer of two planes (iHD's composed export) or
/// as R8 + GR88 layers (FFmpeg's separate layers), all in one object with a known modifier.
pub fn plan_import(desc: &PrimeDesc, coded: (u32, u32)) -> Result<ImportPlan, String> {
    if coded.0 == 0 || coded.1 == 0 {
        return Err("the picture has no size".into());
    }
    let [obj] = desc.objects.as_slice() else {
        return Err(format!("{} dmabuf objects (one image from several is not supported)", desc.objects.len()));
    };
    if obj.modifier == DRM_FORMAT_MOD_INVALID {
        return Err("the dmabuf has no format modifier".into());
    }
    let planes: Vec<PrimePlane> = match desc.layers.as_slice() {
        [l] if l.format == DRM_FORMAT_NV12 && l.planes.len() == 2 => l.planes.clone(),
        [a, b] if a.format == DRM_FORMAT_R8 && b.format == DRM_FORMAT_GR88 && a.planes.len() == 1 && b.planes.len() == 1 => {
            vec![a.planes[0], b.planes[0]]
        }
        other => {
            let f: Vec<String> = other.iter().map(|l| format!("{:#x}x{}", l.format, l.planes.len())).collect();
            return Err(format!("layers [{}] are not NV12 (8-bit 4:2:0 only)", f.join(", ")));
        }
    };
    for p in &planes {
        if p.object != 0 {
            return Err("a plane in another object".into());
        }
        if p.pitch == 0 || p.offset >= obj.size.max(1) {
            return Err(format!("plane offset {} pitch {} outside the {}-byte object", p.offset, p.pitch, obj.size));
        }
    }
    Ok(ImportPlan {
        fd: obj.fd,
        modifier: obj.modifier,
        size: obj.size,
        coded,
        planes: [(planes[0].offset, planes[0].pitch), (planes[1].offset, planes[1].pitch)],
    })
}

/// Whether the Vulkan device lists the plan's modifier for NV12 sampling with the same number of memory
/// planes (`(modifier, plane count)` of every modifier it lists for sampled images).
pub fn modifier_supported(device_list: &[(u64, u32)], plan: &ImportPlan) -> bool {
    device_list.iter().any(|&(m, planes)| m == plan.modifier && planes == 2)
}

/// The `crop` the shader multiplies texture coordinates by: the shown part of the coded picture.
pub fn crop_scale(shown: (u32, u32), coded: (u32, u32)) -> [f32; 2] {
    [shown.0 as f32 / coded.0.max(1) as f32, shown.1 as f32 / coded.1.max(1) as f32]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(modifier: u64) -> PrimeObject {
        PrimeObject { fd: 7, size: 1 << 20, modifier }
    }
    fn pl(offset: u64, pitch: u64) -> PrimePlane {
        PrimePlane { object: 0, offset, pitch }
    }
    fn composed() -> PrimeDesc {
        PrimeDesc { objects: vec![obj(9)], layers: vec![PrimeLayer { format: DRM_FORMAT_NV12, planes: vec![pl(0, 2048), pl(1 << 19, 2048)] }] }
    }
    fn separate() -> PrimeDesc {
        PrimeDesc {
            objects: vec![obj(9)],
            layers: vec![
                PrimeLayer { format: DRM_FORMAT_R8, planes: vec![pl(0, 2048)] },
                PrimeLayer { format: DRM_FORMAT_GR88, planes: vec![pl(1 << 19, 2048)] },
            ],
        }
    }

    #[test]
    fn env_switch() {
        assert!(wanted_from_env(None));
        assert!(wanted_from_env(Some("1")));
        assert!(wanted_from_env(Some("yes")));
        for off in ["0", "off", "FALSE", " no "] {
            assert!(!wanted_from_env(Some(off)), "{off}");
        }
    }

    #[test]
    fn fourccs() {
        assert_eq!(DRM_FORMAT_NV12, 0x3231_564e);
        assert_eq!(DRM_FORMAT_R8, 0x2020_3852);
        assert_eq!(DRM_FORMAT_GR88, 0x3838_5247);
    }

    #[test]
    fn both_layer_shapes_give_the_same_plan() {
        let a = plan_import(&composed(), (1920, 1088)).unwrap();
        let b = plan_import(&separate(), (1920, 1088)).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.planes, [(0, 2048), (1 << 19, 2048)]);
        assert_eq!((a.fd, a.modifier, a.coded), (7, 9, (1920, 1088)));
    }

    #[test]
    fn plans_refuse_what_cannot_be_imported() {
        let mut d = composed();
        d.objects[0].modifier = DRM_FORMAT_MOD_INVALID;
        assert!(plan_import(&d, (64, 64)).unwrap_err().contains("modifier"));
        let mut d = composed();
        d.objects.push(obj(9));
        assert!(plan_import(&d, (64, 64)).unwrap_err().contains("objects"));
        let mut d = composed();
        d.layers[0].format = fourcc(b"P010");
        assert!(plan_import(&d, (64, 64)).unwrap_err().contains("not NV12"));
        let mut d = composed();
        d.layers[0].planes[1].object = 1;
        assert!(plan_import(&d, (64, 64)).is_err());
        let mut d = composed();
        d.layers[0].planes[0].pitch = 0;
        assert!(plan_import(&d, (64, 64)).is_err());
        let mut d = composed();
        d.layers[0].planes[1].offset = 1 << 21;
        assert!(plan_import(&d, (64, 64)).is_err());
        assert!(plan_import(&composed(), (0, 64)).is_err());
        let mut d = separate();
        d.layers.truncate(1);
        assert!(plan_import(&d, (64, 64)).is_err());
    }

    #[test]
    fn modifier_negotiation() {
        let plan = plan_import(&composed(), (64, 64)).unwrap();
        assert!(modifier_supported(&[(1, 2), (9, 2)], &plan));
        assert!(!modifier_supported(&[(1, 2), (9, 3)], &plan), "aux planes change the plane count");
        assert!(!modifier_supported(&[], &plan));
        assert!(!modifier_supported(&[(8, 2)], &plan));
    }

    #[test]
    fn crop() {
        assert_eq!(crop_scale((1920, 1080), (1920, 1088)), [1.0, 1080.0 / 1088.0]);
        assert_eq!(crop_scale((10, 10), (0, 0)), [10.0, 10.0]);
    }

    #[test]
    fn live_budget_counts_and_releases() {
        let l = LiveFrames::default();
        let g: Vec<_> = (0..3).map(|_| l.try_acquire(3).expect("room")).collect();
        assert_eq!(l.live(), 3);
        assert!(l.try_acquire(3).is_none());
        let mut g = g;
        g.pop();
        assert_eq!(l.live(), 2);
        let again = l.try_acquire(3);
        assert!(again.is_some() && l.live() == 3);
        drop(again);
        drop(g);
        assert_eq!(l.live(), 0);
    }

    #[test]
    fn state_round_trip() {
        // The only test that touches the process-wide state.
        set_state(State::Active);
        assert!(active());
        set_state(State::Off);
        assert_eq!(state(), State::Off);
        set_state(State::Unknown);
        assert_eq!(state(), State::Unknown);
    }
}

//! HEVC decode behind one small API: a wire frame in, an NV12 frame out. Low-delay (the host sends no
//! B-frames), hardware through VAAPI when it works, software otherwise. On a decode error the decoder
//! flushes, drops deltas until the next keyframe and says so, so the app can ask for one (kind 44).

use crate::nal;
use crate::zerocopy::{self, LiveFrames, LiveGuard, PrimeDesc, PrimeLayer, PrimeObject, PrimePlane};
use ffmpeg_next as ff;
use ffmpeg_sys_next as sys;
use sill_proto::ParameterSets;
use std::ffi::CString;
use std::sync::{Arc, Once};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderPref {
    /// VAAPI when it opens and decodes, else software.
    Auto,
    Software,
    Vaapi,
}

impl DecoderPref {
    pub fn parse(s: &str) -> Option<DecoderPref> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(DecoderPref::Auto),
            "sw" | "software" | "cpu" => Some(DecoderPref::Software),
            "vaapi" | "hw" => Some(DecoderPref::Vaapi),
            _ => None,
        }
    }
    /// `SILL_DECODER=sw|vaapi|auto`; anything else is `Auto`.
    pub fn from_env() -> DecoderPref {
        std::env::var("SILL_DECODER").ok().and_then(|v| DecoderPref::parse(&v)).unwrap_or(DecoderPref::Auto)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderKind {
    Software,
    Vaapi,
}

impl DecoderKind {
    pub fn name(self) -> &'static str {
        match self {
            DecoderKind::Software => "software (ffmpeg hevc)",
            DecoderKind::Vaapi => "VAAPI (ffmpeg hevc)",
        }
    }
}

/// Coded areas above this (1920x1200) decode in software under `Auto` unless zero-copy is active:
/// VAAPI with a read-back to system memory measured 27 fps at 4K against 124 for software, and is only
/// ahead at 1080p and below. Without the read-back (dmabuf into Vulkan) hardware wins at every size.
pub const AUTO_SOFTWARE_ABOVE_PIXELS: u64 = 1920 * 1200;

/// What `Auto` uses for a stream of this size once VAAPI has opened (`None`: size unknown, keep VAAPI).
/// `zero_copy`: pictures reach the GPU without a read-back, so VAAPI keeps every size.
pub fn auto_kind(size: Option<(u32, u32)>, zero_copy: bool) -> DecoderKind {
    match size {
        Some((w, h)) if !zero_copy && (w as u64) * (h as u64) > AUTO_SOFTWARE_ABOVE_PIXELS => DecoderKind::Software,
        _ => DecoderKind::Vaapi,
    }
}

/// A decoded VAAPI surface exported as a dmabuf (DRM PRIME). The mapped frame owns the file descriptors
/// and, through FFmpeg, the VAAPI surface: both stay until this is dropped, so keep it until the GPU is
/// done with the image made from it.
pub struct PrimeFrame {
    #[allow(dead_code)] // owns the descriptor's file descriptors and the surface
    frame: ff::frame::Video,
    pub desc: PrimeDesc,
    /// The surface's size (aligned up from the shown picture).
    pub coded: (u32, u32),
    _guard: LiveGuard,
}

// The frame is only read (its descriptor was copied out) and dropped from any thread.
unsafe impl Send for PrimeFrame {}
unsafe impl Sync for PrimeFrame {}

impl std::fmt::Debug for PrimeFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PrimeFrame {{ coded: {:?}, {:?} }}", self.coded, self.desc)
    }
}

/// Export `f` (a VAAPI frame) as DRM PRIME. FFmpeg waits for the decoder to finish with the surface.
fn map_prime(f: &ff::frame::Video, guard: LiveGuard) -> Result<PrimeFrame, String> {
    let mut dst = ff::frame::Video::empty();
    let (desc, coded) = unsafe {
        (*dst.as_mut_ptr()).format = sys::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
        let r = sys::av_hwframe_map(dst.as_mut_ptr(), f.as_ptr(), sys::AV_HWFRAME_MAP_READ as i32);
        if r < 0 {
            return Err(format!("av_hwframe_map to DRM PRIME: {}", ff::Error::from(r)));
        }
        let d = (*dst.as_ptr()).data[0] as *const sys::AVDRMFrameDescriptor;
        if d.is_null() {
            return Err("no DRM frame descriptor".into());
        }
        let d = &*d;
        let objects = d.objects[..(d.nb_objects.clamp(0, 4)) as usize]
            .iter()
            .map(|o| PrimeObject { fd: o.fd, size: o.size as u64, modifier: o.format_modifier })
            .collect();
        let layers = d.layers[..(d.nb_layers.clamp(0, 4)) as usize]
            .iter()
            .map(|l| PrimeLayer {
                format: l.format,
                planes: l.planes[..(l.nb_planes.clamp(0, 4)) as usize]
                    .iter()
                    .map(|p| PrimePlane { object: p.object_index.max(0) as usize, offset: p.offset as u64, pitch: p.pitch as u64 })
                    .collect(),
            })
            .collect();
        // The surface is the hardware frames' size; the AVFrame's is the shown (cropped) part.
        let hw = (*f.as_ptr()).hw_frames_ctx;
        let coded = if hw.is_null() {
            (f.width(), f.height())
        } else {
            let c = (*hw).data as *const sys::AVHWFramesContext;
            ((*c).width.max(0) as u32, (*c).height.max(0) as u32)
        };
        (PrimeDesc { objects, layers }, coded)
    };
    Ok(PrimeFrame { frame: dst, desc, coded, _guard: guard })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matrix {
    Bt601,
    Bt709,
    Bt2020,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorInfo {
    pub matrix: Matrix,
    pub full_range: bool,
}

impl Default for ColorInfo {
    /// BT.709, limited range: what the stream is assumed to be when it says nothing.
    fn default() -> Self {
        ColorInfo { matrix: Matrix::Bt709, full_range: false }
    }
}

/// One decoded picture, NV12, planes tightly packed: `y` is `width` x `height`, `uv` is interleaved
/// Cb/Cr, `(width+1)/2` pairs by `(height+1)/2` rows.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
    /// The host's timestamp (seconds, its wall clock) of this picture.
    pub timestamp: f64,
    pub color: ColorInfo,
    /// Set instead of the planes (empty) when the picture stays on the GPU as a dmabuf.
    pub prime: Option<Arc<PrimeFrame>>,
}

impl VideoFrame {
    pub fn y_stride(&self) -> usize {
        self.width as usize
    }
    pub fn uv_stride(&self) -> usize {
        2 * self.width.div_ceil(2) as usize
    }
}

#[derive(Debug, Default)]
pub struct DecodeResult {
    /// The newest picture this packet produced.
    pub frame: Option<VideoFrame>,
    /// Set when the picture was lost and a keyframe is needed (the reason). Reported once per loss.
    pub interruption: Option<String>,
    /// A keyframe decoded after an interruption.
    pub picture_back: bool,
    /// A delta was thrown away because it waits for a keyframe.
    pub dropped: bool,
    /// Milliseconds spent decoding this packet.
    pub decode_ms: f64,
}

#[derive(Debug)]
pub enum DecoderError {
    Ffmpeg(String),
}
impl std::fmt::Display for DecoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let DecoderError::Ffmpeg(s) = self;
        write!(f, "{s}")
    }
}
impl std::error::Error for DecoderError {}
impl From<ff::Error> for DecoderError {
    fn from(e: ff::Error) -> Self {
        DecoderError::Ffmpeg(e.to_string())
    }
}

static INIT: Once = Once::new();
fn init() {
    INIT.call_once(|| {
        let _ = ff::init();
        ff::util::log::set_level(ff::util::log::Level::Fatal);
    });
}

struct HwDevice(*mut sys::AVBufferRef);
unsafe impl Send for HwDevice {}
impl Drop for HwDevice {
    fn drop(&mut self) {
        unsafe { sys::av_buffer_unref(&mut self.0) }
    }
}

fn open_vaapi_device() -> Result<HwDevice, DecoderError> {
    init();
    let dev = std::env::var("SILL_VAAPI_DEVICE").ok().or_else(|| {
        let d = "/dev/dri/renderD128";
        std::path::Path::new(d).exists().then(|| d.to_string())
    });
    let cdev = dev.as_deref().map(|d| CString::new(d).unwrap());
    let mut ptr: *mut sys::AVBufferRef = std::ptr::null_mut();
    let r = unsafe {
        sys::av_hwdevice_ctx_create(
            &mut ptr,
            sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            cdev.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            std::ptr::null_mut(),
            0,
        )
    };
    if r < 0 {
        return Err(DecoderError::Ffmpeg(format!("VAAPI device {dev:?}: {}", ff::Error::from(r))));
    }
    Ok(HwDevice(ptr))
}

unsafe extern "C" fn pick_vaapi(_: *mut sys::AVCodecContext, fmts: *const sys::AVPixelFormat) -> sys::AVPixelFormat {
    let mut p = fmts;
    while *p != sys::AVPixelFormat::AV_PIX_FMT_NONE {
        if *p == sys::AVPixelFormat::AV_PIX_FMT_VAAPI {
            return *p;
        }
        p = p.add(1);
    }
    sys::AVPixelFormat::AV_PIX_FMT_NONE
}

pub struct Decoder {
    pref: DecoderPref,
    kind: DecoderKind,
    hw: Option<HwDevice>,
    ctx: Option<ff::decoder::Video>,
    params: Option<ParameterSets>,
    annexb_params: Vec<u8>,
    /// Lost the picture: deltas are dropped until a keyframe decodes.
    waiting: bool,
    have_key: bool,
    hw_frames_ok: u64,
    hw_errors: u32,
    fallback: Option<String>,
    threads: usize,
    discard: bool,
    discarded: u64,
    /// The coded size of the stream (from its SPS), once known.
    stream_size: Option<(u32, u32)>,
    live: LiveFrames,
    /// Exporting dmabufs failed here: pictures are read back from now on.
    prime_broken: Option<String>,
    /// The last picture came out as a dmabuf.
    last_prime: bool,
}

impl Decoder {
    /// Open per `pref`. `Vaapi` fails when the device or driver does not work; `Auto` falls back.
    pub fn new(pref: DecoderPref) -> Result<Decoder, DecoderError> {
        init();
        let mut d = Decoder {
            pref,
            kind: DecoderKind::Software,
            hw: None,
            ctx: None,
            params: None,
            annexb_params: vec![],
            waiting: false,
            have_key: false,
            hw_frames_ok: 0,
            hw_errors: 0,
            fallback: None,
            discard: false,
            discarded: 0,
            stream_size: None,
            live: LiveFrames::default(),
            prime_broken: None,
            last_prime: false,
            threads: std::env::var("SILL_DECODE_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
                std::thread::available_parallelism().map_or(2, |n| n.get().min(4))
            }),
        };
        match pref {
            DecoderPref::Software => {}
            DecoderPref::Vaapi => {
                d.hw = Some(open_vaapi_device()?);
                d.kind = DecoderKind::Vaapi;
            }
            DecoderPref::Auto => match open_vaapi_device() {
                Ok(h) => {
                    d.hw = Some(h);
                    d.kind = DecoderKind::Vaapi;
                }
                Err(e) => d.fallback = Some(e.to_string()),
            },
        }
        Ok(d)
    }

    /// Benchmarks only: decode but read a picture back to system memory only every 30th frame (the others come back empty, width 0). The read-back is also what waits for the GPU.
    pub fn set_discard_output(&mut self, discard: bool) {
        self.discard = discard;
    }

    pub fn kind(&self) -> DecoderKind {
        self.kind
    }

    /// Pictures leave VAAPI as dmabufs for the GPU (the window's device said it can import them).
    pub fn zero_copy_active(&self) -> bool {
        self.kind == DecoderKind::Vaapi && self.zero_copy_possible()
    }

    fn zero_copy_possible(&self) -> bool {
        self.hw.is_some() && zerocopy::active() && self.prime_broken.is_none()
    }

    /// Which way pictures travel, for the HUD and the log.
    pub fn path_name(&self) -> String {
        let mut s = match self.fallback.as_deref() {
            Some(r) => format!("{} [{r}]", self.kind.name()),
            None => self.kind.name().to_string(),
        };
        if self.kind == DecoderKind::Vaapi {
            if self.last_prime || (self.zero_copy_active()) {
                s.push_str(", zero-copy dmabuf to Vulkan");
            } else {
                s.push_str(", read back to memory");
                if let Some(why) = &self.prime_broken {
                    s.push_str(&format!(" [zero-copy: {why}]"));
                }
            }
        }
        s
    }

    /// `Auto` re-chooses when what it depends on changed (zero-copy came up or went away). A change of
    /// decoder restarts it, which then waits for a keyframe.
    fn rechoose(&mut self) {
        if self.pref == DecoderPref::Auto && self.hw.is_some() {
            let k = auto_kind(self.stream_size, self.zero_copy_possible());
            if k != self.kind {
                self.kind = k;
                self.ctx = None;
                self.have_key = false;
            }
        }
    }
    /// Why `Auto` is not on VAAPI, if it is not.
    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback.as_deref()
    }
    pub fn is_waiting_for_keyframe(&self) -> bool {
        self.waiting
    }

    fn open_context(&mut self) -> Result<(), DecoderError> {
        let codec = ff::decoder::find(ff::codec::Id::HEVC).ok_or_else(|| DecoderError::Ffmpeg("no HEVC decoder".into()))?;
        let mut ctx = ff::codec::Context::new_with_codec(codec);
        unsafe {
            let p = ctx.as_mut_ptr();
            (*p).flags |= sys::AV_CODEC_FLAG_LOW_DELAY as i32;
            (*p).has_b_frames = 0;
            match (self.kind, &self.hw) {
                (DecoderKind::Vaapi, Some(hw)) => {
                    (*p).hw_device_ctx = sys::av_buffer_ref(hw.0);
                    // Exported pictures stay out of the decoder's pool while the GPU reads them.
                    (*p).extra_hw_frames = zerocopy::MAX_LIVE_PICTURES as i32 + 2;
                    (*p).get_format = Some(pick_vaapi);
                    (*p).thread_count = 1;
                }
                _ => {
                    // Slice threads add no delay; frame threads would add a frame each.
                    (*p).thread_count = self.threads as i32;
                    (*p).thread_type = sys::FF_THREAD_SLICE;
                }
            }
        }
        self.ctx = Some(ctx.decoder().video()?);
        Ok(())
    }

    /// Kind 0. The same sets again change nothing; different ones restart the decoder, which then waits
    /// for the keyframe that follows them.
    pub fn set_parameter_sets(&mut self, ps: &ParameterSets) {
        if self.params.as_ref() == Some(ps) {
            return;
        }
        self.annexb_params = nal::param_sets_annex_b(ps);
        self.params = Some(ps.clone());
        self.stream_size = ps.sets.iter().find_map(|s| nal::parse_sps(s)).map(|i| (i.width, i.height));
        if self.pref == DecoderPref::Auto && self.hw.is_some() {
            self.kind = auto_kind(self.stream_size, self.zero_copy_possible());
        }
        self.ctx = None; // reopened by the next keyframe
        self.have_key = false;
    }

    fn switch_to_software(&mut self, why: String) {
        self.kind = DecoderKind::Software;
        self.hw = None;
        self.ctx = None;
        self.have_key = false;
        self.fallback = Some(why);
    }

    /// The caller threw frames away: wait for a keyframe. `Some(why)` the first time only.
    pub fn lose_picture(&mut self, why: &str) -> Option<String> {
        let mut r = DecodeResult::default();
        self.lose(why.to_string(), &mut r);
        r.interruption
    }

    fn lose(&mut self, why: String, res: &mut DecodeResult) {
        if let Some(c) = self.ctx.as_mut() {
            c.flush();
        }
        self.have_key = false;
        if !self.waiting {
            self.waiting = true;
            res.interruption = Some(why);
        }
    }

    /// One kind-1 frame. `keyframe` is the header's flag.
    pub fn decode(&mut self, payload: &[u8], timestamp: f64, keyframe: bool) -> DecodeResult {
        let t0 = Instant::now();
        self.rechoose();
        let mut res = DecodeResult::default();
        let Some(ps) = self.params.as_ref() else {
            self.lose("a frame before any parameter sets".into(), &mut res);
            res.dropped = true;
            return res;
        };
        if self.waiting && !keyframe {
            res.dropped = true;
            return res;
        }
        if !keyframe && !self.have_key {
            self.lose("a delta frame with no keyframe before it".into(), &mut res);
            res.dropped = true;
            return res;
        }
        let annexb = match nal::to_annex_b(payload, ps.nal_unit_header_length) {
            Ok(a) => a,
            Err(e) => {
                self.lose(format!("bad frame: {e}"), &mut res);
                return res;
            }
        };
        let mut data = Vec::with_capacity(annexb.len() + self.annexb_params.len());
        if keyframe {
            data.extend_from_slice(&self.annexb_params);
        }
        data.extend_from_slice(&annexb);

        if self.ctx.is_none() {
            if let Err(e) = self.open_context() {
                if self.kind == DecoderKind::Vaapi && self.pref == DecoderPref::Auto {
                    self.switch_to_software(format!("VAAPI would not open: {e}"));
                    if let Err(e) = self.open_context() {
                        self.lose(format!("decoder: {e}"), &mut res);
                        return res;
                    }
                } else {
                    self.lose(format!("decoder: {e}"), &mut res);
                    return res;
                }
            }
        }
        match self.run(&data, timestamp) {
            Ok(frame) => {
                if let Some(f) = frame {
                    if self.kind == DecoderKind::Vaapi {
                        self.hw_frames_ok += 1;
                    }
                    if self.waiting || !self.have_key {
                        res.picture_back = self.waiting;
                    }
                    self.waiting = false;
                    self.have_key = true;
                    res.frame = Some(f);
                }
            }
            Err(e) => {
                if self.kind == DecoderKind::Vaapi {
                    self.hw_errors += 1;
                    if self.pref == DecoderPref::Auto && self.hw_frames_ok == 0 && self.hw_errors >= 3 {
                        self.switch_to_software(format!("VAAPI decode failed: {e}"));
                    }
                }
                self.lose(format!("decode error: {e}"), &mut res);
            }
        }
        res.decode_ms = t0.elapsed().as_secs_f64() * 1000.0;
        res
    }

    fn run(&mut self, data: &[u8], timestamp: f64) -> Result<Option<VideoFrame>, DecoderError> {
        let export = self.zero_copy_active() && !self.discard;
        let live = self.live.clone();
        let mut prime_error: Option<String> = None;
        let mut skipped = false;
        let ctx = self.ctx.as_mut().expect("opened");
        let mut pkt = ff::Packet::copy(data);
        pkt.set_pts(Some((timestamp * 1e6) as i64));
        pkt.set_dts(Some((timestamp * 1e6) as i64));
        ctx.send_packet(&pkt)?;
        let mut newest = None;
        let mut f = ff::frame::Video::empty();
        loop {
            match ctx.receive_frame(&mut f) {
                Ok(()) if self.discard && { self.discarded += 1; !self.discarded.is_multiple_of(30) } => {
                    newest = Some(VideoFrame { width: 0, height: 0, y: vec![], uv: vec![], timestamp, color: ColorInfo::default(), prime: None })
                }
                Ok(()) => {
                    let prime = export.then(|| (live.try_acquire(zerocopy::MAX_LIVE_PICTURES), &mut prime_error));
                    match extract(&f, timestamp, prime)? {
                        Some(v) => newest = Some(v),
                        None => skipped = true,
                    }
                }
                Err(ff::Error::Other { errno }) if errno == ff::error::EAGAIN => break,
                Err(ff::Error::Eof) => break,
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(why) = prime_error {
            eprintln!("zero-copy: exporting a picture as a dmabuf failed ({why}); reading pictures back to memory from now on");
            self.prime_broken = Some(why);
            self.rechoose();
        }
        if skipped && newest.is_none() {
            // Every exported picture is still held: this one is not shown.
            return Ok(None);
        }
        self.last_prime = newest.as_ref().is_some_and(|v| v.prime.is_some());
        Ok(newest)
    }
}

fn color_of(f: &ff::frame::Video) -> ColorInfo {
    use sys::AVColorSpace::*;
    let (space, range) = unsafe { ((*f.as_ptr()).colorspace, (*f.as_ptr()).color_range) };
    let matrix = match space {
        AVCOL_SPC_BT470BG | AVCOL_SPC_SMPTE170M | AVCOL_SPC_SMPTE240M => Matrix::Bt601,
        AVCOL_SPC_BT2020_NCL | AVCOL_SPC_BT2020_CL => Matrix::Bt2020,
        _ => Matrix::Bt709,
    };
    ColorInfo { matrix, full_range: range == sys::AVColorRange::AVCOL_RANGE_JPEG }
}

/// `prime`: export as a dmabuf instead of reading back; the guard (None: too many pictures alive, so `Ok(None)`)
/// and where to put the reason when the export fails (the picture is then read back).
fn extract(f: &ff::frame::Video, fallback_ts: f64, prime: Option<(Option<LiveGuard>, &mut Option<String>)>) -> Result<Option<VideoFrame>, DecoderError> {
    let color = color_of(f);
    let ts = f.pts().map_or(fallback_ts, |p| p as f64 / 1e6);
    if f.format() == ff::format::Pixel::VAAPI {
        if let Some((guard, err)) = prime {
            let Some(guard) = guard else { return Ok(None) };
            match map_prime(f, guard) {
                Ok(p) => {
                    return Ok(Some(VideoFrame { width: f.width(), height: f.height(), y: vec![], uv: vec![], timestamp: ts, color, prime: Some(Arc::new(p)) }));
                }
                Err(e) => *err = Some(e),
            }
        }
        let mut sw = ff::frame::Video::empty();
        unsafe {
            (*sw.as_mut_ptr()).format = sys::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
            let r = sys::av_hwframe_transfer_data(sw.as_mut_ptr(), f.as_ptr(), 0);
            if r < 0 {
                return Err(ff::Error::from(r).into());
            }
        }
        return copy_planes(&sw, ts, color).map(Some);
    }
    copy_planes(f, ts, color).map(Some)
}

fn copy_planes(f: &ff::frame::Video, timestamp: f64, color: ColorInfo) -> Result<VideoFrame, DecoderError> {
    let (w, h) = (f.width() as usize, f.height() as usize);
    if w == 0 || h == 0 {
        return Err(DecoderError::Ffmpeg("empty frame".into()));
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut y = Vec::with_capacity(w * h);
    let ys = f.stride(0);
    for row in 0..h {
        y.extend_from_slice(&f.data(0)[row * ys..row * ys + w]);
    }
    let mut uv = Vec::with_capacity(cw * ch * 2);
    match f.format() {
        ff::format::Pixel::NV12 => {
            let s = f.stride(1);
            for row in 0..ch {
                uv.extend_from_slice(&f.data(1)[row * s..row * s + cw * 2]);
            }
        }
        ff::format::Pixel::YUV420P | ff::format::Pixel::YUVJ420P => {
            let (su, sv) = (f.stride(1), f.stride(2));
            let (u, v) = (f.data(1), f.data(2));
            for row in 0..ch {
                for x in 0..cw {
                    uv.push(u[row * su + x]);
                    uv.push(v[row * sv + x]);
                }
            }
        }
        other => return Err(DecoderError::Ffmpeg(format!("unsupported pixel format {other:?} (8-bit 4:2:0 only)"))),
    }
    Ok(VideoFrame { width: w as u32, height: h as u32, y, uv, timestamp, color, prime: None })
}

#[cfg(test)]
mod select_tests {
    use super::*;

    #[test]
    fn auto_picks_by_size() {
        assert_eq!(auto_kind(None, false), DecoderKind::Vaapi);
        assert_eq!(auto_kind(Some((1280, 720)), false), DecoderKind::Vaapi);
        assert_eq!(auto_kind(Some((1920, 1080)), false), DecoderKind::Vaapi);
        assert_eq!(auto_kind(Some((1920, 1200)), false), DecoderKind::Vaapi);
        assert_eq!(auto_kind(Some((2560, 1440)), false), DecoderKind::Software);
        assert_eq!(auto_kind(Some((3840, 2160)), false), DecoderKind::Software);
        assert_eq!(auto_kind(Some((3024, 1964)), false), DecoderKind::Software);
        // Zero-copy keeps hardware at every size.
        assert_eq!(auto_kind(Some((3840, 2160)), true), DecoderKind::Vaapi);
        assert_eq!(auto_kind(Some((7680, 4320)), true), DecoderKind::Vaapi);
        assert_eq!(auto_kind(None, true), DecoderKind::Vaapi);
    }
}

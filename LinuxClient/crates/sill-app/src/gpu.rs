//! Adapter and device. Vulkan only (Mesa ANV on the dev machine; lavapipe where there is no GPU).

use crate::zerocopy::Importer;
use sill_media::zerocopy as zc;
use std::sync::Arc;

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    pub adapter: wgpu::Adapter,
    /// Set when the device can import the decoder's dmabufs (VAAPI -> Vulkan without a CPU copy).
    pub importer: Option<Arc<Importer>>,
    /// Why zero-copy is not in use (or "not asked for").
    pub zero_copy_note: String,
}

impl Gpu {
    /// A device, compatible with `surface` when given, without zero-copy. `Err` says why there is none
    /// (callers that can do without, tests among them, skip with that message).
    pub fn request(instance: wgpu::Instance, surface: Option<&wgpu::Surface<'_>>) -> Result<Gpu, String> {
        Gpu::request_with(instance, surface, false)
    }

    /// As [`request`](Self::request); with `zero_copy` a device with the dmabuf extensions when the
    /// adapter has them (else an ordinary one, and `zero_copy_note` says why).
    pub fn request_with(instance: wgpu::Instance, surface: Option<&wgpu::Surface<'_>>, zero_copy: bool) -> Result<Gpu, String> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower, // the integrated GPU decodes and presents; no need for more
            compatible_surface: surface,
            force_fallback_adapter: false,
        }))
        .map_err(|e| format!("no Vulkan adapter: {e}"))?;
        let info = adapter.get_info();
        let limits = wgpu::Limits::default().using_resolution(adapter.limits());
        let mut note = "not asked for".to_string();
        let mut opened = None;
        if zero_copy {
            match crate::zerocopy::open_device(&adapter, limits.clone()) {
                Ok((d, q, imp)) => opened = Some((d, q, Some(Arc::new(imp)))),
                Err(e) => note = e,
            }
        }
        let (device, queue, importer) = match opened {
            Some(o) => o,
            None => {
                let (d, q) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("sill"),
                    required_features: wgpu::Features::empty(),
                    required_limits: limits,
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::Off,
                }))
                .map_err(|e| format!("no device: {e}"))?;
                (d, q, None)
            }
        };
        if importer.is_some() {
            note = "dmabuf import available".into();
        }
        Ok(Gpu { instance, device, queue, info, adapter, importer, zero_copy_note: note })
    }

    pub fn instance() -> wgpu::Instance {
        wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..Default::default() })
    }

    /// Headless: no surface at all.
    pub fn headless() -> Result<Gpu, String> {
        Gpu::request(Gpu::instance(), None)
    }

    /// The window's device: zero-copy unless `SILL_ZEROCOPY=0`. Tells the decoder (process-wide) whether
    /// it may export dmabufs and logs which path is active.
    pub fn request_for_app(instance: wgpu::Instance, surface: Option<&wgpu::Surface<'_>>) -> Result<Gpu, String> {
        let wanted = zc::wanted();
        let g = Gpu::request_with(instance, surface, wanted)?;
        g.announce(wanted);
        Ok(g)
    }

    /// Set the process-wide zero-copy state from this device and say so.
    pub fn announce(&self, wanted: bool) {
        if self.importer.is_some() {
            zc::set_state(zc::State::Active);
            eprintln!("video path: zero-copy VAAPI dmabuf -> Vulkan ({}); the CPU upload path stays as the fallback", self.info.name);
        } else {
            zc::set_state(zc::State::Off);
            let why = if wanted { self.zero_copy_note.as_str() } else { "SILL_ZEROCOPY=0" };
            eprintln!("video path: CPU upload (zero-copy off: {why})");
        }
    }

    pub fn describe(&self) -> String {
        format!("{} ({:?}, {})", self.info.name, self.info.device_type, self.info.driver)
    }
}

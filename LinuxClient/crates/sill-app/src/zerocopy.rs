//! VAAPI -> Vulkan zero-copy, the GPU half: a device made with the dmabuf extensions, and the import of
//! a decoded picture (a DRM PRIME dmabuf from the decoder) as an NV12 image whose two planes the video
//! shader samples as R8 and RG8, as it does for pictures uploaded from memory.
//!
//! Lifetime: the imported image's drop callback owns the [`PrimeFrame`], so the dmabuf and the VAAPI
//! surface behind it stay alive until wgpu destroys the texture, which it does only after the GPU work
//! that used it has finished.

use ash::{ext, khr, vk};
use sill_media::zerocopy::{self, ImportPlan};
use sill_media::PrimeFrame;
use std::ffi::CStr;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd};
use std::sync::Arc;
use wgpu::hal::api::Vulkan;

/// What the device must have for the import (promoted to core in newer Vulkan, still listed by drivers).
fn needed_extensions() -> [&'static CStr; 3] {
    [khr::external_memory_fd::NAME, ext::external_memory_dma_buf::NAME, ext::image_drm_format_modifier::NAME]
}

const NV12_VK: vk::Format = vk::Format::G8_B8R8_2PLANE_420_UNORM;

pub struct Importer {
    dev: ash::Device,
    fd_ext: khr::external_memory_fd::Device,
    mem_props: vk::PhysicalDeviceMemoryProperties,
    /// `(modifier, memory plane count)` the device samples NV12 with.
    pub modifiers: Vec<(u64, u32)>,
}

/// An imported picture: the texture (kept alive here), its two plane views and the size to crop to.
pub struct Imported {
    pub texture: wgpu::Texture,
    pub y: wgpu::TextureView,
    pub uv: wgpu::TextureView,
    pub coded: (u32, u32),
}

/// A device with the dmabuf import extensions on `adapter`, or why not. The caller falls back to an
/// ordinary device on `Err`.
pub fn open_device(adapter: &wgpu::Adapter, limits: wgpu::Limits) -> Result<(wgpu::Device, wgpu::Queue, Importer), String> {
    let features = wgpu::Features::TEXTURE_FORMAT_NV12;
    if !adapter.features().contains(features) {
        return Err("the adapter has no NV12 textures".into());
    }
    let made = unsafe { adapter.as_hal::<Vulkan, _, _>(|hal| match hal {
        Some(hal) => build(hal, features),
        None => Err("not a Vulkan adapter".to_string()),
    }) }?;
    let (open, importer) = made;
    let (device, queue) = unsafe {
        adapter.create_device_from_hal(
            open,
            &wgpu::DeviceDescriptor {
                label: Some("sill zero-copy"),
                required_features: features,
                required_limits: limits,
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            },
        )
    }
    .map_err(|e| format!("wgpu device: {e}"))?;
    Ok((device, queue, importer))
}

type Opened = (wgpu::hal::OpenDevice<wgpu::hal::api::Vulkan>, Importer);

/// The raw Vulkan device with the extra extensions, wrapped as wgpu-hal's.
fn build(hal: &wgpu::hal::vulkan::Adapter, features: wgpu::Features) -> Result<Opened, String> {
    let inst = hal.shared_instance().raw_instance().clone();
    let phd = hal.raw_physical_device();
    let listed: Vec<_> = unsafe { inst.enumerate_device_extension_properties(phd) }.map_err(|e| format!("extension list: {e}"))?;
    let has = |n: &CStr| listed.iter().any(|p| p.extension_name_as_c_str().is_ok_and(|x| x == n));
    for n in needed_extensions() {
        if !has(n) {
            return Err(format!("the driver lacks {}", n.to_string_lossy()));
        }
    }
    // Modifiers the device samples NV12 with.
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
    let mut props = vk::FormatProperties2::default().push_next(&mut list);
    unsafe { inst.get_physical_device_format_properties2(phd, NV12_VK, &mut props) };
    let n = list.drm_format_modifier_count as usize;
    let mut mods = vec![vk::DrmFormatModifierPropertiesEXT::default(); n];
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut mods);
    let mut props = vk::FormatProperties2::default().push_next(&mut list);
    unsafe { inst.get_physical_device_format_properties2(phd, NV12_VK, &mut props) };
    let modifiers: Vec<(u64, u32)> = mods
        .iter()
        .filter(|m| m.drm_format_modifier_tiling_features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE))
        .map(|m| (m.drm_format_modifier, m.drm_format_modifier_plane_count))
        .collect();
    if modifiers.is_empty() {
        return Err("the driver lists no format modifier for sampled NV12".into());
    }

    let mut exts = hal.required_device_extensions(features);
    for n in needed_extensions() {
        if !exts.contains(&n) {
            exts.push(n);
        }
    }
    let mut phd_features = hal.physical_device_features(&exts, features);
    let queue_infos = [vk::DeviceQueueCreateInfo::default().queue_family_index(0).queue_priorities(&[1.0])];
    let ptrs: Vec<_> = exts.iter().map(|e| e.as_ptr()).collect();
    let pre = vk::DeviceCreateInfo::default().queue_create_infos(&queue_infos).enabled_extension_names(&ptrs);
    let info = phd_features.add_to_device_create(pre);
    let raw = unsafe { inst.create_device(phd, &info, None) }.map_err(|e| format!("vkCreateDevice: {e}"))?;
    let mem_props = unsafe { inst.get_physical_device_memory_properties(phd) };
    let importer = Importer { dev: raw.clone(), fd_ext: khr::external_memory_fd::Device::new(&inst, &raw), mem_props, modifiers };
    let open = unsafe { hal.device_from_raw(raw, None, &exts, features, &wgpu::MemoryHints::default(), 0, 0) }
        .map_err(|e| format!("wgpu-hal device: {e:?}"))?;
    Ok((open, importer))
}

impl Drop for Imported {
    fn drop(&mut self) {
        // Free the image as soon as the GPU is done with it, not whenever the device is torn down: the
        // drop callback calls Vulkan, which must happen while the device still exists.
        self.texture.destroy();
    }
}

impl Importer {
    /// Import `frame` into `device` (the device this importer's was made with).
    pub fn import(&self, device: &wgpu::Device, frame: &Arc<PrimeFrame>) -> Result<Imported, String> {
        let plan = zerocopy::plan_import(&frame.desc, frame.coded)?;
        if !zerocopy::modifier_supported(&self.modifiers, &plan) {
            return Err(format!("the device does not sample NV12 with modifier {:#x}", plan.modifier));
        }
        let (image, memory) = unsafe { self.make_image(&plan)? };
        let dev = self.dev.clone();
        let keep = frame.clone();
        let drop_cb: wgpu::hal::DropCallback = Box::new(move || {
            unsafe {
                dev.destroy_image(image, None);
                dev.free_memory(memory, None);
            }
            drop(keep);
        });
        let size = wgpu::Extent3d { width: plan.coded.0, height: plan.coded.1, depth_or_array_layers: 1 };
        let hal_desc = wgpu::hal::TextureDescriptor {
            label: Some("dmabuf nv12"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::NV12,
            usage: wgpu::TextureUses::RESOURCE,
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: vec![],
        };
        let hal_tex = unsafe { wgpu::hal::vulkan::Device::texture_from_raw(image, &hal_desc, Some(drop_cb)) };
        let texture = unsafe {
            device.create_texture_from_hal::<Vulkan>(
                hal_tex,
                &wgpu::TextureDescriptor {
                    label: Some("dmabuf nv12"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::NV12,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            )
        };
        let plane = |aspect| {
            texture.create_view(&wgpu::TextureViewDescriptor { aspect, ..Default::default() })
        };
        let (y, uv) = (plane(wgpu::TextureAspect::Plane0), plane(wgpu::TextureAspect::Plane1));
        Ok(Imported { texture, y, uv, coded: plan.coded })
    }

    /// The Vulkan image over the dmabuf, with its memory. On `Err` nothing is left allocated.
    unsafe fn make_image(&self, plan: &ImportPlan) -> Result<(vk::Image, vk::DeviceMemory), String> {
        let d = &self.dev;
        let layouts = [
            vk::SubresourceLayout::default().offset(plan.planes[0].0).row_pitch(plan.planes[0].1),
            vk::SubresourceLayout::default().offset(plan.planes[1].0).row_pitch(plan.planes[1].1),
        ];
        let mut drm = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default().drm_format_modifier(plan.modifier).plane_layouts(&layouts);
        let mut ext_info = vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let views = [vk::Format::R8_UNORM, vk::Format::R8G8_UNORM];
        let mut fmt_list = vk::ImageFormatListCreateInfo::default().view_formats(&views);
        let info = vk::ImageCreateInfo::default()
            .flags(vk::ImageCreateFlags::MUTABLE_FORMAT)
            .image_type(vk::ImageType::TYPE_2D)
            .format(NV12_VK)
            .extent(vk::Extent3D { width: plan.coded.0, height: plan.coded.1, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut ext_info)
            .push_next(&mut drm)
            .push_next(&mut fmt_list);
        let image = d.create_image(&info, None).map_err(|e| format!("vkCreateImage: {e}"))?;
        let fail = |what: &str, e: vk::Result| {
            d.destroy_image(image, None);
            format!("{what}: {e}")
        };
        let reqs = d.get_image_memory_requirements(image);
        // Vulkan takes ownership of the fd it is given; keep the decoder's.
        let fd = BorrowedFd::borrow_raw(plan.fd).try_clone_to_owned().map_err(|e| {
            d.destroy_image(image, None);
            format!("dup dmabuf fd: {e}")
        })?;
        let mut fd_props = vk::MemoryFdPropertiesKHR::default();
        if let Err(e) = self.fd_ext.get_memory_fd_properties(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT, fd.as_raw_fd(), &mut fd_props) {
            return Err(fail("vkGetMemoryFdPropertiesKHR", e));
        }
        let fd_bits = fd_props.memory_type_bits;
        let bits = reqs.memory_type_bits & fd_bits;
        let Some(type_index) = pick_memory_type(bits, &self.mem_props) else {
            d.destroy_image(image, None);
            return Err("no memory type fits both the image and the dmabuf".into());
        };
        let raw_fd = fd.into_raw_fd();
        let mut import = vk::ImportMemoryFdInfoKHR::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT).fd(raw_fd);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let alloc = vk::MemoryAllocateInfo::default().allocation_size(reqs.size).memory_type_index(type_index).push_next(&mut import).push_next(&mut dedicated);
        let memory = match d.allocate_memory(&alloc, None) {
            Ok(m) => m,
            Err(e) => {
                // Vulkan did not take the descriptor: close it here.
                drop(std::os::fd::OwnedFd::from_raw_fd(raw_fd));
                return Err(fail("vkAllocateMemory", e));
            }
        };
        if let Err(e) = d.bind_image_memory(image, memory, 0) {
            d.free_memory(memory, None);
            return Err(fail("vkBindImageMemory", e));
        }
        Ok((image, memory))
    }
}

/// The first memory type in `bits`, device-local ones first.
pub fn pick_memory_type(bits: u32, props: &vk::PhysicalDeviceMemoryProperties) -> Option<u32> {
    let types = &props.memory_types[..props.memory_type_count as usize];
    let ok = |i: u32| bits & (1 << i) != 0;
    (0..types.len() as u32)
        .find(|&i| ok(i) && types[i as usize].property_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL))
        .or_else(|| (0..types.len() as u32).find(|&i| ok(i)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(flags: &[vk::MemoryPropertyFlags]) -> vk::PhysicalDeviceMemoryProperties {
        let mut p = vk::PhysicalDeviceMemoryProperties { memory_type_count: flags.len() as u32, ..Default::default() };
        for (i, f) in flags.iter().enumerate() {
            p.memory_types[i].property_flags = *f;
        }
        p
    }

    #[test]
    fn memory_type_prefers_device_local_within_the_allowed_bits() {
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE;
        let dev = vk::MemoryPropertyFlags::DEVICE_LOCAL;
        let p = props(&[host, dev, host | dev]);
        assert_eq!(pick_memory_type(0b111, &p), Some(1));
        assert_eq!(pick_memory_type(0b101, &p), Some(2));
        assert_eq!(pick_memory_type(0b001, &p), Some(0));
        assert_eq!(pick_memory_type(0, &p), None);
        assert_eq!(pick_memory_type(0b1000, &p), None, "bits beyond the listed types");
    }
}

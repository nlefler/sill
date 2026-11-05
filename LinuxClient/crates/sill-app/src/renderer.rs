//! Draws the decoded NV12 picture aspect-fit on black, then the Mac's pointer sprite and the HUD.
//! The same code renders to a swapchain and, in tests and `--dump`, to an offscreen texture.

use bytemuck::{Pod, Zeroable};
use sill_media::cursor::{self, CursorImage};
use sill_media::{Matrix, VideoFrame};
use crate::zerocopy::Importer;
use sill_media::zerocopy as zc;
use std::sync::Arc;
use wgpu::util::DeviceExt;

use crate::zerocopy::Imported;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct QuadUniform {
    rect: [f32; 4],
    params: [f32; 4],
    /// Video: the part of the texture the picture covers (x, y), the rest is alignment padding.
    crop: [f32; 4],
}

impl QuadUniform {
    fn new(rect: [f32; 4], params: [f32; 4]) -> Self {
        QuadUniform { rect, params, crop: [1.0, 1.0, 0.0, 0.0] }
    }
}

struct VideoTex {
    /// The shown size.
    w: u32,
    h: u32,
    /// Textures uploaded from memory (none for an imported dmabuf).
    y: Option<wgpu::Texture>,
    uv: Option<wgpu::Texture>,
    /// An imported dmabuf, kept until the next picture replaces it.
    _imported: Option<Imported>,
    bind: wgpu::BindGroup,
}

struct Overlay {
    w: u32,
    h: u32,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    video_pipeline: wgpu::RenderPipeline,
    overlay_pipeline: wgpu::RenderPipeline,
    video_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    video_uniform: wgpu::Buffer,
    video: Option<VideoTex>,
    color_params: [f32; 4],
    /// The shown part of the video texture (an imported dmabuf is the aligned surface).
    crop: [f32; 4],
    importer: Option<Arc<Importer>>,
    cursor: Option<(CursorImage, Overlay)>,
    hud: Option<Overlay>,
    format: wgpu::TextureFormat,
    /// The video area in window pixels (x, y, w, h); the whole window when none.
    area: Option<(f64, f64, f64, f64)>,
}

fn matrix_k(m: Matrix) -> (f32, f32) {
    match m {
        Matrix::Bt601 => (0.299, 0.114),
        Matrix::Bt709 => (0.2126, 0.0722),
        Matrix::Bt2020 => (0.2627, 0.0593),
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // Imported dmabuf images are freed through callbacks that call Vulkan: let the GPU finish and run
        // them while the device is still alive.
        self.video = None;
        let _ = self.device.poll(wgpu::PollType::Wait);
    }
}

impl Renderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat) -> Renderer {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sill shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let uniform_entry = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let sampler_entry = wgpu::BindGroupLayoutEntry {
            binding: 3,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let video_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video"),
            entries: &[uniform_entry, tex_entry(1), tex_entry(2), sampler_entry],
        });
        let overlay_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay"),
            entries: &[uniform_entry, tex_entry(1), sampler_entry],
        });
        let pipeline = |label, bgl: &wgpu::BindGroupLayout, fs, blend| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[bgl],
                push_constant_ranges: &[],
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };
        let video_pipeline = pipeline("video", &video_bgl, "fs_video", None);
        let overlay_pipeline = pipeline("overlay", &overlay_bgl, "fs_overlay", Some(wgpu::BlendState::ALPHA_BLENDING));
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let video_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("video uniform"),
            contents: bytemuck::bytes_of(&QuadUniform::new([0.0; 4], [0.0; 4])),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        Renderer {
            device,
            queue,
            video_pipeline,
            overlay_pipeline,
            video_bgl,
            sampler,
            video_uniform,
            video: None,
            color_params: [0.2126, 0.0722, 0.0, 0.0],
            crop: [1.0, 1.0, 0.0, 0.0],
            importer: None,
            cursor: None,
            hud: None,
            format, area: None,
        }
    }

    pub fn video_size(&self) -> Option<(u32, u32)> {
        self.video.as_ref().map(|v| (v.w, v.h))
    }

    fn make_tex(&self, label: &str, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    fn write_tex(&self, tex: &wgpu::Texture, data: &[u8], bytes_per_row: u32, w: u32, h: u32) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }

    /// The device's dmabuf importer (zero-copy), when it has one.
    pub fn set_importer(&mut self, importer: Option<Arc<Importer>>) {
        self.importer = importer;
    }

    /// Make `frame` the picture: a dmabuf imported (no copy), else planes uploaded, textures made again
    /// when the size changed. `false` when the picture could not be shown; a dmabuf that cannot be
    /// imported turns zero-copy off for good, and the decoder then reads pictures back.
    pub fn upload(&mut self, frame: &VideoFrame) -> bool {
        if let Some(prime) = &frame.prime {
            return self.import(frame, prime);
        }
        let (w, h) = (frame.width, frame.height);
        if frame.y.len() < (w * h) as usize || w == 0 || h == 0 {
            return false;
        }
        let same = matches!(&self.video, Some(v) if (v.w, v.h) == (w, h) && v.y.is_some());
        if !same {
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let y = self.make_tex("luma", w, h, wgpu::TextureFormat::R8Unorm);
            let uv = self.make_tex("chroma", cw, ch, wgpu::TextureFormat::Rg8Unorm);
            let view = |t: &wgpu::Texture| t.create_view(&Default::default());
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("video"),
                layout: &self.video_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.video_uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view(&y)) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view(&uv)) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            self.video = Some(VideoTex { w, h, y: Some(y), uv: Some(uv), _imported: None, bind });
        }
        let v = self.video.as_ref().unwrap();
        let (ty, tuv) = (v.y.as_ref().unwrap(), v.uv.as_ref().unwrap());
        self.write_tex(ty, &frame.y, w, w, h);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        if frame.uv.len() >= (cw * ch * 2) as usize {
            self.write_tex(tuv, &frame.uv, cw * 2, cw, ch);
        }
        self.crop = [1.0, 1.0, 0.0, 0.0];
        self.set_color(frame);
        true
    }

    fn set_color(&mut self, frame: &VideoFrame) {
        let (kr, kb) = matrix_k(frame.color.matrix);
        self.color_params = [kr, kb, if frame.color.full_range { 1.0 } else { 0.0 }, 0.0];
    }

    fn import(&mut self, frame: &VideoFrame, prime: &Arc<sill_media::PrimeFrame>) -> bool {
        let Some(importer) = self.importer.clone() else {
            return false;
        };
        // TEST ONLY: `SILL_TEST_ZEROCOPY_FAIL=1` makes every import fail, to exercise the fallback.
        let imported = if std::env::var_os("SILL_TEST_ZEROCOPY_FAIL").is_some() { Err("test: import refused".to_string()) } else { importer.import(&self.device, prime) };
        match imported {
            Ok(imp) => {
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("video dmabuf"),
                    layout: &self.video_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: self.video_uniform.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&imp.y) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&imp.uv) },
                        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    ],
                });
                let c = zc::crop_scale((frame.width, frame.height), imp.coded);
                self.crop = [c[0], c[1], 0.0, 0.0];
                self.set_color(frame);
                self.video = Some(VideoTex { w: frame.width, h: frame.height, y: None, uv: None, _imported: Some(imp), bind });
                true
            }
            Err(e) => {
                eprintln!("video path: dmabuf import failed ({e}); falling back to CPU upload");
                zc::set_state(zc::State::Off);
                self.importer = None;
                false
            }
        }
    }

    fn make_overlay(&self, w: u32, h: u32, rgba: &[u8]) -> Overlay {
        let tex = self.make_tex("overlay", w, h, wgpu::TextureFormat::Rgba8Unorm);
        self.write_tex(&tex, rgba, w * 4, w, h);
        let uniform = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("overlay uniform"),
            contents: bytemuck::bytes_of(&QuadUniform::new([0.0; 4], [0.0; 4])),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bgl = self.overlay_pipeline.get_bind_group_layout(0);
        let view = tex.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        Overlay { w, h, uniform, bind }
    }

    pub fn set_cursor(&mut self, img: Option<CursorImage>) {
        self.cursor = img.map(|i| {
            let o = self.make_overlay(i.width, i.height, &i.rgba);
            (i, o)
        });
    }

    /// Where the picture goes: the part of the window the panels leave (window pixels).
    pub fn set_area(&mut self, area: Option<(f64, f64, f64, f64)>) {
        self.area = area;
    }

    /// The HUD's image (see [`crate::hud::render_text`]) or none.
    pub fn set_hud(&mut self, img: Option<(u32, u32, Vec<u8>)>) {
        self.hud = img.map(|(w, h, rgba)| self.make_overlay(w, h, &rgba));
    }

    fn place(&self, ov: &Overlay, x: f64, y: f64, w: f64, h: f64, win: (f64, f64)) {
        let ndc = |px: f64, total: f64| (px / total * 2.0 - 1.0) as f32;
        let rect = [ndc(x, win.0), -ndc(y + h, win.1), ndc(x + w, win.0), -ndc(y, win.1)];
        self.queue.write_buffer(&ov.uniform, 0, bytemuck::bytes_of(&QuadUniform::new(rect, [0.0; 4])));
    }

    /// Draw one frame into `view` (`size` pixels). `pointer` is the Mac pointer's tip as fractions of the
    /// video frame; `scale_factor` sizes the sprite (points to pixels). Black where there is no picture.
    pub fn render(&self, view: &wgpu::TextureView, size: (u32, u32), scale_factor: f64, pointer: Option<(f64, f64)>) {
        self.render_with(view, size, scale_factor, pointer, &mut |_| {});
    }

    /// As [`render`](Self::render), then `extra` draws on top of the picture (the panels), after the picture's pass was submitted.
    pub fn render_with(&self, view: &wgpu::TextureView, size: (u32, u32), scale_factor: f64, pointer: Option<(f64, f64)>, extra: &mut dyn FnMut(&wgpu::TextureView)) {
        let win = (size.0 as f64, size.1 as f64);
        let mut video_rect = None;
        if let Some(v) = &self.video {
            let (ax, ay, aw, ah) = self.area.unwrap_or((0.0, 0.0, win.0, win.1));
            let mut r = cursor::video_rect(aw, ah, v.w as f64, v.h as f64);
            r.x += ax;
            r.y += ay;
            let ndc = |px: f64, total: f64| (px / total * 2.0 - 1.0) as f32;
            let rect = [ndc(r.x, win.0), -ndc(r.y + r.h, win.1), ndc(r.x + r.w, win.0), -ndc(r.y, win.1)];
            self.queue.write_buffer(&self.video_uniform, 0, bytemuck::bytes_of(&QuadUniform { rect, params: self.color_params, crop: self.crop }));
            video_rect = Some(r);
        }
        let mut cursor_drawn = None;
        if let (Some((img, ov)), Some(r), Some((px, py))) = (&self.cursor, video_rect, pointer) {
            let tip = (r.x + px * r.w, r.y + py * r.h);
            let (ox, oy) = cursor::sprite_origin(tip, img, scale_factor);
            self.place(ov, ox, oy, img.size.0 as f64 * scale_factor, img.size.1 as f64 * scale_factor, win);
            cursor_drawn = Some(ov);
        }
        if let Some(h) = &self.hud {
            self.place(h, 8.0, 8.0, h.w as f64, h.h as f64, win);
        }
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sill"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some(v) = &self.video {
                pass.set_pipeline(&self.video_pipeline);
                pass.set_bind_group(0, &v.bind, &[]);
                pass.draw(0..4, 0..1);
            }
            pass.set_pipeline(&self.overlay_pipeline);
            for ov in cursor_drawn.into_iter().chain(self.hud.as_ref()) {
                pass.set_bind_group(0, &ov.bind, &[]);
                pass.draw(0..4, 0..1);
            }
        }
        self.queue.submit([enc.finish()]);
        extra(view);
    }

    /// Render into a fresh offscreen texture of the renderer's format and read it back as RGBA (tests, `--dump`).
    pub fn render_to_image(&self, size: (u32, u32), scale_factor: f64, pointer: Option<(f64, f64)>) -> Vec<u8> {
        self.render_to_image_with(size, scale_factor, pointer, &mut |_| {})
    }

    pub fn render_to_image_with(&self, size: (u32, u32), scale_factor: f64, pointer: Option<(f64, f64)>, extra: &mut dyn FnMut(&wgpu::TextureView)) -> Vec<u8> {
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.render_with(&tex.create_view(&Default::default()), size, scale_factor, pointer, extra);
        let row = (size.0 * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (row * size.1) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(size.1) } },
            wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::Wait);
        rx.recv().expect("map callback").expect("map");
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((size.0 * size.1 * 4) as usize);
        for r in 0..size.1 {
            out.extend_from_slice(&data[(r * row) as usize..(r * row + size.0 * 4) as usize]);
        }
        drop(data);
        buf.unmap();
        if matches!(self.format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb) {
            for p in out.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
        }
        out
    }
}

// Reusable GPU pipeline for two-texture fullscreen effects.
//
// Extends SimpleBlitHelper's pattern with a second texture binding for effects
// that read from two sources (e.g. bloom reads _MainTex + _BloomTex, CRT reads
// _MainTex + _GlowTex, halation reads _MainTex + _HaloTex).
//
// Includes a 1x1 dummy texture for passes that don't read the secondary texture.
//
// Uniforms use a ring buffer with 256-byte-aligned slots to avoid
// per-frame Metal buffer allocation. Each `draw()` writes to the next
// slot via `queue.write_buffer()`.

use std::cell::Cell;
use crate::gpu_encoder::GpuEncoder;

const RING_SLOTS: u64 = 64;
const UNIFORM_OFFSET_ALIGN: u64 = 256;

/// BGL entries for the dual-texture render pipeline (shared between wgpu and hal).
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
const DUAL_BLIT_BGL_ENTRIES: [wgpu::BindGroupLayoutEntry; 4] = [
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: None,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 1,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 2,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 3,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
];

/// Cached bind group keyed by main + secondary texture view pointers.
/// Reused across frames when the same textures are bound (common case).
struct CachedBG {
    bind_group: wgpu::BindGroup,
    main_ptr: usize,
    secondary_ptr: usize,
}

pub struct DualTextureBlitHelper {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
    ring_buffer: wgpu::Buffer,
    uniform_size: u64,
    slot_stride: u64,
    ring_index: Cell<u64>,
    /// 1x1 placeholder bound as secondary_tex when it's not read.
    pub dummy_view: wgpu::TextureView,
    cached: Option<CachedBG>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_pipeline: Option<crate::hal_pipeline::HalRenderPipeline>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_sampler: Option<crate::hal_context::MetalSampler>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    ring_mapped_ptr: Option<*mut u8>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_ring_ptr: Option<*const crate::hal_context::MetalBuffer>,
    /// Cached hal view of the 1x1 dummy texture.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_dummy_view_ptr: Option<*const crate::hal_context::MetalTextureView>,
}

#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
unsafe impl Send for DualTextureBlitHelper {}

impl DualTextureBlitHelper {
    /// Create a new two-texture effect pipeline.
    ///
    /// `uniform_size` — byte size of the effect's uniform struct (must be Pod).
    pub fn new(
        device: &wgpu::Device,
        shader_source: &str,
        label: &str,
        uniform_size: u64,
        hal_ctx: Option<&crate::hal_context::HalContext>,
    ) -> Self {
        let _ = &hal_ctx;
        let format = wgpu::TextureFormat::Rgba16Float;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&format!("{label} BGL")),
            entries: &[
                // binding 0: uniforms (dynamic offset for bind group caching)
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: std::num::NonZero::new(uniform_size),
                    },
                    count: None,
                },
                // binding 1: main_tex (_MainTex)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // binding 2: sampler (shared for both textures)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // binding 3: secondary_tex (_BloomTex, _GlowTex, _HaloTex, _PrevTex, etc.)
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("{label} Layout")),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&format!("{label} Pipeline")),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(&format!("{label} Sampler")),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let slot_stride =
            (uniform_size + UNIFORM_OFFSET_ALIGN - 1) & !(UNIFORM_OFFSET_ALIGN - 1);

        // 1x1 dummy texture for secondary_tex binding when it's not read
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("{label} Dummy")),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy_view = dummy_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // --- hal pipeline + shared-memory ring buffer ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (hal_pipeline, hal_sampler, ring_buffer, ring_mapped_ptr, hal_ring_ptr,
             hal_dummy_view_ptr) =
        if let Some(ctx) = hal_ctx {
            use wgpu::hal::Device as HalDevice;
            let hal_pipe = crate::hal_pipeline::create_render_pipeline(
                ctx, shader_source, "vs_main", "fs_main",
                &DUAL_BLIT_BGL_ENTRIES,
                wgpu::TextureFormat::Rgba16Float, label,
            );
            let hal_samp = unsafe {
                ctx.device()
                    .create_sampler(&wgpu::hal::SamplerDescriptor {
                        label: Some(label),
                        address_modes: [wgpu::AddressMode::ClampToEdge; 3],
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                        lod_clamp: 0.0..32.0,
                        compare: None,
                        anisotropy_clamp: 1,
                        border_color: None,
                    })
                    .expect("Failed to create hal dual blit sampler")
            };
            let buf_size = slot_stride * RING_SLOTS;
            let hal_buf = unsafe {
                ctx.device()
                    .create_buffer(&wgpu::hal::BufferDescriptor {
                        label: Some(label),
                        size: buf_size,
                        usage: wgpu::wgt::BufferUses::UNIFORM
                            | wgpu::wgt::BufferUses::MAP_WRITE,
                        memory_flags: wgpu::hal::MemoryFlags::PREFER_COHERENT,
                    })
                    .expect("Failed to create hal dual blit ring buffer")
            };
            let mapping = unsafe {
                ctx.device()
                    .map_buffer(&hal_buf, 0..buf_size)
                    .expect("Failed to map hal dual blit ring buffer")
            };
            let mapped_ptr = mapping.ptr.as_ptr();
            let wgpu_buf = unsafe {
                device.create_buffer_from_hal::<wgpu::hal::api::Metal>(
                    hal_buf,
                    &wgpu::BufferDescriptor {
                        label: Some(&format!("{label} Ring UBO")),
                        size: buf_size,
                        usage: wgpu::BufferUsages::UNIFORM
                            | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    },
                )
            };
            let ring_hal_ptr = {
                let guard = unsafe { wgpu_buf.as_hal::<wgpu::hal::api::Metal>() }
                    .expect("ring buffer not Metal");
                let ptr: *const _ = &*guard;
                ptr
            };
            // Cache hal view of dummy texture
            let dummy_hal_ptr = {
                let guard = unsafe { dummy_view.as_hal::<wgpu::hal::api::Metal>() }
                    .expect("dummy view not Metal");
                let ptr: *const _ = &*guard;
                ptr
            };
            (Some(hal_pipe), Some(hal_samp), wgpu_buf, Some(mapped_ptr),
             Some(ring_hal_ptr), Some(dummy_hal_ptr))
        } else {
            let wgpu_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("{label} Ring UBO")),
                size: slot_stride * RING_SLOTS,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            (None, None, wgpu_buf, None, None, None)
        };

        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        let ring_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label} Ring UBO")),
            size: slot_stride * RING_SLOTS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            ring_buffer,
            uniform_size,
            slot_stride,
            ring_index: Cell::new(0),
            dummy_view,
            cached: None,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_sampler,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            ring_mapped_ptr,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_ring_ptr,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_dummy_view_ptr,
        }
    }

    /// Ensure the cached bind group is valid for the given main/secondary views.
    /// Uses dynamic uniform offset so the bind group can be reused across
    /// frames when the same textures are bound (saves ~10us per call).
    fn ensure_bind_group(
        &mut self,
        device: &wgpu::Device,
        main_view: &wgpu::TextureView,
        secondary_view: &wgpu::TextureView,
        label: &str,
    ) {
        let main_ptr = std::ptr::from_ref(main_view) as usize;
        let sec_ptr = std::ptr::from_ref(secondary_view) as usize;

        let needs_recreate = match &self.cached {
            Some(c) => c.main_ptr != main_ptr || c.secondary_ptr != sec_ptr,
            None => true,
        };

        if needs_recreate {
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.ring_buffer,
                            offset: 0,
                            size: std::num::NonZero::new(self.uniform_size),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(main_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(secondary_view),
                    },
                ],
            });
            self.cached = Some(CachedBG {
                bind_group,
                main_ptr,
                secondary_ptr: sec_ptr,
            });
        }
    }

    /// Execute a fullscreen pass reading only the main texture.
    /// Binds the internal 1x1 dummy as the secondary texture.
    ///
    /// Use this instead of `draw(..., &self.dummy_view, ...)` to avoid
    /// borrow conflicts (`&mut self` + `&self.dummy_view` on the same helper).
    pub fn draw_main_only(
        &mut self,
        gpu: &mut GpuEncoder,
        main_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let slot = self.ring_index.get() % RING_SLOTS;
        self.ring_index.set(self.ring_index.get() + 1);
        let byte_offset = slot * self.slot_stride;

        // --- hal path: encode via hal command encoder ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if gpu.has_hal_encoder() {
            type MetalApi = wgpu::hal::api::Metal;
            let main_ptr = {
                let g = unsafe { main_view.as_hal::<MetalApi>() }
                    .expect("main not Metal");
                &*g as *const _
            };
            let tgt_ptr = {
                let g = unsafe { target_view.as_hal::<MetalApi>() }
                    .expect("target not Metal");
                &*g as *const _
            };
            let dummy_ptr = self.hal_dummy_view_ptr
                .expect("hal dummy view not cached");
            let (hal_enc, hal_ctx) = unsafe { gpu.hal_encoder_mut() }.unwrap();
            unsafe {
                self.draw_hal(
                    hal_enc, hal_ctx, &*main_ptr, &*dummy_ptr, &*tgt_ptr,
                    uniform_bytes, width, height, true,
                );
            }
            return;
        }

        gpu.queue.write_buffer(&self.ring_buffer, byte_offset, uniform_bytes);

        // Inline ensure_bind_group with split borrows — avoids &mut self +
        // &self.dummy_view conflict that would occur if calling draw_inner.
        let main_ptr = std::ptr::from_ref(main_view) as usize;
        let sec_ptr = std::ptr::from_ref(&self.dummy_view) as usize;

        let needs_recreate = match &self.cached {
            Some(c) => c.main_ptr != main_ptr || c.secondary_ptr != sec_ptr,
            None => true,
        };

        if needs_recreate {
            let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.ring_buffer,
                            offset: 0,
                            size: std::num::NonZero::new(self.uniform_size),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(main_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&self.dummy_view),
                    },
                ],
            });
            self.cached = Some(CachedBG {
                bind_group,
                main_ptr,
                secondary_ptr: sec_ptr,
            });
        }

        {
            let ts = profiler.and_then(|p| p.render_timestamps(label, width, height));
            let mut pass = gpu.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: ts,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(
                0,
                &self.cached.as_ref().unwrap().bind_group,
                &[byte_offset as u32],
            );
            pass.draw(0..3, 0..1);
        }
    }

    /// Execute a fullscreen pass reading two textures.
    ///
    /// For passes that don't read the secondary texture, use `draw_main_only`.
    pub fn draw(
        &mut self,
        gpu: &mut GpuEncoder,
        main_view: &wgpu::TextureView,
        secondary_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        self.draw_inner(
            gpu, main_view, secondary_view, target_view,
            uniform_bytes, label, width, height, wgpu::StoreOp::Store, profiler,
        );
    }

    /// Like `draw`, but uses `StoreOp::Discard` — the target's tile memory is
    /// NOT written back to VRAM after the pass. Use for intermediate render
    /// targets that will be immediately overwritten or only read once.
    pub fn draw_discard(
        &mut self,
        gpu: &mut GpuEncoder,
        main_view: &wgpu::TextureView,
        secondary_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        self.draw_inner(
            gpu, main_view, secondary_view, target_view,
            uniform_bytes, label, width, height, wgpu::StoreOp::Discard, profiler,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_inner(
        &mut self,
        gpu: &mut GpuEncoder,
        main_view: &wgpu::TextureView,
        secondary_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
        store_op: wgpu::StoreOp,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let slot = self.ring_index.get() % RING_SLOTS;
        self.ring_index.set(self.ring_index.get() + 1);
        let byte_offset = slot * self.slot_stride;

        // --- hal path: encode via hal command encoder ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if gpu.has_hal_encoder() {
            type MetalApi = wgpu::hal::api::Metal;
            let main_ptr = {
                let g = unsafe { main_view.as_hal::<MetalApi>() }
                    .expect("main not Metal");
                &*g as *const _
            };
            let sec_ptr = {
                let g = unsafe { secondary_view.as_hal::<MetalApi>() }
                    .expect("secondary not Metal");
                &*g as *const _
            };
            let tgt_ptr = {
                let g = unsafe { target_view.as_hal::<MetalApi>() }
                    .expect("target not Metal");
                &*g as *const _
            };
            let (hal_enc, hal_ctx) = unsafe { gpu.hal_encoder_mut() }.unwrap();
            unsafe {
                self.draw_hal(
                    hal_enc, hal_ctx, &*main_ptr, &*sec_ptr, &*tgt_ptr,
                    uniform_bytes, width, height,
                    store_op == wgpu::StoreOp::Store,
                );
            }
            return;
        }

        gpu.queue.write_buffer(&self.ring_buffer, byte_offset, uniform_bytes);

        // Update cached bind group if textures changed (mutation done before
        // the render pass borrow to satisfy the borrow checker).
        self.ensure_bind_group(gpu.device, main_view, secondary_view, label);

        {
            let ts = profiler.and_then(|p| p.render_timestamps(label, width, height));
            let mut pass = gpu.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: store_op,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: ts,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(
                0,
                &self.cached.as_ref().unwrap().bind_group,
                &[byte_offset as u32],
            );
            pass.draw(0..3, 0..1);
        }
    }

    /// HAL path: encode a two-texture fullscreen render pass via hal encoder.
    /// Writes uniforms directly to shared-memory ring buffer (no API call).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) unsafe fn draw_hal(
        &self,
        hal_enc: &mut crate::hal_context::MetalCommandEncoder,
        hal_ctx: &crate::hal_context::HalContext,
        main_hal_view: &crate::hal_context::MetalTextureView,
        secondary_hal_view: &crate::hal_context::MetalTextureView,
        target_hal_view: &crate::hal_context::MetalTextureView,
        uniform_bytes: &[u8],
        width: u32,
        height: u32,
        store: bool,
    ) {
        use wgpu::hal::{self as hal, CommandEncoder as _, Device as _};

        let slot = self.ring_index.get() % RING_SLOTS;
        self.ring_index.set(self.ring_index.get() + 1);
        let byte_offset = slot * self.slot_stride;

        // Direct memcpy to shared-memory ring buffer
        if let Some(mapped_ptr) = self.ring_mapped_ptr {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    uniform_bytes.as_ptr(),
                    mapped_ptr.add(byte_offset as usize),
                    uniform_bytes.len(),
                );
            }
        }

        let hal_pipe = self.hal_pipeline.as_ref().expect("dual blit hal pipeline");
        let hal_samp = self.hal_sampler.as_ref().expect("dual blit hal sampler");
        let hal_ring = unsafe { &*self.hal_ring_ptr.expect("dual blit hal ring") };

        let hal_bg = unsafe {
            hal_ctx.device().create_bind_group(
                &hal::BindGroupDescriptor {
                    label: None,
                    layout: &hal_pipe.bind_group_layout,
                    entries: &[
                        hal::BindGroupEntry { binding: 0, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 1, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 2, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 3, resource_index: 1, count: 1 },
                    ],
                    buffers: &[hal::BufferBinding::new_unchecked(
                        hal_ring,
                        0,
                        std::num::NonZero::new(self.uniform_size),
                    )],
                    samplers: &[hal_samp],
                    textures: &[
                        hal::TextureBinding {
                            view: main_hal_view,
                            usage: wgpu::wgt::TextureUses::RESOURCE,
                        },
                        hal::TextureBinding {
                            view: secondary_hal_view,
                            usage: wgpu::wgt::TextureUses::RESOURCE,
                        },
                    ],
                    acceleration_structures: &[],
                    external_textures: &[],
                },
            )
            .expect("Failed to create hal dual blit bind group")
        };

        let ops = if store {
            hal::AttachmentOps::LOAD_CLEAR | hal::AttachmentOps::STORE
        } else {
            hal::AttachmentOps::LOAD_CLEAR | hal::AttachmentOps::STORE_DISCARD
        };

        unsafe {
            hal_enc.begin_render_pass(&hal::RenderPassDescriptor {
                label: None,
                extent: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                sample_count: 1,
                color_attachments: &[Some(hal::ColorAttachment {
                    target: hal::Attachment {
                        view: target_hal_view,
                        usage: wgpu::wgt::TextureUses::COLOR_TARGET,
                    },
                    resolve_target: None,
                    ops,
                    clear_value: wgpu::Color::TRANSPARENT,
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                multiview_mask: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            }).expect("hal begin_render_pass failed");
            hal_enc.set_render_pipeline(&hal_pipe.pipeline);
            hal_enc.set_bind_group(
                &hal_pipe.pipeline_layout, 0, &hal_bg,
                &[byte_offset as wgpu::DynamicOffset],
            );
            hal_enc.draw(0, 3, 0, 1);
            hal_enc.end_render_pass();
            hal_ctx.device().destroy_bind_group(hal_bg);
        }
    }
}

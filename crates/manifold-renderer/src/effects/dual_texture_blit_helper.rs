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

const RING_SLOTS: u64 = 64;
const UNIFORM_OFFSET_ALIGN: u64 = 256;

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
}

impl DualTextureBlitHelper {
    /// Create a new two-texture effect pipeline.
    ///
    /// `uniform_size` — byte size of the effect's uniform struct (must be Pod).
    pub fn new(
        device: &wgpu::Device,
        shader_source: &str,
        label: &str,
        uniform_size: u64,
    ) -> Self {
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
        let ring_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label} Ring UBO")),
            size: slot_stride * RING_SLOTS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

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
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
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

        queue.write_buffer(&self.ring_buffer, byte_offset, uniform_bytes);

        // Inline ensure_bind_group with split borrows — avoids &mut self +
        // &self.dummy_view conflict that would occur if calling draw_inner.
        let main_ptr = std::ptr::from_ref(main_view) as usize;
        let sec_ptr = std::ptr::from_ref(&self.dummy_view) as usize;

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
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
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
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
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
            device, queue, encoder, main_view, secondary_view, target_view,
            uniform_bytes, label, width, height, wgpu::StoreOp::Store, profiler,
        );
    }

    /// Like `draw`, but uses `StoreOp::Discard` — the target's tile memory is
    /// NOT written back to VRAM after the pass. Use for intermediate render
    /// targets that will be immediately overwritten or only read once.
    pub fn draw_discard(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
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
            device, queue, encoder, main_view, secondary_view, target_view,
            uniform_bytes, label, width, height, wgpu::StoreOp::Discard, profiler,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_inner(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
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

        queue.write_buffer(&self.ring_buffer, byte_offset, uniform_bytes);

        // Update cached bind group if textures changed (mutation done before
        // the render pass borrow to satisfy the borrow checker).
        self.ensure_bind_group(device, main_view, secondary_view, label);

        {
            let ts = profiler.and_then(|p| p.render_timestamps(label, width, height));
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
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
}

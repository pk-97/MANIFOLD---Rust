// Reusable GPU pipeline for single-pass fullscreen effects.
//
// Encapsulates: pipeline creation, ring-buffered uniforms, bind group layout,
// sampler, and fullscreen triangle draw. Every single-pass effect
// creates one of these at init and calls `draw()` each frame.
//
// Uniforms use a ring buffer with 256-byte-aligned slots to avoid
// per-frame Metal buffer allocation. Each `draw()` writes to the next
// slot via `queue.write_buffer()` — all writes target different offsets,
// so the GPU sees correct data per pass despite batched writes.

use std::cell::Cell;

const RING_SLOTS: u64 = 64;
const UNIFORM_OFFSET_ALIGN: u64 = 256;

/// Cached bind group keyed by source texture view pointer.
/// Reused across frames when the same texture is bound (common case).
struct CachedBG {
    bind_group: wgpu::BindGroup,
    source_ptr: usize,
}

pub struct SimpleBlitHelper {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
    ring_buffer: wgpu::Buffer,
    uniform_size: u64,
    slot_stride: u64,
    ring_index: Cell<u64>,
    cached: Option<CachedBG>,
}

impl SimpleBlitHelper {
    /// Create a new single-pass effect pipeline.
    ///
    /// `uniform_size` — byte size of the effect's uniform struct (must be Pod).
    /// The shader must define:
    ///   @group(0) @binding(0) var<uniform> uniforms: YourStruct;
    ///   @group(0) @binding(1) var source_tex: texture_2d<f32>;
    ///   @group(0) @binding(2) var tex_sampler: sampler;
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
                // binding 1: source texture
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
                // binding 2: sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
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

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            ring_buffer,
            uniform_size,
            slot_stride,
            ring_index: Cell::new(0),
            cached: None,
        }
    }

    /// Ensure the cached bind group is valid for the given source view.
    /// Uses dynamic uniform offset so the bind group can be reused across
    /// frames when the same texture is bound (saves ~10us per call).
    fn ensure_bind_group(
        &mut self,
        device: &wgpu::Device,
        source_view: &wgpu::TextureView,
        label: &str,
    ) {
        let src_ptr = std::ptr::from_ref(source_view) as usize;

        let needs_recreate = match &self.cached {
            Some(c) => c.source_ptr != src_ptr,
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
                        resource: wgpu::BindingResource::TextureView(source_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.cached = Some(CachedBG {
                bind_group,
                source_ptr: src_ptr,
            });
        }
    }

    /// Execute a single fullscreen pass: reads source texture, writes to target.
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        self.draw_inner(
            device, queue, encoder, source_view, target_view,
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
        source_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        self.draw_inner(
            device, queue, encoder, source_view, target_view,
            uniform_bytes, label, width, height, wgpu::StoreOp::Discard, profiler,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_inner(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source_view: &wgpu::TextureView,
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

        // Update cached bind group if source texture changed (mutation done
        // before the render pass borrow to satisfy the borrow checker).
        self.ensure_bind_group(device, source_view, label);

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

// Mechanical port of ComputePixelSortFX.cs + ComputeSortEffect.cs.
// Compute-based pixel sort: O(log²N) bitonic merge sort replaces the O(N²)
// counting sort from the original PixelSortFX. Same user-facing params.
//
// Unity refs:
//   Assets/Scripts/Compositing/Effects/ComputeSortEffect.cs  (abstract base)
//   Assets/Scripts/Compositing/Effects/ComputePixelSortFX.cs (subclass)
//
// Pipeline (3 stages):
//   1. Key extraction (compute) — CSExtractKeys kernel
//   2. Bitonic sort   (compute) — BitonicSortStep kernel, O(log²N) dispatches
//   3. Visualization  (render)  — fragment shader scatter

use std::collections::HashMap;
use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};

// --- ComputeSortEffect.cs line 66 — ShouldSkip default ---
// ComputePixelSortFX inherits: ShouldSkip => param0 <= 0.
// Matches ComputeSortEffect.cs: `fx.GetParam(0) <= 0f`

// --- ComputePixelSortFX.cs lines 22-23 ---
const SORT_ROWS: bool = false;             // SortRows = false → vertical (sort columns)
const SORT_RESOLUTION_SCALE: f32 = 0.5;   // SortResolutionScale = 0.5

// --- ComputeSortEffect.cs lines 157-158 — effective dimension clamp ---
const MIN_SORT_DIM: u32 = 16;

// --- ComputeSortEffect.cs line 157 — clamp for SortResolutionScale ---
const SCALE_MIN: f32 = 0.25;
const SCALE_MAX: f32 = 1.0;

// ── Key extraction uniform struct ─────────────────────────────────────────────
// Matches PixelSortKeys.compute uniforms in declaration order, 16-byte aligned.
// Unity: _PaddedWidth (int), _Width (int), _Height (int), _SortVertical (int),
//        _Amount (float), _Threshold (float)
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct KeyParams {
    padded_width:  u32,  // _PaddedWidth
    width:         u32,  // _Width
    height:        u32,  // _Height
    sort_vertical: u32,  // _SortVertical — 0=horizontal, 1=vertical
    amount:        f32,  // _Amount
    threshold:     f32,  // _Threshold
    _pad0:         f32,
    _pad1:         f32,
}

// ── Bitonic sort uniform struct ───────────────────────────────────────────────
// Matches BitonicSort.compute uniforms: _Level, _Step, _PaddedWidth, _Height
// 16 bytes, naturally aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BitonicParams {
    level:        u32,  // _Level
    step:         u32,  // _Step
    padded_width: u32,  // _PaddedWidth
    height:       u32,  // _Height
}

// ── Visualization uniform struct ──────────────────────────────────────────────
// Matches ComputePixelSortVisualize.shader uniforms.
// Unity: _PaddedWidth (int), _Width (int), _Height (int), _SortVertical (int), _Amount (float)
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VizParams {
    padded_width:  u32,  // _PaddedWidth
    width:         u32,  // _Width
    height:        u32,  // _Height
    sort_vertical: u32,  // _SortVertical
    amount:        f32,  // _Amount
    _pad0:         f32,
    _pad1:         f32,
    _pad2:         f32,
}

// ── Per-owner GPU state ───────────────────────────────────────────────────────
// ComputeSortEffect.cs lines 94-99 — OwnerBuffers struct
struct OwnerBuffers {
    sort_buffer:  wgpu::Buffer, // uint2 per element (8 bytes) — ComputeSortEffect.cs line 282
    padded_width: u32,           // paddedWidth at time of allocation
    sort_height:  u32,           // rows at time of allocation
}

// ── Main struct ───────────────────────────────────────────────────────────────

pub struct PixelSortFX {
    // Key extraction compute pipeline
    key_pipeline:        wgpu::ComputePipeline,
    key_bgl:             wgpu::BindGroupLayout,
    key_uniform_buf:     wgpu::Buffer,
    key_sampler:         wgpu::Sampler,

    // Bitonic sort compute pipeline
    bitonic_pipeline:    wgpu::ComputePipeline,
    bitonic_bgl:         wgpu::BindGroupLayout,
    bitonic_uniform_buf: wgpu::Buffer,

    // Visualization render pipeline
    viz_pipeline:        wgpu::RenderPipeline,
    viz_bgl:             wgpu::BindGroupLayout,
    viz_uniform_buf:     wgpu::Buffer,
    viz_sampler:         wgpu::Sampler,

    // ComputeSortEffect.cs lines 101-102 — per-owner sort buffers
    per_owner_buffers: HashMap<i64, OwnerBuffers>,

    // ComputeSortEffect.cs lines 90-92
    output_width:  u32,
    output_height: u32,
}

impl PixelSortFX {
    pub fn new(device: &wgpu::Device) -> Self {
        // ComputeSortEffect.cs lines 106-148 — Initialize()

        // --- Key extraction pipeline ---
        let key_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PixelSortKeys"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/pixel_sort_keys.wgsl").into(),
            ),
        });

        let key_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PixelSortKeys BGL"),
            entries: &[
                // binding 0: KeyParams uniform
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 1: source texture (Rgba16Float, filterable)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // binding 2: sampler (linear clamp — PixelSortKeys.compute: sampler_linear_clamp)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // binding 3: sort buffer (read_write storage)
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let key_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("PixelSortKeys Layout"),
            bind_group_layouts: &[&key_bgl],
            immediate_size: 0,
        });

        let key_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("PixelSortKeys Pipeline"),
            layout: Some(&key_pipeline_layout),
            module: &key_shader,
            entry_point: Some("cs_extract_keys"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let key_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PixelSortKeys Uniforms"),
            size: std::mem::size_of::<KeyParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let key_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("PixelSortKeys Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        // --- Bitonic sort pipeline ---
        let bitonic_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("BitonicSort"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/bitonic_sort.wgsl").into(),
            ),
        });

        let bitonic_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("BitonicSort BGL"),
            entries: &[
                // binding 0: BitonicParams uniform
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 1: sort buffer (read_write storage)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let bitonic_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("BitonicSort Layout"),
                bind_group_layouts: &[&bitonic_bgl],
                immediate_size: 0,
            });

        let bitonic_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("BitonicSort Pipeline"),
            layout: Some(&bitonic_pipeline_layout),
            module: &bitonic_shader,
            entry_point: Some("bitonic_sort_step"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let bitonic_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("BitonicSort Uniforms"),
            size: std::mem::size_of::<BitonicParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // --- Visualization render pipeline ---
        let viz_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PixelSortVisualize"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/pixel_sort_visualize.wgsl").into(),
            ),
        });

        // IMPORTANT: sort_buffer at fragment stage must be ReadOnlyStorage.
        // ComputePixelSortVisualize.shader: StructuredBuffer<uint2> _SortBuffer (read-only)
        let viz_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PixelSortVisualize BGL"),
            entries: &[
                // binding 0: VizParams uniform
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
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
                // binding 3: sort buffer — read-only at fragment stage
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let viz_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("PixelSortVisualize Layout"),
            bind_group_layouts: &[&viz_bgl],
            immediate_size: 0,
        });

        let viz_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("PixelSortVisualize Pipeline"),
            layout: Some(&viz_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &viz_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &viz_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
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

        let viz_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PixelSortVisualize Uniforms"),
            size: std::mem::size_of::<VizParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let viz_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("PixelSortVisualize Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        Self {
            key_pipeline,
            key_bgl,
            key_uniform_buf,
            key_sampler,
            bitonic_pipeline,
            bitonic_bgl,
            bitonic_uniform_buf,
            per_owner_buffers: HashMap::new(),
            viz_pipeline,
            viz_bgl,
            viz_uniform_buf,
            viz_sampler,
            output_width: 0,
            output_height: 0,
        }
    }

    // ComputeSortEffect.cs lines 268-287 — GetOrCreateBuffers
    fn get_or_create_buffers(
        &mut self,
        device: &wgpu::Device,
        owner_key: i64,
        sort_dim: u32,
        rows: u32,
    ) -> &OwnerBuffers {
        let padded_dim = next_power_of_two(sort_dim);

        // Check if existing buffers still match
        if let Some(buf) = self.per_owner_buffers.get(&owner_key) {
            if buf.padded_width == padded_dim && buf.sort_height == rows {
                return self.per_owner_buffers.get(&owner_key).unwrap();
            }
        }

        // Release old (drop happens automatically), allocate new
        // ComputeSortEffect.cs line 282: ComputeBuffer(paddedDim * rows, 8) — uint2 = 8 bytes
        let element_count = padded_dim * rows;
        let sort_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("PixelSort SortBuffer owner={owner_key}")),
            size: element_count as u64 * 8, // uint2 = 8 bytes per element
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });

        self.per_owner_buffers.insert(owner_key, OwnerBuffers {
            sort_buffer,
            padded_width: padded_dim,
            sort_height:  rows,
        });

        self.per_owner_buffers.get(&owner_key).unwrap()
    }
}

impl PostProcessEffect for PixelSortFX {
    fn effect_type(&self) -> EffectType {
        // ComputePixelSortFX.cs line 16
        EffectType::PixelSort
    }

    // ComputeSortEffect.cs line 66 — ShouldSkip: param0 <= 0
    fn should_skip(&self, fx: &EffectInstance) -> bool {
        fx.param_values.first().copied().unwrap_or(0.0) <= 0.0
    }

    fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // ComputeSortEffect.cs lines 107-109 — store current output dimensions
        self.output_width  = ctx.width;
        self.output_height = ctx.height;

        // ComputePixelSortFX.cs line 34 — param 0 = Amount
        let amount = fx.param_values.first().copied().unwrap_or(0.0);

        // ComputePixelSortFX.cs line 35 — threshold formula
        let threshold = 0.05 * (1.0 - amount * 0.8);

        // ComputeSortEffect.cs lines 157-162 — effective sort dimensions
        // scale = Mathf.Clamp(SortResolutionScale, 0.25f, 1f)
        let scale = SORT_RESOLUTION_SCALE.clamp(SCALE_MIN, SCALE_MAX);
        // effectiveWidth  = Mathf.Max(16, Mathf.RoundToInt(outputWidth  * scale))
        let effective_width  = (self.output_width  as f32 * scale).round() as u32;
        let effective_width  = effective_width.max(MIN_SORT_DIM);
        // effectiveHeight = Mathf.Max(16, Mathf.RoundToInt(outputHeight * scale))
        let effective_height = (self.output_height as f32 * scale).round() as u32;
        let effective_height = effective_height.max(MIN_SORT_DIM);

        // ComputeSortEffect.cs lines 161-162
        // SortRows=false → sort columns → sortDim=height, rows=width
        let (sort_dim, rows) = if SORT_ROWS {
            (effective_width, effective_height)
        } else {
            (effective_height, effective_width)
        };

        // ComputeSortEffect.cs lines 164-165 — GetOrCreateBuffers
        let padded_dim = {
            let buffers = self.get_or_create_buffers(device, ctx.owner_key, sort_dim, rows);
            buffers.padded_width
        };

        let sort_vertical: u32 = if SORT_ROWS { 0 } else { 1 };

        // ── Stage 1: Key extraction ───────────────────────────────────────────
        // ComputeSortEffect.cs lines 167-177

        let key_params = KeyParams {
            padded_width:  padded_dim,
            width:         sort_dim,
            height:        rows,
            sort_vertical,
            amount,
            threshold,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        queue.write_buffer(&self.key_uniform_buf, 0, bytemuck::bytes_of(&key_params));

        // ComputeSortEffect.cs line 176 — keyGroupsX = Mathf.CeilToInt(paddedDim / 256f)
        let key_groups_x = padded_dim.div_ceil(256).max(1);

        {
            let sort_buf_slice = self.per_owner_buffers.get(&ctx.owner_key)
                .expect("sort buffer must exist after get_or_create");

            let key_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("PixelSortKeys BG"),
                layout: &self.key_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.key_uniform_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.key_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: sort_buf_slice.sort_buffer.as_entire_binding(),
                    },
                ],
            });

            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PixelSort KeyExtract"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.key_pipeline);
            pass.set_bind_group(0, &key_bg, &[]);
            // ComputeSortEffect.cs line 177 — Dispatch(keyGroupsX, rows, 1)
            pass.dispatch_workgroups(key_groups_x, rows, 1);
        }

        // ── Stage 2: Bitonic sort ─────────────────────────────────────────────
        // ComputeSortEffect.cs lines 179-196

        // ComputeSortEffect.cs line 180
        let log_n = ceil_log2(padded_dim);
        // ComputeSortEffect.cs lines 181-182
        // bitonicGroupsX = Mathf.CeilToInt(paddedDim / 2 / 256f), min 1
        let bitonic_groups_x = (padded_dim / 2).div_ceil(256).max(1);

        {
            let sort_buf_slice = self.per_owner_buffers.get(&ctx.owner_key)
                .expect("sort buffer must exist after get_or_create");

            // ComputeSortEffect.cs lines 188-196 — for level / for step loop
            for level in 0..log_n {
                for step in (0..=level).rev() {
                    let bitonic_params = BitonicParams {
                        level,
                        step,
                        padded_width: padded_dim,
                        height: rows,
                    };
                    queue.write_buffer(
                        &self.bitonic_uniform_buf,
                        0,
                        bytemuck::bytes_of(&bitonic_params),
                    );

                    let bitonic_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some(&format!("BitonicSort BG l={level} s={step}")),
                        layout: &self.bitonic_bgl,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: self.bitonic_uniform_buf.as_entire_binding(),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: sort_buf_slice.sort_buffer.as_entire_binding(),
                            },
                        ],
                    });

                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some(&format!("BitonicSort l={level} s={step}")),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(&self.bitonic_pipeline);
                    pass.set_bind_group(0, &bitonic_bg, &[]);
                    // ComputeSortEffect.cs line 194 — Dispatch(bitonicGroupsX, rows, 1)
                    pass.dispatch_workgroups(bitonic_groups_x, rows, 1);
                }
            }
        }

        // ── Stage 3: Visualization ────────────────────────────────────────────
        // ComputeSortEffect.cs lines 198-213

        let viz_params = VizParams {
            padded_width: padded_dim,
            width:        sort_dim,
            height:       rows,
            sort_vertical,
            amount,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        queue.write_buffer(&self.viz_uniform_buf, 0, bytemuck::bytes_of(&viz_params));

        {
            let sort_buf_slice = self.per_owner_buffers.get(&ctx.owner_key)
                .expect("sort buffer must exist after get_or_create");

            let viz_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("PixelSortVisualize BG"),
                layout: &self.viz_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.viz_uniform_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.viz_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: sort_buf_slice.sort_buffer.as_entire_binding(),
                    },
                ],
            });

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("PixelSortVisualize"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.viz_pipeline);
            pass.set_bind_group(0, &viz_bg, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    // ComputeSortEffect.cs lines 217-219 — ClearState()
    // Sort buffers are transient per-frame — nothing temporal to clear.
    fn clear_state(&mut self) {}

    // ComputeSortEffect.cs lines 222-227 — Resize()
    fn resize(&mut self, _device: &wgpu::Device, width: u32, height: u32) {
        self.output_width  = width;
        self.output_height = height;
        // Recreate buffers lazily on next Apply (size may have changed)
        // Unity: CleanupAllOwners() called here
        self.per_owner_buffers.clear();
    }
}

impl StatefulEffect for PixelSortFX {
    // ComputeSortEffect.cs lines 248 — ClearState(int ownerKey) — no-op
    fn clear_state_for_owner(&mut self, _owner_key: i64) {}

    // ComputeSortEffect.cs lines 250-257 — CleanupOwner
    fn cleanup_owner(&mut self, owner_key: i64) {
        self.per_owner_buffers.remove(&owner_key);
    }

    // ComputeSortEffect.cs lines 259-264 — CleanupAllOwners
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) {
        self.per_owner_buffers.clear();
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

// ComputeSortEffect.cs lines 289-298 — NextPowerOfTwo
fn next_power_of_two(mut v: u32) -> u32 {
    v -= 1;
    v |= v >> 1;
    v |= v >> 2;
    v |= v >> 4;
    v |= v >> 8;
    v |= v >> 16;
    v + 1
}

// ComputeSortEffect.cs lines 300-306 — CeilLog2
fn ceil_log2(v: u32) -> u32 {
    let mut log: u32 = 0;
    let mut val: u32 = 1;
    while val < v {
        val <<= 1;
        log += 1;
    }
    log
}

// Mechanical port of Unity BloomFX.cs + BloomEffect.shader.
// Same logic, same variables, same constants, same edge cases.

use std::collections::HashMap;
use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::render_target::RenderTarget;

// BloomFX.cs lines 19-25 — constants
const MAX_LEVELS: usize = 6;
const MIN_SIZE: u32 = 16;
const PREFILTER_THRESHOLD: f32 = 0.42;
const PREFILTER_KNEE: f32 = 0.24;
const BLOOM_LEVELS: usize = 3;
const RADIUS_AT_ZERO: f32 = 0.70;
const RADIUS_AT_ONE: f32 = 1.25;

// BloomFX.cs lines 9-11 — uniforms matching BloomEffect.shader Properties
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BloomUniforms {
    mode: u32,           // 0=prefilter, 1=downsample, 2=upsample, 3=composite
    threshold: f32,      // _Threshold
    knee: f32,           // _Knee
    intensity: f32,      // _Intensity
    radius_scale: f32,   // _RadiusScale
    combine_weight: f32, // _CombineWeight
    main_texel_size_x: f32,  // _MainTex_TexelSize.x
    main_texel_size_y: f32,  // _MainTex_TexelSize.y
    bloom_texel_size_x: f32, // _BloomTex_TexelSize.x
    bloom_texel_size_y: f32, // _BloomTex_TexelSize.y
    _pad0: f32,
    _pad1: f32,
}

// BloomFX.cs lines 27-32 — OwnerPyramid
struct BloomState {
    mips_a: Vec<RenderTarget>, // Primary mip chain (downsample target, upsample source)
    mips_b: Vec<RenderTarget>, // Secondary mip chain (upsample target, avoids read-write hazard)
    count: usize,
}

// BloomFX.cs line 12 — BloomFX : SimpleBlitEffect, IStatefulEffect
pub struct BloomFX {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    /// 1x1 dummy texture bound as bloom_tex when it's not read (prefilter, downsample).
    dummy_view: wgpu::TextureView,
    states: HashMap<i64, BloomState>,
    width: u32,  // BloomFX.cs line 17 — _width
    height: u32, // BloomFX.cs line 17 — _height
}

impl BloomFX {
    pub fn new(device: &wgpu::Device) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;
        let shader_src = include_str!("shaders/bloom.wgsl");

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Bloom"),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        // Bind group layout: uniforms + main_tex + sampler + bloom_tex
        // Matches BloomEffect.shader: _MainTex, _BloomTex, uniforms
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Bloom BGL"),
            entries: &[
                // binding 0: uniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
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
                // binding 3: bloom_tex (_BloomTex)
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
            label: Some("Bloom Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Bloom Pipeline"),
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
            label: Some("Bloom Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Bloom Uniforms"),
            size: std::mem::size_of::<BloomUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // 1x1 dummy texture for bloom_tex binding when it's not read (prefilter, downsample)
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Bloom Dummy"),
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
            uniform_buffer,
            dummy_view,
            states: HashMap::new(),
            width: 0,
            height: 0,
        }
    }

    // BloomFX.cs lines 42-68 — GetOrCreatePyramid
    fn ensure_state(&mut self, device: &wgpu::Device, owner_key: i64) {
        if self.states.contains_key(&owner_key) {
            return;
        }
        if self.width == 0 || self.height == 0 {
            return;
        }
        let format = wgpu::TextureFormat::Rgba16Float;
        let mut mips_a = Vec::new();
        let mut mips_b = Vec::new();
        let mut count = 0;

        // BloomFX.cs lines 51-52
        let mut pw = (self.width / 2).max(1);
        let mut ph = (self.height / 2).max(1);

        // BloomFX.cs lines 54-64
        for i in 0..MAX_LEVELS {
            if pw < MIN_SIZE || ph < MIN_SIZE {
                break;
            }
            mips_a.push(RenderTarget::new(device, pw, ph, format, &format!("BloomMipA_{i}")));
            mips_b.push(RenderTarget::new(device, pw, ph, format, &format!("BloomMipB_{i}")));
            count += 1;
            pw = (pw / 2).max(1);
            ph = (ph / 2).max(1);
        }

        self.states.insert(owner_key, BloomState { mips_a, mips_b, count });
    }

    // Helper: draw a pass with two texture bindings (main_tex + bloom_tex)
    fn draw_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        main_view: &wgpu::TextureView,   // _MainTex
        bloom_view: &wgpu::TextureView,  // _BloomTex
        target_view: &wgpu::TextureView, // output
        uniforms: &BloomUniforms,
        label: &str,
    ) {
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(uniforms));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
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
                    resource: wgpu::BindingResource::TextureView(bloom_view),
                },
            ],
        });

        {
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
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

impl PostProcessEffect for BloomFX {
    fn effect_type(&self) -> EffectType {
        EffectType::Bloom
    }

    fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,  // buffer in Unity
        target: &wgpu::TextureView,  // ctx.Host.GetTargetBuffer() in Unity
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // BloomFX.cs line 72: if (fx.GetParam(0) <= 0f || material == null) return;
        let amount = fx.param_values.first().copied().unwrap_or(0.187);
        if amount <= 0.0 {
            // Passthrough: copy source to target
            self.draw_pass(
                device, queue, encoder,
                source, &self.dummy_view, target,
                &BloomUniforms {
                    mode: 3, threshold: 0.0, knee: 0.0, intensity: 0.0,
                    radius_scale: 1.0, combine_weight: 0.0,
                    main_texel_size_x: 0.0, main_texel_size_y: 0.0,
                    bloom_texel_size_x: 0.0, bloom_texel_size_y: 0.0,
                    _pad0: 0.0, _pad1: 0.0,
                },
                "Bloom Skip",
            );
            return;
        }

        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_state(device, ctx.owner_key);

        let state = self.states.get(&ctx.owner_key).unwrap();
        // BloomFX.cs line 75: if (pyr.count <= 0) return;
        if state.count == 0 {
            self.draw_pass(
                device, queue, encoder,
                source, &self.dummy_view, target,
                &BloomUniforms {
                    mode: 3, threshold: 0.0, knee: 0.0, intensity: 0.0,
                    radius_scale: 1.0, combine_weight: 0.0,
                    main_texel_size_x: 0.0, main_texel_size_y: 0.0,
                    bloom_texel_size_x: 0.0, bloom_texel_size_y: 0.0,
                    _pad0: 0.0, _pad1: 0.0,
                },
                "Bloom Skip",
            );
            return;
        }

        // BloomFX.cs lines 77-80
        let bloom_t = amount.clamp(0.0, 1.0);
        let used_levels = BLOOM_LEVELS.min(state.count); // Clamp(BLOOM_LEVELS, 1, pyr.count)
        let t_smooth = bloom_t * bloom_t * (3.0 - 2.0 * bloom_t);
        let radius_scale = RADIUS_AT_ZERO + (RADIUS_AT_ONE - RADIUS_AT_ZERO) * t_smooth;

        // BloomFX.cs lines 82-86 — uniforms set once, reused across passes
        let base_uniforms = BloomUniforms {
            mode: 0,
            threshold: PREFILTER_THRESHOLD,   // line 82
            knee: PREFILTER_KNEE,             // line 83
            intensity: amount,                // line 84: fx.GetParam(0)
            radius_scale,                     // line 85
            combine_weight: 1.0,              // line 86
            main_texel_size_x: 0.0,
            main_texel_size_y: 0.0,
            bloom_texel_size_x: 0.0,
            bloom_texel_size_y: 0.0,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        // BloomFX.cs line 89: Graphics.Blit(buffer, pyr.mipsA[0], material, 0)
        // Pass 0: Prefilter. _MainTex = source (buffer), output = mipsA[0]
        // _MainTex_TexelSize = 1/source_width, 1/source_height
        self.draw_pass(
            device, queue, encoder,
            source,                    // main_tex = buffer
            &self.dummy_view,          // bloom_tex = dummy (not read in mode 0)
            &state.mips_a[0].view,     // target = mipsA[0]
            &BloomUniforms {
                mode: 0,
                main_texel_size_x: 1.0 / ctx.width as f32,
                main_texel_size_y: 1.0 / ctx.height as f32,
                ..base_uniforms
            },
            "Bloom Prefilter",
        );

        // BloomFX.cs lines 92-93: Downsample chain (all in mipsA)
        // for (int i = 1; i < usedLevels; i++) Graphics.Blit(mipsA[i-1], mipsA[i], material, 1)
        for i in 1..used_levels {
            let src_w = state.mips_a[i - 1].width;
            let src_h = state.mips_a[i - 1].height;
            self.draw_pass(
                device, queue, encoder,
                &state.mips_a[i - 1].view,  // main_tex = mipsA[i-1]
                &self.dummy_view,            // bloom_tex = dummy (not read in mode 1)
                &state.mips_a[i].view,       // target = mipsA[i]
                &BloomUniforms {
                    mode: 1,
                    main_texel_size_x: 1.0 / src_w as f32,
                    main_texel_size_y: 1.0 / src_h as f32,
                    ..base_uniforms
                },
                "Bloom Down",
            );
        }

        // BloomFX.cs lines 98-105: Upsample chain, ping-pong mipsA → mipsB
        // for (int i = usedLevels - 2; i >= 0; i--)
        //   hi = mipsA[i]                                    → _MainTex
        //   lo = (i == usedLevels-2) ? mipsA[i+1] : mipsB[i+1]  → _BloomTex
        //   Blit(hi, mipsB[i], material, 2)
        for i in (0..used_levels - 1).rev() {
            let hi_w = state.mips_a[i].width;
            let hi_h = state.mips_a[i].height;

            // BloomFX.cs line 101: lo source selection
            let lo_view = if i == used_levels - 2 {
                &state.mips_a[i + 1].view // first upsample: lo from mipsA
            } else {
                &state.mips_b[i + 1].view // subsequent: lo from mipsB (previous upsample output)
            };
            let lo_w = if i == used_levels - 2 {
                state.mips_a[i + 1].width
            } else {
                state.mips_b[i + 1].width
            };
            let lo_h = if i == used_levels - 2 {
                state.mips_a[i + 1].height
            } else {
                state.mips_b[i + 1].height
            };

            self.draw_pass(
                device, queue, encoder,
                &state.mips_a[i].view, // main_tex = hi = mipsA[i]
                lo_view,               // bloom_tex = lo
                &state.mips_b[i].view, // target = mipsB[i]
                &BloomUniforms {
                    mode: 2,
                    // _MainTex_TexelSize = hi dimensions (same size as output)
                    main_texel_size_x: 1.0 / hi_w as f32,
                    main_texel_size_y: 1.0 / hi_h as f32,
                    // _BloomTex_TexelSize = lo dimensions (smaller texture)
                    bloom_texel_size_x: 1.0 / lo_w as f32,
                    bloom_texel_size_y: 1.0 / lo_h as f32,
                    ..base_uniforms
                },
                "Bloom Up",
            );
        }

        // BloomFX.cs lines 108-112: Final composite
        // _BloomTex = mipsB[0], Blit(buffer, target, material, 3)
        let bloom_w = state.mips_b[0].width;
        let bloom_h = state.mips_b[0].height;
        self.draw_pass(
            device, queue, encoder,
            source,                    // main_tex = buffer (original source)
            &state.mips_b[0].view,     // bloom_tex = mipsB[0] (upsample result)
            target,                    // output = target
            &BloomUniforms {
                mode: 3,
                // _MainTex_TexelSize = source dimensions
                main_texel_size_x: 1.0 / ctx.width as f32,
                main_texel_size_y: 1.0 / ctx.height as f32,
                // _BloomTex_TexelSize = mipsB[0] dimensions
                bloom_texel_size_x: 1.0 / bloom_w as f32,
                bloom_texel_size_y: 1.0 / bloom_h as f32,
                ..base_uniforms
            },
            "Bloom Composite",
        );
    }

    fn clear_state(&mut self) {
        self.states.clear();
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let format = wgpu::TextureFormat::Rgba16Float;
        for state in self.states.values_mut() {
            let mut pw = (width / 2).max(1);
            let mut ph = (height / 2).max(1);
            let mut count = 0;
            for i in 0..state.mips_a.len() {
                if pw < MIN_SIZE || ph < MIN_SIZE {
                    break;
                }
                state.mips_a[i] = RenderTarget::new(device, pw, ph, format, &format!("BloomMipA_{i}"));
                state.mips_b[i] = RenderTarget::new(device, pw, ph, format, &format!("BloomMipB_{i}"));
                count += 1;
                pw = (pw / 2).max(1);
                ph = (ph / 2).max(1);
            }
            state.count = count;
        }
    }
}

impl StatefulEffect for BloomFX {
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }

    fn cleanup_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
}

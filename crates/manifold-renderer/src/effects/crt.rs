// Mechanical port of Unity CrtFX.cs + CrtEffect.shader.
// Same logic, same variables, same constants, same edge cases.

use std::collections::HashMap;
use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::render_target::RenderTarget;

// CrtFX.cs lines 8-11 — uniforms matching CrtEffect.shader Properties
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CrtUniforms {
    mode: u32,               // 0=prefilter, 1=downsample, 2=composite
    amount: f32,             // _Amount
    scanlines: f32,          // _Scanlines
    glow: f32,               // _Glow
    curvature: f32,          // _Curvature
    style: f32,              // _Style
    glow_threshold: f32,     // _GlowThreshold = lerp(0.15, 0.05, style)
    screen_height: f32,      // _ScreenHeight
    main_texel_size_x: f32,  // _MainTex_TexelSize.x
    main_texel_size_y: f32,  // _MainTex_TexelSize.y
    main_texel_size_z: f32,  // _MainTex_TexelSize.z (width in pixels, for phosphor mask)
    _pad: f32,
}

// CrtFX.cs lines 20-24 — CrtState
struct CrtState {
    half_res: RenderTarget,   // CrtFX.cs: halfRes
    quarter_res: RenderTarget, // CrtFX.cs: quarterRes
}

// CrtFX.cs line 13 — CrtFX : SimpleBlitEffect, IStatefulEffect
pub struct CrtFX {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    /// 1x1 dummy texture for glow_tex binding in passes 0 and 1 (not read).
    dummy_view: wgpu::TextureView,
    states: HashMap<i64, CrtState>, // CrtFX.cs: ownerStates
    width: u32,  // CrtFX.cs: _width
    height: u32, // CrtFX.cs: _height
}

impl CrtFX {
    pub fn new(device: &wgpu::Device) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;
        let shader_src = include_str!("shaders/fx_crt.wgsl");

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("CRT"),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        // Bind group layout: uniforms + main_tex + sampler + glow_tex
        // Matches CrtEffect.shader: _MainTex, _GlowTex, uniforms
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("CRT BGL"),
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
                // binding 3: glow_tex (_GlowTex)
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
            label: Some("CRT Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("CRT Pipeline"),
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
            label: Some("CRT Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("CRT Uniforms"),
            size: std::mem::size_of::<CrtUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // 1x1 dummy texture for glow_tex binding when it's not read (pass 0 prefilter, pass 1 downsample)
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("CRT Dummy"),
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

    // CrtFX.cs lines 34-52 — GetOrCreateState
    fn ensure_state(&mut self, device: &wgpu::Device, owner_key: i64) {
        if self.states.contains_key(&owner_key) {
            return;
        }
        if self.width == 0 || self.height == 0 {
            return;
        }
        let format = wgpu::TextureFormat::Rgba16Float;

        // CrtFX.cs lines 40-43
        let hw = (self.width / 2).max(1);
        let hh = (self.height / 2).max(1);
        let qw = (self.width / 4).max(1);
        let qh = (self.height / 4).max(1);

        self.states.insert(owner_key, CrtState {
            half_res: RenderTarget::new(device, hw, hh, format, &format!("CrtGlowHalf_{owner_key}")),
            quarter_res: RenderTarget::new(device, qw, qh, format, &format!("CrtGlowQuarter_{owner_key}")),
        });
    }

    // Helper: draw a pass with two texture bindings (main_tex + glow_tex)
    fn draw_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        main_view: &wgpu::TextureView,  // _MainTex
        glow_view: &wgpu::TextureView,  // _GlowTex
        target_view: &wgpu::TextureView, // output
        uniforms: &CrtUniforms,
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
                    resource: wgpu::BindingResource::TextureView(glow_view),
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

impl PostProcessEffect for CrtFX {
    fn effect_type(&self) -> EffectType {
        EffectType::CRT
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
        // CrtFX.cs line 56-57: float amount = fx.GetParam(0); if (amount <= 0f) return;
        let amount = fx.param_values.first().copied().unwrap_or(0.0);
        if amount <= 0.0 {
            return;
        }

        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_state(device, ctx.owner_key);

        let state = match self.states.get(&ctx.owner_key) {
            Some(s) => s,
            None => return,
        };

        // CrtFX.cs line 61
        let style = fx.param_values.get(4).copied().unwrap_or(0.5);

        // CrtFX.cs line 64: material.SetFloat("_GlowThreshold", Mathf.Lerp(0.15f, 0.05f, style))
        let glow_threshold = 0.15_f32 + (0.05 - 0.15) * style; // lerp(0.15, 0.05, style)

        // ── Pass 0: Prefilter — source → halfRes ──────────────────────────────
        // CrtFX.cs line 66: Graphics.Blit(buffer, state.halfRes, material, 0)
        // _MainTex_TexelSize = 1/source_width, 1/source_height (Unity auto-sets from SOURCE)
        self.draw_pass(
            device, queue, encoder,
            source,                       // main_tex = buffer (source)
            &self.dummy_view,             // glow_tex = dummy (not read in mode 0)
            &state.half_res.view,         // target = halfRes
            &CrtUniforms {
                mode: 0,
                amount,
                scanlines: fx.param_values.get(1).copied().unwrap_or(0.5),
                glow: fx.param_values.get(2).copied().unwrap_or(0.3),
                curvature: fx.param_values.get(3).copied().unwrap_or(0.2),
                style,
                glow_threshold,
                screen_height: ctx.height as f32,
                main_texel_size_x: 1.0 / ctx.width as f32,
                main_texel_size_y: 1.0 / ctx.height as f32,
                main_texel_size_z: ctx.width as f32,
                _pad: 0.0,
            },
            "CRT Prefilter",
        );

        // ── Pass 1: Downsample — halfRes → quarterRes ─────────────────────────
        // CrtFX.cs line 69: Graphics.Blit(state.halfRes, state.quarterRes, material, 1)
        // _MainTex_TexelSize = 1/halfRes_width, 1/halfRes_height (SOURCE = halfRes)
        let hw = state.half_res.width;
        let hh = state.half_res.height;
        let qw = state.quarter_res.width;
        self.draw_pass(
            device, queue, encoder,
            &state.half_res.view,         // main_tex = halfRes
            &self.dummy_view,             // glow_tex = dummy (not read in mode 1)
            &state.quarter_res.view,      // target = quarterRes
            &CrtUniforms {
                mode: 1,
                amount,
                scanlines: fx.param_values.get(1).copied().unwrap_or(0.5),
                glow: fx.param_values.get(2).copied().unwrap_or(0.3),
                curvature: fx.param_values.get(3).copied().unwrap_or(0.2),
                style,
                glow_threshold,
                screen_height: ctx.height as f32,
                main_texel_size_x: 1.0 / hw as f32,
                main_texel_size_y: 1.0 / hh as f32,
                main_texel_size_z: qw as f32, // not used in downsample, but kept consistent
                _pad: 0.0,
            },
            "CRT Downsample",
        );

        // ── Pass 2: CRT Composite — source + quarterRes(glow) → target ────────
        // CrtFX.cs lines 72-80: material.SetTexture("_GlowTex", state.quarterRes); Blit(buffer, target, 2)
        // _MainTex_TexelSize = 1/source_width, 1/source_height (SOURCE = buffer)
        self.draw_pass(
            device, queue, encoder,
            source,                       // main_tex = buffer (source)
            &state.quarter_res.view,      // glow_tex = quarterRes (_GlowTex)
            target,                       // output = target
            &CrtUniforms {
                mode: 2,
                amount,
                scanlines: fx.param_values.get(1).copied().unwrap_or(0.5),
                glow: fx.param_values.get(2).copied().unwrap_or(0.3),
                curvature: fx.param_values.get(3).copied().unwrap_or(0.2),
                style,
                glow_threshold,
                screen_height: ctx.height as f32,
                main_texel_size_x: 1.0 / ctx.width as f32,
                main_texel_size_y: 1.0 / ctx.height as f32,
                main_texel_size_z: ctx.width as f32,
                _pad: 0.0,
            },
            "CRT Composite",
        );
    }

    // CrtFX.cs lines 87-94 — ClearState (clears but keeps buffers alive)
    fn clear_state(&mut self) {
        // In Unity: RenderTextureUtil.Clear() — no equivalent needed in wgpu;
        // contents are overwritten each frame. No-op is correct.
    }

    // CrtFX.cs line 125 — CleanupAllOwners (resize = recreate)
    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let format = wgpu::TextureFormat::Rgba16Float;
        for (owner_key, state) in self.states.iter_mut() {
            let hw = (width / 2).max(1);
            let hh = (height / 2).max(1);
            let qw = (width / 4).max(1);
            let qh = (height / 4).max(1);
            state.half_res = RenderTarget::new(device, hw, hh, format, &format!("CrtGlowHalf_{owner_key}"));
            state.quarter_res = RenderTarget::new(device, qw, qh, format, &format!("CrtGlowQuarter_{owner_key}"));
        }
    }
}

impl StatefulEffect for CrtFX {
    // CrtFX.cs lines 96-103 — ClearState(ownerKey): clear but keep alive
    fn clear_state_for_owner(&mut self, _owner_key: i64) {
        // Contents overwritten each frame; no-op is correct.
    }

    // CrtFX.cs lines 105-113 — CleanupOwner
    fn cleanup_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
}

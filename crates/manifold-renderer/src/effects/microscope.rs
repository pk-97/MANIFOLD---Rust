// Mechanical port of Unity MicroscopeFX.cs + MicroscopeEffect.shader.
// Same logic, same variables, same constants, same edge cases.

use ahash::AHashMap;
use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::render_target::RenderTarget;

// MicroscopeFX.cs line 23-27 — MicroscopeState
struct MicroscopeState {
    blur_a: RenderTarget, // H blur output
    blur_b: RenderTarget, // V blur output (final blur)
    edge_rt: RenderTarget, // Sobel edge map
}

// MicroscopeFX.cs line 16 — MicroscopeFX : SimpleBlitEffect, IStatefulEffect
//
// Pass 3 (composite) reads 3 textures: _MainTex, _BlurTex, _EdgeTex.
// Passes 0-2 only read _MainTex. Because the composite pass needs 3 texture
// bindings, we build a custom pipeline (not SimpleBlitHelper or
// DualTextureBlitHelper which only support 1 or 2).
// All 4 passes use the same shader with mode dispatched in fs_main.
pub struct MicroscopeFX {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    /// 1x1 dummy texture for blur_tex / edge_tex when those passes don't need them.
    dummy_view: wgpu::TextureView,
    states: AHashMap<i64, MicroscopeState>,
    width: u32,  // MicroscopeFX.cs line 20 — _width
    height: u32, // MicroscopeFX.cs line 20 — _height
}

// MicroscopeEffect.shader uniforms — all 4 passes share this struct.
// 16-byte aligned: 20 f32/u32 fields → 80 bytes total (5 × vec4).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MicroscopeUniforms {
    // offset 0 (vec4 boundary)
    mode: u32,          // pass selector: 0=HBlur, 1=VBlur, 2=Edge, 3=Composite
    amount: f32,        // _Amount       p0
    zoom: f32,          // _Zoom         p1
    focus: f32,         // _Focus        p2
    // offset 16 (vec4 boundary)
    dof: f32,           // _DOF          p3
    aberration: f32,    // _Aberration   p4
    illumination: f32,  // _Illumination p5 (RoundToInt in C#)
    structure: f32,     // _Structure    p6
    // offset 32 (vec4 boundary)
    distortion: f32,    // _Distortion   p7
    drift: f32,         // _Drift        p8
    noise: f32,         // _Noise        p9
    dust: f32,          // _Dust         p10
    // offset 48 (vec4 boundary)
    texel_x: f32,       // _TexelSize.x = 1/width
    texel_y: f32,       // _TexelSize.y = 1/height
    texel_z: f32,       // _TexelSize.z = width
    texel_w: f32,       // _TexelSize.w = height
    // offset 64 (vec4 boundary)
    time: f32,          // _Time.y (ctx.time) — needed for drift and noise
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

impl MicroscopeFX {
    pub fn new(device: &wgpu::Device) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Microscope"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fx_microscope.wgsl").into(),
            ),
        });

        // Bind group layout: uniforms + _MainTex + sampler + _BlurTex + _EdgeTex
        // Passes 0-2 bind dummy to blur_tex and edge_tex (slots 3, 4).
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Microscope BGL"),
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
                // binding 1: _MainTex (source)
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
                // binding 2: sampler (shared)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // binding 3: _BlurTex
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
                // binding 4: _EdgeTex
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
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
            label: Some("Microscope Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Microscope Pipeline"),
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
            label: Some("Microscope Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Microscope Uniforms"),
            size: std::mem::size_of::<MicroscopeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Dummy 1x1 texture for slots 3 and 4 when not needed (passes 0-2)
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Microscope Dummy"),
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
            states: AHashMap::new(),
            width: 0,
            height: 0,
        }
    }

    // MicroscopeFX.cs lines 38-53 — GetOrCreateState
    fn ensure_state(&mut self, device: &wgpu::Device, owner_key: i64) {
        // MicroscopeFX.cs line 40-42: check all three are non-null
        if self.states.contains_key(&owner_key) {
            return;
        }
        if self.width == 0 || self.height == 0 {
            return;
        }
        let format = wgpu::TextureFormat::Rgba16Float;
        // Blur intermediates at quarter-res — blur is low-frequency, bilinear
        // upscale in the composite shader preserves visual quality.
        let blur_w = (self.width / 4).max(1);
        let blur_h = (self.height / 4).max(1);
        let blur_a = RenderTarget::new(device, blur_w, blur_h, format,
            &format!("MicroscopeBlurA_{owner_key}"));
        let blur_b = RenderTarget::new(device, blur_w, blur_h, format,
            &format!("MicroscopeBlurB_{owner_key}"));
        // Edge detection stays at full res (high-frequency detail matters)
        let edge_rt = RenderTarget::new(device, self.width, self.height, format,
            &format!("MicroscopeEdge_{owner_key}"));
        // RenderTextureUtil.Clear() is handled by clear_render_target below;
        // initial creation is zeroed by GPU (transparent black).
        self.states.insert(owner_key, MicroscopeState { blur_a, blur_b, edge_rt });
    }

    // Execute a single fullscreen pass with the 5-binding layout.
    fn draw_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        uniforms: &MicroscopeUniforms,
        main_view: &wgpu::TextureView,
        blur_view: &wgpu::TextureView,
        edge_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        label: &str,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
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
                    resource: wgpu::BindingResource::TextureView(blur_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(edge_view),
                },
            ],
        });

        {
            let ts = profiler.and_then(|p| p.render_timestamps(label, 0, 0));
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
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

// Clear a RenderTarget to transparent black.
// Unity ref: RenderTextureUtil.Clear() — zeros texture contents.
// Used by ClearOwnerState pattern; currently the drop-and-recreate strategy
// suffices (new textures are zero-initialized by the GPU).
#[allow(dead_code)]
fn clear_render_target(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("Microscope Clear RT"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
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
}

impl PostProcessEffect for MicroscopeFX {
    fn effect_type(&self) -> EffectType {
        EffectType::Microscope
    }

    // MicroscopeFX.cs line 58: if (amount <= 0f || material == null) return;
    // Default should_skip (param[0] <= 0) handles this.

    fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        fx: &EffectInstance,
        ctx: &EffectContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        // MicroscopeFX.cs lines 57-71 — read params
        let amount = fx.param_values.first().copied().unwrap_or(0.0);
        // amount <= 0 is handled by should_skip; material == null case is not possible here
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

        // MicroscopeFX.cs lines 62-71 — GetParam calls
        let zoom        = fx.param_values.get(1).copied().unwrap_or(1.0);
        let focus       = fx.param_values.get(2).copied().unwrap_or(0.5);
        let dof         = fx.param_values.get(3).copied().unwrap_or(0.0);
        let aberration  = fx.param_values.get(4).copied().unwrap_or(0.0);
        // MicroscopeFX.cs line 66: int illumination = Mathf.RoundToInt(fx.GetParam(5))
        let illumination = fx.param_values.get(5).copied().unwrap_or(0.0).round();
        let structure   = fx.param_values.get(6).copied().unwrap_or(0.0);
        let distortion  = fx.param_values.get(7).copied().unwrap_or(0.0);
        let drift       = fx.param_values.get(8).copied().unwrap_or(0.0);
        let noise       = fx.param_values.get(9).copied().unwrap_or(0.0);
        let dust        = fx.param_values.get(10).copied().unwrap_or(0.0);

        // Texel sizes for blur passes use blur RT dimensions (quarter-res)
        let blur_w = (ctx.width / 4).max(1);
        let blur_h = (ctx.height / 4).max(1);
        let texel_x = 1.0 / blur_w as f32;
        let texel_y = 1.0 / blur_h as f32;
        let texel_z = blur_w as f32;
        let texel_w = blur_h as f32;

        // Base uniforms shared across all passes
        let base = MicroscopeUniforms {
            mode: 0,
            amount,
            zoom,
            focus,
            dof,
            aberration,
            illumination,
            structure,
            distortion,
            drift,
            noise,
            dust,
            texel_x,
            texel_y,
            texel_z,
            texel_w,
            time: ctx.time,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        // MicroscopeFX.cs lines 88-89 — needs_blur / needs_edge guards
        let needs_blur = dof > 0.0 || structure > 0.0;
        let needs_edge = illumination as i32 > 0;

        // MicroscopeFX.cs lines 91-96 — Pass 0-1: Separable Gaussian blur
        if needs_blur {
            // Pass 0: H blur — source → blur_a
            // MicroscopeFX.cs line 94: Graphics.Blit(buffer, state.blurA, material, 0)
            let blur_a_view = &state.blur_a.view;
            self.draw_pass(device, queue, encoder,
                &MicroscopeUniforms { mode: 0, ..base },
                source, &self.dummy_view, &self.dummy_view,
                blur_a_view,
                "Microscope HBlur",
                profiler,
            );

            // Pass 1: V blur — blur_a → blur_b
            // MicroscopeFX.cs line 95: Graphics.Blit(state.blurA, state.blurB, material, 1)
            let blur_a_view = &state.blur_a.view;
            let blur_b_view = &state.blur_b.view;
            self.draw_pass(device, queue, encoder,
                &MicroscopeUniforms { mode: 1, ..base },
                blur_a_view, &self.dummy_view, &self.dummy_view,
                blur_b_view,
                "Microscope VBlur",
                profiler,
            );
        }

        // MicroscopeFX.cs lines 98-102 — Pass 2: Sobel edge detection
        if needs_edge {
            // MicroscopeFX.cs line 101: Graphics.Blit(buffer, state.edgeRT, material, 2)
            let edge_view = &state.edge_rt.view;
            self.draw_pass(device, queue, encoder,
                &MicroscopeUniforms { mode: 2, ..base },
                source, &self.dummy_view, &self.dummy_view,
                edge_view,
                "Microscope Edge",
                profiler,
            );
        }

        // MicroscopeFX.cs lines 104-111 — Pass 3: Final composite
        // material.SetTexture("_BlurTex", needsBlur ? state.blurB : buffer)
        // material.SetTexture("_EdgeTex", needsEdge ? state.edgeRT : Texture2D.blackTexture)
        let blur_view: &wgpu::TextureView = if needs_blur {
            &state.blur_b.view
        } else {
            source
        };
        let edge_view: &wgpu::TextureView = if needs_edge {
            &state.edge_rt.view
        } else {
            &self.dummy_view // black texture (dummy is zero-initialized)
        };
        // MicroscopeFX.cs lines 108-111: Blit(buffer, target, material, 3)
        self.draw_pass(device, queue, encoder,
            &MicroscopeUniforms { mode: 3, ..base },
            source, blur_view, edge_view,
            target,
            "Microscope Composite",
            profiler,
        );
    }

    // MicroscopeFX.cs lines 116-120 — ClearState (all owners, keep entries)
    fn clear_state(&mut self) {
        // Unity: foreach owner → RenderTextureUtil.Clear() (zeros contents, keeps RT alive)
        // In wgpu we drop entries; they re-create on next apply() with zeroed contents.
        self.states.clear();
    }

    // MicroscopeFX.cs lines 32-36 — InitializeState / resize
    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let format = wgpu::TextureFormat::Rgba16Float;
        let blur_w = (width / 4).max(1);
        let blur_h = (height / 4).max(1);
        for (key, state) in &mut self.states {
            state.blur_a = RenderTarget::new(device, blur_w, blur_h, format,
                &format!("MicroscopeBlurA_{key}"));
            state.blur_b = RenderTarget::new(device, blur_w, blur_h, format,
                &format!("MicroscopeBlurB_{key}"));
            state.edge_rt = RenderTarget::new(device, width, height, format,
                &format!("MicroscopeEdge_{key}"));
        }
    }

    fn cleanup_owner_state(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
}

impl StatefulEffect for MicroscopeFX {
    // MicroscopeFX.cs lines 122-126 — ClearState(int ownerKey)
    // Unity: if exists → ClearOwnerState (zeros texture contents, keeps entry)
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        // Remove entry; re-creates with zeroed textures on next ensure_state.
        self.states.remove(&owner_key);
    }

    // MicroscopeFX.cs lines 135-142 — CleanupOwner
    fn cleanup_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }

    // MicroscopeFX.cs lines 144-148 — CleanupAllOwners
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) {
        self.states.clear();
    }
}

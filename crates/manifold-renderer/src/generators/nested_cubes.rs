//! Nested Cubes generator — instanced gap-face cubes with EMA-smoothed Y rotation.
//!
//! Replicates the TouchDesigner "Primitive SOP + instancing + Filter CHOP" pattern:
//! - 6 unwelded quads scaled 0.5 from face centers (gap-face cube)
//! - 5 instances with ramp scaling (1.0 → 2.0) and lagged Y-axis rotation
//! - Two-pass rendering: solid black occluders + white quad-edge lines
//! - Isometric orthographic camera

use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::generators::mesh_pipeline::{look_at_rh, mat4_mul};
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

const SHADER: &str = include_str!("shaders/nested_cubes.wgsl");

const INSTANCE_COUNT: u32 = 5;
/// 6 faces × 2 triangles × 3 vertices
const TRI_VERTEX_COUNT: u32 = 36;
/// 6 faces × 4 edges × 2 endpoints
const EDGE_VERTEX_COUNT: u32 = 48;

/// Instance sizes: linear ramp from 1.0 to 2.0.
const INSTANCE_SIZES: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

/// Uniform data matching the WGSL `Uniforms` struct.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NestedCubesUniforms {
    view_proj: [[f32; 4]; 4],
    sizes_0_3: [f32; 4],
    angles_0_3: [f32; 4],
    /// x: size[4], y: angle[4], z: color (0=black, 1=white), w: scatter (0..1)
    extra: [f32; 4],
    /// x: time (seconds)
    extra2: [f32; 4],
}

pub struct NestedCubesGenerator {
    /// Pass 1: triangle fill (vs_main + fs_main)
    fill_pipeline: manifold_gpu::GpuRenderPipeline,
    /// Pass 2: line edges (vs_edges + fs_main)
    edge_pipeline: manifold_gpu::GpuRenderPipeline,
    depth_stencil_write: manifold_gpu::GpuDepthStencilState,
    depth_stencil_read: manifold_gpu::GpuDepthStencilState,
    depth_texture: Option<manifold_gpu::GpuTexture>,
    depth_width: u32,
    depth_height: u32,
    /// EMA-smoothed rotation angles per instance (degrees).
    current_angles: [f32; 5],
}

impl NestedCubesGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let fill_pipeline = device.create_render_pipeline_depth(
            SHADER,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            manifold_gpu::GpuTextureFormat::Depth32Float,
            None,
            1,
            "Nested Cubes Fill",
        );

        let edge_pipeline = device.create_render_pipeline_depth(
            SHADER,
            "vs_edges",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            manifold_gpu::GpuTextureFormat::Depth32Float,
            None,
            1,
            "Nested Cubes Edges",
        );

        let depth_stencil_write =
            device.create_depth_stencil_state(&manifold_gpu::GpuDepthStencilDesc {
                compare: manifold_gpu::GpuCompareFunction::LessEqual,
                write_enabled: true,
            });

        let depth_stencil_read =
            device.create_depth_stencil_state(&manifold_gpu::GpuDepthStencilDesc {
                compare: manifold_gpu::GpuCompareFunction::LessEqual,
                write_enabled: false,
            });

        Self {
            fill_pipeline,
            edge_pipeline,
            depth_stencil_write,
            depth_stencil_read,
            depth_texture: None,
            depth_width: 0,
            depth_height: 0,
            current_angles: [0.0; 5],
        }
    }

    fn ensure_depth_texture(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
    ) {
        if self.depth_width == width
            && self.depth_height == height
            && self.depth_texture.is_some()
        {
            return;
        }
        self.depth_texture = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Depth32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET,
            label: "Nested Cubes Depth",
            mip_levels: 1,
        }));
        self.depth_width = width;
        self.depth_height = height;
    }
}

impl Generator for NestedCubesGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::NESTED_CUBES
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        let speed = if ctx.param_count > 0 {
            ctx.params[0]
        } else {
            1.0
        };
        let filter_width = if ctx.param_count > 1 {
            ctx.params[1]
        } else {
            2.0
        };
        let scale = if ctx.param_count > 2 {
            ctx.params[2]
        } else {
            1.0
        };
        let scatter = if ctx.param_count > 3 {
            ctx.params[3]
        } else {
            0.0
        };

        let time = ctx.time as f32;
        let dt = ctx.dt;

        // Update EMA-smoothed angles per instance.
        // Target: (i / 4.0) * 360.0 + time * 36.0 * speed
        // EMA: current = mix(current, target, 1 - exp(-dt * filter_width))
        let alpha = 1.0 - (-dt * filter_width).exp();
        for i in 0..5 {
            let target_angle = (i as f32 / 4.0) * 360.0 + time * 36.0 * speed;
            self.current_angles[i] += alpha * (target_angle - self.current_angles[i]);
        }

        // Scaled sizes
        let sizes: [f32; 5] = std::array::from_fn(|i| INSTANCE_SIZES[i] * scale);

        // Isometric orthographic camera — ortho width 3.41, aspect-corrected height
        let half_w = 1.705;
        let half_h = half_w / ctx.aspect;
        let view = look_at_rh(
            [2.887, 2.887, 2.887], // ~5 * normalize(1,1,1)
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
        );
        let proj = ortho_rh(half_w, half_h, 0.1, 20.0);
        let view_proj = mat4_mul(proj, view);

        // Build uniforms
        let uniforms = NestedCubesUniforms {
            view_proj,
            sizes_0_3: [sizes[0], sizes[1], sizes[2], sizes[3]],
            angles_0_3: [
                self.current_angles[0],
                self.current_angles[1],
                self.current_angles[2],
                self.current_angles[3],
            ],
            extra: [sizes[4], self.current_angles[4], 0.0, scatter],
            extra2: [time, 0.0, 0.0, 0.0],
        };

        self.ensure_depth_texture(gpu.device, ctx.width, ctx.height);
        let depth_tex = self.depth_texture.as_ref().unwrap();

        // Pass 1: Solid black occluders (triangles, depth write, depth bias)
        gpu.native_enc.draw_instanced_depth_ex(
            &self.fill_pipeline,
            target,
            depth_tex,
            &self.depth_stencil_write,
            &[manifold_gpu::GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            }],
            TRI_VERTEX_COUNT,
            INSTANCE_COUNT,
            manifold_gpu::GpuLoadAction::Clear,
            manifold_gpu::GpuTriangleFillMode::Fill,
            manifold_gpu::GpuPrimitiveType::Triangle,
            Some((1.0, 1.0, 0.0)),
            "Nested Cubes Fill",
        );

        // Pass 2: White quad-edge lines (line primitives, depth read only, no bias)
        let edge_uniforms = NestedCubesUniforms {
            extra: [sizes[4], self.current_angles[4], 1.0, scatter],
            ..uniforms
        };

        gpu.native_enc.draw_instanced_depth_ex(
            &self.edge_pipeline,
            target,
            depth_tex,
            &self.depth_stencil_read,
            &[manifold_gpu::GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&edge_uniforms),
            }],
            EDGE_VERTEX_COUNT,
            INSTANCE_COUNT,
            manifold_gpu::GpuLoadAction::Load,
            manifold_gpu::GpuTriangleFillMode::Fill,
            manifold_gpu::GpuPrimitiveType::Line,
            None,
            "Nested Cubes Edges",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        // Depth texture resized lazily in render()
    }

    fn reset_state(&mut self, _device: &manifold_gpu::GpuDevice) {
        self.current_angles = [0.0; 5];
    }
}

// ─── Orthographic projection (right-handed, depth [0,1] for Metal) ──

fn ortho_rh(half_width: f32, half_height: f32, z_near: f32, z_far: f32) -> [[f32; 4]; 4] {
    let inv_w = 1.0 / half_width;
    let inv_h = 1.0 / half_height;
    let inv_d = 1.0 / (z_near - z_far);
    [
        [inv_w, 0.0, 0.0, 0.0],
        [0.0, inv_h, 0.0, 0.0],
        [0.0, 0.0, inv_d, 0.0],
        [0.0, 0.0, z_near * inv_d, 1.0],
    ]
}

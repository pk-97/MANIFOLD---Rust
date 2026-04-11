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
use crate::generators::registration::GeneratorFactory;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::NESTED_CUBES,
        create: |device| Box::new(NestedCubesGenerator::new(device)),
    }
}

const SHADER: &str = include_str!("shaders/nested_cubes.wgsl");

const INSTANCE_COUNT: u32 = 5;
/// 6 faces × 2 triangles × 3 vertices
const TRI_VERTEX_COUNT: u32 = 36;
/// 6 faces × 4 edges × 2 endpoints
const EDGE_VERTEX_COUNT: u32 = 48;

/// Instance sizes: linear ramp from 1.0 to 2.0.
const INSTANCE_SIZES: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

// Param indices
const SPEED: usize = 0;
const FILTER: usize = 1;
const SCALE: usize = 2;
const SCATTER: usize = 3;
const SNAP: usize = 4;
const SNAP_MODE: usize = 5;

const MODE_ENVELOPE: i32 = 0;
const MODE_POSE: i32 = 1;

/// Exponential decay rate for envelope mode (~300ms to near-zero).
const SNAP_DECAY_RATE: f32 = 10.0;

/// Number of preset poses for pose mode.
const POSE_COUNT: u32 = 6;

/// Preset angular arrangements (degrees) for each of the 5 instances.
const POSES: [[f32; 5]; 6] = [
    [0.0, 90.0, 180.0, 270.0, 360.0],     // cross
    [0.0, 45.0, 90.0, 135.0, 180.0],       // fan
    [0.0, 72.0, 144.0, 216.0, 288.0],      // pentagonal
    [0.0, 30.0, 120.0, 210.0, 300.0],      // asymmetric star
    [0.0, 60.0, 60.0, 120.0, 180.0],       // nested pairs
    [0.0, 0.0, 90.0, 90.0, 180.0],         // stacked pairs
];

/// Uniform data matching the WGSL `Uniforms` struct.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NestedCubesUniforms {
    view_proj: [[f32; 4]; 4],
    sizes_0_3: [f32; 4],
    angles_0_3: [f32; 4],
    /// x: size[4], y: angle[4], z: color (0=black, 1=white), w: scatter (0..1)
    extra: [f32; 4],
    /// x: time (seconds), y: snap_envelope (0..1)
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
    /// Committed target angles (what cubes settle to).
    target_angles: [f32; 5],
    /// Snap envelope (1.0 on trigger, decays to 0).
    snap_envelope: f32,
    /// Last seen trigger count for detecting new triggers.
    last_trigger_count: i32,
    /// Current pose index for pose mode.
    pose_index: u32,
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

        // Initial static spread: (i/4) * 360
        let initial_angles: [f32; 5] = std::array::from_fn(|i| (i as f32 / 4.0) * 360.0);

        Self {
            fill_pipeline,
            edge_pipeline,
            depth_stencil_write,
            depth_stencil_read,
            depth_texture: None,
            depth_width: 0,
            depth_height: 0,
            current_angles: initial_angles,
            target_angles: initial_angles,
            snap_envelope: 0.0,
            last_trigger_count: -1,
            pose_index: 0,
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
        let speed = if ctx.param_count > SPEED as u32 {
            ctx.params[SPEED]
        } else {
            1.0
        };
        let filter_width = if ctx.param_count > FILTER as u32 {
            ctx.params[FILTER]
        } else {
            2.0
        };
        let scale = if ctx.param_count > SCALE as u32 {
            ctx.params[SCALE]
        } else {
            1.0
        };
        let scatter = if ctx.param_count > SCATTER as u32 {
            ctx.params[SCATTER]
        } else {
            0.0
        };
        let snap_on = ctx.param_count > SNAP as u32 && ctx.params[SNAP] > 0.5;
        let snap_mode = if ctx.param_count > SNAP_MODE as u32 {
            (ctx.params[SNAP_MODE].round() as i32).clamp(MODE_ENVELOPE, MODE_POSE)
        } else {
            MODE_ENVELOPE
        };

        let time = ctx.time as f32;
        let dt = ctx.dt;

        // Detect new trigger
        let trigger_count = ctx.trigger_count as i32;
        if trigger_count != self.last_trigger_count {
            let should_snap = snap_on && self.last_trigger_count >= 0;
            self.last_trigger_count = trigger_count;

            if should_snap {
                match snap_mode {
                    MODE_ENVELOPE => {
                        // Kick envelope, advance targets by 90° * speed
                        self.snap_envelope = 1.0;
                        let rotation = 90.0 * speed;
                        for i in 0..5 {
                            self.target_angles[i] += rotation;
                        }
                    }
                    MODE_POSE => {
                        // Jump to next preset pose
                        self.pose_index = (self.pose_index + 1) % POSE_COUNT;
                        let pose = POSES[self.pose_index as usize];
                        self.target_angles.copy_from_slice(&pose);
                    }
                    _ => {}
                }
            }
        }

        // Decay envelope
        if self.snap_envelope > 0.001 {
            self.snap_envelope *= (-SNAP_DECAY_RATE * dt).exp();
        } else {
            self.snap_envelope = 0.0;
        }

        // EMA-smooth current angles toward targets
        let alpha = 1.0 - (-dt * filter_width).exp();
        for i in 0..5 {
            self.current_angles[i] += alpha * (self.target_angles[i] - self.current_angles[i]);
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
            extra2: [time, self.snap_envelope, 0.0, 0.0],
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
        let initial: [f32; 5] = std::array::from_fn(|i| (i as f32 / 4.0) * 360.0);
        self.current_angles = initial;
        self.target_angles = initial;
        self.snap_envelope = 0.0;
        self.last_trigger_count = -1;
        self.pose_index = 0;
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

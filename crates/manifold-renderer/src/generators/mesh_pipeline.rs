//! MeshPipeline — shared infrastructure for depth-tested instanced 3D mesh rendering.
//!
//! Analogous to `LinePipeline` for 2D line rendering:
//! - Generators fill an instance buffer (position, scale, rotation per instance)
//! - MeshPipeline handles the depth-tested draw with two-point lighting
//!
//! The cube geometry is procedurally generated in the vertex shader from
//! `vertex_index` — no vertex buffer needed (same pattern as LinePipeline).

use crate::gpu_encoder::GpuEncoder;

/// Per-instance data uploaded to the GPU storage buffer.
/// Layout matches WGSL `Instance` struct: two vec4s.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshInstance {
    /// xyz: world-space position, w: uniform scale.
    pub pos_scale: [f32; 4],
    /// xyz: Euler rotation in radians (XYZ order), w: padding.
    pub rot_pad: [f32; 4],
}

/// Maximum number of instances. 100K for Galactic Rock, with headroom.
const MAX_INSTANCES: u64 = 131072;
const INSTANCE_STRIDE: u64 = 32; // 2 × vec4<f32>

/// Uniform data for the mesh pipeline.
/// Must match WGSL `Uniforms` struct exactly (16-byte aligned).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshUniforms {
    pub view_proj: [[f32; 4]; 4],
    pub camera_pos: [f32; 4],
    pub light0_pos: [f32; 4],
    pub light1_pos: [f32; 4],
    pub light0_color: [f32; 4],
    pub light1_color: [f32; 4],
    pub ambient_color: [f32; 4],
    /// x: metallic, y: roughness, z: instance_count (unused in shader), w: unused
    pub material: [f32; 4],
}

/// GPU pipeline for instanced 3D mesh rendering with depth testing.
///
/// Owns the render pipeline, depth-stencil state, depth texture, and
/// instance storage buffer. Generators write instances, then call `draw()`.
pub struct MeshPipeline {
    pipeline: manifold_gpu::GpuRenderPipeline,
    depth_stencil: manifold_gpu::GpuDepthStencilState,
    instances_buf: manifold_gpu::GpuBuffer,
    depth_texture: Option<manifold_gpu::GpuTexture>,
    depth_width: u32,
    depth_height: u32,
}

impl MeshPipeline {
    /// Create a new MeshPipeline. Call once during generator initialization.
    pub fn new(device: &manifold_gpu::GpuDevice, label: &str) -> Self {
        let pipeline = device.create_render_pipeline_depth(
            include_str!("shaders/mesh_pipeline.wgsl"),
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            manifold_gpu::GpuTextureFormat::Depth32Float,
            None, // no blending — opaque geometry with depth test
            1,    // no MSAA (depth testing provides edge definition)
            &format!("{label} Mesh"),
        );

        let depth_stencil = device.create_depth_stencil_state(&manifold_gpu::GpuDepthStencilDesc {
            compare: manifold_gpu::GpuCompareFunction::Less,
            write_enabled: true,
        });

        let instances_buf = device.create_buffer_shared(MAX_INSTANCES * INSTANCE_STRIDE);

        Self {
            pipeline,
            depth_stencil,
            instances_buf,
            depth_texture: None,
            depth_width: 0,
            depth_height: 0,
        }
    }

    /// Ensure the depth texture matches the target dimensions.
    fn ensure_depth_texture(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
    ) {
        if self.depth_width == width && self.depth_height == height && self.depth_texture.is_some()
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
            label: "Mesh Depth",
            mip_levels: 1,
        }));
        self.depth_width = width;
        self.depth_height = height;
    }

    /// Draw instanced meshes with depth testing and two-point lighting.
    ///
    /// `instances`: per-instance data (position, scale, rotation).
    /// `uniforms`: camera, lighting, and material configuration.
    /// `target`: the color render target (Rgba16Float).
    pub fn draw(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        instances: &[MeshInstance],
        uniforms: &MeshUniforms,
        label: &str,
        width: u32,
        height: u32,
    ) {
        if instances.is_empty() {
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            return;
        }

        // Ensure depth texture matches target dimensions
        self.ensure_depth_texture(gpu.device, width, height);
        let depth_tex = self.depth_texture.as_ref().unwrap();

        // Write instance data to shared-memory buffer
        let inst_bytes = bytemuck::cast_slice(instances);
        let inst_limit = (MAX_INSTANCES * INSTANCE_STRIDE) as usize;
        let inst_len = inst_bytes.len().min(inst_limit);
        unsafe {
            self.instances_buf.write(0, &inst_bytes[..inst_len]);
        }

        let instance_count = (instances.len() as u64).min(MAX_INSTANCES) as u32;

        // 36 vertices per cube (6 faces × 2 triangles × 3 vertices)
        const CUBE_VERTEX_COUNT: u32 = 36;

        gpu.native_enc.draw_instanced_depth(
            &self.pipeline,
            target,
            depth_tex,
            &self.depth_stencil,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(uniforms),
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1,
                    buffer: &self.instances_buf,
                    offset: 0,
                },
            ],
            CUBE_VERTEX_COUNT,
            instance_count,
            manifold_gpu::GpuLoadAction::Clear,
            label,
        );
    }
}

// ─── Camera helpers ─────────────────────────────────────────────────

/// Build a perspective projection matrix (right-handed, depth [0,1] for Metal).
pub fn perspective_rh(fov_y_rad: f32, aspect: f32, z_near: f32, z_far: f32) -> [[f32; 4]; 4] {
    let f = 1.0 / (fov_y_rad * 0.5).tan();
    let range = z_far / (z_near - z_far);
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, range, -1.0],
        [0.0, 0.0, range * z_near, 0.0],
    ]
}

/// Build a look-at view matrix (right-handed).
pub fn look_at_rh(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> [[f32; 4]; 4] {
    let f = normalize3(sub3(target, eye));
    let s = normalize3(cross3(f, up));
    let u = cross3(s, f);

    [
        [s[0], u[0], -f[0], 0.0],
        [s[1], u[1], -f[1], 0.0],
        [s[2], u[2], -f[2], 0.0],
        [-dot3(s, eye), -dot3(u, eye), dot3(f, eye), 1.0],
    ]
}

/// Multiply two 4×4 column-major matrices: result = A * B.
///
/// Storage: `m[col][row]` — matches WGSL `mat4x4<f32>` layout.
/// For view-projection: `mat4_mul(proj, view)` produces P * V.
pub fn mat4_mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for col in 0..4 {
        for row in 0..4 {
            out[col][row] = a[0][row] * b[col][0]
                + a[1][row] * b[col][1]
                + a[2][row] * b[col][2]
                + a[3][row] * b[col][3];
        }
    }
    out
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-10 {
        return [0.0, 0.0, 1.0];
    }
    [v[0] / len, v[1] / len, v[2] / len]
}

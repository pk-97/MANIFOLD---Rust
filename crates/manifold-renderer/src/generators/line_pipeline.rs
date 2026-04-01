use crate::generators::generator_math::DEFAULT_DOT_RADIUS;
use crate::gpu_encoder::GpuEncoder;

/// Per-instance edge data uploaded to the GPU storage buffer.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EdgeInstance {
    pub a: u32,
    pub b: u32,
    pub alpha_bits: u32,
    pub _pad: u32,
}

/// Maximum number of projected vertex positions (Duocylinder = 576).
const MAX_POSITIONS: u64 = 1024;
/// Maximum instances (edges + dots). Duocylinder = 1152 edges + 576 dots = 1728.
const MAX_INSTANCES: u64 = 2048;

const POSITION_STRIDE: u64 = 8; // vec2<f32>
const INSTANCE_STRIDE: u64 = 16; // EdgeInstance

/// 4x MSAA for line rendering. On Apple Silicon TBDR, MSAA samples live in
/// tile memory and resolve on-chip — the multisample texture is memoryless
/// (zero VRAM cost). This eliminates stair-stepping on diagonal line edges.
const MSAA_SAMPLE_COUNT: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LineUniforms {
    rt_width: f32,
    rt_height: f32,
    edge_half_thick: f32,
    beat: f32,
    dot_half_thick: f32,
    num_edges: u32,
    _pad: [f32; 2],
}

/// GPU pipeline for instanced anti-aliased line rendering with capsule SDF
/// and 4x MSAA. The MSAA texture is memoryless (Apple Silicon tile memory).
pub struct LinePipeline {
    pipeline: manifold_gpu::GpuRenderPipeline,
    positions_buf: manifold_gpu::GpuBuffer,
    instances_buf: manifold_gpu::GpuBuffer,
    msaa_texture: Option<manifold_gpu::GpuTexture>,
    msaa_width: u32,
    msaa_height: u32,
}

impl LinePipeline {
    pub fn new(device: &manifold_gpu::GpuDevice, label: &str) -> Self {
        let blend = manifold_gpu::GpuBlendState {
            src_factor: manifold_gpu::GpuBlendFactor::One,
            dst_factor: manifold_gpu::GpuBlendFactor::One,
            operation: manifold_gpu::GpuBlendOp::Max,
            src_alpha_factor: manifold_gpu::GpuBlendFactor::One,
            dst_alpha_factor: manifold_gpu::GpuBlendFactor::One,
            alpha_operation: manifold_gpu::GpuBlendOp::Max,
        };
        let pipeline = device.create_render_pipeline_msaa(
            include_str!("shaders/generator_lines.wgsl"),
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            Some(blend),
            MSAA_SAMPLE_COUNT,
            &format!("{label} Line"),
        );
        let positions_buf =
            device.create_buffer_shared(MAX_POSITIONS * POSITION_STRIDE);
        let instances_buf =
            device.create_buffer_shared(MAX_INSTANCES * INSTANCE_STRIDE);

        Self {
            pipeline,
            positions_buf,
            instances_buf,
            msaa_texture: None,
            msaa_width: 0,
            msaa_height: 0,
        }
    }

    /// Ensure the memoryless MSAA texture matches the target dimensions.
    fn ensure_msaa_texture(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
    ) {
        if self.msaa_width == width && self.msaa_height == height
            && self.msaa_texture.is_some()
        {
            return;
        }
        self.msaa_texture = Some(device.create_texture_msaa_memoryless(
            width,
            height,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            MSAA_SAMPLE_COUNT,
            "Line MSAA",
        ));
        self.msaa_width = width;
        self.msaa_height = height;
    }

    /// Draw line edges and dots via instanced 4x MSAA rendering.
    ///
    /// `positions`: screen-space [0,1] vertex positions (aspect-corrected).
    /// `instances`: edge + dot instance data (dots appended after edges).
    /// `num_edges`: how many of the instances are edges (rest are dots).
    /// `edge_half_thick` / `dot_half_thick`: half-thickness in pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        positions: &[[f32; 2]],
        instances: &[EdgeInstance],
        num_edges: u32,
        edge_half_thick: f32,
        dot_half_thick: f32,
        beat: f32,
        label: &str,
        width: u32,
        height: u32,
    ) {
        if instances.is_empty() {
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            return;
        }

        // Ensure MSAA texture matches target dimensions
        self.ensure_msaa_texture(gpu.device, width, height);
        let msaa_tex = self.msaa_texture.as_ref().unwrap();

        // Write data directly to shared-memory buffers
        let uniforms = LineUniforms {
            rt_width: width as f32,
            rt_height: height as f32,
            edge_half_thick,
            beat,
            dot_half_thick,
            num_edges,
            _pad: [0.0; 2],
        };

        let pos_bytes = bytemuck::cast_slice(positions);
        let pos_limit = (MAX_POSITIONS * POSITION_STRIDE) as usize;
        let pos_len = pos_bytes.len().min(pos_limit);
        unsafe {
            self.positions_buf.write(0, &pos_bytes[..pos_len]);
        }

        let inst_bytes = bytemuck::cast_slice(instances);
        let inst_limit = (MAX_INSTANCES * INSTANCE_STRIDE) as usize;
        let inst_len = inst_bytes.len().min(inst_limit);
        unsafe {
            self.instances_buf.write(0, &inst_bytes[..inst_len]);
        }

        let instance_count = (instances.len() as u64).min(MAX_INSTANCES) as u32;
        gpu.native_enc.draw_instanced_msaa(
            &self.pipeline,
            msaa_tex,
            target,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1,
                    buffer: &self.positions_buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 2,
                    buffer: &self.instances_buf,
                    offset: 0,
                },
            ],
            6,
            instance_count,
            manifold_gpu::GpuLoadAction::Clear,
            label,
        );
    }
}

// ─── LineGeneratorHelper ───

/// Shared helper for line-based generators. Manages projected vertices,
/// edge connectivity, animation state, and produces GPU-ready instance data.
pub struct LineGeneratorHelper {
    pub projected_x: Vec<f32>,
    pub projected_y: Vec<f32>,
    pub projected_z: Vec<f32>,
    pub edge_a: Vec<usize>,
    pub edge_b: Vec<usize>,
    pub anim_progress: f32,
    // GPU upload data
    positions: Vec<[f32; 2]>,
    instances: Vec<EdgeInstance>,
    // Depth sorting scratch buffers (Unity: LineMeshUtil.edgeDepth/edgeSortedIdx)
    edge_depth: Vec<f32>,
    edge_sorted_idx: Vec<usize>,
}

impl LineGeneratorHelper {
    pub fn new(vertex_count: usize, edge_count: usize) -> Self {
        Self {
            projected_x: vec![0.0; vertex_count],
            projected_y: vec![0.0; vertex_count],
            projected_z: vec![0.0; vertex_count],
            edge_a: Vec::with_capacity(edge_count),
            edge_b: Vec::with_capacity(edge_count),
            anim_progress: 0.0,
            positions: Vec::with_capacity(vertex_count),
            instances: Vec::with_capacity(edge_count + vertex_count),
            edge_depth: vec![0.0; edge_count],
            edge_sorted_idx: vec![0; edge_count],
        }
    }

    /// Resize projected arrays when vertex count changes.
    pub fn resize_vertices(&mut self, count: usize) {
        self.projected_x.resize(count, 0.0);
        self.projected_y.resize(count, 0.0);
        self.projected_z.resize(count, 0.0);
    }

    /// Prepare instance data for GPU upload. Returns (positions, instances, num_edges,
    /// edge_half_thick_px, dot_half_thick_px).
    ///
    /// Positions are in [0,1] screen-space with aspect correction applied.
    /// Instances contain edges first, then dots (if show_verts).
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_instances(
        &mut self,
        rt_height: f32,
        aspect: f32,
        line_thickness: f32,
        show_verts: bool,
        vert_size: f32,
        animate: bool,
        speed: f32,
        window: f32,
        scale: f32,
        dot_scale: f32,
        dt: f32,
    ) -> (&[[f32; 2]], &[EdgeInstance], u32, f32, f32) {
        let vert_count = self.projected_x.len();
        let edge_count = self.edge_a.len();
        let s = if scale <= 0.0 { 1.0 } else { scale };

        // Build screen-space positions (aspect-corrected, in [0,1])
        self.positions.clear();
        for i in 0..vert_count {
            self.positions.push([
                self.projected_x[i] * s / aspect + 0.5,
                self.projected_y[i] * s + 0.5,
            ]);
        }

        // Build edge instances
        self.instances.clear();
        let edge_half_thick = line_thickness * rt_height * 0.5;

        if animate && edge_count > 0 {
            // Depth sort edges back-to-front (Unity: LineMeshUtil.BuildEdgeQuads)
            self.ensure_sort_buffers(edge_count);
            for i in 0..edge_count {
                let a = self.edge_a[i];
                let b = self.edge_b[i];
                self.edge_depth[i] =
                    (self.projected_z[a] + self.projected_z[b]) * 0.5;
                self.edge_sorted_idx[i] = i;
            }
            let depths = &self.edge_depth;
            self.edge_sorted_idx[..edge_count].sort_by(|&a, &b| {
                depths[a]
                    .partial_cmp(&depths[b])
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            self.anim_progress += speed * (edge_count as f32 / 100.0) * dt * 60.0;
            let total = edge_count as f32;
            if self.anim_progress >= total {
                self.anim_progress -= total;
            }
            let window_edges =
                ((edge_count as f32 * window).ceil() as usize).max(1);
            let window_start = (self.anim_progress
                / (edge_count as f32 / 100.0).max(1.0))
            .floor() as usize
                % edge_count;

            for offset in 0..window_edges {
                let sort_pos = (window_start + offset) % edge_count;
                let edge_idx = self.edge_sorted_idx[sort_pos];
                let fade =
                    1.0 - offset as f32 / window_edges as f32;
                self.instances.push(EdgeInstance {
                    a: self.edge_a[edge_idx] as u32,
                    b: self.edge_b[edge_idx] as u32,
                    alpha_bits: fade.to_bits(),
                    _pad: 0,
                });
            }
        } else {
            for i in 0..edge_count {
                self.instances.push(EdgeInstance {
                    a: self.edge_a[i] as u32,
                    b: self.edge_b[i] as u32,
                    alpha_bits: 1.0_f32.to_bits(),
                    _pad: 0,
                });
            }
        }

        let num_edges = self.instances.len() as u32;

        // Append dot instances (same position for a and b → capsule degenerates to circle)
        let dot_half_thick = if show_verts {
            let base_radius =
                DEFAULT_DOT_RADIUS * rt_height * vert_size * dot_scale;
            for i in 0..vert_count {
                self.instances.push(EdgeInstance {
                    a: i as u32,
                    b: i as u32,
                    alpha_bits: 1.0_f32.to_bits(),
                    _pad: 0,
                });
            }
            base_radius
        } else {
            0.0
        };

        (
            &self.positions,
            &self.instances,
            num_edges,
            edge_half_thick,
            dot_half_thick,
        )
    }

    /// Ensure sort scratch buffers are large enough.
    fn ensure_sort_buffers(&mut self, edge_count: usize) {
        if self.edge_depth.len() < edge_count {
            self.edge_depth.resize(edge_count, 0.0);
            self.edge_sorted_idx.resize(edge_count, 0);
        }
    }
}

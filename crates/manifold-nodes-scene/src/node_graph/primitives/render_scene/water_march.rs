//! Per-pixel ray march that draws water straight from its level set.
//! One dispatch per field, nearest hit wins through the shared `hits`
//! G-buffer. Buffers grow only when the target size changes, and `reset`
//! drops them so a frame with no water costs nothing.
use manifold_gpu::*;

use super::mat4_inverse;

const MARCH_WGSL: &str = concat!(
    include_str!("../shaders/water_field_common.wgsl"),
    include_str!("../shaders/water_march_core.wgsl"),
    include_str!("../shaders/water_march.wgsl")
);

/// Bytes of one `FieldHit` in the G-buffer.
pub(super) const FIELD_HIT_BYTES: u64 = 12;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MarchUniforms {
    inv_view_proj: [[f32; 4]; 4],
    view_proj: [[f32; 4]; 4],
    prev_view_proj: [[f32; 4]; 4],
    world_to_local: [[f32; 4]; 4],
    local_to_world: [[f32; 4]; 4],
    prev_local_to_world: [[f32; 4]; 4],
    lattice_min: [f32; 4],
    spacing: [f32; 4],
    nodes: [u32; 4],
    target_size: [f32; 4],
    jitter: [f32; 4],
    flags: [u32; 4],
}

/// One water field to march this frame.
pub(super) struct FieldDraw<'a> {
    pub levelset: &'a GpuBuffer,
    pub velocity: Option<&'a GpuBuffer>,
    pub center: [f32; 3],
    pub size: [f32; 3],
    pub nodes: [u32; 3],
    pub model: [[f32; 4]; 4],
    pub prev_model: [[f32; 4]; 4],
    pub slot: u32,
}

/// This frame's camera, the same matrices and jitter the raster uses.
pub(super) struct FieldView {
    pub view_proj: [[f32; 4]; 4],
    pub prev_view_proj: [[f32; 4]; 4],
    pub jitter_ndc: [f32; 2],
    pub prev_jitter_ndc: [f32; 2],
    pub frame_seconds: f32,
    pub width: u32,
    pub height: u32,
}

#[derive(Default)]
pub(super) struct WaterMarch {
    pipeline: Option<GpuComputePipeline>,
    hits: Option<GpuBuffer>,
    hits_size: (u32, u32),
    dummy_depth_out: Option<GpuTexture>,
    dummy_velocity_out: Option<GpuTexture>,
}

fn dummy_texture(device: &GpuDevice, format: GpuTextureFormat, label: &'static str) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        mip_levels: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label,
    })
}

/// The part of a `FieldDraw` the uniforms depend on, so they build without a buffer.
pub(super) struct FieldGeometry {
    pub center: [f32; 3],
    pub size: [f32; 3],
    pub nodes: [u32; 3],
    pub model: [[f32; 4]; 4],
    pub prev_model: [[f32; 4]; 4],
    pub slot: u32,
    pub has_velocity: bool,
}

impl FieldGeometry {
    fn of(field: &FieldDraw) -> Self {
        Self {
            center: field.center,
            size: field.size,
            nodes: field.nodes,
            model: field.model,
            prev_model: field.prev_model,
            slot: field.slot,
            has_velocity: field.velocity.is_some(),
        }
    }
}

/// The march uniforms for one field, without the output flags.
pub(super) fn march_uniforms(
    view: &FieldView,
    field: &FieldGeometry,
) -> Result<MarchUniforms, &'static str> {
    let inv_view_proj =
        mat4_inverse(view.view_proj).ok_or("Water march: singular camera matrix")?;
    let world_to_local =
        mat4_inverse(field.model).ok_or("Water march: singular water model matrix")?;
    let mut lattice_min = [0.0f32; 4];
    let mut spacing = [0.0f32; 4];
    let mut cell = f32::MAX;
    for axis in 0..3 {
        if field.nodes[axis] < 2 {
            return Err("Water march: lattice needs at least 2 nodes per axis");
        }
        lattice_min[axis] = field.center[axis] - field.size[axis] * 0.5;
        spacing[axis] = field.size[axis] / (field.nodes[axis] - 1) as f32;
        cell = cell.min(spacing[axis]);
    }
    lattice_min[3] = cell;
    spacing[3] = view.frame_seconds;
    Ok(MarchUniforms {
        inv_view_proj,
        view_proj: view.view_proj,
        prev_view_proj: view.prev_view_proj,
        world_to_local,
        local_to_world: field.model,
        prev_local_to_world: field.prev_model,
        lattice_min,
        spacing,
        nodes: [
            field.nodes[0],
            field.nodes[1],
            field.nodes[2],
            field.slot + 1,
        ],
        target_size: [
            view.width as f32,
            view.height as f32,
            if field.has_velocity { 1.0 } else { 0.0 },
            0.0,
        ],
        jitter: [
            view.jitter_ndc[0],
            view.jitter_ndc[1],
            view.prev_jitter_ndc[0],
            view.prev_jitter_ndc[1],
        ],
        flags: [0; 4],
    })
}

impl WaterMarch {
    /// Compile the march ahead of the first water frame.
    pub(super) fn prewarm(device: &GpuDevice) {
        device.create_compute_pipeline(MARCH_WGSL, "cs_march", "water level-set march");
    }

    /// Start a frame: make sure the pipeline, the dummies and a zeroed
    /// `hits` buffer of the right size exist.
    pub(super) fn begin(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        width: u32,
        height: u32,
    ) {
        if self.pipeline.is_none() {
            self.pipeline = Some(device.create_compute_pipeline(
                MARCH_WGSL,
                "cs_march",
                "water level-set march",
            ));
        }
        if self.dummy_depth_out.is_none() {
            self.dummy_depth_out = Some(dummy_texture(
                device,
                GpuTextureFormat::R32Float,
                "water march unused depth out",
            ));
            self.dummy_velocity_out = Some(dummy_texture(
                device,
                GpuTextureFormat::Rg16Float,
                "water march unused velocity out",
            ));
        }
        if self.hits.is_none() || self.hits_size != (width, height) {
            self.hits =
                Some(device.create_buffer(u64::from(width) * u64::from(height) * FIELD_HIT_BYTES));
            self.hits_size = (width, height);
        }
        enc.clear_buffer(self.hits.as_ref().expect("ensured"));
    }

    /// March one field into `hits`, keeping whatever nearer field an earlier
    /// call already drew. `begin` must have run this frame with the same size.
    pub(super) fn encode(
        &mut self,
        enc: &mut GpuEncoder,
        view: &FieldView,
        field: &FieldDraw,
        opaque_depth: &GpuTexture,
        depth_out: Option<&GpuTexture>,
        velocity_out: Option<&GpuTexture>,
    ) -> Result<(), &'static str> {
        let mut uniforms = march_uniforms(view, &FieldGeometry::of(field))?;
        uniforms.flags[0] =
            u32::from(depth_out.is_some()) | (u32::from(velocity_out.is_some()) << 1);
        let pipeline = self
            .pipeline
            .as_ref()
            .ok_or("Water march: begin was not called")?;
        let hits = self
            .hits
            .as_ref()
            .ok_or("Water march: begin was not called")?;
        if self.hits_size != (view.width, view.height) {
            return Err("Water march: view size differs from begin");
        }
        let depth_out = match depth_out {
            Some(t) => t,
            None => self
                .dummy_depth_out
                .as_ref()
                .ok_or("Water march: begin was not called")?,
        };
        let velocity_out = match velocity_out {
            Some(t) => t,
            None => self
                .dummy_velocity_out
                .as_ref()
                .ok_or("Water march: begin was not called")?,
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: field.levelset,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: field.velocity.unwrap_or(field.levelset),
                offset: 0,
            },
            GpuBinding::Texture {
                binding: 3,
                texture: opaque_depth,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: hits,
                offset: 0,
            },
            GpuBinding::Texture {
                binding: 5,
                texture: depth_out,
            },
            GpuBinding::Texture {
                binding: 6,
                texture: velocity_out,
            },
        ];
        enc.dispatch_compute(
            pipeline,
            &bindings,
            [view.width.div_ceil(8), view.height.div_ceil(8), 1],
            "water level-set march",
        );
        Ok(())
    }

    /// The G-buffer this frame's marches wrote, if `begin` ran.
    pub(super) fn hits(&self) -> Option<&GpuBuffer> {
        self.hits.as_ref()
    }

    /// Drop the G-buffer so a frame with no water holds nothing.
    pub(super) fn reset(&mut self) {
        self.hits = None;
    }
}

#[cfg(test)]
mod tests {
    use super::super::model_matrix;
    use super::*;

    #[test]
    fn march_wgsl_parses_and_validates() {
        let src = MARCH_WGSL;
        let module = naga::front::wgsl::parse_str(src).expect("water march must naga-parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = validator.validate(&module) {
            panic!("water march validation failed:\n{}", e.emit_to_string(src));
        }
        assert!(module.entry_points.iter().any(|ep| ep.name == "cs_march"));
    }

    #[test]
    fn uniforms_are_six_matrices_and_six_vectors() {
        // Matches `MarchUniforms` in water_march.wgsl field for field.
        assert_eq!(std::mem::size_of::<MarchUniforms>(), 6 * 64 + 6 * 16);
    }

    fn view() -> FieldView {
        let m = model_matrix([0.0; 3], [0.0; 3], [1.0; 3]);
        FieldView {
            view_proj: m,
            prev_view_proj: m,
            jitter_ndc: [0.001, -0.002],
            prev_jitter_ndc: [0.003, 0.004],
            frame_seconds: 1.0 / 30.0,
            width: 96,
            height: 72,
        }
    }

    fn geometry(
        center: [f32; 3],
        size: [f32; 3],
        nodes: [u32; 3],
        model: [[f32; 4]; 4],
        slot: u32,
    ) -> FieldGeometry {
        FieldGeometry {
            center,
            size,
            nodes,
            model,
            prev_model: model_matrix([0.0; 3], [0.0; 3], [1.0; 3]),
            slot,
            has_velocity: false,
        }
    }

    #[test]
    fn non_cubic_lattice_derives_min_corner_spacing_and_cell() {
        let field = geometry(
            [0.1, -0.05, 0.2],
            [1.8, 1.6, 2.0],
            [37, 33, 41],
            model_matrix([0.3, 0.2, -0.4], [0.0, 0.35, 0.0], [1.25; 3]),
            4,
        );
        let u = march_uniforms(&view(), &field).expect("valid field");
        assert_eq!(u.lattice_min[0], 0.1 - 1.8 * 0.5);
        assert_eq!(u.lattice_min[1], -0.05 - 1.6 * 0.5);
        assert_eq!(u.lattice_min[2], 0.2 - 2.0 * 0.5);
        assert_eq!(u.spacing[0], 1.8 / 36.0);
        assert_eq!(u.spacing[1], 1.6 / 32.0);
        assert_eq!(u.spacing[2], 2.0 / 40.0);
        let cell = (1.8f32 / 36.0).min(1.6 / 32.0).min(2.0 / 40.0);
        assert_eq!(u.lattice_min[3], cell);
        assert_eq!(u.spacing[3], 1.0 / 30.0);
        assert_eq!(u.nodes, [37, 33, 41, 5]);
        assert_eq!(u.target_size[2], 0.0);
        assert_eq!(u.jitter, [0.001, -0.002, 0.003, 0.004]);
    }

    #[test]
    fn singular_model_is_an_error() {
        let field = geometry(
            [0.0; 3],
            [1.0; 3],
            [8, 8, 8],
            model_matrix([0.0; 3], [0.0; 3], [1.0, 0.0, 1.0]),
            0,
        );
        assert!(march_uniforms(&view(), &field).is_err());
    }
}

//! Per-pixel ray march that draws water straight from its level set.
//! One dispatch per field, nearest hit wins through the shared `hits`
//! G-buffer. Buffers grow only when the target size changes, and `reset`
//! drops them so a frame with no water costs nothing. A field with a mist
//! array then integrates its optical depth per pixel up to the nearest hit.
//! Every array is read through the field's world-tile table, and a ray's span
//! is its extent. Before a field's march, one workgroup per pool slot flags
//! what each held tile holds, so the marches step over the tiles with nothing
//! for them as they step over unheld ones.
use std::sync::LazyLock;

use manifold_gpu::*;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, table_entries};

use super::{FIELD_TILES_WGSL, mat4_inverse};

static MARCH_WGSL: LazyLock<String> = LazyLock::new(|| {
    [
        READ_SIDE,
        FIELD_TILES_WGSL,
        include_str!("../shaders/water_field_common.wgsl"),
        include_str!("../shaders/water_march_core.wgsl"),
        include_str!("../shaders/water_march.wgsl"),
    ]
    .concat()
});

static FLAGS_WGSL: LazyLock<String> = LazyLock::new(|| {
    [
        READ_SIDE,
        FIELD_TILES_WGSL,
        include_str!("../shaders/water_march_core.wgsl"),
        include_str!("../shaders/water_tile_flags.wgsl"),
    ]
    .concat()
});

static MIST_WGSL: LazyLock<String> = LazyLock::new(|| {
    [
        READ_SIDE,
        FIELD_TILES_WGSL,
        include_str!("../shaders/water_field_common.wgsl"),
        include_str!("../shaders/water_march_core.wgsl"),
        include_str!("../shaders/water_mist.wgsl"),
    ]
    .concat()
});

/// Bytes of one `FieldHit` in the G-buffer.
pub(super) const FIELD_HIT_BYTES: u64 = 12;

/// Bytes of one pixel of the mist buffer: `(tau, m)` as two `f32`.
const MIST_PIXEL_BYTES: u64 = 8;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MarchUniforms {
    inv_view_proj: [[f32; 4]; 4],
    view_proj: [[f32; 4]; 4],
    prev_view_proj: [[f32; 4]; 4],
    world_to_local: [[f32; 4]; 4],
    local_to_world: [[f32; 4]; 4],
    prev_local_to_world: [[f32; 4]; 4],
    /// x display cell size in metres, y frame delta in seconds.
    cell: [f32; 4],
    /// x pool tiles, y table entries, z slot + 1.
    tiles: [u32; 4],
    /// x width, y height, z 1 when a velocity array is bound.
    target_size: [f32; 4],
    jitter: [f32; 4],
    flags: [u32; 4],
}

/// One water field to march this frame.
pub(super) struct FieldDraw<'a> {
    pub levelset: &'a GpuBuffer,
    pub velocity: Option<&'a GpuBuffer>,
    /// Mist extinction per node, per metre.
    pub mist: Option<&'a GpuBuffer>,
    /// The world-tile table every array is read through.
    pub tiles: &'a GpuBuffer,
    /// Eight floats: the held tiles' node bounds in object space.
    pub extent: &'a GpuBuffer,
    pub pool_tiles: u32,
    pub cell_size: f32,
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
    flags_pipeline: Option<GpuComputePipeline>,
    /// Per field slot, one `u32` of TILE_ bits per pool slot, written by that
    /// field's march and read by its mist.
    tile_flags: Vec<GpuBuffer>,
    hits: Option<GpuBuffer>,
    hits_size: (u32, u32),
    mist_pipeline: Option<GpuComputePipeline>,
    /// `(tau, m)` per pixel, 8 bytes each, summed over this frame's mist fields.
    mist: Option<GpuBuffer>,
    mist_size: (u32, u32),
    /// Whether this frame's mist buffer has been cleared yet.
    mist_cleared: bool,
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
    pub pool_tiles: u32,
    pub cell_size: f32,
    pub model: [[f32; 4]; 4],
    pub prev_model: [[f32; 4]; 4],
    pub slot: u32,
    pub has_velocity: bool,
}

impl FieldGeometry {
    fn of(field: &FieldDraw) -> Self {
        Self {
            pool_tiles: field.pool_tiles,
            cell_size: field.cell_size,
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
    if !(field.cell_size > 0.0 && field.cell_size.is_finite()) || field.pool_tiles == 0 {
        return Err("Water march: the field needs a cell size above zero and a pool of tiles");
    }
    Ok(MarchUniforms {
        inv_view_proj,
        view_proj: view.view_proj,
        prev_view_proj: view.prev_view_proj,
        world_to_local,
        local_to_world: field.model,
        prev_local_to_world: field.prev_model,
        cell: [field.cell_size, view.frame_seconds, 0.0, 0.0],
        tiles: [
            field.pool_tiles,
            table_entries(field.pool_tiles),
            field.slot + 1,
            0,
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
        device.create_compute_pipeline(&MARCH_WGSL, "cs_march", "water level-set march");
        device.create_compute_pipeline(&FLAGS_WGSL, "cs_tile_flags", "water tile flags");
        device.create_compute_pipeline(&MIST_WGSL, "cs_mist", "water mist optical depth");
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
                &MARCH_WGSL,
                "cs_march",
                "water level-set march",
            ));
            self.flags_pipeline = Some(device.create_compute_pipeline(
                &FLAGS_WGSL,
                "cs_tile_flags",
                "water tile flags",
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
        self.mist_cleared = false;
    }

    /// Flag one field's tiles, then march it into `hits`, keeping whatever
    /// nearer field an earlier call already drew. `first_hit`, for a field
    /// with a surface mesh, is the water meshes' nearest depth over the
    /// opaque scene: the march searches only round it. `begin` must have run
    /// this frame with the same size.
    #[allow(
        clippy::too_many_arguments,
        reason = "one march dispatch: the field, the camera and its five targets"
    )]
    pub(super) fn encode(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        view: &FieldView,
        field: &FieldDraw,
        opaque_depth: &GpuTexture,
        first_hit: Option<&GpuTexture>,
        depth_out: Option<&GpuTexture>,
        velocity_out: Option<&GpuTexture>,
    ) -> Result<(), &'static str> {
        let mut uniforms = march_uniforms(view, &FieldGeometry::of(field))?;
        uniforms.flags[0] =
            u32::from(depth_out.is_some()) | (u32::from(velocity_out.is_some()) << 1);
        uniforms.flags[2] = u32::from(first_hit.is_some());
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
        let flags_pipeline = self
            .flags_pipeline
            .as_ref()
            .ok_or("Water march: begin was not called")?;
        let slot = field.slot as usize;
        let flag_bytes = u64::from(field.pool_tiles) * 4;
        if self.tile_flags.len() <= slot {
            self.tile_flags
                .resize_with(slot + 1, || device.create_buffer(flag_bytes));
        }
        if self.tile_flags[slot].size < flag_bytes {
            self.tile_flags[slot] = device.create_buffer(flag_bytes);
        }
        let tile_flags = &self.tile_flags[slot];
        let mut flag_uniforms = uniforms;
        flag_uniforms.flags[1] = u32::from(field.mist.is_some());
        enc.dispatch_compute(
            flags_pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&flag_uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: field.levelset,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 7,
                    buffer: field.mist.unwrap_or(field.levelset),
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 70,
                    buffer: field.tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 72,
                    buffer: tile_flags,
                    offset: 0,
                },
            ],
            [field.pool_tiles, 1, 1],
            "water tile flags",
        );
        enc.compute_memory_barrier_buffers();
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
            GpuBinding::Texture {
                binding: 8,
                texture: first_hit.unwrap_or(opaque_depth),
            },
            GpuBinding::Buffer {
                binding: 70,
                buffer: field.tiles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 71,
                buffer: field.extent,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 72,
                buffer: tile_flags,
                offset: 0,
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

    /// Adds one field's mist optical depth to the mist buffer, up to the
    /// nearest of the opaque scene and the water already in `hits`, so every
    /// march must have run first (the field's march also flags its tiles). `Ok(false)` and nothing done when the field
    /// has no mist.
    pub(super) fn encode_mist(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        view: &FieldView,
        field: &FieldDraw,
        opaque_depth: &GpuTexture,
    ) -> Result<bool, &'static str> {
        let Some(mist_field) = field.mist else {
            return Ok(false);
        };
        let uniforms = march_uniforms(view, &FieldGeometry::of(field))?;
        let hits = self
            .hits
            .as_ref()
            .ok_or("Water mist: begin was not called")?;
        if self.hits_size != (view.width, view.height) {
            return Err("Water mist: view size differs from begin");
        }
        if self.mist_pipeline.is_none() {
            self.mist_pipeline = Some(device.create_compute_pipeline(
                &MIST_WGSL,
                "cs_mist",
                "water mist optical depth",
            ));
        }
        if self.mist.is_none() || self.mist_size != (view.width, view.height) {
            self.mist =
                Some(device.create_buffer(
                    u64::from(view.width) * u64::from(view.height) * MIST_PIXEL_BYTES,
                ));
            self.mist_size = (view.width, view.height);
            self.mist_cleared = false;
        }
        let mist = self.mist.as_ref().expect("ensured");
        if !self.mist_cleared {
            enc.clear_buffer(mist);
            self.mist_cleared = true;
        }
        let pipeline = self.mist_pipeline.as_ref().expect("ensured");
        let tile_flags = self
            .tile_flags
            .get(field.slot as usize)
            .ok_or("Water mist: the field was not marched")?;
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
            GpuBinding::Texture {
                binding: 3,
                texture: opaque_depth,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: hits,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 7,
                buffer: mist_field,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 8,
                buffer: mist,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 70,
                buffer: field.tiles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 71,
                buffer: field.extent,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 72,
                buffer: tile_flags,
                offset: 0,
            },
        ];
        enc.dispatch_compute(
            pipeline,
            &bindings,
            [view.width.div_ceil(8), view.height.div_ceil(8), 1],
            "water mist optical depth",
        );
        Ok(true)
    }

    /// The per-pixel `(tau, m)` this frame's mist fields summed, once any
    /// field's `encode_mist` returned `true`.
    pub(super) fn mist(&self) -> Option<&GpuBuffer> {
        self.mist.as_ref()
    }

    /// The tile flags field `slot`'s march wrote this frame.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(super) fn tile_flags(&self, slot: u32) -> Option<&GpuBuffer> {
        self.tile_flags.get(slot as usize)
    }

    /// The G-buffer this frame's marches wrote, if `begin` ran.
    pub(super) fn hits(&self) -> Option<&GpuBuffer> {
        self.hits.as_ref()
    }

    /// Drop the G-buffer so a frame with no water holds nothing.
    pub(super) fn reset(&mut self) {
        self.hits = None;
        self.mist = None;
        self.tile_flags.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::super::model_matrix;
    use super::*;

    fn validates(name: &str, src: &str, entry: &str) {
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("{name} must naga-parse: {}", e.emit_to_string(src)));
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = validator.validate(&module) {
            panic!("{name} validation failed:\n{}", e.emit_to_string(src));
        }
        assert!(module.entry_points.iter().any(|ep| ep.name == entry));
    }

    #[test]
    fn march_wgsl_parses_and_validates() {
        validates("water march", &MARCH_WGSL, "cs_march");
    }

    #[test]
    fn flags_wgsl_parses_and_validates() {
        validates("water tile flags", &FLAGS_WGSL, "cs_tile_flags");
    }

    #[test]
    fn mist_wgsl_parses_and_validates() {
        validates("water mist", &MIST_WGSL, "cs_mist");
    }

    #[test]
    fn the_shader_far_value_is_the_rust_constant() {
        let line = FIELD_TILES_WGSL
            .lines()
            .find(|line| line.starts_with("const FIELD_FAR_CELLS"))
            .expect("field_tiles.wgsl declares FIELD_FAR_CELLS");
        let value: f32 = line
            .split('=')
            .nth(1)
            .and_then(|rest| rest.trim().strip_suffix(';'))
            .expect("const value")
            .trim()
            .parse()
            .expect("a float literal");
        assert_eq!(
            value,
            manifold_node_engine::scene::scene_object::FIELD_FAR_CELLS
        );
    }

    #[test]
    fn uniforms_are_six_matrices_and_five_vectors() {
        // Matches `MarchUniforms` in water_march_core.wgsl field for field.
        assert_eq!(std::mem::size_of::<MarchUniforms>(), 6 * 64 + 5 * 16);
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

    fn geometry(pool_tiles: u32, cell_size: f32, model: [[f32; 4]; 4], slot: u32) -> FieldGeometry {
        FieldGeometry {
            pool_tiles,
            cell_size,
            model,
            prev_model: model_matrix([0.0; 3], [0.0; 3], [1.0; 3]),
            slot,
            has_velocity: false,
        }
    }

    #[test]
    fn uniforms_carry_the_cell_the_table_size_and_the_slot() {
        // 37 tiles: not a power of two, so the table is the next one up from 4P.
        let field = geometry(
            37,
            0.045,
            model_matrix([0.3, 0.2, -0.4], [0.0, 0.35, 0.0], [1.25; 3]),
            4,
        );
        let u = march_uniforms(&view(), &field).expect("valid field");
        assert_eq!(u.cell, [0.045, 1.0 / 30.0, 0.0, 0.0]);
        assert_eq!(u.tiles, [37, 256, 5, 0]);
        assert_eq!(u.target_size, [96.0, 72.0, 0.0, 0.0]);
        assert_eq!(u.jitter, [0.001, -0.002, 0.003, 0.004]);
    }

    #[test]
    fn singular_model_or_empty_field_is_an_error() {
        let flat = model_matrix([0.0; 3], [0.0; 3], [1.0, 0.0, 1.0]);
        assert!(march_uniforms(&view(), &geometry(3, 0.05, flat, 0)).is_err());
        let identity = model_matrix([0.0; 3], [0.0; 3], [1.0; 3]);
        assert!(march_uniforms(&view(), &geometry(3, 0.05, identity, 0)).is_ok());
        for cell in [0.0, -0.05, f32::NAN, f32::INFINITY] {
            assert!(
                march_uniforms(&view(), &geometry(3, cell, identity, 0)).is_err(),
                "cell size {cell}"
            );
        }
        assert!(march_uniforms(&view(), &geometry(0, 0.05, identity, 0)).is_err());
    }
}

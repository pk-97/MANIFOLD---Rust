//! Smoke and fire in the scene renderer: each gas's packed fields unpacked
//! into 3D textures, a light volume of the optical depth toward the sun, and
//! a full-box ray march blended over the frame (`gas_light.wgsl`,
//! `gas_volume.wgsl`). The march lights the gas with the scene's own sun,
//! shadow map and irradiance map, so it sits beside the meshes and the water.
use std::sync::LazyLock;

use manifold_gpu::*;
use manifold_node_engine::load::expand::admit_candidate_bytes;
use manifold_node_engine::scene::blackbody::{
    BLACKBODY_ENTRIES, BLACKBODY_MAX_KELVIN, BLACKBODY_MIN_KELVIN, blackbody_table,
};
use manifold_node_engine::scene::scene_object::GasVolume;

use super::RenderSceneUniforms;
use super::water_shade::SCENE_BINDINGS;

/// The scene shading, the constants generated from the Rust side, then the
/// march.
pub(super) static GAS_VOLUME_WGSL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}\n{}\n{}",
        include_str!("../shaders/scene_shading.wgsl"),
        shader_constants(),
        include_str!("../shaders/gas_volume.wgsl"),
    )
});

/// The compute passes: unpack, light volume and noise fill.
const GAS_LIGHT_WGSL: &str = include_str!("../shaders/gas_light.wgsl");

/// The constants `gas_volume.wgsl` reads that this file owns, so the shader
/// and the table and noise they read cannot drift apart.
fn shader_constants() -> String {
    format!(
        "const BLACKBODY_MIN_KELVIN: f32 = {BLACKBODY_MIN_KELVIN:?};\n\
         const BLACKBODY_MAX_KELVIN: f32 = {BLACKBODY_MAX_KELVIN:?};\n\
         const BLACKBODY_ENTRIES: u32 = {BLACKBODY_ENTRIES};\n\
         const NOISE_LATTICE_CELLS: f32 = {NOISE_LATTICE_CELLS}.0;\n"
    )
}

/// The pass blends premultiplied colour over the frame with
/// `dst * (1 - a) + colour`, as the water's mist does.
const GAS_BLEND: GpuBlendState = GpuBlendState {
    src_factor: GpuBlendFactor::One,
    dst_factor: GpuBlendFactor::OneMinusSrcAlpha,
    operation: GpuBlendOp::Add,
    src_alpha_factor: GpuBlendFactor::One,
    dst_alpha_factor: GpuBlendFactor::OneMinusSrcAlpha,
    alpha_operation: GpuBlendOp::Add,
};

/// Bindings the march adds to the scene's: the object uniform at 0, then
/// 80 to 88 (`gas_volume.wgsl`).
const OWN_BINDINGS: usize = 10;

/// Threads along each side of a compute workgroup (`@workgroup_size(4, 4, 4)`).
const GROUP_SIDE: u32 = 4;

/// What a quality level costs and buys.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RenderPreset {
    /// March samples per gas cell along the ray.
    pub samples_per_cell: f32,
    /// Gas cells per light volume cell along each axis.
    pub light_divisor: u32,
    /// Samples of the density from a light volume cell toward the sun.
    pub light_steps: u32,
    pub detail_octaves: u32,
    pub scatter_octaves: u32,
    /// The sun's shadow map shades the smoke.
    pub scene_shadows: bool,
}

const PRESETS: [RenderPreset; 4] = [
    RenderPreset {
        samples_per_cell: 0.75,
        light_divisor: 2,
        light_steps: 16,
        detail_octaves: 1,
        scatter_octaves: 2,
        scene_shadows: false,
    },
    RenderPreset {
        samples_per_cell: 1.0,
        light_divisor: 2,
        light_steps: 24,
        detail_octaves: 2,
        scatter_octaves: 3,
        scene_shadows: false,
    },
    RenderPreset {
        samples_per_cell: 1.5,
        light_divisor: 1,
        light_steps: 48,
        detail_octaves: 3,
        scatter_octaves: 4,
        scene_shadows: true,
    },
    RenderPreset {
        samples_per_cell: 2.0,
        light_divisor: 1,
        light_steps: 64,
        detail_octaves: 4,
        scatter_octaves: 4,
        scene_shadows: true,
    },
];

/// The preset for a quality level: 0 Low, 1 Medium, 2 High, 3 Extra High.
/// Callers check the level against `PRESETS` first (`validate`).
fn render_preset(quality: u32) -> RenderPreset {
    PRESETS[quality as usize]
}

// ---- The detail noise ------------------------------------------------------

/// Texels along each side of the noise volume.
const NOISE_SIZE: usize = 64;
/// Lattice cells along each side. The gradient lattice wraps, so the volume
/// tiles every `NOISE_SIZE` texels.
const NOISE_LATTICE_CELLS: usize = 8;
const NOISE_SEED: u32 = 0x4741_5321;

/// PCG output hash of `v`.
fn hash(v: u32) -> u32 {
    let state = v.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let word = ((state >> ((state >> 28) + 4)) ^ state).wrapping_mul(277_803_737);
    (word >> 22) ^ word
}

/// A uniformly random unit vector for lattice point (x, y, z).
fn lattice_gradient(x: usize, y: usize, z: usize) -> [f32; 3] {
    let h = hash(x as u32 ^ hash(y as u32 ^ hash(z as u32 ^ NOISE_SEED)));
    let to_unit = |v: u32| v as f32 / 4_294_967_296.0;
    let cos_polar = 2.0 * to_unit(hash(h)) - 1.0;
    let azimuth = std::f32::consts::TAU * to_unit(hash(h ^ 0x68E3_1DA4));
    let sin_polar = (1.0 - cos_polar * cos_polar).sqrt();
    [
        sin_polar * azimuth.cos(),
        sin_polar * azimuth.sin(),
        cos_polar,
    ]
}

/// Quintic ease, so the noise has continuous first and second derivatives.
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// One octave of tileable gradient noise, `NOISE_SIZE`^3 bytes in order
/// x + n * (y + n * z), stretched so its largest value reaches 0 or 255 about
/// the middle 128.
fn detail_noise_volume() -> Vec<u8> {
    let lattice = NOISE_LATTICE_CELLS;
    let gradients: Vec<[f32; 3]> = (0..lattice * lattice * lattice)
        .map(|i| lattice_gradient(i % lattice, i / lattice % lattice, i / (lattice * lattice)))
        .collect();
    // Along each axis a texel sits in one lattice cell, at an offset into it
    // and an eased copy of that offset.
    let texels_per_cell = NOISE_SIZE / lattice;
    let axis: Vec<(usize, f32, f32)> = (0..NOISE_SIZE)
        .map(|i| {
            let offset = (i % texels_per_cell) as f32 / texels_per_cell as f32;
            (i / texels_per_cell, offset, fade(offset))
        })
        .collect();
    let mut raw = Vec::with_capacity(NOISE_SIZE * NOISE_SIZE * NOISE_SIZE);
    for &(cz, tz, fz) in &axis {
        for &(cy, ty, fy) in &axis {
            for &(cx, tx, fx) in &axis {
                let mut value = 0.0;
                for corner in 0..8usize {
                    let (bx, by, bz) = (corner & 1, (corner >> 1) & 1, (corner >> 2) & 1);
                    let g = gradients[(cx + bx) % lattice
                        + lattice * ((cy + by) % lattice + lattice * ((cz + bz) % lattice))];
                    let weight = (if bx == 1 { fx } else { 1.0 - fx })
                        * (if by == 1 { fy } else { 1.0 - fy })
                        * (if bz == 1 { fz } else { 1.0 - fz });
                    value += weight
                        * (g[0] * (tx - bx as f32)
                            + g[1] * (ty - by as f32)
                            + g[2] * (tz - bz as f32));
                }
                raw.push(value);
            }
        }
    }
    let reach = raw.iter().fold(f32::MIN_POSITIVE, |m, v| m.max(v.abs()));
    raw.iter()
        .map(|v| ((0.5 + 0.5 * v / reach) * 255.0).round() as u8)
        .collect()
}

// ---- Uniforms --------------------------------------------------------------

/// `GasUniforms` in `gas_volume.wgsl`, row for row.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GasUniforms {
    inv_view_proj: [[f32; 4]; 4],
    /// xyz box min corner, w cell size.
    box_min_cell: [f32; 4],
    /// xyz box max corner, w seconds.
    box_max_seconds: [f32; 4],
    /// rgb smoke colour, w extinction per metre at density 1.
    smoke_density: [f32; 4],
    /// x albedo, y detail, z flame brightness, w glow.
    look: [f32; 4],
    /// x kelvin at temperature 0, y at 1, z scatter octaves, w samples per cell.
    flame_kelvin: [f32; 4],
    /// xyz unit direction toward the sun, w its shadow caster slot (-1 none).
    sun_dir: [f32; 4],
    /// rgb premultiplied sun colour, w 1 when a sun lights the gas.
    sun_color: [f32; 4],
    /// x detail octaves, y 1 for scene shadows, z frame, w unused.
    counts: [u32; 4],
    /// x width, y height, zw 0.
    target_size: [f32; 4],
}

/// `GasLightUniforms` in `gas_light.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct GasLightUniforms {
    /// xyz light volume cells, w steps toward the sun.
    dims: [u32; 4],
    /// xyz box min corner, w extinction per metre at density 1.
    box_min: [f32; 4],
    /// xyz box max corner, w 1 when a sun lights the gas.
    box_max: [f32; 4],
    /// xyz unit direction toward the sun, w unused.
    sun: [f32; 4],
}

/// `GasUnpackUniforms` in `gas_light.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct GasUnpackUniforms {
    /// xyz cells, w unused.
    dims: [u32; 4],
}

/// The scene's sun as the gas sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sun {
    /// Unit direction toward the sun.
    direction: [f32; 3],
    /// Premultiplied colour: the irradiance on a surface facing the sun.
    color: [f32; 3],
    /// The sun's shadow caster slot, -1 for none.
    caster: f32,
}

fn resolve_sun(sun: Option<([f32; 3], [f32; 3], f32)>) -> Result<Option<Sun>, String> {
    let Some((direction, color, caster)) = sun else {
        return Ok(None);
    };
    let length = direction.iter().map(|c| c * c).sum::<f32>().sqrt();
    if !(length > 1e-6 && length.is_finite()) {
        return Err("the sun's direction has no length".into());
    }
    Ok(Some(Sun {
        direction: direction.map(|c| c / length),
        color,
        caster,
    }))
}

/// The box's far corner.
fn box_max(volume: &GasVolume) -> [f32; 3] {
    let size = volume.dims.map(|n| n as f32 * volume.cell_size);
    [0, 1, 2].map(|a| volume.origin[a] + size[a])
}

fn gas_uniforms(
    volume: &GasVolume,
    preset: &RenderPreset,
    view: &GasView,
    sun: Option<Sun>,
) -> GasUniforms {
    let look = &volume.look;
    let max = box_max(volume);
    let (sun_dir, sun_color) = match sun {
        Some(sun) => (
            [
                sun.direction[0],
                sun.direction[1],
                sun.direction[2],
                sun.caster,
            ],
            [sun.color[0], sun.color[1], sun.color[2], 1.0],
        ),
        None => ([0.0, 1.0, 0.0, -1.0], [0.0; 4]),
    };
    GasUniforms {
        inv_view_proj: view.inv_view_proj,
        box_min_cell: [
            volume.origin[0],
            volume.origin[1],
            volume.origin[2],
            volume.cell_size,
        ],
        box_max_seconds: [max[0], max[1], max[2], view.seconds],
        smoke_density: [
            look.smoke_color[0],
            look.smoke_color[1],
            look.smoke_color[2],
            look.density,
        ],
        look: [look.albedo, look.detail, look.flame_brightness, look.glow],
        flame_kelvin: [
            look.kelvin_cool,
            look.kelvin_hot,
            preset.scatter_octaves as f32,
            preset.samples_per_cell,
        ],
        sun_dir,
        sun_color,
        counts: [
            preset.detail_octaves,
            u32::from(preset.scene_shadows),
            view.frame,
            0,
        ],
        target_size: [view.width as f32, view.height as f32, 0.0, 0.0],
    }
}

/// Light volume cells for a gas of `dims` cells.
fn light_volume_dims(dims: [u32; 3], preset: &RenderPreset) -> [u32; 3] {
    dims.map(|n| n.div_ceil(preset.light_divisor).max(1))
}

fn light_uniforms(
    volume: &GasVolume,
    preset: &RenderPreset,
    light_cells: [u32; 3],
    sun: Option<Sun>,
) -> GasLightUniforms {
    let max = box_max(volume);
    let direction = sun.map_or([0.0; 3], |sun| sun.direction);
    GasLightUniforms {
        dims: [
            light_cells[0],
            light_cells[1],
            light_cells[2],
            preset.light_steps,
        ],
        box_min: [
            volume.origin[0],
            volume.origin[1],
            volume.origin[2],
            volume.look.density,
        ],
        box_max: [max[0], max[1], max[2], f32::from(u8::from(sun.is_some()))],
        sun: [direction[0], direction[1], direction[2], 0.0],
    }
}

/// The preset for a gas, once its box and look are known to be usable.
fn validate(volume: &GasVolume) -> Result<RenderPreset, String> {
    if volume.dims.contains(&0) {
        return Err(format!("a box of {:?} cells holds none", volume.dims));
    }
    if !(volume.cell_size > 0.0 && volume.cell_size.is_finite()) {
        return Err(format!(
            "the cell size {} is not a length above zero",
            volume.cell_size
        ));
    }
    if !(volume.look.density >= 0.0 && volume.look.density.is_finite()) {
        return Err(format!(
            "the density {} is not an extinction of zero or more",
            volume.look.density
        ));
    }
    if volume.look.quality as usize >= PRESETS.len() {
        return Err(format!(
            "quality {} is not 0 (Low) to 3 (Extra High)",
            volume.look.quality
        ));
    }
    Ok(render_preset(volume.look.quality))
}

// ---- Resources -------------------------------------------------------------

/// One gas to draw this frame.
pub(super) struct GasObject<'ctx> {
    /// The solver's packed state: two words a cell, half floats (density,
    /// temperature) then (fuel, flame).
    pub state: &'ctx GpuBuffer,
    /// The solver's packed velocity: two words a cell, half floats (u, v)
    /// then (w, 0).
    pub velocity: &'ctx GpuBuffer,
    pub volume: GasVolume,
    /// The object's `RenderSceneUniforms` bytes (view_proj, camera_pos,
    /// scene_params: x light count, z exposure EV).
    pub uniforms: &'ctx [u8],
}

/// What the camera and the frame give every gas.
pub(super) struct GasView {
    pub inv_view_proj: [[f32; 4]; 4],
    pub width: u32,
    pub height: u32,
    pub seconds: f32,
    pub frame: u32,
    /// The first Sun light: direction toward the light, premultiplied colour,
    /// caster slot (-1 none).
    pub sun: Option<([f32; 3], [f32; 3], f32)>,
}

struct Pipelines {
    unpack: GpuComputePipeline,
    light: GpuComputePipeline,
    noise: GpuComputePipeline,
}

/// What every gas shares, made when the first one draws.
struct Shared {
    /// Linear, clamped, mipmapped: the fields and the light volume.
    linear: GpuSampler,
    /// Linear, repeating: the detail noise.
    repeat: GpuSampler,
    /// `blackbody_table()` as `array<vec4<f32>>`.
    blackbody: GpuBuffer,
    noise: GpuTexture,
}

/// A gas's fields as textures: the state with its mip chain, and the velocity.
struct Fields {
    dims: [u32; 3],
    state: GpuTexture,
    velocity: GpuTexture,
}

struct LightVolume {
    dims: [u32; 3],
    texture: GpuTexture,
}

/// The textures one gas keeps between frames. Each is made again only when its
/// size changes.
#[derive(Default)]
struct GasSlot {
    fields: Option<Fields>,
    light: Option<LightVolume>,
}

#[derive(Default)]
pub(super) struct GasPass {
    pipeline: Option<(GpuRenderPipeline, GpuTextureFormat)>,
    /// Always passes and writes nothing: the pass draws at depth 0 and stops
    /// each ray at the opaque depth itself.
    depth_state: Option<GpuDepthStencilState>,
    compute: Option<Pipelines>,
    shared: Option<Shared>,
    /// One per gas index.
    slots: Vec<GasSlot>,
    /// (squared distance from the camera to the box centre, gas index).
    order: Vec<(f32, usize)>,
}

/// Mip levels of a 3D texture: a chain down to one texel.
fn mip_levels(dims: [u32; 3]) -> u32 {
    1 + dims[0].max(dims[1]).max(dims[2]).max(1).ilog2()
}

/// Bytes of a 3D texture of `dims` with `levels` mips of `bytes_per_texel`.
fn texture_bytes(dims: [u32; 3], levels: u32, bytes_per_texel: u32) -> u64 {
    (0..levels)
        .map(|level| {
            dims.iter()
                .map(|&n| u64::from((n >> level).max(1)))
                .product::<u64>()
        })
        .sum::<u64>()
        * u64::from(bytes_per_texel)
}

/// A 3D texture of `dims`, admitted against the device's memory first.
fn create_volume(
    device: &GpuDevice,
    dims: [u32; 3],
    levels: u32,
    format: GpuTextureFormat,
    usage: GpuTextureUsage,
    label: &str,
) -> Result<GpuTexture, String> {
    let bytes = texture_bytes(dims, levels, format.bytes_per_pixel());
    admit_candidate_bytes(device.modifier_memory_snapshot(), bytes)
        .map_err(|error| format!("{label}: {error}"))?;
    device.try_create_texture(&GpuTextureDesc {
        width: dims[0],
        height: dims[1],
        depth: dims[2],
        format,
        dimension: GpuTextureDimension::D3,
        usage,
        label,
        mip_levels: levels,
    })
}

fn groups(dims: [u32; 3]) -> [u32; 3] {
    dims.map(|n| n.div_ceil(GROUP_SIDE))
}

/// Where the object uniforms put the camera.
fn camera_position(uniforms: &[u8]) -> Result<[f32; 3], String> {
    let at = std::mem::offset_of!(RenderSceneUniforms, camera_pos);
    uniforms
        .get(at..at + 12)
        .map(bytemuck::pod_read_unaligned)
        .ok_or_else(|| {
            format!(
                "the object uniforms hold {} bytes, short of the camera",
                uniforms.len()
            )
        })
}

impl GasPass {
    fn build_pipeline(device: &GpuDevice, color_format: GpuTextureFormat) -> GpuRenderPipeline {
        device.create_render_pipeline(
            &RenderPipelineDesc::new(
                &GAS_VOLUME_WGSL,
                "vs_gas",
                "fs_gas",
                "node.render_scene gas",
            )
            .color(color_format, Some(GAS_BLEND))
            .depth(GpuTextureFormat::Depth32Float),
        )
    }

    /// Builds the draw pipeline into the device's shared cache ahead of the
    /// first draw.
    pub(super) fn prewarm(device: &GpuDevice, color_format: GpuTextureFormat) {
        Self::build_pipeline(device, color_format);
    }

    fn ensure_pipelines(&mut self, device: &GpuDevice, color_format: GpuTextureFormat) {
        if !matches!(&self.pipeline, Some((_, f)) if *f == color_format) {
            self.pipeline = Some((Self::build_pipeline(device, color_format), color_format));
        }
        if self.depth_state.is_none() {
            self.depth_state = Some(device.create_depth_stencil_state(&GpuDepthStencilDesc {
                compare: GpuCompareFunction::Always,
                write_enabled: false,
            }));
        }
        if self.compute.is_none() {
            let make = |entry: &str, label: &str| {
                device.create_compute_pipeline(GAS_LIGHT_WGSL, entry, label)
            };
            self.compute = Some(Pipelines {
                unpack: make("cs_gas_unpack", "node.render_scene gas unpack"),
                light: make("cs_gas_light", "node.render_scene gas light"),
                noise: make("cs_gas_noise", "node.render_scene gas noise"),
            });
        }
    }

    /// The samplers, the blackbody table and the noise volume, made on the
    /// first call. The noise goes up through a staging buffer and a compute
    /// pass, which the retirement queue keeps alive until the GPU is done.
    fn ensure_shared(&mut self, device: &GpuDevice, enc: &mut GpuEncoder) -> Result<(), String> {
        if self.shared.is_some() {
            return Ok(());
        }
        let pipelines = self.compute.as_ref().expect("pipelines are made first");

        let table = blackbody_table();
        if table.len() != BLACKBODY_ENTRIES {
            return Err(format!(
                "the blackbody table holds {} entries, the shader reads {BLACKBODY_ENTRIES}",
                table.len()
            ));
        }
        let table_bytes: &[u8] = bytemuck::cast_slice(&table);
        let table_size = table_bytes.len() as u64;
        admit_candidate_bytes(device.modifier_memory_snapshot(), table_size)
            .map_err(|error| format!("blackbody table: {error}"))?;
        let blackbody = device.try_create_buffer(table_size, BufferUse::Upload)?;
        // SAFETY: the buffer was just made at `table_size` bytes, and no GPU
        // work has read it yet.
        unsafe { blackbody.write(0, table_bytes) };

        let side = NOISE_SIZE as u32;
        let noise = create_volume(
            device,
            [side; 3],
            1,
            GpuTextureFormat::R8Unorm,
            GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
            "gas detail noise",
        )?;
        let bytes = detail_noise_volume();
        let staging_size = bytes.len() as u64;
        admit_candidate_bytes(device.modifier_memory_snapshot(), staging_size)
            .map_err(|error| format!("gas detail noise staging: {error}"))?;
        let staging = device.try_create_buffer(staging_size, BufferUse::Upload)?;
        // SAFETY: as above.
        unsafe { staging.write(0, &bytes) };
        enc.dispatch_compute(
            &pipelines.noise,
            &[
                GpuBinding::Buffer {
                    binding: 9,
                    buffer: &staging,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 10,
                    texture: &noise,
                },
            ],
            groups([side; 3]),
            "node.render_scene gas noise",
        );

        self.shared = Some(Shared {
            linear: device.create_sampler(&GpuSamplerDesc {
                mip_filter: GpuFilterMode::Linear,
                ..GpuSamplerDesc::default()
            }),
            repeat: device.create_sampler(&GpuSamplerDesc {
                address_mode_u: GpuAddressMode::Repeat,
                address_mode_v: GpuAddressMode::Repeat,
                address_mode_w: GpuAddressMode::Repeat,
                ..GpuSamplerDesc::default()
            }),
            blackbody,
            noise,
        });
        Ok(())
    }

    /// Unpacks gas `index`'s fields, builds their mips and marches the light
    /// volume, remaking a texture only when its size changed.
    fn prepare(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        index: usize,
        gas: &GasObject,
        preset: &RenderPreset,
        sun: Option<Sun>,
    ) -> Result<(), String> {
        let pipelines = self.compute.as_ref().expect("pipelines are made first");
        let shared = self
            .shared
            .as_ref()
            .expect("shared resources are made first");
        let slot = &mut self.slots[index];
        let dims = gas.volume.dims;

        if slot.fields.as_ref().is_none_or(|f| f.dims != dims) {
            // The old textures stay until the new ones are admitted.
            let state = create_volume(
                device,
                dims,
                mip_levels(dims),
                GpuTextureFormat::Rgba16Float,
                GpuTextureUsage::RENDER_TARGET_FULL,
                "gas state",
            )?;
            let velocity = create_volume(
                device,
                dims,
                1,
                GpuTextureFormat::Rgba16Float,
                GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
                "gas velocity",
            )?;
            slot.fields = Some(Fields {
                dims,
                state,
                velocity,
            });
        }
        let fields = slot.fields.as_ref().expect("made above");

        let light_cells = light_volume_dims(dims, preset);
        if slot.light.as_ref().is_none_or(|l| l.dims != light_cells) {
            let texture = create_volume(
                device,
                light_cells,
                1,
                GpuTextureFormat::Rgba16Float,
                GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
                "gas light volume",
            )?;
            slot.light = Some(LightVolume {
                dims: light_cells,
                texture,
            });
        }
        let light = slot.light.as_ref().expect("made above");

        let unpack = GasUnpackUniforms {
            dims: [dims[0], dims[1], dims[2], 0],
        };
        enc.dispatch_compute(
            &pipelines.unpack,
            &[
                GpuBinding::Bytes {
                    binding: 4,
                    data: bytemuck::bytes_of(&unpack),
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: gas.state,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: gas.velocity,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 7,
                    texture: &fields.state,
                },
                GpuBinding::Texture {
                    binding: 8,
                    texture: &fields.velocity,
                },
            ],
            groups(dims),
            "node.render_scene gas unpack",
        );
        enc.generate_mipmaps(&fields.state);

        let uniforms = light_uniforms(&gas.volume, preset, light_cells, sun);
        enc.dispatch_compute(
            &pipelines.light,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: &fields.state,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler: &shared.linear,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: &light.texture,
                },
            ],
            groups(light_cells),
            "node.render_scene gas light",
        );
        Ok(())
    }

    /// Draws gas `index` over `target`.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        enc: &mut GpuEncoder,
        target: &GpuTexture,
        depth_attachment: &GpuTexture,
        opaque_depth: &GpuTexture,
        scene: &[GpuBinding; SCENE_BINDINGS],
        view: &GasView,
        sun: Option<Sun>,
        index: usize,
        gas: &GasObject,
        preset: &RenderPreset,
    ) {
        let pipeline = &self.pipeline.as_ref().expect("pipeline is made first").0;
        let depth_state = self.depth_state.as_ref().expect("made with the pipeline");
        let shared = self
            .shared
            .as_ref()
            .expect("shared resources are made first");
        let slot = &self.slots[index];
        let fields = slot.fields.as_ref().expect("prepared first");
        let light = slot.light.as_ref().expect("prepared first");
        let uniforms = gas_uniforms(&gas.volume, preset, view, sun);
        let own = [
            GpuBinding::Bytes {
                binding: 0,
                data: gas.uniforms,
            },
            GpuBinding::Bytes {
                binding: 80,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Texture {
                binding: 81,
                texture: &fields.state,
            },
            GpuBinding::Texture {
                binding: 82,
                texture: &fields.velocity,
            },
            GpuBinding::Sampler {
                binding: 83,
                sampler: &shared.linear,
            },
            GpuBinding::Texture {
                binding: 84,
                texture: &light.texture,
            },
            GpuBinding::Buffer {
                binding: 85,
                buffer: &shared.blackbody,
                offset: 0,
            },
            GpuBinding::Texture {
                binding: 86,
                texture: opaque_depth,
            },
            GpuBinding::Texture {
                binding: 87,
                texture: &shared.noise,
            },
            GpuBinding::Sampler {
                binding: 88,
                sampler: &shared.repeat,
            },
        ];
        let mut bindings = [scene[0]; SCENE_BINDINGS + OWN_BINDINGS];
        bindings[..SCENE_BINDINGS].copy_from_slice(scene);
        bindings[SCENE_BINDINGS..].copy_from_slice(&own);
        // Two triangles over the box's screen rectangle.
        let draw = Draw::new(pipeline, &bindings, 6, 1);
        enc.draw(
            &DrawPass::color(target, GpuLoadAction::Load).depth(
                depth_attachment,
                depth_state,
                GpuLoadAction::Load,
                DepthStore::Store,
            ),
            &[draw],
            "node.render_scene gas",
        );
    }

    /// Draws every gas over `target`, farthest box first, each after its
    /// light volume. `depth_attachment` is the pass's depth, left as it is;
    /// each ray stops at `opaque_depth`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn encode(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        target: &GpuTexture,
        depth_attachment: &GpuTexture,
        opaque_depth: &GpuTexture,
        scene: &[GpuBinding; SCENE_BINDINGS],
        view: &GasView,
        gases: &[GasObject],
    ) -> Result<(), String> {
        if gases.is_empty() {
            return Ok(());
        }
        let sun = resolve_sun(view.sun)?;
        self.ensure_pipelines(device, target.format);
        self.ensure_shared(device, enc)?;
        self.slots.truncate(gases.len());
        self.slots.resize_with(gases.len(), GasSlot::default);
        self.order.clear();
        for (index, gas) in gases.iter().enumerate() {
            let preset = validate(&gas.volume).map_err(|error| format!("gas {index}: {error}"))?;
            self.prepare(device, enc, index, gas, &preset, sun)
                .map_err(|error| format!("gas {index}: {error}"))?;
            let camera = camera_position(gas.uniforms)?;
            let size = gas.volume.dims.map(|n| n as f32 * gas.volume.cell_size);
            let distance_squared: f32 = (0..3)
                .map(|a| {
                    let centre = gas.volume.origin[a] + 0.5 * size[a];
                    (centre - camera[a]) * (centre - camera[a])
                })
                .sum();
            self.order.push((distance_squared, index));
        }
        self.order.sort_by(|a, b| b.0.total_cmp(&a.0));
        for &(_, index) in &self.order {
            let gas = &gases[index];
            let preset = render_preset(gas.volume.look.quality);
            self.draw(
                enc,
                target,
                depth_attachment,
                opaque_depth,
                scene,
                view,
                sun,
                index,
                gas,
                &preset,
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::bindings::Slot;
    use manifold_node_engine::scene::scene_object::{GasLight, GasLook};

    fn validate_module(source: &str, what: &str) -> naga::Module {
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|e| panic!("{what} must naga-parse:\n{}", e.emit_to_string(source)));
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = validator.validate(&module) {
            panic!("{what} validation failed:\n{}", e.emit_to_string(source));
        }
        module
    }

    fn entry_points(module: &naga::Module) -> Vec<&str> {
        module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect()
    }

    #[test]
    fn the_march_shader_validates_with_its_entry_points() {
        let module = validate_module(&GAS_VOLUME_WGSL, "gas volume shader");
        let names = entry_points(&module);
        assert!(
            names.contains(&"vs_gas") && names.contains(&"fs_gas"),
            "{names:?}"
        );
    }

    #[test]
    fn the_compute_shader_validates_with_its_entry_points() {
        let module = validate_module(GAS_LIGHT_WGSL, "gas light shader");
        let names = entry_points(&module);
        for entry in ["cs_gas_unpack", "cs_gas_light", "cs_gas_noise"] {
            assert!(names.contains(&entry), "{entry} missing from {names:?}");
        }
    }

    /// The size of the uniform struct `name` in the shader's own layout.
    fn shader_struct_size(module: &naga::Module, name: &str) -> usize {
        module
            .types
            .iter()
            .find_map(|(_, ty)| match &ty.inner {
                naga::TypeInner::Struct { span, .. } if ty.name.as_deref() == Some(name) => {
                    Some(*span as usize)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{name} is not declared in its shader"))
    }

    #[test]
    fn uniform_structs_match_their_shader_rows() {
        let march = validate_module(&GAS_VOLUME_WGSL, "gas volume shader");
        let compute = validate_module(GAS_LIGHT_WGSL, "gas light shader");
        assert_eq!(std::mem::size_of::<GasUniforms>(), 208);
        assert_eq!(shader_struct_size(&march, "GasUniforms"), 208);
        assert_eq!(std::mem::size_of::<GasLightUniforms>(), 64);
        assert_eq!(shader_struct_size(&compute, "GasLightUniforms"), 64);
        assert_eq!(std::mem::size_of::<GasUnpackUniforms>(), 16);
        assert_eq!(shader_struct_size(&compute, "GasUnpackUniforms"), 16);
    }

    #[test]
    fn presets_follow_the_quality_table() {
        let row = |quality| {
            let p = render_preset(quality);
            (
                p.samples_per_cell,
                p.light_divisor,
                p.light_steps,
                p.detail_octaves,
                p.scatter_octaves,
                p.scene_shadows,
            )
        };
        assert_eq!(row(0), (0.75, 2, 16, 1, 2, false));
        assert_eq!(row(1), (1.0, 2, 24, 2, 3, false));
        assert_eq!(row(2), (1.5, 1, 48, 3, 4, true));
        assert_eq!(row(3), (2.0, 1, 64, 4, 4, true));
    }

    #[test]
    fn the_noise_is_centred_and_tiles() {
        let n = NOISE_SIZE;
        let noise = detail_noise_volume();
        assert_eq!(noise.len(), n * n * n);
        let mean = noise.iter().map(|&b| f32::from(b)).sum::<f32>() / noise.len() as f32 / 255.0;
        assert!((mean - 0.5).abs() < 0.05, "mean {mean}");
        let (lowest, highest) = (noise.iter().min(), noise.iter().max());
        assert!(
            lowest < Some(&64) && highest > Some(&192),
            "stretched over the range: {lowest:?} to {highest:?}"
        );
        // Along each axis, the step across the wrap is no bigger than the
        // biggest step between neighbours inside the volume.
        let at = |c: [usize; 3]| i32::from(noise[c[0] + n * (c[1] + n * c[2])]);
        for axis in 0..3 {
            let mut inside = 0;
            let mut wrap = 0;
            for a in 0..n {
                for b in 0..n {
                    let point = |i: usize| match axis {
                        0 => [i, a, b],
                        1 => [a, i, b],
                        _ => [a, b, i],
                    };
                    for i in 0..n - 1 {
                        inside = inside.max((at(point(i + 1)) - at(point(i))).abs());
                    }
                    wrap = wrap.max((at(point(0)) - at(point(n - 1))).abs());
                }
            }
            assert!(
                wrap <= inside,
                "axis {axis}: wrap step {wrap} against {inside}"
            );
        }
    }

    fn sample_volume() -> GasVolume {
        GasVolume {
            state: Slot(0),
            velocity: Slot(1),
            origin: [-1.5, 0.25, 2.0],
            cell_size: 0.0625,
            dims: [48, 96, 32],
            look: GasLook {
                smoke_color: [0.2, 0.25, 0.3],
                density: 3.0,
                albedo: 0.8,
                detail: 0.6,
                flame_brightness: 40.0,
                kelvin_cool: 1200.0,
                kelvin_hot: 2400.0,
                glow: 0.3,
                light_brightness: 1.0,
                quality: 2,
            },
            light: GasLight::default(),
        }
    }

    fn sample_view(sun: Option<([f32; 3], [f32; 3], f32)>) -> GasView {
        GasView {
            inv_view_proj: [[0.0; 4]; 4],
            width: 1280,
            height: 720,
            seconds: 12.5,
            frame: 301,
            sun,
        }
    }

    #[test]
    fn the_box_ends_a_whole_number_of_cells_from_its_origin() {
        let volume = sample_volume();
        let preset = render_preset(volume.look.quality);
        let sun = resolve_sun(Some(([0.0, 3.0, 4.0], [4.0, 3.5, 3.0], 1.0)))
            .expect("a sun with a direction");
        let u = gas_uniforms(&volume, &preset, &sample_view(None), sun);
        assert_eq!(u.box_min_cell, [-1.5, 0.25, 2.0, 0.0625]);
        assert_eq!(u.box_max_seconds, [-1.5 + 3.0, 0.25 + 6.0, 2.0 + 2.0, 12.5]);
        // The sun is stored as a unit direction, with its caster slot.
        assert_eq!(u.sun_dir, [0.0, 0.6, 0.8, 1.0]);
        assert_eq!(u.sun_color, [4.0, 3.5, 3.0, 1.0]);
        assert_eq!(u.counts, [3, 1, 301, 0]);
        assert_eq!(u.flame_kelvin, [1200.0, 2400.0, 4.0, 1.5]);
    }

    #[test]
    fn a_missing_sun_is_marked_off_and_a_zero_one_is_refused() {
        let volume = sample_volume();
        let preset = render_preset(volume.look.quality);
        let u = gas_uniforms(&volume, &preset, &sample_view(None), None);
        assert_eq!(u.sun_color[3], 0.0);
        assert_eq!(u.sun_dir[3], -1.0);
        assert!(resolve_sun(Some(([0.0; 3], [1.0; 3], -1.0))).is_err());
    }

    #[test]
    fn light_volumes_follow_the_preset_and_round_up() {
        let volume = sample_volume();
        let half = render_preset(1);
        assert_eq!(light_volume_dims([48, 95, 1], &half), [24, 48, 1]);
        assert_eq!(
            light_volume_dims(volume.dims, &render_preset(3)),
            volume.dims
        );
        let lit = light_uniforms(&volume, &half, light_volume_dims(volume.dims, &half), None);
        assert_eq!(lit.dims, [24, 48, 16, 24]);
        assert_eq!(lit.box_min[3], 3.0);
        assert_eq!(lit.box_max[3], 0.0);
    }

    #[test]
    fn mip_chains_reach_one_texel_and_count_their_bytes() {
        assert_eq!(mip_levels([48, 96, 32]), 7);
        assert_eq!(mip_levels([1, 1, 1]), 1);
        // 4x2x2 down to 1x1x1: 16 + 2 + 1 texels.
        assert_eq!(texture_bytes([4, 2, 2], 3, 8), (16 + 2 + 1) * 8);
    }

    #[test]
    fn a_gas_with_no_cells_or_a_missing_preset_is_refused() {
        let mut volume = sample_volume();
        assert!(validate(&volume).is_ok());
        volume.look.quality = 4;
        assert!(validate(&volume).is_err());
        volume = sample_volume();
        volume.dims[1] = 0;
        assert!(validate(&volume).is_err());
        volume = sample_volume();
        volume.cell_size = 0.0;
        assert!(validate(&volume).is_err());
    }
}

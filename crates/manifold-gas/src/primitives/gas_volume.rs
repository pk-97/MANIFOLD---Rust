//! `node.gas_volume` — the gas solver of Smoke and Fire: a dense, axis-aligned
//! box of cells advanced on the GPU one tick at a time (see
//! docs/SMOKE_AND_FIRE_MAP.md). The kernels are in `shaders/gas_step.wgsl`.
//!
//! It is the body of the gas's tick loop (`node.gas_ticks`): its domain
//! (`node.gas_domain`) follows the World's clock and lays the scene's solids,
//! forces and hits before the loop, and the step runs once a tick, after the
//! water's and the Matter's tick in the World's loop. The box floor is a wall;
//! the sides and top are open. The scene's Colliders and Bodies are solids the
//! air flows round and a moving body pushes. Water and sand are solids too:
//! a cell the water fills, read through the water's own tiles, or at least
//! half full of sand, read from the Matter's own grid as this tick left it,
//! moves with them,
//! so smoke rolls over water and a splash or a sand slide shoves it. The
//! fields stay on the GPU; the frame's last tick publishes the packed state
//! and velocity of every cell into the loop's published arrays and the
//! flames' light sums into the domain's ring.

use std::borrow::Cow;
use std::fmt::Display;
use std::sync::LazyLock;

use manifold_core::fluid_domain::DEFAULT_CELL_SIZE;
use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::{Primitive, PrimitiveSpec};
use manifold_node_engine::scene::blackbody::{
    BLACKBODY_ENTRIES, BLACKBODY_MAX_KELVIN, BLACKBODY_MIN_KELVIN, blackbody_table,
};
use manifold_node_engine::scene::transform::Transform;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, pool_len, read_side_as, table_words};
use manifold_physics::force_lattice::ForceLattice;
use manifold_water_liquid::bodies::{
    COLLIDER_BVH, LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape,
};
use manifold_water_liquid::fluid_particles::{
    FaceSample, MatterGridNode, SAND_BULK, SAND_BULK_BYTES, SAND_READ,
};
use manifold_water_liquid::grid::LIQUID_FACES;
use manifold_water_liquid::whitewater::WHITEWATER_COMMON;

/// Most cells along one axis of the box.
const MAX_AXIS_CELLS: u32 = 512;
/// Fewest cells along one axis; the stencils need room to look both ways.
const MIN_AXIS_CELLS: u32 = 4;
/// Most cells in the whole box.
const MAX_CELLS: u64 = 16_777_216;
/// A box this far (in cells) past a whole number of cells still counts as that
/// many, so matrix rounding in the box's Transform never flips its cell count
/// and restarts the gas.
const EDGE_TOLERANCE: f64 = 1e-3;
/// Gravity of a scene that wires none: Earth's, down.
const DEFAULT_GRAVITY: f32 = 9.81;
/// The temperature past which fuel burns.
const IGNITION: f32 = 0.2;
/// Threads per workgroup edge in `gas_step.wgsl`.
const GROUP_EDGE: u32 = 4;
/// Light sum buffers in the domain's ring: this frame's write, the one still
/// in flight and the one the CPU reads.
pub(crate) const RING: usize = 3;
/// Words of one light sum buffer (see `cs_publish`).
pub(crate) const LIGHT_WORDS: usize = 8;
pub(crate) const LIGHT_BYTES: u64 = LIGHT_WORDS as u64 * 4;
/// The light sums' fixed point: a sum adds `round(value * LIGHT_FIXED_SCALE)`
/// as an unsigned word. Positions are relative to the box origin, so every sum
/// is non-negative; a word wraps past `2^32 / LIGHT_FIXED_SCALE` = 262 144.
pub(crate) const LIGHT_FIXED_SCALE: f32 = 16384.0;
const BLACKBODY_BYTES: u64 = BLACKBODY_ENTRIES as u64 * 16;

/// The shader's constants, composed ahead of the kernels so the Rust values
/// stay the only copy. `solids` builds the kernels that see the scene's
/// solids.
fn shader_constants(solids: bool) -> String {
    format!(
        "const BLACKBODY_ENTRIES: u32 = {BLACKBODY_ENTRIES}u;\n\
         const BLACKBODY_MIN_KELVIN: f32 = {BLACKBODY_MIN_KELVIN:?};\n\
         const BLACKBODY_MAX_KELVIN: f32 = {BLACKBODY_MAX_KELVIN:?};\n\
         const LIGHT_FIXED_SCALE: f32 = {LIGHT_FIXED_SCALE:?};\n\
         const LIGHT_WORDS: u32 = {LIGHT_WORDS}u;\n\
         const FINE: u32 = {FINE}u;\n\
         const SOLIDS: bool = {solids};\n"
    )
}

/// Fine state cells per air cell along each axis. Smoke, heat, fuel and
/// flame live on the fine grid, carried by the air and burning there, so the
/// flow draws them sharper than the air's cells; only the flow and its
/// pressure, the costly part, stay coarse.
const FINE: u32 = 2;

/// The kernels without solids, then with them.
static SHADERS: LazyLock<[String; 2]> = LazyLock::new(|| {
    [false, true].map(|solids| {
        format!(
            "{}\n{LIQUID_POSE}\n{LIQUID_COLLIDER}\n{COLLIDER_BVH}\n{READ_SIDE}\n{}\n{SAND_BULK}\n{SAND_READ}\n{LIQUID_FACES}\n{WHITEWATER_COMMON}\n{}",
            shader_constants(solids),
            read_side_as("ms_"),
            include_str!("shaders/gas_step.wgsl")
        )
    })
});

macro_rules! gas_param {
    ($name:literal, $label:literal, $ty:ident, $default:expr, $min:expr, $max:expr) => {
        ParamDef {
            name: Cow::Borrowed($name),
            label: $label,
            ty: ParamType::$ty,
            default: ParamValue::Float($default),
            range: Some(($min, $max)),
            enum_values: &[],
        }
    };
}

// ---- Geometry ------------------------------------------------------------

/// The box in cells: its first corner on whole cells, and how many cells.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GasGeometry {
    pub cell: f32,
    pub origin: [f32; 3],
    pub dims: [u32; 3],
}

impl GasGeometry {
    /// The air's grid from the fine grid the domain publishes: `FINE` fine
    /// cells to an air cell along each axis, so both are exact.
    pub(crate) fn from_fine(cell_size: f32, origin: [f32; 3], fine_dims: [u32; 3]) -> Self {
        Self {
            cell: cell_size * FINE as f32,
            origin,
            dims: fine_dims.map(|d| d / FINE),
        }
    }

    pub(crate) fn cells(&self) -> u64 {
        self.dims.iter().map(|&d| u64::from(d)).product()
    }

    /// Nodes of the staggered velocity: one more than the cells along each
    /// axis, so every face of every cell has one.
    fn face_nodes(&self) -> u64 {
        self.dims.iter().map(|&d| u64::from(d) + 1).product()
    }

    /// The fine state grid's cells along each axis.
    pub(crate) fn fine_dims(&self) -> [u32; 3] {
        self.dims.map(|d| d * FINE)
    }

    fn fine_cells(&self) -> u64 {
        self.cells() * u64::from(FINE.pow(3))
    }

    /// The scalar outputs this geometry fixes, by name: the published
    /// fields' grid, which is the fine one.
    pub(crate) fn outputs(&self) -> [(&'static str, f32); 7] {
        let [x, y, z] = self.origin;
        let [nx, ny, nz] = self.fine_dims();
        [
            ("cell_size", self.cell / FINE as f32),
            ("origin_x", x),
            ("origin_y", y),
            ("origin_z", z),
            ("dims_x", nx as f32),
            ("dims_y", ny as f32),
            ("dims_z", nz as f32),
        ]
    }
}

/// The grid from the node's wires and params (`read` is `scalar_or_param`):
/// dims = ceil(size / h) per axis, at least [`MIN_AXIS_CELLS`], from an origin
/// on whole cells of h at or below the box's corner. The node and the extent
/// rule both call this. Refused by name without a box, with a bad cell size,
/// or past [`MAX_AXIS_CELLS`] on an axis or [`MAX_CELLS`] in all.
pub(crate) fn gas_geometry(
    read: impl Fn(&str, f32) -> f32,
    domain: Option<Transform>,
) -> Result<GasGeometry, String> {
    let cell = read("cell_metres", DEFAULT_CELL_SIZE);
    if !(cell.is_finite() && cell > 0.0) {
        return Err(format!(
            "Gas Volume: Cell Size must be above 0 and finite, got {cell}"
        ));
    }
    let domain = domain.ok_or("Gas Volume: wire a domain Transform, the box the gas fills")?;
    let world = domain.world_box();
    let (centre, size) = (world.pos, world.scale);
    if !centre.iter().all(|v| v.is_finite()) || !size.iter().all(|&s| s.is_finite() && s > 0.0) {
        return Err(format!(
            "Gas Volume: the domain box must be finite with every side above 0, got {size:?} m at {centre:?}"
        ));
    }
    let h = f64::from(cell);
    let wanted: [u64; 3] = size.map(|side| {
        (f64::from(side) / h - EDGE_TOLERANCE)
            .ceil()
            .max(f64::from(MIN_AXIS_CELLS)) as u64
    });
    let [nx, ny, nz] = wanted;
    if wanted.iter().any(|&n| n > u64::from(MAX_AXIS_CELLS)) {
        return Err(format!(
            "Gas Volume: a {} × {} × {} m box at Cell Size {cell} m needs {nx} × {ny} × {nz} cells, and an axis holds at most {MAX_AXIS_CELLS}; raise the World's Cell Size or lower Quality",
            size[0], size[1], size[2]
        ));
    }
    let cells = nx * ny * nz;
    if cells > MAX_CELLS {
        return Err(format!(
            "Gas Volume: a {} × {} × {} m box at Cell Size {cell} m needs {nx} × {ny} × {nz} = {cells} cells, at most {MAX_CELLS}; raise the World's Cell Size or lower Quality",
            size[0], size[1], size[2]
        ));
    }
    let origin = std::array::from_fn(|axis| {
        let min = f64::from(centre[axis]) - 0.5 * f64::from(size[axis]);
        ((min / h + EDGE_TOLERANCE).floor() * h) as f32
    });
    Ok(GasGeometry {
        cell,
        origin,
        dims: wanted.map(|n| n as u32),
    })
}

/// Bytes of one published field on a fine grid of `fine_dims`: two words a
/// fine cell.
pub(crate) fn fine_field_bytes(fine_dims: [u32; 3]) -> u64 {
    fine_dims.iter().map(|&d| u64::from(d)).product::<u64>() * 8
}

/// The force lattice over the box: a node every `FORCE_LATTICE_STRIDE` cells
/// from its first face to its last, as water and sand lay theirs.
pub(crate) fn force_lattice(geometry: &GasGeometry) -> ForceLattice {
    ForceLattice::covering(geometry.origin, geometry.cell, geometry.dims.map(|d| d + 1))
}

// ---- Settings ------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Quality {
    Low,
    Medium,
    High,
    ExtraHigh,
}

impl Quality {
    fn from_param(value: f32) -> Self {
        match value.round() as i32 {
            ..=0 => Self::Low,
            1 => Self::Medium,
            2 => Self::High,
            _ => Self::ExtraHigh,
        }
    }

    /// Red plus black pressure sweeps per step.
    fn pressure_pairs(self) -> u32 {
        match self {
            Self::Low => 20,
            Self::Medium => 30,
            Self::High => 50,
            Self::ExtraHigh => 80,
        }
    }
}

/// What the node's params and wires say, other than the box.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GasSettings {
    /// 0 Smoke, 1 Fire.
    kind: u32,
    quality: Quality,
    gravity: [f32; 3],
    seed: u32,
    source_size: f32,
    rate: f32,
    inflow: f32,
    source_heat: f32,
    buoyancy: f32,
    weight: f32,
    vorticity: f32,
    turbulence: f32,
    dissipation: f32,
    cooling: f32,
    burn_rate: f32,
    heat_release: f32,
    soot: f32,
    expansion: f32,
}

/// The default a Float param declares: the one copy of every default.
fn declared_default(name: &str) -> f32 {
    GasVolume::PARAMS
        .iter()
        .find(|param| param.name == name)
        .and_then(|param| match param.default {
            ParamValue::Float(value) => Some(value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("node.gas_volume declares no Float param {name}"))
}

/// The settings from `read` (`scalar_or_param`), refused by name when a value
/// is not a finite number or the source has no size.
fn gas_settings(read: impl Fn(&str, f32) -> f32) -> Result<GasSettings, String> {
    let param = |name: &str| read(name, declared_default(name));
    let settings = GasSettings {
        kind: u32::from(param("kind") >= 0.5),
        quality: Quality::from_param(param("quality")),
        gravity: [
            read("gravity_x", 0.0),
            read("gravity", -DEFAULT_GRAVITY),
            read("gravity_z", 0.0),
        ],
        seed: param("seed").round().clamp(0.0, 16_777_215.0) as u32,
        source_size: param("source_size"),
        rate: param("rate"),
        inflow: param("inflow"),
        source_heat: param("source_heat"),
        buoyancy: param("buoyancy"),
        weight: param("weight"),
        vorticity: param("vorticity"),
        turbulence: param("turbulence"),
        dissipation: param("dissipation"),
        cooling: param("cooling"),
        burn_rate: param("burn_rate"),
        heat_release: param("heat_release"),
        soot: param("soot"),
        expansion: param("expansion"),
    };
    let numbers = [
        ("gravity_x", settings.gravity[0]),
        ("gravity", settings.gravity[1]),
        ("gravity_z", settings.gravity[2]),
        ("source_size", settings.source_size),
        ("rate", settings.rate),
        ("inflow", settings.inflow),
        ("source_heat", settings.source_heat),
        ("buoyancy", settings.buoyancy),
        ("weight", settings.weight),
        ("vorticity", settings.vorticity),
        ("turbulence", settings.turbulence),
        ("dissipation", settings.dissipation),
        ("cooling", settings.cooling),
        ("burn_rate", settings.burn_rate),
        ("heat_release", settings.heat_release),
        ("soot", settings.soot),
        ("expansion", settings.expansion),
    ];
    if let Some((name, value)) = numbers.iter().find(|(_, value)| !value.is_finite()) {
        return Err(format!("Gas Volume: {name} must be finite, got {value}"));
    }
    if settings.source_size <= 0.0 {
        return Err(format!(
            "Gas Volume: Source Size must be above 0, got {}",
            settings.source_size
        ));
    }
    Ok(settings)
}

/// The direction smoke rises and the strength of gravity: up is -gravity,
/// straight up when there is none (a weightless gas still flows out of its
/// source, and nothing makes it rise).
fn up_and_gravity(gravity: [f32; 3]) -> ([f32; 3], f32) {
    let g = gravity.iter().map(|c| c * c).sum::<f32>().sqrt();
    if g < 1e-6 {
        ([0.0, 1.0, 0.0], 0.0)
    } else {
        (gravity.map(|c| -c / g), g)
    }
}

// ---- Uniforms ------------------------------------------------------------

/// Field order identical to the shader's `Params`; 16-byte rows.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct GasUniforms {
    dims: [u32; 3],
    cells: u32,
    origin: [f32; 3],
    h: f32,
    up: [f32; 3],
    g: f32,
    dt: f32,
    time: f32,
    kind: u32,
    pad: u32,
    source_size: f32,
    rate: f32,
    inflow: f32,
    source_heat: f32,
    buoyancy: f32,
    weight: f32,
    vorticity: f32,
    turbulence: f32,
    dissipation: f32,
    cooling: f32,
    burn_rate: f32,
    heat_release: f32,
    soot: f32,
    expansion: f32,
    ignition: f32,
    seed: u32,
    force_min: [f32; 3],
    force_spacing: f32,
    force_nodes: [u32; 3],
    forces_on: u32,
    force_base: u32,
    impulse_on: u32,
    impulse_base: u32,
    solid_base: u32,
    solid_count: u32,
    /// The water's tiles' pool (0 while no water reaches the gas), one over
    /// the Sand's cell (0 while no sand does) and the water's cell in metres.
    water_pool: u32,
    sand_inv_cell: f32,
    water_cell: f32,
}

impl GasUniforms {
    fn new(
        geometry: &GasGeometry,
        settings: &GasSettings,
        time: f32,
        lattice: ForceLattice,
        step: Tick,
        matter: &Matter<'_>,
    ) -> Self {
        let nodes = lattice.node_count() as u32;
        let (up, g) = up_and_gravity(settings.gravity);
        Self {
            dims: geometry.dims,
            cells: geometry.cells() as u32,
            origin: geometry.origin,
            h: geometry.cell,
            up,
            g,
            dt: step.dt,
            time,
            kind: settings.kind,
            pad: 0,
            source_size: settings.source_size,
            rate: settings.rate,
            inflow: settings.inflow,
            source_heat: settings.source_heat,
            buoyancy: settings.buoyancy,
            weight: settings.weight,
            vorticity: settings.vorticity,
            turbulence: settings.turbulence,
            dissipation: settings.dissipation,
            cooling: settings.cooling,
            burn_rate: settings.burn_rate,
            heat_release: settings.heat_release,
            soot: settings.soot,
            expansion: settings.expansion,
            ignition: IGNITION,
            seed: settings.seed,
            force_min: lattice.min(),
            force_spacing: lattice.spacing(),
            force_nodes: lattice.nodes(),
            forces_on: u32::from(step.forces.is_some()),
            force_base: step.forces.unwrap_or(0) * nodes,
            impulse_on: u32::from(step.impulses.is_some()),
            impulse_base: step.impulses.unwrap_or(0) * nodes,
            solid_base: step.solids.map_or(0, |(base, _)| base),
            solid_count: step.solids.map_or(0, |(_, count)| count),
            water_pool: matter.water.map_or(0, |water| water.pool_tiles),
            sand_inv_cell: matter.sand.map_or(0.0, |sand| sand.inv_cell),
            water_cell: matter.water.map_or(0.0, |water| water.cell),
        }
    }
}

// ---- Storage -------------------------------------------------------------

/// Bytes of each kind of buffer for a box of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GasBytes {
    /// A vec4 per cell: the averaged state and the curl.
    vec4: u64,
    /// A vec4 per face node: a velocity buffer (three of them).
    faces: u64,
    /// An f32 per cell: pressure and divergence.
    scalar: u64,
    /// Two words per fine cell: a fine state buffer (three of them).
    fine: u64,
}

impl GasBytes {
    pub(crate) fn of(geometry: &GasGeometry) -> Self {
        let cells = geometry.cells();
        Self {
            vec4: cells * 16,
            faces: geometry.face_nodes() * 16,
            scalar: cells * 4,
            fine: geometry.fine_cells() * 8,
        }
    }

    /// Everything the step holds: the rotating fields, the averaged state,
    /// the curl, pressure and divergence, and the blackbody table. The
    /// published fields are the tick loop's, the light ring the domain's.
    pub(crate) fn total(&self) -> u64 {
        2 * self.vec4 + 3 * self.faces + 2 * self.scalar + 3 * self.fine + BLACKBODY_BYTES
    }
}

fn refusal(geometry: &GasGeometry, bytes: u64, error: impl Display) -> String {
    let [nx, ny, nz] = geometry.dims;
    format!(
        "Gas Volume: {nx} × {ny} × {nz} = {} cells need {bytes} bytes the device cannot give: {error}; raise the World's Cell Size or lower Quality",
        geometry.cells()
    )
}

/// Every GPU buffer of one box of cells.
pub struct GasBuffers {
    dims: [u32; 3],
    /// Last tick's result, a step's scratch, and the other: roles rotate.
    velocity: [GpuBuffer; RING],
    fine: [GpuBuffer; RING],
    /// The fine state averaged onto the air's cells.
    state: GpuBuffer,
    curl: GpuBuffer,
    /// Kept across ticks: the solve starts from the last tick's pressure.
    pressure: GpuBuffer,
    divergence: GpuBuffer,
    blackbody: GpuBuffer,
    /// What the force and solid bindings hold while nothing reaches them.
    no_forces: GpuBuffer,
    /// Two words a cell: the solid there, while the scene has solids.
    solid: Option<GpuBuffer>,
    /// The domain's still-solids epoch `solid` holds.
    static_written: Option<f32>,
    /// One word: where the source sits on the static solids.
    source_floor: GpuBuffer,
}

impl GasBuffers {
    /// Allocate the box's buffers, admitted against the World's memory budget
    /// as a whole first. The contents are not zeroed.
    fn allocate(device: &GpuDevice, geometry: &GasGeometry) -> Result<Self, String> {
        let bytes = GasBytes::of(geometry);
        let total = bytes.total();
        manifold_node_engine::load::expand::admit_world_bytes(
            device.modifier_memory_snapshot(),
            total,
        )
        .map_err(|error| refusal(geometry, total, error))?;
        let make = |size: u64, usage: BufferUse| {
            device
                .try_create_buffer(size, usage)
                .map_err(|error| refusal(geometry, total, error))
        };
        let blackbody = make(BLACKBODY_BYTES, BufferUse::Upload)?;
        // SAFETY: the buffer is shared, holds exactly the table, and no GPU
        // work has used it yet.
        unsafe { blackbody.write(0, bytemuck::cast_slice(&blackbody_table())) };
        Ok(Self {
            dims: geometry.dims,
            velocity: [
                make(bytes.faces, BufferUse::Gpu)?,
                make(bytes.faces, BufferUse::Gpu)?,
                make(bytes.faces, BufferUse::Gpu)?,
            ],
            fine: [
                make(bytes.fine, BufferUse::Gpu)?,
                make(bytes.fine, BufferUse::Gpu)?,
                make(bytes.fine, BufferUse::Gpu)?,
            ],
            state: make(bytes.vec4, BufferUse::Gpu)?,
            curl: make(bytes.vec4, BufferUse::Gpu)?,
            pressure: make(bytes.scalar, BufferUse::Gpu)?,
            divergence: make(bytes.scalar, BufferUse::Gpu)?,
            blackbody,
            no_forces: make(32, BufferUse::Gpu)?,
            solid: None,
            static_written: None,
            source_floor: make(16, BufferUse::Gpu)?,
        })
    }

    /// Keep the solid field while the scene has solids, admitted against the
    /// World's budget; drop it when it has none.
    fn keep_solid(
        &mut self,
        device: &GpuDevice,
        geometry: &GasGeometry,
        wanted: bool,
    ) -> Result<(), String> {
        if !wanted {
            self.solid = None;
            self.static_written = None;
            return Ok(());
        }
        if self.solid.is_none() {
            let bytes = geometry.cells() * 8;
            manifold_node_engine::load::expand::admit_world_bytes(
                device.modifier_memory_snapshot(),
                bytes,
            )
            .map_err(|error| refusal(geometry, bytes, error))?;
            self.solid = Some(
                device
                    .try_create_buffer(bytes, BufferUse::Gpu)
                    .map_err(|error| refusal(geometry, bytes, error))?,
            );
            self.static_written = None;
        }
        Ok(())
    }
}

/// One compute pipeline per kernel of `gas_step.wgsl`.
pub struct Pipelines {
    advect: GpuComputePipeline,
    advect_back: GpuComputePipeline,
    maccormack: GpuComputePipeline,
    centres_in: GpuComputePipeline,
    centres_a: GpuComputePipeline,
    fine_advect: GpuComputePipeline,
    fine_advect_back: GpuComputePipeline,
    fine_maccormack: GpuComputePipeline,
    fine_sources: GpuComputePipeline,
    restrict: GpuComputePipeline,
    forces: GpuComputePipeline,
    curl: GpuComputePipeline,
    confine: GpuComputePipeline,
    divergence: GpuComputePipeline,
    pressure_red: GpuComputePipeline,
    pressure_black: GpuComputePipeline,
    project: GpuComputePipeline,
    clear_sums: GpuComputePipeline,
    publish: GpuComputePipeline,
    publish_velocity: GpuComputePipeline,
    clear: GpuComputePipeline,
    /// The kernels that write the solid field, built with the solids only.
    solids_static: Option<GpuComputePipeline>,
    solids: Option<GpuComputePipeline>,
    fill_solids: Option<GpuComputePipeline>,
}

impl Pipelines {
    fn build(device: &GpuDevice, solids: bool) -> Self {
        let source = SHADERS[usize::from(solids)].as_str();
        let make = |entry: &str| device.create_compute_pipeline(source, entry, "node.gas_volume");
        Self {
            advect: make("cs_advect"),
            advect_back: make("cs_advect_back"),
            maccormack: make("cs_maccormack"),
            centres_in: make("cs_centres_in"),
            centres_a: make("cs_centres_a"),
            fine_advect: make("cs_fine_advect"),
            fine_advect_back: make("cs_fine_advect_back"),
            fine_maccormack: make("cs_fine_maccormack"),
            fine_sources: make("cs_fine_sources"),
            restrict: make("cs_restrict"),
            forces: make("cs_forces"),
            curl: make("cs_curl"),
            confine: make("cs_confine"),
            divergence: make("cs_divergence"),
            pressure_red: make("cs_pressure_red"),
            pressure_black: make("cs_pressure_black"),
            project: make("cs_project"),
            clear_sums: make("cs_clear_sums"),
            publish: make("cs_publish"),
            publish_velocity: make("cs_publish_velocity"),
            clear: make("cs_clear"),
            solids_static: solids.then(|| make("cs_solids_static")),
            solids: solids.then(|| make("cs_solids")),
            fill_solids: solids.then(|| make("cs_fill_solids")),
        }
    }
}

/// Which of the three velocity and fine state buffers play which role in a
/// dispatch (`vel_in`/`fine_in`, `vel_a`/`fine_a`, `vel_b`/`fine_b`).
#[derive(Clone, Copy)]
struct Roles {
    input: usize,
    a: usize,
    b: usize,
}

impl Roles {
    /// Read `input`, work in `a`, and keep the third as scratch.
    fn new(input: usize, a: usize) -> Self {
        let b = (0..RING)
            .find(|&i| i != input && i != a)
            .expect("a ring of three has a buffer besides two");
        Self { input, a, b }
    }
}

/// What the step reads and writes that it does not own: the domain's force
/// and impulse lattices, moving bits and light sums, and the tick loop's
/// published fields.
#[derive(Clone, Copy)]
struct Shared<'a> {
    lattices: [&'a GpuBuffer; 2],
    moving: &'a GpuBuffer,
    light: &'a GpuBuffer,
    state: &'a GpuBuffer,
    velocity: &'a GpuBuffer,
}

/// Every binding of `gas_step.wgsl`. A kernel binds all of them and uses its
/// own; the encoder skips a binding the kernel does not declare.
fn binding_list<'a>(
    uniforms: &'a [u8],
    buffers: &'a GasBuffers,
    roles: Roles,
    shared: Shared<'a>,
    solids: &SolidBuffers<'a>,
) -> [GpuBinding<'a>; 31] {
    let bind = |binding: u32, buffer: &'a GpuBuffer| GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    };
    [
        GpuBinding::Bytes {
            binding: 0,
            data: uniforms,
        },
        bind(1, &buffers.velocity[roles.input]),
        bind(2, &buffers.fine[roles.input]),
        bind(3, &buffers.velocity[roles.a]),
        bind(4, &buffers.state),
        bind(5, &buffers.velocity[roles.b]),
        bind(6, &buffers.fine[roles.a]),
        bind(7, &buffers.curl),
        bind(8, &buffers.pressure),
        bind(9, &buffers.divergence),
        bind(10, &buffers.fine[roles.b]),
        bind(11, shared.velocity),
        bind(12, shared.light),
        bind(13, &buffers.blackbody),
        bind(14, shared.lattices[0]),
        bind(15, shared.lattices[1]),
        bind(16, solids.bodies),
        bind(17, solids.shapes),
        bind(18, solids.atlas),
        bind(19, solids.nodes),
        bind(20, solids.triangles),
        bind(21, solids.cells),
        bind(22, &buffers.source_floor),
        bind(23, solids.sand_grid),
        bind(24, solids.water_faces),
        bind(25, solids.sand_bulk),
        bind(26, shared.moving),
        bind(27, shared.state),
        bind(28, solids.water_distance),
        bind(29, solids.water_tiles),
        bind(30, solids.sand_tiles),
    ]
}

/// What the solid bindings hold: the scene's bodies, shapes, lattices and
/// trees, the solid field, the water's tiles and the Sand's grid, or the
/// placeholder for each the scene has none of.
struct SolidBuffers<'a> {
    bodies: &'a GpuBuffer,
    shapes: &'a GpuBuffer,
    atlas: &'a GpuBuffer,
    nodes: &'a GpuBuffer,
    triangles: &'a GpuBuffer,
    cells: &'a GpuBuffer,
    water_faces: &'a GpuBuffer,
    water_distance: &'a GpuBuffer,
    water_tiles: &'a GpuBuffer,
    sand_grid: &'a GpuBuffer,
    sand_bulk: &'a GpuBuffer,
    sand_tiles: &'a GpuBuffer,
}

/// The domain's bodies this frame: rows, shapes, lattices and trees.
#[derive(Clone, Copy, Default)]
struct Bodies<'a> {
    rows: Option<&'a GpuBuffer>,
    shapes: Option<&'a GpuBuffer>,
    atlas: Option<&'a GpuBuffer>,
    nodes: Option<&'a GpuBuffer>,
    triangles: Option<&'a GpuBuffer>,
}

impl<'a> SolidBuffers<'a> {
    fn of(
        buffers: &'a GasBuffers,
        bodies: Bodies<'a>,
        matter: &Matter<'a>,
        with_bodies: bool,
    ) -> Self {
        let none = &buffers.no_forces;
        let rows = bodies.rows.filter(|_| with_bodies);
        let body = |buffer: Option<&'a GpuBuffer>| rows.and(buffer).unwrap_or(none);
        Self {
            bodies: rows.unwrap_or(none),
            shapes: body(bodies.shapes),
            atlas: body(bodies.atlas),
            nodes: body(bodies.nodes),
            triangles: body(bodies.triangles),
            cells: buffers.solid.as_ref().unwrap_or(none),
            water_faces: matter.water.map_or(none, |water| water.faces),
            water_distance: matter.water.map_or(none, |water| water.distance),
            water_tiles: matter.water.map_or(none, |water| water.tiles),
            sand_grid: matter.sand.map_or(none, |sand| sand.grid),
            sand_bulk: matter.sand.map_or(none, |sand| sand.bulk),
            sand_tiles: matter.sand.map_or(none, |sand| sand.tiles),
        }
    }
}

/// The first Water's step as this tick left it: its pooled faces and
/// distance on its world tiles, the pool and its cell in metres.
#[derive(Clone, Copy)]
struct WaterTiles<'a> {
    faces: &'a GpuBuffer,
    distance: &'a GpuBuffer,
    tiles: &'a GpuBuffer,
    pool_tiles: u32,
    cell: f32,
}

/// The first Sand's grid as its tick ended (sand_read.wgsl):
/// node.matter_grid_update's nodes, node.matter_to_grid's bulk, its table and
/// one over its cell in metres.
#[derive(Clone, Copy)]
struct SandGrid<'a> {
    grid: &'a GpuBuffer,
    bulk: &'a GpuBuffer,
    tiles: &'a GpuBuffer,
    inv_cell: f32,
}

/// The water and sand that reach the gas this tick: the first Water's
/// tiles, and the first Sand's grid; None for one not wired, empty or not
/// yet stepped.
#[derive(Clone, Copy, Default)]
struct Matter<'a> {
    water: Option<WaterTiles<'a>>,
    sand: Option<SandGrid<'a>>,
}

impl<'a> Matter<'a> {
    fn read(ctx: &EffectNodeContext<'a, '_>) -> Self {
        let water = (|| {
            let pool = ctx.scalar_or_param("pool_tiles", 0.0).round();
            let cell = ctx.scalar_or_param("water_cell", 0.0);
            // Before the water's first tick its storage holds no pool.
            if !((1.0..=16_777_216.0).contains(&pool) && cell > 0.0 && cell.is_finite()) {
                return None;
            }
            let pool_tiles = pool as u32;
            let elements = pool_len(pool_tiles);
            let (faces, distance, tiles) = (
                ctx.inputs.array("faces")?,
                ctx.inputs.array("distance")?,
                ctx.inputs.array("tiles")?,
            );
            (faces.size >= elements * size_of::<FaceSample>() as u64
                && distance.size >= elements * 4
                && tiles.size >= table_words(pool_tiles) * 4)
                .then_some(WaterTiles {
                    faces,
                    distance,
                    tiles,
                    pool_tiles,
                    cell,
                })
        })();
        let sand = (|| {
            let pool = ctx.scalar_or_param("sand_pool_tiles", 0.0).round();
            let cell = ctx.scalar_or_param("sand_cell", 0.0);
            // Before the Sand's first tick it has laid no bulk.
            if !((1.0..=16_777_216.0).contains(&pool) && cell > 0.0 && cell.is_finite()) {
                return None;
            }
            let pool_tiles = pool as u32;
            let nodes = pool_len(pool_tiles);
            let (grid, bulk, tiles) = (
                ctx.inputs.array("sand_grid")?,
                ctx.inputs.array("sand_bulk")?,
                ctx.inputs.array("sand_tiles")?,
            );
            (grid.size >= nodes * size_of::<MatterGridNode>() as u64
                && bulk.size >= nodes * SAND_BULK_BYTES
                && tiles.size >= table_words(pool_tiles) * 4)
                .then_some(SandGrid {
                    grid,
                    bulk,
                    tiles,
                    inv_cell: 1.0 / cell,
                })
        })();
        Self { water, sand }
    }

    fn any(&self) -> bool {
        self.water.is_some() || self.sand.is_some()
    }
}

/// One dispatch over the whole box, then a barrier for the next one.
fn pass(
    enc: &mut manifold_gpu::GpuEncoder,
    pipeline: &GpuComputePipeline,
    bindings: &[GpuBinding<'_>],
    groups: [u32; 3],
    label: &str,
) {
    enc.dispatch_compute(pipeline, bindings, groups, label);
    enc.compute_memory_barrier_buffers();
}

// ---- Light ---------------------------------------------------------------

/// The fire's light as the CPU reads it back: the flames' emission sum, its
/// emission-weighted centre in the world and temperature.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LightReadout {
    pub(crate) power: f32,
    pub(crate) position: [f32; 3],
    pub(crate) temperature: f32,
}

impl LightReadout {
    pub(crate) fn outputs(&self) -> [(&'static str, f32); 5] {
        [
            ("light_power", self.power),
            ("light_x", self.position[0]),
            ("light_y", self.position[1]),
            ("light_z", self.position[2]),
            ("light_temperature", self.temperature),
        ]
    }
}

// ---- Node ----------------------------------------------------------------

/// The tick the domain published for this iteration: its simulated seconds,
/// its place in the frame, the force and impulse lattice it reads and its
/// body rows (`count` from row `base`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Tick {
    dt: f32,
    index: u32,
    last: bool,
    forces: Option<u32>,
    impulses: Option<u32>,
    solids: Option<(u32, u32)>,
}

impl Tick {
    fn read(read: impl Fn(&str, f32) -> f32) -> Self {
        let whole = |name: &str| read(name, 0.0).round().max(0.0) as u32;
        let lattice = |name: &str| {
            let at = read(name, -1.0);
            (at >= 0.0).then(|| at.round() as u32)
        };
        let count = whole("tick_solid_count");
        Self {
            dt: read("tick_dt", 0.0).max(0.0),
            index: whole("tick_index"),
            last: read("tick_last", 0.0) > 0.5,
            forces: lattice("tick_forces"),
            impulses: lattice("tick_impulses"),
            solids: (count > 0).then(|| (whole("tick_solid_base"), count)),
        }
    }
}

manifold_node_engine::primitive! {
    name: GasVolume,
    type_id: "node.gas_volume",
    purpose: "Step smoke or fire one tick in an axis-aligned box of cells: the body of the gas's tick loop (node.gas_ticks), once a tick of node.gas_domain's clock. The box is the domain's fine grid (cell_size, origin_x, origin_y, origin_z, dims_x, dims_y, dims_z; the air's cells are twice as coarse). The floor is a wall; the sides and top are open and fade what reaches them. A source at the floor's centre adds smoke density (Kind Smoke) or fuel that ignites past the ignition temperature (Kind Fire); hot gas rises against gravity, swirls, and expands where it burns. Each tick steps tick_dt seconds and reads the domain's force lattice tick_forces and impulse lattice tick_impulses (-1 for none) and its body rows (tick_solid_count from tick_solid_base): the scene's Colliders and Bodies, solids the air flows round and a moving body pushes. Water and sand are solids too: a cell the first Water's step fills at least half (its faces and distance read through its tiles, pool_tiles, at water_cell) moves with the water, and a cell at least half full of the first Sand's bulk (read from its grid through its table: sand_grid, sand_bulk, sand_tiles, sand_pool_tiles, sand_cell) with the sand. restarted clears the fields on the frame's first tick; so does a change of the box's cell counts. On the frame's last tick (tick_last) it writes the published state (two words a fine cell: density and temperature, then fuel and flame, each pair as two halves) and velocity (u and v, then w and 0, as halves) in place into state and velocity, the loop's arrays, and the flames' light sums into light.",
    inputs: {
        cell_size: ScalarF32 optional,
        origin_x: ScalarF32 optional, origin_y: ScalarF32 optional, origin_z: ScalarF32 optional,
        dims_x: ScalarF32 optional, dims_y: ScalarF32 optional, dims_z: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        tick_dt: ScalarF32 optional,
        tick_index: ScalarF32 optional,
        tick_last: ScalarF32 optional,
        tick_forces: ScalarF32 optional,
        tick_impulses: ScalarF32 optional,
        tick_solid_base: ScalarF32 optional,
        tick_solid_count: ScalarF32 optional,
        restarted: ScalarF32 optional,
        solids_move: ScalarF32 optional,
        static_epoch: ScalarF32 optional,
        forces: Array(f32) optional,
        impulses: Array(f32) optional,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        collider_nodes: Array(manifold_node_engine::mesh::Vec4Vertex) optional,
        collider_triangles: Array(manifold_node_engine::mesh::Vec4Vertex) optional,
        moving: Array(u32) optional,
        light: Array(u32) optional,
        state: Array(u32) optional,
        velocity: Array(u32) optional,
        faces: Array(FaceSample) optional,
        distance: Array(f32) optional,
        tiles: Array(u32) optional,
        pool_tiles: ScalarF32 optional,
        water_cell: ScalarF32 optional,
        sand_grid: Array(MatterGridNode) optional,
        sand_bulk: Array(u32) optional,
        sand_tiles: Array(u32) optional,
        sand_pool_tiles: ScalarF32 optional,
        sand_cell: ScalarF32 optional,
    },
    outputs: {
        state_out: Array(u32),
        velocity_out: Array(u32),
    },
    params: [
        gas_param!("kind", "Kind", Int, 0.0, 0.0, 1.0),
        gas_param!("quality", "Quality", Int, 2.0, 0.0, 3.0),
        gas_param!("source_size", "Source Size (m)", Float, 0.6, 0.02, 20.0),
        gas_param!("rate", "Rate (per second)", Float, 4.0, 0.0, 100.0),
        gas_param!("inflow", "Inflow Speed (m/s)", Float, 1.0, 0.0, 30.0),
        gas_param!("source_heat", "Source Heat", Float, 0.6, 0.0, 2.0),
        gas_param!("buoyancy", "Buoyancy (m/s²)", Float, 2.0, 0.0, 50.0),
        gas_param!("weight", "Weight (m/s²)", Float, 0.2, 0.0, 20.0),
        gas_param!("vorticity", "Swirl", Float, 0.0, 0.0, 20.0),
        gas_param!("turbulence", "Turbulence (m/s²)", Float, 0.0, 0.0, 30.0),
        gas_param!("dissipation", "Smoke Fade (1/s)", Float, 0.15, 0.0, 10.0),
        gas_param!("cooling", "Cooling (1/s)", Float, 0.6, 0.0, 20.0),
        gas_param!("burn_rate", "Burn Rate (1/s)", Float, 6.0, 0.0, 50.0),
        gas_param!("heat_release", "Heat Release", Float, 2.0, 0.0, 10.0),
        gas_param!("soot", "Smoke from Fire", Float, 0.6, 0.0, 5.0),
        gas_param!("expansion", "Expansion (1/s)", Float, 1.5, 0.0, 10.0),
        gas_param!("seed", "Seed", Int, 0.0, 0.0, 16_777_215.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The body of node.gas_ticks' region: state and velocity from node.gas_ticks, state_out and velocity_out back into its state_in and velocity_in. The fine grid, every tick_* scalar, restarted, solids_move, static_epoch, the lattices, body rows, shapes, trees, moving and light from node.gas_domain; gravity from the World. faces, distance, tiles and pool_tiles from the first Water's node.gpu_flip_step and water_cell its cell, and the sand_* inputs from the first Sand's node.matter_grid_update grid and node.matter_to_grid bulk, tiles, pool and cell: both step before the gas in each tick of the World's loop. The defaults are Smoke's; Fire sets Kind to 1 and its own rates.",
    examples: [],
    picker: { label: "Gas Volume", category: Atom },
    summary: "Steps smoke or fire in a box of cells, one tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["smoke", "fire", "gas", "flame", "volume simulation"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        buffers: Option<GasBuffers> = None,
        // Without solids, then with them.
        pipelines: Option<[Pipelines; 2]> = None,
        // Which of the three velocity and state buffers holds the last result.
        current: usize = 0,
        // Seconds stepped since the last clear: the source's noise clock.
        sim_time: f64 = 0.0,
    },
}

/// What one tick reads from its domain and loop besides the scalars.
struct Frame<'a> {
    geometry: GasGeometry,
    settings: GasSettings,
    tick: Tick,
    restart: bool,
    bodies_move: bool,
    static_epoch: f32,
    matter: Matter<'a>,
    bodies: Bodies<'a>,
    lattices: [Option<&'a GpuBuffer>; 2],
    moving: &'a GpuBuffer,
    light: &'a GpuBuffer,
    state: &'a GpuBuffer,
    velocity: &'a GpuBuffer,
}

impl<'a> Frame<'a> {
    /// None while the domain refused its box: no tick runs then.
    fn read(ctx: &EffectNodeContext<'a, '_>) -> Result<Option<Self>, String> {
        let read = |name: &str, default: f32| ctx.scalar_or_param(name, default);
        let whole = |name: &str| read(name, 0.0).round().max(0.0) as u32;
        let geometry = GasGeometry::from_fine(
            read("cell_size", 0.0),
            [
                read("origin_x", 0.0),
                read("origin_y", 0.0),
                read("origin_z", 0.0),
            ],
            [whole("dims_x"), whole("dims_y"), whole("dims_z")],
        );
        if geometry.cells() == 0 || !(geometry.cell > 0.0 && geometry.cell.is_finite()) {
            return Ok(None);
        }
        let settings = gas_settings(read)?;
        let tick = Tick::read(read);
        let array = |name: &str| ctx.inputs.array(name);
        let (Some(moving), Some(light), Some(state), Some(velocity)) = (
            array("moving"),
            array("light"),
            array("state"),
            array("velocity"),
        ) else {
            return Err("Gas Volume: wire node.gas_domain's moving and light and node.gas_ticks' state and velocity".into());
        };
        let fine = GasBytes::of(&geometry).fine;
        if state.size < fine || velocity.size < fine {
            return Err(format!(
                "Gas Volume: the published fields hold {} and {} bytes, the box needs {fine}",
                state.size, velocity.size
            ));
        }
        Ok(Some(Self {
            geometry,
            settings,
            tick,
            restart: tick.index == 0 && read("restarted", 0.0) > 0.5,
            bodies_move: read("solids_move", 0.0) > 0.5,
            static_epoch: read("static_epoch", 0.0),
            matter: Matter::read(ctx),
            bodies: Bodies {
                rows: array("bodies"),
                shapes: array("shapes"),
                atlas: array("atlas"),
                nodes: array("collider_nodes"),
                triangles: array("collider_triangles"),
            },
            lattices: [array("forces"), array("impulses")],
            moving,
            light,
            state,
            velocity,
        }))
    }
}

impl GasVolume {
    /// Encode one tick: reallocate and clear when the box changed or the
    /// clock restarted, lay the still solids when they changed, step the
    /// fields, and on the frame's last tick publish them and the light.
    fn advance(&mut self, gpu: &mut GpuEncoder<'_>, frame: Frame<'_>) -> Result<(), String> {
        let Frame {
            geometry,
            settings,
            tick,
            restart,
            bodies_move,
            static_epoch,
            matter,
            bodies,
            lattices,
            moving,
            light,
            state,
            velocity,
        } = frame;
        let device = gpu.device;
        // Both sets at once, so a solid joining mid-show never waits on a
        // build.
        if self.pipelines.is_none() {
            self.pipelines = Some([false, true].map(|solids| Pipelines::build(device, solids)));
        }
        let resized = self
            .buffers
            .as_ref()
            .is_none_or(|buffers| buffers.dims != geometry.dims);
        if resized {
            // The old fields go first so the new ones are admitted against
            // what is left.
            self.buffers = None;
            self.buffers = Some(GasBuffers::allocate(device, &geometry)?);
            log::info!(
                "Gas Volume: {} × {} × {} cells",
                geometry.dims[0],
                geometry.dims[1],
                geometry.dims[2]
            );
        }
        let with_bodies = tick.solids.is_some();
        let with_solids = with_bodies || matter.any();
        let stored = self.buffers.as_mut().expect("buffers were just allocated");
        stored.keep_solid(device, &geometry, with_solids)?;
        // The Collider meshes are read through their trees, so they are laid
        // only when they or the box change; the moving solids, a lattice
        // each, and the water and sand every tick.
        let rewrite_static = with_solids && stored.static_written != Some(static_epoch);
        stored.static_written = with_solids.then_some(static_epoch);
        let solids_move = (with_bodies && bodies_move) || matter.any();
        let clear = resized || restart;
        if clear {
            self.current = 0;
            self.sim_time = 0.0;
        }
        let lattice = force_lattice(&geometry);
        let buffers = self.buffers.as_ref().expect("buffers were just allocated");
        let pipelines =
            &self.pipelines.as_ref().expect("pipelines were just built")[usize::from(with_solids)];
        let shared = Shared {
            lattices: lattices.map(|lattice| lattice.unwrap_or(&buffers.no_forces)),
            moving,
            light,
            state,
            velocity,
        };
        let solids = SolidBuffers::of(buffers, bodies, &matter, with_bodies);

        // Cell kernels cover the cells; face kernels the one-larger face nodes.
        let groups = geometry.dims.map(|d| d.div_ceil(GROUP_EDGE));
        let face_groups = geometry.dims.map(|d| (d + 1).div_ceil(GROUP_EDGE));
        let fine_groups = geometry.fine_dims().map(|d| d.div_ceil(GROUP_EDGE));
        let enc = &mut *gpu.native_enc;
        let still = Tick { dt: 0.0, ..tick };

        if clear {
            for fine in &buffers.fine {
                enc.clear_buffer(fine);
            }
            let uniforms = GasUniforms::new(&geometry, &settings, 0.0, lattice, still, &matter);
            // vel_in and state_in are read-only, so two dispatches with the
            // roles rotated reach all three buffers.
            for roles in [Roles::new(2, 0), Roles::new(0, 2)] {
                let bindings = binding_list(
                    bytemuck::bytes_of(&uniforms),
                    buffers,
                    roles,
                    shared,
                    &solids,
                );
                pass(
                    enc,
                    &pipelines.clear,
                    &bindings,
                    face_groups,
                    "node.gas_volume.clear",
                );
            }
        }

        let mut last = self.current;
        let mut uniforms = GasUniforms::new(
            &geometry,
            &settings,
            self.sim_time as f32,
            lattice,
            tick,
            &matter,
        );
        let uniform_bytes = bytemuck::bytes_of(&uniforms);
        if rewrite_static {
            enc.clear_buffer(&buffers.source_floor);
            let bindings = binding_list(
                uniform_bytes,
                buffers,
                Roles::new(last, (last + 1) % RING),
                shared,
                &solids,
            );
            pass(
                enc,
                pipelines
                    .solids_static
                    .as_ref()
                    .expect("solids build their kernels"),
                &bindings,
                groups,
                "node.gas_volume.solids_static",
            );
        }
        if tick.dt > 0.0 {
            let (a, b) = ((last + 1) % RING, (last + 2) % RING);
            let advection = binding_list(
                uniform_bytes,
                buffers,
                Roles { input: last, a, b },
                shared,
                &solids,
            );
            if solids_move {
                pass(
                    enc,
                    pipelines
                        .solids
                        .as_ref()
                        .expect("solids build their kernels"),
                    &advection,
                    groups,
                    "node.gas_volume.solids",
                );
            }
            pass(
                enc,
                &pipelines.advect,
                &advection,
                face_groups,
                "node.gas_volume.advect",
            );
            pass(
                enc,
                &pipelines.advect_back,
                &advection,
                face_groups,
                "node.gas_volume.advect_back",
            );
            pass(
                enc,
                &pipelines.maccormack,
                &advection,
                face_groups,
                "node.gas_volume.maccormack",
            );
            pass(
                enc,
                &pipelines.centres_in,
                &advection,
                groups,
                "node.gas_volume.centres_in",
            );
            for (pipeline, label) in [
                (&pipelines.fine_advect, "node.gas_volume.fine_advect"),
                (
                    &pipelines.fine_advect_back,
                    "node.gas_volume.fine_advect_back",
                ),
                (
                    &pipelines.fine_maccormack,
                    "node.gas_volume.fine_maccormack",
                ),
            ] {
                pass(enc, pipeline, &advection, fine_groups, label);
            }
            let working =
                binding_list(uniform_bytes, buffers, Roles::new(last, b), shared, &solids);
            // Swirl at 0 confines nothing: its two passes don't run.
            let swirl = settings.vorticity > 0.0;
            for (pipeline, grid, label, runs) in [
                (
                    &pipelines.fine_sources,
                    fine_groups,
                    "node.gas_volume.fine_sources",
                    true,
                ),
                (
                    &pipelines.restrict,
                    groups,
                    "node.gas_volume.restrict",
                    true,
                ),
                (
                    &pipelines.forces,
                    face_groups,
                    "node.gas_volume.forces",
                    true,
                ),
                (&pipelines.curl, groups, "node.gas_volume.curl", swirl),
                (
                    &pipelines.confine,
                    face_groups,
                    "node.gas_volume.confine",
                    swirl,
                ),
                (
                    &pipelines.divergence,
                    groups,
                    "node.gas_volume.divergence",
                    true,
                ),
            ] {
                if runs {
                    pass(enc, pipeline, &working, grid, label);
                }
            }
            for _ in 0..settings.quality.pressure_pairs() {
                pass(
                    enc,
                    &pipelines.pressure_red,
                    &working,
                    groups,
                    "node.gas_volume.pressure_red",
                );
                pass(
                    enc,
                    &pipelines.pressure_black,
                    &working,
                    groups,
                    "node.gas_volume.pressure_black",
                );
            }
            pass(
                enc,
                &pipelines.project,
                &working,
                face_groups,
                "node.gas_volume.project",
            );
            if let Some(fill) = &pipelines.fill_solids {
                pass(
                    enc,
                    fill,
                    &working,
                    fine_groups,
                    "node.gas_volume.fill_solids",
                );
            }
            last = b;
            self.sim_time += f64::from(tick.dt);
        }
        self.current = last;
        if !tick.last {
            return Ok(());
        }

        // The published fields and this frame's light, from the result.
        uniforms.time = self.sim_time as f32;
        let working = binding_list(
            bytemuck::bytes_of(&uniforms),
            buffers,
            Roles::new((last + 1) % RING, last),
            shared,
            &solids,
        );
        pass(
            enc,
            &pipelines.clear_sums,
            &working,
            [1, 1, 1],
            "node.gas_volume.clear_sums",
        );
        pass(
            enc,
            &pipelines.publish,
            &working,
            groups,
            "node.gas_volume.publish",
        );
        pass(
            enc,
            &pipelines.centres_a,
            &working,
            groups,
            "node.gas_volume.centres_a",
        );
        pass(
            enc,
            &pipelines.publish_velocity,
            &working,
            fine_groups,
            "node.gas_volume.publish_velocity",
        );
        Ok(())
    }
}

impl Primitive for GasVolume {
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("state", "state_out"), ("velocity", "velocity_out")]
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        let input = match port {
            "state_out" => "state",
            "velocity_out" => "velocity",
            _ => return None,
        };
        inputs
            .iter()
            .find(|(name, _)| *name == input)
            .map(|&(_, n)| n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let frame = match Frame::read(ctx) {
            Ok(Some(frame)) => frame,
            Ok(None) => return,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        if let Err(error) = self.advance(ctx.gpu_encoder(), frame) {
            ctx.error(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_shader(solids: bool) -> naga::Module {
        let module =
            naga::front::wgsl::parse_str(&SHADERS[usize::from(solids)]).expect("gas shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("gas shader validates");
        module
    }

    #[test]
    fn shader_is_valid_wgsl_with_every_entry_point() {
        for module in [valid_shader(false), valid_shader(true)] {
            for entry in [
                "cs_advect",
                "cs_advect_back",
                "cs_maccormack",
                "cs_centres_in",
                "cs_centres_a",
                "cs_fine_advect",
                "cs_fine_advect_back",
                "cs_fine_maccormack",
                "cs_fine_sources",
                "cs_restrict",
                "cs_forces",
                "cs_curl",
                "cs_confine",
                "cs_divergence",
                "cs_pressure_red",
                "cs_pressure_black",
                "cs_project",
                "cs_clear_sums",
                "cs_publish",
                "cs_publish_velocity",
                "cs_clear",
                "cs_solids_static",
                "cs_solids",
                "cs_fill_solids",
            ] {
                assert!(
                    module.entry_points.iter().any(|point| point.name == entry),
                    "missing {entry}"
                );
            }
        }
    }

    #[test]
    fn uniforms_mirror_the_shader_params_field_for_field() {
        macro_rules! offsets {
            ($($field:ident),* $(,)?) => {
                [$((stringify!($field), std::mem::offset_of!(GasUniforms, $field))),*]
            };
        }
        let rust = offsets!(
            dims,
            cells,
            origin,
            h,
            up,
            g,
            dt,
            time,
            kind,
            pad,
            source_size,
            rate,
            inflow,
            source_heat,
            buoyancy,
            weight,
            vorticity,
            turbulence,
            dissipation,
            cooling,
            burn_rate,
            heat_release,
            soot,
            expansion,
            ignition,
            seed,
            force_min,
            force_spacing,
            force_nodes,
            forces_on,
            force_base,
            impulse_on,
            impulse_base,
            solid_base,
            solid_count,
            water_pool,
            sand_inv_cell,
            water_cell,
        );
        assert_eq!(std::mem::size_of::<GasUniforms>(), 192);
        let module = valid_shader(true);
        let params = module
            .types
            .iter()
            .map(|(_, ty)| ty)
            .find(|ty| ty.name.as_deref() == Some("Params"))
            .expect("shader declares Params");
        let naga::TypeInner::Struct { members, span } = &params.inner else {
            panic!("Params is a struct");
        };
        assert_eq!(*span as usize, std::mem::size_of::<GasUniforms>());
        assert_eq!(members.len(), rust.len());
        for (name, offset) in rust {
            let member = members
                .iter()
                .find(|member| member.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("Params has no {name}"));
            assert_eq!(member.offset as usize, offset, "{name}");
        }
    }

    fn read_with(domain: Transform, cell: f32) -> Result<GasGeometry, String> {
        gas_geometry(
            |name, default| if name == "cell_metres" { cell } else { default },
            Some(domain),
        )
    }

    fn boxed(centre: [f32; 3], size: [f32; 3]) -> Transform {
        Transform {
            pos: centre,
            scale: size,
            ..Transform::default()
        }
    }

    #[test]
    fn a_two_by_three_by_two_metre_box_is_32_by_48_by_32_cells_on_whole_cells() {
        let aligned = read_with(boxed([0.0, 1.5, 0.0], [2.0, 3.0, 2.0]), 0.0625).unwrap();
        assert_eq!(aligned.dims, [32, 48, 32]);
        assert_eq!(aligned.origin, [-1.0, 0.0, -1.0]);
        // Off the grid, the corner still lands on a whole cell at or below the
        // box, and the cell counts do not depend on where the box sits.
        let offset = read_with(boxed([0.031, 1.52, -0.4], [2.0, 3.0, 2.0]), 0.0625).unwrap();
        assert_eq!(offset.dims, [32, 48, 32]);
        let min = [0.031_f64 - 1.0, 1.52 - 1.5, -0.4 - 1.0];
        for (axis, (&origin, min)) in offset.origin.iter().zip(min).enumerate() {
            let origin = f64::from(origin);
            let cells = origin / 0.0625;
            assert!((cells - cells.round()).abs() < 1e-9, "axis {axis}: {cells}");
            assert!(origin <= min + 1e-3, "axis {axis}");
            assert!(min - origin < 0.0625 + 1e-3, "axis {axis}");
        }
    }

    #[test]
    fn a_tiny_box_still_gets_cells_to_look_both_ways() {
        let geometry = read_with(boxed([0.0; 3], [0.01, 0.01, 0.01]), 0.0625).unwrap();
        assert_eq!(geometry.dims, [MIN_AXIS_CELLS; 3]);
    }

    #[test]
    fn a_box_too_big_for_its_cells_is_refused_by_name() {
        // 40 m at 6.25 cm is 640 cells on an axis.
        let error = read_with(boxed([0.0; 3], [40.0, 1.0, 1.0]), 0.0625).unwrap_err();
        assert!(error.starts_with("Gas Volume:"), "{error}");
        assert!(error.contains("640") && error.contains("512"), "{error}");
        assert!(error.contains("Cell Size"), "{error}");
        // 16.5 m cubed at 6.25 cm is 264 cells per axis, past 16 777 216 in all.
        let error = read_with(boxed([0.0; 3], [16.0, 16.0, 16.5]), 0.0625).unwrap_err();
        assert!(error.contains("17301504"), "{error}");
    }

    #[test]
    fn a_missing_box_or_a_bad_cell_size_is_refused_by_name() {
        let none = gas_geometry(|_, default| default, None).unwrap_err();
        assert!(none.contains("domain"), "{none}");
        for cell in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let error = read_with(boxed([0.0; 3], [1.0; 3]), cell).unwrap_err();
            assert!(error.contains("Cell Size"), "{cell}: {error}");
        }
        let flat = read_with(boxed([0.0; 3], [1.0, 0.0, 1.0]), 0.0625).unwrap_err();
        assert!(flat.contains("above 0"), "{flat}");
    }

    #[test]
    fn settings_default_to_smoke_and_refuse_what_is_not_a_number() {
        let settings = gas_settings(|_, default| default).unwrap();
        assert_eq!(settings.kind, 0);
        assert_eq!(settings.quality, Quality::High);
        assert_eq!(settings.gravity, [0.0, -DEFAULT_GRAVITY, 0.0]);
        assert_eq!(settings.rate, 4.0);
        assert_eq!(settings.soot, 0.6);
        let fire =
            gas_settings(|name, default| if name == "kind" { 1.0 } else { default }).unwrap();
        assert_eq!(fire.kind, 1);
        let error = gas_settings(|name, default| if name == "rate" { f32::NAN } else { default })
            .unwrap_err();
        assert!(error.contains("rate"), "{error}");
        let error = gas_settings(|name, default| if name == "source_size" { 0.0 } else { default })
            .unwrap_err();
        assert!(error.contains("Source Size"), "{error}");
    }

    #[test]
    fn quality_sets_the_pressure_sweeps() {
        let pairs = [0.0, 1.0, 2.0, 3.0].map(|q| Quality::from_param(q).pressure_pairs());
        assert_eq!(pairs, [20, 30, 50, 80]);
        assert_eq!(Quality::from_param(-4.0), Quality::Low);
        assert_eq!(Quality::from_param(9.0), Quality::ExtraHigh);
    }

    #[test]
    fn up_is_against_gravity_and_a_weightless_gas_has_no_weight() {
        assert_eq!(up_and_gravity([0.0, -9.81, 0.0]), ([0.0, 1.0, 0.0], 9.81));
        let (up, g) = up_and_gravity([9.81, 0.0, 0.0]);
        assert_eq!(up, [-1.0, 0.0, 0.0]);
        assert!((g - 9.81).abs() < 1e-6);
        assert_eq!(up_and_gravity([0.0; 3]), ([0.0, 1.0, 0.0], 0.0));
    }

    #[test]
    fn every_buffer_is_counted_once() {
        let geometry = GasGeometry {
            cell: 0.1,
            origin: [0.0; 3],
            dims: [10, 10, 10],
        };
        let bytes = GasBytes::of(&geometry);
        assert_eq!(bytes.vec4, 16_000);
        assert_eq!(bytes.faces, 11 * 11 * 11 * 16);
        assert_eq!(bytes.fine, 8_000 * 8);
        assert_eq!(
            bytes.total(),
            2 * 16_000 + 3 * 21_296 + 2 * 4_000 + 3 * 64_000 + 256 * 16
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

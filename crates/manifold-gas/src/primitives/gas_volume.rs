//! `node.gas_volume` — the gas solver of Smoke and Fire: a dense, axis-aligned
//! box of cells advanced on the GPU once a tick of the World's clock (see
//! docs/SMOKE_AND_FIRE_MAP.md). The kernels are in `shaders/gas_step.wgsl`.
//!
//! The box floor is a wall; the sides and top are open. The scene's Colliders
//! and Bodies are solids the air flows round and a moving body pushes: the
//! gas takes them from the water that owns the World, as a second water
//! does, or from the World itself when no water owns it
//! (`liquid::coupling::SharedSolids`). Water and sand are solids too: their
//! particles from the frame before fill the cells they cover, which move with them, so
//! smoke rolls over water and a splash or a sand slide shoves it. The fields
//! stay on the GPU. The node
//! publishes the packed state and velocity of every cell as provided arrays,
//! and reads the flames' light back from a ring of small buffers, two frames
//! late.

use std::borrow::Cow;
use std::fmt::Display;
use std::sync::LazyLock;

use manifold_core::fluid_domain::{CELL_SIZE_METRES, DEFAULT_CELL_SIZE};
use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};
use manifold_node_engine::exec::effect_node::SimStep;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::{Primitive, PrimitiveSpec};
use manifold_node_engine::scene::blackbody::{
    BLACKBODY_ENTRIES, BLACKBODY_MAX_KELVIN, BLACKBODY_MIN_KELVIN, blackbody_table,
};
use manifold_node_engine::scene::transform::Transform;
use manifold_nodes_physics::force_lattice::{LatticeBuffers, LatticeUpload, felt_field};
use manifold_nodes_physics::hits::{TakesHits, TakesHitsRegistration};
use manifold_nodes_physics::tick_sampled::{TickSampled, TickSampledRegistration};
use manifold_physics::clock::{ClockFrame, LiveLoad, SimulationClock};
use manifold_physics::force_lattice::{ForceLattice, TickForces};
use manifold_physics::input::{AppliedEvent, EventStamp};
use manifold_physics::tick_hits::TickHits;
use manifold_physics::{FieldValue, PlayedTime, Seconds, TickStamp, VectorField};
use manifold_water_liquid::bodies::{
    BodiesStatus, COLLIDER_BVH, LIQUID_COLLIDER, LIQUID_POSE, LiquidBodies, LiquidBody, LiquidShape,
};
use manifold_water_liquid::body_buffers::LiquidBodyBuffers;
use manifold_water_liquid::coupling::{LiquidRigidOwner, SharedSolids, SharesSolids};

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
/// The longest step, in seconds: the solver stays stable under a slow frame.
const MAX_STEP_SECONDS: f32 = 1.0 / 20.0;
/// Gravity of a scene that wires none: Earth's, down.
const DEFAULT_GRAVITY: f32 = 9.81;
/// The temperature past which fuel burns.
const IGNITION: f32 = 0.2;
/// Threads per workgroup edge in `gas_step.wgsl`.
const GROUP_EDGE: u32 = 4;
/// Most workgroups along x of the particle kernel; more wrap onto y.
const MAX_GROUPS_ACROSS: u32 = 65_535;
/// Light sum buffers in the ring: this frame's write, the one still in
/// flight and the one the CPU reads.
const RING: usize = 3;
/// Words of one light sum buffer (see `cs_publish`).
const LIGHT_WORDS: usize = 8;
const LIGHT_BYTES: u64 = LIGHT_WORDS as u64 * 4;
/// The light sums' fixed point: a sum adds `round(value * LIGHT_FIXED_SCALE)`
/// as an unsigned word. Positions are relative to the box origin, so every sum
/// is non-negative; a word wraps past `2^32 / LIGHT_FIXED_SCALE` = 262 144.
const LIGHT_FIXED_SCALE: f32 = 16384.0;
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
            "{}\n{LIQUID_POSE}\n{LIQUID_COLLIDER}\n{COLLIDER_BVH}\n{}",
            shader_constants(solids),
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
    fn cells(&self) -> u64 {
        self.dims.iter().map(|&d| u64::from(d)).product()
    }

    /// Nodes of the staggered velocity: one more than the cells along each
    /// axis, so every face of every cell has one.
    fn face_nodes(&self) -> u64 {
        self.dims.iter().map(|&d| u64::from(d) + 1).product()
    }

    /// The fine state grid's cells along each axis.
    fn fine_dims(&self) -> [u32; 3] {
        self.dims.map(|d| d * FINE)
    }

    fn fine_cells(&self) -> u64 {
        self.cells() * u64::from(FINE.pow(3))
    }

    /// The scalar outputs this geometry fixes, by name: the published
    /// fields' grid, which is the fine one.
    fn outputs(&self) -> [(&'static str, f32); 7] {
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
fn gas_geometry(
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
    speed: f32,
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
        speed: read("speed", 1.0),
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
        ("speed", settings.speed),
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
    water_count: u32,
    sand_count: u32,
    pad4: u32,
    /// One bit a body row: a Collider that moves this frame ([`moving_trees`]).
    moving: [u32; 8],
}

impl GasUniforms {
    fn new(
        geometry: &GasGeometry,
        settings: &GasSettings,
        time: f32,
        lattice: ForceLattice,
        step: Step,
        matter: &Matter<'_>,
        moving: [u32; 8],
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
            solid_base: step.solids.map_or(0, |solids| solids.base),
            solid_count: step.solids.map_or(0, |solids| solids.count),
            water_count: matter.water.map_or(0, |(_, count)| count),
            sand_count: matter.sand.map_or(0, |(_, count)| count),
            pad4: 0,
            moving,
        }
    }
}

// ---- Storage -------------------------------------------------------------

/// Bytes of each kind of buffer for a box of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GasBytes {
    /// A vec4 per cell: the averaged state and the curl.
    vec4: u64,
    /// A vec4 per face node: a velocity buffer (three of them).
    faces: u64,
    /// An f32 per cell: pressure and divergence.
    scalar: u64,
    /// Two words per fine cell: a fine state buffer (three of them) and the
    /// published velocity.
    fine: u64,
}

impl GasBytes {
    fn of(geometry: &GasGeometry) -> Self {
        let cells = geometry.cells();
        Self {
            vec4: cells * 16,
            faces: geometry.face_nodes() * 16,
            scalar: cells * 4,
            fine: geometry.fine_cells() * 8,
        }
    }

    /// Everything the node holds: the rotating fields, the averaged state,
    /// the curl, pressure and divergence, the published velocity, the light
    /// ring and the blackbody table.
    fn total(&self) -> u64 {
        2 * self.vec4
            + 3 * self.faces
            + 2 * self.scalar
            + 4 * self.fine
            + RING as u64 * LIGHT_BYTES
            + BLACKBODY_BYTES
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
    /// Last frame's result, a step's scratch, and the other: roles rotate.
    /// The fine state's result is also the published state.
    velocity: [GpuBuffer; RING],
    fine: [GpuBuffer; RING],
    /// The fine state averaged onto the air's cells.
    state: GpuBuffer,
    curl: GpuBuffer,
    /// Kept across frames: the solve starts from the last frame's pressure.
    pressure: GpuBuffer,
    divergence: GpuBuffer,
    velocity_out: GpuBuffer,
    lights: [GpuBuffer; RING],
    blackbody: GpuBuffer,
    /// What the force and solid bindings hold while nothing reaches them.
    no_forces: GpuBuffer,
    /// Two words a cell: the solid there, while the scene has solids.
    solid: Option<GpuBuffer>,
    /// The static solids `solid` holds, by [`static_hash`].
    static_written: Option<u64>,
    /// One word: where the source sits on the static solids.
    source_floor: GpuBuffer,
    /// Four words a cell: the water and sand there, while any is wired.
    matter: Option<GpuBuffer>,
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
            velocity_out: make(bytes.fine, BufferUse::Gpu)?,
            lights: [
                make(LIGHT_BYTES, BufferUse::Readback)?,
                make(LIGHT_BYTES, BufferUse::Readback)?,
                make(LIGHT_BYTES, BufferUse::Readback)?,
            ],
            blackbody,
            no_forces: make(32, BufferUse::Gpu)?,
            solid: None,
            static_written: None,
            source_floor: make(16, BufferUse::Gpu)?,
            matter: None,
        })
    }

    /// Keep the water and sand field while either reaches the gas, admitted
    /// against the World's budget; drop it when neither does.
    fn keep_matter(
        &mut self,
        device: &GpuDevice,
        geometry: &GasGeometry,
        wanted: bool,
    ) -> Result<(), String> {
        if !wanted {
            self.matter = None;
        } else if self.matter.is_none() {
            let bytes = geometry.cells() * 16;
            manifold_node_engine::load::expand::admit_world_bytes(
                device.modifier_memory_snapshot(),
                bytes,
            )
            .map_err(|error| refusal(geometry, bytes, error))?;
            self.matter = Some(
                device
                    .try_create_buffer(bytes, BufferUse::Gpu)
                    .map_err(|error| refusal(geometry, bytes, error))?,
            );
        }
        Ok(())
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
    matter: Option<GpuComputePipeline>,
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
            matter: solids.then(|| make("cs_matter")),
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

/// Every binding of `gas_step.wgsl`. A kernel binds all of them and uses its
/// own; the encoder skips a binding the kernel does not declare.
fn binding_list<'a>(
    uniforms: &'a [u8],
    buffers: &'a GasBuffers,
    roles: Roles,
    light: &'a GpuBuffer,
    lattices: [&'a GpuBuffer; 2],
    solids: &SolidBuffers<'a>,
) -> [GpuBinding<'a>; 26] {
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
        bind(11, &buffers.velocity_out),
        bind(12, light),
        bind(13, &buffers.blackbody),
        bind(14, lattices[0]),
        bind(15, lattices[1]),
        bind(16, solids.bodies),
        bind(17, solids.shapes),
        bind(18, solids.atlas),
        bind(19, solids.nodes),
        bind(20, solids.triangles),
        bind(21, solids.cells),
        bind(22, &buffers.source_floor),
        bind(23, solids.matter),
        bind(24, solids.water),
        bind(25, solids.sand),
    ]
}

/// What the solid bindings hold: the scene's bodies, shapes, lattices and
/// trees, the solid field, the water and sand field and their particles, or
/// the placeholder for each the scene has none of.
struct SolidBuffers<'a> {
    bodies: &'a GpuBuffer,
    shapes: &'a GpuBuffer,
    atlas: &'a GpuBuffer,
    nodes: &'a GpuBuffer,
    triangles: &'a GpuBuffer,
    cells: &'a GpuBuffer,
    matter: &'a GpuBuffer,
    water: &'a GpuBuffer,
    sand: &'a GpuBuffer,
}

impl<'a> SolidBuffers<'a> {
    fn of(
        buffers: &'a GasBuffers,
        bodies: &'a LiquidBodyBuffers,
        matter: &Matter<'a>,
        with_bodies: bool,
    ) -> Self {
        let none = &buffers.no_forces;
        let rows = bodies.bodies().filter(|_| with_bodies);
        Self {
            bodies: rows.unwrap_or(none),
            shapes: rows.and(bodies.shapes()).unwrap_or(none),
            atlas: rows.and(bodies.atlas()).unwrap_or(none),
            nodes: rows.and(bodies.collider_nodes()).unwrap_or(none),
            triangles: rows.and(bodies.collider_triangles()).unwrap_or(none),
            cells: buffers.solid.as_ref().unwrap_or(none),
            matter: buffers.matter.as_ref().unwrap_or(none),
            water: matter.water.map_or(none, |(points, _)| points),
            sand: matter.sand.map_or(none, |(points, _)| points),
        }
    }
}

/// Last frame's water and sand particles that reach the gas: each buffer and
/// the count of its particles in use, None for one not wired or empty.
#[derive(Clone, Copy, Default)]
struct Matter<'a> {
    water: Option<(&'a GpuBuffer, u32)>,
    sand: Option<(&'a GpuBuffer, u32)>,
}

impl<'a> Matter<'a> {
    fn read(ctx: &EffectNodeContext<'a, '_>) -> Self {
        let read = |points: &str, count: &str| {
            let buffer = ctx.inputs.array(points)?;
            let slots = buffer.size / std::mem::size_of::<FluidParticle>() as u64;
            let count = (ctx.scalar_or_param(count, 0.0).round().max(0.0) as u64).min(slots);
            (count > 0).then_some((buffer, count as u32))
        };
        Self {
            water: read("water", "water_count"),
            sand: read("sand", "sand_count"),
        }
    }

    fn any(&self) -> bool {
        self.water.is_some() || self.sand.is_some()
    }

    fn count(&self) -> u32 {
        self.water.map_or(0, |(_, n)| n) + self.sand.map_or(0, |(_, n)| n)
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
    power: f32,
    position: [f32; 3],
    temperature: f32,
}

impl LightReadout {
    fn outputs(&self) -> [(&'static str, f32); 5] {
        [
            ("light_power", self.power),
            ("light_x", self.position[0]),
            ("light_y", self.position[1]),
            ("light_z", self.position[2]),
            ("light_temperature", self.temperature),
        ]
    }
}

/// Decode a light sum buffer written by `cs_publish`: word 0 is the emission
/// sum, words 1 to 3 the emission times the cell centre relative to `origin`,
/// word 4 the emission times the temperature, all in [`LIGHT_FIXED_SCALE`]
/// fixed point. No emission reads as no light.
fn decode_light(words: &[u32; LIGHT_WORDS], origin: [f32; 3]) -> LightReadout {
    let value = |word: u32| f64::from(word) / f64::from(LIGHT_FIXED_SCALE);
    let power = value(words[0]);
    if power <= 0.0 {
        return LightReadout::default();
    }
    LightReadout {
        power: power as f32,
        position: std::array::from_fn(|axis| {
            (f64::from(origin[axis]) + value(words[1 + axis]) / power) as f32
        }),
        temperature: (value(words[4]) / power) as f32,
    }
}

fn read_light_words(buffer: &GpuBuffer) -> [u32; LIGHT_WORDS] {
    let source = buffer
        .mapped_ptr()
        .expect("gas light sums must be CPU-visible")
        .cast::<u32>()
        .cast_const();
    let mut words = [0u32; LIGHT_WORDS];
    // SAFETY: shared, 4-byte aligned storage of LIGHT_WORDS words whose last
    // GPU writer has retired (the caller checked the frame clock).
    unsafe { std::ptr::copy_nonoverlapping(source, words.as_mut_ptr(), LIGHT_WORDS) };
    words
}

/// The ring slot frame `frame` writes and the one it reads (written two
/// frames earlier).
fn ring_slots(frame: u64) -> (usize, usize) {
    let ring = RING as u64;
    ((frame % ring) as usize, ((frame + 1) % ring) as usize)
}

/// A written light slot: when its frame retires, and the box origin the sums
/// are relative to.
#[derive(Clone, Copy)]
pub struct LightSlot {
    stamp: u64,
    origin: [f32; 3],
}

// ---- Node ----------------------------------------------------------------

manifold_node_engine::primitive! {
    name: GasVolume,
    type_id: "node.gas_volume",
    purpose: "Simulate smoke or fire in an axis-aligned box of cells: the domain Transform is the box (its position the centre, its scale the size in metres; rotation is ignored) and cell_metres the cell size. The floor is a wall; the sides and top are open and fade what reaches them. A source at the floor's centre adds smoke density (Kind Smoke) or fuel that ignites past the ignition temperature (Kind Fire); hot gas rises against gravity, swirls, and expands where it burns. It steps once a tick of the World's fixed clock (each tick's length times speed, at most 1/20 s); scene forces on acceleration_field push it (Radial, Vortex, Uniform) or carry it (a wind is the air moving), laid on a lattice over the box at each tick's start, and a force's hit changes its velocity once on the tick it lands on. A reset edge, a transport restart, a backward seek or a change of the box's cell counts clears it. Publishes state (two words per cell: density and temperature, then fuel and flame, each pair as two halves) and velocity (u and v, then w and 0, as halves) as arrays with cell (x, y, z) at index x + nx (y + ny z); the box's first corner as origin_x, origin_y, origin_z with cell_size and dims_x, dims_y, dims_z; and the flames' light read back two frames late: light_power (the emission sum, 0 for no flame), light_x, light_y, light_z (its centre in the world) and light_temperature (0 ambient, 1 a hot flame).",
    inputs: {
        domain: Transform optional,
        cell_metres: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        speed: ScalarF32 optional,
        reset: ScalarF32 optional,
        acceleration_field: VectorField optional,
        water: Array(FluidParticle) optional,
        water_count: ScalarF32 optional,
        sand: Array(FluidParticle) optional,
        sand_count: ScalarF32 optional,
    },
    outputs: {
        state: Array(u32),
        velocity: Array(u32),
        cell_size: ScalarF32,
        origin_x: ScalarF32, origin_y: ScalarF32, origin_z: ScalarF32,
        dims_x: ScalarF32, dims_y: ScalarF32, dims_z: ScalarF32,
        light_power: ScalarF32,
        light_x: ScalarF32, light_y: ScalarF32, light_z: ScalarF32,
        light_temperature: ScalarF32,
    },
    params: [
        gas_param!("kind", "Kind", Int, 0.0, 0.0, 1.0),
        gas_param!("quality", "Quality", Int, 2.0, 0.0, 3.0),
        gas_param!("cell_metres", "Cell Size", Float, DEFAULT_CELL_SIZE, CELL_SIZE_METRES.0, CELL_SIZE_METRES.1),
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
    composition_notes: "Wire the Smoke or Fire item's box Transform into domain, and the World's gravity, speed and reset into their inputs. Wire a Water's node.liquid_frame particles_b and count_b into water and water_count, and a Sand's node.matter_frame ones into sand and sand_count: a cell at least half full of them is a solid moving with them. The defaults are Smoke's; Fire sets Kind to 1 and its own rates. A change of cell_metres or of the box's size past a whole cell restarts the gas; moving the box does not. state and velocity hold half-float pairs for the volume renderer; light_* feed the fire's point light.",
    examples: [],
    picker: { label: "Gas Volume", category: Atom },
    summary: "Simulates smoke or fire in a box of cells.",
    category: Particles3D,
    role: Source,
    aliases: ["smoke", "fire", "gas", "flame", "volume simulation"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        buffers: Option<GasBuffers> = None,
        // Without solids, then with them.
        pipelines: Option<[Pipelines; 2]> = None,
        // Which of the three velocity and state buffers holds the last result.
        current: usize = 0,
        // The World's fixed ticks: the gas steps once a tick, so a force or
        // a hit lands on the same step at any frame rate, as water does.
        clock: SimulationClock = SimulationClock::default(),
        steps: Vec<Step> = Vec::new(),
        // Seconds stepped since the last clear: the source's noise clock.
        sim_time: f64 = 0.0,
        // Frames published since the last clear: the light ring's cursor.
        frames: u64 = 0,
        light_slots: [Option<LightSlot>; RING] = [None; RING],
        light: LightReadout = LightReadout::default(),
        // The scene's forces laid at each tick's start on a lattice over the
        // box, the hits landing on the frame's ticks, and their GPU copies.
        forces: TickForces = TickForces::default(),
        hits: TickHits<FieldValue> = TickHits::new("Smoke and Fire impulses"),
        impulses: Vec<[f32; 4]> = Vec::new(),
        impulses_dirty: bool = false,
        lattices: LatticeBuffers = LatticeBuffers::default(),
        // The scene's Colliders and Bodies, handed over before the gas runs;
        // their shapes and lattices at the gas's cell, a row per body per
        // tick, and the GPU copies.
        solids: SharedSolids = SharedSolids::default(),
        bodies: LiquidBodies = LiquidBodies::default(),
        body_buffers: LiquidBodyBuffers = LiquidBodyBuffers::default(),
        // The first tick's body poses last frame, to tell which Colliders
        // moved since.
        tree_poses: Vec<[f32; 7]> = Vec::new(),
    },
}

/// The box and settings this frame's wires and params describe.
fn plan(ctx: &EffectNodeContext<'_, '_>) -> Result<(GasGeometry, GasSettings), String> {
    let read = |name: &str, default: f32| ctx.scalar_or_param(name, default);
    let geometry = gas_geometry(read, ctx.inputs.transform("domain"))?;
    let settings = gas_settings(read)?;
    Ok((geometry, settings))
}

/// Set every scalar output, so no consumer reads a slot this node left unwritten.
fn publish(ctx: &mut EffectNodeContext<'_, '_>, geometry: &GasGeometry, light: &LightReadout) {
    for (name, value) in geometry.outputs().into_iter().chain(light.outputs()) {
        ctx.outputs.set_scalar(name, ParamValue::Float(value));
    }
}

/// One tick of a frame: its simulated seconds, the force and impulse
/// lattices it reads, by index, and its solids' rows, if any.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    dt: f32,
    forces: Option<u32>,
    impulses: Option<u32>,
    solids: Option<StepSolids>,
}

/// A tick's body rows: `count` of them from row `base`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepSolids {
    base: u32,
    count: u32,
}

/// The Collider meshes as the first tick places them, every shape, the
/// trees' sizes and the box: a change of any rewrites the static solids.
/// Without bodies only the box counts.
fn static_hash(bodies: Option<&LiquidBodies>, geometry: &GasGeometry, moving: [u32; 8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    geometry.origin.map(f32::to_bits).hash(&mut hasher);
    geometry.dims.hash(&mut hasher);
    geometry.cell.to_bits().hash(&mut hasher);
    let Some(bodies) = bodies else {
        return hasher.finish();
    };
    let shapes = bodies.shapes();
    bytemuck::cast_slice::<LiquidShape, u8>(shapes).hash(&mut hasher);
    bodies.collider_nodes().len().hash(&mut hasher);
    bodies.collider_triangles().len().hash(&mut hasher);
    // A moving Collider is laid each tick, so only which ones move counts.
    moving.hash(&mut hasher);
    for (index, row) in tree_rows(bodies, &bodies.last_rows()[..first_tick_rows(bodies)]) {
        if !is_moving(moving, index) {
            bytemuck::bytes_of(row).hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Which Colliders read through their trees move this frame, one bit a body:
/// a row with motion, or a pose at any of the frame's ticks other than its
/// pose at the last frame's first tick (`last`). A Collider driven by a
/// param can take a new pose with no motion in its row.
fn moving_trees(bodies: &LiquidBodies, last: &[[f32; 7]]) -> [u32; 8] {
    let mut moving = [0; 8];
    let count = bodies.count();
    if count == 0 {
        return moving;
    }
    for (index, row) in tree_rows(bodies, bodies.last_rows()) {
        let body = index % count;
        if row_moves(row) || last.get(body) != Some(&pose(row)) {
            moving[body / 32] |= 1 << (body % 32);
        }
    }
    moving
}

fn is_moving(moving: [u32; 8], body: usize) -> bool {
    moving[body / 32] & (1 << (body % 32)) != 0
}

/// A row's position and rotation.
fn pose(row: &LiquidBody) -> [f32; 7] {
    let (p, q) = (row.position_inv_mass, row.rotation);
    [p[0], p[1], p[2], q[0], q[1], q[2], q[3]]
}

/// The rows of the frame's first tick, which the static solids are laid
/// from.
fn first_tick_rows(bodies: &LiquidBodies) -> usize {
    bodies.count().min(bodies.last_rows().len())
}

/// The rows among `rows` of Colliders read through their trees, with their
/// index.
fn tree_rows<'a>(
    bodies: &'a LiquidBodies,
    rows: &'a [LiquidBody],
) -> impl Iterator<Item = (usize, &'a LiquidBody)> {
    let shapes = bodies.shapes();
    rows.iter().enumerate().filter(move |(_, row)| {
        let shape = row.accel_shape[3];
        shape >= 0.0 && shapes.get(shape as usize).is_some_and(|s| s.tree == 1)
    })
}

/// A body's row carries motion.
fn row_moves(row: &LiquidBody) -> bool {
    row.linear_velocity[..3] != [0.0; 3] || row.angular_velocity[..3] != [0.0; 3]
}

/// Simulated seconds of the frame's tick `tick`.
fn tick_seconds(frame: &ClockFrame, tick: u32) -> f32 {
    frame
        .interval(u64::from(tick))
        .map_or(0.0, |interval| interval.duration().0) as f32
}

/// The force lattice over the box: a node every `FORCE_LATTICE_STRIDE` cells
/// from its first face to its last, as water and sand lay theirs.
fn force_lattice(geometry: &GasGeometry) -> ForceLattice {
    ForceLattice::covering(geometry.origin, geometry.cell, geometry.dims.map(|d| d + 1))
}

impl GasVolume {
    /// Advance the clock and lay the frame's ticks: each tick's length, the
    /// force lattice it reads (one while the field holds still, else one a
    /// tick, none without a field) and the impulse lattice of the hits
    /// landing on it. Returns whether the clock restarted.
    fn plan_ticks(
        &mut self,
        ctx: &EffectNodeContext<'_, '_>,
        geometry: &GasGeometry,
        settings: &GasSettings,
    ) -> Result<bool, String> {
        // The air itself: no still air of its own (Air Resistance is the
        // air), wind felt at the full drag rate.
        let field = felt_field(ctx, "Gas Volume")?;
        let frame = self.clock.advance(
            ctx.played(),
            ctx.sim_step.interval.0,
            settings.speed,
            ctx.scalar_or_param("reset", 0.0),
            false,
            ctx.sim_step.offline(),
        );
        // Hits follow the clock before anything else can fail, so a refused
        // frame never leaves them a frame behind it.
        self.hits.observe_frame(ctx.played(), &frame)?;
        let lattice = force_lattice(geometry);
        self.forces
            .prepare(lattice, field.as_ref(), &self.clock, &frame)?;
        let hit_ticks = self.lay_impulses(lattice, frame.first_sequence, frame.ticks)?;
        let solids = self.lay_solids(geometry.cell, &frame, ctx.sim_step.offline());
        // The next frame takes the solids anew.
        self.solids.used();
        let solids = solids?;
        let lattices = self.forces.lattices() as u32;
        self.steps.clear();
        for tick in 0..frame.ticks {
            self.steps.push(Step {
                dt: tick_seconds(&frame, tick).clamp(0.0, MAX_STEP_SECONDS),
                forces: (lattices > 0).then(|| tick.min(lattices - 1)),
                impulses: hit_ticks.then_some(tick),
                solids: solids.map(|count| StepSolids {
                    base: tick * count,
                    count,
                }),
            });
        }
        Ok(frame.restarted)
    }

    /// Lay this frame's solids: their shapes at the gas's cell and a row per
    /// body per tick, each carried to its tick's start. Returns the bodies a
    /// tick, or None when the scene has none, the frame has no tick, or
    /// (live) while a shape's tree is still building.
    fn lay_solids(
        &mut self,
        cell: f32,
        frame: &ClockFrame,
        offline: bool,
    ) -> Result<Option<u32>, String> {
        if let Some(reason) = self.solids.refused() {
            return Err(format!("Gas Volume: the scene's solids: {reason}"));
        }
        if self.solids.geometries().is_empty() || frame.ticks == 0 {
            return Ok(None);
        }
        if self
            .bodies
            .prepare(&[], self.solids.geometries(), cell, offline)?
            == BodiesStatus::Pending
        {
            return Ok(None);
        }
        self.bodies
            .rows(frame.first_sequence, frame.ticks, self.solids.rows())?;
        // The owner water's rows sit at the first tick's start; a
        // free-running World's at the frame's end.
        let mut seconds = if self.solids.at_frame_end() {
            -(0..frame.ticks)
                .map(|tick| tick_seconds(frame, tick))
                .sum::<f32>()
        } else {
            0.0
        };
        for tick in 0..frame.ticks {
            if seconds != 0.0 {
                self.bodies
                    .set_coupled_rows(tick as usize, self.solids.rows_after(seconds))?;
            }
            seconds += tick_seconds(frame, tick);
        }
        Ok(Some(self.bodies.count() as u32))
    }

    /// Lay this frame's hits, one impulse lattice per tick of the frame from
    /// `first`, each the sum of the hits landing on that tick (zero where
    /// none did). False when no hit landed: nothing is laid or read.
    fn lay_impulses(
        &mut self,
        lattice: ForceLattice,
        first: u64,
        ticks: u32,
    ) -> Result<bool, String> {
        if self.hits.impulse_tick().is_none() {
            return Ok(false);
        }
        let events = self.hits.frame_events();
        let count = lattice.node_count();
        self.impulses.clear();
        self.impulses.resize(ticks as usize * count, [0.0; 4]);
        for (tick, values) in (first..).zip(self.impulses.chunks_exact_mut(count)) {
            if !events.iter().any(|event| event.applied.tick == tick) {
                continue;
            }
            lattice
                .fill(values, |x| {
                    events
                        .iter()
                        .filter(|event| event.applied.tick == tick)
                        .fold([0.0; 3], |sum, event| {
                            let kick = event.value.sample(x);
                            std::array::from_fn(|axis| sum[axis] + kick[axis])
                        })
                })
                .map_err(|_| "Gas Volume: a hit is not finite inside the box".to_string())?;
        }
        self.impulses_dirty = true;
        Ok(true)
    }

    /// Copy the frame's force and impulse lattices to the GPU when any tick
    /// reads one; with neither, nothing is laid or uploaded.
    fn upload_lattices(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        lattice: ForceLattice,
        offline: bool,
    ) -> Result<(), String> {
        let reads = self
            .steps
            .iter()
            .any(|step| step.forces.is_some() || step.impulses.is_some());
        if !reads {
            return Ok(());
        }
        self.lattices.upload(
            gpu,
            offline,
            LatticeUpload {
                lattice,
                forces: self.forces.forces(),
                forces_dirty: self.forces.is_dirty(),
                impulses: &self.impulses,
                impulses_dirty: self.impulses_dirty,
                hits: &[],
                tick_seconds: &[],
                far: &[],
            },
        )?;
        self.forces.mark_uploaded();
        self.impulses_dirty = false;
        Ok(())
    }

    /// Encode one frame: reallocate and clear when the box changed or the
    /// clock restarted, read the light back, run each of the frame's ticks
    /// and publish the fields.
    fn advance(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        geometry: &GasGeometry,
        settings: &GasSettings,
        restart: bool,
        offline: bool,
        matter: Matter<'_>,
    ) -> Result<(), String> {
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
            self.buffers = Some(GasBuffers::allocate(device, geometry)?);
            log::info!(
                "Gas Volume: {} × {} × {} cells",
                geometry.dims[0],
                geometry.dims[1],
                geometry.dims[2]
            );
        }
        let first_solids = self.steps.iter().find_map(|step| step.solids);
        let with_solids = first_solids.is_some() || matter.any();
        let stored = self.buffers.as_mut().expect("buffers were just allocated");
        stored.keep_solid(device, geometry, with_solids)?;
        stored.keep_matter(device, geometry, matter.any())?;
        // The Collider meshes are read through their trees, so they are
        // written only when they or the box change; the moving solids, a
        // lattice each, and the water and sand every tick. With none, the
        // rows stay as they are.
        let mut moving = [0; 8];
        if first_solids.is_some() {
            moving = moving_trees(&self.bodies, &self.tree_poses);
            let first = &self.bodies.last_rows()[..first_tick_rows(&self.bodies)];
            self.tree_poses.clear();
            self.tree_poses.extend(first.iter().map(pose));
        }
        let static_hash =
            with_solids.then(|| static_hash(first_solids.map(|_| &self.bodies), geometry, moving));
        let rewrite_static = static_hash.is_some() && stored.static_written != static_hash;
        // Bodies and hulls are lattices sampled each tick, and a Collider
        // that moves is read through its tree each tick.
        let bodies_move = first_solids.is_some()
            && (self.bodies.shapes().iter().any(|s| s.tree != 1) || moving.iter().any(|&w| w != 0));
        let solids_move = bodies_move || matter.any();
        if first_solids.is_some() && (rewrite_static || bodies_move) {
            self.body_buffers
                .upload(gpu, &self.bodies, true, "node.gas_volume.bodies")?;
        }
        stored.static_written = static_hash;
        let clear = resized || restart;
        if clear {
            self.current = 0;
            self.sim_time = 0.0;
            self.frames = 0;
            self.light_slots = [None; RING];
            self.light = LightReadout::default();
        }
        let lattice = force_lattice(geometry);
        self.upload_lattices(gpu, lattice, offline)?;
        let buffers = self.buffers.as_ref().expect("buffers were just allocated");
        let pipelines =
            &self.pipelines.as_ref().expect("pipelines were just built")[usize::from(with_solids)];
        let lattices = [
            self.lattices.forces().unwrap_or(&buffers.no_forces),
            self.lattices.impulses().unwrap_or(&buffers.no_forces),
        ];
        let solids = SolidBuffers::of(buffers, &self.body_buffers, &matter, first_solids.is_some());

        // The light written two frames ago, once its frame has retired.
        let clock = device.frame_clock();
        let (write_slot, read_slot) = ring_slots(self.frames);
        if self.frames >= RING as u64 - 1
            && let Some(slot) = self.light_slots[read_slot]
            && clock
                .as_ref()
                .is_none_or(|clock| clock.is_complete(slot.stamp))
        {
            self.light = decode_light(&read_light_words(&buffers.lights[read_slot]), slot.origin);
        }

        // Cell kernels cover the cells; face kernels the one-larger face nodes.
        let groups = geometry.dims.map(|d| d.div_ceil(GROUP_EDGE));
        let face_groups = geometry.dims.map(|d| (d + 1).div_ceil(GROUP_EDGE));
        let fine_groups = geometry.fine_dims().map(|d| d.div_ceil(GROUP_EDGE));
        let light = &buffers.lights[write_slot];
        let enc = &mut *gpu.native_enc;
        let still = Step {
            dt: 0.0,
            forces: None,
            impulses: None,
            solids: first_solids,
        };

        if clear {
            for fine in &buffers.fine {
                enc.clear_buffer(fine);
            }
            enc.clear_buffer(&buffers.velocity_out);
            let uniforms =
                GasUniforms::new(geometry, settings, 0.0, lattice, still, &matter, moving);
            // vel_in and state_in are read-only, so two dispatches with the
            // roles rotated reach all three buffers.
            for roles in [Roles::new(2, 0), Roles::new(0, 2)] {
                let bindings = binding_list(
                    bytemuck::bytes_of(&uniforms),
                    buffers,
                    roles,
                    light,
                    lattices,
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
            geometry,
            settings,
            self.sim_time as f32,
            lattice,
            still,
            &matter,
            moving,
        );
        if rewrite_static {
            enc.clear_buffer(&buffers.source_floor);
            let bindings = binding_list(
                bytemuck::bytes_of(&uniforms),
                buffers,
                Roles::new(last, (last + 1) % RING),
                light,
                lattices,
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
        if let (Some(field), true) = (&buffers.matter, self.steps.iter().any(|s| s.dt > 0.0)) {
            // The frame's water and sand, laid once for its ticks.
            enc.clear_buffer(field);
            enc.compute_memory_barrier_buffers();
            let groups = matter.count().div_ceil(64);
            let across = groups.min(MAX_GROUPS_ACROSS);
            let bindings = binding_list(
                bytemuck::bytes_of(&uniforms),
                buffers,
                Roles::new(last, (last + 1) % RING),
                light,
                lattices,
                &solids,
            );
            pass(
                enc,
                pipelines
                    .matter
                    .as_ref()
                    .expect("solids build their kernels"),
                &bindings,
                [across, groups.div_ceil(across), 1],
                "node.gas_volume.matter",
            );
        }
        for &step in self.steps.iter().filter(|step| step.dt > 0.0) {
            uniforms = GasUniforms::new(
                geometry,
                settings,
                self.sim_time as f32,
                lattice,
                step,
                &matter,
                moving,
            );
            let uniform_bytes = bytemuck::bytes_of(&uniforms);
            let (a, b) = ((last + 1) % RING, (last + 2) % RING);
            let advection = binding_list(
                uniform_bytes,
                buffers,
                Roles { input: last, a, b },
                light,
                lattices,
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
            let working = binding_list(
                uniform_bytes,
                buffers,
                Roles::new(last, b),
                light,
                lattices,
                &solids,
            );
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
            self.sim_time += f64::from(step.dt);
        }

        // The published fields and this frame's light, from the result.
        let working = binding_list(
            bytemuck::bytes_of(&uniforms),
            buffers,
            Roles::new(self.current, last),
            light,
            lattices,
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

        self.current = last;
        self.light_slots[write_slot] = Some(LightSlot {
            stamp: clock.as_ref().map_or(0, |clock| clock.stamp()),
            origin: geometry.origin,
        });
        self.frames += 1;
        Ok(())
    }

    /// Plan the frame's ticks and encode them. On an error the gas holds and
    /// the frame's hits are discarded with a receipt, never replayed.
    fn frame(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        geometry: &GasGeometry,
        settings: &GasSettings,
    ) -> Result<(), String> {
        let restarted = self.plan_ticks(ctx, geometry, settings)?;
        let offline = ctx.sim_step.offline();
        let matter = Matter::read(ctx);
        self.advance(
            ctx.gpu_encoder(),
            geometry,
            settings,
            restarted,
            offline,
            matter,
        )?;
        // A hit is applied once its tick's lattice is on the GPU.
        self.hits.commit_frame();
        Ok(())
    }
}

impl TickSampled for GasVolume {
    fn request_physics_samples(
        &mut self,
        from: PlayedTime,
        until: PlayedTime,
        out: &mut Vec<PlayedTime>,
    ) {
        self.forces.request_samples(&self.clock, from, until, out);
    }
}

inventory::submit! { TickSampledRegistration::new::<GasVolume>() }

impl TakesHits for GasVolume {
    fn hit_epoch(&self) -> Option<u64> {
        self.hits.epoch()
    }

    /// A gas whose clock has not started makes a hit wait; a broken clock
    /// refuses it.
    fn hit_readiness(&self) -> Result<bool, String> {
        match self.hits.failure() {
            Some(error) => Err(error.into()),
            None => Ok(self.hits.epoch().is_some()),
        }
    }

    /// A hit is stamped at the fire's own simulated time, so it lands on the
    /// tick whose interval holds that time, the same tick at any frame rate.
    fn hit_stamp(
        &self,
        played: PlayedTime,
        sequence: u64,
        step: SimStep,
    ) -> Result<EventStamp, String> {
        if !self.hit_readiness()? {
            return Err(
                "Smoke and Fire impulses: the gas has not started; fire waits for it".into(),
            );
        }
        let mut stamp = self.hits.stamp(played, sequence)?;
        let load = step.load().map(|load| LiveLoad {
            previous: load.previous,
            budget: load.budget,
        });
        stamp.time = Seconds(self.clock.simulation_at(played, load));
        Ok(stamp)
    }

    fn enqueue_hit(&mut self, stamp: EventStamp, field: FieldValue) -> Result<TickStamp, String> {
        self.hits.enqueue(stamp, field)
    }

    fn drain_applied_hits(&mut self, consume: &mut dyn FnMut(AppliedEvent<FieldValue>)) {
        self.hits.drain_applied(consume);
    }

    fn drain_discarded_hits(&mut self, consume: &mut dyn FnMut(EventStamp)) -> u64 {
        self.hits.drain_discarded(consume)
    }
}

inventory::submit! { TakesHitsRegistration::new::<GasVolume>() }

/// A gas owns no rigid world: it takes the scene's solids from the water
/// that does, or from the World.
impl SharesSolids for GasVolume {
    fn rigid_owner(&self) -> Option<&LiquidRigidOwner> {
        None
    }

    fn shared_solids_mut(&mut self) -> &mut SharedSolids {
        &mut self.solids
    }
}

impl Primitive for GasVolume {
    // The water and sand are last frame's: a gas runs right after the water
    // or sand that owns the World hands it the solids, before this frame's
    // ticks, so this frame's particles would close a loop.
    fn state_capture_input_ports(&self) -> &[&str] {
        &["water", "water_count", "sand", "sand_count"]
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "state" | "velocity")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        let buffers = self.buffers.as_ref()?;
        match port {
            "state" => Some(&buffers.fine[self.current]),
            "velocity" => Some(&buffers.velocity_out),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, sized to the box at run time.
        matches!(port, "state" | "velocity").then_some(1)
    }

    fn clear_state(&mut self) {
        self.clock.restart();
    }

    fn acceleration_input(&self) -> Option<&'static str> {
        Some("acceleration_field")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.clock
            .set_live_load(ctx.sim_step.load().map(|load| LiveLoad {
                previous: load.previous,
                budget: load.budget,
            }));
        self.clock.set_played_target(ctx.sim_step.played_target);
        // A physics sample reads the field at a tick's start; it never
        // advances time.
        if ctx.sim_step.authored_sample_only {
            self.clock
                .observe_speed(ctx.played(), ctx.scalar_or_param("speed", 1.0));
            match felt_field(ctx, "Gas Volume") {
                Ok(field) => self
                    .forces
                    .observe_sample(&self.clock, ctx.played(), field.as_ref()),
                Err(error) => ctx.error(error),
            }
            return;
        }
        let outcome = plan(ctx).and_then(|(geometry, settings)| {
            self.frame(ctx, &geometry, &settings).map(|()| geometry)
        });
        match outcome {
            Ok(geometry) => publish(ctx, &geometry, &self.light),
            Err(error) => {
                self.hits.abandon_frame();
                ctx.error(error);
                publish(ctx, &GasGeometry::default(), &LightReadout::default());
            }
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
                "cs_matter",
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
            water_count,
            sand_count,
            pad4,
            moving,
        );
        assert_eq!(std::mem::size_of::<GasUniforms>(), 224);
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
        assert_eq!(settings.speed, 1.0);
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
    fn light_sums_decode_to_the_emission_centre_and_temperature() {
        let encode = |value: f64| (value * f64::from(LIGHT_FIXED_SCALE)).round() as u32;
        // Three workgroups' (emission, centre relative to the origin,
        // temperature), summed as the shader's atomics do.
        let groups = [
            (0.5_f64, [1.0_f64, 2.0, 0.5], 0.9_f64),
            (0.25, [1.5, 2.5, 1.0], 0.5),
            (0.125, [0.5, 1.0, 0.25], 1.0),
        ];
        let mut words = [0u32; LIGHT_WORDS];
        for (power, centre, temperature) in groups {
            words[0] = words[0].wrapping_add(encode(power));
            for (word, c) in words[1..4].iter_mut().zip(centre) {
                *word = word.wrapping_add(encode(power * c));
            }
            words[4] = words[4].wrapping_add(encode(power * temperature));
        }
        // A box whose corner is at a negative position, so the centre comes
        // out negative on x.
        let origin = [-3.5_f32, 0.25, -1.0];
        let light = decode_light(&words, origin);
        let power: f64 = groups.iter().map(|g| g.0).sum();
        assert!((f64::from(light.power) - power).abs() < 2.0 / 16384.0);
        for (axis, (&o, &got)) in origin.iter().zip(&light.position).enumerate() {
            let expected =
                f64::from(o) + groups.iter().map(|g| g.0 * g.1[axis]).sum::<f64>() / power;
            assert!(
                (f64::from(got) - expected).abs() < 2e-4,
                "axis {axis}: {got} vs {expected}"
            );
        }
        assert!(light.position[0] < 0.0);
        let temperature = groups.iter().map(|g| g.0 * g.2).sum::<f64>() / power;
        assert!((f64::from(light.temperature) - temperature).abs() < 2e-4);
        assert_eq!(
            decode_light(&[0; LIGHT_WORDS], origin),
            LightReadout::default()
        );
    }

    #[test]
    fn the_light_ring_reads_what_was_written_two_frames_ago() {
        for frame in 2..30u64 {
            let (_, read) = ring_slots(frame);
            let (written_then, _) = ring_slots(frame - 2);
            assert_eq!(read, written_then, "frame {frame}");
            assert_ne!(ring_slots(frame).0, read, "frame {frame}");
        }
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
            2 * 16_000 + 3 * 21_296 + 2 * 4_000 + 4 * 64_000 + 3 * 32 + 256 * 16
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

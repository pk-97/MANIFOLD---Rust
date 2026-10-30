//! `node.gas_volume` — the gas solver of Smoke and Fire: an axis-aligned box
//! of cells held on world tiles, advanced on the GPU one tick at a time (see
//! docs/SMOKE_AND_FIRE_MAP.md). The kernels are in `shaders/gas_step.wgsl`.
//!
//! The gas's cells are world cells of its own tile table (`WorldTiles`, the
//! table water and Matter keep theirs in), rebuilt every tick from the
//! source's tiles and every tile that still holds gas, with a ring of one
//! tile round them and clipped to the box: the kernels step only those
//! tiles, so a thin plume in a big box costs its plume.
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
use manifold_node_engine::scene::world_tiles::{
    READ_SIDE, TILE, lattice_origin, pool_len, read_side_as, table_words,
};
use manifold_physics::force_lattice::ForceLattice;
use manifold_water_liquid::bodies::{
    COLLIDER_BVH, LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape,
};
use manifold_water_liquid::fluid_particles::{
    FaceSample, MatterGridNode, SAND_BULK, SAND_BULK_BYTES, SAND_READ,
};
use manifold_water_liquid::grid::LIQUID_FACES;
use manifold_water_liquid::primitives::prefix_scan::open_gate;
use manifold_water_liquid::whitewater::WHITEWATER_COMMON;
use manifold_water_liquid::world_tiles::{
    Allocation, PointCount, Reach, TileClip, WorldTiles, allocated_args_offset, fresh_args_offset,
    held_bytes, run_tiles,
};

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
/// Threads a workgroup of `cs_source_points` and `cs_source_floor`.
const SMALL_GROUP: u32 = 64;
/// The gas's table reaches one tile round every tile with gas or source in
/// it: no face moves more than four cells a step (`cs_forces`), half a tile,
/// so a tile the gas reaches next tick is held this tick.
const GAS_REACH: Reach = Reach {
    ring_max: 1,
    alloc_ring: 1,
    cell_reach: 0,
};

/// The build's points (`world_tiles.wgsl`): the world cells
/// `cs_source_points` lays, one per source tile, at binding 13.
const GAS_POINTS: &str = "@group(0) @binding(13) var<storage, read> wt_gas_points: array<vec4<i32>>;\n\
fn wt_point_live(i: u32) -> bool { return i < wt.point_count; }\n\
fn wt_point_cell(i: u32) -> vec3<i32> { return wt_gas_points[i].xyz; }\n";
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
         const TILE_PAIRS: u32 = {TILE_PAIRS}u;\n\
         const SOLIDS: bool = {solids};\n"
    )
}

/// Red-black sweep pairs one pressure dispatch runs inside each tile
/// (`pressure_tile`): a quarter of the dispatches global sweeps take, each
/// reading and writing the field once.
const TILE_PAIRS: u32 = 4;

/// Fine state cells per air cell along each axis. Smoke, heat, fuel and
/// flame live on the fine grid, carried by the air and burning there, so the
/// flow draws them sharper than the air's cells; only the flow and its
/// pressure, the costly part, stay coarse.
const FINE: u32 = 2;

/// The kernels without solids, then with them.
static SHADERS: LazyLock<[String; 2]> = LazyLock::new(|| {
    [false, true].map(|solids| {
        format!(
            "{}\n{LIQUID_POSE}\n{LIQUID_COLLIDER}\n{COLLIDER_BVH}\n{READ_SIDE}\n{}\n{}\n{SAND_BULK}\n{SAND_READ}\n{LIQUID_FACES}\n{WHITEWATER_COMMON}\n{}",
            shader_constants(solids),
            read_side_as("ws_"),
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

    /// The box's first cell in world cells: cell p spans [p h, (p + 1) h).
    fn first(&self) -> [i32; 3] {
        self.origin.map(|o| (o / self.cell).round() as i32)
    }

    /// The tiles the table may hold: those of the box's cells and of the
    /// cells one past its far sides, where its far faces live.
    fn clip(&self) -> TileClip {
        let first = self.first();
        [
            first.map(|f| f >> 3),
            std::array::from_fn(|axis| (first[axis] + self.dims[axis] as i32) >> 3),
        ]
    }

    /// The table's pool: every tile the clip holds, wherever the box sits,
    /// so the pool never runs short.
    fn pool_tiles(&self) -> u32 {
        self.dims
            .iter()
            .map(|&d| run_tiles(d + 1))
            .product::<u64>()
            .min(u64::from(u32::MAX)) as u32
    }

    /// Records of every pooled field: the pool's cells and the canonical one.
    pub(crate) fn records(&self) -> u64 {
        pool_len(self.pool_tiles())
    }

    /// The fine state grid's cells along each axis.
    pub(crate) fn fine_dims(&self) -> [u32; 3] {
        self.dims.map(|d| d * FINE)
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

/// The source's tiles along each axis that `cs_source_points` samples: the
/// ellipsoid's box a cell wider on each side (`source_radii`), a point
/// every tile and one more for its far end, never more than the box's.
fn source_tiles(geometry: &GasGeometry, source_size: f32) -> [u32; 3] {
    let h = geometry.cell;
    let radii = [
        0.5 * source_size,
        (1.5 * h).max(0.3 * source_size),
        0.5 * source_size,
    ];
    std::array::from_fn(|axis| {
        let span = 2.0 * radii[axis] / h + 3.0;
        ((span / TILE as f32).ceil() as u32 + 1).min(geometry.dims[axis].div_ceil(TILE) + 1)
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

    /// Red plus black pressure sweep pairs per step.
    fn pressure_pairs(self) -> u32 {
        match self {
            Self::Low => 20,
            Self::Medium => 30,
            Self::High => 50,
            Self::ExtraHigh => 80,
        }
    }

    /// Pressure rounds per step, each solving the even tiles and then the
    /// odd ones, TILE_PAIRS pairs a tile: half of `pressure_pairs`, since a
    /// round's tiles read their neighbours' latest values.
    fn pressure_rounds(self) -> u32 {
        self.pressure_pairs().div_ceil(2 * TILE_PAIRS)
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
    source_count: u32,
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
    /// The box's first cell in world cells, the table's pool, the source's
    /// tiles along each axis, and the word of the table's args holding the
    /// held slots' triple's x.
    first: [i32; 3],
    pool_tiles: u32,
    source_tiles: [u32; 3],
    held_word: u32,
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
        let source_tiles = source_tiles(geometry, settings.source_size);
        Self {
            dims: geometry.dims,
            source_count: source_tiles.iter().product(),
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
            first: geometry.first(),
            pool_tiles: geometry.pool_tiles(),
            source_tiles,
            held_word: (allocated_args_offset(GAS_REACH.ring_max) / 4) as u32,
        }
    }
}

// ---- Storage -------------------------------------------------------------

/// Bytes of each kind of buffer for a box of cells, every field a record
/// per pooled cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GasBytes {
    /// A vec4 per record: the averaged state, the curl and a velocity
    /// buffer (three of them).
    vec4: u64,
    /// An f32 per record: pressure and divergence.
    scalar: u64,
    /// Two words per fine cell, FINE³ a record: a fine state buffer (three
    /// of them).
    fine: u64,
    /// The tile table, its build and the kept words.
    table: u64,
}

impl GasBytes {
    pub(crate) fn of(geometry: &GasGeometry) -> Self {
        let records = geometry.records();
        let pool = geometry.pool_tiles();
        Self {
            vec4: records * 16,
            scalar: records * 4,
            fine: records * u64::from(FINE.pow(3)) * 8,
            table: held_bytes(pool, GAS_REACH.ring_max) + u64::from(pool) * 4,
        }
    }

    /// Everything the step holds: the rotating velocity and fine state, the
    /// averaged state, the curl, pressure and divergence, the blackbody table
    /// and the tile table. The published fields are the tick loop's, the
    /// light ring the domain's.
    pub(crate) fn total(&self) -> u64 {
        5 * self.vec4 + 2 * self.scalar + 3 * self.fine + BLACKBODY_BYTES + self.table
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
    /// The table the fields are pooled on, built every stepped tick.
    tiles: WorldTiles,
    /// A word a slot: a cell of its tile still holding gas (`cs_fine_sources`).
    kept: GpuBuffer,
    /// The source's points for the build (`cs_source_points`).
    source_points: GpuBuffer,
    gate: GpuBuffer,
    /// The fine kernels' indirect triple (`cs_fine_args`).
    fine_args: GpuBuffer,
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
        let pool = geometry.pool_tiles();
        let mut tiles = WorldTiles::new(device, pool, GAS_REACH, GAS_POINTS)
            .map_err(|error| refusal(geometry, bytes.table, error))?;
        tiles.prepare(device);
        tiles.prepare_kept(device);
        let kept = make(u64::from(pool) * 4, BufferUse::Upload)?;
        // No groups until the first stepped tick lays them.
        let fine_args = make(24, BufferUse::Upload)?;
        fine_args.zero_fill();
        // SAFETY: a new shared buffer of `pool` words no GPU work has used.
        unsafe { kept.write(0, bytemuck::cast_slice(&vec![u32::MAX; pool as usize])) };
        Ok(Self {
            dims: geometry.dims,
            velocity: [
                make(bytes.vec4, BufferUse::Gpu)?,
                make(bytes.vec4, BufferUse::Gpu)?,
                make(bytes.vec4, BufferUse::Gpu)?,
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
            tiles,
            kept,
            source_points: make(16, BufferUse::Gpu)?,
            gate: open_gate(device),
            fine_args,
        })
    }

    /// Room for `count` source points.
    fn reserve_points(&mut self, device: &GpuDevice, count: u32) -> Result<(), String> {
        let bytes = u64::from(count.max(1)) * 16;
        if self.source_points.size < bytes {
            self.source_points = device.try_create_buffer(bytes, BufferUse::Gpu)?;
        }
        Ok(())
    }

    /// The fields a clear zeroes.
    fn fields(&self) -> impl Iterator<Item = &GpuBuffer> {
        self.velocity
            .iter()
            .chain(&self.fine)
            .chain([&self.state, &self.curl, &self.pressure, &self.divergence])
            .chain(self.solid.as_ref())
    }

    /// Keep the solid field while the scene has solids, admitted against the
    /// World's budget; drop it when it has none. True when it was just made,
    /// so the caller zeroes it before any kernel reads it.
    fn keep_solid(
        &mut self,
        device: &GpuDevice,
        geometry: &GasGeometry,
        wanted: bool,
    ) -> Result<bool, String> {
        if !wanted {
            self.solid = None;
            self.static_written = None;
            return Ok(false);
        }
        if self.solid.is_none() {
            let bytes = geometry.records() * 8;
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
            return Ok(true);
        }
        Ok(false)
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
    pressure_even: GpuComputePipeline,
    pressure_odd: GpuComputePipeline,
    project: GpuComputePipeline,
    clear_sums: GpuComputePipeline,
    publish: GpuComputePipeline,
    publish_velocity: GpuComputePipeline,
    source_points: GpuComputePipeline,
    fine_args: GpuComputePipeline,
    fresh: GpuComputePipeline,
    reset_kept: GpuComputePipeline,
    /// The kernels that write the solid field, built with the solids only.
    source_floor: Option<GpuComputePipeline>,
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
            pressure_even: make("cs_pressure_even"),
            pressure_odd: make("cs_pressure_odd"),
            project: make("cs_project"),
            clear_sums: make("cs_clear_sums"),
            publish: make("cs_publish"),
            publish_velocity: make("cs_publish_velocity"),
            source_points: make("cs_source_points"),
            fine_args: make("cs_fine_args"),
            fresh: make("cs_fresh"),
            reset_kept: make("cs_reset_kept"),
            source_floor: solids.then(|| make("cs_source_floor")),
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
    slots: &'a GpuBuffer,
) -> [GpuBinding<'a>; 37] {
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
        bind(31, buffers.tiles.table()),
        bind(32, slots),
        bind(33, &buffers.kept),
        bind(34, &buffers.source_points),
        bind(35, buffers.tiles.args()),
        bind(36, &buffers.fine_args),
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

/// One dispatch, then a barrier for the next one.
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

/// One dispatch over a list of the table's slots from the triple at `args.1`
/// of `args.0` (the table's, 512 threads a slot, or the fine kernels', 512
/// SUBS); then a barrier.
fn list(
    enc: &mut manifold_gpu::GpuEncoder,
    pipeline: &GpuComputePipeline,
    bindings: &[GpuBinding<'_>],
    args: (&GpuBuffer, u64),
    label: &str,
) {
    enc.dispatch_compute_indirect(pipeline, bindings, args.0, args.1, label);
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
        let fine = fine_field_bytes(geometry.fine_dims());
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
    /// clock restarted; on a stepped tick build the table, clear the slots it
    /// handed out, lay the solids and step the fields; and on the frame's
    /// last tick publish them and the light.
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
                "Gas Volume: {} × {} × {} cells on a pool of {} tiles",
                geometry.dims[0],
                geometry.dims[1],
                geometry.dims[2],
                geometry.pool_tiles()
            );
        }
        let with_bodies = tick.solids.is_some();
        let with_solids = with_bodies || matter.any();
        let steps = tick.dt > 0.0;
        let counts = source_tiles(&geometry, settings.source_size);
        let source_count: u32 = counts.iter().product();
        let stored = self.buffers.as_mut().expect("buffers were just allocated");
        let new_solid = stored.keep_solid(device, &geometry, with_solids)?;
        stored
            .reserve_points(device, source_count)
            .map_err(|error| refusal(&geometry, u64::from(source_count) * 16, error))?;
        let clear = resized || restart;
        if clear {
            self.current = 0;
            self.sim_time = 0.0;
            stored.static_written = None;
        }
        // The Collider meshes are read through their trees, so every held
        // cell is laid only when they or the box change, and otherwise only
        // the slots each build hands out; the moving solids, a lattice each,
        // and the water and sand every tick.
        let rewrite_static = steps && with_solids && stored.static_written != Some(static_epoch);
        if steps {
            stored.static_written = with_solids.then_some(static_epoch);
        }
        let solids_move = (with_bodies && bodies_move) || matter.any();
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
        let tiles = &buffers.tiles;
        let held_list = &tiles.lists().by_rank;
        let fresh_list = &tiles.lists().fresh;
        let held = (tiles.args(), allocated_args_offset(GAS_REACH.ring_max));
        let fresh = (tiles.args(), fresh_args_offset(GAS_REACH.ring_max));
        let fine = (&buffers.fine_args, 0);
        let tile_groups = (&buffers.fine_args, 12);
        let enc = &mut *gpu.native_enc;
        let still = Tick { dt: 0.0, ..tick };
        // A new solid field is all air: records outside the held tiles, the
        // canonical one among them, are never written.
        if let (true, Some(solid)) = (new_solid && !clear, &buffers.solid) {
            enc.clear_buffer(solid);
        }

        if clear {
            for field in buffers.fields() {
                enc.clear_buffer(field);
            }
            let uniforms = GasUniforms::new(&geometry, &settings, 0.0, lattice, still, &matter);
            let bindings = binding_list(
                bytemuck::bytes_of(&uniforms),
                buffers,
                Roles::new(0, 1),
                shared,
                &solids,
                held_list,
            );
            pass(
                enc,
                &pipelines.reset_kept,
                &bindings,
                [geometry.pool_tiles().div_ceil(256), 1, 1],
                "node.gas_volume.reset_kept",
            );
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
        if steps {
            let (a, b) = ((last + 1) % RING, (last + 2) % RING);
            let advection = binding_list(
                uniform_bytes,
                buffers,
                Roles { input: last, a, b },
                shared,
                &solids,
                held_list,
            );
            if rewrite_static {
                enc.clear_buffer(&buffers.source_floor);
                pass(
                    enc,
                    pipelines
                        .source_floor
                        .as_ref()
                        .expect("solids build their kernels"),
                    &advection,
                    [geometry.dims[1].div_ceil(SMALL_GROUP), 1, 1],
                    "node.gas_volume.source_floor",
                );
            }
            pass(
                enc,
                &pipelines.source_points,
                &advection,
                [source_count.div_ceil(SMALL_GROUP), 1, 1],
                "node.gas_volume.source_points",
            );
            tiles.encode_allocation(
                enc,
                &Allocation {
                    points: &buffers.source_points,
                    count: PointCount::Slots(source_count),
                    origin: lattice_origin(geometry.cell),
                    inv_cell: 1.0 / geometry.cell,
                    replay_counts: [0; 2],
                    gate: &buffers.gate,
                    clip: Some(geometry.clip()),
                    kept: Some(&buffers.kept),
                    shared: None,
                },
            );
            pass(
                enc,
                &pipelines.fine_args,
                &advection,
                [1, 1, 1],
                "node.gas_volume.fine_args",
            );
            // A slot the build handed out held another tile last tick: its
            // records start clean in last tick's result.
            let handed_out = binding_list(
                uniform_bytes,
                buffers,
                Roles::new(a, last),
                shared,
                &solids,
                fresh_list,
            );
            list(
                enc,
                &pipelines.fresh,
                &handed_out,
                fresh,
                "node.gas_volume.fresh",
            );
            if let Some(solids_static) = &pipelines.solids_static {
                let (bindings, slots) = if rewrite_static {
                    (&advection, held)
                } else {
                    (&handed_out, fresh)
                };
                list(
                    enc,
                    solids_static,
                    bindings,
                    slots,
                    "node.gas_volume.solids_static",
                );
            }
            if solids_move {
                list(
                    enc,
                    pipelines
                        .solids
                        .as_ref()
                        .expect("solids build their kernels"),
                    &advection,
                    held,
                    "node.gas_volume.solids",
                );
            }
            for (pipeline, label, slots) in [
                (&pipelines.advect, "node.gas_volume.advect", held),
                (&pipelines.advect_back, "node.gas_volume.advect_back", held),
                (&pipelines.maccormack, "node.gas_volume.maccormack", held),
                (&pipelines.centres_in, "node.gas_volume.centres_in", held),
                (&pipelines.fine_advect, "node.gas_volume.fine_advect", fine),
                (
                    &pipelines.fine_advect_back,
                    "node.gas_volume.fine_advect_back",
                    fine,
                ),
                (
                    &pipelines.fine_maccormack,
                    "node.gas_volume.fine_maccormack",
                    fine,
                ),
            ] {
                list(enc, pipeline, &advection, slots, label);
            }
            let working = binding_list(
                uniform_bytes,
                buffers,
                Roles::new(last, b),
                shared,
                &solids,
                held_list,
            );
            // Swirl at 0 confines nothing: its two passes don't run.
            let swirl = settings.vorticity > 0.0;
            for (pipeline, label, slots, runs) in [
                (
                    &pipelines.fine_sources,
                    "node.gas_volume.fine_sources",
                    fine,
                    true,
                ),
                (&pipelines.restrict, "node.gas_volume.restrict", held, true),
                (&pipelines.forces, "node.gas_volume.forces", held, true),
                (&pipelines.curl, "node.gas_volume.curl", held, swirl),
                (&pipelines.confine, "node.gas_volume.confine", held, swirl),
                (
                    &pipelines.divergence,
                    "node.gas_volume.divergence",
                    held,
                    true,
                ),
            ] {
                if runs {
                    list(enc, pipeline, &working, slots, label);
                }
            }
            for _ in 0..settings.quality.pressure_rounds() {
                for pipeline in [&pipelines.pressure_even, &pipelines.pressure_odd] {
                    list(
                        enc,
                        pipeline,
                        &working,
                        tile_groups,
                        "node.gas_volume.pressure",
                    );
                }
            }
            list(
                enc,
                &pipelines.project,
                &working,
                held,
                "node.gas_volume.project",
            );
            if let Some(fill) = &pipelines.fill_solids {
                list(enc, fill, &working, fine, "node.gas_volume.fill_solids");
            }
            last = b;
            self.sim_time += f64::from(tick.dt);
        }
        self.current = last;
        if !tick.last {
            return Ok(());
        }

        // The published fields and this frame's light, from the result. A
        // cell of a tile the table does not hold publishes still, clear air.
        uniforms.time = self.sim_time as f32;
        let working = binding_list(
            bytemuck::bytes_of(&uniforms),
            buffers,
            Roles::new((last + 1) % RING, last),
            shared,
            &solids,
            held_list,
        );
        enc.clear_buffer(state);
        enc.clear_buffer(velocity);
        pass(
            enc,
            &pipelines.clear_sums,
            &working,
            [1, 1, 1],
            "node.gas_volume.clear_sums",
        );
        for (pipeline, label, slots) in [
            (&pipelines.publish, "node.gas_volume.publish", held),
            (&pipelines.centres_a, "node.gas_volume.centres_a", held),
            (
                &pipelines.publish_velocity,
                "node.gas_volume.publish_velocity",
                fine,
            ),
        ] {
            list(enc, pipeline, &working, slots, label);
        }
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
                "cs_pressure_even",
                "cs_pressure_odd",
                "cs_project",
                "cs_clear_sums",
                "cs_publish",
                "cs_publish_velocity",
                "cs_source_points",
                "cs_fine_args",
                "cs_fresh",
                "cs_reset_kept",
                "cs_source_floor",
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
            source_count,
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
            first,
            pool_tiles,
            source_tiles,
            held_word,
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
        // Half as many pairs a tile, rounded up to whole rounds.
        let rounds = [0.0, 1.0, 2.0, 3.0].map(|q| Quality::from_param(q).pressure_rounds());
        assert_eq!(rounds, [3, 4, 7, 10]);
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
        // 11 cells along each axis (the far faces' cells) touch at most
        // three tiles wherever they start.
        assert_eq!(geometry.pool_tiles(), 27);
        let records = 27 * 512 + 1;
        let bytes = GasBytes::of(&geometry);
        assert_eq!(bytes.vec4, records * 16);
        assert_eq!(bytes.fine, records * 8 * 8);
        assert_eq!(
            bytes.total(),
            5 * records * 16 + 2 * records * 4 + 3 * records * 64 + 256 * 16 + bytes.table
        );
    }

    #[test]
    fn the_clip_holds_the_box_and_its_far_faces() {
        let geometry = GasGeometry {
            cell: 0.1,
            origin: [-1.0, 0.0, 0.7],
            dims: [20, 8, 9],
        };
        assert_eq!(geometry.first(), [-10, 0, 7]);
        // Cells -10..=10, 0..=8 and 7..=16 lie in tiles -2..=1, 0..=1 and
        // 0..=2.
        assert_eq!(geometry.clip(), [[-2, 0, 0], [1, 1, 2]]);
    }

    #[test]
    fn the_source_points_reach_every_tile_the_source_touches() {
        let geometry = GasGeometry {
            cell: 0.05,
            origin: [0.0; 3],
            dims: [64, 96, 64],
        };
        // 0.6 m across is 12 cells, 15 with a cell each side and the
        // floor's rounding: two tiles and one more for the far end.
        assert_eq!(source_tiles(&geometry, 0.6), [3, 3, 3]);
        // A source wider than the box is held to the box's tiles.
        let narrow = GasGeometry {
            dims: [8, 96, 8],
            ..geometry
        };
        assert_eq!(source_tiles(&narrow, 6.0), [2, 11, 2]);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

//! `node.gas_volume` — the gas solver of Smoke and Fire: a dense, axis-aligned
//! box of cells advanced once a frame on the GPU (see
//! docs/SMOKE_FIRE_DESIGN.md). The kernels are in `shaders/gas_step.wgsl`.
//!
//! The box floor is a wall; the sides and top are open. The fields stay on
//! the GPU. The node publishes the packed state and velocity of every cell as
//! provided arrays, and reads the flames' light back from a ring of small
//! buffers, two frames late.

use std::borrow::Cow;
use std::fmt::Display;
use std::sync::LazyLock;

use manifold_core::fluid_domain::{CELL_SIZE_METRES, DEFAULT_CELL_SIZE};
use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::{Primitive, PrimitiveSpec};
use manifold_node_engine::scene::blackbody::{
    BLACKBODY_ENTRIES, BLACKBODY_MAX_KELVIN, BLACKBODY_MIN_KELVIN, blackbody_table,
};
use manifold_node_engine::scene::transform::{Transform, transform_point};

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
/// stay the only copy.
fn shader_constants() -> String {
    format!(
        "const BLACKBODY_ENTRIES: u32 = {BLACKBODY_ENTRIES}u;\n\
         const BLACKBODY_MIN_KELVIN: f32 = {BLACKBODY_MIN_KELVIN:?};\n\
         const BLACKBODY_MAX_KELVIN: f32 = {BLACKBODY_MAX_KELVIN:?};\n\
         const LIGHT_FIXED_SCALE: f32 = {LIGHT_FIXED_SCALE:?};\n\
         const LIGHT_WORDS: u32 = {LIGHT_WORDS}u;\n"
    )
}

static SHADER: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}\n{}",
        shader_constants(),
        include_str!("shaders/gas_step.wgsl")
    )
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

    /// The scalar outputs this geometry fixes, by name.
    fn outputs(&self) -> [(&'static str, f32); 7] {
        let [x, y, z] = self.origin;
        let [nx, ny, nz] = self.dims;
        [
            ("cell_size", self.cell),
            ("origin_x", x),
            ("origin_y", y),
            ("origin_z", z),
            ("dims_x", nx as f32),
            ("dims_y", ny as f32),
            ("dims_z", nz as f32),
        ]
    }
}

/// The box a Transform places: its centre carried through the parent frame,
/// each world side the extent of the box as the frame turns and scales it. The
/// box stays axis-aligned: the solver takes no rotation.
fn world_box(domain: Transform) -> ([f32; 3], [f32; 3]) {
    let centre = transform_point(&domain.parent, domain.pos);
    let size = std::array::from_fn(|world| {
        (0..3)
            .map(|local| domain.parent[local][world].abs() * domain.scale[local])
            .sum::<f32>()
    });
    (centre, size)
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
    let (centre, size) = world_box(domain);
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

/// This frame's step in seconds: the frame time times speed, never past
/// [`MAX_STEP_SECONDS`]. Zero steps nothing.
fn step_seconds(frame_delta: f32, speed: f32) -> f32 {
    (frame_delta * speed).clamp(0.0, MAX_STEP_SECONDS)
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
}

impl GasUniforms {
    fn new(geometry: &GasGeometry, settings: &GasSettings, dt: f32, time: f32) -> Self {
        let (up, g) = up_and_gravity(settings.gravity);
        Self {
            dims: geometry.dims,
            cells: geometry.cells() as u32,
            origin: geometry.origin,
            h: geometry.cell,
            up,
            g,
            dt,
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
        }
    }
}

// ---- Storage -------------------------------------------------------------

/// Bytes of each kind of buffer for a box of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GasBytes {
    /// A vec4 per cell: a velocity or state buffer (three of each), the curl.
    vec4: u64,
    /// An f32 per cell: pressure and divergence.
    scalar: u64,
    /// Two words per cell: the published state and velocity.
    packed: u64,
}

impl GasBytes {
    fn of(cells: u64) -> Self {
        Self {
            vec4: cells * 16,
            scalar: cells * 4,
            packed: cells * 8,
        }
    }

    /// Everything the node holds: the rotating fields, the curl, pressure and
    /// divergence, both published arrays, the light ring and the blackbody
    /// table.
    fn total(&self) -> u64 {
        7 * self.vec4
            + 2 * self.scalar
            + 2 * self.packed
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
    velocity: [GpuBuffer; RING],
    state: [GpuBuffer; RING],
    curl: GpuBuffer,
    /// Kept across frames: the solve starts from the last frame's pressure.
    pressure: GpuBuffer,
    divergence: GpuBuffer,
    state_out: GpuBuffer,
    velocity_out: GpuBuffer,
    lights: [GpuBuffer; RING],
    blackbody: GpuBuffer,
}

impl GasBuffers {
    /// Allocate the box's buffers, admitted against the World's memory budget
    /// as a whole first. The contents are not zeroed.
    fn allocate(device: &GpuDevice, geometry: &GasGeometry) -> Result<Self, String> {
        let bytes = GasBytes::of(geometry.cells());
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
                make(bytes.vec4, BufferUse::Gpu)?,
                make(bytes.vec4, BufferUse::Gpu)?,
                make(bytes.vec4, BufferUse::Gpu)?,
            ],
            state: [
                make(bytes.vec4, BufferUse::Gpu)?,
                make(bytes.vec4, BufferUse::Gpu)?,
                make(bytes.vec4, BufferUse::Gpu)?,
            ],
            curl: make(bytes.vec4, BufferUse::Gpu)?,
            pressure: make(bytes.scalar, BufferUse::Gpu)?,
            divergence: make(bytes.scalar, BufferUse::Gpu)?,
            state_out: make(bytes.packed, BufferUse::Gpu)?,
            velocity_out: make(bytes.packed, BufferUse::Gpu)?,
            lights: [
                make(LIGHT_BYTES, BufferUse::Readback)?,
                make(LIGHT_BYTES, BufferUse::Readback)?,
                make(LIGHT_BYTES, BufferUse::Readback)?,
            ],
            blackbody,
        })
    }
}

/// One compute pipeline per kernel of `gas_step.wgsl`.
pub struct Pipelines {
    advect: GpuComputePipeline,
    advect_back: GpuComputePipeline,
    maccormack: GpuComputePipeline,
    sources: GpuComputePipeline,
    curl: GpuComputePipeline,
    confine: GpuComputePipeline,
    divergence: GpuComputePipeline,
    pressure_red: GpuComputePipeline,
    pressure_black: GpuComputePipeline,
    project: GpuComputePipeline,
    clear_sums: GpuComputePipeline,
    publish: GpuComputePipeline,
    clear: GpuComputePipeline,
}

impl Pipelines {
    fn build(device: &GpuDevice) -> Self {
        let make =
            |entry: &str| device.create_compute_pipeline(SHADER.as_str(), entry, "node.gas_volume");
        Self {
            advect: make("cs_advect"),
            advect_back: make("cs_advect_back"),
            maccormack: make("cs_maccormack"),
            sources: make("cs_sources"),
            curl: make("cs_curl"),
            confine: make("cs_confine"),
            divergence: make("cs_divergence"),
            pressure_red: make("cs_pressure_red"),
            pressure_black: make("cs_pressure_black"),
            project: make("cs_project"),
            clear_sums: make("cs_clear_sums"),
            publish: make("cs_publish"),
            clear: make("cs_clear"),
        }
    }
}

/// Which of the three velocity and state buffers play which role in a
/// dispatch (`vel_in`/`state_in`, `vel_a`/`state_a`, `vel_b`/`state_b`).
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
) -> [GpuBinding<'a>; 14] {
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
        bind(2, &buffers.state[roles.input]),
        bind(3, &buffers.velocity[roles.a]),
        bind(4, &buffers.state[roles.a]),
        bind(5, &buffers.velocity[roles.b]),
        bind(6, &buffers.state[roles.b]),
        bind(7, &buffers.curl),
        bind(8, &buffers.pressure),
        bind(9, &buffers.divergence),
        bind(10, &buffers.state_out),
        bind(11, &buffers.velocity_out),
        bind(12, light),
        bind(13, &buffers.blackbody),
    ]
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
    purpose: "Simulate smoke or fire in an axis-aligned box of cells: the domain Transform is the box (its position the centre, its scale the size in metres; rotation is ignored) and cell_metres the cell size. The floor is a wall; the sides and top are open and fade what reaches them. A source at the floor's centre adds smoke density (Kind Smoke) or fuel that ignites past the ignition temperature (Kind Fire); hot gas rises against gravity, swirls, and expands where it burns. It steps once a frame by the frame time times speed, at most 1/20 s; a change of the reset value, a transport restart or a change of the box's cell counts clears it. Publishes state (two words per cell: density and temperature, then fuel and flame, each pair as two halves) and velocity (u and v, then w and 0, as halves) as arrays with cell (x, y, z) at index x + nx (y + ny z); the box's first corner as origin_x, origin_y, origin_z with cell_size and dims_x, dims_y, dims_z; and the flames' light read back two frames late: light_power (the emission sum, 0 for no flame), light_x, light_y, light_z (its centre in the world) and light_temperature (0 ambient, 1 a hot flame).",
    inputs: {
        domain: Transform optional,
        cell_metres: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        speed: ScalarF32 optional,
        reset: ScalarF32 optional,
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
        gas_param!("vorticity", "Swirl", Float, 3.0, 0.0, 20.0),
        gas_param!("turbulence", "Turbulence (m/s²)", Float, 1.0, 0.0, 30.0),
        gas_param!("dissipation", "Smoke Fade (1/s)", Float, 0.15, 0.0, 10.0),
        gas_param!("cooling", "Cooling (1/s)", Float, 0.6, 0.0, 20.0),
        gas_param!("burn_rate", "Burn Rate (1/s)", Float, 6.0, 0.0, 50.0),
        gas_param!("heat_release", "Heat Release", Float, 2.0, 0.0, 10.0),
        gas_param!("soot", "Smoke from Fire", Float, 0.6, 0.0, 5.0),
        gas_param!("expansion", "Expansion (1/s)", Float, 1.5, 0.0, 10.0),
        gas_param!("seed", "Seed", Int, 0.0, 0.0, 16_777_215.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire the Smoke or Fire item's box Transform into domain, and the World's gravity, speed and reset into their inputs. The defaults are Smoke's; Fire sets Kind to 1 and its own rates. A change of cell_metres or of the box's size past a whole cell restarts the gas; moving the box does not. state and velocity hold half-float pairs for the volume renderer; light_* feed the fire's point light.",
    examples: [],
    picker: { label: "Gas Volume", category: Atom },
    summary: "Simulates smoke or fire in a box of cells.",
    category: Particles3D,
    role: Source,
    aliases: ["smoke", "fire", "gas", "flame", "volume simulation"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        buffers: Option<GasBuffers> = None,
        pipelines: Option<Pipelines> = None,
        // Which of the three velocity and state buffers holds the last result.
        current: usize = 0,
        previous_reset: Option<f32> = None,
        // A transport restart asked for a clear on the next frame.
        restart: bool = false,
        // Seconds stepped since the last clear: the source's noise clock.
        sim_time: f64 = 0.0,
        // Frames published since the last clear: the light ring's cursor.
        frames: u64 = 0,
        light_slots: [Option<LightSlot>; RING] = [None; RING],
        light: LightReadout = LightReadout::default(),
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

impl GasVolume {
    /// Encode one frame: reallocate and clear when the box changed or a
    /// restart was asked for, read the light back, step (when `dt` is above
    /// zero) and publish the fields.
    fn advance(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        geometry: &GasGeometry,
        settings: &GasSettings,
        dt: f32,
        restart: bool,
    ) -> Result<(), String> {
        let device = gpu.device;
        if self.pipelines.is_none() {
            self.pipelines = Some(Pipelines::build(device));
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
        let clear = resized || restart;
        if clear {
            self.current = 0;
            self.sim_time = 0.0;
            self.frames = 0;
            self.light_slots = [None; RING];
            self.light = LightReadout::default();
        }
        let buffers = self.buffers.as_ref().expect("buffers were just allocated");
        let pipelines = self.pipelines.as_ref().expect("pipelines were just built");

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

        let uniforms = GasUniforms::new(geometry, settings, dt, self.sim_time as f32);
        let uniform_bytes = bytemuck::bytes_of(&uniforms);
        let groups = geometry.dims.map(|d| d.div_ceil(GROUP_EDGE));
        let light = &buffers.lights[write_slot];
        let enc = &mut *gpu.native_enc;

        if clear {
            // vel_in and state_in are read-only, so two dispatches with the
            // roles rotated reach all three buffers.
            for roles in [Roles::new(2, 0), Roles::new(0, 2)] {
                let bindings = binding_list(uniform_bytes, buffers, roles, light);
                pass(
                    enc,
                    &pipelines.clear,
                    &bindings,
                    groups,
                    "node.gas_volume.clear",
                );
            }
        }

        let last = self.current;
        let mut result = last;
        if dt > 0.0 {
            let (a, b) = ((last + 1) % RING, (last + 2) % RING);
            let advection = binding_list(uniform_bytes, buffers, Roles { input: last, a, b }, light);
            pass(
                enc,
                &pipelines.advect,
                &advection,
                groups,
                "node.gas_volume.advect",
            );
            pass(
                enc,
                &pipelines.advect_back,
                &advection,
                groups,
                "node.gas_volume.advect_back",
            );
            pass(
                enc,
                &pipelines.maccormack,
                &advection,
                groups,
                "node.gas_volume.maccormack",
            );
            result = b;
            let working = binding_list(uniform_bytes, buffers, Roles::new(last, result), light);
            for (pipeline, label) in [
                (&pipelines.sources, "node.gas_volume.sources"),
                (&pipelines.curl, "node.gas_volume.curl"),
                (&pipelines.confine, "node.gas_volume.confine"),
                (&pipelines.divergence, "node.gas_volume.divergence"),
            ] {
                pass(enc, pipeline, &working, groups, label);
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
                groups,
                "node.gas_volume.project",
            );
        }

        // The published fields and this frame's light, from the result.
        let working = binding_list(uniform_bytes, buffers, Roles::new(last, result), light);
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

        self.current = result;
        self.sim_time += f64::from(dt);
        self.light_slots[write_slot] = Some(LightSlot {
            stamp: clock.as_ref().map_or(0, |clock| clock.stamp()),
            origin: geometry.origin,
        });
        self.frames += 1;
        Ok(())
    }
}

impl Primitive for GasVolume {
    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "state" | "velocity")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        let buffers = self.buffers.as_ref()?;
        match port {
            "state" => Some(&buffers.state_out),
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
        self.restart = true;
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (geometry, settings) = match plan(ctx) {
            Ok(planned) => planned,
            Err(error) => {
                ctx.error(error);
                publish(ctx, &GasGeometry::default(), &LightReadout::default());
                return;
            }
        };
        let reset = ctx.scalar_or_param("reset", 0.0);
        let reset_edge = self.previous_reset.is_some_and(|previous| previous != reset);
        self.previous_reset = Some(reset);
        let restart = std::mem::take(&mut self.restart) || reset_edge;
        let dt = step_seconds(ctx.time.delta.0 as f32, settings.speed);
        let outcome = self.advance(ctx.gpu_encoder(), &geometry, &settings, dt, restart);
        match outcome {
            Ok(()) => publish(ctx, &geometry, &self.light),
            Err(error) => {
                ctx.error(error);
                publish(ctx, &GasGeometry::default(), &LightReadout::default());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_shader() -> naga::Module {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("gas shader parses");
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
        let module = valid_shader();
        for entry in [
            "cs_advect",
            "cs_advect_back",
            "cs_maccormack",
            "cs_sources",
            "cs_curl",
            "cs_confine",
            "cs_divergence",
            "cs_pressure_red",
            "cs_pressure_black",
            "cs_project",
            "cs_clear_sums",
            "cs_publish",
            "cs_clear",
        ] {
            assert!(
                module.entry_points.iter().any(|point| point.name == entry),
                "missing {entry}"
            );
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
        );
        assert_eq!(std::mem::size_of::<GasUniforms>(), 128);
        let module = valid_shader();
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
    fn the_box_follows_its_parent_frame() {
        // A parent that doubles everything and moves it 10 m along x.
        let parent = [
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 2.0, 0.0, 0.0],
            [0.0, 0.0, 2.0, 0.0],
            [10.0, 0.0, 0.0, 1.0],
        ];
        let (centre, size) = world_box(Transform {
            pos: [1.0, 0.0, 0.0],
            scale: [2.0, 1.0, 3.0],
            parent,
            ..Transform::default()
        });
        assert_eq!(centre, [12.0, 0.0, 0.0]);
        assert_eq!(size, [4.0, 2.0, 6.0]);
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
        let fire = gas_settings(|name, default| if name == "kind" { 1.0 } else { default }).unwrap();
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
    fn the_step_follows_the_frame_and_speed_up_to_a_twentieth() {
        assert!((step_seconds(1.0 / 60.0, 1.0) - 1.0 / 60.0).abs() < 1e-7);
        assert!((step_seconds(1.0 / 60.0, 2.0) - 1.0 / 30.0).abs() < 1e-7);
        assert_eq!(step_seconds(0.5, 1.0), MAX_STEP_SECONDS);
        assert_eq!(step_seconds(1.0 / 60.0, 0.0), 0.0);
        assert_eq!(step_seconds(-0.1, 1.0), 0.0);
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
        assert_eq!(decode_light(&[0; LIGHT_WORDS], origin), LightReadout::default());
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
        let bytes = GasBytes::of(1000);
        assert_eq!(bytes.vec4, 16_000);
        assert_eq!(bytes.packed, 8_000);
        assert_eq!(
            bytes.total(),
            7 * 16_000 + 2 * 4_000 + 2 * 8_000 + 3 * 32 + 256 * 16
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

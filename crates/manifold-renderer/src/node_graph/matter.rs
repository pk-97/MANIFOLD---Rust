//! MLS-MPM matter (`docs/GPU_MPM_SOLVER_DESIGN.md`): the point and grid
//! records the matter atoms share, the water constants, the lattice, the
//! fixed-point encoding of grid accumulation (D5), the substep rule (D4) and
//! the fixed-tick clock (D8).

use crate::node_graph::channel_names::well_known;
use crate::node_graph::fluid::{FluidDomainLayout, TICK};
use crate::node_graph::ports::{ChannelElementType, ChannelSpec, KnownItem};
use crate::node_graph::transform::Transform;

pub mod bodies;
pub mod coupling;
/// The f64 CPU oracle, compiled for unit tests and the `gpu-proofs` binary.
pub mod look;
#[cfg(any(test, feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod reference;

/// One material point. 80 bytes. Storage order is id order and never changes
/// (D9): the published frame must be id-sorted.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MatterPoint {
    /// Scene metres.
    pub position: [f32; 3],
    /// Birth ordinal within the identity epoch; 0 marks an unused slot.
    pub id: u32,
    /// Metres per second.
    pub velocity: [f32; 3],
    /// J, current over rest volume.
    pub volume_ratio: f32,
    /// Affine velocity C, row 0 (1/s); w = plastic volume ratio (1 when unused).
    pub affine_x: [f32; 4],
    /// C row 1; w = rest volume V0 in cubic metres.
    pub affine_y: [f32; 4],
    /// C row 2; w = 0.
    pub affine_z: [f32; 4],
}

/// Std430: position Vec3F at 0, id at 12, velocity at 16, volume_ratio at 28,
/// the three affine rows at 32, 48, 64. Stride 80.
pub const MATTER_POINT_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POSITION, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::ID, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::VELOCITY, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::VOLUME_RATIO, ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::AFFINE_X, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::AFFINE_Y, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::AFFINE_Z, ty: ChannelElementType::Vec4F },
];

impl KnownItem for MatterPoint {
    const SPECS: &'static [ChannelSpec] = MATTER_POINT_SPECS;
}

/// One resolved grid node. 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MatterGridNode {
    /// Velocity after forces and boundaries; w = mass in kg (0 = empty).
    pub velocity_mass: [f32; 4],
    /// Velocity before forces (momentum / mass), for Liveliness; w = 1 when the
    /// resolve clamped this node's velocity.
    pub velocity_before: [f32; 4],
}

pub const MATTER_GRID_NODE_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::VELOCITY_MASS, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::VELOCITY_BEFORE, ty: ChannelElementType::Vec4F },
];

impl KnownItem for MatterGridNode {
    const SPECS: &'static [ChannelSpec] = MATTER_GRID_NODE_SPECS;
}

/// A collider, source, drain or coupled body during one tick. 128 bytes.
/// The domain uploads one per body per tick of the frame, holding the tick's
/// start pose and its motion over the tick; `node.matter_move_bodies` turns
/// that into the pose at each substep (section 4.1 step 2).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MatterBody {
    /// World position of the shape's origin (a coupled body's centre of
    /// mass); w = 1/m (0 = prescribed).
    pub position_inv_mass: [f32; 4],
    /// Unit quaternion xyzw, body to world.
    pub rotation: [f32; 4],
    /// m/s; w = friction.
    pub linear_velocity: [f32; 4],
    /// World rad/s; w = role (0 collider, 1 fill, 2 inflow, 3 drain).
    pub angular_velocity: [f32; 4],
    /// World inverse inertia rows at tick start (zero when prescribed); the
    /// rows' w carry the predicted external angular acceleration x, y, z.
    pub inv_inertia_x: [f32; 4],
    pub inv_inertia_y: [f32; 4],
    pub inv_inertia_z: [f32; 4],
    /// xyz predicted external linear acceleration; w = shape index, −1 when
    /// the body is disabled.
    pub accel_shape: [f32; 4],
}

pub const MATTER_BODY_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POSITION_INV_MASS, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::ROTATION, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::LINEAR_VELOCITY, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::ANGULAR_VELOCITY, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::INV_INERTIA_X, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::INV_INERTIA_Y, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::INV_INERTIA_Z, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::ACCEL_SHAPE, ty: ChannelElementType::Vec4F },
];

impl KnownItem for MatterBody {
    const SPECS: &'static [ChannelSpec] = MATTER_BODY_SPECS;
}

/// A body-local signed-distance lattice in the domain's atlas. 48 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MatterShape {
    /// Local position of node (0, 0, 0), unscaled; w = spacing in metres.
    pub origin_spacing: [f32; 4],
    pub dims_x: u32,
    pub dims_y: u32,
    pub dims_z: u32,
    /// Index of node (0, 0, 0) in the atlas's half-precision values (even).
    pub atlas_offset: u32,
    /// The role's scale per axis; w = the smallest, which turns a local
    /// distance into a world one that never overstates it.
    pub scale_min: [f32; 4],
}

pub const MATTER_SHAPE_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::ORIGIN_SPACING, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::DIMS_X, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::DIMS_Y, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::DIMS_Z, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::ATLAS_OFFSET, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::SCALE_MIN, ty: ChannelElementType::Vec4F },
];

impl KnownItem for MatterShape {
    const SPECS: &'static [ChannelSpec] = MATTER_SHAPE_SPECS;
}

const _: () = {
    assert!(std::mem::size_of::<MatterPoint>() == 80);
    assert!(std::mem::size_of::<MatterGridNode>() == 32);
    assert!(std::mem::size_of::<MatterBody>() == 128);
    assert!(std::mem::size_of::<MatterShape>() == 48);
};

/// Packs distance values two per word as WGSL's `pack2x16float` does (the
/// first value in the low half), for the shape atlas (D21: storage-only half
/// precision). An odd count pads with +∞, which reads as far outside.
pub fn pack_distance_atlas(values: &[f32], out: &mut Vec<u32>) {
    let half = |v: f32| u32::from(half::f16::from_f32(v).to_bits());
    for pair in values.chunks(2) {
        let high = pair.get(1).copied().unwrap_or(f32::INFINITY);
        out.push(half(pair[0]) | (half(high) << 16));
    }
}

/// A body's pose `t` seconds after its tick-start row: translation along the
/// linear velocity, rotation by the constant angular velocity (the slerp
/// between the tick's end poses). `node.matter_move_bodies` computes the same
/// in f32.
pub fn body_pose_at(body: &MatterBody, t: f32) -> ([f32; 3], [f32; 4]) {
    let p = body.position_inv_mass;
    let v = body.linear_velocity;
    let position = [p[0] + v[0] * t, p[1] + v[1] * t, p[2] + v[2] * t];
    let w = body.angular_velocity;
    let angle = (w[0] * w[0] + w[1] * w[1] + w[2] * w[2]).sqrt() * t;
    let q = body.rotation;
    if angle <= 0.0 {
        return (position, q);
    }
    let scale = (0.5 * angle).sin() / (angle / t);
    let d = [w[0] * scale, w[1] * scale, w[2] * scale, (0.5 * angle).cos()];
    // d ⊗ q: the world-frame rotation applied after the tick-start one.
    let rotation = [
        d[3] * q[0] + d[0] * q[3] + d[1] * q[2] - d[2] * q[1],
        d[3] * q[1] - d[0] * q[2] + d[1] * q[3] + d[2] * q[0],
        d[3] * q[2] + d[0] * q[1] - d[1] * q[0] + d[2] * q[3],
        d[3] * q[3] - d[0] * q[0] - d[1] * q[1] - d[2] * q[2],
    ];
    (position, rotation)
}

/// Accumulator words per grid node: momentum x, y, z, then mass.
pub const ACCUM_WORDS_PER_NODE: u32 = 4;

/// Reaction words per coupled body, written by `node.matter_body_reaction`
/// (grid projection) and `node.grid_to_matter` (point push-out, D30), and
/// read by `node.matter_move_bodies` and the domain (section 5). Each is
/// value·2^24/U with U the tick's momentum unit:
/// [0..3) Σ Δv, the body's velocity change (m/s);
/// [3..6) Σ (s/n)·Δv, s the substep, n the substeps per tick;
/// [6..9) Σ I·Δω/dx·(1/m), the angular impulse times 1/m over dx;
/// [9..12) Σ (s/n)· the same; [12..16) padding.
pub const REACTION_WORDS: u32 = 16;

/// Words of the per-tick stats array `node.matter_stats` writes (D14).
pub const STATS_WORDS: u32 = 16;

/// One tick's statistics, decoded from the stats array (floats stored as
/// bits): non-finite count, clamped nodes, live points, max speed, J range,
/// volume Σ V0·J, max accumulator magnitude, mass, momentum, kinetic,
/// potential and elastic energy, tick index.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MatterTickStats {
    pub nonfinite: u32,
    pub clamped: u32,
    pub live: u32,
    pub max_speed: f32,
    pub min_j: f32,
    pub max_j: f32,
    pub volume: f32,
    pub max_accum: u32,
    pub mass: f32,
    pub momentum: [f32; 3],
    pub kinetic: f32,
    pub potential: f32,
    pub elastic: f32,
    pub tick: u32,
}

impl MatterTickStats {
    pub fn from_words(w: &[u32]) -> Self {
        let f = |i: usize| f32::from_bits(w[i]);
        Self {
            nonfinite: w[0],
            clamped: w[1],
            live: w[2],
            max_speed: f(3),
            min_j: f(4),
            max_j: f(5),
            volume: f(6),
            max_accum: w[7],
            mass: f(8),
            momentum: [f(9), f(10), f(11)],
            kinetic: f(12),
            potential: f(13),
            elastic: f(14),
            tick: w[15],
        }
    }

    /// Kinetic + potential + elastic energy in joules.
    pub fn energy(&self) -> f32 {
        self.kinetic + self.potential + self.elastic
    }
}

/// Fixed-point scale of the mass words, per `m_unit` (D5).
pub const MASS_SCALE: f32 = 65_536.0;

/// Fixed-point scale of the momentum words, per `m_unit·U` with U the
/// [`momentum_unit`] (D5). 2^11 finer than mass relative to U, so a low-mass
/// node still resolves its velocity.
pub const MOMENTUM_SCALE: f32 = 134_217_728.0;

/// D5's momentum unit U: the power of two at or above dx/dt. The host computes
/// it once per frame (`node.matter_domain`) and wires the same value to P2G
/// and the grid update, so the encode and decode scales are exact inverses in
/// f32; dt/dx and dx/dt computed separately are not. Rounding up keeps the
/// headroom bound p ≤ 2^30 at the velocity clamp.
pub fn momentum_unit(cell_size: f32, step_dt: f64) -> f32 {
    let cells_per_second = f64::from(cell_size) / step_dt;
    2f64.powi(cells_per_second.log2().ceil() as i32) as f32
}

/// Whether a wired momentum unit is one [`momentum_unit`] could produce for
/// this lattice and substep: a normal power of two at or above dx/dt (up to
/// the f32 rounding of dx and dt).
pub fn momentum_unit_fits(unit: f32, cell_size: f32, step_dt: f32) -> bool {
    unit.is_normal()
        && unit > 0.0
        && unit.to_bits() & 0x007f_ffff == 0
        && f64::from(unit) >= f64::from(cell_size) / f64::from(step_dt) * (1.0 - 1e-6)
}

/// The integer hash behind D5's unbiased rounding (lowbias32). The P2G
/// kernel repeats it; `matter_to_grid_body_pins_fixed_point_constants` pins the two.
pub fn rounding_hash(v: u32) -> u32 {
    let mut x = v;
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

/// Key of one point's contributions in one substep.
pub fn rounding_point_key(id: u32, tick_index: u32, substep_in_tick: u32) -> u32 {
    rounding_hash(id ^ rounding_hash(tick_index.wrapping_mul(4096).wrapping_add(substep_in_tick)))
}

/// D5's encoding of a scaled contribution `x` into accumulator word `slot`
/// (node·4 + word): floor(x + u), u the 24-bit hash offset, with the carry
/// taken in integers so the result is unbiased at any magnitude.
pub fn encode_fixed(x: f64, point_key: u32, slot: u32) -> i64 {
    let whole = x.floor();
    let fraction = ((x - whole) * 16_777_216.0) as u32;
    let carry = (fraction + (rounding_hash(point_key ^ slot) >> 8)) >> 24;
    whole as i64 + i64::from(carry)
}

/// Stencil base nodes per block axis in D6's block-local P2G; its workgroup
/// tile spans BLOCK_NODES + 2 nodes per axis.
pub const BLOCK_NODES: u32 = 4;

/// Largest J cohesive water keeps (D3): the Cohesion tension κ·λ·J·(J − 1)
/// pulls a stretched point back; a point stretched to twice its rest volume
/// is torn, and the cap keeps λ·J·(J − 1) finite. Tension-free water
/// (Cohesion 0) keeps J ≤ 1.
pub const COHESIVE_J_MAX: f32 = 2.0;

/// The largest J a point keeps after each update, for a Cohesion (D3).
pub fn j_max(cohesion: f32) -> f32 {
    if cohesion <= 0.0 { 1.0 } else { COHESIVE_J_MAX }
}

/// Rest density of water, kg/m³ (taichi_elements `p_rho`).
pub const WATER_DENSITY: f32 = 1000.0;

/// Mass unit of the accumulators: `1000 · dx³ / 8` kg (D5), so a full node of
/// water sums to about 8 units.
pub fn mass_unit(cell_size: f32) -> f32 {
    125.0 * cell_size * cell_size * cell_size
}

/// Young's modulus per metre of the longest domain side and Poisson's ratio
/// (taichi_elements: `E = 1e6 · size`, `nu = 0.2`).
const YOUNG_PER_METRE: f64 = 1.0e6;
const POISSON_RATIO: f64 = 0.2;

/// Water's bulk term λ in Pa at `stiffness` (D3: λ scales by s²).
pub fn water_lambda(longest_side_m: f64, stiffness: f64) -> f64 {
    let e = YOUNG_PER_METRE * longest_side_m;
    let lambda0 = e * POISSON_RATIO / ((1.0 + POISSON_RATIO) * (1.0 - 2.0 * POISSON_RATIO));
    lambda0 * stiffness * stiffness
}

/// Acoustic wave speed `√(λ/ρ)` in m/s.
pub fn wave_speed(lambda: f64, density: f64) -> f64 {
    (lambda / density).sqrt()
}

/// Acoustic CFL (taichi_elements' default dt at L = 1 gives c·dt/dx = 0.33).
pub const ACOUSTIC_CFL: f64 = 1.0 / 3.0;
/// The substep cap (D4).
pub const MAX_SUBSTEPS: u32 = 128;
/// Grid velocity clamp per component, in cells per substep
/// (taichi_elements `g2p2g_allowed_cfl`).
pub const VELOCITY_CLAMP_CFL: f32 = 0.9;
/// Gravity used for the free-fall speed estimate. Live gravity is not used:
/// changing it must not change the substep count (D4).
const ESTIMATE_GRAVITY: f64 = 9.81;

/// Free-fall speed from rest over `height` metres, the D4 `v_est` floor.
pub fn free_fall_speed(height_m: f64) -> f64 {
    (2.0 * ESTIMATE_GRAVITY * height_m).sqrt()
}

/// Substeps per 1/60 s tick (D4): the acoustic CFL limit plus optional
/// body-coupling and viscous limits (seconds). Uncapped; the caller fits the
/// dials to [`MAX_SUBSTEPS`] with [`stiffness_fitting_cap`].
pub fn substeps_per_tick(
    dx: f32,
    wave_speed: f32,
    v_est: f32,
    body_limit: Option<f32>,
    viscous_limit: Option<f32>,
) -> u32 {
    let acoustic = ACOUSTIC_CFL * f64::from(dx) / (f64::from(wave_speed) + f64::from(v_est));
    let dt = [body_limit, viscous_limit]
        .into_iter()
        .flatten()
        .map(f64::from)
        .fold(acoustic, f64::min);
    ((TICK / dt) - 1e-9).ceil().max(1.0) as u32
}

/// The largest Stiffness whose substep count fits [`MAX_SUBSTEPS`] (D4:
/// limited, never silently). `unit_wave_speed` is the wave speed at
/// Stiffness 1.
pub fn stiffness_fitting_cap(dx: f64, unit_wave_speed: f64, v_est: f64) -> f64 {
    let max_wave = ACOUSTIC_CFL * dx * f64::from(MAX_SUBSTEPS) / TICK - v_est;
    // A hair inside the cap, so f32 rounding of the wave speed downstream
    // cannot tip the count to one past it.
    (max_wave / unit_wave_speed * (1.0 - 1e-5)).max(0.0)
}

/// Nodes added outside the authored box on every side (taichi `padding = 3`).
pub const PADDING_NODES: u32 = 3;

/// The matter lattice: the domain layout's box grown by [`PADDING_NODES`] per
/// side (D5), node (i, j, k) at `min + (i, j, k) · cell_size`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatterLattice {
    pub min: [f32; 3],
    pub nodes: [u32; 3],
    pub cell_size: f32,
    /// Cells of the authored box per axis.
    pub cells: [u32; 3],
}

impl MatterLattice {
    pub fn from_layout(layout: &FluidDomainLayout) -> Self {
        let dx = layout.cell_size as f32;
        let pad = PADDING_NODES as f32 * dx;
        Self {
            min: layout.min.map(|v| v - pad),
            nodes: layout.cells.map(|n| n + 1 + 2 * PADDING_NODES),
            cell_size: dx,
            cells: layout.cells,
        }
    }

    pub fn node_count(&self) -> u32 {
        self.nodes[0] * self.nodes[1] * self.nodes[2]
    }

    /// D6 blocks per axis: 4 stencil base nodes each, covering every base a
    /// point can have (0..=nodes − 3).
    pub fn blocks(&self) -> [u32; 3] {
        self.nodes.map(|n| n.saturating_sub(2).div_ceil(BLOCK_NODES).max(1))
    }

    /// The cell-sort box whose bins are the D6 blocks: a point's bin is its
    /// stencil base node's block, floor((q − 1/2) / 4) with q in cells.
    /// Returns (centre, size, bin size) for `node.sort_particles_into_cells`.
    /// The size stops half a cell short of the last block's far edge, so the
    /// sort's ceil(size / bin) is exactly [`Self::blocks`] despite f32
    /// rounding; points beyond it clamp into the last block.
    pub fn block_sort_box(&self) -> ([f32; 3], [f32; 3], f32) {
        let bin = BLOCK_NODES as f32 * self.cell_size;
        let blocks = self.blocks();
        let size: [f32; 3] = std::array::from_fn(|i| blocks[i] as f32 * bin - 0.5 * self.cell_size);
        let centre = std::array::from_fn(|i| self.min[i] + 0.5 * self.cell_size + 0.5 * size[i]);
        (centre, size, bin)
    }

    /// Scene AABB of the lattice nodes (the seam's `grid_bounds`).
    pub fn bounds(&self) -> Transform {
        let size: [f32; 3] =
            std::array::from_fn(|i| (self.nodes[i] - 1) as f32 * self.cell_size);
        Transform {
            pos: std::array::from_fn(|i| self.min[i] + size[i] * 0.5),
            scale: size,
            ..Transform::default()
        }
    }
}

/// Nodes of a lattice with `nodes` per axis, in u64 so no byte size wraps.
pub fn lattice_nodes(nodes: [u32; 3]) -> u64 {
    nodes.iter().map(|&n| u64::from(n)).product()
}

/// Bytes of the per-node arrays. The atom that allocates each one and every
/// atom that dispatches over it take the size from here, from the same
/// `nodes` wires, so storage always covers the dispatch.
pub fn grid_accum_bytes(nodes: [u32; 3]) -> u64 {
    lattice_nodes(nodes) * u64::from(ACCUM_WORDS_PER_NODE) * 4
}

/// See [`grid_accum_bytes`].
pub fn grid_bytes(nodes: [u32; 3]) -> u64 {
    lattice_nodes(nodes) * std::mem::size_of::<MatterGridNode>() as u64
}

/// See [`grid_accum_bytes`]: one f32 distance per node.
pub fn solid_bytes(nodes: [u32; 3]) -> u64 {
    lattice_nodes(nodes) * 4
}

/// One frame of the D8 clock: how many fixed ticks to run and where the
/// display sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockFrame {
    pub ticks: u32,
    pub epoch: u32,
    /// This frame starts a new simulation (first frame, reset, setup change or
    /// backward seek); the state reseeds before any tick runs.
    pub restarted: bool,
    /// Simulated seconds at the end of this frame's ticks.
    pub simulation_time: f64,
    /// Simulated seconds this display frame reached (at most one tick past
    /// `simulation_time` live); authored controls are sampled here.
    pub target_time: f64,
    /// Display time `s = target − tick` (surface design D10).
    pub display_time: f64,
    /// Simulated time dropped under live overload since the epoch began.
    pub dropped_seconds: f64,
}

/// Most live ticks one display frame may run: 1 at 60 fps, 2 at 30, 3 at 24.
/// A slow frame never earns more than this, so live cannot spiral (D8).
pub const MAX_LIVE_TICKS: u32 = 3;

/// The fixed 60 Hz clock the domain node owns (D8). Transport and Speed build
/// a target time; live runs at most the frame's share of ticks and drops the
/// rest (reported), keeping at most one tick of jitter debt; offline runs
/// every due tick.
#[derive(Clone, Debug, Default)]
pub struct MatterClock {
    /// 0 before the first start, so outputs a domain holds while it waits
    /// (for a role's geometry, say) never share an epoch with the first
    /// simulation, which then seeds.
    epoch: u32,
    started: bool,
    last_transport: f64,
    previous_reset: Option<f32>,
    target_time: f64,
    ticks_done: u64,
    dropped_seconds: f64,
    tick_cap: Option<u32>,
}

impl MatterClock {
    /// Cap the ticks of the frames that follow, live and offline alike: at most
    /// `cap` run, one tick of debt is kept and the rest is dropped, reported.
    /// A coupled domain caps at 0 while its body reaction is pending and at 1
    /// otherwise (section 5). `None` restores the uncapped clock.
    pub fn set_tick_cap(&mut self, cap: Option<u32>) {
        self.tick_cap = cap;
    }

    /// Advance by one display frame. `frame_interval` is this frame's host
    /// delta in seconds; it only sets the live tick allowance.
    pub fn advance(
        &mut self,
        transport: f64,
        frame_interval: f64,
        speed: f32,
        reset: f32,
        setup_changed: bool,
        offline: bool,
    ) -> ClockFrame {
        // A trigger publishes a counter; any change (undo included) resets once.
        let reset_edge = self.previous_reset.is_some_and(|previous| previous != reset);
        self.previous_reset = Some(reset);
        let restarted = !self.started
            || setup_changed
            || reset_edge
            || transport < self.last_transport - 1e-9;
        if restarted {
            self.epoch = self.epoch.wrapping_add(1);
            self.started = true;
            self.target_time = 0.0;
            self.ticks_done = 0;
            self.dropped_seconds = 0.0;
        } else {
            self.target_time += (transport - self.last_transport).max(0.0) * f64::from(speed);
        }
        self.last_transport = transport;
        let due = ((self.target_time / TICK + 1e-9).floor() as u64).saturating_sub(self.ticks_done);
        let ticks = if offline && self.tick_cap.is_none() {
            due
        } else {
            let allowance = match self.tick_cap {
                Some(cap) => u64::from(cap),
                None => ((frame_interval / TICK) - 1e-6).ceil().clamp(1.0, f64::from(MAX_LIVE_TICKS)) as u64,
            };
            let run = due.min(allowance);
            // Keep one tick of scheduling jitter; drop the rest visibly.
            let dropped = due.saturating_sub(run).saturating_sub(1);
            if dropped > 0 {
                let seconds = dropped as f64 * TICK;
                self.target_time -= seconds;
                self.dropped_seconds += seconds;
            }
            run
        };
        self.ticks_done += ticks;
        ClockFrame {
            ticks: ticks as u32,
            epoch: self.epoch,
            restarted,
            simulation_time: self.ticks_done as f64 * TICK,
            target_time: self.target_time,
            display_time: (self.target_time - TICK).max(0.0),
            dropped_seconds: self.dropped_seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::std430_stride;

    #[test]
    fn matter_records_match_their_channel_layouts() {
        assert_eq!(std430_stride(MATTER_POINT_SPECS), 80);
        assert_eq!(std430_stride(MATTER_GRID_NODE_SPECS), 32);
    }

    /// D4's worked numbers at the Dam Break setup: L = H = 4 m, 64 cells,
    /// Stiffness 1 → λ = 1.11e6 Pa, c = 33.3 m/s, v_est = 8.9 m/s, n = 34.
    #[test]
    fn matter_substep_rule_matches_worked_example() {
        let dx = 4.0 / 64.0;
        let lambda = water_lambda(4.0, 1.0);
        assert!((lambda - 1.111e6).abs() < 1.0e3, "{lambda}");
        let c = wave_speed(lambda, 1000.0);
        assert!((c - 33.33).abs() < 0.01, "{c}");
        let v = free_fall_speed(4.0);
        assert!((v - 8.86).abs() < 0.01, "{v}");
        assert_eq!(substeps_per_tick(dx, c as f32, v as f32, None, None), 34);
        let dt = TICK / 34.0;
        assert!((dt - 4.90e-4).abs() < 1.0e-6, "{dt}");
    }

    #[test]
    fn matter_substeps_follow_stiffness() {
        let dx = 4.0f32 / 64.0;
        let v = free_fall_speed(4.0) as f32;
        let n = |s: f64| substeps_per_tick(dx, wave_speed(water_lambda(4.0, s), 1000.0) as f32, v, None, None);
        assert_eq!(n(0.5), 21);
        assert_eq!(n(1.0), 34);
        assert_eq!(n(2.0), 61);
    }

    #[test]
    fn matter_dials_limited_to_substep_cap() {
        // Resolution 256 in a 4 m domain: Stiffness 3 needs more than 128.
        let dx = 4.0f64 / 256.0;
        let v = free_fall_speed(4.0);
        let unit = wave_speed(water_lambda(4.0, 1.0), 1000.0);
        let requested = 3.0;
        let n_requested =
            substeps_per_tick(dx as f32, (unit * requested) as f32, v as f32, None, None);
        assert!(n_requested > MAX_SUBSTEPS, "{n_requested}");
        let fitted = stiffness_fitting_cap(dx, unit, v);
        assert!(fitted < requested);
        let n_fitted = substeps_per_tick(dx as f32, (unit * fitted) as f32, v as f32, None, None);
        assert!(n_fitted <= MAX_SUBSTEPS, "{n_fitted}");
        // The largest fitting value: slightly stiffer needs one more substep.
        let n_above =
            substeps_per_tick(dx as f32, (unit * fitted * 1.001) as f32, v as f32, None, None);
        assert!(n_above > MAX_SUBSTEPS, "{n_above}");
    }

    #[test]
    fn matter_lattice_pads_the_authored_box() {
        let layout = crate::node_graph::fluid::domain_layout(None, 4.0, 64).unwrap();
        let lattice = MatterLattice::from_layout(&layout);
        assert_eq!(lattice.nodes, [71; 3]);
        assert_eq!(lattice.cell_size, 0.0625);
        assert_eq!(lattice.min, [-2.1875, -0.1875, -2.1875]);
        let bounds = lattice.bounds();
        assert_eq!(bounds.scale, [4.375; 3]);
    }

    /// U sits at or above dx/dt, and with it the encode and decode scales
    /// round-trip exactly in f32 for every resolution and substep count.
    #[test]
    fn matter_momentum_unit_round_trips() {
        let tick = crate::node_graph::fluid::TICK;
        assert_eq!(momentum_unit(0.0625, tick / 34.0), 128.0);
        assert_eq!(momentum_unit(1.0, 1.0 / 64.0), 64.0);
        assert!(momentum_unit_fits(128.0, 0.0625, (tick / 34.0) as f32));
        assert!(momentum_unit_fits(256.0, 0.0625, (tick / 34.0) as f32));
        assert!(!momentum_unit_fits(64.0, 0.0625, (tick / 34.0) as f32));
        assert!(!momentum_unit_fits(192.0, 0.0625, (tick / 34.0) as f32));
        assert!(!momentum_unit_fits(0.0, 0.0625, (tick / 34.0) as f32));
        for domain in [0.5f32, 1.0, 4.0, 20.0] {
            for resolution in [8u32, 32, 64, 100, 512] {
                let dx = domain / resolution as f32;
                for substeps in 1..=MAX_SUBSTEPS {
                    let dt = tick / f64::from(substeps);
                    let unit = momentum_unit(dx, dt);
                    assert!(momentum_unit_fits(unit, dx, dt as f32), "dx {dx} substeps {substeps}: {unit}");
                    // The kernels' f32 arithmetic, as written in their WGSL.
                    let inv_mass_unit = 1.0 / mass_unit(dx);
                    let to_mass = MASS_SCALE * inv_mass_unit;
                    let to_momentum = MOMENTUM_SCALE / unit * inv_mass_unit;
                    let to_velocity = unit * (MASS_SCALE / MOMENTUM_SCALE);
                    assert_eq!(to_momentum / to_mass * to_velocity, 1.0, "dx {dx} substeps {substeps}");
                }
            }
        }
    }

    /// A quarter turn about y over one tick: halfway through, the pose is the
    /// slerp midpoint (45°) and the translation the lerp midpoint; the
    /// quaternion stays unit length. The atlas halves round-trip.
    #[test]
    fn matter_body_pose_follows_the_tick() {
        let tick = crate::node_graph::fluid::TICK as f32;
        let turn = std::f32::consts::FRAC_PI_2;
        let body = MatterBody {
            position_inv_mass: [1.0, 2.0, 3.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            linear_velocity: [0.6 / tick, 0.0, -0.3 / tick, 0.5],
            angular_velocity: [0.0, turn / tick, 0.0, 0.0],
            ..MatterBody::default()
        };
        let (position, rotation) = body_pose_at(&body, 0.5 * tick);
        let expected = [0.0, (turn * 0.25).sin(), 0.0, (turn * 0.25).cos()];
        assert!((0..3).all(|i| (position[i] - [1.3, 2.0, 2.85][i]).abs() < 1e-5), "{position:?}");
        assert!((0..4).all(|i| (rotation[i] - expected[i]).abs() < 1e-6), "{rotation:?}");
        let (_, end) = body_pose_at(&body, tick);
        assert!((end.iter().map(|c| c * c).sum::<f32>() - 1.0).abs() < 1e-6);
        assert!((end[1] - (turn * 0.5).sin()).abs() < 1e-6);
        let mut words = Vec::new();
        pack_distance_atlas(&[-0.25, 1.5, 0.125], &mut words);
        let unpack = |w: u32| half::f16::from_bits(w as u16).to_f32();
        assert_eq!((unpack(words[0]), unpack(words[0] >> 16), unpack(words[1])), (-0.25, 1.5, 0.125));
        assert_eq!(unpack(words[1] >> 16), f32::INFINITY);
    }

    /// The sort makes exactly `blocks` bins per axis at every resolution, and
    /// a point's bin (floor((p − box_min) · (1 / bin)), as the sort atom
    /// computes it) is the block of its stencil base node, floor(base / 4).
    #[test]
    fn matter_block_bins_are_base_node_blocks() {
        for domain in [0.5f32, 1.0, 4.0, 20.0] {
            for resolution in [8u32, 32, 63, 64, 100, 128, 512] {
                let layout = crate::node_graph::fluid::domain_layout(None, domain, resolution).unwrap();
                let lattice = MatterLattice::from_layout(&layout);
                let (_, size, bin) = lattice.block_sort_box();
                assert_eq!(
                    crate::node_graph::fluid_particles::bin_counts(size, bin),
                    lattice.blocks(),
                    "domain {domain} resolution {resolution}"
                );
            }
        }
        let layout = crate::node_graph::fluid::domain_layout(None, 4.0, 64).unwrap();
        let lattice = MatterLattice::from_layout(&layout);
        assert_eq!(lattice.blocks(), [18; 3]);
        let (centre, size, bin) = lattice.block_sort_box();
        let dx = lattice.cell_size;
        let mut seed = 0x1234_5678u32;
        for _ in 0..10_000 {
            seed = rounding_hash(seed);
            let q = 1.5 + (seed >> 8) as f32 / 16_777_216.0 * 66.0;
            let p = lattice.min[0] + q * dx;
            let base = (q - 0.5).floor() as i64;
            let sorted = ((p - (centre[0] - 0.5 * size[0])) * (1.0 / bin)).floor() as i64;
            // f32 may put a point within an ulp of a block edge in the
            // neighbour; P2G then adds it globally, still correctly.
            let near_edge = ((q - 0.5) / 4.0 - ((q - 0.5) / 4.0).round()).abs() < 1e-4;
            assert!(near_edge || sorted == base.div_euclid(4), "q {q}: bin {sorted}, base {base}");
        }
    }

    fn run(clock: &mut MatterClock, frames: &[(f64, f64)], offline: bool) -> Vec<ClockFrame> {
        frames
            .iter()
            .map(|&(t, dt)| clock.advance(t, dt, 1.0, 0.0, false, offline))
            .collect()
    }

    #[test]
    fn matter_live_caps_ticks_per_frame() {
        let mut clock = MatterClock::default();
        // 60 fps: one tick per frame after the first.
        let frames: Vec<(f64, f64)> = (0..=10).map(|i| (i as f64 * TICK, TICK)).collect();
        let out = run(&mut clock, &frames, false);
        assert!(out[0].restarted);
        assert!(out[1..].iter().all(|f| f.ticks == 1 && !f.restarted));
        // A one-second stall: the frame runs its allowance, keeps one tick of
        // debt and drops the rest, reported.
        let stalled = clock.advance(10.0 * TICK + 1.0, 1.0, 1.0, 0.0, false, false);
        assert_eq!(stalled.ticks, MAX_LIVE_TICKS);
        let owed = 60u32;
        let dropped_ticks = owed - MAX_LIVE_TICKS - 1;
        assert!((stalled.dropped_seconds - f64::from(dropped_ticks) * TICK).abs() < 1e-9);
        // 30 fps runs two ticks per frame.
        let mut clock = MatterClock::default();
        let frames: Vec<(f64, f64)> = (0..=6).map(|i| (i as f64 * 2.0 * TICK, 2.0 * TICK)).collect();
        let out = run(&mut clock, &frames, false);
        assert!(out[1..].iter().all(|f| f.ticks == 2));
    }

    #[test]
    fn matter_export_runs_every_tick() {
        let mut clock = MatterClock::default();
        clock.advance(0.0, TICK, 1.0, 0.0, false, true);
        let frame = clock.advance(1.0, 1.0, 1.0, 0.0, false, true);
        assert_eq!(frame.ticks, 60);
        assert_eq!(frame.dropped_seconds, 0.0);
    }

    #[test]
    fn matter_clock_pause_reset_speed_and_seek() {
        let mut clock = MatterClock::default();
        clock.advance(0.0, TICK, 1.0, 0.0, false, false);
        let a = clock.advance(TICK, TICK, 1.0, 0.0, false, false);
        assert_eq!(a.ticks, 1);
        // Paused transport holds.
        let held = clock.advance(TICK, 0.0, 1.0, 0.0, false, false);
        assert_eq!(held.ticks, 0);
        assert_eq!(held.simulation_time, a.simulation_time);
        // Speed 0.5 runs a tick every other frame.
        let ticks: u32 = (2..6)
            .map(|i| clock.advance(i as f64 * TICK, TICK, 0.5, 0.0, false, false).ticks)
            .sum();
        assert_eq!(ticks, 2);
        // A changed reset counter restarts in a new epoch.
        let reset = clock.advance(6.0 * TICK, TICK, 1.0, 1.0, false, false);
        assert!(reset.restarted);
        assert_eq!(reset.epoch, 2);
        assert_eq!(reset.ticks, 0);
        // Seeking backwards restarts too.
        let seek = clock.advance(2.0 * TICK, TICK, 1.0, 1.0, false, false);
        assert!(seek.restarted);
        assert_eq!(seek.epoch, 3);
        // Display sits one tick behind the target.
        let next = clock.advance(3.0 * TICK, TICK, 1.0, 1.0, false, false);
        assert!((next.display_time - 0.0).abs() < 1e-12);
        assert_eq!(next.ticks, 1);
    }

    /// A coupled domain's cap: 0 holds without dropping the owed tick, 1 runs
    /// one; offline obeys the cap too and drops beyond one tick of debt.
    #[test]
    fn matter_clock_tick_cap_holds_and_limits() {
        let mut clock = MatterClock::default();
        clock.set_tick_cap(Some(1));
        clock.advance(0.0, TICK, 1.0, 0.0, false, false);
        clock.set_tick_cap(Some(0));
        let held = clock.advance(TICK, TICK, 1.0, 0.0, false, false);
        assert_eq!((held.ticks, held.dropped_seconds), (0, 0.0));
        clock.set_tick_cap(Some(1));
        let caught = clock.advance(2.0 * TICK, TICK, 1.0, 0.0, false, false);
        assert_eq!((caught.ticks, caught.dropped_seconds), (1, 0.0));
        let offline = clock.advance(5.0 * TICK, 3.0 * TICK, 1.0, 0.0, false, true);
        assert_eq!(offline.ticks, 1);
        // Four due (one still owed from the catch-up): one runs, one stays
        // owed, two drop.
        assert!((offline.dropped_seconds - 2.0 * TICK).abs() < 1e-9);
        clock.set_tick_cap(None);
        let uncapped = clock.advance(8.0 * TICK, 3.0 * TICK, 1.0, 0.0, false, true);
        assert_eq!(uncapped.ticks, 4);
    }
}

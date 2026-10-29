//! MLS-MPM matter: the point and grid
//! records the matter atoms share, the sand constants, the block sort of
//! the lattice, the fixed-point encoding of grid accumulation (D5) and the
//! substep rule (D4). The clock, lattice, bodies and rigid owner are every
//! GPU liquid's: `liquid`.

use manifold_core::fluid_domain::FluidDomainLayout;
use manifold_core::liquid_setup::LATTICE_PADDING_CELLS;
use manifold_node_engine::channel_names::well_known;
use manifold_node_engine::ports::{ChannelElementType, ChannelSpec, KnownItem};
use manifold_node_engine::scene::world_tiles::{TILE, TILE_CELLS, pool_len};
use manifold_physics::clock::TICK;
use manifold_water_liquid::world_tiles::{Reach, run_tiles};

pub mod coupling;
#[cfg(all(test, feature = "gpu-proofs"))]
mod material_gpu_tests;

/// One material point. 160 bytes. Storage order is id order and never changes
/// (D9): the published frame must be id-sorted. The material state (F, its
/// plastic history and the stress) rides in the same record, so the sort
/// still permutes indices, never records.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MatterPoint {
    /// Scene metres.
    pub position: [f32; 3],
    /// Birth ordinal within the identity epoch; 0 marks an unused slot.
    pub id: u32,
    /// Metres per second.
    pub velocity: [f32; 3],
    /// J = det F, current over rest volume.
    pub volume_ratio: f32,
    /// Affine velocity C, row 0 (1/s); w = the log volume the grains gained
    /// while pulled apart, paid back before pressure builds.
    pub affine_x: [f32; 4],
    /// C row 1; w = rest volume V0 in cubic metres.
    pub affine_y: [f32; 4],
    /// C row 2; w = the material index (0 sand), kept by every pass.
    pub affine_z: [f32; 4],
    /// Elastic deformation F, row 0; w = accumulated plastic strain.
    pub f_x: [f32; 4],
    /// F row 1; w = 0.
    pub f_y: [f32; 4],
    /// F row 2; w = 0.
    pub f_z: [f32; 4],
    /// Kirchhoff stress τ (Pa): xx, yy, zz; w = 0.
    pub stress_diag: [f32; 4],
    /// τ: xy, xz, yz; w = 0.
    pub stress_off: [f32; 4],
}

/// Std430: position Vec3F at 0, id at 12, velocity at 16, volume_ratio at 28,
/// the three affine rows at 32, 48, 64, the three F rows at 80, 96, 112, the
/// stress at 128 and 144. Stride 160.
pub const MATTER_POINT_SPECS: &[ChannelSpec] = &[
    ChannelSpec {
        name: well_known::POSITION,
        ty: ChannelElementType::Vec3F,
    },
    ChannelSpec {
        name: well_known::ID,
        ty: ChannelElementType::U32,
    },
    ChannelSpec {
        name: well_known::VELOCITY,
        ty: ChannelElementType::Vec3F,
    },
    ChannelSpec {
        name: well_known::VOLUME_RATIO,
        ty: ChannelElementType::F32,
    },
    ChannelSpec {
        name: well_known::AFFINE_X,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::AFFINE_Y,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::AFFINE_Z,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::F_X,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::F_Y,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::F_Z,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::STRESS_DIAG,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::STRESS_OFF,
        ty: ChannelElementType::Vec4F,
    },
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
    /// Velocity before forces (momentum / mass), which node.matter_body_reaction
    /// replays forces from; w = 1 when the resolve clamped this node's velocity.
    pub velocity_before: [f32; 4],
}

pub const MATTER_GRID_NODE_SPECS: &[ChannelSpec] = &[
    ChannelSpec {
        name: well_known::VELOCITY_MASS,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::VELOCITY_BEFORE,
        ty: ChannelElementType::Vec4F,
    },
];

impl KnownItem for MatterGridNode {
    const SPECS: &'static [ChannelSpec] = MATTER_GRID_NODE_SPECS;
}

const _: () = {
    assert!(std::mem::size_of::<MatterPoint>() == 160);
    assert!(std::mem::size_of::<MatterGridNode>() == 32);
};

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
pub const STATS_WORDS: u32 = 17;

/// One tick's statistics, decoded from the stats array (floats stored as
/// bits): non-finite count, clamped nodes, live points, max speed, J range,
/// volume Σ V0·J, max accumulator magnitude, mass, momentum, kinetic,
/// potential and elastic energy, tick index, and the points removed because
/// their tile got no slot in the pool.
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
    /// Points `node.grid_to_matter` removed because their tile had no slot
    /// (the pool ran short), since the epoch began.
    pub tile_removed: u32,
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
            tile_removed: w[16],
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
/// kernel repeats it.
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

/// D5's rounding key of accumulator word `word` (0..4: momentum x, y, z,
/// mass) of world node `node`. It names the node, not its place in the pool,
/// so a run's sums do not depend on where its tiles sit. P2G repeats it
/// (`node_word` in matter_to_grid.wgsl).
pub fn node_word_key(node: [i32; 3], word: u32) -> u32 {
    let u = node.map(|c| c as u32);
    (u[0].wrapping_mul(73_856_093) ^ u[1].wrapping_mul(19_349_663) ^ u[2].wrapping_mul(83_492_791))
        .wrapping_mul(4)
        .wrapping_add(word)
}

/// D5's encoding of a scaled contribution `x` into the accumulator word
/// keyed `slot` ([`node_word_key`]): floor(x + u), u the 24-bit hash offset,
/// with the carry taken in integers so the result is unbiased at any
/// magnitude.
pub fn encode_fixed(x: f64, point_key: u32, slot: u32) -> i64 {
    let whole = x.floor();
    let fraction = ((x - whole) * 16_777_216.0) as u32;
    let carry = (fraction + (rounding_hash(point_key ^ slot) >> 8)) >> 24;
    whole as i64 + i64::from(carry)
}

/// Mass unit of the accumulators: `1000 · dx³ / 8` kg (D5), so a full node of
/// sand at 1600 kg/m³ sums to about 13 units.
pub fn mass_unit(cell_size: f32) -> f32 {
    125.0 * cell_size * cell_size * cell_size
}

/// Sand's Young's modulus (Pa) and Poisson's ratio at Stiffness 1, the values
/// Tampubolon et al. 2017 used. Sand's look comes from its plasticity; the
/// stiffness only has to hold it up without bouncing.
const SAND_YOUNG: f64 = 3.537e5;
const SAND_POISSON: f64 = 0.3;
/// Dry sand's bulk density, kg/m³.
pub const SAND_DENSITY: f32 = 1600.0;
/// The Friction Angle a new Sand starts at, degrees.
pub const SAND_FRICTION_ANGLE: f32 = 35.0;
/// The Friction Angle's range, degrees: below it the cone is too narrow for
/// a cohesion to mean anything, above it sand holds cliffs it never holds.
pub const FRICTION_ANGLE_RANGE: (f32, f32) = (5.0, 60.0);
/// The plastic strain over which soil's cohesion falls to 1/e: slow enough
/// that the settling a fresh fill goes through leaves a cliff its Cohesion
/// holds standing, so a cliff fails when the dial drops, not on its own.
pub const COHESION_SOFTENING: f32 = 0.5;

/// Which rule a material's points follow (`kind` in matter_material.wgsl).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatterRule {
    /// Drucker-Prager with cohesion: sand and soil.
    Granular = 0,
    /// Stomakhin et al. 2013: packs, hardens and crumbles.
    Snow = 1,
    /// Von Mises with a viscous overstress: clay, mud and goo.
    Viscoplastic = 2,
    /// Springs back from any shape: jelly.
    Elastic = 3,
}

/// The materials Matter simulates, in the domain's `material` enum order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatterKind {
    Sand,
    Snow,
    Clay,
    Mud,
    Goo,
    Jelly,
}

/// The domain's `material` enum, in [`MatterKind`] order.
pub const MATTER_KINDS: [&str; 6] = ["sand", "snow", "clay", "mud", "goo", "jelly"];

/// Snow at Stiffness 1: Stomakhin et al. 2013's reference snow (Table 2),
/// E₀ = 1.4e5 Pa, ν = 0.2, hardening ξ = 10, 400 kg/m³.
const SNOW_YOUNG: f64 = 1.4e5;
const SNOW_POISSON: f64 = 0.2;
/// The Hardening a new Snow starts at, ξ.
pub const SNOW_HARDENING: f32 = 10.0;
/// matter_material.wgsl's MM_SNOW_HARDENING_MAX: the most packing stiffens
/// snow, which the substeps budget for.
const SNOW_HARDENING_MAX: f64 = 5.0;
/// Jelly: a gelatin dessert's Young's modulus, about 20 kPa, nearly
/// incompressible (ν = 0.45). A metre-tall block sags under its own weight,
/// as real jelly that size would.
const JELLY_YOUNG: f64 = 2.0e4;
const JELLY_POISSON: f64 = 0.45;
/// Clay: modelling clay yields at about 10 kPa in shear. Its true Young's
/// modulus (about 1 MPa) would double the substeps for a springiness no one
/// sees, so it runs at sand's; the yield, which makes the look, is real.
const CLAY_YOUNG: f64 = 3.0e5;
const CLAY_POISSON: f64 = 0.4;
/// Mud and goo: Bingham fluids, their stiffness only the weak
/// compressibility a fluid solver needs.
const MUD_YOUNG: f64 = 1.0e5;
const MUD_POISSON: f64 = 0.4;
const GOO_YOUNG: f64 = 5.0e4;
const GOO_POISSON: f64 = 0.45;

impl MatterKind {
    pub fn from_index(index: usize) -> Option<Self> {
        [
            Self::Sand,
            Self::Snow,
            Self::Clay,
            Self::Mud,
            Self::Goo,
            Self::Jelly,
        ]
        .get(index)
        .copied()
    }
}

/// The dials that shape a material, each read by the kinds it names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatterDials {
    /// All: E scales by its square, so the wave speed and the substeps scale
    /// by it (D3).
    pub stiffness: f64,
    /// Sand: the Friction Angle, degrees.
    pub friction_angle: f64,
    /// Sand: the Cohesion, kPa.
    pub cohesion_kpa: f64,
    /// Snow: ξ.
    pub hardening: f64,
    /// Clay, mud and goo: the shear yield stress, Pa.
    pub yield_stress: f64,
    /// Clay, mud and goo: the plastic viscosity, Pa·s.
    pub viscosity: f64,
}

/// One material's constants as the material stage takes them
/// (`MatterMaterial` in matter_material.wgsl), less the softening.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatterMaterial {
    pub rule: MatterRule,
    /// Lamé shear modulus μ, Pa.
    pub mu: f64,
    /// Lamé λ, Pa.
    pub lambda: f64,
    /// Drucker-Prager friction α = √(2/3)·2 sin φ / (3 − sin φ).
    pub alpha: f64,
    /// β₀, the tensile log strain intact soil carries.
    pub cohesion: f64,
    /// The Friction Angle φ, radians.
    pub friction_angle: f64,
    /// Viscoplastic: the deviatoric log strain it yields at, τ_y / (√2 μ).
    pub yield_strain: f64,
    /// Viscoplastic: η / μ, s.
    pub relax_time: f64,
    /// Snow: ξ.
    pub hardening: f64,
}

/// Lamé μ and λ of Young's modulus `young` and Poisson's ratio `poisson`.
fn lame(young: f64, poisson: f64) -> (f64, f64) {
    (
        young / (2.0 * (1.0 + poisson)),
        young * poisson / ((1.0 + poisson) * (1.0 - 2.0 * poisson)),
    )
}

impl MatterMaterial {
    /// `kind` shaped by `dials`.
    pub fn of(kind: MatterKind, dials: MatterDials) -> Self {
        let s2 = dials.stiffness * dials.stiffness;
        let elastic = |rule, young: f64, poisson| {
            let (mu, lambda) = lame(young * s2, poisson);
            Self {
                rule,
                mu,
                lambda,
                alpha: 0.0,
                cohesion: 0.0,
                friction_angle: 0.0,
                yield_strain: 0.0,
                relax_time: 0.0,
                hardening: 0.0,
            }
        };
        let flowing = |young, poisson| {
            let m = elastic(MatterRule::Viscoplastic, young, poisson);
            Self {
                yield_strain: dials.yield_stress.max(0.0) / (2f64.sqrt() * m.mu),
                relax_time: dials.viscosity.max(0.0) / m.mu,
                ..m
            }
        };
        match kind {
            MatterKind::Sand => {
                Self::sand(dials.stiffness, dials.friction_angle, dials.cohesion_kpa)
            }
            MatterKind::Snow => Self {
                hardening: dials.hardening.max(0.0),
                ..elastic(MatterRule::Snow, SNOW_YOUNG, SNOW_POISSON)
            },
            MatterKind::Clay => flowing(CLAY_YOUNG, CLAY_POISSON),
            MatterKind::Mud => flowing(MUD_YOUNG, MUD_POISSON),
            MatterKind::Goo => flowing(GOO_YOUNG, GOO_POISSON),
            MatterKind::Jelly => elastic(MatterRule::Elastic, JELLY_YOUNG, JELLY_POISSON),
        }
    }

    /// Sand at `stiffness` (E scales by its square, so the wave speed and the
    /// substeps scale by it, D3), a Friction Angle in degrees and a Cohesion
    /// in kPa. The kPa become β₀ through the shear strength a cohesion adds,
    /// α·(3λ + 2μ)·β₀, so the dial means the same at any Stiffness or
    /// Friction Angle.
    pub fn sand(stiffness: f64, friction_angle: f64, cohesion_kpa: f64) -> Self {
        let (mu, lambda) = lame(SAND_YOUNG * stiffness * stiffness, SAND_POISSON);
        let friction_angle = friction_angle.to_radians();
        let sin = friction_angle.sin();
        let alpha = (2.0f64 / 3.0).sqrt() * 2.0 * sin / (3.0 - sin);
        Self {
            rule: MatterRule::Granular,
            mu,
            lambda,
            alpha,
            cohesion: cohesion_kpa * 1000.0 / (alpha * (3.0 * lambda + 2.0 * mu)),
            friction_angle,
            yield_strain: 0.0,
            relax_time: 0.0,
            hardening: 0.0,
        }
    }

    /// The Bounds walls' Coulomb friction: sand grips a wall as it grips
    /// itself (tan φ), snow and jelly grip moderately, and clay, mud and goo
    /// stick.
    pub fn wall_friction(&self) -> f64 {
        match self.rule {
            MatterRule::Granular => self.friction_angle.tan(),
            MatterRule::Snow | MatterRule::Elastic => 0.5,
            MatterRule::Viscoplastic => 1.0,
        }
    }

    /// The fastest pressure wave, √((λ + 2μ)/ρ) m/s, at the most packing
    /// stiffens snow.
    pub fn wave_speed(&self, density: f64) -> f64 {
        let hardened = if self.rule == MatterRule::Snow {
            SNOW_HARDENING_MAX
        } else {
            1.0
        };
        ((self.lambda + 2.0 * self.mu) * hardened / density).sqrt()
    }
}

/// Acoustic CFL (taichi_elements' default dt at L = 1 gives c·dt/dx = 0.33).
pub const ACOUSTIC_CFL: f64 = 1.0 / 3.0;
/// The substep cap (D4).
pub const MAX_SUBSTEPS: u32 = 128;

/// Duration of one MPM iteration for an observed frame interval.
///
/// Live and export divide the same accepted interval, including when the
/// substep cap stretches work over the whole interval.
pub fn substep_duration(interval_duration: f32, substeps: u32) -> f32 {
    interval_duration / substeps.max(1) as f32
}

/// Use the same duration representation as the GPU before choosing a count.
/// Subtracting f64 transport endpoints can straddle an integer by a few
/// bits even when both intervals encode the same f32 duration.
pub fn substeps_for_interval(duration: f32, nominal: u32) -> u32 {
    (f64::from(duration / TICK as f32) * f64::from(nominal))
        .ceil()
        .max(1.0) as u32
}

/// A point's drawn radius over the cube root of its rest volume: the
/// sphere of that volume (FLIP's marker-radius rule, `node.matter_frame`).
pub const RADIUS_SCALE: f32 = 0.620_350_5;

/// Grid velocity clamp per component, in cells per substep
/// (taichi_elements `g2p2g_allowed_cfl`).
pub const VELOCITY_CLAMP_CFL: f32 = 0.9;

/// How far Matter's world tiles reach: every tile next to a point's tile is
/// held and gets a slot, so a point's stencil (its nearest node and the nodes
/// one away) stays on held tiles while it moves.
pub const MATTER_REACH: Reach = Reach {
    ring_max: 1,
    alloc_ring: 1,
    cell_reach: 1,
};

/// Substeps between builds of Matter's world tiles: `node.matter_to_grid`
/// builds on a substep whose `substep_in_tick` is a multiple of it. The grid
/// clamps velocity to [`VELOCITY_CLAMP_CFL`] cells a substep, so between a
/// build and the last P2G that uses it a point moves at most 0.9 · 5 cells
/// and its stencil reaches one node further: 5.5 cells, inside the one-tile
/// ring with 2.5 to spare for a collider's push-out.
pub const REBUILD_SUBSTEPS: u32 = 6;
const _: () = assert!((REBUILD_SUBSTEPS - 1) as f32 * VELOCITY_CLAMP_CFL + 1.0 + 2.0 < TILE as f32);
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

/// Nodes of a lattice with `nodes` per axis, in u64 so no byte size wraps.
pub fn lattice_nodes(nodes: [u32; 3]) -> u64 {
    nodes.iter().map(|&n| u64::from(n)).product()
}

/// The nodes of Matter's box with its 3-cell margin, which the force lattices
/// and the pool cover.
pub fn matter_nodes(cells: [u32; 3]) -> [u32; 3] {
    cells.map(|n| n + 1 + 2 * LATTICE_PADDING_CELLS)
}

/// The first node of Matter's margin: the box's first corner less the margin's
/// cells. The force lattices and the potential's datum start here.
pub fn field_min(layout: &FluidDomainLayout) -> [f32; 3] {
    let pad = LATTICE_PADDING_CELLS as f32 * layout.cell_size as f32;
    layout.min.map(|v| v - pad)
}

/// Tiles Matter's pool holds for a box with `nodes` per axis
/// ([`matter_nodes`]): every tile its nodes can touch, wherever it sits on the
/// world lattice, and the ring round them; or the whole tiles `budget_nodes`
/// buys, at least one.
pub fn matter_pool_tiles(nodes: [u32; 3], budget_nodes: u64) -> u32 {
    let ring = u64::from(MATTER_REACH.ring_max);
    let wanted: u64 = nodes.iter().map(|&n| run_tiles(n) + 2 * ring).product();
    wanted.min(budget_nodes / u64::from(TILE_CELLS)).max(1) as u32
}

/// The `pool_tiles` an atom over Matter's pooled grid reads, at least one,
/// checked so the accumulator's word index fits 32 bits.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub fn pool_extent(
    x: &manifold_node_engine::exec::extent::AtomExtent<'_>,
) -> Result<u32, manifold_node_engine::exec::extent::Verdict> {
    let pool = manifold_water_liquid::extent::whole(x, "pool_tiles", 1.0).max(1);
    if pool_len(pool) * u64::from(ACCUM_WORDS_PER_NODE) > u64::from(u32::MAX) {
        return Err(x.uncovered(format!(
            "a pool of {pool} tiles overflows the accumulator's 32-bit word index"
        )));
    }
    Ok(pool)
}

/// Bytes of the pooled per-node arrays over a pool of `pool_tiles` slots:
/// [`pool_len`] nodes, the canonical one last. The atom that allocates each
/// one and every atom that dispatches over it take the size from here, from
/// the same `pool_tiles` wire, so storage always covers the dispatch.
pub fn grid_accum_bytes(pool_tiles: u32) -> u64 {
    pool_len(pool_tiles) * u64::from(ACCUM_WORDS_PER_NODE) * 4
}

/// See [`grid_accum_bytes`].
pub fn grid_bytes(pool_tiles: u32) -> u64 {
    pool_len(pool_tiles) * std::mem::size_of::<MatterGridNode>() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Sand item's Matter and the domain's `material` enum count the same
    /// materials in the same order.
    #[test]
    fn scene_matter_follows_the_domain_material_enum() {
        use manifold_core::scene_doc::Matter;
        assert_eq!(Matter::ALL.len(), MATTER_KINDS.len());
        for (index, matter) in Matter::ALL.into_iter().enumerate() {
            assert_eq!(matter.index() as usize, index);
            assert_eq!(matter.name().to_lowercase(), MATTER_KINDS[index]);
            assert!(MatterKind::from_index(index).is_some());
        }
    }
    use manifold_node_engine::ports::std430_stride;
    use manifold_physics::PlayedTime;

    #[test]
    fn matter_export_substeps_ignore_frame_partition_roundoff() {
        use manifold_physics::clock::SimulationClock;
        let run = |stride: u32| {
            let mut clock = SimulationClock::default();
            let mut schedule = Vec::new();
            for frame in 0..=120 / stride {
                let accepted = clock.advance(
                    PlayedTime::from_played(f64::from(frame * stride) * TICK),
                    TICK,
                    1.0,
                    0.0,
                    false,
                    true,
                );
                for i in 0..accepted.ticks {
                    let interval = accepted.interval(u64::from(i)).unwrap().duration().0 as f32;
                    let substeps = substeps_for_interval(interval, 34);
                    let dt = substep_duration(interval, substeps);
                    schedule.push((substeps, dt.to_bits()));
                }
            }
            schedule
        };
        let reference = run(1);
        assert!(reference.iter().all(|&(steps, _)| steps == 34));
        for stride in [2, 3, 4, 6] {
            assert_eq!(run(stride), reference, "export grouping {stride}");
        }
        assert_eq!(substeps_for_interval((2.0 * TICK) as f32, 34), 68);
        assert_eq!(substeps_for_interval((1.5 * TICK) as f32, 34), 51);
    }

    #[test]
    fn matter_records_match_their_channel_layouts() {
        assert_eq!(std430_stride(MATTER_POINT_SPECS), 160);
        assert_eq!(std430_stride(MATTER_GRID_NODE_SPECS), 32);
    }

    /// MATTER_SAND_DESIGN.md section 3's worked numbers: Stiffness 1 gives
    /// μ = 136 kPa and λ = 204 kPa, a 17.3 m/s wave in 1600 kg/m³ sand, and
    /// with a 4 m fall 21 substeps a tick at 6.25 cm cells, 11 at 12.5 cm and
    /// 6 at 25 cm; a 35° Friction Angle gives α = 0.386.
    #[test]
    fn sand_constants_match_the_design() {
        let sand = MatterMaterial::sand(1.0, 35.0, 0.0);
        assert!((sand.mu - 1.360e5).abs() < 100.0, "{}", sand.mu);
        assert!((sand.lambda - 2.040e5).abs() < 100.0, "{}", sand.lambda);
        assert!((sand.alpha - 0.386).abs() < 1e-3, "{}", sand.alpha);
        assert_eq!(sand.cohesion, 0.0);
        let c = sand.wave_speed(f64::from(SAND_DENSITY));
        assert!((c - 17.25).abs() < 0.01, "{c}");
        let v = free_fall_speed(4.0) as f32;
        let n = |dx: f32| substeps_per_tick(dx, c as f32, v, None, None);
        assert_eq!([n(0.0625), n(0.125), n(0.25)], [21, 11, 6]);
    }

    /// One kPa of Cohesion holds the same shear strength whatever the
    /// Stiffness and Friction Angle: α·(3λ + 2μ)·β₀ = 1000 Pa.
    #[test]
    fn cohesion_kpa_is_a_shear_strength() {
        for (stiffness, angle) in [(0.5, 20.0), (1.0, 35.0), (2.5, 50.0)] {
            let soil = MatterMaterial::sand(stiffness, angle, 1.0);
            let strength = soil.alpha * (3.0 * soil.lambda + 2.0 * soil.mu) * soil.cohesion;
            assert!(
                (strength - 1000.0).abs() < 1e-6,
                "{stiffness} {angle}: {strength}"
            );
        }
        // Stiffness scales the wave speed linearly (E by its square).
        let wave = |s: f64| MatterMaterial::sand(s, 35.0, 0.0).wave_speed(1600.0);
        assert!((wave(2.0) / wave(1.0) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn matter_dials_limited_to_substep_cap() {
        // Resolution 256 in a 4 m domain: Stiffness 3 needs more than 128.
        let dx = 4.0f64 / 256.0;
        let v = free_fall_speed(4.0);
        let unit = MatterMaterial::sand(1.0, 35.0, 0.0).wave_speed(f64::from(SAND_DENSITY));
        let requested = 3.0;
        let n_requested =
            substeps_per_tick(dx as f32, (unit * requested) as f32, v as f32, None, None);
        assert!(n_requested > MAX_SUBSTEPS, "{n_requested}");
        let fitted = stiffness_fitting_cap(dx, unit, v);
        assert!(fitted < requested);
        let n_fitted = substeps_per_tick(dx as f32, (unit * fitted) as f32, v as f32, None, None);
        assert!(n_fitted <= MAX_SUBSTEPS, "{n_fitted}");
        // The largest fitting value: slightly stiffer needs one more substep.
        let n_above = substeps_per_tick(
            dx as f32,
            (unit * fitted * 1.001) as f32,
            v as f32,
            None,
            None,
        );
        assert!(n_above > MAX_SUBSTEPS, "{n_above}");
    }

    #[test]
    fn live_mpm_substep_duration_covers_display_rates() {
        for fps in [20.0f32, 24.0, 30.0, 60.0] {
            let interval = 1.0 / fps;
            let duration = substep_duration(interval, 4);
            assert_eq!(duration.to_bits(), (interval / 4.0).to_bits());
            assert!((duration * 4.0 - interval).abs() < 1.0e-6);
        }
    }

    #[test]
    fn live_mpm_substep_cap_still_covers_the_full_interval() {
        let interval = 1.0f32 / 20.0;
        let duration = substep_duration(interval, MAX_SUBSTEPS);
        assert!((duration * MAX_SUBSTEPS as f32 - interval).abs() < 1.0e-6);
    }

    #[test]
    fn export_mpm_substep_duration_uses_the_accepted_project_interval() {
        for interval in [1.0f32 / 24.0, 1.0 / 60.0, 0.25] {
            for substeps in 1..=MAX_SUBSTEPS {
                let expected = interval / substeps as f32;
                assert_eq!(
                    substep_duration(interval, substeps).to_bits(),
                    expected.to_bits(),
                    "substeps={substeps}"
                );
            }
        }
    }

    /// U sits at or above dx/dt, and with it the encode and decode scales
    /// round-trip exactly in f32 for every resolution and substep count.
    #[test]
    fn matter_momentum_unit_round_trips() {
        let tick = manifold_physics::clock::TICK;
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
                    assert!(
                        momentum_unit_fits(unit, dx, dt as f32),
                        "dx {dx} substeps {substeps}: {unit}"
                    );
                    // The kernels' f32 arithmetic, as written in their WGSL.
                    let inv_mass_unit = 1.0 / mass_unit(dx);
                    let to_mass = MASS_SCALE * inv_mass_unit;
                    let to_momentum = MOMENTUM_SCALE / unit * inv_mass_unit;
                    let to_velocity = unit * (MASS_SCALE / MOMENTUM_SCALE);
                    assert_eq!(
                        to_momentum / to_mass * to_velocity,
                        1.0,
                        "dx {dx} substeps {substeps}"
                    );
                }
            }
        }
    }

    /// The pool holds every tile a 64³ box's 71 nodes can touch and the ring
    /// round them (10 + 2 per axis), or what a smaller budget buys.
    #[test]
    fn matter_pool_covers_the_box_and_its_ring_within_the_budget() {
        let layout = manifold_core::fluid_domain::domain_layout(None, 4.0, 0.0625).unwrap();
        let nodes = matter_nodes(layout.cells);
        assert_eq!(nodes, [71; 3]);
        assert_eq!(matter_pool_tiles(nodes, u64::MAX), 12 * 12 * 12);
        assert_eq!(matter_pool_tiles(nodes, 100 * 512 + 7), 100);
        assert_eq!(matter_pool_tiles(nodes, 0), 1);
    }
}

#[cfg(test)]
mod grid_budget_tests {
    use super::{lattice_nodes, matter_nodes};
    use manifold_core::fluid_domain::domain_layout;

    use crate::primitives::matter_domain::admit_lattice;

    /// MPM's lattice arithmetic at every resolution the domain allows: Grid
    /// Budget admits or names the refusal, and the default budget stops at
    /// 193 (200³ nodes).
    #[test]
    fn matter_grid_budget_gates_every_resolution() {
        let mut largest_default = 0;
        for resolution in 8..=512 {
            let layout = domain_layout(None, 4.0, 4.0 / f64::from(resolution)).expect("layout");
            let per_axis = matter_nodes(layout.cells);
            let nodes = lattice_nodes(per_axis);
            for budget in [8.0f32, 512.0] {
                let fits = nodes as f64 <= f64::from(budget) * 1e6;
                match admit_lattice(per_axis, budget) {
                    Ok(()) => assert!(fits, "res {resolution}"),
                    Err(error) => assert!(
                        !fits && error.contains("Grid Budget"),
                        "res {resolution}: {error}"
                    ),
                }
            }
            if admit_lattice(per_axis, 8.0).is_ok() {
                largest_default = resolution;
            }
        }
        assert_eq!(largest_default, 193);
    }
}

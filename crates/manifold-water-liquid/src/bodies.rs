//! A GPU liquid's bodies: the coupled Physics World's bodies become
//! one [`LiquidBody`] row per body per tick, Inflow and Outflow roles one
//! region row per role per tick from their authored motion, and every
//! shape's distance lattice one packed atlas with a [`LiquidShape`] each.
//! Solids reach the water only as coupled bodies, the World's Colliders and
//! Bodies (SCENE_DOCUMENT_DESIGN.md section 7.8 (Physics and collision
//! shapes)). Role motion is the authored roles replayed at each tick's start
//! ([`TickSamples`]), never observed per display frame, so a source moves
//! the same at any frame rate.

use std::sync::{Arc, mpsc};

use manifold_physics::coupled_motion::{MAX_SUPPORT_POINTS, SupportPoint};

use crate::clock::{ClockFrame, LiquidClock};
use crate::fluid_role::{
    FluidRole, FluidRoleKind, LatticePlan, PreparedFluidGeometry, scale_steps,
};
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_core::physics_gizmo::{PhysicsGizmo, PhysicsGizmoFamily, PhysicsGizmoShape};
use manifold_node_engine::channel_names::well_known;
use manifold_node_engine::ports::{ChannelElementType, ChannelSpec, KnownItem};
use manifold_node_engine::scene::transform::Transform;
use manifold_physics::PlayedTime;
use manifold_physics::clock::TICK;
use manifold_physics::sdf::{DistanceLattice, MAX_LATTICE_NODES};
use manifold_physics::tick_samples::TickSamples;
use manifold_water_rigid::physics::pose_from_transform;

/// A body vertex used by both obstacle and initial-source prediction.
/// `velocity` is the current linear velocity (for a source it already
/// includes the source's fluid velocity). `velocity.w` is zero for prescribed
/// geometry and is a coupled body-row index plus one for dynamic hulls.
/// `position.w` is an eligibility bit; noncoupled vertices outside the liquid
/// domain have zero there. Coupled hull vertices stay eligible even when
/// outside, as in the reference engine. Layout mirrors `BodyVertex` in
/// shaders/gpu_flip_clock.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuFlipBodyVertex {
    pub position: [f32; 4],
    pub velocity: [f32; 4],
    pub acceleration: [f32; 4],
    pub angular_velocity: [f32; 4],
    pub angular_acceleration: [f32; 4],
    pub centroid: [f32; 4],
}

manifold_core::testkit_visible! {
/// Body poses on the GPU: rotation, the constant-angular-velocity turn,
/// material velocity at a point. Matches [`body_pose_at`].
pub const LIQUID_POSE: &str = include_str!("primitives/shaders/liquid_pose.wgsl");
}
manifold_core::testkit_visible! {
/// Collider lattice sampling; needs [`LIQUID_POSE`] and the including body's
/// `liquid_atlas_half`.
pub const LIQUID_COLLIDER: &str = include_str!("primitives/shaders/liquid_collider.wgsl");
}

/// A collider, source, drain or coupled body during one tick. 128 bytes.
/// The domain uploads one per body per tick of the frame, holding the tick's
/// start pose and its motion over the tick; the solver turns that into the
/// pose at each substep.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LiquidBody {
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

pub const LIQUID_BODY_SPECS: &[ChannelSpec] = &[
    ChannelSpec {
        name: well_known::POSITION_INV_MASS,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::ROTATION,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::LINEAR_VELOCITY,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::ANGULAR_VELOCITY,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::INV_INERTIA_X,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::INV_INERTIA_Y,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::INV_INERTIA_Z,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::ACCEL_SHAPE,
        ty: ChannelElementType::Vec4F,
    },
];

impl KnownItem for LiquidBody {
    const SPECS: &'static [ChannelSpec] = LIQUID_BODY_SPECS;
}

/// Vec4s a support point takes.
pub const SUPPORT_POINT_VEC4S: usize = 5;

/// Vec4s a body's support points take beside its row.
pub const SUPPORT_VEC4S: usize = SUPPORT_POINT_VEC4S * MAX_SUPPORT_POINTS;

/// Bytes a body's packed 6 × 6 mobility takes on the GPU (`pose_bodies`):
/// six vec4, 21 floats used.
pub const MOBILITY_BYTES: u64 = 96;

/// A coupled body's touching support points at a tick's start
///Five vec4 a point: the lever
/// arm xyz and friction; the normal xyz out of the support and the patch;
/// the support's velocity xyz and its spin about the normal; the patch
/// centre's lever xyz; the support's velocity xyz there. The points run from
/// the first; a zero normal ends them. 1280 bytes, beside the body's row.
pub type BodySupports = [[f32; 4]; SUPPORT_VEC4S];

/// `points` as [`BodySupports`], up to [`MAX_SUPPORT_POINTS`].
#[inline]
pub fn pack_supports(points: &[SupportPoint]) -> BodySupports {
    let mut out = [[0.0; 4]; SUPPORT_VEC4S];
    for (point, slot) in points
        .iter()
        .take(MAX_SUPPORT_POINTS)
        .zip(out.chunks_exact_mut(SUPPORT_POINT_VEC4S))
    {
        let (l, n, v, c, u) = (
            point.lever,
            point.normal,
            point.support_velocity,
            point.patch_lever,
            point.patch_velocity,
        );
        slot.copy_from_slice(&[
            [l[0], l[1], l[2], point.friction],
            [n[0], n[1], n[2], point.patch as f32],
            [v[0], v[1], v[2], point.support_spin],
            [c[0], c[1], c[2], 0.0],
            [u[0], u[1], u[2], 0.0],
        ]);
    }
    out
}

/// The points of `supports` into `out`; returns how many.
#[inline]
pub fn unpack_supports(
    supports: &BodySupports,
    out: &mut [SupportPoint; MAX_SUPPORT_POINTS],
) -> usize {
    let xyz = |v: [f32; 4]| [v[0], v[1], v[2]];
    for (k, (slot, packed)) in out
        .iter_mut()
        .zip(supports.chunks_exact(SUPPORT_POINT_VEC4S))
        .enumerate()
    {
        let [l, n, v, c, u] = [packed[0], packed[1], packed[2], packed[3], packed[4]];
        if n[0] == 0.0 && n[1] == 0.0 && n[2] == 0.0 {
            return k;
        }
        *slot = SupportPoint {
            lever: xyz(l),
            friction: l[3],
            normal: xyz(n),
            patch: n[3] as u32,
            support_velocity: xyz(v),
            support_spin: v[3],
            patch_lever: xyz(c),
            patch_velocity: xyz(u),
        };
    }
    MAX_SUPPORT_POINTS
}

/// A body-local signed-distance lattice in the domain's atlas. 48 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LiquidShape {
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

pub const LIQUID_SHAPE_SPECS: &[ChannelSpec] = &[
    ChannelSpec {
        name: well_known::ORIGIN_SPACING,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::DIMS_X,
        ty: ChannelElementType::U32,
    },
    ChannelSpec {
        name: well_known::DIMS_Y,
        ty: ChannelElementType::U32,
    },
    ChannelSpec {
        name: well_known::DIMS_Z,
        ty: ChannelElementType::U32,
    },
    ChannelSpec {
        name: well_known::ATLAS_OFFSET,
        ty: ChannelElementType::U32,
    },
    ChannelSpec {
        name: well_known::SCALE_MIN,
        ty: ChannelElementType::Vec4F,
    },
];

impl KnownItem for LiquidShape {
    const SPECS: &'static [ChannelSpec] = LIQUID_SHAPE_SPECS;
}

const _: () = {
    assert!(std::mem::size_of::<LiquidBody>() == 128);
    assert!(std::mem::size_of::<LiquidShape>() == 48);
};

/// Packs distance values two per word as WGSL's `pack2x16float` does (the
/// first value in the low half), for the shape atlas (storage-only half
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
/// between the tick's end poses). `liquid_pose.wgsl` computes the same in f32.
#[inline]
pub fn body_pose_at(body: &LiquidBody, t: f32) -> ([f32; 3], [f32; 4]) {
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
    let d = [
        w[0] * scale,
        w[1] * scale,
        w[2] * scale,
        (0.5 * angle).cos(),
    ];
    // d ⊗ q: the world-frame rotation applied after the tick-start one.
    let rotation = [
        d[3] * q[0] + d[0] * q[3] + d[1] * q[2] - d[2] * q[1],
        d[3] * q[1] - d[0] * q[2] + d[1] * q[3] + d[2] * q[0],
        d[3] * q[2] + d[0] * q[1] - d[1] * q[0] + d[2] * q[3],
        d[3] * q[3] - d[0] * q[0] - d[1] * q[1] - d[2] * q[2],
    ];
    (position, rotation)
}

/// A collider thinner than this many cells along any axis can let liquid
/// through between substeps (D11's thinness warning).
pub const THIN_COLLIDER_CELLS: f32 = 2.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Controls {
    transform: Transform,
    enabled: bool,
    friction: f32,
    velocity: [f32; 3],
    inherit_motion: f32,
}

impl Controls {
    fn from_role(role: &FluidRole) -> Self {
        Self {
            transform: role.transform,
            enabled: role.enabled,
            friction: role.friction,
            velocity: role.velocity,
            inherit_motion: role.inherit_motion,
        }
    }
}

/// Region codes in a region row's `angular_velocity.w`.
pub const REGION_INFLOW: f32 = 2.0;
pub const REGION_OUTFLOW: f32 = 3.0;

/// Every role slot's controls at one tick's start.
type Poses = Vec<Controls>;

/// `roles`' controls into `out`, one per slot, reusing its storage.
fn fill_controls(roles: &[Option<FluidRole>], out: &mut Poses) {
    out.clear();
    out.extend(roles.iter().map(|role| {
        role.as_ref()
            .map_or_else(Controls::default, Controls::from_role)
    }));
}

struct BodyRole {
    slot: usize,
    kind: FluidRoleKind,
    geometry: Arc<PreparedFluidGeometry>,
    scale: [f32; 3],
}

/// How thin a role's solid is in world metres, for the thin-collider
/// warning. A shell's wall is its thinnest part whatever the mesh's bounds;
/// a closed solid is taken at its bounds, without the lattice's padding.
fn thinnest(plan: &LatticePlan, scale: [f32; 3]) -> f32 {
    if plan.open {
        2.0 * plan.half_shell() * scale[0].min(scale[1]).min(scale[2])
    } else {
        (0..3)
            .map(|axis| plan.extent[axis] * scale[axis])
            .fold(f32::INFINITY, f32::min)
    }
}

type LatticeBuild = Result<Arc<DistanceLattice>, String>;

/// One geometry's distance lattice in one liquid. A liquid owns its
/// lattices because their spacing follows its cells and their crop its
/// domain; one build runs at a time and the newest plan wins.
struct RoleLattice {
    geometry: Arc<PreparedFluidGeometry>,
    /// The cell size and scale steps (`scale_steps`) `plan` was made from (a
    /// closed solid's plan ignores both, so they are left at zero for it),
    /// and the local domain box it was cropped to.
    asked: (f32, [i32; 2], Option<[[f32; 3]; 2]>),
    plan: LatticePlan,
    ready: Option<(LatticePlan, Arc<DistanceLattice>)>,
    building: Option<(LatticePlan, mpsc::Receiver<LatticeBuild>)>,
    failed: Option<(LatticePlan, String)>,
}

/// `geometry`'s lattice for this frame, covering at least `domain` (local
/// min then max) when given: `None` while live and nothing is built yet. A
/// crop that still holds the domain is kept; one that doesn't is made again
/// with a margin ([`with_margin`]). Live, a lattice built to an older plan
/// stands in until the current one is ready; `wait` blocks for the current
/// one.
fn resolve_lattice(
    lattices: &mut Vec<RoleLattice>,
    geometry: &Arc<PreparedFluidGeometry>,
    cell_size: f32,
    scale: [f32; 3],
    domain: Option<[[f32; 3]; 2]>,
    wait: bool,
) -> Result<Option<(Arc<DistanceLattice>, LatticePlan)>, String> {
    let held = lattices
        .iter()
        .find(|entry| Arc::ptr_eq(&entry.geometry, geometry))
        .and_then(|entry| entry.asked.2);
    let domain = domain.map(|need| match held {
        Some(have)
            if (0..3)
                .all(|axis| have[0][axis] <= need[0][axis] && need[1][axis] <= have[1][axis]) =>
        {
            have
        }
        _ => with_margin(need),
    });
    let asked = if geometry.has_open_parts() {
        (cell_size, scale_steps(scale), domain)
    } else {
        (0.0, [0; 2], domain)
    };
    let index = match lattices
        .iter()
        .position(|entry| Arc::ptr_eq(&entry.geometry, geometry))
    {
        Some(index) => index,
        None => {
            let plan = geometry.lattice_plan(cell_size, scale, domain)?;
            lattices.push(RoleLattice {
                geometry: Arc::clone(geometry),
                asked,
                plan,
                ready: None,
                building: None,
                failed: None,
            });
            lattices.len() - 1
        }
    };
    let entry = &mut lattices[index];
    if entry.asked != asked {
        entry.plan = geometry.lattice_plan(cell_size, scale, domain)?;
        entry.asked = asked;
    }
    let plan = entry.plan;
    loop {
        if let Some((built_plan, receiver)) = &entry.building {
            let settled =
                if wait {
                    Some(receiver.recv().unwrap_or_else(|_| {
                        Err("Fluid role distance lattice build stopped".into())
                    }))
                } else {
                    match receiver.try_recv() {
                        Ok(result) => Some(result),
                        Err(mpsc::TryRecvError::Empty) => None,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            Some(Err("Fluid role distance lattice build stopped".into()))
                        }
                    }
                };
            if let Some(result) = settled {
                let built_plan = *built_plan;
                entry.building = None;
                match result {
                    Ok(lattice) => entry.ready = Some((built_plan, lattice)),
                    Err(error) => entry.failed = Some((built_plan, error)),
                }
            }
        }
        if let Some((ready_plan, lattice)) = &entry.ready
            && *ready_plan == plan
        {
            return Ok(Some((Arc::clone(lattice), plan)));
        }
        if let Some((failed_plan, error)) = &entry.failed
            && *failed_plan == plan
        {
            return Err(error.clone());
        }
        if entry.building.is_none() {
            let (sender, receiver) = mpsc::channel();
            let source = Arc::clone(&entry.geometry);
            let spawned = std::thread::Builder::new()
                .name("fluid-role-distance".into())
                .spawn(move || {
                    // Always settle, so a waiter never waits on a build that panicked.
                    let built = std::panic::catch_unwind(|| source.build_lattice(&plan))
                        .unwrap_or_else(|_| {
                            Err("Fluid role distance lattice build panicked".into())
                        });
                    let _ = sender.send(built);
                });
            match spawned {
                Ok(_) => entry.building = Some((plan, receiver)),
                Err(error) => {
                    entry.failed = Some((
                        plan,
                        format!("Fluid role distance lattice could not start: {error}"),
                    ));
                    continue;
                }
            }
        }
        if wait {
            continue;
        }
        return Ok(entry
            .ready
            .as_ref()
            .map(|(stale, lattice)| (Arc::clone(lattice), *stale)));
    }
}

/// Whether the bodies can run this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodiesStatus {
    Ready,
    /// A role's geometry or distance lattice is still being prepared; the
    /// liquid holds.
    Pending,
}

#[derive(Default)]
pub struct LiquidBodies {
    roles: Vec<BodyRole>,
    /// Coupled rigid bodies' hulls, in the rigid world's order, after the
    /// roles: their shapes follow the roles' and their rows each tick's.
    coupled: Vec<Arc<PreparedFluidGeometry>>,
    /// The roles' controls at each tick's start, from the oldest tick not
    /// yet run.
    samples: TickSamples<Poses>,
    /// Scratch for one sample's controls, refilled in place.
    controls: Poses,
    frame: Option<ClockFrame>,
    clock_obstacles: Vec<GpuFlipBodyVertex>,
    clock_sources: Vec<GpuFlipBodyVertex>,
    shapes: Vec<LiquidShape>,
    /// Each shape's local mesh bounds, unscaled: min then max.
    bounds: Vec<[[f32; 3]; 2]>,
    atlas: Vec<u32>,
    /// Bumped whenever `shapes` or `atlas` is rebuilt.
    pub version: u64,
    rows: Vec<LiquidBody>,
    /// One per row of `rows` once [`Self::set_contacts`] ran; zero for a
    /// collider.
    contacts: Vec<BodySupports>,
    /// Inflow and Outflow rows, tick major, built beside `rows`.
    region_rows: Vec<LiquidBody>,
    warned_thin: bool,
    /// Each geometry's distance lattice at this liquid's plan for it.
    lattices: Vec<RoleLattice>,
    /// This frame's lattice and plan per shape, roles then coupled bodies.
    resolved: Vec<(Arc<DistanceLattice>, LatticePlan)>,
    /// The lattices the shapes and atlas were last built from.
    used: Vec<Arc<DistanceLattice>>,
    /// Each coupled body's lattice and plan in the last build, beside
    /// `coupled`: a body whose shape was swapped in place keeps its own
    /// (found by its Box3D handle) until its new one is ready, so the liquid
    /// never holds for a reshaped body.
    coupled_lattices: Vec<(Arc<DistanceLattice>, LatticePlan)>,
    /// Whether Inflow and Outflow roles are accepted as regions; a solver
    /// without sources and drains leaves this off and refuses them.
    accepts_regions: bool,
}

impl LiquidBodies {
    pub fn clock_obstacles(&self) -> &[GpuFlipBodyVertex] {
        &self.clock_obstacles
    }
    pub fn clock_sources(&self) -> &[GpuFlipBodyVertex] {
        &self.clock_sources
    }

    /// Conservative initial obstacle speed for the first accepted interval.
    /// This uses its prepared body rows with zero initial reaction, and is
    /// useful only beside a fresh marker sample of the matching incoming tick.
    /// It does not predict reactions from subsequent pressure solves.
    pub fn initial_clock_obstacle_speed(&self, interval: f32) -> f32 {
        if !interval.is_finite() || interval <= 0.0 {
            return -1.0;
        }
        let norm = |xyz: [f64; 3]| {
            xyz.into_iter()
                .map(|value| value * value)
                .sum::<f64>()
                .sqrt()
        };
        let xyz = |values: [f32; 4]| std::array::from_fn(|axis| f64::from(values[axis]));
        let mut maximum = 0.0f64;
        for vertex in &self.clock_obstacles {
            if vertex.position[3] == 0.0 {
                continue;
            }
            if !vertex.position[3].is_finite() {
                return -1.0;
            }
            if ![
                vertex.position,
                vertex.centroid,
                vertex.velocity,
                vertex.angular_velocity,
                vertex.acceleration,
                vertex.angular_acceleration,
            ]
            .into_iter()
            .all(|values| values[..3].iter().all(|value| value.is_finite()))
            {
                return -1.0;
            }
            let radius = norm(std::array::from_fn(|axis| {
                f64::from(vertex.position[axis]) - f64::from(vertex.centroid[axis])
            }));
            let speed = norm(xyz(vertex.velocity))
                + norm(xyz(vertex.angular_velocity)) * radius
                + f64::from(interval)
                    * (norm(xyz(vertex.acceleration))
                        + norm(xyz(vertex.angular_acceleration)) * radius);
            maximum = maximum.max(speed);
        }
        let mut bound = maximum as f32;
        if !bound.is_finite() {
            return -1.0;
        }
        // Casting rounds to nearest; move one positive float upward if needed.
        if f64::from(bound) < maximum {
            bound = f32::from_bits(bound.to_bits() + 1);
        }
        if bound.is_finite() { bound } else { -1.0 }
    }

    /// Reference CFL samples the actual mesh vertices. A coupled hull counts
    /// every vertex while its bounds overlap the domain, so a hull crossing
    /// or enclosing it with every vertex outside still counts, and a body
    /// falling far below the water never sets its substeps. Sources always
    /// feed the first-substep prediction; drains feed nothing.
    /// `tick` selects the accepted tick's rows within this display frame.
    pub fn prepare_clock_vertices(&mut self, min: [f32; 3], size: [f32; 3], tick: usize) {
        self.clock_obstacles.clear();
        self.clock_sources.clear();
        let body_offset = tick * self.count();
        let region_offset = tick * self.region_count();
        let mut append = |geometry: &PreparedFluidGeometry,
                          scale: [f32; 3],
                          row: LiquidBody,
                          coupled: bool,
                          source: bool,
                          row_index: Option<usize>| {
            let output = if source {
                &mut self.clock_sources
            } else {
                &mut self.clock_obstacles
            };
            let world_of = |vertex: &[f32; 3]| -> [f32; 3] {
                let local = std::array::from_fn(|i| vertex[i] * scale[i]);
                let q = row.rotation;
                let cross = |a: [f32; 3], b: [f32; 3]| {
                    [
                        a[1] * b[2] - a[2] * b[1],
                        a[2] * b[0] - a[0] * b[2],
                        a[0] * b[1] - a[1] * b[0],
                    ]
                };
                let axis = [q[0], q[1], q[2]];
                let uv = cross(axis, local);
                let uuv = cross(axis, uv);
                std::array::from_fn(|i| {
                    row.position_inv_mass[i] + local[i] + 2.0 * (q[3] * uv[i] + uuv[i])
                })
            };
            let vertices = || geometry.meshes.iter().flat_map(|mesh| &mesh.vertices);
            let overlaps = coupled && {
                let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
                for world in vertices().map(world_of) {
                    for i in 0..3 {
                        lo[i] = lo[i].min(world[i]);
                        hi[i] = hi[i].max(world[i]);
                    }
                }
                (0..3).all(|i| lo[i] <= min[i] + size[i] && hi[i] >= min[i])
            };
            for vertex in vertices() {
                let world = world_of(vertex);
                let eligible = row.accel_shape[3] >= 0.0 && (source || overlaps);
                let inherit = if source { row.inv_inertia_x[3] } else { 1.0 };
                // The fourth velocity word is spare for the clock seam. A
                // coupled hull carries its relative body-row index plus one;
                // sources keep zero so the shader cannot accidentally read
                // the reaction buffer.
                let velocity: [f32; 4] = std::array::from_fn(|i| {
                    if i < 3 {
                        inherit * row.linear_velocity[i]
                            + if source { row.inv_inertia_x[i] } else { 0.0 }
                    } else {
                        row_index.map_or(0.0, |index| (index + 1) as f32)
                    }
                });
                output.push(GpuFlipBodyVertex {
                    position: [world[0], world[1], world[2], u32::from(eligible) as f32],
                    velocity,
                    acceleration: if coupled { row.accel_shape } else { [0.0; 4] },
                    angular_velocity: row.angular_velocity.map(|v| v * inherit),
                    angular_acceleration: if coupled {
                        [
                            row.inv_inertia_x[3],
                            row.inv_inertia_y[3],
                            row.inv_inertia_z[3],
                            0.0,
                        ]
                    } else {
                        [0.0; 4]
                    },
                    centroid: row.position_inv_mass,
                });
            }
        };
        for (index, role) in self.roles.iter().enumerate() {
            if role.kind == FluidRoleKind::Inflow
                && let Some(&row) = self.region_rows.get(region_offset + index)
            {
                append(&role.geometry, role.scale, row, false, true, None);
            }
        }
        for (index, geometry) in self.coupled.iter().enumerate() {
            if let Some(&row) = self.rows.get(body_offset + index) {
                append(geometry, [1.0; 3], row, true, false, Some(index));
            }
        }
    }

    /// Bodies that also take Inflow and Outflow roles as region rows.
    pub fn with_regions() -> Self {
        Self {
            accepts_regions: true,
            ..Self::default()
        }
    }

    /// Coupled bodies: the body rows per tick.
    pub fn count(&self) -> usize {
        self.coupled.len()
    }

    /// Inflow and Outflow roles: the region rows per tick.
    pub fn region_count(&self) -> usize {
        self.roles.len()
    }

    /// The region rows the last [`Self::rows`] call produced.
    pub fn last_region_rows(&self) -> &[LiquidBody] {
        &self.region_rows
    }

    pub fn shapes(&self) -> &[LiquidShape] {
        &self.shapes
    }

    pub fn atlas(&self) -> &[u32] {
        &self.atlas
    }

    /// The rows the last [`Self::rows`] call produced.
    pub fn last_rows(&self) -> &[LiquidBody] {
        &self.rows
    }

    /// Take this frame's roles and coupled bodies' hulls (body-local, about
    /// the centre of mass, unscaled). Inflows and drains are regions where
    /// the liquid takes them; anything else is refused rather than ignored
    /// (a fill is seeded, never simulated). A new geometry,
    /// scale or distance lattice rebuilds the shapes and the atlas. Each
    /// geometry's lattice is built on a worker to the plan `cell_size` and
    /// the role's scale ask for ([`PreparedFluidGeometry::lattice_plan`]).
    /// Live, a geometry with no lattice yet leaves the bodies Pending, and
    /// one whose plan changed keeps its last lattice until the new one is
    /// ready; `wait` (offline) waits for the lattice the plan asks for, so no
    /// simulated time goes by while it builds. A fixed collider's lattice
    /// covers only `domain` (world min then max) where its row places it
    /// (SCENE_DOCUMENT_DESIGN.md section 7.18 (Fit with the water end
    /// game)), so a large ground costs and resolves only what the water
    /// touches.
    pub fn prepare(
        &mut self,
        roles: &[Option<FluidRole>],
        coupled: &[Arc<PreparedFluidGeometry>],
        coupled_rows: &[LiquidBody],
        domain: [[f32; 3]; 2],
        cell_size: f32,
        wait: bool,
    ) -> Result<BodiesStatus, String> {
        if coupled.len() > MAX_LIQUID_BODIES {
            return Err(format!(
                "Liquid: {} coupled bodies exceed the {MAX_LIQUID_BODIES} bodies a liquid holds",
                coupled.len()
            ));
        }
        self.resolved.clear();
        let mut same = true;
        let mut count = 0;
        for (slot, role) in roles.iter().enumerate() {
            let Some(role) = role else { continue };
            let region = matches!(role.kind, FluidRoleKind::Inflow | FluidRoleKind::Outflow);
            if !(region && self.accepts_regions) {
                let takes = if self.accepts_regions {
                    "Inflow and Outflow roles only"
                } else {
                    "no Inflow or Outflow roles"
                };
                return Err(format!(
                    "Liquid: role {slot} is {:?}; this liquid takes {takes}",
                    role.kind
                ));
            }
            if role
                .transform
                .scale
                .iter()
                .any(|s| !(s.is_finite() && *s > 0.0))
            {
                return Err(format!(
                    "Liquid: role {slot} scale must be finite and positive"
                ));
            }
            match resolve_lattice(
                &mut self.lattices,
                &role.geometry,
                cell_size,
                role.transform.scale,
                None,
                wait,
            ) {
                Ok(Some(found)) => self.resolved.push(found),
                Ok(None) => return Ok(BodiesStatus::Pending),
                Err(error) => return Err(format!("Liquid: role {slot}: {error}")),
            }
            same &= self.roles.get(count).is_some_and(|known| {
                known.slot == slot
                    && known.kind == role.kind
                    && Arc::ptr_eq(&known.geometry, &role.geometry)
                    && known.scale == role.transform.scale
            });
            count += 1;
        }
        for (index, geometry) in coupled.iter().enumerate() {
            let stand_in = geometry
                .body()
                .and_then(|body| {
                    self.coupled
                        .iter()
                        .position(|known| known.body() == Some(body))
                })
                .and_then(|known| self.coupled_lattices.get(known));
            let crop = coupled_rows
                .get(index)
                .filter(|_| geometry.is_fixed())
                .map(|row| local_domain(row, domain));
            match resolve_lattice(
                &mut self.lattices,
                geometry,
                cell_size,
                [1.0; 3],
                crop,
                wait,
            ) {
                Ok(Some(found)) => self.resolved.push(found),
                Ok(None) => match stand_in {
                    Some(stand_in) => self.resolved.push(stand_in.clone()),
                    None => return Ok(BodiesStatus::Pending),
                },
                Err(error) => return Err(format!("Liquid: coupled body {index}: {error}")),
            }
        }
        same &= count == self.roles.len()
            && self.coupled.len() == coupled.len()
            && self
                .coupled
                .iter()
                .zip(coupled)
                .all(|(a, b)| Arc::ptr_eq(a, b))
            && self.used.len() == self.resolved.len()
            && self
                .used
                .iter()
                .zip(&self.resolved)
                .all(|(a, (b, _))| Arc::ptr_eq(a, b));
        if same {
            return Ok(BodiesStatus::Ready);
        }
        self.rebuild(roles, coupled, cell_size);
        Ok(BodiesStatus::Ready)
    }

    fn rebuild(
        &mut self,
        roles: &[Option<FluidRole>],
        coupled: &[Arc<PreparedFluidGeometry>],
        cell_size: f32,
    ) {
        self.roles.clear();
        self.shapes.clear();
        self.bounds.clear();
        self.atlas.clear();
        self.used.clear();
        let mut placed: Vec<(Arc<DistanceLattice>, LiquidShape)> = Vec::new();
        let role_shapes = roles.iter().enumerate().filter_map(|(slot, role)| {
            role.as_ref().map(|role| {
                (
                    format!("{:?} role {slot}", role.kind),
                    Some((slot, role.kind)),
                    &role.geometry,
                    role.transform.scale,
                )
            })
        });
        let coupled_shapes = coupled
            .iter()
            .enumerate()
            .map(|(index, geometry)| (format!("coupled body {index}"), None, geometry, [1.0; 3]));
        let shapes: Vec<_> = role_shapes
            .chain(coupled_shapes)
            .zip(&self.resolved)
            .collect();
        for ((name, slot, geometry, scale), (lattice, plan)) in shapes {
            let base = match placed.iter().find(|(known, _)| Arc::ptr_eq(known, lattice)) {
                Some((_, shape)) => *shape,
                None => {
                    let shape = LiquidShape {
                        origin_spacing: [
                            lattice.origin[0],
                            lattice.origin[1],
                            lattice.origin[2],
                            lattice.spacing,
                        ],
                        dims_x: lattice.dims[0],
                        dims_y: lattice.dims[1],
                        dims_z: lattice.dims[2],
                        atlas_offset: self.atlas.len() as u32 * 2,
                        scale_min: [1.0; 4],
                    };
                    pack_distance_atlas(&lattice.values, &mut self.atlas);
                    placed.push((Arc::clone(lattice), shape));
                    shape
                }
            };
            let smallest = scale[0].min(scale[1]).min(scale[2]);
            let largest = scale[0].max(scale[1]).max(scale[2]);
            self.shapes.push(LiquidShape {
                scale_min: [scale[0], scale[1], scale[2], smallest],
                ..base
            });
            self.bounds.push(mesh_bounds(geometry));
            let thinnest = thinnest(plan, scale);
            if slot.is_none()
                && (plan.capped || thinnest < THIN_COLLIDER_CELLS * cell_size)
                && !self.warned_thin
            {
                self.warned_thin = true;
                if plan.capped {
                    log::warn!(
                        "[liquid] {name} needs more than {MAX_LATTICE_NODES} distance nodes at the liquid's {cell_size:.3} m cells; its lattice is {:.3} m apart, so its shell is {thinnest:.3} m thick",
                        plan.spacing * largest
                    );
                } else {
                    log::warn!(
                        "[liquid] {name} is {thinnest:.3} m thick, under {THIN_COLLIDER_CELLS} cells ({:.3} m); liquid may leak through it",
                        THIN_COLLIDER_CELLS * cell_size
                    );
                }
            }
            if let Some((slot, kind)) = slot {
                self.roles.push(BodyRole {
                    slot,
                    kind,
                    geometry: Arc::clone(geometry),
                    scale,
                });
            }
            self.used.push(Arc::clone(lattice));
        }
        self.coupled.clear();
        self.coupled.extend(coupled.iter().cloned());
        self.coupled_lattices.clear();
        self.coupled_lattices.extend(
            self.resolved[self.resolved.len() - coupled.len()..]
                .iter()
                .cloned(),
        );
        self.lattices.retain(|entry| {
            roles
                .iter()
                .flatten()
                .any(|role| Arc::ptr_eq(&role.geometry, &entry.geometry))
                || coupled
                    .iter()
                    .any(|geometry| Arc::ptr_eq(geometry, &entry.geometry))
        });
        self.version += 1;
    }

    /// The played times the history replay must sample before the next
    /// frame: each boundary a tick may start at under `clock`, in `(from, until]`.
    pub fn request_samples(
        &mut self,
        clock: &LiquidClock,
        from: PlayedTime,
        until: PlayedTime,
        out: &mut Vec<PlayedTime>,
    ) {
        self.samples.request(clock, from, until, out);
    }

    /// A history replay sample of the roles at played time `now`, after `clock`
    /// observed this sample's Speed; `None` while a role is still pending.
    pub fn observe_sample(
        &mut self,
        clock: &LiquidClock,
        now: PlayedTime,
        roles: Option<&[Option<FluidRole>]>,
    ) {
        if let Some(roles) = roles {
            fill_controls(roles, &mut self.controls);
        }
        let controls = roles.map(|_| &self.controls);
        self.samples.observe(clock, now, controls);
    }

    /// This frame's roles, right after `clock` advanced to `frame`: they
    /// belong to a tick that starts now.
    pub fn settle(&mut self, roles: &[Option<FluidRole>], clock: &LiquidClock, frame: &ClockFrame) {
        self.frame = Some(frame.clone());
        fill_controls(roles, &mut self.controls);
        self.samples.settle(clock, frame, Some(&self.controls));
    }

    /// One row per coupled body for each of this frame's ticks,
    /// `first_tick..`, tick major: `coupled`, the coupled bodies' tick-start
    /// state. Beside them ([`Self::last_region_rows`]) one region row per
    /// role: its pose at the tick's start and the velocities that reach the
    /// pose at its end (the next tick's start). `ticks` 0 publishes the first tick's
    /// start poses at rest, which a restart seeds around. Every tick gets the
    /// same coupled rows; a frame of several coupled ticks rewrites
    /// each later tick's with [`Self::set_coupled_rows`] once Box3D has
    /// stepped the tick before. A coupled row's shape index counts from the
    /// first coupled body, or is −1. Run ticks' samples are pruned afterwards.
    pub fn rows(
        &mut self,
        first_tick: u64,
        ticks: u32,
        coupled: &[LiquidBody],
    ) -> Result<&[LiquidBody], String> {
        self.rows.clear();
        self.contacts.clear();
        self.region_rows.clear();
        debug_assert_eq!(
            coupled.len(),
            self.coupled.len(),
            "one row per prepared coupled body"
        );
        let offset = self.roles.len() as f32;
        let row_ticks = ticks.max(1) as usize;
        if self.roles.is_empty() {
            for _ in 0..row_ticks {
                self.rows
                    .extend(coupled.iter().map(|row| coupled_row(row, offset)));
            }
            return Ok(&self.rows);
        }
        // A reanchor separates the accepted closing pose from the next start.
        // A seed row has no motion.
        let poses = self
            .samples
            .span(first_tick, row_ticks)
            .map_err(|tick| {
                format!("Liquid bodies: tick {tick} was never sampled; the host must replay physics history before each frame")
            })?;
        for (ordinal, (_, start)) in poses.enumerate() {
            let (end, duration) = if ticks > 0 {
                let tick = first_tick + ordinal as u64;
                let end = self.samples.endpoint(tick + 1).ok_or_else(|| {
                    format!("Liquid bodies: tick {} end was never sampled; the host must replay physics history before each frame", tick + 1)
                })?;
                let interval = self
                    .frame
                    .as_ref()
                    .and_then(|frame| {
                        tick.checked_sub(frame.first_sequence)
                            .and_then(|ordinal| frame.interval(ordinal))
                    })
                    .ok_or_else(|| {
                        format!("Liquid bodies: tick {tick} has no accepted clock interval")
                    })?;
                (end, interval.duration().0)
            } else {
                (start, 0.0)
            };
            for (index, role) in self.roles.iter().enumerate() {
                let from = start.get(role.slot).copied().unwrap_or_default();
                let row = body_row(
                    from,
                    end.get(role.slot).copied().unwrap_or_default(),
                    index as f32,
                    duration,
                );
                self.region_rows.push(region_row(row, role.kind, from));
            }
            self.rows
                .extend(coupled.iter().map(|row| coupled_row(row, offset)));
        }
        self.samples.prune_before(first_tick + u64::from(ticks));
        Ok(&self.rows)
    }

    /// Support points beside the last [`Self::rows`]: `coupled` for the
    /// coupled bodies of every tick.
    pub fn set_contacts(&mut self, coupled: &[BodySupports]) -> Result<(), String> {
        if coupled.len() != self.coupled.len() {
            return Err("Liquid bodies: one contact set per coupled body".into());
        }
        self.contacts.clear();
        for _ in 0..self.rows.len() / self.count().max(1) {
            self.contacts.extend_from_slice(coupled);
        }
        Ok(())
    }

    /// The support points the last [`Self::set_contacts`] produced.
    pub fn last_contacts(&self) -> &[BodySupports] {
        &self.contacts
    }

    /// Replace tick `tick`'s coupled support points with `coupled`, as
    /// [`Self::set_coupled_rows`] does the rows; returns them and their byte
    /// offset in [`Self::last_contacts`].
    pub fn set_coupled_contacts(
        &mut self,
        tick: usize,
        coupled: &[BodySupports],
    ) -> Result<(u64, &[BodySupports]), String> {
        let start = tick * self.count();
        if coupled.len() != self.coupled.len() || start + coupled.len() > self.contacts.len() {
            return Err(format!(
                "Liquid coupling: tick {tick} has no coupled contacts this frame"
            ));
        }
        let contacts = &mut self.contacts[start..start + coupled.len()];
        contacts.copy_from_slice(coupled);
        Ok((
            (start * std::mem::size_of::<BodySupports>()) as u64,
            contacts,
        ))
    }

    /// Replace tick `tick`'s coupled rows (counted from this frame's first
    /// tick) with `coupled`; returns the rewritten rows and their byte offset
    /// in [`Self::last_rows`].
    pub fn set_coupled_rows(
        &mut self,
        tick: usize,
        coupled: &[LiquidBody],
    ) -> Result<(u64, &[LiquidBody]), String> {
        let start = tick * self.count();
        if coupled.len() != self.coupled.len() || start + coupled.len() > self.rows.len() {
            return Err(format!(
                "Liquid coupling: tick {tick} has no coupled rows this frame"
            ));
        }
        let offset = self.roles.len() as f32;
        let rows = &mut self.rows[start..start + coupled.len()];
        for (row, body) in rows.iter_mut().zip(coupled) {
            *row = coupled_row(body, offset);
        }
        Ok(((start * std::mem::size_of::<LiquidBody>()) as u64, rows))
    }

    /// The box around every enabled body and region at the first tick's
    /// start of the last [`Self::rows`] call, posed as the solver poses it.
    pub fn write_gizmos(&self, out: &mut Vec<PhysicsGizmo>) {
        // A body with no inverse mass is prescribed: a collider the water meets.
        let bodies = self.rows.iter().take(self.count()).map(|row| {
            (
                row,
                if row.position_inv_mass[3] == 0.0 {
                    PhysicsGizmoFamily::Collider
                } else {
                    PhysicsGizmoFamily::Body
                },
            )
        });
        let regions = self
            .region_rows
            .iter()
            .take(self.region_count())
            .map(|row| {
                (
                    row,
                    if row.angular_velocity[3] == REGION_INFLOW {
                        PhysicsGizmoFamily::Source
                    } else {
                        PhysicsGizmoFamily::Drain
                    },
                )
            });
        for (row, family) in bodies.chain(regions) {
            // A disabled body carries shape -1.
            let shape = row.accel_shape[3];
            if shape < 0.0 {
                continue;
            }
            let (Some(bounds), Some(scaled)) = (
                self.bounds.get(shape as usize),
                self.shapes.get(shape as usize),
            ) else {
                continue;
            };
            let scale = [
                scaled.scale_min[0],
                scaled.scale_min[1],
                scaled.scale_min[2],
            ];
            let local: [f32; 3] =
                std::array::from_fn(|i| 0.5 * (bounds[0][i] + bounds[1][i]) * scale[i]);
            let offset = rotate(row.rotation, local);
            out.push(PhysicsGizmo {
                family,
                shape: PhysicsGizmoShape::Box {
                    center: std::array::from_fn(|i| row.position_inv_mass[i] + offset[i]),
                    half: std::array::from_fn(|i| 0.5 * (bounds[1][i] - bounds[0][i]) * scale[i]),
                    rotation: row.rotation,
                },
            });
        }
    }
}

fn mesh_bounds(geometry: &PreparedFluidGeometry) -> [[f32; 3]; 2] {
    let mut bounds = [[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]];
    for vertex in geometry.meshes.iter().flat_map(|mesh| &mesh.vertices) {
        for i in 0..3 {
            bounds[0][i] = bounds[0][i].min(vertex[i]);
            bounds[1][i] = bounds[1][i].max(vertex[i]);
        }
    }
    bounds
}

/// `v` turned by the unit quaternion `q` (xyzw).
fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let axis = [q[0], q[1], q[2]];
    let uv = cross(axis, v);
    let uuv = cross(axis, uv);
    std::array::from_fn(|i| v[i] + 2.0 * (q[3] * uv[i] + uuv[i]))
}

/// The share of a crop's longest side added on every side when a fixed
/// collider's lattice is cropped again. A collider dragged less than that
/// while its new lattice builds is still covered by the old one, so water
/// never drops through at the domain's edge.
const CROP_MARGIN: f32 = 0.125;

/// `crop` grown by [`CROP_MARGIN`] of its longest side on every side.
fn with_margin(crop: [[f32; 3]; 2]) -> [[f32; 3]; 2] {
    let longest = (0..3)
        .map(|axis| crop[1][axis] - crop[0][axis])
        .fold(0.0f32, f32::max);
    let margin = CROP_MARGIN * longest;
    [
        crop[0].map(|value| value - margin),
        crop[1].map(|value| value + margin),
    ]
}

/// The world box `domain` (min then max) in the frame of the body `row`
/// places (about its centre of mass): the box round the domain's corners
/// turned back into the body.
fn local_domain(row: &LiquidBody, domain: [[f32; 3]; 2]) -> [[f32; 3]; 2] {
    let q = row.rotation;
    let inverse = [-q[0], -q[1], -q[2], q[3]];
    let mut local = [[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]];
    for corner in 0..8 {
        let offset: [f32; 3] = std::array::from_fn(|axis| {
            domain[(corner >> axis) & 1][axis] - row.position_inv_mass[axis]
        });
        let point = rotate(inverse, offset);
        for axis in 0..3 {
            local[0][axis] = local[0][axis].min(point[axis]);
            local[1][axis] = local[1][axis].max(point[axis]);
        }
    }
    local
}

/// A coupled body's row with its shape index moved past the roles' shapes.
fn coupled_row(row: &LiquidBody, offset: f32) -> LiquidBody {
    let shape = row.accel_shape[3];
    LiquidBody {
        accel_shape: [
            row.accel_shape[0],
            row.accel_shape[1],
            row.accel_shape[2],
            if shape >= 0.0 { shape + offset } else { -1.0 },
        ],
        ..*row
    }
}

/// A source or drain's row: a body row with the region code in
/// `angular_velocity.w`, and the emitted velocity (m/s, world) with the
/// share of the region's own motion added to it in `inv_inertia_x`.
fn region_row(row: LiquidBody, kind: FluidRoleKind, controls: Controls) -> LiquidBody {
    let code = if kind == FluidRoleKind::Inflow {
        REGION_INFLOW
    } else {
        REGION_OUTFLOW
    };
    let v = controls.velocity;
    LiquidBody {
        angular_velocity: [
            row.angular_velocity[0],
            row.angular_velocity[1],
            row.angular_velocity[2],
            code,
        ],
        inv_inertia_x: [v[0], v[1], v[2], controls.inherit_motion.clamp(0.0, 1.0)],
        ..row
    }
}

/// The row for one tick: `start`'s pose, the linear velocity from `start` to
/// `end`, and the angular velocity of the shortest turn between them.
fn body_row(start: Controls, end: Controls, shape: f32, duration: f64) -> LiquidBody {
    let tick = if duration > 0.0 {
        duration as f32
    } else {
        TICK as f32
    };
    let a = pose_from_transform(start.transform);
    let b = pose_from_transform(end.transform);
    let velocity: [f32; 3] = std::array::from_fn(|i| (b.position[i] - a.position[i]) / tick);
    // rel = b ⊗ a⁻¹, the world-frame turn over the tick.
    let (p, q) = (
        b.rotation,
        [
            -a.rotation[0],
            -a.rotation[1],
            -a.rotation[2],
            a.rotation[3],
        ],
    );
    let mut rel = [
        p[3] * q[0] + p[0] * q[3] + p[1] * q[2] - p[2] * q[1],
        p[3] * q[1] - p[0] * q[2] + p[1] * q[3] + p[2] * q[0],
        p[3] * q[2] + p[0] * q[1] - p[1] * q[0] + p[2] * q[3],
        p[3] * q[3] - p[0] * q[0] - p[1] * q[1] - p[2] * q[2],
    ];
    if rel[3] < 0.0 {
        rel = rel.map(|c| -c);
    }
    let sine = (rel[0] * rel[0] + rel[1] * rel[1] + rel[2] * rel[2]).sqrt();
    let angle = 2.0 * sine.atan2(rel[3]);
    let angular: [f32; 3] = if sine > 1e-9 {
        std::array::from_fn(|i| rel[i] / sine * angle / tick)
    } else {
        [0.0; 3]
    };
    LiquidBody {
        position_inv_mass: [a.position[0], a.position[1], a.position[2], 0.0],
        rotation: a.rotation,
        linear_velocity: [
            velocity[0],
            velocity[1],
            velocity[2],
            start.friction.clamp(0.0, 1.0),
        ],
        angular_velocity: [angular[0], angular[1], angular[2], 0.0],
        accel_shape: [0.0, 0.0, 0.0, if start.enabled { shape } else { -1.0 }],
        ..LiquidBody::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fluid_role::PreparedFluidGeometry;
    use manifold_node_engine::ports::std430_stride;

    fn cube() -> Arc<PreparedFluidGeometry> {
        cube_of(0.5)
    }

    fn cube_of(half: f32) -> Arc<PreparedFluidGeometry> {
        Arc::new(PreparedFluidGeometry::new(vec![box_mesh([half; 3])]))
    }

    /// A closed box about the origin with half sides `half`.
    fn box_mesh(half: [f32; 3]) -> manifold_physics::TriangleMesh {
        let v = |x: usize, y: usize, z: usize| {
            [
                [-half[0], half[0]][x],
                [-half[1], half[1]][y],
                [-half[2], half[2]][z],
            ]
        };
        manifold_physics::TriangleMesh {
            vertices: vec![
                v(0, 0, 0),
                v(1, 0, 0),
                v(1, 1, 0),
                v(0, 1, 0),
                v(0, 0, 1),
                v(1, 0, 1),
                v(1, 1, 1),
                v(0, 1, 1),
            ],
            triangles: vec![
                [0, 2, 1],
                [0, 3, 2],
                [4, 5, 6],
                [4, 6, 7],
                [0, 1, 5],
                [0, 5, 4],
                [2, 3, 7],
                [2, 7, 6],
                [1, 2, 6],
                [1, 6, 5],
                [0, 4, 7],
                [0, 7, 3],
            ],
        }
    }

    fn inflow(geometry: &Arc<PreparedFluidGeometry>, pos: [f32; 3], yaw: f32) -> Option<FluidRole> {
        Some(FluidRole {
            geometry: Arc::clone(geometry),
            kind: FluidRoleKind::Inflow,
            transform: Transform {
                pos,
                rot_euler: [0.0, yaw, 0.0],
                scale: [0.4, 0.2, 0.3],
                ..Transform::default()
            },
            enabled: true,
            velocity: [0.0; 3],
            inherit_motion: 0.0,
            friction: 0.25,
        })
    }

    fn ready(bodies: &mut LiquidBodies, roles: &[Option<FluidRole>]) {
        ready_coupled(bodies, roles, &[]);
    }

    /// The region rows of this frame's ticks.
    fn region_rows(bodies: &mut LiquidBodies, first_tick: u64, ticks: u32) -> Vec<LiquidBody> {
        bodies.rows(first_tick, ticks, &[]).unwrap();
        bodies.last_region_rows().to_vec()
    }

    fn ready_coupled(
        bodies: &mut LiquidBodies,
        roles: &[Option<FluidRole>],
        coupled: &[Arc<PreparedFluidGeometry>],
    ) {
        // A poll count, not a wall-clock budget, so a loaded machine cannot fail it.
        let mut polls = 0u32;
        while bodies
            .prepare(roles, coupled, &[], [[0.0; 3]; 2], 0.0625, false)
            .expect("bodies")
            == BodiesStatus::Pending
        {
            polls += 1;
            assert!(polls < 6000, "the lattice never arrived");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn liquid_body_records_match_their_channel_layouts() {
        assert_eq!(std430_stride(LIQUID_BODY_SPECS), 128);
        assert_eq!(std430_stride(LIQUID_SHAPE_SPECS), 48);
    }

    /// A quarter turn about y over one tick: halfway through, the pose is the
    /// slerp midpoint (45°) and the translation the lerp midpoint; the
    /// quaternion stays unit length. The atlas halves round-trip.
    #[test]
    fn liquid_body_pose_follows_the_tick() {
        let tick = TICK as f32;
        let turn = std::f32::consts::FRAC_PI_2;
        let body = LiquidBody {
            position_inv_mass: [1.0, 2.0, 3.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            linear_velocity: [0.6 / tick, 0.0, -0.3 / tick, 0.5],
            angular_velocity: [0.0, turn / tick, 0.0, 0.0],
            ..LiquidBody::default()
        };
        let (position, rotation) = body_pose_at(&body, 0.5 * tick);
        let expected = [0.0, (turn * 0.25).sin(), 0.0, (turn * 0.25).cos()];
        assert!(
            (0..3).all(|i| (position[i] - [1.3, 2.0, 2.85][i]).abs() < 1e-5),
            "{position:?}"
        );
        assert!(
            (0..4).all(|i| (rotation[i] - expected[i]).abs() < 1e-6),
            "{rotation:?}"
        );
        let (_, end) = body_pose_at(&body, tick);
        assert!((end.iter().map(|c| c * c).sum::<f32>() - 1.0).abs() < 1e-6);
        assert!((end[1] - (turn * 0.5).sin()).abs() < 1e-6);
        let mut words = Vec::new();
        pack_distance_atlas(&[-0.25, 1.5, 0.125], &mut words);
        let unpack = |w: u32| half::f16::from_bits(w as u16).to_f32();
        assert_eq!(
            (unpack(words[0]), unpack(words[0] >> 16), unpack(words[1])),
            (-0.25, 1.5, 0.125)
        );
        assert_eq!(unpack(words[1] >> 16), f32::INFINITY);
    }

    /// Drives bodies as the host does: before each frame the history replay
    /// samples the roles (here a function of transport time) at every
    /// requested tick start and closes the interval at the frame's own time;
    /// the frame then advances the clock and settles.
    #[derive(Default)]
    struct Rig {
        clock: LiquidClock,
        last: Option<f64>,
        times: Vec<f64>,
    }

    impl Rig {
        fn frame(
            &mut self,
            bodies: &mut LiquidBodies,
            transport: f64,
            interval: f64,
            roles_at: &dyn Fn(f64) -> Vec<Option<FluidRole>>,
        ) -> ClockFrame {
            if let Some(last) = self.last.filter(|&last| transport > last) {
                self.times.clear();
                {
                    let mut played = Vec::new();
                    bodies.request_samples(
                        &self.clock,
                        PlayedTime::from_played(last),
                        PlayedTime::from_played(transport),
                        &mut played,
                    );
                    self.times
                        .extend(played.into_iter().map(PlayedTime::seconds));
                }
                for &time in self.times.iter().filter(|&&time| time < transport) {
                    bodies.observe_sample(
                        &self.clock,
                        PlayedTime::from_played(time),
                        Some(&roles_at(time)),
                    );
                }
                bodies.observe_sample(
                    &self.clock,
                    PlayedTime::from_played(transport),
                    Some(&roles_at(transport)),
                );
            }
            self.last = Some(transport);
            let frame = self.clock.advance(
                PlayedTime::from_played(transport),
                interval,
                1.0,
                0.0,
                false,
                false,
            );
            bodies.settle(&roles_at(transport), &self.clock, &frame);
            frame
        }
    }

    fn first_tick(frame: &ClockFrame) -> u64 {
        crate::fields::first_tick(frame)
    }

    /// A source moving linearly gets one row over each accepted interval.
    #[test]
    fn liquid_body_rows_follow_authored_motion() {
        let geometry = cube();
        let roles_at = |t: f64| {
            let alpha = (t / (2.0 * TICK)) as f32;
            let mut roles = vec![None; 3];
            roles[2] = inflow(&geometry, [0.3 * alpha, 1.0, -0.15 * alpha], 0.6 * alpha);
            roles[2].as_mut().unwrap().enabled = t < 2.5 * TICK;
            roles
        };
        let mut bodies = LiquidBodies::with_regions();
        ready(&mut bodies, &roles_at(0.0));
        assert_eq!(bodies.region_count(), 1);
        assert_eq!(bodies.shapes()[0].scale_min, [0.4, 0.2, 0.3, 0.2]);
        assert_eq!(bodies.shapes()[0].atlas_offset, 0);
        let words = bodies.atlas().len();
        assert_eq!(words, (37usize.pow(3)).div_ceil(2));
        let mut rig = Rig::default();
        let interval = 2.0 * TICK;
        let restart = rig.frame(&mut bodies, 0.0, interval, &roles_at);
        assert_eq!(
            region_rows(&mut bodies, 0, restart.ticks)[0].linear_velocity[..3],
            [0.0; 3],
            "a seed row is at rest"
        );
        let frame = rig.frame(&mut bodies, interval, interval, &roles_at);
        let rows = region_rows(&mut bodies, first_tick(&frame), frame.ticks);
        assert_eq!(rows.len(), 1);
        let tick = (2.0 * TICK) as f32;
        for (k, row) in rows.iter().enumerate() {
            let at = |t: f32| {
                let alpha = t / tick;
                pose_from_transform(Transform {
                    pos: [0.3 * alpha, 1.0, -0.15 * alpha],
                    rot_euler: [0.0, 0.6 * alpha, 0.0],
                    ..Transform::default()
                })
            };
            let (start, end) = (at(k as f32 * tick), at((k + 1) as f32 * tick));
            assert!((0..3).all(|i| (row.position_inv_mass[i] - start.position[i]).abs() < 1e-6));
            let (position, rotation) = body_pose_at(row, tick);
            assert!(
                (0..3).all(|i| (position[i] - end.position[i]).abs() < 1e-5),
                "{position:?} vs {:?}",
                end.position
            );
            let dot: f32 = (0..4).map(|i| rotation[i] * end.rotation[i]).sum();
            assert!(
                dot.abs() > 1.0 - 1e-6,
                "tick {k}: {rotation:?} vs {:?}",
                end.rotation
            );
            assert_eq!(row.linear_velocity[3], 0.25);
            assert_eq!(row.accel_shape[3], 0.0);
        }
        let frame = rig.frame(&mut bodies, 2.0 * interval, interval, &roles_at);
        let rows = region_rows(&mut bodies, first_tick(&frame), frame.ticks);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].accel_shape[3], 0.0);
    }

    #[test]
    fn liquid_body_rows_accept_two_live_intervals() {
        let geometry = cube();
        let roles = vec![inflow(&geometry, [0.0; 3], 0.0)];
        let mut bodies = LiquidBodies::with_regions();
        ready(&mut bodies, &roles);
        let mut rig = Rig::default();
        rig.frame(&mut bodies, 0.0, TICK, &|_| roles.clone());
        let frame = rig.frame(&mut bodies, 2.0 * TICK, TICK, &|_| roles.clone());
        assert_eq!(
            region_rows(&mut bodies, first_tick(&frame), frame.ticks).len(),
            2
        );
    }

    #[test]
    fn liquid_body_rows_do_not_compress_discarded_source_motion() {
        let geometry = cube();
        let roles_at = |transport: f64| {
            vec![inflow(
                &geometry,
                [transport as f32, 0.0, 0.0],
                transport as f32,
            )]
        };
        let mut bodies = LiquidBodies::with_regions();
        ready(&mut bodies, &roles_at(0.0));
        let mut rig = Rig::default();
        rig.frame(&mut bodies, 0.0, TICK, &roles_at);
        let frame = rig.frame(&mut bodies, 0.7, TICK, &roles_at);
        assert!(frame.reanchored);
        assert_eq!(frame.ticks, 2);
        let rows = region_rows(&mut bodies, first_tick(&frame), frame.ticks);
        assert_eq!(rows.len(), 2);
        for (ordinal, row) in rows.iter().enumerate() {
            let interval = frame.interval(ordinal as u64).unwrap();
            assert!((row.position_inv_mass[0] - interval.start.0 as f32).abs() < 1e-6);
            assert!((row.linear_velocity[0] - 1.0).abs() < 1e-5);
            assert!((row.angular_velocity[1] - 1.0).abs() < 1e-5);
            let (position, _) = body_pose_at(row, interval.duration().0 as f32);
            assert!((position[0] - interval.end.0 as f32).abs() < 1e-6);
        }

        let frame = rig.frame(&mut bodies, 0.7 + TICK, TICK, &roles_at);
        assert_eq!(frame.ticks, 1);
        let row = region_rows(&mut bodies, first_tick(&frame), frame.ticks)[0];
        assert!((row.position_inv_mass[0] - 0.7).abs() < 1e-6);
        assert!((row.linear_velocity[0] - 1.0).abs() < 1e-5);
        assert!((row.angular_velocity[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn liquid_body_rows_use_each_speed_interval_duration() {
        let geometry = cube();
        let roles_at = |transport: f64| vec![inflow(&geometry, [transport as f32, 0.0, 0.0], 0.0)];
        let mut bodies = LiquidBodies::with_regions();
        ready(&mut bodies, &roles_at(0.0));
        let mut rig = Rig::default();
        rig.frame(&mut bodies, 0.0, TICK, &roles_at);
        rig.clock.observe_speed(PlayedTime::from_played(TICK), 2.0);
        let frame = rig.frame(&mut bodies, 2.0 * TICK, TICK, &roles_at);
        assert_eq!(frame.ticks, 2);
        assert!((frame.interval(0).unwrap().duration().0 - TICK).abs() < 1e-9);
        assert!((frame.interval(1).unwrap().duration().0 - 2.0 * TICK).abs() < 1e-9);
        let rows = region_rows(&mut bodies, first_tick(&frame), frame.ticks);
        for (ordinal, (row, speed)) in rows.iter().zip([1.0, 0.5]).enumerate() {
            assert!((row.linear_velocity[0] - speed).abs() < 1e-5);
            let (position, _) = body_pose_at(
                row,
                frame.interval(ordinal as u64).unwrap().duration().0 as f32,
            );
            assert!((position[0] - (ordinal + 1) as f32 * TICK as f32).abs() < 1e-6);
        }
    }

    /// Every accepted live interval gets one row at the authored interval
    /// start, at each display rate.
    #[test]
    fn liquid_body_rows_match_at_every_frame_rate() {
        let geometry = cube();
        let roles_at = |t: f64| {
            // Smoothstep ease along an arc that turns as it goes.
            let s = (t / 0.8).clamp(0.0, 1.0);
            let eased = (s * s * (3.0 - 2.0 * s)) as f32;
            let angle = std::f32::consts::PI * eased;
            vec![
                None,
                inflow(
                    &geometry,
                    [angle.cos(), 1.0 + 0.5 * angle.sin(), 0.2 * eased],
                    1.3 * angle,
                ),
            ]
        };
        let run = |fps: f64| {
            let mut bodies = LiquidBodies::with_regions();
            ready(&mut bodies, &roles_at(0.0));
            let mut rig = Rig::default();
            let mut out = Vec::new();
            for index in 0..=(fps as u64) {
                let frame = rig.frame(&mut bodies, index as f64 / fps, 1.0 / fps, &roles_at);
                assert_eq!(frame.dropped_seconds, 0.0, "{fps} fps dropped time");
                if frame.ticks == 0 {
                    continue;
                }
                assert_eq!(frame.ticks, 1, "one accepted interval per live frame");
                let interval = frame.interval(0).unwrap();
                let row = region_rows(&mut bodies, first_tick(&frame), frame.ticks)[0];
                out.push((interval.start.0, row));
            }
            out
        };
        for fps in [20.0, 24.0, 30.0, 60.0] {
            let rows = run(fps);
            assert_eq!(rows.len(), fps as usize, "{fps} fps interval count");
            for (start, row) in rows {
                let end = start + 1.0 / fps;
                let from = Controls::from_role(roles_at(start)[1].as_ref().unwrap());
                let to = Controls::from_role(roles_at(end)[1].as_ref().unwrap());
                let expected = region_row(
                    body_row(from, to, 0.0, end - start),
                    FluidRoleKind::Inflow,
                    from,
                );
                let fields = |b: &LiquidBody| {
                    [
                        b.position_inv_mass,
                        b.rotation,
                        b.linear_velocity,
                        b.angular_velocity,
                    ]
                };
                for (actual, expected) in fields(&row).iter().zip(fields(&expected)) {
                    assert!(
                        (0..4).all(|i| (actual[i] - expected[i]).abs() < 1e-5),
                        "{fps} fps at {start}: {actual:?} vs {expected:?}"
                    );
                }
            }
        }
    }

    /// Coupled bodies' shapes follow the roles' at scale 1, and their rows
    /// follow the roles' each tick with shape indices past the roles'.
    #[test]
    fn liquid_bodies_append_coupled_bodies() {
        let geometry = cube();
        let hull = cube();
        let roles = vec![inflow(&geometry, [0.0; 3], 0.0), None];
        let mut bodies = LiquidBodies::with_regions();
        ready_coupled(&mut bodies, &roles, &[Arc::clone(&hull), Arc::clone(&hull)]);
        assert_eq!(bodies.count(), 2);
        assert_eq!(bodies.shapes().len(), 3);
        assert_eq!(bodies.shapes()[1].scale_min, [1.0; 4]);
        assert_ne!(
            bodies.shapes()[0].atlas_offset,
            bodies.shapes()[1].atlas_offset
        );
        assert_eq!(
            bodies.shapes()[1].atlas_offset,
            bodies.shapes()[2].atlas_offset
        );
        let version = bodies.version;
        ready_coupled(&mut bodies, &roles, &[Arc::clone(&hull), Arc::clone(&hull)]);
        assert_eq!(bodies.version, version, "unchanged hulls rebuild nothing");
        let mut rig = Rig::default();
        rig.frame(&mut bodies, 0.0, TICK, &|_| roles.clone());
        rig.frame(&mut bodies, TICK, TICK, &|_| roles.clone());
        let body = |shape: f32| LiquidBody {
            position_inv_mass: [0.0, 1.0, 0.0, 0.5],
            accel_shape: [0.0, -9.81, 0.0, shape],
            ..LiquidBody::default()
        };
        let rows = bodies
            .rows(0, 1, &[body(0.0), body(-1.0)])
            .unwrap()
            .to_vec();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].accel_shape, [0.0, -9.81, 0.0, 1.0]);
        assert_eq!(rows[1].accel_shape[3], -1.0);
        assert_eq!(rows[0].position_inv_mass, [0.0, 1.0, 0.0, 0.5]);
        // The coupled bodies fill the liquid's body rows, past the role ports.
        let full: Vec<_> = (0..MAX_LIQUID_BODIES).map(|_| Arc::clone(&hull)).collect();
        ready_coupled(&mut bodies, &roles, &full);
        assert_eq!(bodies.count(), MAX_LIQUID_BODIES);
        let many: Vec<_> = (0..=MAX_LIQUID_BODIES).map(|_| Arc::clone(&hull)).collect();
        assert!(
            bodies
                .prepare(&roles, &many, &[], [[0.0; 3]; 2], 0.0625, false)
                .unwrap_err()
                .contains("exceed")
        );
    }

    /// Offline, the first prepare of fresh geometry waits for its distance
    /// lattices and is Ready; live, the same call is Pending.
    #[test]
    fn liquid_bodies_offline_prepare_waits_for_the_lattice() {
        let roles = vec![inflow(&cube(), [0.0; 3], 0.0)];
        let mut live = LiquidBodies::with_regions();
        assert_eq!(
            live.prepare(&roles, &[], &[], [[0.0; 3]; 2], 0.0625, false),
            Ok(BodiesStatus::Pending)
        );
        let roles = vec![inflow(&cube(), [0.0; 3], 0.0)];
        let mut offline = LiquidBodies::with_regions();
        assert_eq!(
            offline.prepare(&roles, &[cube()], &[], [[0.0; 3]; 2], 0.0625, true),
            Ok(BodiesStatus::Ready)
        );
        assert_eq!((offline.count(), offline.region_count()), (1, 1));
    }

    /// A fixed collider far larger than the water (a 100 m slab under a 4 m
    /// domain) gets a lattice over only the domain where its row places it,
    /// with a margin, a 32nd of the crop apart, exact on the floor; a second
    /// water crops the same slab to its own box. A moving hull keeps its
    /// whole lattice.
    #[test]
    fn liquid_bodies_crop_a_fixed_collider_to_the_domain() {
        let slab = [box_mesh([50.0, 0.5, 50.0])];
        let fixed = Arc::new(PreparedFluidGeometry::fixed(slab.to_vec()));
        let moving = Arc::new(PreparedFluidGeometry::new(slab.to_vec()));
        // The slab's top face at world y = 0.
        let row = LiquidBody {
            position_inv_mass: [0.0, -0.5, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            ..LiquidBody::default()
        };
        let lattice_in = |geometry: &Arc<PreparedFluidGeometry>, domain: [[f32; 3]; 2]| {
            let mut bodies = LiquidBodies::default();
            let status = bodies.prepare(
                &[],
                std::slice::from_ref(geometry),
                &[row],
                domain,
                0.0625,
                true,
            );
            assert_eq!(status, Ok(BodiesStatus::Ready));
            bodies.resolved[0].clone()
        };
        let margin = 4.0 * CROP_MARGIN;
        let spacing = (4.0 + 2.0 * margin) / crate::fluid_role::DISTANCE_NODES_ALONG_LONGEST;
        let (lattice, plan) = lattice_in(&fixed, [[-2.0, 0.0, -2.0], [2.0, 4.0, 2.0]]);
        assert_eq!(lattice.spacing, spacing);
        assert!(plan.crop.is_some());
        // The crop plus two spacings each side across; in height the slab's
        // top half-metre the margin reaches down into, plus two spacings.
        assert_eq!(lattice.dims, [37, 9, 37]);
        let low = -2.0 - margin - 2.0 * spacing;
        assert_eq!(lattice.origin, [low, -2.0 * spacing, low]);
        for k in [5, 8] {
            let above_top = lattice.origin[1] + k as f32 * spacing - 0.5;
            assert!(
                (lattice.value([18, k, 18]) - above_top).abs() < 1e-5,
                "node {k}: {}",
                lattice.value([18, k, 18])
            );
        }

        let (other, _) = lattice_in(&fixed, [[10.0, 0.0, 10.0], [14.0, 4.0, 14.0]]);
        assert_eq!(other.origin[0], 10.0 - margin - 2.0 * spacing);

        let (whole, plan) = lattice_in(&moving, [[-2.0, 0.0, -2.0], [2.0, 4.0, 2.0]]);
        assert!(plan.crop.is_none());
        assert_eq!(
            whole.spacing,
            100.0 / crate::fluid_role::DISTANCE_NODES_ALONG_LONGEST
        );
    }

    /// Dragging a fixed collider less than the crop's margin keeps its
    /// lattice; dragging it further crops again, and live the old lattice
    /// stands in, still covering the domain, until the new one is built.
    #[test]
    fn a_dragged_fixed_collider_stays_covered() {
        let fixed = Arc::new(PreparedFluidGeometry::fixed(vec![box_mesh([
            50.0, 0.5, 50.0,
        ])]));
        let domain = [[-2.0, 0.0, -2.0], [2.0, 4.0, 2.0]];
        let at = |x: f32| LiquidBody {
            position_inv_mass: [x, -0.5, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            ..LiquidBody::default()
        };
        let mut bodies = LiquidBodies::default();
        let geometries = std::slice::from_ref(&fixed);
        assert_eq!(
            bodies.prepare(&[], geometries, &[at(0.0)], domain, 0.0625, true),
            Ok(BodiesStatus::Ready)
        );
        let (first, _) = bodies.resolved[0].clone();
        assert_eq!(
            bodies.prepare(&[], geometries, &[at(0.4)], domain, 0.0625, false),
            Ok(BodiesStatus::Ready)
        );
        assert!(
            Arc::ptr_eq(&bodies.resolved[0].0, &first),
            "a drag inside the margin keeps the lattice"
        );
        assert_eq!(
            bodies.prepare(&[], geometries, &[at(0.6)], domain, 0.0625, false),
            Ok(BodiesStatus::Ready)
        );
        let crop = bodies.resolved[0].1.crop.expect("cropped");
        assert!(
            Arc::ptr_eq(&bodies.resolved[0].0, &first) && crop[0][0] <= -2.6,
            "the old lattice stands in and covers: {crop:?}"
        );
        assert_eq!(
            bodies.prepare(&[], geometries, &[at(0.6)], domain, 0.0625, true),
            Ok(BodiesStatus::Ready)
        );
        let (moved, plan) = bodies.resolved[0].clone();
        assert!(
            !Arc::ptr_eq(&moved, &first),
            "a drag past the margin crops again"
        );
        let crop = plan.crop.expect("cropped");
        assert!(
            crop[0][0] <= -2.6 && crop[1][0] >= 1.4,
            "the new crop covers the domain: {crop:?}"
        );
    }

    /// A turned body sees the domain turned back into its own frame: a
    /// quarter turn about Y carries world X onto local Z.
    #[test]
    fn a_fixed_collider_crop_follows_its_turn() {
        let (s, c) = std::f32::consts::FRAC_PI_4.sin_cos();
        let row = LiquidBody {
            rotation: [0.0, s, 0.0, c],
            ..LiquidBody::default()
        };
        let local = local_domain(&row, [[0.0, 0.0, -1.0], [4.0, 1.0, 1.0]]);
        let expected = [[-1.0, 0.0, 0.0], [1.0, 1.0, 4.0]];
        for side in 0..2 {
            for axis in 0..3 {
                assert!(
                    (local[side][axis] - expected[side][axis]).abs() <= 1e-5,
                    "{local:?}"
                );
            }
        }
    }

    /// Two roles sharing one geometry share its atlas block; a scale change
    /// rebuilds the shapes; sources and drains are refused unless the liquid
    /// takes regions, and a fill is always refused.
    #[test]
    fn liquid_bodies_share_geometry_and_refuse_other_roles() {
        let geometry = cube();
        let mut roles = vec![
            inflow(&geometry, [0.0; 3], 0.0),
            None,
            inflow(&geometry, [1.0, 0.0, 0.0], 0.0),
        ];
        let mut bodies = LiquidBodies::with_regions();
        ready(&mut bodies, &roles);
        assert_eq!(bodies.shapes().len(), 2);
        assert_eq!(
            bodies.shapes()[0].atlas_offset,
            bodies.shapes()[1].atlas_offset
        );
        let (words, version) = (bodies.atlas().len(), bodies.version);
        ready(&mut bodies, &roles);
        assert_eq!(bodies.version, version, "unchanged roles rebuild nothing");
        roles[2].as_mut().unwrap().transform.scale = [1.0; 3];
        ready(&mut bodies, &roles);
        assert_eq!((bodies.atlas().len(), bodies.version), (words, version + 1));
        assert_eq!(bodies.shapes()[1].scale_min, [1.0; 4]);
        roles[1] = Some(FluidRole {
            kind: FluidRoleKind::Outflow,
            ..roles[0].clone().unwrap()
        });
        ready(&mut bodies, &roles);
        let mut no_regions = LiquidBodies::default();
        assert!(
            no_regions
                .prepare(&roles, &[], &[], [[0.0; 3]; 2], 0.0625, false)
                .unwrap_err()
                .contains("no Inflow or Outflow")
        );
        roles[1] = Some(FluidRole {
            kind: FluidRoleKind::InitialFill,
            ..roles[0].clone().unwrap()
        });
        assert!(
            bodies
                .prepare(&roles, &[], &[], [[0.0; 3]; 2], 0.0625, false)
                .unwrap_err()
                .contains("Inflow and Outflow roles only")
        );
    }

    /// Inflow and Outflow roles become region rows, not body rows: the code,
    /// the emitted velocity and the inherited share ride in the row, shape
    /// indices stay the role order, and coupled shapes follow the roles'.
    #[test]
    fn liquid_bodies_split_regions_from_coupled_bodies() {
        let geometry = cube();
        let hull = cube();
        let region = |kind, pos| {
            Some(FluidRole {
                kind,
                velocity: [0.0, -2.0, 1.0],
                inherit_motion: 0.5,
                ..inflow(&geometry, pos, 0.0).unwrap()
            })
        };
        let roles = vec![
            region(FluidRoleKind::Inflow, [0.0, 2.0, 0.0]),
            region(FluidRoleKind::Outflow, [1.0, 0.0, 0.0]),
        ];
        let mut bodies = LiquidBodies::with_regions();
        ready_coupled(&mut bodies, &roles, &[Arc::clone(&hull)]);
        assert_eq!(
            (bodies.count(), bodies.region_count(), bodies.shapes().len()),
            (1, 2, 3)
        );
        let mut rig = Rig::default();
        rig.frame(&mut bodies, 0.0, 3.0 * TICK, &|_| roles.clone());
        rig.frame(&mut bodies, 3.0 * TICK, 3.0 * TICK, &|_| roles.clone());
        let coupled = LiquidBody {
            accel_shape: [0.0, 0.0, 0.0, 0.0],
            ..LiquidBody::default()
        };
        let rows = bodies.rows(0, 1, &[coupled]).unwrap().to_vec();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].accel_shape[3], 2.0,
            "a coupled shape counts past the roles'"
        );
        let regions = bodies.last_region_rows();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].angular_velocity[3], REGION_INFLOW);
        assert_eq!(regions[1].angular_velocity[3], REGION_OUTFLOW);
        assert_eq!(
            (regions[0].accel_shape[3], regions[1].accel_shape[3]),
            (0.0, 1.0)
        );
        assert_eq!(regions[0].inv_inertia_x, [0.0, -2.0, 1.0, 0.5]);
        assert_eq!(regions[0].position_inv_mass[..3], [0.0, 2.0, 0.0]);
        let moved = LiquidBody {
            position_inv_mass: [1.0, 0.0, 0.0, 1.0],
            ..coupled
        };
        let (offset, written) = bodies.set_coupled_rows(0, &[moved]).unwrap();
        assert_eq!(offset, 0);
        assert_eq!(written[0].position_inv_mass, moved.position_inv_mass);
        assert_eq!(bodies.last_rows()[0].position_inv_mass[0], 1.0);
        assert_eq!(bodies.last_rows()[0].accel_shape[3], 2.0);
    }

    /// The overlay boxes are the solver's rows: each coupled body, inflow
    /// and drain at its tick-start pose with its mesh bounds scaled, and a
    /// disabled source draws nothing.
    #[test]
    fn liquid_body_gizmos_are_the_boxes_the_solver_poses() {
        let geometry = cube();
        let turned = std::f32::consts::FRAC_PI_2;
        let off = Some(FluidRole {
            enabled: false,
            ..inflow(&geometry, [3.0; 3], 0.0).unwrap()
        });
        let roles = vec![
            inflow(&geometry, [0.0, 2.0, 0.0], turned),
            Some(FluidRole {
                kind: FluidRoleKind::Outflow,
                ..inflow(&geometry, [1.0, 0.0, 0.0], 0.0).unwrap()
            }),
            off,
        ];
        let mut bodies = LiquidBodies::with_regions();
        ready_coupled(&mut bodies, &roles, &[cube()]);
        let mut rig = Rig::default();
        rig.frame(&mut bodies, 0.0, TICK, &|_| roles.clone());
        rig.frame(&mut bodies, TICK, TICK, &|_| roles.clone());
        let coupled = LiquidBody {
            position_inv_mass: [0.0, 1.0, 0.0, 0.5],
            rotation: [0.0, 0.0, 0.0, 1.0],
            ..LiquidBody::default()
        };
        bodies.rows(0, 1, &[coupled]).unwrap();
        let mut gizmos = Vec::new();
        bodies.write_gizmos(&mut gizmos);
        let families: Vec<_> = gizmos.iter().map(|gizmo| gizmo.family).collect();
        use PhysicsGizmoFamily::*;
        assert_eq!(families, [Body, Source, Drain]);
        let boxes: Vec<_> = gizmos
            .iter()
            .map(|gizmo| match gizmo.shape {
                PhysicsGizmoShape::Box {
                    center,
                    half,
                    rotation,
                } => (center, half, rotation),
                other => panic!("{other:?}"),
            })
            .collect();
        let close = |a: [f32; 3], b: [f32; 3]| (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5);
        assert_eq!((boxes[0].0, boxes[0].1), ([0.0, 1.0, 0.0], [0.5; 3]));
        // The source's yaw turns its box; the box keeps its local extents.
        assert!(close(boxes[1].0, [0.0, 2.0, 0.0]) && close(boxes[1].1, [0.2, 0.1, 0.15]));
        let x_axis = rotate(boxes[1].2, [1.0, 0.0, 0.0]);
        assert!(close(x_axis, [0.0, 0.0, -1.0]), "{x_axis:?}");
        assert!(close(boxes[2].0, [1.0, 0.0, 0.0]));
    }

    #[test]
    fn liquid_clock_vertices_refresh_the_selected_coupled_tick() {
        let geometry = cube();
        let hulls = [cube(), cube()];
        let roles_at = |time: f64| {
            let tick = (time / TICK) as f32;
            let source = FluidRole {
                kind: FluidRoleKind::Inflow,
                velocity: [1.0, -2.0, 0.5],
                inherit_motion: 0.5,
                ..inflow(&geometry, [1.0 + tick, 0.0, 0.0], tick * 0.25).unwrap()
            };
            vec![
                Some(source),
                Some(FluidRole {
                    kind: FluidRoleKind::Outflow,
                    ..inflow(&geometry, [0.0; 3], 0.0).unwrap()
                }),
            ]
        };
        let coupled_row = |y, yaw: f32, shape| LiquidBody {
            position_inv_mass: [0.5, y, -0.25, 1.0],
            rotation: [0.0, (yaw * 0.5).sin(), 0.0, (yaw * 0.5).cos()],
            linear_velocity: [1.0, -2.0, 3.0, 0.0],
            angular_velocity: [0.0, 4.0, 0.0, 0.0],
            accel_shape: [0.0, -9.81, 0.0, shape],
            ..LiquidBody::default()
        };
        let initial = [coupled_row(-40.0, 0.0, 0.0), coupled_row(0.0, 0.0, 1.0)];
        let moved = [coupled_row(0.0, 0.75, 0.0), coupled_row(-40.0, -0.5, 1.0)];
        let mut grouped = LiquidBodies::with_regions();
        let mut separate = LiquidBodies::with_regions();
        for bodies in [&mut grouped, &mut separate] {
            ready_coupled(bodies, &roles_at(0.0), &hulls);
        }
        let mut grouped_rig = Rig::default();
        grouped_rig.frame(&mut grouped, 0.0, TICK, &roles_at);
        let frame = grouped_rig.frame(&mut grouped, 2.0 * TICK, TICK, &roles_at);
        assert_eq!(frame.ticks, 2);
        grouped
            .rows(first_tick(&frame), frame.ticks, &initial)
            .unwrap();
        grouped.prepare_clock_vertices([-4.0; 3], [8.0; 3], 0);
        let before_obstacles = grouped.clock_obstacles().to_vec();
        let before_sources = grouped.clock_sources().to_vec();
        assert!(
            before_obstacles[..8]
                .iter()
                .all(|vertex| vertex.position[3] == 0.0)
        );
        assert!(
            before_obstacles[8..]
                .iter()
                .all(|vertex| vertex.position[3] == 1.0)
        );
        grouped.set_coupled_rows(1, &moved).unwrap();
        grouped.prepare_clock_vertices([-4.0; 3], [8.0; 3], 1);

        let mut separate_rig = Rig::default();
        separate_rig.frame(&mut separate, 0.0, TICK, &roles_at);
        let first = separate_rig.frame(&mut separate, TICK, TICK, &roles_at);
        separate
            .rows(first_tick(&first), first.ticks, &initial)
            .unwrap();
        let second = separate_rig.frame(&mut separate, 2.0 * TICK, TICK, &roles_at);
        assert_eq!(second.ticks, 1);
        separate
            .rows(first_tick(&second), second.ticks, &moved)
            .unwrap();
        separate.prepare_clock_vertices([-4.0; 3], [8.0; 3], 0);

        let obstacles = grouped.clock_obstacles();
        assert_eq!(obstacles.len(), 16, "two coupled hulls");
        assert_eq!(
            grouped.clock_sources().len(),
            8,
            "the drain contributes no source vertices"
        );
        assert_eq!(
            bytemuck::cast_slice::<_, u32>(obstacles),
            bytemuck::cast_slice::<_, u32>(separate.clock_obstacles())
        );
        assert_eq!(
            bytemuck::cast_slice::<_, u32>(grouped.clock_sources()),
            bytemuck::cast_slice::<_, u32>(separate.clock_sources())
        );
        assert_ne!(
            bytemuck::cast_slice::<_, u32>(&before_sources),
            bytemuck::cast_slice::<_, u32>(grouped.clock_sources()),
            "the selected tick refreshes the moving source"
        );
        assert!(
            obstacles[..8]
                .iter()
                .all(|vertex| vertex.position[3] == 1.0 && vertex.velocity[3] == 1.0),
            "the entering hull is eligible and retains its relative body row"
        );
        assert!(
            obstacles[8..]
                .iter()
                .all(|vertex| vertex.position[3] == 0.0 && vertex.velocity[3] == 2.0),
            "the leaving hull is ineligible and retains its relative body row"
        );
        assert!(
            grouped
                .clock_sources()
                .iter()
                .all(|vertex| vertex.velocity[3] == 0.0)
        );
    }

    fn initial_obstacle(vertex: GpuFlipBodyVertex) -> LiquidBodies {
        LiquidBodies {
            clock_obstacles: vec![vertex],
            ..LiquidBodies::default()
        }
    }

    fn eligible_vertex() -> GpuFlipBodyVertex {
        GpuFlipBodyVertex {
            position: [0.0, 0.0, 0.0, 1.0],
            ..Default::default()
        }
    }

    #[test]
    fn liquid_initial_obstacle_speed_bounds_translation_and_angular_radius() {
        let mut vertex = eligible_vertex();
        vertex.velocity = [3.0, 4.0, 0.0, 0.0];
        assert_eq!(
            initial_obstacle(vertex).initial_clock_obstacle_speed(0.5),
            5.0
        );
        vertex.position = [3.0, 4.0, 0.0, 1.0];
        vertex.angular_velocity = [0.0, 0.0, 2.0, 0.0];
        assert_eq!(
            initial_obstacle(vertex).initial_clock_obstacle_speed(0.5),
            15.0
        );
        vertex.angular_acceleration = [0.0, 0.0, 4.0, 0.0];
        assert_eq!(
            initial_obstacle(vertex).initial_clock_obstacle_speed(0.5),
            25.0
        );
    }

    #[test]
    fn liquid_initial_obstacle_speed_bounds_acceleration_and_deceleration() {
        for acceleration in [-4.0, 4.0] {
            let mut vertex = eligible_vertex();
            vertex.velocity = [3.0, 0.0, 0.0, 0.0];
            vertex.acceleration = [acceleration, 0.0, 0.0, 0.0];
            assert_eq!(
                initial_obstacle(vertex).initial_clock_obstacle_speed(0.5),
                5.0
            );
        }
    }

    #[test]
    fn liquid_initial_obstacle_speed_ignores_ineligible_vertices() {
        assert_eq!(
            LiquidBodies::default().initial_clock_obstacle_speed(0.5),
            0.0
        );
        let mut vertex = eligible_vertex();
        vertex.position[3] = 0.0;
        vertex.velocity[0] = f32::NAN;
        let mut bodies = initial_obstacle(vertex);
        assert_eq!(bodies.initial_clock_obstacle_speed(0.5), 0.0);
        let mut valid = eligible_vertex();
        valid.velocity[0] = 2.0;
        bodies.clock_obstacles.push(valid);
        assert_eq!(bodies.initial_clock_obstacle_speed(0.5), 2.0);
    }

    #[test]
    fn liquid_initial_obstacle_speed_rejects_nonfinite_inputs_and_overflow() {
        for interval in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                initial_obstacle(eligible_vertex()).initial_clock_obstacle_speed(interval),
                -1.0
            );
        }
        for field in 0..6 {
            for invalid in [f32::NAN, f32::INFINITY] {
                let mut vertex = eligible_vertex();
                let values = match field {
                    0 => &mut vertex.position,
                    1 => &mut vertex.centroid,
                    2 => &mut vertex.velocity,
                    3 => &mut vertex.angular_velocity,
                    4 => &mut vertex.acceleration,
                    5 => &mut vertex.angular_acceleration,
                    _ => unreachable!(),
                };
                values[0] = invalid;
                assert_eq!(
                    initial_obstacle(vertex).initial_clock_obstacle_speed(0.5),
                    -1.0,
                    "field {field}"
                );
            }
        }
        let mut vertex = eligible_vertex();
        vertex.velocity = [f32::MAX, f32::MAX, 0.0, 0.0];
        assert_eq!(
            initial_obstacle(vertex).initial_clock_obstacle_speed(0.5),
            -1.0
        );
    }

    #[test]
    fn liquid_initial_obstacle_speed_rounds_up_to_a_finite_float() {
        let mut vertex = eligible_vertex();
        vertex.velocity = [1.0, 1.0, 0.0, 0.0];
        let bound = initial_obstacle(vertex).initial_clock_obstacle_speed(0.5);
        assert!(f64::from(bound) >= 2.0f64.sqrt());
        assert!(f64::from(f32::from_bits(bound.to_bits() - 1)) < 2.0f64.sqrt());
    }

    #[test]
    fn liquid_initial_obstacle_speed_dominates_sampled_affine_point_velocities() {
        let cross = |a: [f64; 3], b: [f64; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let mut vertex = eligible_vertex();
        vertex.position = [3.0, -2.0, 5.0, 1.0];
        vertex.centroid = [1.0, 2.0, 1.0, 0.0];
        vertex.velocity = [2.0, -3.0, 1.0, 0.0];
        vertex.angular_velocity = [-0.5, 0.25, 2.0, 0.0];
        vertex.acceleration = [-4.0, 1.0, 0.5, 0.0];
        vertex.angular_acceleration = [1.0, -0.5, 0.25, 0.0];
        let interval = 1.25;
        let bound = f64::from(initial_obstacle(vertex).initial_clock_obstacle_speed(interval));
        let radius = std::array::from_fn(|axis| {
            f64::from(vertex.position[axis]) - f64::from(vertex.centroid[axis])
        });
        let angular = cross(
            std::array::from_fn(|axis| f64::from(vertex.angular_velocity[axis])),
            radius,
        );
        let angular_acceleration = cross(
            std::array::from_fn(|axis| f64::from(vertex.angular_acceleration[axis])),
            radius,
        );
        for fraction in [0.0, 0.125, 0.5, 0.875, 1.0] {
            let time = f64::from(interval) * fraction;
            let speed = (0..3)
                .map(|axis| {
                    let value = f64::from(vertex.velocity[axis])
                        + angular[axis]
                        + time
                            * (f64::from(vertex.acceleration[axis]) + angular_acceleration[axis]);
                    value * value
                })
                .sum::<f64>()
                .sqrt();
            assert!(speed <= bound, "time {time}: {speed} exceeds {bound}");
        }
    }

    /// A coupled hull joins the clock's CFL while its bounds overlap the
    /// domain: a hull falling far below the water adds nothing at any speed,
    /// and a large hull crossing the domain with every vertex outside counts.
    #[test]
    fn liquid_clock_vertices_count_a_coupled_hull_only_while_it_overlaps_the_domain() {
        let hulls = [cube(), cube_of(6.0), cube()];
        let mut bodies = LiquidBodies::default();
        ready_coupled(&mut bodies, &[], &hulls);
        let mut rig = Rig::default();
        rig.frame(&mut bodies, 0.0, TICK, &|_| Vec::new());
        let frame = rig.frame(&mut bodies, TICK, TICK, &|_| Vec::new());
        let falling = |y: f32| LiquidBody {
            position_inv_mass: [0.0, y, 0.0, 1.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            linear_velocity: [0.0, -250.0, 0.0, 0.0],
            ..LiquidBody::default()
        };
        bodies
            .rows(
                first_tick(&frame),
                frame.ticks,
                &[falling(0.0), falling(0.0), falling(-40.0)],
            )
            .unwrap();
        bodies.prepare_clock_vertices([-4.0; 3], [8.0; 3], 0);
        let obstacles = bodies.clock_obstacles();
        assert_eq!(obstacles.len(), 24, "three cubes");
        assert!(
            obstacles[..8].iter().all(|v| v.position[3] == 1.0),
            "a hull inside the domain counts"
        );
        assert!(
            obstacles[8..16]
                .iter()
                .all(|v| v.position[3] == 1.0 && v.position[1].abs() > 4.0),
            "a hull enclosing the domain counts though every vertex is outside"
        );
        assert!(
            obstacles[16..].iter().all(|v| v.position[3] == 0.0),
            "a hull falling below the domain never counts"
        );
    }

    fn open_plane(half: f32) -> Arc<PreparedFluidGeometry> {
        Arc::new(PreparedFluidGeometry::new(vec![
            manifold_physics::TriangleMesh {
                vertices: vec![
                    [-half, 0.0, -half],
                    [half, 0.0, -half],
                    [half, 0.0, half],
                    [-half, 0.0, half],
                ],
                triangles: vec![[0, 2, 1], [0, 3, 2]],
            },
        ]))
    }

    /// The thin-collider check sees a shell's wall, not its mesh's bounds:
    /// a 1 m box missing its lid is as thin as its shell, two cells, while a
    /// closed box is as thin as its scaled bounds.
    #[test]
    fn liquid_thinnest_counts_a_shell_wall_not_its_bounds() {
        let closed = cube_of(0.5);
        let plan = closed.lattice_plan(0.1, [1.0; 3], None).unwrap();
        assert_eq!(thinnest(&plan, [2.0, 1.0, 3.0]), 1.0);
        let mut meshes = closed.meshes.clone();
        meshes[0].triangles.truncate(10);
        let lidless = PreparedFluidGeometry::new(meshes);
        let plan = lidless.lattice_plan(0.1, [1.0; 3], None).unwrap();
        assert!(plan.open);
        assert!(
            (thinnest(&plan, [1.0; 3]) - 0.2).abs() < 1e-6,
            "{}",
            thinnest(&plan, [1.0; 3])
        );
    }

    /// An open collider's lattice follows the liquid's cells: a new cell
    /// size builds a finer one, which offline waits for and live picks up
    /// once ready. A closed source's lattice is the same at any cells, so
    /// a new cell size rebuilds nothing.
    #[test]
    fn liquid_open_source_lattice_follows_the_cells() {
        let plane = open_plane(1.0);
        let roles = [inflow(&plane, [0.0; 3], 0.0)];
        let mut bodies = LiquidBodies::with_regions();
        let spacing = |cell: f32| {
            plane
                .lattice_plan(cell, [0.4, 0.2, 0.3], None)
                .unwrap()
                .spacing
        };
        assert_eq!(
            bodies.prepare(&roles, &[], &[], [[0.0; 3]; 2], 0.1, true),
            Ok(BodiesStatus::Ready)
        );
        assert_eq!(bodies.shapes()[0].origin_spacing[3], 2.0 / 32.0);
        let mut polls = 0u32;
        while bodies.shapes()[0].origin_spacing[3] != spacing(0.01) {
            assert_eq!(
                bodies.prepare(&roles, &[], &[], [[0.0; 3]; 2], 0.01, false),
                Ok(BodiesStatus::Ready),
                "live keeps the last lattice"
            );
            polls += 1;
            assert!(polls < 6000, "the finer lattice never arrived");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            bodies.prepare(&roles, &[], &[], [[0.0; 3]; 2], 0.005, true),
            Ok(BodiesStatus::Ready)
        );
        assert_eq!(
            bodies.shapes()[0].origin_spacing[3],
            spacing(0.005),
            "offline waits for the plan's lattice"
        );

        let cube = cube_of(0.5);
        let roles = [inflow(&cube, [0.0; 3], 0.0)];
        assert_eq!(
            bodies.prepare(&roles, &[], &[], [[0.0; 3]; 2], 0.1, true),
            Ok(BodiesStatus::Ready)
        );
        let version = bodies.version;
        assert_eq!(
            bodies.prepare(&roles, &[], &[], [[0.0; 3]; 2], 0.01, false),
            Ok(BodiesStatus::Ready)
        );
        assert_eq!(bodies.version, version);
    }

    /// An open source's scale animated within one eighth-octave step keeps
    /// its lattice: no new plan, no build, only the shapes' scale moves. A
    /// scale a step away builds a new lattice.
    #[test]
    fn liquid_open_source_scale_within_a_step_keeps_its_lattice() {
        let plane = open_plane(1.0);
        let posed = |scale: f32| {
            let mut role = inflow(&plane, [0.0; 3], 0.0);
            if let Some(role) = &mut role {
                role.transform.scale = [scale; 3];
            }
            [role]
        };
        let mut bodies = LiquidBodies::with_regions();
        assert_eq!(
            bodies.prepare(&posed(1.0), &[], &[], [[0.0; 3]; 2], 0.05, true),
            Ok(BodiesStatus::Ready)
        );
        let lattice = Arc::clone(&bodies.used[0]);
        for k in 0..40 {
            let scale = 1.0 + 0.04 * (k as f32 * 0.3).sin();
            assert_eq!(
                bodies.prepare(&posed(scale), &[], &[], [[0.0; 3]; 2], 0.05, true),
                Ok(BodiesStatus::Ready)
            );
            assert!(
                Arc::ptr_eq(&bodies.used[0], &lattice),
                "scale {scale} replanned"
            );
            assert!(
                bodies.lattices[0].building.is_none(),
                "scale {scale} started a build"
            );
            assert_eq!(bodies.shapes()[0].scale_min[3], scale);
        }
        assert_eq!(
            bodies.prepare(&posed(1.2), &[], &[], [[0.0; 3]; 2], 0.05, true),
            Ok(BodiesStatus::Ready)
        );
        assert!(
            !Arc::ptr_eq(&bodies.used[0], &lattice),
            "a step away builds a new lattice"
        );
    }
}

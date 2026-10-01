//! A GPU liquid's bodies (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.6;
//! GPU_MPM_SOLVER_DESIGN.md D11): Collider roles become one [`LiquidBody`]
//! row per body per tick from their authored motion, and their geometry's
//! distance lattices one packed atlas with a [`LiquidShape`] per role.
//! Motion is sampled from display-frame observations through the shared
//! `InputHistory`, as FLIP roles are.

use std::sync::Arc;

use manifold_physics::Seconds;
use manifold_physics::input::{InputHistory, Timestamped, input_span, input_span_before};

use crate::node_graph::channel_names::well_known;
use crate::node_graph::fluid::TICK;
use crate::node_graph::fluid_role::{
    DistanceState, FluidRole, FluidRoleKind, MAX_FLUID_ROLES, PreparedFluidGeometry,
};
use crate::node_graph::physics::pose_from_transform;
use crate::node_graph::ports::{ChannelElementType, ChannelSpec, KnownItem};
use crate::node_graph::transform::Transform;

/// Body poses on the GPU: rotation, the constant-angular-velocity turn,
/// material velocity at a point. Matches [`body_pose_at`].
pub(crate) const LIQUID_POSE: &str = include_str!("../primitives/shaders/liquid_pose.wgsl");
/// Collider lattice sampling; needs [`LIQUID_POSE`] and the including body's
/// `liquid_atlas_half`.
pub(crate) const LIQUID_COLLIDER: &str = include_str!("../primitives/shaders/liquid_collider.wgsl");
/// Body ownership of faces, face centres and the rigid basis for the atoms
/// that couple bodies into the GPU FLIP pressure solve.
pub(crate) const SOLID_BODY_FACES: &str = include_str!("../primitives/shaders/solid_body_faces.wgsl");

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
    ChannelSpec { name: well_known::POSITION_INV_MASS, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::ROTATION, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::LINEAR_VELOCITY, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::ANGULAR_VELOCITY, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::INV_INERTIA_X, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::INV_INERTIA_Y, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::INV_INERTIA_Z, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::ACCEL_SHAPE, ty: ChannelElementType::Vec4F },
];

impl KnownItem for LiquidBody {
    const SPECS: &'static [ChannelSpec] = LIQUID_BODY_SPECS;
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
    ChannelSpec { name: well_known::ORIGIN_SPACING, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::DIMS_X, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::DIMS_Y, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::DIMS_Z, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::ATLAS_OFFSET, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::SCALE_MIN, ty: ChannelElementType::Vec4F },
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

/// A collider thinner than this many cells along any axis can let liquid
/// through between substeps (D11's thinness warning).
pub const THIN_COLLIDER_CELLS: f32 = 2.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Controls {
    transform: Transform,
    enabled: bool,
    friction: f32,
}

impl Controls {
    fn from_role(role: &FluidRole) -> Self {
        Self { transform: role.transform, enabled: role.enabled, friction: role.friction }
    }

    /// As FLIP roles interpolate: position and Euler angles lerp, a switch
    /// takes effect at the later sample.
    fn interpolate(self, next: Self, alpha: f32) -> Self {
        let lerp = |a: f32, b: f32| a + alpha * (b - a);
        Self {
            transform: Transform {
                pos: std::array::from_fn(|i| lerp(self.transform.pos[i], next.transform.pos[i])),
                rot_euler: std::array::from_fn(|i| lerp(self.transform.rot_euler[i], next.transform.rot_euler[i])),
                ..self.transform
            },
            enabled: if alpha >= 1.0 { next.enabled } else { self.enabled },
            friction: lerp(self.friction, next.friction),
        }
    }
}

#[derive(Clone, Copy)]
struct Sample {
    time: Seconds,
    controls: [Controls; MAX_FLUID_ROLES],
}

impl Timestamped for Sample {
    fn time(&self) -> Seconds {
        self.time
    }
}

struct BodyRole {
    slot: usize,
    geometry: Arc<PreparedFluidGeometry>,
    scale: [f32; 3],
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
    history: Option<InputHistory<Sample>>,
    epoch: Option<u32>,
    shapes: Vec<LiquidShape>,
    atlas: Vec<u32>,
    /// Bumped whenever `shapes` or `atlas` is rebuilt.
    pub version: u64,
    rows: Vec<LiquidBody>,
    warned_thin: bool,
}

impl LiquidBodies {
    /// Roles and coupled bodies: the rows per tick.
    pub fn count(&self) -> usize {
        self.roles.len() + self.coupled.len()
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
    /// the centre of mass, unscaled). Collider roles are simulated; fills,
    /// inflows and drains are refused rather than ignored. A new geometry or
    /// scale rebuilds the shapes, and the atlas when the set of geometries
    /// changed. Live, a distance lattice still building leaves the bodies
    /// Pending; `wait` (offline) waits for it, so no simulated time goes by
    /// while it builds.
    pub fn prepare(
        &mut self,
        roles: &[Option<FluidRole>],
        coupled: &[Arc<PreparedFluidGeometry>],
        cell_size: f32,
        wait: bool,
    ) -> Result<BodiesStatus, String> {
        let lattice = |geometry: &Arc<PreparedFluidGeometry>| {
            if wait { geometry.wait_distance_lattice() } else { geometry.distance_lattice() }
        };
        let occupied = roles.iter().flatten().count();
        if occupied + coupled.len() > MAX_FLUID_ROLES {
            return Err(format!(
                "Liquid: {occupied} roles and {} coupled bodies exceed the {MAX_FLUID_ROLES} bodies a liquid holds",
                coupled.len()
            ));
        }
        for (index, geometry) in coupled.iter().enumerate() {
            match lattice(geometry) {
                DistanceState::Pending => return Ok(BodiesStatus::Pending),
                DistanceState::Failed(error) => return Err(format!("Liquid: coupled body {index}: {error}")),
                DistanceState::Ready(_) => {}
            }
        }
        let mut same = self.coupled.len() == coupled.len()
            && self.coupled.iter().zip(coupled).all(|(a, b)| Arc::ptr_eq(a, b));
        let mut count = 0;
        for (slot, role) in roles.iter().enumerate() {
            let Some(role) = role else { continue };
            if role.kind != FluidRoleKind::Collider {
                return Err(format!(
                    "Liquid: role {slot} is {:?}; the live liquid simulates Collider roles only until sources and drains arrive",
                    role.kind
                ));
            }
            if role.transform.scale.iter().any(|s| !(s.is_finite() && *s > 0.0)) {
                return Err(format!("Liquid: role {slot} scale must be finite and positive"));
            }
            match lattice(&role.geometry) {
                DistanceState::Pending => return Ok(BodiesStatus::Pending),
                DistanceState::Failed(error) => return Err(format!("Liquid: role {slot}: {error}")),
                DistanceState::Ready(_) => {}
            }
            same &= self.roles.get(count).is_some_and(|known| {
                known.slot == slot && Arc::ptr_eq(&known.geometry, &role.geometry) && known.scale == role.transform.scale
            });
            count += 1;
        }
        if same && count == self.roles.len() {
            return Ok(BodiesStatus::Ready);
        }
        self.rebuild(roles, coupled, cell_size);
        Ok(BodiesStatus::Ready)
    }

    fn rebuild(&mut self, roles: &[Option<FluidRole>], coupled: &[Arc<PreparedFluidGeometry>], cell_size: f32) {
        self.roles.clear();
        self.shapes.clear();
        self.atlas.clear();
        let mut placed: Vec<(Arc<PreparedFluidGeometry>, LiquidShape)> = Vec::new();
        let role_shapes = roles.iter().enumerate().filter_map(|(slot, role)| {
            role.as_ref().map(|role| (format!("collider role {slot}"), Some(slot), &role.geometry, role.transform.scale))
        });
        let coupled_shapes = coupled
            .iter()
            .enumerate()
            .map(|(index, geometry)| (format!("coupled body {index}"), None, geometry, [1.0; 3]));
        for (name, slot, geometry, scale) in role_shapes.chain(coupled_shapes).collect::<Vec<_>>() {
            let DistanceState::Ready(lattice) = geometry.distance_lattice() else {
                unreachable!("prepare checked every lattice");
            };
            let base = match placed.iter().find(|(known, _)| Arc::ptr_eq(known, geometry)) {
                Some((_, shape)) => *shape,
                None => {
                    let shape = LiquidShape {
                        origin_spacing: [lattice.origin[0], lattice.origin[1], lattice.origin[2], lattice.spacing],
                        dims_x: lattice.dims[0],
                        dims_y: lattice.dims[1],
                        dims_z: lattice.dims[2],
                        atlas_offset: self.atlas.len() as u32 * 2,
                        scale_min: [1.0; 4],
                    };
                    pack_distance_atlas(&lattice.values, &mut self.atlas);
                    placed.push((Arc::clone(geometry), shape));
                    shape
                }
            };
            let smallest = scale[0].min(scale[1]).min(scale[2]);
            self.shapes.push(LiquidShape { scale_min: [scale[0], scale[1], scale[2], smallest], ..base });
            // The geometry's extent without the lattice padding, scaled.
            let thinnest = (0..3)
                .map(|axis| {
                    let nodes = [lattice.dims[0], lattice.dims[1], lattice.dims[2]][axis];
                    ((nodes - 1) as f32 * lattice.spacing - 4.0 * lattice.spacing) * scale[axis]
                })
                .fold(f32::INFINITY, f32::min);
            if thinnest < THIN_COLLIDER_CELLS * cell_size && !self.warned_thin {
                self.warned_thin = true;
                log::warn!(
                    "[liquid] {name} is {thinnest:.3} m thick, under {THIN_COLLIDER_CELLS} cells ({:.3} m); liquid may leak through it",
                    THIN_COLLIDER_CELLS * cell_size
                );
            }
            if let Some(slot) = slot {
                self.roles.push(BodyRole { slot, geometry: Arc::clone(geometry), scale });
            }
        }
        self.coupled.clear();
        self.coupled.extend(coupled.iter().cloned());
        self.version += 1;
    }

    /// Record this frame's controls at the simulation time the frame reached
    /// (`target_time`); a new epoch starts a new history.
    pub fn observe(
        &mut self,
        roles: &[Option<FluidRole>],
        epoch: u32,
        target_time: f64,
        consumed_until: f64,
    ) -> Result<(), String> {
        if self.epoch != Some(epoch) {
            self.epoch = Some(epoch);
            if let Some(history) = &mut self.history {
                history.clear();
            }
        }
        let history = match &mut self.history {
            Some(history) => history,
            None => self.history.insert(InputHistory::with_growing_capacity(8).map_err(|e| e.to_string())?),
        };
        let mut controls = [Controls::default(); MAX_FLUID_ROLES];
        for (slot, role) in roles.iter().enumerate().take(MAX_FLUID_ROLES) {
            if let Some(role) = role {
                controls[slot] = Controls::from_role(role);
            }
        }
        history
            .record(Sample { time: Seconds(target_time), controls }, Seconds(consumed_until))
            .map(|_| ())
            .map_err(|e| format!("Liquid: {e}"))
    }

    /// One row per body for each of this frame's ticks, `first_tick..`, tick
    /// major: each role's pose at the tick's start and the velocities that
    /// reach the pose at its end, then `coupled`, the coupled bodies'
    /// tick-start state. Every tick gets the same coupled rows; an offline
    /// frame of several coupled ticks rewrites each later tick's with
    /// [`Self::set_coupled_rows`] once Box3D has stepped the tick before. A
    /// coupled row's shape index counts from the first coupled body, or is −1.
    /// Consumed samples are pruned afterwards.
    pub fn rows(&mut self, first_tick: u64, ticks: u32, coupled: &[LiquidBody]) -> &[LiquidBody] {
        self.rows.clear();
        let Some(history) = &mut self.history else {
            return &self.rows;
        };
        debug_assert_eq!(coupled.len(), self.coupled.len(), "one row per prepared coupled body");
        let offset = self.roles.len() as f32;
        for tick in first_tick..first_tick + u64::from(ticks) {
            let start_time = Seconds(tick as f64 * TICK);
            let end_time = Seconds((tick + 1) as f64 * TICK);
            for (index, role) in self.roles.iter().enumerate() {
                let at = |span: Option<manifold_physics::input::InputSpan<'_, Sample>>| {
                    let span = span.expect("observe before rows");
                    span.before.controls[role.slot].interpolate(span.after.controls[role.slot], span.alpha)
                };
                let start = at(input_span(history.iter(), start_time));
                let end = at(input_span_before(history.iter(), end_time));
                self.rows.push(body_row(start, end, index as f32));
            }
            self.rows.extend(coupled.iter().map(|row| coupled_row(row, offset)));
        }
        let _ = history.prune_before(Seconds((first_tick + u64::from(ticks)) as f64 * TICK));
        &self.rows
    }

    /// Replace tick `tick`'s coupled rows (counted from this frame's first
    /// tick) with `coupled`; returns the rewritten rows and their byte offset
    /// in [`Self::last_rows`].
    pub fn set_coupled_rows(&mut self, tick: usize, coupled: &[LiquidBody]) -> Result<(u64, &[LiquidBody]), String> {
        let count = self.count();
        let start = tick * count + self.roles.len();
        if coupled.len() != self.coupled.len() || start + coupled.len() > self.rows.len() {
            return Err(format!("Liquid coupling: tick {tick} has no coupled rows this frame"));
        }
        let offset = self.roles.len() as f32;
        let rows = &mut self.rows[start..start + coupled.len()];
        for (row, body) in rows.iter_mut().zip(coupled) {
            *row = coupled_row(body, offset);
        }
        Ok(((start * std::mem::size_of::<LiquidBody>()) as u64, rows))
    }
}

/// A coupled body's row with its shape index moved past the roles' shapes.
fn coupled_row(row: &LiquidBody, offset: f32) -> LiquidBody {
    let shape = row.accel_shape[3];
    LiquidBody {
        accel_shape: [row.accel_shape[0], row.accel_shape[1], row.accel_shape[2], if shape >= 0.0 { shape + offset } else { -1.0 }],
        ..*row
    }
}

/// The row for one tick: `start`'s pose, the linear velocity from `start` to
/// `end`, and the angular velocity of the shortest turn between them.
fn body_row(start: Controls, end: Controls, shape: f32) -> LiquidBody {
    let tick = TICK as f32;
    let a = pose_from_transform(start.transform);
    let b = pose_from_transform(end.transform);
    let velocity: [f32; 3] = std::array::from_fn(|i| (b.position[i] - a.position[i]) / tick);
    // rel = b ⊗ a⁻¹, the world-frame turn over the tick.
    let (p, q) = (b.rotation, [-a.rotation[0], -a.rotation[1], -a.rotation[2], a.rotation[3]]);
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
        linear_velocity: [velocity[0], velocity[1], velocity[2], start.friction.clamp(0.0, 1.0)],
        angular_velocity: [angular[0], angular[1], angular[2], 0.0],
        accel_shape: [0.0, 0.0, 0.0, if start.enabled { shape } else { -1.0 }],
        ..LiquidBody::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::fluid_role::PreparedFluidGeometry;
    use crate::node_graph::ports::std430_stride;

    fn cube() -> Arc<PreparedFluidGeometry> {
        let v = |x: usize, y: usize, z: usize| [[-0.5f32, 0.5][x], [-0.5, 0.5][y], [-0.5, 0.5][z]];
        Arc::new(PreparedFluidGeometry::new(vec![manifold_physics::TriangleMesh {
            vertices: vec![
                v(0, 0, 0), v(1, 0, 0), v(1, 1, 0), v(0, 1, 0),
                v(0, 0, 1), v(1, 0, 1), v(1, 1, 1), v(0, 1, 1),
            ],
            triangles: vec![
                [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
                [0, 1, 5], [0, 5, 4], [2, 3, 7], [2, 7, 6],
                [1, 2, 6], [1, 6, 5], [0, 4, 7], [0, 7, 3],
            ],
        }]))
    }

    fn collider(geometry: &Arc<PreparedFluidGeometry>, pos: [f32; 3], yaw: f32) -> Option<FluidRole> {
        Some(FluidRole {
            geometry: Arc::clone(geometry),
            kind: FluidRoleKind::Collider,
            transform: Transform { pos, rot_euler: [0.0, yaw, 0.0], scale: [0.4, 0.2, 0.3], ..Transform::default() },
            enabled: true,
            velocity: [0.0; 3],
            inherit_motion: 0.0,
            friction: 0.25,
        })
    }

    fn ready(bodies: &mut LiquidBodies, roles: &[Option<FluidRole>]) {
        ready_coupled(bodies, roles, &[]);
    }

    fn ready_coupled(bodies: &mut LiquidBodies, roles: &[Option<FluidRole>], coupled: &[Arc<PreparedFluidGeometry>]) {
        let start = std::time::Instant::now();
        while bodies.prepare(roles, coupled, 0.0625, false).expect("colliders") == BodiesStatus::Pending {
            assert!(start.elapsed().as_secs() < 30, "the lattice never arrived");
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

    /// A collider observed at two display frames 1/30 s apart runs two ticks
    /// between them: each tick's row starts where the authored motion is at
    /// the tick's start, and its velocities land on the pose at its end.
    #[test]
    fn liquid_body_rows_follow_authored_motion() {
        let geometry = cube();
        let mut roles = vec![None; 3];
        roles[2] = collider(&geometry, [0.0, 1.0, 0.0], 0.0);
        let mut bodies = LiquidBodies::default();
        ready(&mut bodies, &roles);
        assert_eq!(bodies.count(), 1);
        assert_eq!(bodies.shapes()[0].scale_min, [0.4, 0.2, 0.3, 0.2]);
        assert_eq!(bodies.shapes()[0].atlas_offset, 0);
        let words = bodies.atlas().len();
        assert_eq!(words, (37usize.pow(3)).div_ceil(2));
        bodies.observe(&roles, 0, 0.0, 0.0).unwrap();
        roles[2] = collider(&geometry, [0.3, 1.0, -0.15], 0.6);
        bodies.observe(&roles, 0, 2.0 * TICK, 0.0).unwrap();
        let rows = bodies.rows(0, 2, &[]).to_vec();
        assert_eq!(rows.len(), 2);
        let tick = TICK as f32;
        for (k, row) in rows.iter().enumerate() {
            let at = |t: f32| {
                let alpha = t / (2.0 * tick);
                pose_from_transform(Transform {
                    pos: [0.3 * alpha, 1.0, -0.15 * alpha],
                    rot_euler: [0.0, 0.6 * alpha, 0.0],
                    ..Transform::default()
                })
            };
            let (start, end) = (at(k as f32 * tick), at((k + 1) as f32 * tick));
            assert!((0..3).all(|i| (row.position_inv_mass[i] - start.position[i]).abs() < 1e-6));
            let (position, rotation) = body_pose_at(row, tick);
            assert!((0..3).all(|i| (position[i] - end.position[i]).abs() < 1e-5), "{position:?} vs {:?}", end.position);
            let dot: f32 = (0..4).map(|i| rotation[i] * end.rotation[i]).sum();
            assert!(dot.abs() > 1.0 - 1e-6, "tick {k}: {rotation:?} vs {:?}", end.rotation);
            assert_eq!(row.linear_velocity[3], 0.25);
            assert_eq!(row.accel_shape[3], 0.0);
        }
        // Consumed samples go; the last stays as the next frame's bracket.
        roles[2].as_mut().unwrap().enabled = false;
        bodies.observe(&roles, 0, 3.0 * TICK, 2.0 * TICK).unwrap();
        let rows = bodies.rows(2, 1, &[]).to_vec();
        assert_eq!(rows[0].accel_shape[3], 0.0, "a switch takes effect at the later sample");
        bodies.observe(&roles, 0, 4.0 * TICK, 3.0 * TICK).unwrap();
        assert_eq!(bodies.rows(3, 1, &[])[0].accel_shape[3], -1.0);
    }

    /// Coupled bodies' shapes follow the roles' at scale 1, and their rows
    /// follow the roles' each tick with shape indices past the roles'.
    #[test]
    fn liquid_bodies_append_coupled_bodies() {
        let geometry = cube();
        let hull = cube();
        let roles = vec![collider(&geometry, [0.0; 3], 0.0), None];
        let mut bodies = LiquidBodies::default();
        ready_coupled(&mut bodies, &roles, &[Arc::clone(&hull), Arc::clone(&hull)]);
        assert_eq!(bodies.count(), 3);
        assert_eq!(bodies.shapes().len(), 3);
        assert_eq!(bodies.shapes()[1].scale_min, [1.0; 4]);
        assert_ne!(bodies.shapes()[0].atlas_offset, bodies.shapes()[1].atlas_offset);
        assert_eq!(bodies.shapes()[1].atlas_offset, bodies.shapes()[2].atlas_offset);
        let version = bodies.version;
        ready_coupled(&mut bodies, &roles, &[Arc::clone(&hull), Arc::clone(&hull)]);
        assert_eq!(bodies.version, version, "unchanged hulls rebuild nothing");
        bodies.observe(&roles, 0, TICK, 0.0).unwrap();
        let body = |shape: f32| LiquidBody { position_inv_mass: [0.0, 1.0, 0.0, 0.5], accel_shape: [0.0, -9.81, 0.0, shape], ..LiquidBody::default() };
        let rows = bodies.rows(0, 1, &[body(0.0), body(-1.0)]).to_vec();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].accel_shape, [0.0, -9.81, 0.0, 1.0]);
        assert_eq!(rows[2].accel_shape[3], -1.0);
        assert_eq!(rows[1].position_inv_mass, [0.0, 1.0, 0.0, 0.5]);
        let many: Vec<_> = (0..MAX_FLUID_ROLES).map(|_| Arc::clone(&hull)).collect();
        assert!(bodies.prepare(&roles, &many, 0.0625, false).unwrap_err().contains("exceed"));
    }

    /// Offline, the first prepare of fresh geometry waits for its distance
    /// lattices and is Ready; live, the same call is Pending.
    #[test]
    fn liquid_bodies_offline_prepare_waits_for_the_lattice() {
        let roles = vec![collider(&cube(), [0.0; 3], 0.0)];
        let mut live = LiquidBodies::default();
        assert_eq!(live.prepare(&roles, &[], 0.0625, false), Ok(BodiesStatus::Pending));
        let roles = vec![collider(&cube(), [0.0; 3], 0.0)];
        let mut offline = LiquidBodies::default();
        assert_eq!(offline.prepare(&roles, &[cube()], 0.0625, true), Ok(BodiesStatus::Ready));
        assert_eq!(offline.count(), 2);
    }

    /// Two roles sharing one geometry share its atlas block; a scale change
    /// rebuilds the shapes; non-collider roles are refused.
    #[test]
    fn liquid_bodies_share_geometry_and_refuse_other_roles() {
        let geometry = cube();
        let mut roles = vec![collider(&geometry, [0.0; 3], 0.0), None, collider(&geometry, [1.0, 0.0, 0.0], 0.0)];
        let mut bodies = LiquidBodies::default();
        ready(&mut bodies, &roles);
        assert_eq!(bodies.shapes().len(), 2);
        assert_eq!(bodies.shapes()[0].atlas_offset, bodies.shapes()[1].atlas_offset);
        let (words, version) = (bodies.atlas().len(), bodies.version);
        ready(&mut bodies, &roles);
        assert_eq!(bodies.version, version, "unchanged roles rebuild nothing");
        roles[2].as_mut().unwrap().transform.scale = [1.0; 3];
        ready(&mut bodies, &roles);
        assert_eq!((bodies.atlas().len(), bodies.version), (words, version + 1));
        assert_eq!(bodies.shapes()[1].scale_min, [1.0; 4]);
        roles[1] = Some(FluidRole { kind: FluidRoleKind::Inflow, ..roles[0].clone().unwrap() });
        assert!(bodies.prepare(&roles, &[], 0.0625, false).unwrap_err().contains("Collider roles only"));
    }
}

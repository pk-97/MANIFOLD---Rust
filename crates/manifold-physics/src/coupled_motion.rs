//! The one motion law a body coupled to a liquid follows inside a tick, and
//! its response to the liquid's pressure while it stays on its supports
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` D15, D16). The liquid places the body
//! by the law while it solves; Box3D, handed the liquid's reaction as a
//! steady force over the step (D17), ends the tick on it exactly when no
//! contact, damping, motion lock or speed cap acts. `liquid_pose.wgsl` is the
//! GPU twin.

use crate::stepping::box3d_substep_count;
use crate::{BodyDynamics, Seconds};

/// Support points a body's prediction reads: four faces' corners.
pub const MAX_SUPPORT_POINTS: usize = 16;

/// Projected Gauss–Seidel sweeps over the support rows.
pub const PROJECTION_SWEEPS: usize = 16;

/// A support stays closed while the predicted motion leaves it slower than
/// this, m/s.
pub const HELD_SPEED: f32 = 1e-3;

/// A held row adds a direction to the held space when at least this share
/// of it is new after the directions before it: a face's four corners hold
/// three.
pub const RANK_TOLERANCE: f32 = 1e-3;

/// A point sticks while its friction stays this far inside the cone.
pub const STICK_MARGIN: f32 = 0.999;

/// A touching contact of a body with a static or kinematic support.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SupportPoint {
    /// From the body's centre of mass to the contact, world frame.
    pub lever: [f32; 3],
    /// Unit, out of the support into the body.
    pub normal: [f32; 3],
    /// Coulomb friction, mixed as the world mixes it.
    pub friction: f32,
    /// The support's velocity at the contact.
    pub support_velocity: [f32; 3],
}

/// A body's touching support points, and how many a read kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SupportCount {
    pub kept: usize,
    pub found: usize,
}

/// The support points a body stays on, held through a pressure solve.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    /// Bit i: point i stays closed.
    pub closed: u32,
    /// Bit i: point i also stays put along its surface, its friction inside
    /// the cone.
    pub stuck: u32,
}

/// A coupled body at the start of a tick, as Box3D holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoupledStart {
    /// Centre of mass.
    pub position: [f32; 3],
    /// Unit quaternion xyzw, body to world.
    pub rotation: [f32; 4],
    pub linear_velocity: [f32; 3],
    pub angular_velocity: [f32; 3],
    pub inverse_mass: f32,
    /// World frame, row-major.
    pub inverse_inertia: [[f32; 3]; 3],
    /// Box3D's external acceleration: gravity and queued forces.
    pub linear_acceleration: [f32; 3],
    pub angular_acceleration: [f32; 3],
}

impl CoupledStart {
    pub fn new(dynamics: &BodyDynamics, rotation: [f32; 4]) -> Self {
        Self {
            position: dynamics.center_of_mass,
            rotation,
            linear_velocity: dynamics.linear_velocity,
            angular_velocity: dynamics.angular_velocity,
            inverse_mass: dynamics.inverse_mass,
            inverse_inertia: dynamics.inverse_inertia,
            linear_acceleration: dynamics.external_linear_acceleration,
            angular_acceleration: dynamics.external_angular_acceleration,
        }
    }
}

/// A coupled body's predicted state inside a tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoupledState {
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub linear_velocity: [f32; 3],
    pub angular_velocity: [f32; 3],
    /// The supports the body stays on now.
    pub held: Held,
}

/// Box3D's substep for a tick of `dt`: the h in the law.
pub fn coupled_substep(dt: Seconds) -> f32 {
    (dt.0 / f64::from(box3d_substep_count(dt).value)) as f32
}

/// The body `t` seconds into the tick, with `linear_push` and `angular_push`
/// the reaction impulse the liquid has put on it so far. The supports first
/// stop any motion into them at the tick's start, as Box3D's first substep
/// does: (v0, ω0) becomes (v0', ω0'). The known increment Δv = a·t + P/m (and
/// Δω = α·t + I⁻¹·L) then loses its motion into them; v = v0' + Δv and
/// x = x0 + v0'·t + ½·Δv·(t + h), the symplectic Euler sum over substeps of
/// h. Angular follows the same form as a world-frame rotation vector.
pub fn coupled_state_at(
    start: &CoupledStart,
    supports: &[SupportPoint],
    linear_push: [f32; 3],
    angular_push: [f32; 3],
    t: f32,
    h: f32,
) -> CoupledState {
    let lead = 0.5 * (t + h);
    let i = &start.inverse_inertia;
    let (mut v0, mut w0) = (start.linear_velocity, start.angular_velocity);
    let (mut dv, mut dw) = ([0.0; 3], [0.0; 3]);
    project_off(start, supports, v0, w0, &mut dv, &mut dw);
    for r in 0..3 {
        v0[r] += dv[r];
        w0[r] += dw[r];
    }
    let mut dv: [f32; 3] = std::array::from_fn(|r| start.linear_acceleration[r] * t + start.inverse_mass * linear_push[r]);
    let mut dw: [f32; 3] = std::array::from_fn(|r| start.angular_acceleration[r] * t + dot(i[r], angular_push));
    let held = project_off(start, supports, v0, w0, &mut dv, &mut dw);
    let x0 = start.position;
    let turn: [f32; 3] = std::array::from_fn(|r| w0[r] * t + dw[r] * lead);
    CoupledState {
        position: std::array::from_fn(|r| x0[r] + v0[r] * t + dv[r] * lead),
        rotation: turned(start.rotation, turn),
        linear_velocity: std::array::from_fn(|r| v0[r] + dv[r]),
        angular_velocity: std::array::from_fn(|r| w0[r] + dw[r]),
        held,
    }
}

/// A body's velocity change per unit impulse, (Δv, Δω) for a linear and an
/// angular impulse (J, L): 6 × 6 symmetric, upper triangle by rows.
pub type Mobility = [f32; 21];

/// Where entry (i, j) of a 6 × 6 [`Mobility`] sits.
pub const fn mobility_index(i: usize, j: usize) -> usize {
    let (i, j) = if i <= j { (i, j) } else { (j, i) };
    i * (13 - i) / 2 + j - i
}

/// The body's response to the liquid's pressure while it stays on its `held`
/// supports (D16): every held row is closed both ways for the whole pressure
/// solve, so the solve's operator keeps its symmetry. With M⁻¹ = L·Lᵀ and P
/// the projector off the held rows whitened by Lᵀ (modified Gram–Schmidt,
/// run twice), the mobility is (L·P)·(L·P)ᵀ: symmetric and positive
/// semidefinite however it rounds, and an impulse moves no held row. Nothing
/// held gives M⁻¹ itself.
pub fn constrained_mobility(start: &CoupledStart, supports: &[SupportPoint], held: Held) -> Mobility {
    let mut free = [[0.0f32; 6]; 6];
    for r in 0..3 {
        free[r][r] = start.inverse_mass;
        for c in 0..3 {
            free[3 + r][3 + c] = start.inverse_inertia[r][c];
        }
    }
    let mut rows = [[0.0f32; 6]; 3 * MAX_SUPPORT_POINTS];
    let mut count = 0;
    for (p, point) in supports.iter().take(MAX_SUPPORT_POINTS).enumerate() {
        let directions = directions(point.normal);
        let kept = [held.closed >> p & 1 == 1, held.stuck >> p & 1 == 1, held.stuck >> p & 1 == 1];
        for (d, keep) in directions.iter().zip(kept) {
            if keep {
                let arm = cross(point.lever, *d);
                rows[count] = [d[0], d[1], d[2], arm[0], arm[1], arm[2]];
                count += 1;
            }
        }
    }
    if count == 0 {
        return pack(&free);
    }
    let l = whitening(&free);
    let mut basis = [[0.0f32; 6]; 6];
    let mut rank = 0;
    for row in &rows[..count] {
        let c: [f32; 6] = std::array::from_fn(|a| (0..6).map(|b| l[b][a] * row[b]).sum());
        let size = norm(&c);
        if size <= 0.0 || rank == 6 {
            continue;
        }
        let mut v = c;
        for _ in 0..2 {
            for q in &basis[..rank] {
                let along = (0..6).map(|k| q[k] * v[k]).sum::<f32>();
                for k in 0..6 {
                    v[k] -= along * q[k];
                }
            }
        }
        let rest = norm(&v);
        if rest > RANK_TOLERANCE * size {
            basis[rank] = v.map(|x| x / rest);
            rank += 1;
        }
    }
    let projector: [[f32; 6]; 6] = std::array::from_fn(|r| {
        std::array::from_fn(|c| f32::from(u8::from(r == c)) - basis[..rank].iter().map(|q| q[r] * q[c]).sum::<f32>())
    });
    let a: [[f32; 6]; 6] = std::array::from_fn(|r| std::array::from_fn(|c| (0..6).map(|k| l[r][k] * projector[k][c]).sum()));
    let mut out = [0.0; 21];
    for r in 0..6 {
        for c in r..6 {
            out[mobility_index(r, c)] = (0..6).map(|k| a[r][k] * a[c][k]).sum();
        }
    }
    out
}

/// The known increment (dv, dw) for a body moving at (v0, w0) with no motion
/// into any support: Box3D's contact rule (Catto, sequential impulses)
/// without restitution, softness or position correction. A normal multiplier
/// is one-sided; friction is bounded by μ·λn, a pyramid over two tangents.
/// Returns the supports the body stays on.
fn project_off(
    start: &CoupledStart,
    supports: &[SupportPoint],
    v0: [f32; 3],
    w0: [f32; 3],
    dv: &mut [f32; 3],
    dw: &mut [f32; 3],
) -> Held {
    let points = &supports[..supports.len().min(MAX_SUPPORT_POINTS)];
    let (m, i) = (start.inverse_mass, &start.inverse_inertia);
    let mut lambda = [[0.0f32; 3]; MAX_SUPPORT_POINTS];
    let speed = |point: &SupportPoint, d: [f32; 3], arm: [f32; 3], dv: &[f32; 3], dw: &[f32; 3]| {
        let v: [f32; 3] = std::array::from_fn(|r| v0[r] + dv[r]);
        let w: [f32; 3] = std::array::from_fn(|r| w0[r] + dw[r]);
        dot(d, v) + dot(arm, w) - dot(d, point.support_velocity)
    };
    for _ in 0..PROJECTION_SWEEPS {
        for (p, point) in points.iter().enumerate() {
            for (k, d) in directions(point.normal).into_iter().enumerate() {
                let arm = cross(point.lever, d);
                let turn: [f32; 3] = std::array::from_fn(|r| dot(i[r], arm));
                let mass = m + dot(arm, turn);
                if mass <= 0.0 {
                    continue;
                }
                let trial = lambda[p][k] - speed(point, d, arm, dv, dw) / mass;
                let bound = (point.friction * lambda[p][0]).max(0.0);
                let next = if k == 0 { trial.max(0.0) } else { trial.clamp(-bound, bound) };
                let change = next - lambda[p][k];
                lambda[p][k] = next;
                for r in 0..3 {
                    dv[r] += change * m * d[r];
                    dw[r] += change * turn[r];
                }
            }
        }
    }
    let mut held = Held::default();
    for (p, point) in points.iter().enumerate() {
        let n = point.normal;
        let arm = cross(point.lever, n);
        let mass = m + (0..3).map(|r| arm[r] * dot(i[r], arm)).sum::<f32>();
        if mass <= 0.0 || speed(point, n, arm, dv, dw) > HELD_SPEED {
            continue;
        }
        held.closed |= 1 << p;
        let cone = STICK_MARGIN * point.friction * lambda[p][0];
        if cone > 0.0 && lambda[p][1].abs() < cone && lambda[p][2].abs() < cone {
            held.stuck |= 1 << p;
        }
    }
    held
}

/// n and two unit tangents completing it to a right-handed basis (Duff et
/// al. 2017, "Building an Orthonormal Basis, Revisited").
fn directions(n: [f32; 3]) -> [[f32; 3]; 3] {
    let sign = if n[2] >= 0.0 { 1.0 } else { -1.0 };
    let a = -1.0 / (sign + n[2]);
    let b = n[0] * n[1] * a;
    [n, [1.0 + sign * n[0] * n[0] * a, sign * b, -sign * n[0]], [b, sign + n[1] * n[1] * a, -n[1]]]
}

/// L with L·Lᵀ = `free`, block diagonal: √m⁻¹ on the linear block, the
/// Cholesky factor of I⁻¹ on the angular. A pivot under 1e-7 of the trace
/// (a locked axis) leaves its column zero: no motion that way.
fn whitening(free: &[[f32; 6]; 6]) -> [[f32; 6]; 6] {
    let mut l = [[0.0f32; 6]; 6];
    let root = free[0][0].max(0.0).sqrt();
    for (r, row) in l.iter_mut().enumerate().take(3) {
        row[r] = root;
    }
    let scale = free[3][3].abs() + free[4][4].abs() + free[5][5].abs();
    for j in 3..6 {
        let pivot = free[j][j] - (3..j).map(|k| l[j][k] * l[j][k]).sum::<f32>();
        if pivot <= 1e-7 * scale {
            continue;
        }
        let root = pivot.sqrt();
        l[j][j] = root;
        for r in j + 1..6 {
            l[r][j] = (free[r][j] - (3..j).map(|k| l[r][k] * l[j][k]).sum::<f32>()) / root;
        }
    }
    l
}

fn pack(m: &[[f32; 6]; 6]) -> Mobility {
    let mut out = [0.0; 21];
    for r in 0..6 {
        for c in r..6 {
            out[mobility_index(r, c)] = m[r][c];
        }
    }
    out
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn norm(v: &[f32; 6]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// `q` turned in the world frame by the rotation vector `turn`.
fn turned(q: [f32; 4], turn: [f32; 3]) -> [f32; 4] {
    let angle = (turn[0] * turn[0] + turn[1] * turn[1] + turn[2] * turn[2]).sqrt();
    if angle <= 0.0 {
        return q;
    }
    let s = (0.5 * angle).sin() / angle;
    let d = [turn[0] * s, turn[1] * s, turn[2] * s, (0.5 * angle).cos()];
    [
        d[3] * q[0] + d[0] * q[3] + d[1] * q[2] - d[2] * q[1],
        d[3] * q[1] - d[0] * q[2] + d[1] * q[3] + d[2] * q[0],
        d[3] * q[2] + d[0] * q[1] - d[1] * q[0] + d[2] * q[3],
        d[3] * q[3] - d[0] * q[0] - d[1] * q[1] - d[2] * q[2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BodyConfig, BodyHandle, BodyImpulse, BodyKind, PhysicsWorld};

    const G: [f32; 3] = [0.0, -9.81, 0.0];

    fn cuboid(half: [f32; 3]) -> Vec<[f32; 3]> {
        (0..8).map(|k| std::array::from_fn(|a| if k >> a & 1 == 1 { half[a] } else { -half[a] })).collect()
    }

    fn start_of(world: &PhysicsWorld, body: BodyHandle) -> CoupledStart {
        CoupledStart::new(&world.dynamics(body).unwrap(), world.pose(body).unwrap().rotation)
    }

    fn supports_of(world: &PhysicsWorld, body: BodyHandle) -> Vec<SupportPoint> {
        let mut out = [SupportPoint::default(); MAX_SUPPORT_POINTS];
        let count = world.support_points(body, &mut out).unwrap();
        assert_eq!(count.kept, count.found, "every touching point fits");
        out[..count.kept].to_vec()
    }

    fn settle(world: &mut PhysicsWorld, dt: Seconds, ticks: usize) {
        for _ in 0..ticks {
            world.step(dt, box3d_substep_count(dt).value).unwrap();
        }
    }

    /// The angle between two unit quaternions, radians.
    fn between(a: [f32; 4], b: [f32; 4]) -> f32 {
        // conj(a) ⊗ b
        let r = [
            a[3] * b[0] - a[0] * b[3] - a[1] * b[2] + a[2] * b[1],
            a[3] * b[1] + a[0] * b[2] - a[1] * b[3] - a[2] * b[0],
            a[3] * b[2] - a[0] * b[1] + a[1] * b[0] - a[2] * b[3],
            a[3] * b[3] + a[0] * b[0] + a[1] * b[1] + a[2] * b[2],
        ];
        2.0 * (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt().atan2(r[3].abs())
    }

    fn gap(a: [f32; 3], b: [f32; 3]) -> f32 {
        (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f32>().sqrt()
    }

    fn length(a: [f32; 3]) -> f32 {
        gap(a, [0.0; 3])
    }

    /// How fast the predicted body closes on `point`'s support, m/s: never
    /// above zero once the law has projected.
    fn closing(state: &CoupledState, point: &SupportPoint) -> f32 {
        let spin = cross(state.angular_velocity, point.lever);
        let v: [f32; 3] = std::array::from_fn(|r| state.linear_velocity[r] + spin[r] - point.support_velocity[r]);
        -dot(point.normal, v)
    }

    /// I17: a free body given the liquid's reaction over the step ends the
    /// tick where the law puts it, at 60, 30 and 15 Hz. Linear is exact to f32
    /// rounding. Angular carries Box3D's gyroscopic step and its inertia
    /// turning with the body: within 2.5% of the spin gained and inside the
    /// handover check's 0.1° (D18).
    /// The impulse-before-the-step handover it replaces misses by
    /// g·dt·(N − 1)/(2N) in velocity, centimetres a second at these rates.
    #[test]
    fn coupled_motion_matches_box3d() {
        for hz in [60.0, 30.0, 15.0] {
            let dt = Seconds(1.0 / hz);
            let mut world = PhysicsWorld::new(G).unwrap();
            let body = world
                .add_hull(
                    &cuboid([0.3, 0.2, 0.5]),
                    BodyConfig { position: [0.0, 5.0, 0.0], rotation: [0.1, 0.2, 0.05, 0.97], mass: 6.0, ..BodyConfig::default() },
                )
                .unwrap();
            world.set_velocity(body, [0.4, 1.0, -0.3], [0.3, -0.2, 0.5]).unwrap();
            let start = start_of(&world, body);
            let (push, turn) = ([3.0, 40.0, -2.0], [0.4, -0.3, 0.2]);
            world.queue_reaction_over_step(&[BodyImpulse { body, linear: push, angular: turn }], dt).unwrap();
            world.step(dt, box3d_substep_count(dt).value).unwrap();
            let law = coupled_state_at(&start, &[], push, turn, dt.0 as f32, coupled_substep(dt));
            let end = world.dynamics(body).unwrap();
            let rotation = world.pose(body).unwrap().rotation;
            let position = gap(law.position, end.center_of_mass);
            let velocity = gap(law.linear_velocity, end.linear_velocity);
            let spin = gap(law.angular_velocity, end.angular_velocity);
            let spun = gap(start.angular_velocity, end.angular_velocity);
            let turned = between(law.rotation, rotation);
            eprintln!(
                "{hz} Hz: position {position:.2e} m, velocity {velocity:.2e} m/s, spin {spin:.2e} of {spun:.2e} rad/s, rotation {turned:.2e} rad"
            );
            assert!(position < 2e-6, "{hz} Hz: the law ends {position} m from Box3D");
            assert!(velocity < 2e-5, "{hz} Hz: the law ends {velocity} m/s from Box3D");
            assert!(spin < 0.025 * spun, "{hz} Hz: the law's spin is {spin} rad/s from Box3D's, of {spun} rad/s gained");
            assert!(turned < 0.1f32.to_radians(), "{hz} Hz: the law's rotation is {turned} rad from Box3D's");
        }
    }

    /// A body the liquid holds up drifts slower than Box3D's sleep speed for
    /// two seconds; every tick still ends where the law puts it.
    #[test]
    fn coupled_motion_matches_box3d_while_slow() {
        let dt = Seconds(1.0 / 30.0);
        let mut world = PhysicsWorld::new(G).unwrap();
        let body = world.add_hull(&cuboid([0.2; 3]), BodyConfig { position: [0.0, 5.0, 0.0], mass: 8.0, ..BodyConfig::default() }).unwrap();
        world.set_velocity(body, [0.01, 0.0, 0.0], [0.0; 3]).unwrap();
        let push = [0.0, -8.0 * G[1] * dt.0 as f32, 0.0];
        for tick in 0..60 {
            let start = start_of(&world, body);
            world.queue_reaction_over_step(&[BodyImpulse { body, linear: push, angular: [0.0; 3] }], dt).unwrap();
            world.step(dt, box3d_substep_count(dt).value).unwrap();
            let law = coupled_state_at(&start, &[], push, [0.0; 3], dt.0 as f32, coupled_substep(dt));
            let end = world.dynamics(body).unwrap();
            let velocity = gap(law.linear_velocity, end.linear_velocity);
            assert!(velocity < 2e-5, "tick {tick}: the law ends {velocity} m/s from Box3D, awake {}", end.awake);
        }
        // Handed nothing more, it sleeps again.
        let mut drifting = PhysicsWorld::new([0.0; 3]).unwrap();
        let body = drifting.add_hull(&cuboid([0.2; 3]), BodyConfig { mass: 8.0, ..BodyConfig::default() }).unwrap();
        drifting.queue_reaction_over_step(&[BodyImpulse { body, linear: [0.08, 0.0, 0.0], angular: [0.0; 3] }], dt).unwrap();
        settle(&mut drifting, dt, 30);
        assert!(!drifting.dynamics(body).unwrap().awake, "a body no longer handed a reaction falls asleep");
    }

    /// D16: a box resting flat on a fixed floor reads its bottom face's four
    /// corners, normals up. The law holds it still against gravity, a
    /// downward push and a tipping turn, and keeps all four closed; a push up
    /// past its weight still lifts it and frees them.
    #[test]
    fn coupled_motion_holds_a_resting_box_on_its_support() {
        let mut world = PhysicsWorld::new(G).unwrap();
        world
            .add_hull(&cuboid([2.0, 0.5, 2.0]), BodyConfig { kind: BodyKind::Fixed, position: [0.0, -0.5, 0.0], ..BodyConfig::default() })
            .unwrap();
        let body = world.add_hull(&cuboid([0.2; 3]), BodyConfig { position: [0.0, 0.2, 0.0], mass: 8.0, ..BodyConfig::default() }).unwrap();
        let dt = Seconds(1.0 / 30.0);
        settle(&mut world, dt, 30);
        let supports = supports_of(&world, body);
        assert_eq!(supports.len(), 4, "the bottom face's corners: {supports:?}");
        for point in &supports {
            assert!(gap(point.normal, [0.0, 1.0, 0.0]) < 1e-4, "the floor's normal points up into the box: {point:?}");
        }
        let start = start_of(&world, body);
        let (t, h) = (dt.0 as f32, coupled_substep(dt));
        let held = coupled_state_at(&start, &supports, [0.0, -20.0, 0.0], [0.3, 0.0, 0.2], t, h);
        assert!(length(held.linear_velocity) < 1e-3, "pressed into the floor, the box is predicted still: {held:?}");
        assert!(length(held.angular_velocity) < 1e-3, "turned against the floor, the box does not tip: {held:?}");
        assert_eq!(held.held.closed, 0b1111, "all four corners stay closed: {held:?}");
        for point in &supports {
            assert!(closing(&held, point) < 1e-4, "no corner moves into the floor: {held:?}");
        }
        let lifted = coupled_state_at(&start, &supports, [0.0, 8.0 * 9.81 * t * 1.5, 0.0], [0.0; 3], t, h);
        let rise = 0.5 * 9.81 * t;
        assert!((lifted.linear_velocity[1] - rise).abs() < 1e-3, "pushed up by half again its weight, it rises at {rise}: {lifted:?}");
        assert_eq!(lifted.held.closed, 0, "rising, it leaves the floor: {lifted:?}");
    }

    /// A plank leaning on a wall from the floor stands because friction holds
    /// its foot: the law predicts it still with every support closed and
    /// stuck. With the friction taken away it slides down the wall, as a
    /// frictionless ladder does.
    #[test]
    fn coupled_motion_holds_a_leaning_plank_by_friction() {
        let mut world = PhysicsWorld::new(G).unwrap();
        let fixed = |position| BodyConfig { kind: BodyKind::Fixed, position, ..BodyConfig::default() };
        world.add_hull(&cuboid([2.0, 0.5, 2.0]), fixed([0.0, -0.5, 0.0])).unwrap();
        world.add_hull(&cuboid([0.5, 2.0, 2.0]), fixed([-0.5, 2.0, 0.0])).unwrap();
        let lean = 0.25f32;
        let rotation = [0.0, 0.0, (0.5 * lean).sin(), (0.5 * lean).cos()];
        let position = [0.1226 + 2e-3, 0.303 + 2e-3, 0.0];
        let plank = world
            .add_hull(&cuboid([0.05, 0.3, 0.2]), BodyConfig { position, rotation, mass: 4.0, ..BodyConfig::default() })
            .unwrap();
        let dt = Seconds(1.0 / 30.0);
        settle(&mut world, dt, 60);
        let resting = world.dynamics(plank).unwrap();
        assert!(length(resting.linear_velocity) < 1e-3, "Box3D holds the plank up: {resting:?}");
        let supports = supports_of(&world, plank);
        let on = |n: [f32; 3]| supports.iter().filter(|p| gap(p.normal, n) < 1e-3).count();
        assert!(on([0.0, 1.0, 0.0]) > 0 && on([1.0, 0.0, 0.0]) > 0, "the plank stands on the floor and the wall: {supports:?}");
        let start = start_of(&world, plank);
        let (t, h) = (dt.0 as f32, coupled_substep(dt));
        let held = coupled_state_at(&start, &supports, [0.0; 3], [0.0; 3], t, h);
        assert!(length(held.linear_velocity) < 1e-3 && length(held.angular_velocity) < 1e-2, "friction holds it: {held:?}");
        assert_eq!(held.held.closed.count_ones() as usize, supports.len(), "every support stays closed: {held:?}");
        let fast = unpack(&constrained_mobility(&start, &supports, held.held));
        let free = largest(&free_of(&start));
        assert!(largest(&fast) < 1e-4 * free, "the solve holds it fast: {:?}, held {:?}", fast, held.held);
        let slick: Vec<SupportPoint> = supports.iter().map(|p| SupportPoint { friction: 0.0, ..*p }).collect();
        let slides = coupled_state_at(&start, &slick, [0.0; 3], [0.0; 3], t, h);
        assert!(length(slides.linear_velocity) > 0.02, "without friction the plank slides: {slides:?}");
        assert_eq!(slides.held.stuck, 0, "{slides:?}");
        for point in &slick {
            assert!(closing(&slides, point) < 1e-4, "sliding, it still never moves into a support: {slides:?}");
        }
    }

    /// A box touching the floor while moving down at 1 m/s is stopped at the
    /// tick's start, as Box3D's first substep stops it, and is placed no
    /// deeper.
    #[test]
    fn coupled_motion_stops_a_closing_body_at_the_start() {
        let mut world = PhysicsWorld::new(G).unwrap();
        world
            .add_hull(&cuboid([2.0, 0.5, 2.0]), BodyConfig { kind: BodyKind::Fixed, position: [0.0, -0.5, 0.0], ..BodyConfig::default() })
            .unwrap();
        let body = world.add_hull(&cuboid([0.2; 3]), BodyConfig { position: [0.0, 0.2, 0.0], mass: 8.0, ..BodyConfig::default() }).unwrap();
        let dt = Seconds(1.0 / 30.0);
        settle(&mut world, dt, 30);
        world.set_velocity(body, [0.0, -1.0, 0.0], [0.0; 3]).unwrap();
        let supports = supports_of(&world, body);
        let start = start_of(&world, body);
        let (t, h) = (dt.0 as f32, coupled_substep(dt));
        let state = coupled_state_at(&start, &supports, [0.0; 3], [0.0; 3], t, h);
        assert!(state.linear_velocity[1] > -1e-3, "stopped: {state:?}");
        assert!(state.position[1] > start.position[1] - 1e-4, "placed no deeper: {state:?} from {start:?}");
    }

    /// A box riding a kinematic platform that slides sideways moves with it.
    #[test]
    fn coupled_motion_rides_a_moving_support() {
        let mut world = PhysicsWorld::new(G).unwrap();
        let platform = world
            .add_hull(&cuboid([2.0, 0.5, 2.0]), BodyConfig { kind: BodyKind::Animated, position: [0.0, -0.5, 0.0], ..BodyConfig::default() })
            .unwrap();
        world.set_velocity(platform, [0.5, 0.0, 0.0], [0.0; 3]).unwrap();
        let body = world.add_hull(&cuboid([0.2; 3]), BodyConfig { position: [0.0, 0.2, 0.0], mass: 8.0, ..BodyConfig::default() }).unwrap();
        let dt = Seconds(1.0 / 30.0);
        settle(&mut world, dt, 30);
        let supports = supports_of(&world, body);
        assert!(!supports.is_empty());
        for point in &supports {
            assert!(gap(point.support_velocity, [0.5, 0.0, 0.0]) < 1e-4, "{point:?}");
        }
        let start = start_of(&world, body);
        let state = coupled_state_at(&start, &supports, [0.0; 3], [0.0; 3], dt.0 as f32, coupled_substep(dt));
        assert!(gap(state.linear_velocity, [0.5, 0.0, 0.0]) < 1e-2, "{state:?}");
    }

    /// A box resting on a dynamic box reads no support: only static and
    /// kinematic supports shape the prediction.
    #[test]
    fn coupled_motion_ignores_dynamic_supports() {
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        world.add_hull(&cuboid([0.2; 3]), BodyConfig { position: [0.0, 0.0, 0.0], ..BodyConfig::default() }).unwrap();
        let top = world.add_hull(&cuboid([0.2; 3]), BodyConfig { position: [0.0, 0.399, 0.0], ..BodyConfig::default() }).unwrap();
        world.step(Seconds(1.0 / 60.0), 4).unwrap();
        assert!(supports_of(&world, top).is_empty());
    }

    /// Supports at right angles project jointly: a body in a floor-wall corner
    /// pushed down and into the wall keeps only its motion along the wall.
    #[test]
    fn coupled_motion_projects_off_a_corner() {
        let start = CoupledStart { inverse_inertia: [[0.0; 3]; 3], ..floor_box().0 };
        let point = |lever, normal| SupportPoint { lever, normal, ..SupportPoint::default() };
        let corner = [point([0.0, -0.2, 0.0], [0.0, 1.0, 0.0]), point([-0.2, 0.0, 0.0], [1.0, 0.0, 0.0])];
        let state = coupled_state_at(&start, &corner, [-16.0, -24.0, 12.0], [0.0; 3], 0.1, 0.025);
        assert!(gap(state.linear_velocity, [0.0, 0.0, 1.5]) < 1e-5, "{state:?}");
    }

    /// A 0.4 m, 8 kg cube resting flat on a floor, its bottom corners touching.
    fn floor_box() -> (CoupledStart, Vec<SupportPoint>) {
        let inertia = 8.0 * 0.16 / 6.0;
        let start = CoupledStart {
            position: [0.0, 0.2, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            linear_velocity: [0.0; 3],
            angular_velocity: [0.0; 3],
            inverse_mass: 1.0 / 8.0,
            inverse_inertia: [[1.0 / inertia, 0.0, 0.0], [0.0, 1.0 / inertia, 0.0], [0.0, 0.0, 1.0 / inertia]],
            linear_acceleration: G,
            angular_acceleration: [0.0; 3],
        };
        let corners = [[0.2, -0.2, 0.2], [-0.2, -0.2, 0.2], [-0.2, -0.2, -0.2], [0.2, -0.2, -0.2]];
        let supports =
            corners.iter().map(|&lever| SupportPoint { lever, normal: [0.0, 1.0, 0.0], friction: 0.5, ..SupportPoint::default() }).collect();
        (start, supports)
    }

    fn unpack(m: &Mobility) -> [[f32; 6]; 6] {
        std::array::from_fn(|r| std::array::from_fn(|c| m[mobility_index(r, c)]))
    }

    fn free_of(start: &CoupledStart) -> [[f32; 6]; 6] {
        std::array::from_fn(|r| {
            std::array::from_fn(|c| match (r < 3, c < 3) {
                (true, true) => f32::from(u8::from(r == c)) * start.inverse_mass,
                (false, false) => start.inverse_inertia[r - 3][c - 3],
                _ => 0.0,
            })
        })
    }

    fn largest(m: &[[f32; 6]; 6]) -> f32 {
        m.iter().flatten().fold(0.0f32, |a, b| a.max(b.abs()))
    }

    /// Every row `held` keeps, as the solve holds it.
    fn held_rows(supports: &[SupportPoint], held: Held) -> Vec<[f32; 6]> {
        let mut rows = Vec::new();
        for (p, point) in supports.iter().enumerate() {
            let [n, t1, t2] = directions(point.normal);
            let kept = [(n, held.closed), (t1, held.stuck), (t2, held.stuck)];
            for (d, bits) in kept {
                if bits >> p & 1 == 1 {
                    let arm = cross(point.lever, d);
                    rows.push([d[0], d[1], d[2], arm[0], arm[1], arm[2]]);
                }
            }
        }
        rows
    }

    /// The mobility's guarantees: no held row moves under any impulse, and
    /// it never gives an impulse negative work, both to f32 rounding of the
    /// free mobility's size.
    fn assert_sound(start: &CoupledStart, supports: &[SupportPoint], held: Held) -> [[f32; 6]; 6] {
        let m = unpack(&constrained_mobility(start, supports, held));
        let scale = largest(&free_of(start));
        assert!(m.iter().flatten().all(|x| x.is_finite()), "{m:?}");
        for row in held_rows(supports, held) {
            let moved: [f32; 6] = std::array::from_fn(|r| (0..6).map(|c| m[r][c] * row[c]).sum());
            let size = row.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!(moved.iter().all(|x| x.abs() < 1e-5 * scale * size), "a held row moves: {moved:?} for {row:?}");
        }
        let mut seed = 0x9e37_79b9u32;
        for _ in 0..64 {
            let x: [f32; 6] = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            });
            let work: f32 = (0..6).map(|r| x[r] * (0..6).map(|c| m[r][c] * x[c]).sum::<f32>()).sum();
            assert!(work > -1e-6 * scale, "negative work {work} for {x:?}");
        }
        m
    }

    /// Nothing held: the free mobility exactly.
    #[test]
    fn constrained_mobility_without_supports_is_the_free_mobility() {
        let (start, supports) = floor_box();
        assert_eq!(unpack(&constrained_mobility(&start, &supports, Held::default())), free_of(&start));
    }

    /// A box closed on a floor still slides and spins freely about the
    /// vertical but neither sinks nor tips; stuck too, it is held fast.
    #[test]
    fn constrained_mobility_holds_a_box_on_its_floor() {
        let (start, supports) = floor_box();
        let free = free_of(&start);
        let closed = assert_sound(&start, &supports, Held { closed: 0b1111, stuck: 0 });
        for (r, c) in [(0, 0), (2, 2), (4, 4)] {
            assert!((closed[r][c] - free[r][c]).abs() < 1e-5 * free[r][c], "({r}, {c}) stays free: {closed:?}");
        }
        for k in [1, 3, 5] {
            assert!(closed[k][k].abs() < 1e-5 * largest(&free), "{k} is held: {closed:?}");
        }
        let stuck = assert_sound(&start, &supports, Held { closed: 0b1111, stuck: 0b1111 });
        assert!(largest(&stuck) < 1e-5 * largest(&free), "held fast: {stuck:?}");
    }

    /// The order the points come in and a nearly repeated point change the
    /// mobility by rounding only.
    #[test]
    fn constrained_mobility_ignores_order_and_near_repeats() {
        let (start, supports) = floor_box();
        let held = Held { closed: 0b1111, stuck: 0 };
        let base = assert_sound(&start, &supports, held);
        let reversed: Vec<SupportPoint> = supports.iter().rev().copied().collect();
        let mut nudged = supports.clone();
        nudged[3].lever[0] += 1e-5;
        nudged.push(SupportPoint { lever: [0.2, -0.2, -0.19999], ..supports[3] });
        let scale = largest(&free_of(&start));
        for (what, other, bits) in [("reversed", &reversed, held), ("nudged", &nudged, Held { closed: 0b11111, stuck: 0 })] {
            let m = assert_sound(&start, other, bits);
            let worst = (0..6).flat_map(|r| (0..6).map(move |c| (r, c))).map(|(r, c)| (m[r][c] - base[r][c]).abs()).fold(0.0, f32::max);
            assert!(worst < 1e-4 * scale, "{what}: off by {worst}");
        }
    }

    /// Very light and very heavy bodies, and a body locked against turning
    /// about x, keep the guarantees.
    #[test]
    fn constrained_mobility_survives_scale_and_locked_axes() {
        let (start, supports) = floor_box();
        let held = Held { closed: 0b1111, stuck: 0b0101 };
        for (inverse_mass, turn) in [(1e-4, 1e3), (1e3, 1e-4), (1.0 / 8.0, 0.0)] {
            let mut inverse_inertia = start.inverse_inertia;
            if turn > 0.0 {
                inverse_inertia = [[turn, 0.1 * turn, 0.0], [0.1 * turn, turn, 0.0], [0.0, 0.0, turn]];
            } else {
                inverse_inertia[0] = [0.0; 3];
                inverse_inertia[1][0] = 0.0;
                inverse_inertia[2][0] = 0.0;
            }
            assert_sound(&CoupledStart { inverse_mass, inverse_inertia, ..start }, &supports, held);
        }
    }
}

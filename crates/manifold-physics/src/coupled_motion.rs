//! The one motion law a body coupled to a liquid follows inside a tick
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` D15, D16). The liquid places the body
//! by it while it solves; Box3D, handed the liquid's reaction as a steady
//! force over the step (D17), ends the tick on it exactly when no contact,
//! damping, motion lock or speed cap acts. `liquid_pose.wgsl` is the GPU twin.

use crate::stepping::box3d_substep_count;
use crate::{BodyDynamics, Seconds};

/// Touching static contacts the prediction respects (D16).
pub const MAX_CONTACT_NORMALS: usize = 3;

/// Projected Gauss–Seidel sweeps over the contact normals. Normals at right
/// angles (floor and walls) settle in one; the rest only shape a prediction
/// the handover check measures.
const PROJECTION_SWEEPS: usize = 4;

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
}

/// Box3D's substep for a tick of `dt`: the h in the law.
pub fn coupled_substep(dt: Seconds) -> f32 {
    (dt.0 / f64::from(box3d_substep_count(dt).value)) as f32
}

/// The body `t` seconds into the tick, with `linear_push` and `angular_push`
/// the reaction impulse the liquid has put on it so far. The known increment
/// Δv = a·t + P/m loses its part into each of `normals` (unit, pointing out of
/// the support); then v = v0 + Δv and x = x0 + v0·t + ½·Δv·(t + h), the
/// symplectic Euler sum over substeps of h. Angular follows the same form as
/// a world-frame rotation vector.
pub fn coupled_state_at(
    start: &CoupledStart,
    normals: &[[f32; 3]],
    linear_push: [f32; 3],
    angular_push: [f32; 3],
    t: f32,
    h: f32,
) -> CoupledState {
    let lead = 0.5 * (t + h);
    let mut dv: [f32; 3] =
        std::array::from_fn(|i| start.linear_acceleration[i] * t + start.inverse_mass * linear_push[i]);
    project_off(&mut dv, normals);
    let i = &start.inverse_inertia;
    let dw: [f32; 3] = std::array::from_fn(|r| {
        start.angular_acceleration[r] * t + i[r][0] * angular_push[0] + i[r][1] * angular_push[1] + i[r][2] * angular_push[2]
    });
    let (x0, v0, w0) = (start.position, start.linear_velocity, start.angular_velocity);
    let turn: [f32; 3] = std::array::from_fn(|r| w0[r] * t + dw[r] * lead);
    CoupledState {
        position: std::array::from_fn(|r| x0[r] + v0[r] * t + dv[r] * lead),
        rotation: turned(start.rotation, turn),
        linear_velocity: std::array::from_fn(|r| v0[r] + dv[r]),
        angular_velocity: std::array::from_fn(|r| w0[r] + dw[r]),
    }
}

/// `v` with no part into any of `normals`, removed jointly by projected
/// Gauss–Seidel on the one-sided multipliers.
fn project_off(v: &mut [f32; 3], normals: &[[f32; 3]]) {
    let normals = &normals[..normals.len().min(MAX_CONTACT_NORMALS)];
    let mut lambda = [0.0f32; MAX_CONTACT_NORMALS];
    for _ in 0..PROJECTION_SWEEPS {
        for (k, n) in normals.iter().enumerate() {
            let into = v[0] * n[0] + v[1] * n[1] + v[2] * n[2];
            let next = (lambda[k] - into).max(0.0);
            let change = next - lambda[k];
            lambda[k] = next;
            for r in 0..3 {
                v[r] += change * n[r];
            }
        }
    }
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

    /// D16: a box resting on a fixed floor reads the floor's normal, up, and
    /// the law then holds it still against gravity and a downward push while
    /// a push up past its weight still lifts it.
    #[test]
    fn coupled_motion_holds_a_resting_box_on_its_support() {
        let mut world = PhysicsWorld::new(G).unwrap();
        world
            .add_hull(&cuboid([2.0, 0.5, 2.0]), BodyConfig { kind: BodyKind::Fixed, position: [0.0, -0.5, 0.0], ..BodyConfig::default() })
            .unwrap();
        let body = world.add_hull(&cuboid([0.2; 3]), BodyConfig { position: [0.0, 0.2, 0.0], mass: 8.0, ..BodyConfig::default() }).unwrap();
        let dt = Seconds(1.0 / 30.0);
        for _ in 0..30 {
            world.step(dt, box3d_substep_count(dt).value).unwrap();
        }
        let mut normals = [[0.0; 3]; 3];
        let count = world.static_contact_normals(body, &mut normals).unwrap();
        assert_eq!(count, 1, "one support: {normals:?}");
        assert!(gap(normals[0], [0.0, 1.0, 0.0]) < 1e-4, "the floor's normal points up into the box: {:?}", normals[0]);
        let start = start_of(&world, body);
        let h = coupled_substep(dt);
        let t = dt.0 as f32;
        let held = coupled_state_at(&start, &normals[..count], [0.0, -20.0, 0.0], [0.0; 3], t, h);
        assert!(held.linear_velocity[1].abs() < 1e-3, "pressed into the floor, the box is predicted still: {held:?}");
        let lifted = coupled_state_at(&start, &normals[..count], [0.0, 8.0 * 9.81 * t * 1.5, 0.0], [0.0; 3], t, h);
        let rise = 0.5 * 9.81 * t;
        assert!((lifted.linear_velocity[1] - rise).abs() < 1e-4, "pushed up by half again its weight, it rises at {rise}: {lifted:?}");
    }

    /// A box resting on a dynamic box reads no normal: only static and
    /// kinematic supports shape the prediction.
    #[test]
    fn coupled_motion_ignores_dynamic_supports() {
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        world.add_hull(&cuboid([0.2; 3]), BodyConfig { position: [0.0, 0.0, 0.0], ..BodyConfig::default() }).unwrap();
        let top = world.add_hull(&cuboid([0.2; 3]), BodyConfig { position: [0.0, 0.399, 0.0], ..BodyConfig::default() }).unwrap();
        world.step(Seconds(1.0 / 60.0), 4).unwrap();
        let mut normals = [[0.0; 3]; 3];
        assert_eq!(world.static_contact_normals(top, &mut normals).unwrap(), 0);
    }

    /// Normals at right angles project jointly: a box in a floor-wall corner
    /// pushed down and into the wall keeps only its motion along the wall.
    #[test]
    fn coupled_motion_projects_off_a_corner() {
        let start = CoupledStart {
            position: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            linear_velocity: [0.0; 3],
            angular_velocity: [0.0; 3],
            inverse_mass: 1.0,
            inverse_inertia: [[0.0; 3]; 3],
            linear_acceleration: [0.0; 3],
            angular_acceleration: [0.0; 3],
        };
        let corner = [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0]];
        let state = coupled_state_at(&start, &corner, [-2.0, -3.0, 1.5], [0.0; 3], 0.1, 0.025);
        assert!(gap(state.linear_velocity, [0.0, 0.0, 1.5]) < 1e-6, "{state:?}");
    }
}

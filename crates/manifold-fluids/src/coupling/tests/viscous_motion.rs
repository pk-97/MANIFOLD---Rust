use super::*;

struct MotionResult {
    final_pose: BodyPose,
    final_dynamics: RigidBodyState,
}

fn finite_pose(pose: BodyPose) -> bool {
    pose.position
        .iter()
        .chain(pose.rotation.iter())
        .all(|value| value.is_finite())
}

fn finite_dynamics(dynamics: &RigidBodyState) -> bool {
    dynamics
        .pose
        .position
        .iter()
        .chain(dynamics.pose.rotation.iter())
        .chain(dynamics.dynamics.center_of_mass.iter())
        .chain(dynamics.dynamics.linear_velocity.iter())
        .chain(dynamics.dynamics.angular_velocity.iter())
        .chain(std::iter::once(&dynamics.dynamics.inverse_mass))
        .chain(
            dynamics
                .dynamics
                .inverse_inertia
                .iter()
                .flat_map(|row| row.iter()),
        )
        .chain(dynamics.dynamics.external_linear_acceleration.iter())
        .chain(dynamics.dynamics.external_angular_acceleration.iter())
        .all(|value| value.is_finite())
}

fn finite_reaction(reaction: &RigidReaction) -> bool {
    reaction
        .linear
        .iter()
        .chain(reaction.angular.iter())
        .chain(reaction.delta_linear.iter())
        .chain(reaction.delta_angular.iter())
        .all(|value| value.is_finite())
}

fn body_energy(state: &RigidBodyState) -> f64 {
    let inverse_mass = f64::from(state.dynamics.inverse_mass);
    assert!(inverse_mass.is_finite() && inverse_mass > 0.0);
    let mass = 1.0 / inverse_mass;
    let linear = state.dynamics.linear_velocity.map(f64::from);
    let angular = state.dynamics.angular_velocity.map(f64::from);
    let inverse = state.dynamics.inverse_inertia.map(|row| row.map(f64::from));

    let determinant = inverse[0][0]
        * (inverse[1][1] * inverse[2][2] - inverse[1][2] * inverse[2][1])
        - inverse[0][1] * (inverse[1][0] * inverse[2][2] - inverse[1][2] * inverse[2][0])
        + inverse[0][2] * (inverse[1][0] * inverse[2][1] - inverse[1][1] * inverse[2][0]);
    assert!(determinant.is_finite() && determinant > 0.0);
    let inertia = [
        [
            (inverse[1][1] * inverse[2][2] - inverse[1][2] * inverse[2][1]) / determinant,
            (inverse[0][2] * inverse[2][1] - inverse[0][1] * inverse[2][2]) / determinant,
            (inverse[0][1] * inverse[1][2] - inverse[0][2] * inverse[1][1]) / determinant,
        ],
        [
            (inverse[1][2] * inverse[2][0] - inverse[1][0] * inverse[2][2]) / determinant,
            (inverse[0][0] * inverse[2][2] - inverse[0][2] * inverse[2][0]) / determinant,
            (inverse[0][2] * inverse[1][0] - inverse[0][0] * inverse[1][2]) / determinant,
        ],
        [
            (inverse[1][0] * inverse[2][1] - inverse[1][1] * inverse[2][0]) / determinant,
            (inverse[0][1] * inverse[2][0] - inverse[0][0] * inverse[2][1]) / determinant,
            (inverse[0][0] * inverse[1][1] - inverse[0][1] * inverse[1][0]) / determinant,
        ],
    ];
    let linear_energy = 0.5 * mass * linear.iter().map(|value| value * value).sum::<f64>();
    let rotational_energy = 0.5
        * angular
            .iter()
            .enumerate()
            .map(|(row, value)| {
                *value
                    * inertia[row]
                        .iter()
                        .zip(angular)
                        .map(|(coefficient, component)| coefficient * component)
                        .sum::<f64>()
            })
            .sum::<f64>();
    let total = linear_energy + rotational_energy;
    assert!(total.is_finite() && total >= 0.0);
    total
}

fn run_motion_case(ratio: f32, hz: f64, center_y: f32) -> MotionResult {
    let frame_dt = Seconds(1.0 / hz);
    let frame_count = (0.5 * hz).round() as usize;
    let mut pair = Pair::at_height(ratio, 1.0, BodyKind::Dynamic, 1000.0, true, center_y);
    pair.rigid
        .set_velocity(pair.body, [0.6, 0.0, 0.0], [0.0, 0.4, 0.0])
        .unwrap();

    let initial_state = pair.state();
    assert!(finite_dynamics(&initial_state));
    let initial_energy = body_energy(&initial_state);
    let mut maximum_energy = initial_energy;
    let mut maximum_response_error: f64 = 0.0;

    for frame_index in 0..frame_count {
        let mut frame = pair.fluid.begin_frame(frame_dt).unwrap();
        let mut elapsed = 0.0;
        while elapsed < frame_dt.0 - 1e-12 {
            let before = RigidBodyState {
                pose: pair.rigid.pose(pair.body).unwrap(),
                dynamics: pair.rigid.dynamics(pair.body).unwrap(),
            };
            assert!(finite_dynamics(&before));
            frame.set_rigid_bodies(&[before]).unwrap();
            let dt = frame.next_substep().unwrap().unwrap();
            frame.advance(dt).unwrap_or_else(|error| {
                panic!("viscous ratio={ratio} hz={hz} center_y={center_y} frame={frame_index} elapsed={elapsed} dt={dt:?} state={before:?}: {error}");
            });
            let reaction = frame.rigid_reactions().unwrap()[0];
            assert!(finite_reaction(&reaction));
            pair.rigid
                .apply_impulses(&[reaction.body_impulse(pair.body).unwrap()])
                .unwrap();
            let after = RigidBodyState {
                pose: pair.rigid.pose(pair.body).unwrap(),
                dynamics: pair.rigid.dynamics(pair.body).unwrap(),
            };
            assert!(finite_dynamics(&after));
            for axis in 0..3 {
                let linear_error = f64::from(
                    after.dynamics.linear_velocity[axis] - before.dynamics.linear_velocity[axis],
                ) - reaction.delta_linear[axis];
                let angular_error = f64::from(
                    after.dynamics.angular_velocity[axis] - before.dynamics.angular_velocity[axis],
                ) - reaction.delta_angular[axis];
                maximum_response_error = maximum_response_error
                    .max(linear_error.abs())
                    .max(angular_error.abs());
                assert!(linear_error.abs() < 5e-5);
                assert!(angular_error.abs() < 5e-5);
            }
            let energy = body_energy(&after);
            maximum_energy = maximum_energy.max(energy);
            assert!(energy <= initial_energy * 1.01);
            pair.rigid.step(dt, 1).unwrap();
            let advanced = RigidBodyState {
                pose: pair.rigid.pose(pair.body).unwrap(),
                dynamics: pair.rigid.dynamics(pair.body).unwrap(),
            };
            assert!(finite_dynamics(&advanced));
            let energy = body_energy(&advanced);
            maximum_energy = maximum_energy.max(energy);
            assert!(
                energy <= initial_energy * 1.01,
                "viscous ratio={ratio} hz={hz} frame={frame_index}: body energy {energy} exceeds initial {initial_energy}"
            );
            elapsed += dt.0;
        }
        assert_eq!(frame.next_substep().unwrap(), None);
        frame.finish().unwrap();
    }

    let final_pose = pair.rigid.pose(pair.body).unwrap();
    let final_dynamics = pair.state();
    assert!(finite_pose(final_pose));
    assert!(finite_dynamics(&final_dynamics));
    let final_energy = body_energy(&final_dynamics);
    assert!(final_energy < initial_energy * 0.95);
    println!(
        "ratio={ratio} hz={hz} center_y={center_y} initial_energy={initial_energy} final_energy={final_energy} maximum_energy={maximum_energy} maximum_response_error={maximum_response_error} final_position={:?} final_linear_velocity={:?}",
        final_pose.position, final_dynamics.dynamics.linear_velocity
    );
    MotionResult {
        final_pose,
        final_dynamics,
    }
}

#[test]
fn production_coupling_sustained_viscous_motion_is_bounded_and_rate_stable() {
    check_motion_at_height(1.05);
}

#[test]
fn production_coupling_sustained_surface_viscosity_is_bounded_and_rate_stable() {
    // The same predeclared limits, now with the body crossing the free
    // surface. No gravity: all initial kinetic energy belongs to the body.
    check_motion_at_height(1.65);
}

fn check_motion_at_height(center_y: f32) {
    for ratio in [0.1, 1.0] {
        let at_60 = run_motion_case(ratio, 60.0, center_y);
        let at_120 = run_motion_case(ratio, 120.0, center_y);
        let translation_difference = at_60
            .final_pose
            .position
            .into_iter()
            .zip(at_120.final_pose.position)
            .map(|(low, high)| f64::from(low - high).powi(2))
            .sum::<f64>()
            .sqrt();
        let speed_difference = at_60
            .final_dynamics
            .dynamics
            .linear_velocity
            .into_iter()
            .zip(at_120.final_dynamics.dynamics.linear_velocity)
            .map(|(low, high)| f64::from(low - high).powi(2))
            .sum::<f64>()
            .sqrt();
        println!(
            "ratio={ratio} center_y={center_y} translation_difference={translation_difference} speed_difference={speed_difference}"
        );
        assert!(translation_difference < 0.075);
        assert!(speed_difference < 0.05);
    }
}

use super::*;

fn box_points(half_extents: [f32; 3]) -> Vec<[f32; 3]> {
    let [x, y, z] = half_extents;
    vec![
        [-x, -y, -z],
        [x, -y, -z],
        [x, y, -z],
        [-x, y, -z],
        [-x, -y, z],
        [x, -y, z],
        [x, y, z],
        [-x, y, z],
    ]
}

fn z_rotation(radians: f32) -> [f32; 4] {
    [0.0, 0.0, (radians * 0.5).sin(), (radians * 0.5).cos()]
}

#[test]
fn coupling_body_linear_impulse_scales_by_mass() {
    let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
    let light = world
        .add_hull(
            &cube(0.5),
            BodyConfig {
                position: [-2.0, 0.0, 0.0],
                mass: 1.0,
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let heavy = world
        .add_hull(
            &cube(0.5),
            BodyConfig {
                position: [2.0, 0.0, 0.0],
                mass: 4.0,
                ..BodyConfig::default()
            },
        )
        .unwrap();
    world
        .apply_impulses(&[
            BodyImpulse {
                body: light,
                linear: [2.0, 0.0, 0.0],
                angular: [0.0; 3],
            },
            BodyImpulse {
                body: heavy,
                linear: [2.0, 0.0, 0.0],
                angular: [0.0; 3],
            },
        ])
        .unwrap();
    assert_eq!(world.linear_velocity(light).unwrap(), [2.0, 0.0, 0.0]);
    assert_eq!(world.linear_velocity(heavy).unwrap(), [0.5, 0.0, 0.0]);
}

#[test]
fn coupling_body_rotated_inertia_exposes_off_diagonal_response() {
    let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
    let body = world
        .add_hull(
            &box_points([0.5, 1.0, 1.5]),
            BodyConfig {
                mass: 2.0,
                rotation: z_rotation(std::f32::consts::FRAC_PI_4),
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let dynamics = world.dynamics(body).unwrap();
    let [hx, hy, hz] = [0.5, 1.0, 1.5];
    let mass = 2.0;
    let local = [
        1.0 / (mass * (hy * hy + hz * hz) / 3.0),
        1.0 / (mass * (hx * hx + hz * hz) / 3.0),
        1.0 / (mass * (hx * hx + hy * hy) / 3.0),
    ];
    let (sine, cosine) = (
        std::f32::consts::FRAC_1_SQRT_2,
        std::f32::consts::FRAC_1_SQRT_2,
    );
    let expected = [
        [
            cosine * cosine * local[0] + sine * sine * local[1],
            sine * cosine * (local[0] - local[1]),
            0.0,
        ],
        [
            sine * cosine * (local[0] - local[1]),
            sine * sine * local[0] + cosine * cosine * local[1],
            0.0,
        ],
        [0.0, 0.0, local[2]],
    ];
    for (row, expected_row) in expected.iter().enumerate() {
        for (column, expected_value) in expected_row.iter().enumerate() {
            assert!((dynamics.inverse_inertia[row][column] - expected_value).abs() < 1.0e-5);
        }
    }
    let angular_impulse = [0.7, -1.1, 0.4];
    world
        .apply_impulses(&[BodyImpulse {
            body,
            linear: [0.0; 3],
            angular: angular_impulse,
        }])
        .unwrap();
    let angular_velocity = world.angular_velocity(body).unwrap();
    for (row, expected_row) in expected.iter().enumerate() {
        let expected_response = expected_row
            .iter()
            .zip(angular_impulse)
            .map(|(coefficient, impulse)| coefficient * impulse)
            .sum::<f32>();
        assert!((angular_velocity[row] - expected_response).abs() < 1.0e-5);
    }
}

#[test]
fn coupling_body_snapshot_tracks_pose_and_mass_updates() {
    let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
    let body = world.add_hull(&cube(0.5), BodyConfig::default()).unwrap();
    let before = world.dynamics(body).unwrap();
    world
        .update_body(
            body,
            BodyConfig {
                position: [3.0, 4.0, 5.0],
                mass: 2.0,
                ..BodyConfig::default()
            },
            true,
        )
        .unwrap();
    let after = world.dynamics(body).unwrap();
    assert_ne!(before.center_of_mass, after.center_of_mass);
    assert_eq!(after.center_of_mass, [3.0, 4.0, 5.0]);
    assert!((after.inverse_mass - 0.5).abs() < 1.0e-6);
}

#[test]
fn coupling_body_snapshot_transforms_eccentric_local_center_of_mass() {
    let mut points = box_points([0.5, 0.5, 0.5]);
    for point in &mut points {
        point[0] += 2.0;
    }
    let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
    let body = world
        .add_hull(
            &points,
            BodyConfig {
                position: [3.0, 4.0, 5.0],
                rotation: z_rotation(std::f32::consts::FRAC_PI_2),
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let center = world.dynamics(body).unwrap().center_of_mass;
    assert!((center[0] - 3.0).abs() < 1.0e-5);
    assert!((center[1] - 6.0).abs() < 1.0e-5);
    assert!((center[2] - 5.0).abs() < 1.0e-5);
}

#[test]
fn coupling_body_mass_only_update_refreshes_inertia_and_eccentric_response() {
    let mut points = box_points([0.5, 0.5, 0.5]);
    for point in &mut points {
        point[0] += 2.0;
    }
    let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
    let body = world.add_hull(&points, BodyConfig::default()).unwrap();
    let before = world.dynamics(body).unwrap();
    world
        .update_body(
            body,
            BodyConfig {
                mass: 2.0,
                ..BodyConfig::default()
            },
            false,
        )
        .unwrap();
    let after = world.dynamics(body).unwrap();
    for (row, before_row) in before.inverse_inertia.iter().enumerate() {
        for (column, before_value) in before_row.iter().enumerate() {
            assert!((after.inverse_inertia[row][column] - before_value * 0.5).abs() < 1.0e-5);
        }
    }
    let angular_impulse = [0.0, 0.0, 1.0];
    world
        .apply_impulses(&[BodyImpulse {
            body,
            linear: [0.0; 3],
            angular: angular_impulse,
        }])
        .unwrap();
    let omega = world.angular_velocity(body).unwrap();
    let point_velocity = world
        .velocity_at_local_point(body, [2.0, 1.0, 0.0])
        .unwrap();
    assert!((omega[2] - after.inverse_inertia[2][2]).abs() < 1.0e-5);
    assert!((point_velocity[0] + omega[2]).abs() < 1.0e-5);
    assert!(point_velocity[1].abs() < 1.0e-5);
}

#[test]
fn coupling_body_zero_impulse_does_not_wake_sleeping_body() {
    let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
    let body = world.add_hull(&cube(0.5), BodyConfig::default()).unwrap();
    for _ in 0..120 {
        world.step(Seconds(1.0 / 60.0), 1).unwrap();
    }
    assert!(!world.dynamics(body).unwrap().awake);
    world
        .apply_impulses(&[BodyImpulse {
            body,
            linear: [0.0; 3],
            angular: [0.0; 3],
        }])
        .unwrap();
    assert!(!world.dynamics(body).unwrap().awake);
    world
        .apply_impulses(&[BodyImpulse {
            body,
            linear: [1.0, 0.0, 0.0],
            angular: [0.0; 3],
        }])
        .unwrap();
    assert!(world.dynamics(body).unwrap().awake);
}

#[test]
fn coupling_body_ignores_non_dynamic_and_disabled_recipients() {
    let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
    let fixed = world
        .add_hull(
            &cube(0.5),
            BodyConfig {
                kind: BodyKind::Fixed,
                mass: 0.0,
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let animated = world
        .add_hull(
            &cube(0.5),
            BodyConfig {
                kind: BodyKind::Animated,
                ..BodyConfig::default()
            },
        )
        .unwrap();
    world
        .set_velocity(animated, [1.0, 2.0, 3.0], [4.0, 5.0, 6.0])
        .unwrap();
    let disabled = world.add_hull(&cube(0.5), BodyConfig::default()).unwrap();
    world.set_enabled(disabled, false).unwrap();
    let before = [fixed, animated, disabled].map(|body| world.dynamics(body).unwrap());
    world
        .apply_impulses(&[
            BodyImpulse {
                body: fixed,
                linear: [1.0, 2.0, 3.0],
                angular: [4.0, 5.0, 6.0],
            },
            BodyImpulse {
                body: animated,
                linear: [1.0, 2.0, 3.0],
                angular: [4.0, 5.0, 6.0],
            },
            BodyImpulse {
                body: disabled,
                linear: [1.0, 2.0, 3.0],
                angular: [4.0, 5.0, 6.0],
            },
        ])
        .unwrap();
    for (body, expected) in [fixed, animated, disabled].into_iter().zip(before) {
        let after = world.dynamics(body).unwrap();
        assert_eq!(after.linear_velocity, expected.linear_velocity);
        assert_eq!(after.angular_velocity, expected.angular_velocity);
        assert_eq!(after.inverse_mass, 0.0);
        assert_eq!(after.inverse_inertia, [[0.0; 3]; 3]);
    }
}

#[test]
fn coupling_body_rejects_invalid_batches_atomically_and_reuses_scratch() {
    let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
    world.set_max_linear_speed(1.0).unwrap();
    let first = world.add_hull(&cube(0.5), BodyConfig::default()).unwrap();
    let second = world
        .add_hull(
            &cube(0.5),
            BodyConfig {
                position: [2.0, 0.0, 0.0],
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let mut foreign_world = PhysicsWorld::new([0.0; 3]).unwrap();
    let foreign = foreign_world
        .add_hull(&cube(0.5), BodyConfig::default())
        .unwrap();
    let capacity = world.impulse_scratch.capacity();
    let before = [
        world.linear_velocity(first).unwrap(),
        world.linear_velocity(second).unwrap(),
    ];

    assert!(
        world
            .apply_impulses(&[
                BodyImpulse {
                    body: first,
                    linear: [0.5, 0.0, 0.0],
                    angular: [0.0; 3],
                },
                BodyImpulse {
                    body: first,
                    linear: [0.5, 0.0, 0.0],
                    angular: [0.0; 3],
                },
            ])
            .is_err()
    );
    assert_eq!(world.linear_velocity(first).unwrap(), before[0]);
    assert!(
        world
            .apply_impulses(&[
                BodyImpulse {
                    body: first,
                    linear: [0.5, 0.0, 0.0],
                    angular: [0.0; 3],
                },
                BodyImpulse {
                    body: foreign,
                    linear: [0.5, 0.0, 0.0],
                    angular: [0.0; 3],
                },
            ])
            .is_err()
    );
    assert_eq!(world.linear_velocity(first).unwrap(), before[0]);
    assert!(
        world
            .apply_impulses(&[BodyImpulse {
                body: first,
                linear: [2.0, 0.0, 0.0],
                angular: [0.0; 3],
            }])
            .is_err()
    );
    assert!(
        world
            .apply_impulses(&[BodyImpulse {
                body: second,
                linear: [f32::MAX, 0.0, 0.0],
                angular: [0.0; 3],
            }])
            .is_err()
    );
    assert_eq!(world.linear_velocity(second).unwrap(), before[1]);
    assert!(
        world
            .apply_impulses(&[
                BodyImpulse {
                    body: first,
                    linear: [0.1, 0.0, 0.0],
                    angular: [0.0; 3],
                },
                BodyImpulse {
                    body: second,
                    linear: [0.0; 3],
                    angular: [f32::NAN, 0.0, 0.0],
                },
            ])
            .is_err()
    );
    assert_eq!(world.linear_velocity(first).unwrap(), before[0]);
    assert_eq!(world.linear_velocity(second).unwrap(), before[1]);
    let angular_before = [first, second].map(|body| world.angular_velocity(body).unwrap());
    assert!(
        world
            .apply_impulses(&[
                BodyImpulse {
                    body: first,
                    linear: [0.1, 0.0, 0.0],
                    angular: [0.2, -0.1, 0.3],
                },
                BodyImpulse {
                    body: second,
                    linear: [0.1, 0.0, 0.0],
                    angular: [f32::MAX, f32::MAX, f32::MAX],
                },
            ])
            .is_err()
    );
    for (index, body) in [first, second].into_iter().enumerate() {
        assert_eq!(world.linear_velocity(body).unwrap(), before[index]);
        assert_eq!(world.angular_velocity(body).unwrap(), angular_before[index]);
    }
    assert!(
        world
            .apply_impulses(&[
                BodyImpulse {
                    body: first,
                    linear: [0.1, 0.0, 0.0],
                    angular: [0.0; 3],
                },
                BodyImpulse {
                    body: second,
                    linear: [0.1, 0.0, 0.0],
                    angular: [0.0; 3],
                },
            ])
            .is_ok()
    );
    let repeated_capacity = world.impulse_scratch.capacity();
    for _ in 0..4 {
        world
            .apply_impulses(&[
                BodyImpulse {
                    body: first,
                    linear: [0.1, 0.0, 0.0],
                    angular: [0.0; 3],
                },
                BodyImpulse {
                    body: second,
                    linear: [0.1, 0.0, 0.0],
                    angular: [0.0; 3],
                },
            ])
            .unwrap();
    }
    assert_eq!(world.impulse_scratch.capacity(), repeated_capacity);
    assert_eq!(world.impulse_scratch.capacity(), capacity);
}

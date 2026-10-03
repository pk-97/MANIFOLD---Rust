use super::*;

use manifold_physics::{BodyConfig, BodyHandle, BodyKind, PhysicsWorld};

fn small_fluid() -> FluidWorld {
    let mut fluid = FluidWorld::new(Config {
        cells: [8; 3],
        cell_size: 0.25,
        surface_subdivisions: 0,
        apic: false,
    })
    .unwrap();
    fluid.set_gravity([0.0; 3]).unwrap();
    fluid
        .set_time_step_options(TimeStepOptions {
            min_substeps: 1,
            max_substeps: 32,
            cfl: 1,
            adaptive_obstacles: false,
        })
        .unwrap();
    fluid
}

fn body_fixture() -> (FluidWorld, PhysicsWorld, BodyHandle, MeshHandle) {
    let mesh = proxy();
    let mut rigid = PhysicsWorld::new([0.0; 3]).unwrap();
    let body = rigid
        .add_hull(
            &mesh.vertices,
            BodyConfig {
                kind: BodyKind::Dynamic,
                position: [1.0; 3],
                mass: 100.0,
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let mut fluid = small_fluid();
    let collider = fluid
        .add_mesh(&mesh, MeshRole::Collider, rigid.pose(body).unwrap())
        .unwrap();
    (fluid, rigid, body, collider)
}

fn state(rigid: &PhysicsWorld, body: BodyHandle) -> RigidBodyState {
    RigidBodyState {
        pose: rigid.pose(body).unwrap(),
        dynamics: rigid.dynamics(body).unwrap(),
    }
}

fn assert_valid_retry(
    fluid: &mut FluidWorld,
    rigid: &PhysicsWorld,
    body: BodyHandle,
    collider: MeshHandle,
) {
    fluid.prepare_rigid_coupling(&[collider], 1000.0).unwrap();
    let mut frame = fluid.begin_frame(DT).unwrap();
    frame.set_rigid_bodies(&[state(rigid, body)]).unwrap();
    let dt = frame
        .next_substep()
        .unwrap()
        .expect("empty world still offers a step");
    frame.advance(dt).unwrap();
    assert_eq!(frame.rigid_reactions().unwrap().len(), 1);
    assert_eq!(frame.next_substep().unwrap(), None);
    frame.finish().unwrap();
}

#[test]
fn production_coupling_boundary_prepare_rejects_invalid_inputs_and_allows_retry() {
    for density in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let (mut fluid, rigid, body, collider) = body_fixture();
        assert!(fluid.prepare_rigid_coupling(&[collider], density).is_err());
        assert_valid_retry(&mut fluid, &rigid, body, collider);
    }

    {
        let (mut fluid, rigid, body, collider) = body_fixture();
        assert!(
            fluid
                .prepare_rigid_coupling(&[collider, collider], 1000.0)
                .is_err()
        );
        assert_valid_retry(&mut fluid, &rigid, body, collider);
    }

    {
        let (mut fluid, rigid, body, collider) = body_fixture();
        let (foreign_fluid, _foreign_rigid, _foreign_body, foreign_collider) = body_fixture();
        assert!(
            fluid
                .prepare_rigid_coupling(&[foreign_collider], 1000.0)
                .is_err()
        );
        drop(foreign_fluid);
        assert_valid_retry(&mut fluid, &rigid, body, collider);
    }

    {
        let (mut fluid, rigid, body, collider) = body_fixture();
        fluid.remove_mesh(collider).unwrap();
        assert!(fluid.prepare_rigid_coupling(&[collider], 1000.0).is_err());
        let replacement = fluid
            .add_mesh(&proxy(), MeshRole::Collider, rigid.pose(body).unwrap())
            .unwrap();
        assert_valid_retry(&mut fluid, &rigid, body, replacement);
    }

    {
        let (mut fluid, rigid, body, collider) = body_fixture();
        let inflow = fluid
            .add_mesh(&proxy(), MeshRole::Inflow, rigid.pose(body).unwrap())
            .unwrap();
        assert!(fluid.prepare_rigid_coupling(&[inflow], 1000.0).is_err());
        assert_valid_retry(&mut fluid, &rigid, body, collider);
    }
}

#[test]
fn production_coupling_boundary_invalid_native_body_mapping_leaves_topology_retryable() {
    // Exercise the C boundary's validation independently of the safe grouped
    // API, which constructs a complete, in-range mapping by definition.
    for (indices, collider_count, body_count) in [
        (Some([0, 2]), 2, 2),
        (Some([0, 0]), 2, 2),
        (Some([0, 1]), 2, 3),
        (Some([0, 1]), 0, 0),
        (None, 2, 2),
    ] {
        let (mut fluid, rigid, body, collider) = body_fixture();
        let extra = fluid
            .add_mesh(&proxy(), MeshRole::Collider, rigid.pose(body).unwrap())
            .unwrap();
        let slots = [collider, extra].map(|handle| {
            fluid
                .mesh_state
                .validate_handle(handle, Some(MeshRole::Collider))
                .unwrap() as u32
        });
        let index_ptr = indices
            .as_ref()
            .map_or(std::ptr::null(), |indices| indices.as_ptr());
        let result = unsafe {
            manifold_fluids_world_prepare_rigid_coupling(
                fluid.native,
                slots.as_ptr(),
                index_ptr,
                collider_count,
                body_count,
                1000.0,
            )
        };
        assert_eq!(result, 0);
        // Both colliders must remain editable after rejected preparation.
        fluid.set_mesh_enabled(extra, false).unwrap();
        fluid.set_mesh_enabled(extra, true).unwrap();
        fluid
            .prepare_rigid_coupling_groups(&[&[collider, extra]], 1000.0)
            .unwrap();
        let mut frame = fluid.begin_frame(DT).unwrap();
        frame.set_rigid_bodies(&[state(&rigid, body)]).unwrap();
        let dt = frame.next_substep().unwrap().unwrap();
        frame.advance(dt).unwrap();
        assert_eq!(frame.rigid_reactions().unwrap().len(), 1);
        frame.finish().unwrap();
    }
}

fn coupled_pair_fixture() -> (FluidWorld, PhysicsWorld, [BodyHandle; 2], [MeshHandle; 2]) {
    let mesh = proxy();
    let mut rigid = PhysicsWorld::new([0.0; 3]).unwrap();
    let angle_a = 0.19_f32;
    let angle_b = -0.31_f32;
    let bodies = [
        rigid
            .add_hull(
                &mesh.vertices,
                BodyConfig {
                    kind: BodyKind::Dynamic,
                    position: [0.86, 1.03, 1.0],
                    rotation: [0.0, angle_a.sin(), 0.0, angle_a.cos()],
                    mass: 72.0,
                    ..BodyConfig::default()
                },
            )
            .unwrap(),
        rigid
            .add_hull(
                &mesh.vertices,
                BodyConfig {
                    kind: BodyKind::Dynamic,
                    position: [1.56, 1.08, 1.24],
                    rotation: [0.0, angle_b.sin(), 0.0, angle_b.cos()],
                    mass: 180.0,
                    ..BodyConfig::default()
                },
            )
            .unwrap(),
    ];
    rigid
        .set_velocity(bodies[0], [0.65, 0.0, 0.15], [0.0, 0.42, 0.0])
        .unwrap();
    rigid
        .set_velocity(bodies[1], [-0.25, 0.0, -0.2], [0.0, -0.3, 0.18])
        .unwrap();

    let mut fluid = FluidWorld::new(Config {
        cells: [16; 3],
        cell_size: 0.15,
        surface_subdivisions: 0,
        apic: false,
    })
    .unwrap();
    fluid.set_gravity([0.0; 3]).unwrap();
    fluid
        .set_time_step_options(TimeStepOptions {
            min_substeps: 2,
            max_substeps: 32,
            cfl: 1,
            adaptive_obstacles: false,
        })
        .unwrap();
    fluid
        .set_liquid_options(LiquidOptions {
            viscosity: 1.0,
            surface_tension: 0.0,
        })
        .unwrap();
    let colliders = [
        fluid
            .add_mesh(&mesh, MeshRole::Collider, rigid.pose(bodies[0]).unwrap())
            .unwrap(),
        fluid
            .add_mesh(&mesh, MeshRole::Collider, rigid.pose(bodies[1]).unwrap())
            .unwrap(),
    ];
    fluid.set_collider_friction(colliders[0], 1.0).unwrap();
    fluid.set_collider_friction(colliders[1], 1.0).unwrap();
    fluid
        .add_fluid_box(
            Bounds {
                min: [0.35, 0.45, 0.35],
                max: [1.95, 1.65, 1.95],
            },
            [0.0; 3],
        )
        .unwrap();
    fluid.step(DT).unwrap();
    fluid.prepare_rigid_coupling(&colliders, 1000.0).unwrap();
    (fluid, rigid, bodies, colliders)
}

#[test]
fn production_coupling_boundary_batch_upload_preserves_order_and_box3d_response() {
    let (mut fluid, mut rigid, bodies, _) = coupled_pair_fixture();
    let mut states = bodies.map(|body| state(&rigid, body));
    let mut invalid = states;
    invalid[1].dynamics.linear_velocity[0] = f32::NAN;

    let mut frame = fluid.begin_frame(DT).unwrap();
    assert!(frame.set_rigid_bodies(&invalid).is_err());
    assert!(frame.rigid_reactions().is_err());
    assert!(frame.next_substep().is_err());

    frame.set_rigid_bodies(&states).unwrap();
    let mut elapsed = 0.0;
    let mut first = true;
    let mut total_reaction = [0.0_f64; 2];
    while elapsed < DT.0 - 1e-12 {
        let dt = frame.next_substep().unwrap().expect("coupled liquid step");
        let before = bodies.map(|body| rigid.dynamics(body).unwrap());
        frame.advance(dt).unwrap();
        let reactions = frame.rigid_reactions().unwrap().to_vec();
        assert_eq!(reactions.len(), 2);
        for (total, reaction) in total_reaction.iter_mut().zip(&reactions) {
            *total += reaction
                .linear
                .iter()
                .chain(reaction.angular.iter())
                .map(|value| value.abs())
                .sum::<f64>();
        }
        let impulses = [
            reactions[0].body_impulse(bodies[0]).unwrap(),
            reactions[1].body_impulse(bodies[1]).unwrap(),
        ];
        rigid.apply_impulses(&impulses).unwrap();
        for ((body, reaction), before_dynamics) in bodies.iter().zip(&reactions).zip(before) {
            let after = rigid.dynamics(*body).unwrap();
            for axis in 0..3 {
                let linear_error =
                    f64::from(after.linear_velocity[axis] - before_dynamics.linear_velocity[axis])
                        - reaction.delta_linear[axis];
                let angular_error = f64::from(
                    after.angular_velocity[axis] - before_dynamics.angular_velocity[axis],
                ) - reaction.delta_angular[axis];
                assert!(
                    linear_error.abs() < 5e-5,
                    "linear response error: {linear_error}"
                );
                assert!(
                    angular_error.abs() < 5e-5,
                    "angular response error: {angular_error}"
                );
            }
        }
        if first {
            assert!(total_reaction.iter().all(|value| *value > 1e-8));
            first = false;
        }
        elapsed += dt.0;
        if elapsed < DT.0 - 1e-12 {
            states = bodies.map(|body| state(&rigid, body));
            frame.set_rigid_bodies(&states).unwrap();
        }
    }
    assert_eq!(frame.next_substep().unwrap(), None);
    frame.finish().unwrap();
}

fn large_proxy() -> TriangleMesh {
    let [x, y, z] = [1.1, 1.1, 1.1];
    TriangleMesh {
        vertices: vec![
            [-x, -y, -z],
            [x, -y, -z],
            [x, y, -z],
            [-x, y, -z],
            [-x, -y, z],
            [x, -y, z],
            [x, y, z],
            [-x, y, z],
        ],
        triangles: proxy().triangles,
    }
}

#[test]
fn production_coupling_boundary_cfl_counts_prescribed_speed_outside_domain_vertices() {
    let mesh = large_proxy();
    let mut rigid = PhysicsWorld::new([0.0; 3]).unwrap();
    let body = rigid
        .add_hull(
            &mesh.vertices,
            BodyConfig {
                kind: BodyKind::Animated,
                position: [1.0; 3],
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let mut fluid = small_fluid();
    let collider = fluid
        .add_mesh(&mesh, MeshRole::Collider, rigid.pose(body).unwrap())
        .unwrap();
    fluid
        .add_fluid_box(
            Bounds {
                min: [0.5; 3],
                max: [1.5; 3],
            },
            [0.0; 3],
        )
        .unwrap();
    fluid.prepare_rigid_coupling(&[collider], 1000.0).unwrap();

    let mut body_state = state(&rigid, body);
    body_state.dynamics.linear_velocity = [100.0, 0.0, 0.0];
    let mut frame = fluid.begin_frame(DT).unwrap();
    frame.set_rigid_bodies(&[body_state]).unwrap();
    let offered = frame
        .next_substep()
        .unwrap()
        .expect("liquid offers a CFL step");
    let limit = 0.25 / 100.0;
    assert!(
        offered.0 <= limit * (1.0 + 1e-6),
        "offered {offered:?} exceeds rigid speed limit {limit}"
    );
}

/// A coupled body wholly outside the domain cannot touch the liquid, so a
/// body falling far below it at 250 m/s leaves the liquid its whole frame.
#[test]
fn production_coupling_boundary_cfl_ignores_a_body_wholly_outside_the_domain() {
    let mesh = proxy();
    let mut rigid = PhysicsWorld::new([0.0; 3]).unwrap();
    let body = rigid
        .add_hull(
            &mesh.vertices,
            BodyConfig {
                kind: BodyKind::Animated,
                position: [1.0, -5.0, 1.0],
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let mut fluid = small_fluid();
    let collider = fluid
        .add_mesh(&mesh, MeshRole::Collider, rigid.pose(body).unwrap())
        .unwrap();
    fluid
        .add_fluid_box(
            Bounds {
                min: [0.5; 3],
                max: [1.5; 3],
            },
            [0.0; 3],
        )
        .unwrap();
    fluid.prepare_rigid_coupling(&[collider], 1000.0).unwrap();

    let mut body_state = state(&rigid, body);
    body_state.dynamics.linear_velocity = [0.0, -250.0, 0.0];
    let mut frame = fluid.begin_frame(DT).unwrap();
    frame.set_rigid_bodies(&[body_state]).unwrap();
    let offered = frame
        .next_substep()
        .unwrap()
        .expect("liquid offers a CFL step");
    assert!(
        (offered.0 - DT.0).abs() < 1e-12,
        "offered {offered:?}; a body outside the domain must not split the {DT:?} frame"
    );
}

#[test]
fn production_coupling_boundary_cfl_counts_pending_external_acceleration() {
    let (mut fluid, rigid, body, collider) = body_fixture();
    // Native obstacle CFL applies while liquid is present or being generated.
    fluid
        .add_fluid_box(
            Bounds {
                min: [0.5; 3],
                max: [1.5; 3],
            },
            [0.0; 3],
        )
        .unwrap();
    fluid.prepare_rigid_coupling(&[collider], 1000.0).unwrap();
    let mut input = state(&rigid, body);
    input.dynamics.external_linear_acceleration = [6000.0, 0.0, 0.0];
    assert_eq!(input.dynamics.linear_velocity, [0.0; 3]);
    let mut frame = fluid.begin_frame(DT).unwrap();
    frame.set_rigid_bodies(&[input]).unwrap();
    let offered = frame.next_substep().unwrap().unwrap();
    let limit = 0.25 / (6000.0 * DT.0);
    assert!(
        offered.0 <= limit * (1.0 + 1e-6),
        "offered {offered:?} exceeds predicted boundary speed limit {limit}"
    );
}

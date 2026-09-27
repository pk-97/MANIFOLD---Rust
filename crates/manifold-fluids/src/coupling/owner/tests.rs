use super::*;
use crate::{Bounds, Config, LiquidOptions, TimeStepOptions};
use manifold_physics::{
    BodyConfig, BodyHandle, BodyPose, PhysicsWorld, Seconds, TriangleMesh, cook_hull_mesh,
    stepping::SubstepExchange,
};

const DT: Seconds = Seconds(1.0 / 60.0);
const ORIGIN: [f32; 3] = [4.0, -2.0, 8.0];
const CENTRE: [f32; 3] = [1.2, 1.05, 1.2];

struct Fixture {
    fluid: FluidWorld,
    rigid: PhysicsWorld,
    body: BodyHandle,
    collider: MeshHandle,
}

struct PreparedFixture {
    fluid: FluidWorld,
    rigid: PhysicsWorld,
    body: BodyHandle,
    coupling: RigidFluidCoupling,
}

fn cuboid() -> TriangleMesh {
    let points = [
        [-0.25, -0.2, -0.225],
        [0.25, -0.2, -0.225],
        [0.25, 0.2, -0.225],
        [-0.25, 0.2, -0.225],
        [-0.25, -0.2, 0.225],
        [0.25, -0.2, 0.225],
        [0.25, 0.2, 0.225],
        [-0.25, 0.2, 0.225],
    ];
    cook_hull_mesh(&points).expect("the compact cuboid should cook")
}

fn fixture(filled: bool) -> Fixture {
    let mesh = cuboid();
    let mut rigid = PhysicsWorld::new([0.0; 3]).unwrap();
    let body = rigid
        .add_hull(
            &mesh.vertices,
            BodyConfig {
                position: [
                    CENTRE[0] + ORIGIN[0],
                    CENTRE[1] + ORIGIN[1],
                    CENTRE[2] + ORIGIN[2],
                ],
                mass: 90.0,
                ..BodyConfig::default()
            },
        )
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
            max_substeps: 8,
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
    let collider = fluid
        .add_mesh(
            &mesh,
            MeshRole::Collider,
            BodyPose {
                position: CENTRE,
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
        )
        .unwrap();
    if filled {
        fluid
            .add_fluid_box(
                Bounds {
                    min: [0.45; 3],
                    max: [1.95, 1.65, 1.95],
                },
                [0.0; 3],
            )
            .unwrap();
        fluid.step(DT).unwrap();
    }

    Fixture {
        fluid,
        rigid,
        body,
        collider,
    }
}

fn prepared_fixture(filled: bool) -> PreparedFixture {
    let Fixture {
        mut fluid,
        rigid,
        body,
        collider,
    } = fixture(filled);
    let coupling =
        RigidFluidCoupling::prepare(&mut fluid, &rigid, &[(collider, body)], ORIGIN, 1000.0)
            .unwrap();
    PreparedFixture {
        fluid,
        rigid,
        body,
        coupling,
    }
}

fn complete_frame(fixture: &mut PreparedFixture) -> usize {
    let mut frame = fixture
        .coupling
        .begin_frame(&mut fixture.fluid, DT, &[])
        .unwrap();
    let mut substeps = 0;
    let mut remaining = DT.0;
    while remaining > 0.0 {
        let duration = frame
            .next_substep(&fixture.rigid, Seconds(remaining))
            .unwrap_or_else(|error| panic!("offering substep failed: {error}"));
        frame
            .exchange(&mut fixture.rigid, duration)
            .unwrap_or_else(|error| panic!("exchanging substep failed: {error}"));
        fixture.rigid.step(duration, 4).unwrap();
        remaining -= duration.0;
        substeps += 1;
    }
    frame.finish().unwrap();
    substeps
}

#[test]
fn owner_prepare_rejects_invalid_duplicate_and_foreign_bindings_but_can_retry() {
    let Fixture {
        mut fluid,
        rigid,
        body,
        collider,
    } = fixture(false);
    assert!(RigidFluidCoupling::prepare(&mut fluid, &rigid, &[], ORIGIN, 1000.0).is_err());
    assert!(
        RigidFluidCoupling::prepare(&mut fluid, &rigid, &[(collider, body)], ORIGIN, 1000.0,)
            .is_ok()
    );

    let Fixture {
        mut fluid,
        rigid,
        body,
        collider,
    } = fixture(false);
    assert!(
        RigidFluidCoupling::prepare(
            &mut fluid,
            &rigid,
            &[(collider, body), (collider, body)],
            ORIGIN,
            1000.0,
        )
        .is_err()
    );
    assert!(
        RigidFluidCoupling::prepare(&mut fluid, &rigid, &[(collider, body)], ORIGIN, 1000.0,)
            .is_ok()
    );

    let Fixture {
        mut fluid,
        rigid,
        body,
        collider,
    } = fixture(false);
    let foreign = fixture(false);
    assert!(
        RigidFluidCoupling::prepare(
            &mut fluid,
            &rigid,
            &[(foreign.collider, body)],
            ORIGIN,
            1000.0,
        )
        .is_err()
    );
    assert!(
        RigidFluidCoupling::prepare(&mut fluid, &rigid, &[(collider, body)], ORIGIN, 1000.0,)
            .is_ok()
    );
}

#[test]
fn owner_begin_frame_rejects_foreign_fluid_before_mutation() {
    let mut local = prepared_fixture(false);
    let mut foreign = fixture(false);
    assert!(
        local
            .coupling
            .begin_frame(&mut foreign.fluid, DT, &[])
            .is_err()
    );
    assert!(foreign.fluid.step(DT).is_ok());
}

#[test]
fn owner_rejects_stale_pending_exchange_until_state_is_restored() {
    let mut fixture = prepared_fixture(true);
    let mut foreign = PhysicsWorld::new([0.0; 3]).unwrap();
    let mut frame = fixture
        .coupling
        .begin_frame(&mut fixture.fluid, DT, &[])
        .unwrap();
    let offered = frame.next_substep(&fixture.rigid, DT).unwrap();
    assert!(
        frame
            .exchange(&mut fixture.rigid, Seconds(offered.0 * 0.5))
            .is_err()
    );

    let before = fixture.rigid.dynamics(fixture.body).unwrap();
    fixture
        .rigid
        .set_velocity(fixture.body, [0.6, 0.0, 0.0], [0.0, 0.4, 0.0])
        .unwrap();
    assert!(frame.exchange(&mut fixture.rigid, offered).is_err());
    fixture
        .rigid
        .set_velocity(
            fixture.body,
            before.linear_velocity,
            before.angular_velocity,
        )
        .unwrap();
    assert!(frame.exchange(&mut foreign, offered).is_err());
    frame.exchange(&mut fixture.rigid, offered).unwrap();
    assert!(frame.exchange(&mut fixture.rigid, offered).is_err());
    fixture.rigid.step(offered, 4).unwrap();

    let mut substeps = 1;
    let mut remaining = DT.0 - offered.0;
    while remaining > 0.0 {
        let duration = frame
            .next_substep(&fixture.rigid, Seconds(remaining))
            .unwrap();
        frame.exchange(&mut fixture.rigid, duration).unwrap();
        fixture.rigid.step(duration, 4).unwrap();
        remaining -= duration.0;
        substeps += 1;
    }
    assert_eq!(substeps, 2);
    frame.finish().unwrap();
}

#[test]
fn owner_reuses_scratch_and_invalidates_stats_and_snapshots_at_frame_boundaries() {
    let mut fixture = prepared_fixture(true);
    let states_ptr = fixture.coupling.states.as_ptr();
    let states_capacity = fixture.coupling.states.capacity();
    let impulses_ptr = fixture.coupling.impulses.as_ptr();
    let impulses_capacity = fixture.coupling.impulses.capacity();

    assert_eq!(complete_frame(&mut fixture), 2);
    assert!(fixture.coupling.last_stats().is_some());

    let mut frame = fixture
        .coupling
        .begin_frame(&mut fixture.fluid, DT, &[])
        .unwrap();
    assert!(frame.coupling.last_stats.is_none());
    let mut remaining = DT.0;
    while remaining > 0.0 {
        let duration = frame
            .next_substep(&fixture.rigid, Seconds(remaining))
            .unwrap();
        frame.exchange(&mut fixture.rigid, duration).unwrap();
        fixture.rigid.step(duration, 4).unwrap();
        remaining -= duration.0;
    }
    frame.finish().unwrap();
    assert!(fixture.coupling.last_stats().is_some());
    assert_eq!(fixture.coupling.states.as_ptr(), states_ptr);
    assert_eq!(fixture.coupling.states.capacity(), states_capacity);
    assert_eq!(fixture.coupling.impulses.as_ptr(), impulses_ptr);
    assert_eq!(fixture.coupling.impulses.capacity(), impulses_capacity);

    let frame = fixture
        .coupling
        .begin_frame(&mut fixture.fluid, DT, &[])
        .unwrap();
    drop(frame);
    assert!(fixture.fluid.surface(&mut Vec::new()).is_err());
    assert!(fixture.coupling.last_stats().is_none());
}

#[test]
fn owner_reaction_application_failure_latches_filled_frame_without_mutating_velocity() {
    let mut fixture = prepared_fixture(true);
    fixture.rigid.set_max_linear_speed(0.0001).unwrap();
    fixture
        .rigid
        .set_velocity(fixture.body, [0.6, 0.0, 0.0], [0.0, 0.4, 0.0])
        .unwrap();
    let before = fixture.rigid.dynamics(fixture.body).unwrap();

    let mut frame = fixture
        .coupling
        .begin_frame(&mut fixture.fluid, DT, &[])
        .unwrap();
    let offered = frame.next_substep(&fixture.rigid, DT).unwrap();
    assert!(frame.exchange(&mut fixture.rigid, offered).is_err());
    let after = fixture.rigid.dynamics(fixture.body).unwrap();
    assert_eq!(after.linear_velocity, before.linear_velocity);
    assert_eq!(after.angular_velocity, before.angular_velocity);
    assert!(frame.next_substep(&fixture.rigid, DT).is_err());
    assert!(frame.finish().is_err());
    assert!(fixture.coupling.last_stats().is_none());
    assert!(
        fixture
            .coupling
            .begin_frame(&mut fixture.fluid, DT, &[])
            .is_err()
    );
}

#[test]
fn owner_compound_interleaved_bindings_apply_once_per_body_and_reuse_storage() {
    let narrow: Vec<_> = cuboid()
        .vertices
        .iter()
        .map(|p| [p[0] * 0.48, p[1], p[2]])
        .collect();
    let mut rigid = PhysicsWorld::new([0.0; 3]).unwrap();
    let single = rigid
        .add_hull(
            &narrow,
            BodyConfig {
                position: [ORIGIN[0] + 0.65, ORIGIN[1] + 1.05, ORIGIN[2] + 1.2],
                mass: 60.0,
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let parts: Vec<Vec<_>> = [-0.15, 0.15]
        .iter()
        .map(|x| narrow.iter().map(|p| [p[0] + x, p[1], p[2]]).collect())
        .collect();
    let compound = rigid
        .add_hulls(
            &parts,
            BodyConfig {
                position: [ORIGIN[0] + 1.65, ORIGIN[1] + 1.05, ORIGIN[2] + 1.2],
                mass: 120.0,
                ..BodyConfig::default()
            },
        )
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
    let mut handles = Vec::new();
    for body in [single, compound] {
        let mut pose = rigid.pose(body).unwrap();
        for (axis, offset) in ORIGIN.into_iter().enumerate() {
            pose.position[axis] -= offset;
        }
        for mesh in rigid.hull_meshes(body).unwrap() {
            handles.push(fluid.add_mesh(&mesh, MeshRole::Collider, pose).unwrap());
        }
    }
    fluid
        .add_fluid_box(
            Bounds {
                min: [0.45; 3],
                max: [1.95, 1.65, 1.95],
            },
            [0.0; 3],
        )
        .unwrap();
    fluid.step(DT).unwrap();
    let mut coupling = RigidFluidCoupling::prepare(
        &mut fluid,
        &rigid,
        &[
            (handles[1], compound),
            (handles[0], single),
            (handles[2], compound),
        ],
        ORIGIN,
        1000.0,
    )
    .unwrap();
    assert_eq!(coupling.bodies, [compound, single]);
    assert_eq!(coupling.states.len(), 2);
    assert_eq!(coupling.colliders.len(), 3);
    rigid
        .set_velocity(single, [0.5, 0.0, 0.0], [0.0; 3])
        .unwrap();
    rigid
        .set_velocity(compound, [0.0, 0.0, -0.4], [0.0; 3])
        .unwrap();
    let pointers = (coupling.states.as_ptr(), coupling.impulses.as_ptr());
    let capacities = (coupling.states.capacity(), coupling.impulses.capacity());
    let mut total_reaction = 0.0_f32;
    for _ in 0..2 {
        let mut frame = coupling.begin_frame(&mut fluid, DT, &[]).unwrap();
        let mut remaining = DT.0;
        while remaining > 0.0 {
            let duration = frame.next_substep(&rigid, Seconds(remaining)).unwrap();
            let before = [compound, single].map(|body| rigid.dynamics(body).unwrap());
            frame.exchange(&mut rigid, duration).unwrap();
            assert_eq!(frame.coupling.impulses.len(), 2);
            for ((impulse, before), body) in frame
                .coupling
                .impulses
                .iter()
                .zip(before)
                .zip([compound, single])
            {
                assert_eq!(impulse.body, body);
                let after = rigid.dynamics(body).unwrap();
                total_reaction += impulse
                    .linear
                    .iter()
                    .chain(&impulse.angular)
                    .map(|v| v.abs())
                    .sum::<f32>();
                for axis in 0..3 {
                    let expected_v =
                        before.linear_velocity[axis] + before.inverse_mass * impulse.linear[axis];
                    let expected_w = before.angular_velocity[axis]
                        + before.inverse_inertia[axis]
                            .iter()
                            .zip(impulse.angular)
                            .map(|(m, j)| m * j)
                            .sum::<f32>();
                    assert!((after.linear_velocity[axis] - expected_v).abs() < 5e-5);
                    assert!((after.angular_velocity[axis] - expected_w).abs() < 5e-5);
                }
            }
            rigid.step(duration, 4).unwrap();
            remaining -= duration.0;
        }
        frame.finish().unwrap();
    }
    assert!(total_reaction > 1e-3);
    assert_eq!(
        (coupling.states.as_ptr(), coupling.impulses.as_ptr()),
        pointers
    );
    assert_eq!(
        (coupling.states.capacity(), coupling.impulses.capacity()),
        capacities
    );
}

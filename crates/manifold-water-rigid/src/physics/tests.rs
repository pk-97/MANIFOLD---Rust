use super::*;

mod live_clock;

const GRAVITY: [f32; 3] = [0.0, -9.8, 0.0];
const FRAME: f64 = 1.0 / 60.0;

/// Live preview at 60 Hz, what a render frame passes the world.
pub(super) fn live_step() -> SimStep {
    SimStep::live(FIXED_TICK)
}

fn body(position: [f32; 3]) -> RigidBody {
    RigidBody {
        transform: Transform {
            pos: position,
            ..Transform::default()
        },
        ..RigidBody::default()
    }
}

/// Mass is the card value, not the size: Enable Physics on the Dam Break obstacle wires its
/// unit `cube_mesh` source into the body, so the hull is the drawn box at
/// scale (0.6, 1.16, 0.85): about 0.59 m³, and still 600 kg as the card says.
/// Box3D reports that mass back.
#[test]
fn dam_break_obstacle_mass_is_the_card_mass_whatever_the_hull_volume() {
    let scale = [0.6, 1.16, 0.85];
    let points: Vec<_> = manifold_node_engine::scene::mesh_source::MeshSource::Cube { size: 1.0 }
        .load_vertices()
        .unwrap()
        .into_iter()
        .map(|vertex| vertex.position)
        .collect();
    let obstacle = RigidBody {
        transform: Transform {
            scale,
            ..Transform::default()
        },
        collider: Some(Arc::new(Solid::Hulls(vec![
            manifold_physics::cook_hull(&points).unwrap(),
        ]))),
        ..RigidBody::default()
    };
    let mut world = PhysicsWorld::new(GRAVITY).unwrap();
    let (handle, volume) = add_body_geometry(&mut world, &obstacle).unwrap();
    let box_volume: f32 = scale.iter().product();
    assert!(
        (volume - box_volume).abs() < 1e-3 * box_volume,
        "hull {volume} m³ vs box {box_volume} m³"
    );
    let mass = DEFAULT_MASS_KG;
    let reported = 1.0 / world.dynamics(handle).unwrap().inverse_mass;
    assert!(
        (reported - mass).abs() < 1e-3 * mass,
        "Box3D mass {reported} kg vs {mass} kg"
    );
}

fn one_body(position: [f32; 3]) -> Vec<Option<RigidBody>> {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(body(position));
    bodies
}

#[test]
fn fixed_tick_results_are_frame_partition_invariant() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut per_frame = RigidSimulation::default();
    let mut partitioned = RigidSimulation::default();
    per_frame
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    partitioned
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();

    for frame in 1..=60 {
        per_frame
            .advance(
                &bodies,
                GRAVITY,
                PlayedTime::from_played(frame as f64 * FRAME),
                1.0,
                0.0,
            )
            .unwrap();
    }
    for half_frame in 1..=120 {
        partitioned
            .advance(
                &bodies,
                GRAVITY,
                PlayedTime::from_played(half_frame as f64 * FRAME / 2.0),
                1.0,
                0.0,
            )
            .unwrap();
    }

    assert!((per_frame.poses[0].pos[1] - partitioned.poses[0].pos[1]).abs() < 1.0e-5);
    assert!((per_frame.poses[0].pos[0] - partitioned.poses[0].pos[0]).abs() < 1.0e-5);
}

#[test]
fn uniform_field_is_mass_independent() {
    let field = FieldValue::uniform([0.0, -4.0, 0.0]).unwrap();
    let light_body = one_body([0.0, 8.0, 0.0]);
    let mut heavy_body = one_body([10.0, 8.0, 0.0]);
    heavy_body[0].as_mut().unwrap().mass *= 7.0;
    let mut light = RigidSimulation::default();
    let mut heavy = RigidSimulation::default();
    light
        .advance_scene(
            &copies_scene(
                &light_body,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Some(field.clone()),
                &[],
            ),
            PlayedTime::ZERO,
            1.0,
            0.0,
        )
        .unwrap();
    heavy
        .advance_scene(
            &copies_scene(
                &heavy_body,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Some(field.clone()),
                &[],
            ),
            PlayedTime::ZERO,
            1.0,
            0.0,
        )
        .unwrap();
    light
        .advance_scene(
            &copies_scene(
                &light_body,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Some(field.clone()),
                &[],
            ),
            PlayedTime::from_played(1.0),
            1.0,
            0.0,
        )
        .unwrap();
    heavy
        .advance_scene(
            &copies_scene(
                &heavy_body,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Some(field),
                &[],
            ),
            PlayedTime::from_played(1.0),
            1.0,
            0.0,
        )
        .unwrap();

    assert!((light.poses[0].pos[1] - heavy.poses[0].pos[1]).abs() < 1.0e-5);
    let light_velocity = light
        .world
        .as_ref()
        .unwrap()
        .linear_velocity(light.handles[0].unwrap())
        .unwrap();
    let heavy_velocity = heavy
        .world
        .as_ref()
        .unwrap()
        .linear_velocity(heavy.handles[0].unwrap())
        .unwrap();
    for (actual, expected) in light_velocity.iter().zip(heavy_velocity) {
        assert!((actual - expected).abs() < 1.0e-5);
    }
    assert!((light_velocity[1] + 4.0).abs() < 0.1);
}

#[test]
fn fixed_bodies_ignore_field_while_copies_receive_it() {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        kind: 0,
        transform: Transform {
            pos: [0.0, -100.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    let prototype = body([0.0, 8.0, 0.0]);
    let field = FieldValue::uniform([0.0, -4.0, 0.0]).unwrap();
    let mut simulation = RigidSimulation::default();
    simulation
        .advance_scene(
            &copies_scene(
                &bodies,
                Some(prototype.clone()),
                1.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Some(field.clone()),
                &[],
            ),
            PlayedTime::ZERO,
            1.0,
            0.0,
        )
        .unwrap();
    simulation
        .advance_scene(
            &copies_scene(
                &bodies,
                Some(prototype),
                1.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Some(field),
                &[],
            ),
            PlayedTime::from_played(1.0),
            1.0,
            0.0,
        )
        .unwrap();

    assert_eq!(simulation.poses[0].pos, [0.0, -100.0, 0.0]);
    assert!(simulation.group_poses(0)[0].pos[1] < 8.0);
}

fn run_uniform_field_trace(fps: usize) -> ([f32; 3], [f32; 3]) {
    let bodies = one_body([0.0, 8.0, 0.0]);
    let field = FieldValue::uniform([1.0, -4.0, 0.5]).unwrap();
    let mut simulation = RigidSimulation::default();
    simulation
        .advance_scene(
            &copies_scene(
                &bodies,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Some(field.clone()),
                &[],
            ),
            PlayedTime::ZERO,
            1.0,
            0.0,
        )
        .unwrap();
    for frame in 1..=fps {
        simulation
            .advance_scene(
                &copies_scene(
                    &bodies,
                    None,
                    0.0,
                    1.25,
                    16.0,
                    0.0,
                    [0.0; 3],
                    Some(field.clone()),
                    &[],
                ),
                PlayedTime::from_played(frame as f64 / fps as f64),
                1.0,
                0.0,
            )
            .unwrap();
    }
    let velocity = simulation
        .world
        .as_ref()
        .unwrap()
        .linear_velocity(simulation.handles[0].unwrap())
        .unwrap();
    (simulation.poses[0].pos, velocity)
}

#[test]
fn uniform_field_trace_is_render_partition_invariant() {
    let traces = [24, 30, 60].map(run_uniform_field_trace);
    for trace in traces.iter().skip(1) {
        for (actual, expected) in trace.0.iter().zip(traces[0].0) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
        for (actual, expected) in trace.1.iter().zip(traces[0].1) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
    }
}

#[test]
fn changed_field_applies_at_the_next_accepted_interval() {
    let bodies = one_body([0.0, 8.0, 0.0]);
    let old_field = FieldValue::uniform([0.0, -4.0, 0.0]).unwrap();
    let new_field = FieldValue::uniform([0.0, 8.0, 0.0]).unwrap();
    let mut simulation = RigidSimulation::default().with_step(live_step());
    simulation
        .advance_scene(
            &copies_scene(
                &bodies,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Some(old_field.clone()),
                &[],
            ),
            PlayedTime::ZERO,
            1.0,
            0.0,
        )
        .unwrap();
    simulation
        .advance_scene(
            &copies_scene(
                &bodies,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Some(old_field),
                &[],
            ),
            PlayedTime::from_played(0.5),
            1.0,
            0.0,
        )
        .unwrap();
    let pose_before_edit = simulation.poses[0];
    let time_before_edit = simulation.physics_time;
    simulation
        .advance_scene(
            &copies_scene(
                &bodies,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Some(new_field.clone()),
                &[],
            ),
            PlayedTime::from_played(0.5),
            1.0,
            0.0,
        )
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert_eq!(simulation.physics_time, time_before_edit);
    assert_eq!(simulation.poses[0], pose_before_edit);

    simulation
        .advance_scene(
            &copies_scene(
                &bodies,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Some(new_field),
                &[],
            ),
            PlayedTime::from_played(1.0),
            1.0,
            0.0,
        )
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert!((simulation.physics_time - 4.0 * FRAME).abs() < 1e-12);
    assert_ne!(simulation.poses[0], pose_before_edit);
}

fn target_slots(index: usize, field: FieldValue) -> Vec<Option<FieldValue>> {
    let mut fields = vec![None; 65];
    fields[index] = Some(field);
    fields
}

fn advance_targeted(
    simulation: &mut RigidSimulation,
    bodies: Vec<Option<RigidBody>>,
    prototype: Option<RigidBody>,
    now: f64,
    gravity: [f32; 3],
    targeted_fields: &[Option<FieldValue>],
) {
    simulation
        .advance_scene(
            &copies_scene(
                &bodies,
                prototype,
                1.0,
                1.25,
                16.0,
                0.0,
                gravity,
                None,
                targeted_fields,
            ),
            PlayedTime::from_played(now),
            1.0,
            0.0,
        )
        .unwrap();
}

#[test]
fn targeted_field_moves_selected_body_only() {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(body([0.0, 8.0, 0.0]));
    bodies[1] = Some(body([10.0, 8.0, 0.0]));
    let fields = target_slots(0, FieldValue::uniform([0.0, -4.0, 0.0]).unwrap());
    let mut simulation = RigidSimulation::default();
    advance_targeted(
        &mut simulation,
        bodies.clone(),
        None,
        0.0,
        [0.0; 3],
        &fields,
    );
    assert_eq!(
        simulation
            .targeted_fields
            .span(0, 0, 0.0)
            .unwrap()
            .field(0)
            .sample([0.0; 3]),
        [0.0, -4.0, 0.0],
        "the seeded target must align with the first authored sample",
    );
    advance_targeted(&mut simulation, bodies, None, 1.0, [0.0; 3], &fields);

    assert!(simulation.poses[0].pos[1] < 8.0);
    assert_eq!(simulation.poses[1].pos, [10.0, 8.0, 0.0]);
}

#[test]
fn targeted_field_adds_to_global_acceleration() {
    let bodies = one_body([0.0, 8.0, 0.0]);
    let fields = target_slots(0, FieldValue::uniform([0.0, -2.0, 0.0]).unwrap());
    let mut simulation = RigidSimulation::default();
    simulation
        .advance_scene(
            &copies_scene(
                &bodies,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0, -3.0, 0.0],
                Some(FieldValue::uniform([0.0, -1.0, 0.0]).unwrap()),
                &fields,
            ),
            PlayedTime::ZERO,
            1.0,
            0.0,
        )
        .unwrap();
    simulation
        .advance_scene(
            &copies_scene(
                &bodies,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0, -3.0, 0.0],
                Some(FieldValue::uniform([0.0, -1.0, 0.0]).unwrap()),
                &fields,
            ),
            PlayedTime::from_played(1.0),
            1.0,
            0.0,
        )
        .unwrap();
    let velocity = simulation
        .world
        .as_ref()
        .unwrap()
        .linear_velocity(simulation.handles[0].unwrap())
        .unwrap();
    assert!((velocity[1] + 6.0).abs() < 0.1);
}

#[test]
fn targeted_copy_field_maps_to_all_active_copies() {
    let prototype = body([0.0, 8.0, 0.0]);
    let fields = target_slots(64, FieldValue::uniform([0.0, -4.0, 0.0]).unwrap());
    let mut simulation = RigidSimulation::default();
    for time in [0.0, 1.0] {
        simulation
            .advance_scene(
                &copies_scene(
                    &vec![None; 64],
                    Some(prototype.clone()),
                    4.0,
                    2.0,
                    2.0,
                    0.0,
                    [0.0; 3],
                    None,
                    &fields,
                ),
                PlayedTime::from_played(time),
                1.0,
                0.0,
            )
            .unwrap();
    }
    assert_eq!(simulation.active_copies(0), 4);
    for pose in &simulation.group_poses(0)[..4] {
        assert!(pose.pos[1] < 7.0, "every copy must receive the field");
    }
}

#[test]
fn targeted_field_ignores_static_body() {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        kind: 0,
        transform: Transform {
            pos: [0.0, 8.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    let fields = target_slots(0, FieldValue::uniform([0.0, -4.0, 0.0]).unwrap());
    let mut simulation = RigidSimulation::default();
    advance_targeted(
        &mut simulation,
        bodies.clone(),
        None,
        0.0,
        [0.0; 3],
        &fields,
    );
    advance_targeted(&mut simulation, bodies, None, 1.0, [0.0; 3], &fields);
    assert_eq!(simulation.poses[0].pos, [0.0, 8.0, 0.0]);
}

#[test]
fn all_none_target_input_stays_unallocated_and_connected_storage_is_reused() {
    let bodies = one_body([0.0, 8.0, 0.0]);
    let empty: Vec<Option<FieldValue>> = vec![None; 65];
    let mut simulation = RigidSimulation::default();
    advance_targeted(&mut simulation, bodies.clone(), None, 0.0, [0.0; 3], &empty);
    assert!(!simulation.targeted_fields.is_connected());
    assert_eq!(simulation.targeted_fields.columns(), 0);

    let fields = target_slots(0, FieldValue::uniform([0.0, -4.0, 0.0]).unwrap());
    advance_targeted(
        &mut simulation,
        bodies.clone(),
        None,
        0.0,
        [0.0; 3],
        &fields,
    );
    let capacity = simulation.targeted_fields.columns();
    let storage_ptr = simulation.targeted_fields.storage_ptr();
    advance_targeted(
        &mut simulation,
        bodies.clone(),
        None,
        0.5,
        [0.0; 3],
        &fields,
    );
    simulation
        .advance_scene(
            &copies_scene(&bodies, None, 0.0, 1.25, 16.0, 0.0, [0.0; 3], None, &fields),
            PlayedTime::from_played(0.5),
            1.0,
            1.0,
        )
        .unwrap();
    assert_eq!(simulation.targeted_fields.columns(), capacity);
    assert_eq!(simulation.targeted_fields.storage_ptr(), storage_ptr);
}

fn run_targeted_trace(fps: usize) -> ([f32; 3], [f32; 3]) {
    let bodies = one_body([0.0, 8.0, 0.0]);
    let fields = target_slots(0, FieldValue::uniform([1.0, -4.0, 0.5]).unwrap());
    let mut simulation = RigidSimulation::default();
    advance_targeted(
        &mut simulation,
        bodies.clone(),
        None,
        0.0,
        [0.0; 3],
        &fields,
    );
    for frame in 1..=fps {
        advance_targeted(
            &mut simulation,
            bodies.clone(),
            None,
            frame as f64 / fps as f64,
            [0.0; 3],
            &fields,
        );
    }
    let velocity = simulation
        .world
        .as_ref()
        .unwrap()
        .linear_velocity(simulation.handles[0].unwrap())
        .unwrap();
    (simulation.poses[0].pos, velocity)
}

#[test]
fn targeted_field_trace_is_render_partition_invariant() {
    let traces = [24, 30, 60].map(run_targeted_trace);
    for trace in traces.iter().skip(1) {
        for (actual, expected) in trace.0.iter().zip(traces[0].0) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
        for (actual, expected) in trace.1.iter().zip(traces[0].1) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
    }
}

#[test]
fn targeted_fields_preserve_first_connection_and_paused_edits() {
    let bodies = one_body([0.0, 8.0, 0.0]);
    let empty: Vec<Option<FieldValue>> = vec![None; 65];
    let old = target_slots(0, FieldValue::uniform([2.0, 0.0, 0.0]).unwrap());
    let new = target_slots(0, FieldValue::uniform([-4.0, 0.0, 0.0]).unwrap());

    // Compare the same accepted history without discarded wall time.
    // The old field already advanced two live intervals before the edit;
    // a paused edit must not replace that completed motion retroactively.
    let mut expected = RigidSimulation::default().with_step(SimStep::export(FIXED_TICK));
    {
        advance_targeted(&mut expected, bodies.clone(), None, 0.0, [0.0; 3], &empty);
        advance_targeted(
            &mut expected,
            bodies.clone(),
            None,
            2.0 * FRAME,
            [0.0; 3],
            &old,
        );
        advance_targeted(
            &mut expected,
            bodies.clone(),
            None,
            2.0 * FRAME,
            [0.0; 3],
            &new,
        );
        advance_targeted(
            &mut expected,
            bodies.clone(),
            None,
            4.0 * FRAME,
            [0.0; 3],
            &new,
        );
    }

    let mut edited = RigidSimulation::default().with_step(live_step());
    advance_targeted(&mut edited, bodies.clone(), None, 0.0, [0.0; 3], &empty);
    advance_targeted(&mut edited, bodies.clone(), None, 0.5, [0.0; 3], &old);
    let identity = edited.handles[0];
    let time_before_edit = edited.physics_time;
    let pose_before_edit = edited.poses.clone();
    let velocity = |simulation: &RigidSimulation| {
        simulation
            .world
            .as_ref()
            .unwrap()
            .linear_velocity(simulation.handles[0].unwrap())
            .unwrap()
    };
    let velocity_before_edit = velocity(&edited);
    advance_targeted(&mut edited, bodies.clone(), None, 0.5, [0.0; 3], &new);
    assert_eq!(edited.pending_time, Seconds::ZERO);
    assert_eq!(edited.physics_time, time_before_edit);
    assert!((edited.physics_time - 2.0 * FRAME).abs() < 1e-12);
    assert_eq!(edited.poses, pose_before_edit);
    assert_eq!(velocity(&edited), velocity_before_edit);
    assert_eq!(edited.handles[0], identity);
    advance_targeted(&mut edited, bodies, None, 0.75, [0.0; 3], &new);

    assert_eq!(edited.pending_time, Seconds::ZERO);
    assert!((edited.physics_time - 4.0 * FRAME).abs() < 1e-12);
    assert_eq!(
        edited.poses, expected.poses,
        "paused edit changed the accepted endpoint"
    );
    assert_eq!(velocity(&edited), velocity(&expected));
}

fn varying_gravity(sample: usize) -> [f32; 3] {
    let phase = sample as f32 * 0.09;
    [
        phase.sin() * 2.0,
        -9.8 + phase.cos() * 3.0,
        phase.sin() * -0.75,
    ]
}

fn run_varying_gravity_trace(fps: usize) -> ([f32; 3], [f32; 3]) {
    assert_eq!(240 % fps, 0);
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, varying_gravity(0), PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    let samples_per_frame = 240 / fps;
    for frame in 1..=fps {
        let end_sample = frame * samples_per_frame;
        {
            let frame_step = simulation.step();
            simulation.set_step(frame_step.authored_sample());
            for sample in ((frame - 1) * samples_per_frame + 1)..=end_sample {
                simulation
                    .advance(
                        &bodies,
                        varying_gravity(sample),
                        PlayedTime::from_played(sample as f64 / 240.0),
                        1.0,
                        0.0,
                    )
                    .unwrap();
            }
            simulation.set_step(frame_step);
        }
        simulation
            .advance(
                &bodies,
                varying_gravity(end_sample),
                PlayedTime::from_played(end_sample as f64 / 240.0),
                1.0,
                0.0,
            )
            .unwrap();
    }
    let velocity = simulation
        .world
        .as_ref()
        .expect("trace builds a native world")
        .linear_velocity(simulation.handles[0].expect("trace body has a handle"))
        .unwrap();
    (simulation.poses[0].pos, velocity)
}

#[test]
fn retained_gravity_trace_is_render_partition_invariant() {
    let traces = [24, 30, 60].map(run_varying_gravity_trace);
    for trace in traces.iter().skip(1) {
        for (actual, expected) in trace.0.iter().zip(traces[0].0) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
        for (actual, expected) in trace.1.iter().zip(traces[0].1) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
    }
}

#[test]
fn changed_gravity_applies_at_the_next_accepted_interval() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let old_gravity = [0.0, -9.8, 0.0];
    let new_gravity = [0.0, 8.0, 0.0];
    let mut simulation = RigidSimulation::default().with_step(live_step());
    simulation
        .advance(&bodies, old_gravity, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    simulation
        .advance(&bodies, old_gravity, PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    let pose_before_edit = simulation.poses[0];
    simulation
        .advance(&bodies, new_gravity, PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert_eq!(simulation.poses[0], pose_before_edit);
    simulation
        .advance(&bodies, new_gravity, PlayedTime::from_played(1.0), 1.0, 0.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert!((simulation.physics_time - 4.0 * FRAME).abs() < 1e-12);
    assert_ne!(simulation.poses[0], pose_before_edit);
}

#[test]
fn exhausted_authored_history_preserves_state_until_reset() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let gravity = [0.0, -9.8, 0.0];
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, gravity, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    simulation.authored_samples = InputHistory::with_capacity(2).unwrap();
    simulation.authored_bodies.clear();
    simulation
        .record_authored_sample(
            0.0,
            &SceneRef {
                bodies: &bodies,
                groups: &[],
                gravity,
                acceleration_field: None,
                targeted_fields: &[],
            },
            None,
        )
        .unwrap();
    simulation
        .record_authored_sample(
            FRAME,
            &SceneRef {
                bodies: &bodies,
                groups: &[],
                gravity,
                acceleration_field: None,
                targeted_fields: &[],
            },
            None,
        )
        .unwrap();
    let accepted_len = simulation.authored_samples.len();
    let accepted_pose = simulation.poses.clone();
    let accepted_time = simulation.authored_time;
    let accepted_debt = simulation.pending_time;

    let error = simulation
        .advance(
            &bodies,
            gravity,
            PlayedTime::from_played(2.0 * FRAME),
            1.0,
            0.0,
        )
        .unwrap_err();
    assert!(error.contains("exhausted") || error.contains("full"));
    assert!(simulation.authored_samples.is_exhausted());
    assert_eq!(simulation.authored_samples.len(), accepted_len);
    assert_eq!(simulation.poses, accepted_pose);
    assert_eq!(simulation.authored_time, accepted_time);
    assert_eq!(simulation.pending_time, accepted_debt);

    let retry = simulation
        .record_authored_sample(
            FRAME,
            &SceneRef {
                bodies: &bodies,
                groups: &[],
                gravity,
                acceleration_field: None,
                targeted_fields: &[],
            },
            None,
        )
        .unwrap_err();
    assert!(retry.contains("exhausted"));
    assert_eq!(simulation.authored_samples.len(), accepted_len);

    simulation
        .advance(
            &bodies,
            gravity,
            PlayedTime::from_played(2.0 * FRAME),
            1.0,
            1.0,
        )
        .unwrap();
    assert!(!simulation.authored_samples.is_exhausted());
    assert_eq!(simulation.authored_samples.len(), 1);
}

#[test]
fn zero_speed_finishes_preceding_interval_and_holds_until_resume() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    let authored = simulation.poses[0].pos;
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(0.5), 0.0, 0.0)
        .unwrap();
    let held = simulation.poses[0].pos;
    assert!(held[1] < authored[1] - 0.1);
    assert_eq!(simulation.physics_time, 0.5);
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(1.0), 1.0, 0.0)
        .unwrap();
    assert_eq!(simulation.poses[0].pos, held);
    assert_eq!(simulation.physics_time, 0.5);
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(1.5), 1.0, 0.0)
        .unwrap();
    assert!(simulation.poses[0].pos[1] < held[1] - 0.1);
    assert_eq!(simulation.physics_time, 1.0);
}

/// Fundamental 4 (SCENE_DOCUMENT_DESIGN.md section 10 (Testing)), with the
/// join rule: a body joins the running world when it is enabled or its shape
/// is ready, and a NaN scale or a flat mesh is refused with a reason while
/// everything else keeps stepping. Nothing reaches Box3D's checks.
#[test]
fn a_body_joins_the_running_world_when_ready_and_bad_input_stays_out() {
    let asserts_before = manifold_physics::native_assert_count();
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        enabled: false,
        kind: 0,
        transform: Transform {
            pos: [0.0, 0.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    bodies[1] = Some(body([0.0, 1.5, 0.0]));
    let mut nan_scale = body([3.0, 1.5, 0.0]);
    nan_scale.transform.scale = [f32::NAN, 1.0, 1.0];
    bodies[2] = Some(nan_scale);
    let flat = [
        [-1.0, 0.0, -1.0],
        [1.0, 0.0, -1.0],
        [1.0, 0.0, 1.0],
        [-1.0, 0.0, 1.0],
    ];
    bodies[3] = Some(RigidBody {
        collider: Some(Arc::new(Solid::Hulls(vec![flat.to_vec()]))),
        ..body([6.0, 1.5, 0.0])
    });
    // Its shape is still being built.
    bodies[4] = Some(RigidBody {
        collider: None,
        ..body([-3.0, 1.5, 0.0])
    });
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(1.0), 1.0, 0.0)
        .unwrap();

    assert_eq!(simulation.poses[0].pos, [0.0, 0.0, 0.0]);
    assert!(simulation.poses[1].pos[1] < 0.0, "the world keeps stepping");
    assert_eq!(
        simulation.handles[2..5],
        [None; 3],
        "refused and unready bodies stay out of the solver"
    );
    let refused: Vec<Member> = simulation
        .refusals()
        .iter()
        .map(|refusal| refusal.member)
        .collect();
    assert_eq!(
        refused,
        [Member::Body(2), Member::Body(3)],
        "a refusal names each bad body; an unready one is not refused"
    );
    assert!(
        simulation
            .refusals()
            .iter()
            .all(|refusal| !refusal.reason.to_string().is_empty())
    );
    assert!(
        simulation
            .refusals()
            .iter()
            .all(|refusal| refusal.reason.param() == Some("scale_x")),
        "a bad scale and a shape its scale flattens both warn on Scale"
    );
    let stand =
        |slot: usize| simulation.poses[slot].pos == bodies[slot].as_ref().unwrap().transform.pos;
    assert!(
        stand(2) && stand(3) && stand(4),
        "refused and unready bodies draw where they stand, never at the origin"
    );

    bodies[0].as_mut().unwrap().enabled = true;
    bodies[4].as_mut().unwrap().collider = Some(default_solid());
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(2.0), 1.0, 0.0)
        .unwrap();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(3.0), 1.0, 0.0)
        .unwrap();
    assert!(simulation.poses[0].pos[1].abs() < 1.0e-5);
    assert!(
        simulation.poses[1].pos[1] < 0.0,
        "the crate that already fell is not reset by the new ground"
    );
    assert!(
        simulation.handles[4].is_some() && simulation.poses[4].pos[1] < 1.5,
        "a body whose shape arrives joins and falls"
    );
    assert_eq!(
        manifold_physics::native_assert_count(),
        asserts_before,
        "no Box3D check failed"
    );
}

#[test]
fn prepared_fragments_follow_parent_then_inherit_motion_on_release() {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        transform: Transform {
            pos: [0.0, 4.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    bodies[1] = Some(RigidBody {
        transform: Transform {
            pos: [0.0, 4.0, 0.0],
            scale: [0.7, 0.7, 0.7],
            ..Transform::default()
        },
        fragment_parent: Some(0),
        ..RigidBody::default()
    });
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    assert!(!simulation.fragment_parent_released[0]);
    assert!(!simulation.fragment_active[1]);
    assert!((simulation.poses[1].pos[1] - simulation.poses[0].pos[1]).abs() < 1.0e-5);
    let parent_before_release = simulation.poses[0].pos;

    bodies[0].as_mut().unwrap().release_count = 1.0;
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(1.0), 1.0, 0.0)
        .unwrap();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(1.5), 1.0, 0.0)
        .unwrap();
    assert!(simulation.fragment_parent_released[0]);
    assert!(simulation.fragment_active[1]);
    assert!((simulation.poses[1].pos[1] - parent_before_release[1]).abs() > 0.02);
    assert!(simulation.poses[1].pos[1] < parent_before_release[1]);
}

#[test]
fn disabled_parent_does_not_release_prepared_fragments() {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        enabled: false,
        release_count: 1.0,
        transform: Transform {
            pos: [0.0, 4.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    bodies[1] = Some(RigidBody {
        fragment_parent: Some(0),
        transform: Transform {
            pos: [0.0, 4.0, 0.0],
            scale: [0.7, 0.7, 0.7],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(1.0), 1.0, 0.0)
        .unwrap();
    assert!(!simulation.fragment_parent_released[0]);
    assert!(!simulation.fragment_active[1]);
    assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
    assert_eq!(simulation.poses[1].pos, [0.0, 4.0, 0.0]);
}

#[test]
fn fragment_release_reset_restores_intact_state_and_allows_retrigger() {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        transform: Transform {
            pos: [0.0, 4.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    bodies[1] = Some(RigidBody {
        fragment_parent: Some(0),
        transform: Transform {
            pos: [0.0, 4.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    bodies[0].as_mut().unwrap().release_count = 1.0;
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    assert!(simulation.fragment_parent_released[0]);

    bodies[0].as_mut().unwrap().release_count = 0.0;
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(0.5), 1.0, 1.0)
        .unwrap();
    assert!(!simulation.fragment_parent_released[0]);
    assert!(!simulation.fragment_active[1]);
    assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
    assert_eq!(simulation.poses[1].pos, [0.0, 4.0, 0.0]);

    bodies[0].as_mut().unwrap().release_count = 1.0;
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(0.5), 1.0, 1.0)
        .unwrap();
    assert!(simulation.fragment_parent_released[0]);
    assert!(simulation.fragment_active[1]);
}

#[test]
fn fragment_release_inherits_spinning_parent_pose_and_point_velocity() {
    let child_geometry = Arc::new(Solid::Hulls(vec![vec![
        [0.35, -0.2, -0.2],
        [0.85, -0.2, -0.2],
        [0.35, 0.2, -0.2],
        [0.35, -0.2, 0.2],
    ]]));
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        transform: Transform {
            pos: [0.0, 4.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    bodies[1] = Some(RigidBody {
        fragment_parent: Some(0),
        collider: Some(child_geometry),
        transform: Transform {
            pos: [0.0, 4.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    bodies[2] = Some(body([5.0, 4.0, 0.0]));

    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    let parent_handle = simulation.handles[0].unwrap();
    let child_handle = simulation.handles[1].unwrap();
    let unrelated_handle = simulation.handles[2].unwrap();
    let parent_linear = [1.0, 0.5, 0.0];
    let parent_angular = [0.0, 2.0, 0.0];
    simulation
        .world
        .as_mut()
        .unwrap()
        .set_velocity(parent_handle, parent_linear, parent_angular)
        .unwrap();
    let (
        parent_pose,
        expected_angular,
        expected_child_velocity,
        unrelated_pose,
        unrelated_velocity,
    ) = {
        let world = simulation.world.as_ref().unwrap();
        let child_center = world.local_center_of_mass(child_handle).unwrap();
        (
            world.pose(parent_handle).unwrap(),
            world.angular_velocity(parent_handle).unwrap(),
            world
                .velocity_at_local_point(parent_handle, child_center)
                .unwrap(),
            world.pose(unrelated_handle).unwrap(),
            world.linear_velocity(unrelated_handle).unwrap(),
        )
    };
    assert!(expected_child_velocity[2].abs() > 0.5);

    bodies[0].as_mut().unwrap().release_count = 1.0;
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 0.0, 0.0)
        .unwrap();

    let world = simulation.world.as_ref().unwrap();
    let child_pose = world.pose(child_handle).unwrap();
    let child_velocity = world.linear_velocity(child_handle).unwrap();
    let child_angular = world.angular_velocity(child_handle).unwrap();
    let after_unrelated_pose = world.pose(unrelated_handle).unwrap();
    let after_unrelated_velocity = world.linear_velocity(unrelated_handle).unwrap();
    for (actual, expected) in child_pose.position.iter().zip(parent_pose.position) {
        assert!((actual - expected).abs() < 1.0e-5);
    }
    for (actual, expected) in child_pose.rotation.iter().zip(parent_pose.rotation) {
        assert!((actual - expected).abs() < 1.0e-5);
    }
    for (actual, expected) in child_velocity.iter().zip(expected_child_velocity) {
        assert!((actual - expected).abs() < 1.0e-5);
    }
    for (actual, expected) in child_angular.iter().zip(expected_angular) {
        assert!((actual - expected).abs() < 1.0e-5);
    }
    for (actual, expected) in after_unrelated_pose
        .position
        .iter()
        .zip(unrelated_pose.position)
    {
        assert!((actual - expected).abs() < 1.0e-5);
    }
    for (actual, expected) in after_unrelated_velocity.iter().zip(unrelated_velocity) {
        assert!((actual - expected).abs() < 1.0e-5);
    }
}

#[test]
fn live_fragment_release_occurs_at_the_authored_boundary() {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(body([0.0, 4.0, 0.0]));
    bodies[1] = Some(RigidBody {
        fragment_parent: Some(0),
        transform: Transform {
            pos: [0.0, 4.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    let mut released = bodies.clone();
    released[0].as_mut().unwrap().release_count = 1.0;
    let mut simulation = RigidSimulation::default().with_step(live_step());
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    let boundary = 0.5;
    simulation
        .advance(
            &bodies,
            GRAVITY,
            PlayedTime::from_played(boundary - FRAME),
            1.0,
            0.0,
        )
        .unwrap();
    assert!(!simulation.fragment_parent_released[0]);
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    simulation
        .advance(
            &released,
            GRAVITY,
            PlayedTime::from_played(boundary),
            1.0,
            0.0,
        )
        .unwrap();
    assert!(simulation.fragment_parent_released[0]);
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    // The first late observation accepted two intervals; the authored
    // release boundary closes the third, without replaying discarded time.
    assert!((simulation.physics_time - 3.0 * FRAME).abs() < 1e-12);
    simulation
        .advance(
            &released,
            GRAVITY,
            PlayedTime::from_played(boundary + FRAME),
            1.0,
            0.0,
        )
        .unwrap();
    assert!(simulation.fragment_parent_released[0]);
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert!((simulation.physics_time - 4.0 * FRAME).abs() < 1e-12);
}

#[test]
fn animated_body_pushes_a_dynamic_body_instead_of_teleporting_through_it() {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        kind: 2,
        transform: Transform {
            pos: [-1.3, 0.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    bodies[1] = Some(body([0.4, 0.0, 0.0]));
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();

    for frame in 1..=30 {
        bodies[0].as_mut().unwrap().transform.pos[0] = -1.3 + frame as f32 * 0.05;
        simulation
            .advance(
                &bodies,
                [0.0; 3],
                PlayedTime::from_played(frame as f64 * FRAME),
                1.0,
                0.0,
            )
            .unwrap();
    }

    assert!(
        simulation.poses[1].pos[0] > 0.6,
        "moving collider failed to push body: {:?}",
        simulation.poses[1].pos
    );
}

#[test]
fn rotating_animated_body_contacts_a_dynamic_body() {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        kind: 2,
        transform: Transform {
            scale: [3.0, 0.3, 0.3],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    let start = [1.0, 0.0, -1.0];
    bodies[1] = Some(body(start));
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();

    for frame in 1..=60 {
        bodies[0].as_mut().unwrap().transform.rot_euler[1] =
            frame as f32 * std::f32::consts::FRAC_PI_2 / 60.0;
        simulation
            .advance(
                &bodies,
                [0.0; 3],
                PlayedTime::from_played(frame as f64 * FRAME),
                1.0,
                0.0,
            )
            .unwrap();
    }

    let result = simulation.poses[1].pos;
    assert!(
        (result[0] - start[0]).abs() + (result[2] - start[2]).abs() > 0.1,
        "rotating collider missed body: {result:?}"
    );
}

#[test]
fn reset_or_backward_time_restores_authored_pose() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(0.75), 1.0, 0.0)
        .unwrap();
    assert!(simulation.poses[0].pos[1] < 4.0);

    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(0.75), 1.0, 1.0)
        .unwrap();
    assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(0.5), 1.0, 1.0)
        .unwrap();
    assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
}

#[test]
fn material_edit_preserves_the_current_falling_pose() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    let falling_pose = simulation.poses[0].pos;

    let mut edited = bodies;
    let mut edited_body = edited[0].clone().unwrap();
    edited_body.friction = 0.9;
    edited_body.mass *= 2.0;
    edited_body.bounce = 0.4;
    edited[0] = Some(edited_body);
    simulation
        .advance(&edited, GRAVITY, PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    assert!((simulation.poses[0].pos[1] - falling_pose[1]).abs() < 1.0e-5);

    simulation
        .advance(&edited, GRAVITY, PlayedTime::from_played(0.75), 1.0, 0.0)
        .unwrap();
    assert!(simulation.poses[0].pos[1] < falling_pose[1] - 0.05);
}

#[test]
fn multi_hull_geometry_rebuilds_on_reset_and_material_edits_keep_pose() {
    let geometry = Arc::new(Solid::Hulls(vec![
        vec![
            [-0.5, -0.5, -0.5],
            [0.5, -0.5, -0.5],
            [0.0, 0.5, -0.5],
            [0.0, 0.0, 0.5],
        ],
        vec![
            [-0.5, -0.5, 0.5],
            [0.5, -0.5, 0.5],
            [0.0, 0.5, 0.5],
            [0.0, 0.0, -0.5],
        ],
    ]));
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        collider: Some(geometry.clone()),
        transform: Transform {
            pos: [0.0, 4.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    let falling_pose = simulation.poses[0].pos;

    let mut edited = bodies.clone();
    let mut edited_body = edited[0].take().expect("body exists");
    edited_body.friction = 0.9;
    edited_body.mass *= 2.0;
    edited_body.collider = Some(geometry.clone());
    edited[0] = Some(edited_body);
    simulation
        .advance(&edited, GRAVITY, PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    assert!((simulation.poses[0].pos[1] - falling_pose[1]).abs() < 1.0e-5);

    simulation
        .advance(&edited, GRAVITY, PlayedTime::from_played(0.5), 1.0, 1.0)
        .unwrap();
    assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
}

#[test]
fn all_scaled_platonic_hulls_settle_on_the_ground() {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        transform: Transform {
            pos: [0.0, -1.0, 0.0],
            scale: [20.0, 1.0, 20.0],
            ..Transform::default()
        },
        kind: 0,
        mass: 0.0,
        ..RigidBody::default()
    });
    for (index, x) in [-8.0, -4.0, 0.0, 4.0, 8.0].into_iter().enumerate() {
        bodies[index + 1] = Some(RigidBody {
            transform: Transform {
                pos: [x, 4.0, 0.0],
                scale: [0.7 + index as f32 * 0.15, 0.8 + index as f32 * 0.1, 0.7],
                ..Transform::default()
            },
            collider: Some(platonic_solid(index as u32)),
            ..RigidBody::default()
        });
    }

    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    // A dodecahedron can still roll between faces at four seconds.
    // Give every shape time to sleep before measuring stable contact.
    for frame in 1..=720 {
        simulation
            .advance(
                &bodies,
                GRAVITY,
                PlayedTime::from_played(frame as f64 * FRAME),
                1.0,
                0.0,
            )
            .unwrap();
    }
    let settled = simulation.poses.clone();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(12.5), 1.0, 0.0)
        .unwrap();
    for (index, before) in settled.iter().enumerate().take(6).skip(1) {
        assert!(
            (simulation.poses[index].pos[1] - before.pos[1]).abs() < 0.03,
            "shape={} before={:?} after={:?}",
            index - 1,
            before,
            simulation.poses[index]
        );
        assert!(simulation.poses[index].pos[1] > -1.0);
    }
}

#[test]
fn long_catch_up_matches_regular_ticks_and_keeps_running() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut caught_up = RigidSimulation::default();
    let mut regular = RigidSimulation::default();
    caught_up
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    for frame in 0..=181 {
        regular
            .advance(
                &bodies,
                GRAVITY,
                PlayedTime::from_played(frame as f64 * FRAME),
                1.0,
                0.0,
            )
            .unwrap();
    }
    caught_up
        .advance(
            &bodies,
            GRAVITY,
            PlayedTime::from_played(180.0 * FRAME),
            1.0,
            0.0,
        )
        .unwrap();
    caught_up
        .advance(
            &bodies,
            GRAVITY,
            PlayedTime::from_played(181.0 * FRAME),
            1.0,
            0.0,
        )
        .unwrap();
    assert_eq!(caught_up.poses, regular.poses);
    caught_up
        .advance(
            &bodies,
            GRAVITY,
            PlayedTime::from_played(181.0 * FRAME),
            1.0,
            1.0,
        )
        .unwrap();
    assert_eq!(caught_up.poses[0].pos, [0.0, 4.0, 0.0]);
}

#[test]
fn live_accepts_two_fixed_intervals_then_reanchors_without_backlog() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default().with_step(live_step());
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    // Discard the overload, including its fractional remainder.
    let now = Seconds(3.0 + FRAME / 2.0);
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played((now).0), 1.0, 0.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert!((simulation.physics_time - 2.0 * FRAME).abs() < 1e-12);
    let accepted_pose = simulation.poses.clone();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played((now).0), 0.0, 0.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert!((simulation.physics_time - 2.0 * FRAME).abs() < 1e-12);
    assert_eq!(simulation.poses, accepted_pose);
    // The reanchored interval ran at Speed 0 and adds nothing. Resume
    // takes effect from this new observation onward.
    let next = Seconds(now.0 + FRAME);
    simulation
        .advance(
            &bodies,
            GRAVITY,
            PlayedTime::from_played((next).0),
            1.0,
            0.0,
        )
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert!((simulation.physics_time - 2.0 * FRAME).abs() < 1e-12);
    simulation
        .advance(
            &bodies,
            GRAVITY,
            PlayedTime::from_played(next.0 + FRAME),
            1.0,
            0.0,
        )
        .unwrap();
    assert!((simulation.physics_time - 3.0 * FRAME).abs() < 1e-12);
}

#[test]
fn animated_pose_timeline_covers_each_live_endpoint() {
    let mut moving = vec![None; 64];
    moving[0] = Some(RigidBody {
        kind: 2,
        transform: Transform {
            pos: [-1.3, 0.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    moving[1] = Some(body([0.4, 0.0, 0.0]));
    let mut simulation = RigidSimulation::default().with_step(live_step());
    simulation
        .advance(&moving, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    moving[0].as_mut().unwrap().transform.pos[0] = -0.55;
    simulation
        .advance(&moving, [0.0; 3], PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert!((simulation.physics_time - 2.0 * FRAME).abs() < 1e-12);
    moving[0].as_mut().unwrap().transform.pos[0] = 0.2;
    simulation
        .advance(&moving, [0.0; 3], PlayedTime::from_played(1.0), 1.0, 0.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert!((simulation.physics_time - 4.0 * FRAME).abs() < 1e-12);

    let mut rotating = vec![None; 64];
    rotating[0] = Some(RigidBody {
        kind: 2,
        transform: Transform {
            scale: [3.0, 0.3, 0.3],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    rotating[1] = Some(body([1.0, 0.0, -1.0]));
    let mut rotation_simulation = RigidSimulation::default().with_step(live_step());
    rotation_simulation
        .advance(&rotating, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    rotating[0].as_mut().unwrap().transform.rot_euler[1] = std::f32::consts::FRAC_PI_4;
    rotation_simulation
        .advance(&rotating, [0.0; 3], PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    rotating[0].as_mut().unwrap().transform.rot_euler[1] = std::f32::consts::FRAC_PI_2;
    rotation_simulation
        .advance(&rotating, [0.0; 3], PlayedTime::from_played(1.0), 1.0, 0.0)
        .unwrap();
    assert_eq!(rotation_simulation.pending_time, Seconds::ZERO);
    assert!((rotation_simulation.physics_time - 4.0 * FRAME).abs() < 1e-12);
}

#[test]
fn paused_authored_edit_is_visible_without_pending_backlog() {
    let mut bodies = vec![None; 64];
    let mut animated = body([-2.0, 0.0, 0.0]);
    animated.kind = 2;
    bodies[0] = Some(animated);
    let mut simulation = RigidSimulation::default().with_step(live_step());
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    bodies[0].as_mut().unwrap().transform.pos[0] = -0.5;
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    bodies[0].as_mut().unwrap().transform.pos[0] = 2.0;
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(0.5), 0.0, 0.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert!((simulation.poses[0].pos[0] - 2.0).abs() < 1e-5);
    assert!((simulation.physics_time - 2.0 * FRAME).abs() < 1e-12);
}

#[test]
fn paused_animated_pose_edit_and_undo_move_only_the_authored_body() {
    let mut bodies = vec![None; 64];
    let mut animated = body([0.0, 0.0, 0.0]);
    animated.kind = 2;
    bodies[0] = Some(animated.clone());
    bodies[1] = Some(body([10.0, 0.0, 0.0]));
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    let dynamic_before = simulation.poses[1];

    bodies[0].as_mut().unwrap().transform.pos[0] = 5.0;
    bodies[0].as_mut().unwrap().transform.rot_euler[2] = 0.4;
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    assert!((simulation.poses[0].pos[0] - 5.0).abs() < 1.0e-5);
    assert!((simulation.poses[0].rot_euler[2] - 0.4).abs() < 1.0e-5);
    assert_eq!(simulation.poses[1], dynamic_before);
    assert_eq!(simulation.pending_time, Seconds::ZERO);

    bodies[0] = Some(animated);
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    assert!(simulation.poses[0].pos[0].abs() < 1.0e-5);
    assert!(simulation.poses[0].rot_euler[2].abs() < 1.0e-5);
    assert_eq!(simulation.poses[1], dynamic_before);
}

#[test]
fn paused_animated_copy_edit_and_undo_update_every_copy_without_ticks() {
    let mut prototype = body([0.0, 0.0, 0.0]);
    prototype.kind = 2;
    let mut simulation = RigidSimulation::default();
    let mut advance = |prototype: RigidBody| {
        simulation
            .advance_scene(
                &copies_scene(
                    &vec![None; 64],
                    Some(prototype.clone()),
                    2.0,
                    1.25,
                    2.0,
                    0.0,
                    [0.0; 3],
                    None,
                    &[],
                ),
                PlayedTime::ZERO,
                1.0,
                0.0,
            )
            .unwrap();
        simulation.group_poses(0)[..2].to_vec()
    };
    let before = advance(prototype.clone());
    prototype.transform.pos[0] = 5.0;
    prototype.transform.rot_euler[1] = 0.25;
    let edited = advance(prototype.clone());
    // The group moves and turns as one about its first copy, which sits on its origin: that copy moves 5 m, every copy turns.
    assert!((edited[0].pos[0] - before[0].pos[0] - 5.0).abs() < 1.0e-5);
    for (old, new) in before.iter().zip(&edited) {
        assert!((new.rot_euler[1] - old.rot_euler[1] - 0.25).abs() < 1.0e-5);
    }
    let span = |poses: &[Transform]| {
        [
            poses[1].pos[0] - poses[0].pos[0],
            poses[1].pos[2] - poses[0].pos[2],
        ]
    };
    let [dx, dz] = span(&edited);
    let [bx, _] = span(&before);
    assert!(
        (dx - bx * 0.25f32.cos()).abs() < 1.0e-4 && (dz + bx * 0.25f32.sin()).abs() < 1.0e-4,
        "the row turns with the group"
    );
    prototype.transform.pos[0] = 0.0;
    prototype.transform.rot_euler[1] = 0.0;
    let undone = advance(prototype.clone());
    assert_eq!(undone, before);
}

#[test]
fn paused_edit_stays_visible_without_sweeping_a_dynamic_body() {
    let mut bodies = vec![None; 64];
    let mut animated = body([-2.0, 0.0, 0.0]);
    animated.kind = 2;
    bodies[0] = Some(animated);
    bodies[1] = Some(body([2.0, 0.0, 0.0]));
    let mut simulation = RigidSimulation::default().with_step(live_step());
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    bodies[0].as_mut().unwrap().transform.pos[0] = -1.0;
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    bodies[0].as_mut().unwrap().transform.pos[0] = 5.0;
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(0.5), 0.0, 0.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert!((simulation.poses[0].pos[0] - 5.0).abs() < 1e-5);
    assert!((simulation.poses[1].pos[0] - 2.0).abs() < 1e-3);
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(1.0), 1.0, 0.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
}

#[test]
fn fast_animated_sweep_uses_outer_steps_to_reach_dynamic_body() {
    let mut bodies = vec![None; 64];
    let mut animated = body([-2.0, 0.0, 0.0]);
    animated.kind = 2;
    bodies[0] = Some(animated);
    bodies[1] = Some(body([0.0, 0.0, 0.0]));
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    bodies[0].as_mut().unwrap().transform.pos[0] = 2.0;
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(FRAME), 1.0, 0.0)
        .unwrap();
    assert!(
        simulation.poses[1]
            .pos
            .iter()
            .any(|value| value.abs() > 0.01),
        "fast animated body passed through the dynamic body"
    );
}

#[test]
fn nonlinear_animated_round_trip_is_sampled_inside_one_render_frame() {
    let mut bodies = vec![None; 64];
    let mut animated = body([-2.0, 0.0, 0.0]);
    animated.kind = 2;
    bodies[0] = Some(animated);
    bodies[1] = Some(body([0.0, 0.0, 0.0]));
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();

    // The authored curve returns to its starting position by the next
    // rendered frame. Endpoint interpolation alone sees zero movement.
    for quarter in 1..4 {
        let t = FRAME * f64::from(quarter) / 4.0;
        bodies[0].as_mut().unwrap().transform.pos[0] =
            -2.0 * (std::f32::consts::TAU * quarter as f32 / 4.0).cos();
        simulation.set_step(SimStep::default().authored_sample());
        simulation
            .advance(&bodies, [0.0; 3], PlayedTime::from_played(t), 1.0, 0.0)
            .unwrap();
    }
    simulation.set_step(SimStep::default());
    bodies[0].as_mut().unwrap().transform.pos[0] = -2.0;
    assert!(simulation.animated_microsteps(&bodies, &[], FRAME).0 > 1);
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(FRAME), 1.0, 0.0)
        .unwrap();
    assert!(
        simulation.poses[1]
            .pos
            .iter()
            .any(|value| value.abs() > 0.01),
        "nonlinear Animated collider missed the Dynamic body: {:?}",
        simulation.poses[1].pos
    );
}

#[test]
fn nonlinear_contacts_accept_regular_and_irregular_live_spans() {
    fn trajectory(tick: usize) -> Vec<Option<RigidBody>> {
        let mut bodies = vec![None; 64];
        let mut animated = body([
            -2.0 * (std::f32::consts::TAU * tick as f32 / 8.0).cos(),
            0.0,
            0.0,
        ]);
        animated.kind = 2;
        animated.transform.rot_euler[2] = 0.6 * (tick as f32 / 8.0).sin();
        bodies[0] = Some(animated);
        bodies[1] = Some(body([0.0, 0.0, 0.0]));
        bodies
    }

    fn run(irregular: bool) -> ([f32; 3], f64, Seconds) {
        let mut simulation = RigidSimulation::default().with_step(live_step());
        simulation
            .advance(&trajectory(0), [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
            .unwrap();
        for sample in 1..=32 {
            let time = Seconds(sample as f64 / 240.0);
            if irregular && sample != 32 {
                simulation.set_step(live_step().authored_sample());
                simulation
                    .advance(
                        &trajectory(sample),
                        [0.0; 3],
                        PlayedTime::from_played((time).0),
                        1.0,
                        0.0,
                    )
                    .unwrap();
                simulation.set_step(live_step());
            } else {
                simulation
                    .advance(
                        &trajectory(sample),
                        [0.0; 3],
                        PlayedTime::from_played((time).0),
                        1.0,
                        0.0,
                    )
                    .unwrap();
            }
        }
        assert_eq!(simulation.pending_time, Seconds::ZERO);
        (
            simulation.poses[1].pos,
            simulation.physics_time,
            simulation.pending_time,
        )
    }

    for irregular in [false, true] {
        let (position, physics_time, pending_time) = run(irregular);
        assert!(position.iter().all(|value| value.is_finite()));
        assert!(position.iter().any(|value| value.abs() > 0.01));
        // Authored samples retain the nonlinear path, but one late
        // displayed frame accepts only two fixed intervals.
        let accepted = if irregular { 2.0 * FRAME } else { 32.0 / 240.0 };
        assert!((physics_time - accepted).abs() < 1e-12);
        assert_eq!(pending_time, Seconds::ZERO);
    }
}

#[test]
fn fast_dynamic_body_uses_bullet_collision_against_animated_body() {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(body([0.0, 3.0, 0.0]));
    let mut animated = body([0.0, 0.0, 0.0]);
    animated.kind = 2;
    bodies[1] = Some(animated);
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, [0.0, -20_000.0, 0.0], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    simulation
        .advance(
            &bodies,
            [0.0, -20_000.0, 0.0],
            PlayedTime::from_played(FRAME),
            1.0,
            0.0,
        )
        .unwrap();
    assert!(
        simulation.poses[0].pos[1] > 0.9,
        "fast dynamic body passed through the animated body: {:?}",
        simulation.poses[0].pos
    );
}

#[test]
fn two_fast_dynamics_collide_without_bullet_target_skip() {
    let mut bodies = vec![None; 64];
    let mut floor = body([0.0, -2.0, 0.0]);
    floor.kind = 0;
    bodies[0] = Some(floor);
    bodies[1] = Some(body([0.0, -0.5, 0.0]));
    bodies[2] = Some(body([0.0, 3.0, 0.0]));
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, [0.0, -20_000.0, 0.0], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    simulation
        .advance(
            &bodies,
            [0.0, -20_000.0, 0.0],
            PlayedTime::from_played(FRAME),
            1.0,
            0.0,
        )
        .unwrap();
    assert!(
        simulation.poses[2].pos[1] > 0.0,
        "fast Dynamic passed through another Dynamic: {:?}",
        simulation.poses[2].pos
    );
}

#[test]
fn extreme_animated_translation_uses_automatic_outer_steps() {
    let mut bodies = vec![None; 64];
    let mut animated = body([-100.0, 0.0, 0.0]);
    animated.kind = 2;
    bodies[0] = Some(animated);
    bodies[1] = Some(body([0.0, 0.0, 0.0]));
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    bodies[0].as_mut().unwrap().transform.pos[0] = 100.0;
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(FRAME), 1.0, 0.0)
        .unwrap();
    assert!(
        simulation.poses[1]
            .pos
            .iter()
            .any(|value| value.abs() > 0.01),
        "extreme Animated translation missed the Dynamic body: animated={:?}, dynamic={:?}",
        simulation.poses[0].pos,
        simulation.poses[1].pos
    );
}

#[test]
fn ordinary_live_caps_progress_at_two_intervals_per_display_frame() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    for fps in [20, 24, 30, 60] {
        let mut simulation = RigidSimulation::default().with_step(live_step());
        simulation
            .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
            .unwrap();
        for frame in 1..=fps {
            simulation
                .advance(
                    &bodies,
                    GRAVITY,
                    PlayedTime::from_played(frame as f64 / fps as f64),
                    1.0,
                    0.0,
                )
                .unwrap();
        }
        assert_eq!(simulation.pending_time, Seconds::ZERO, "fps={fps}");
        let expected = (2.0 * f64::from(fps) * FRAME).min(1.0);
        assert!(
            (simulation.physics_time - expected).abs() < 1e-12,
            "fps={fps}"
        );
    }
}

#[test]
fn sim_rate_rigid_export_matches_live() {
    for rate in manifold_physics::SimRate::ALL {
        let project_fps = rate.hz();
        let run = |fps, offline| {
            let interval = Seconds(rate.interval());
            let step = if offline {
                SimStep::export(interval)
            } else {
                SimStep::live(interval)
            };
            let mut simulation = RigidSimulation::default().with_step(step);
            let bodies = one_body([0.0, 4.0, 0.0]);
            for frame in 0..=fps {
                simulation
                    .advance(
                        &bodies,
                        GRAVITY,
                        PlayedTime::from_played(frame as f64 * (1.0 / fps as f64)),
                        1.0,
                        0.0,
                    )
                    .unwrap();
            }
            (simulation.physics_time, simulation.poses)
        };
        let live = run(60, false);
        for fps in [20, 24, 30, 60] {
            assert_eq!(run(fps, true), live, "project {project_fps}, export {fps}");
        }
    }
}

#[test]
fn sim_rate_shown_body_moves_every_frame_between_ticks() {
    // 30 Hz ticks under 60 fps: the settled pose moves every other frame,
    // the shown pose on every frame, always between the two settled ticks.
    let interval = Seconds(manifold_physics::SimRate::Hz30.interval());
    let mut simulation = RigidSimulation::default().with_step(SimStep::live(interval));
    let bodies = one_body([0.0, 8.0, 0.0]);
    let (mut settled_moves, mut shown_moves) = (0, 0);
    let mut last = None;
    for frame in 0..48 {
        simulation
            .advance(
                &bodies,
                GRAVITY,
                PlayedTime::from_played(frame as f64 / 60.0),
                1.0,
                0.0,
            )
            .unwrap();
        let settled = simulation.poses[0].pos[1];
        let shown = simulation.shown().poses[0].pos[1];
        assert!(
            shown >= settled - 1e-6,
            "frame {frame}: shown {shown} below settled {settled}"
        );
        if let Some((last_settled, last_shown)) = last
            && frame > 12
        {
            settled_moves += usize::from(settled != last_settled);
            shown_moves += usize::from(shown < last_shown);
        }
        last = Some((settled, shown));
    }
    assert_eq!(shown_moves, 35);
    assert!(settled_moves <= 18, "settled moved {settled_moves} times");
}

#[test]
fn live_endpoint_has_no_backlog_and_reset_clears_state() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default().with_step(live_step());
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(3.0), 1.0, 0.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert!((simulation.physics_time - 2.0 * FRAME).abs() < 1e-12);
    let accepted_pose = simulation.poses.clone();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(3.0), 0.0, 0.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert_eq!(simulation.poses, accepted_pose);
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(6.0), 1.0, 0.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert!((simulation.physics_time - 2.0 * FRAME).abs() < 1e-12);
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::from_played(6.0), 1.0, 1.0)
        .unwrap();
    assert_eq!(simulation.pending_time, Seconds::ZERO);
    assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
}

#[test]
fn step_budget_exists_only_live() {
    let budget = std::time::Duration::from_millis(33);
    let live = live_step().with_preview_budget(budget);
    assert_eq!(live.budget(), Some(budget));
    assert_eq!(
        SimStep::export(FIXED_TICK)
            .with_preview_budget(budget)
            .budget(),
        None
    );
    assert_eq!(SimStep::default().budget(), None);
}

#[test]
fn history_drain_is_honoured_only_in_export() {
    assert!(!SimStep::export(FIXED_TICK).history_drain_active());
    assert!(
        SimStep::export(FIXED_TICK)
            .draining_history()
            .history_drain_active()
    );
    assert!(!live_step().draining_history().history_drain_active());
}

#[test]
fn offline_history_drain_keeps_authored_history_bounded() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, GRAVITY, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();

    let authored = SimStep::default().authored_sample();
    for sample in 1..=1024 {
        simulation.set_step(authored);
        let time = Seconds(sample as f64 / 240.0);
        if sample % 64 == 0 {
            simulation.set_step(authored.draining_history());
            simulation
                .advance(
                    &bodies,
                    GRAVITY,
                    PlayedTime::from_played((time).0),
                    1.0,
                    0.0,
                )
                .unwrap();
        } else {
            simulation
                .advance(
                    &bodies,
                    GRAVITY,
                    PlayedTime::from_played((time).0),
                    1.0,
                    0.0,
                )
                .unwrap();
        }
        assert!(simulation.authored_samples.len() <= AUTHORED_HISTORY_CAPACITY);
    }

    assert!(simulation.physics_time > 0.0);
    assert!(simulation.poses[0].pos[1] < 4.0);
}

#[test]
fn bulk_boxes_share_floor_contacts_and_reset_after_catch_up() {
    let mut bodies = vec![None; 64];
    let mut ground = body([0.0, -0.28867513, 0.0]);
    ground.kind = 0;
    ground.transform.scale = [20.0, 0.5, 20.0];
    bodies[0] = Some(ground);
    let mut prototype = body([0.0, 3.0, 0.0]);
    prototype.transform.scale = [0.5; 3];
    let mut sim = RigidSimulation::default();
    for frame in 0..=240 {
        sim.advance_scene(
            &copies_scene(
                &bodies,
                Some(prototype.clone()),
                32.0,
                1.25,
                4.0,
                0.0,
                GRAVITY,
                None,
                &[],
            ),
            PlayedTime::from_played(frame as f64 * FRAME),
            1.0,
            0.0,
        )
        .unwrap();
    }
    assert_eq!(sim.active_copies(0), 32);
    for pose in &sim.group_poses(0)[..32] {
        assert!(
            (0.2..1.3).contains(&pose.pos[1]),
            "box must settle on floor/another box: {pose:?}"
        );
    }
    sim.advance_scene(
        &copies_scene(
            &bodies,
            Some(prototype.clone()),
            64.0,
            1.25,
            4.0,
            0.0,
            GRAVITY,
            None,
            &[],
        ),
        PlayedTime::from_played(5.0),
        0.0,
        0.0,
    )
    .unwrap();
    // Held on a tick boundary, the next tick starts where time stands.
    assert_eq!(
        sim.active_copies(0),
        64,
        "a held Count edit lays out at once, with no reset"
    );
    assert_eq!(sim.group_poses(0).len(), 64);
    sim.advance_scene(
        &copies_scene(
            &bodies,
            Some(prototype.clone()),
            64.0,
            1.25,
            4.0,
            0.0,
            GRAVITY,
            None,
            &[],
        ),
        PlayedTime::from_played(8.0),
        1.0,
        0.0,
    )
    .unwrap();
    sim.advance_scene(
        &copies_scene(
            &bodies,
            Some(prototype.clone()),
            64.0,
            1.25,
            4.0,
            0.0,
            GRAVITY,
            None,
            &[],
        ),
        PlayedTime::from_played(8.0),
        1.0,
        1.0,
    )
    .unwrap();
    assert_eq!(sim.active_copies(0), 64);
    assert_eq!(sim.group_poses(0)[0].pos[1], 3.0);
}

#[test]
fn bulk_fixed_ticks_match_across_frame_partitions() {
    let mut full = RigidSimulation::default();
    let mut half = RigidSimulation::default();
    for (simulation, frames, dt) in [(&mut full, 60, FRAME), (&mut half, 120, FRAME / 2.0)] {
        for frame in 0..=frames {
            simulation
                .advance_scene(
                    &copies_scene(
                        &vec![None; 64],
                        Some(body([0.0, 8.0, 0.0])),
                        20.0,
                        2.0,
                        4.0,
                        0.0,
                        GRAVITY,
                        None,
                        &[],
                    ),
                    PlayedTime::from_played(frame as f64 * dt),
                    1.0,
                    0.0,
                )
                .unwrap();
        }
    }
    assert_eq!(full.group_poses(0), half.group_poses(0));
    let full_handle = full.native_copy_handles(0)[0].unwrap();
    let half_handle = half.native_copy_handles(0)[0].unwrap();
    let full_world = full.world.as_ref().unwrap();
    let half_world = half.world.as_ref().unwrap();
    let full_pose = full_world.pose(full_handle).unwrap();
    let half_pose = half_world.pose(half_handle).unwrap();
    let full_velocity = full_world.linear_velocity(full_handle).unwrap();
    let half_velocity = half_world.linear_velocity(half_handle).unwrap();
    assert_eq!(
        full_pose.position.map(|value| value.to_bits()),
        half_pose.position.map(|value| value.to_bits())
    );
    assert_eq!(
        full_pose.rotation.map(|value| value.to_bits()),
        half_pose.rotation.map(|value| value.to_bits())
    );
    assert_eq!(
        full_velocity.map(|value| value.to_bits()),
        half_velocity.map(|value| value.to_bits())
    );
}

/// A Fixed wedge 4 m long, rising from 0 at x = 2 to 1 m at x = −2, 2 m deep:
/// an exact mesh with every face wound outward.
fn wedge_collider() -> Arc<Solid> {
    let vertices = vec![
        [-2.0, 0.0, -1.0],
        [2.0, 0.0, -1.0],
        [-2.0, 1.0, -1.0],
        [-2.0, 0.0, 1.0],
        [2.0, 0.0, 1.0],
        [-2.0, 1.0, 1.0],
    ];
    let triangles = vec![
        [0, 2, 1],
        [3, 4, 5],
        [0, 1, 4],
        [0, 4, 3],
        [0, 3, 5],
        [0, 5, 2],
        [1, 2, 5],
        [1, 5, 4],
    ];
    Arc::new(Solid::Mesh(manifold_physics::TriangleMesh {
        vertices,
        triangles,
    }))
}

/// A doorstop: a box whose top slopes down toward +x, so its mirror is a
/// different hull.
fn doorstop() -> Arc<Solid> {
    let points: Vec<[f32; 3]> = [-0.2f32, 0.2]
        .into_iter()
        .flat_map(|z| {
            [
                [-0.3, -0.2, z],
                [0.3, -0.2, z],
                [-0.3, 0.2, z],
                [0.3, 0.0, z],
            ]
        })
        .collect();
    Arc::new(Solid::Hulls(vec![
        manifold_physics::cook_hull(&points).unwrap(),
    ]))
}

/// A doorstop dropped on the wedge's slope, with friction to hold it there,
/// after three seconds; `mirror` mirrors both in X, as Scale X −1 does.
fn doorstop_on_wedge(mirror: f32) -> (Transform, Vec<BodyRefusal>) {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        transform: Transform {
            scale: [mirror, 1.0, 1.0],
            ..Transform::default()
        },
        kind: 0,
        mass: 0.0,
        friction: 1.0,
        collider: Some(wedge_collider()),
        ..RigidBody::default()
    });
    bodies[1] = Some(RigidBody {
        transform: Transform {
            pos: [0.3 * mirror, 1.5, 0.0],
            scale: [mirror, 1.0, 1.0],
            ..Transform::default()
        },
        friction: 1.0,
        bounce: 0.0,
        collider: Some(doorstop()),
        ..RigidBody::default()
    });
    let mut simulation = RigidSimulation::default();
    for frame in 0..=180 {
        simulation
            .advance(
                &bodies,
                GRAVITY,
                PlayedTime::from_played(frame as f64 * FRAME),
                1.0,
                0.0,
            )
            .unwrap();
    }
    (simulation.poses[1], simulation.refusals().to_vec())
}

/// A mirrored Body resting on a mirrored Collider settles where the
/// unmirrored pair does, mirrored: Box3D rebuilds the reflected hull with
/// its faces out, and the reflected mesh's turned triangles still face up.
#[test]
fn a_mirrored_body_on_a_mirrored_collider_settles_mirrored() {
    let (plain, plain_refusals) = doorstop_on_wedge(1.0);
    let (mirrored, mirrored_refusals) = doorstop_on_wedge(-1.0);
    assert!(
        plain_refusals.is_empty() && mirrored_refusals.is_empty(),
        "{plain_refusals:?} {mirrored_refusals:?}"
    );
    // The slope's height under each body's centre.
    let slope = |x: f32| (2.0 - x) / 4.0;
    assert!(
        plain.pos[1] > slope(plain.pos[0]) && plain.pos[1] < slope(plain.pos[0]) + 0.5,
        "the doorstop rests on the slope: {plain:?}"
    );
    assert!(
        plain.pos[0] > 0.0,
        "it slid down the slope toward +x before it held: {plain:?}"
    );
    // Both shapes are symmetric across the slope, so the X mirror is also a
    // half turn about Y. Box3D's contact solve drifts the doorstop a few mm
    // across the slope and lands it the half-turned way, so that axis
    // matches up to sign.
    let expected = [-plain.pos[0], plain.pos[1], plain.pos[2].abs()];
    let got = [mirrored.pos[0], mirrored.pos[1], mirrored.pos[2].abs()];
    for axis in 0..3 {
        assert!(
            (got[axis] - expected[axis]).abs() < 1e-4,
            "mirrored {:?} vs plain mirrored {expected:?}",
            mirrored.pos
        );
    }
    assert_eq!(
        mirrored.scale,
        [-1.0, 1.0, 1.0],
        "the mirrored body draws mirrored"
    );
}

/// Any finite non-zero scale enters the world: a 0.0005-scale cube and a
/// 2000-scale Collider, and a mirrored copy group. Only zero and a shape
/// Box3D's hull builder cannot make (flat for its size) are refused, each
/// on Scale with its reason.
#[test]
fn any_finite_nonzero_scale_enters_the_world() {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        transform: Transform {
            pos: [0.0, -2000.0, 0.0],
            scale: [2000.0; 3],
            ..Transform::default()
        },
        kind: 0,
        mass: 0.0,
        collider: Some(wedge_collider()),
        ..RigidBody::default()
    });
    bodies[1] = Some(RigidBody {
        transform: Transform {
            pos: [0.0, 10.0, 0.0],
            scale: [0.0005; 3],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    bodies[2] = Some(RigidBody {
        transform: Transform {
            pos: [3.0, 10.0, 0.0],
            scale: [0.5, 1.0e-9, 0.5],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    bodies[3] = Some(RigidBody {
        transform: Transform {
            pos: [6.0, 10.0, 0.0],
            scale: [0.0, 1.0, 1.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    let prototype = RigidBody {
        transform: Transform {
            pos: [-6.0, 10.0, 0.0],
            scale: [-0.5, 0.5, 0.5],
            ..Transform::default()
        },
        ..RigidBody::default()
    };
    let scene = copies_scene(
        &bodies,
        Some(prototype),
        4.0,
        1.25,
        2.0,
        0.0,
        GRAVITY,
        None,
        &[],
    );
    let mut simulation = RigidSimulation::default();
    for frame in 0..=30 {
        simulation
            .advance_scene(
                &scene,
                PlayedTime::from_played(frame as f64 * FRAME),
                1.0,
                0.0,
            )
            .unwrap();
    }
    assert!(
        simulation.handles[0].is_some() && simulation.handles[1].is_some(),
        "the 2000-scale Collider and the 0.0005-scale cube are in the world"
    );
    assert!(simulation.poses[1].pos[1] < 10.0, "the tiny cube falls");
    let refused: Vec<(Member, Option<&str>)> = simulation
        .refusals()
        .iter()
        .map(|refusal| (refusal.member, refusal.reason.param()))
        .collect();
    assert_eq!(
        refused,
        [
            (Member::Body(2), Some("scale_x")),
            (Member::Body(3), Some("scale_x"))
        ]
    );
    let reasons: Vec<String> = simulation
        .refusals()
        .iter()
        .map(|refusal| refusal.reason.to_string())
        .collect();
    assert!(
        reasons[0].contains("too thin to collide") && reasons[1].contains("not zero"),
        "{reasons:?}"
    );
    let copies = simulation.group_poses(0);
    assert_eq!(copies.len(), 4);
    assert!(
        copies
            .iter()
            .all(|pose| pose.scale == [-0.5, 0.5, 0.5] && pose.pos[1] < 10.0),
        "the mirrored copies fall, drawn mirrored: {copies:?}"
    );
}

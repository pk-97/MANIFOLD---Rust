//! Worlds past the old 64-body cap and with several copy groups.

use super::*;
use manifold_physics::input::EventStamp;

const DT: f64 = 1.0 / 60.0;

fn at(position: [f32; 3]) -> RigidBody {
    RigidBody {
        transform: Transform { pos: position, scale: [0.4; 3], ..Transform::default() },
        ..RigidBody::default()
    }
}

fn spawner(position: [f32; 3], count: f32) -> RigidBody {
    RigidBody { spawn: CopySpawn { count, spacing: 1.0, columns: 4.0, layout: 0.0, turn: [0.0; 3] }, ..at(position) }
}

fn hit(simulation: &mut RigidSimulation, sequence: u64, targets: RigidImpulseTargets) {
    let stamp = EventStamp { epoch: simulation.impulse_epoch().unwrap(), time: Seconds::ZERO, sequence };
    simulation
        .enqueue_impulse(stamp, ResolvedRigidImpulse { field: FieldValue::uniform([3.0, 0.0, 0.0]).unwrap(), targets })
        .unwrap();
}

#[test]
fn a_256_body_world_takes_every_body_and_a_hit_lands_on_body_200_only() {
    let bodies: Vec<_> = (0..MAX_BODIES)
        .map(|index| Some(at([(index % 16) as f32 * 2.0, 0.0, (index / 16) as f32 * 2.0])))
        .collect();
    let mut simulation = RigidSimulation::default();
    simulation.advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0).unwrap();
    assert!(simulation.native_handles().iter().all(Option::is_some), "every one of 256 bodies is installed");
    let before = simulation.poses.clone();
    hit(&mut simulation, 1, RigidImpulseTargets::body(200).unwrap());
    simulation.advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0).unwrap();
    for (index, (now, then)) in simulation.poses.iter().zip(&before).enumerate() {
        if index == 200 {
            assert!(now.pos[0] - then.pos[0] > 0.04, "body 200 takes the hit");
        } else {
            assert_eq!(now.pos, then.pos, "body {index} is untouched");
        }
    }
}

#[test]
fn two_copy_groups_share_one_world_and_a_hit_on_group_one_moves_only_its_copies() {
    let scene = RigidSceneInputs {
        bodies: vec![None; 4],
        groups: vec![Some(spawner([0.0, 0.0, 0.0], 8.0)), Some(spawner([20.0, 0.0, 0.0], 5.0))],
        gravity: [0.0; 3],
        acceleration_field: None,
        targeted_fields: Vec::new(),
    };
    let mut simulation = RigidSimulation::default();
    simulation.advance_scene(&scene, PlayedTime::ZERO, 1.0, 0.0).unwrap();
    assert_eq!((simulation.active_copies(0), simulation.active_copies(1)), (8, 5));
    assert_eq!(simulation.total_active_copies(), 13);
    let before = [simulation.group_poses(0).to_vec(), simulation.group_poses(1).to_vec()];
    hit(&mut simulation, 1, RigidImpulseTargets::group(1).unwrap());
    simulation.advance_scene(&scene, PlayedTime::from_played(DT), 1.0, 0.0).unwrap();
    assert_eq!(simulation.group_poses(0), &before[0][..], "group 0 is untouched");
    for (now, then) in simulation.group_poses(1).iter().zip(&before[1]) {
        assert!(now.pos[0] - then.pos[0] > 0.04, "every copy of group 1 takes the hit");
    }
    assert!(
        RigidImpulseTargets::group(2).is_some_and(|missing| simulation.validate_impulse_targets(missing).is_err()),
        "a group the world does not have refuses the hit"
    );
}

#[test]
fn copies_of_two_groups_collide_with_each_other() {
    // Group 1's copies fall onto group 0's one fixed copy and come to rest on it.
    let floor = RigidBody {
        kind: 0,
        transform: Transform { pos: [0.0, 0.0, 0.0], scale: [4.0; 3], ..Transform::default() },
        spawn: CopySpawn { count: 1.0, spacing: 1.0, columns: 1.0, layout: 0.0, turn: [0.0; 3] },
        ..RigidBody::default()
    };
    let falling = spawner([0.0, 8.0, 0.0], 4.0);
    let scene = RigidSceneInputs {
        bodies: vec![None; 1],
        groups: vec![Some(floor), Some(falling)],
        gravity: [0.0, -9.8, 0.0],
        acceleration_field: None,
        targeted_fields: Vec::new(),
    };
    let mut simulation = RigidSimulation::default();
    for frame in 0..=120 {
        simulation.advance_scene(&scene, PlayedTime::from_played(f64::from(frame) * DT), 1.0, 0.0).unwrap();
    }
    for copy in simulation.group_poses(1) {
        assert!(copy.pos[1] > 1.0, "a falling copy rests on the other group: {:?}", copy.pos);
    }
}

#[test]
fn the_world_copy_budget_is_shared_and_later_groups_make_what_fits() {
    let scene = RigidSceneInputs {
        bodies: Vec::new(),
        groups: vec![
            Some(spawner([0.0, 0.0, 0.0], (MAX_COPIES - 1_000) as f32)),
            Some(spawner([2_000.0, 0.0, 0.0], 3_000.0)),
            Some(spawner([400.0, 0.0, 0.0], 10.0)),
        ],
        gravity: [0.0; 3],
        acceleration_field: None,
        targeted_fields: Vec::new(),
    };
    let mut simulation = RigidSimulation::default();
    simulation.advance_scene(&scene, PlayedTime::ZERO, 1.0, 0.0).unwrap();
    assert_eq!(
        (simulation.active_copies(0), simulation.active_copies(1), simulation.active_copies(2)),
        (MAX_COPIES - 1_000, 1_000, 0)
    );
    assert_eq!(simulation.copies_not_made(), 2_000 + 10);
}

#[test]
fn steady_ticks_never_regrow_the_world_storage() {
    let mut bodies = vec![None; MAX_BODIES];
    bodies[0] = Some(at([0.0, 4.0, 0.0]));
    bodies[255] = Some(at([4.0, 4.0, 0.0]));
    let mut targeted = vec![None; MAX_BODIES + 2];
    targeted[255] = Some(FieldValue::uniform([0.0, 1.0, 0.0]).unwrap());
    let scene = RigidSceneInputs {
        bodies,
        groups: vec![Some(spawner([0.0, 8.0, 0.0], 6.0)), None],
        gravity: [0.0, -9.8, 0.0],
        acceleration_field: None,
        targeted_fields: targeted,
    };
    let mut simulation = RigidSimulation::default();
    simulation.advance_scene(&scene, PlayedTime::ZERO, 1.0, 0.0).unwrap();
    simulation.advance_scene(&scene, PlayedTime::from_played(DT), 1.0, 0.0).unwrap();
    let storage = (
        simulation.authored_bodies.storage_ptr(),
        simulation.targeted_fields.storage_ptr(),
        simulation.handles.as_ptr() as usize,
        simulation.poses.as_ptr() as usize,
        simulation.field_handles.capacity(),
    );
    for frame in 2..240 {
        simulation.advance_scene(&scene, PlayedTime::from_played(f64::from(frame) * DT), 1.0, 0.0).unwrap();
        assert_eq!(
            storage,
            (
                simulation.authored_bodies.storage_ptr(),
                simulation.targeted_fields.storage_ptr(),
                simulation.handles.as_ptr() as usize,
                simulation.poses.as_ptr() as usize,
                simulation.field_handles.capacity(),
            ),
            "frame {frame} regrew world storage"
        );
    }
    assert!(simulation.poses[255].pos[1] > simulation.poses[0].pos[1], "body 255 feels its own field");
}

#[test]
fn a_new_body_count_rebuilds_the_world_with_the_same_wired_bodies() {
    let mut small = vec![None; 64];
    small[3] = Some(at([1.0, 2.0, 3.0]));
    let mut simulation = RigidSimulation::default();
    simulation.advance(&small, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0).unwrap();
    let epoch = simulation.impulse_epoch();
    let mut large = small.clone();
    large.resize(200, None);
    large[150] = Some(at([5.0, 2.0, 3.0]));
    simulation.advance(&large, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0).unwrap();
    assert_ne!(simulation.impulse_epoch(), epoch, "a count change is a new world");
    assert_eq!(simulation.native_handles().len(), 200);
    assert_eq!(simulation.poses[3].pos, [1.0, 2.0, 3.0]);
    assert_eq!(simulation.poses[150].pos, [5.0, 2.0, 3.0]);
}

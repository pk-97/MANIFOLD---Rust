//! Old shows play bit-identically: digests of every pose at tick 120,
//! captured from main before the Physics World took its body and copy-group
//! counts.

use super::*;
use manifold_core::copies_body::{platonic_width, size_multiple, spacing_unit};
use manifold_node_engine::scene::transform::Transform;

const GRAVITY: [f32; 3] = [0.0, -9.81, 0.0];

fn digest(poses: impl Iterator<Item = Transform>) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for pose in poses {
        for value in pose.pos.into_iter().chain(pose.rot_euler).chain(pose.scale) {
            for byte in value.to_bits().to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    hash
}

fn placed(pos: [f32; 3], rot: [f32; 3], scale: [f32; 3], kind: u32) -> RigidBody {
    RigidBody {
        transform: Transform { pos, rot_euler: rot, scale, billboard: false },
        kind,
        ..RigidBody::default()
    }
}

/// The bundled Physics Boxes scene: a floor, two ramps and a 256-box pile.
fn physics_boxes() -> (Vec<Option<RigidBody>>, RigidBody) {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(RigidBody {
        friction: 0.6,
        bounce: 0.1,
        ..placed([0.0, -0.144_337_57, 0.0], [0.0; 3], [100.0, 0.25, 100.0], 0)
    });
    for (slot, side) in [(1, -1.0f32), (2, 1.0)] {
        bodies[slot] = Some(RigidBody {
            friction: 0.2,
            bounce: 0.28,
            ..placed([4.0 * side, 3.0, 0.0], [0.0, 0.12 * side, 0.45 * side], [6.0, 0.5, 10.0], 0)
        });
    }
    // As the load migration leaves it: the old rotation is each box's own
    // turn, the group is unturned, and 1.85 m is a multiple of the box size.
    let unit = spacing_unit(true, platonic_width(1, 0.8))[0];
    let prototype = RigidBody {
        friction: 0.25,
        bounce: 0.38,
        spawn: CopySpawn { count: 256.0, spacing: size_multiple(1.85, unit), columns: 24.0, layout: 1.0, turn: [0.08, 0.12, 0.04] },
        ..placed([0.0, 9.0, 0.0], [0.0; 3], [0.8; 3], 1)
    };
    assert_eq!(size_multiple(1.85, unit) * unit, 1.85, "the migrated spacing is exact");
    (bodies, prototype)
}

/// Sixty-four wired bodies: a fixed floor and 63 dynamics of every shape.
fn sixty_four_bodies() -> Vec<Option<RigidBody>> {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(placed([0.0, -0.5, 0.0], [0.0; 3], [40.0, 1.0, 40.0], 0));
    for (slot, body) in bodies.iter_mut().enumerate().skip(1) {
        let i = slot - 1;
        let (column, row, layer) = (i % 4, (i / 4) % 4, i / 16);
        *body = Some(RigidBody {
            shape: (slot % 5) as u32,
            density: 400.0 + 20.0 * slot as f32,
            ..placed(
                [column as f32 * 1.6 - 2.4, 1.5 + layer as f32 * 1.7, row as f32 * 1.6 - 2.4],
                [0.1 * slot as f32, 0.07 * slot as f32, 0.0],
                [0.6; 3],
                1,
            )
        });
    }
    bodies
}

#[test]
fn physics_boxes_and_a_sixty_four_body_world_match_main_at_tick_120() {
    let (bodies, prototype) = physics_boxes();
    let scene = RigidSceneInputs {
        bodies,
        groups: vec![Some(prototype)],
        gravity: GRAVITY,
        ..RigidSceneInputs::default()
    };
    let mut boxes = RigidSimulation::default();
    for frame in 0..=120 {
        boxes.advance_scene(&scene, PlayedTime::from_played(f64::from(frame) / 60.0), 1.0, 0.0).unwrap();
    }
    assert_eq!(boxes.active_copies(0), 256);
    let boxes_digest = digest(boxes.poses.iter().chain(boxes.group_poses(0)).copied());
    assert_eq!(boxes_digest, 0x3978_cfcf_78b7_3c38, "Physics Boxes left main's trajectory");

    let wired = sixty_four_bodies();
    let mut world = RigidSimulation::default();
    for frame in 0..=120 {
        world.advance(&wired, GRAVITY, PlayedTime::from_played(f64::from(frame) / 60.0), 1.0, 0.0).unwrap();
    }
    assert_eq!(digest(world.poses.iter().copied()), 0xfed1_7989_d710_3655, "a 64-body world left main's trajectory");
}

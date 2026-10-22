//! Live copy-group layouts: where copies rest, and how layout, group and
//! Count edits land in a running world.

use super::copy_layout::{copy_width, Arrangement, CopyLayout};
use super::test_scene::copies_scene;
use super::*;
use manifold_core::copies_body::{platonic_width, spacing_unit, PLATONIC_HALF_WIDTH};
use manifold_node_engine::scene::transform::Transform;

const GRAVITY: [f32; 3] = [0.0, -9.81, 0.0];

fn cube(position: [f32; 3], scale: f32) -> RigidBody {
    RigidBody {
        transform: Transform { pos: position, scale: [scale; 3], ..Transform::default() },
        ..RigidBody::default()
    }
}

fn group(layout: CopyLayout, count: usize, spacing: f32, columns: usize) -> Arrangement {
    Arrangement { layout, count, columns, spacing, turn: [0.0; 3], origin: [1.0, 2.0, 3.0], rotation: [0.0; 3] }
}

fn rests(arrangement: &Arrangement, width: [f32; 3]) -> Vec<manifold_physics::BodyPose> {
    (0..arrangement.count).map(|index| arrangement.rest(width, index)).collect()
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|axis| (a[axis] - b[axis]).powi(2)).sum::<f32>().sqrt()
}

fn centroid(poses: &[manifold_physics::BodyPose]) -> [f32; 3] {
    let mut sum = [0.0f64; 3];
    for pose in poses {
        for (total, value) in sum.iter_mut().zip(pose.position) {
            *total += f64::from(value);
        }
    }
    sum.map(|value| (value / poses.len() as f64) as f32)
}

/// Rotate `v` about Y by `angle`, the scene's Ry.
fn yaw(v: [f32; 2], angle: f32) -> [f32; 2] {
    let (sin, cos) = angle.sin_cos();
    [v[0] * cos + v[1] * sin, -v[0] * sin + v[1] * cos]
}

/// Whether two boxes of half-size `half` (X, Z), turned `a` and `b` about Y
/// at centres `p` and `q`, overlap in the XZ plane (separating axes).
fn boxes_overlap(p: [f32; 3], a: f32, q: [f32; 3], b: f32, half: [f32; 2]) -> bool {
    let axes = [yaw([1.0, 0.0], a), yaw([0.0, 1.0], a), yaw([1.0, 0.0], b), yaw([0.0, 1.0], b)];
    let d = [q[0] - p[0], q[2] - p[2]];
    axes.iter().all(|axis| {
        let project = |angle: f32| {
            let x = yaw([1.0, 0.0], angle);
            let z = yaw([0.0, 1.0], angle);
            half[0] * (x[0] * axis[0] + x[1] * axis[1]).abs() + half[1] * (z[0] * axis[0] + z[1] * axis[1]).abs()
        };
        (d[0] * axis[0] + d[1] * axis[1]).abs() < project(a) + project(b) - 1.0e-4
    })
}

fn yaw_of(rotation: [f32; 4]) -> f32 {
    // Pure Y rotations only: q = (0, sin(a/2), 0, cos(a/2)).
    2.0 * rotation[1].atan2(rotation[3])
}

#[test]
fn the_platonic_widths_are_the_installed_hulls_bit_for_bit() {
    for shape in 0..5u32 {
        let body = RigidBody { collider: Some(platonic_solid(shape)), ..cube([0.0; 3], 0.8) };
        assert_eq!(copy_width(&body), platonic_width(shape, 0.8), "shape {shape}");
        let half = PLATONIC_HALF_WIDTH[shape as usize];
        let max = manifold_node_engine::platonic::platonic_points(shape).iter().map(|p| p[0]).fold(f32::MIN, f32::max);
        assert_eq!(max.to_bits(), half.to_bits(), "shape {shape}");
    }
}

#[test]
fn line_and_grid_are_centred_and_spaced_by_the_copy_size() {
    let width = platonic_width(1, 0.5);
    let line = rests(&group(CopyLayout::Line, 5, 1.5, 16), width);
    let c = centroid(&line);
    assert!(distance(c, [1.0, 2.0, 3.0]) < 1.0e-5, "line centre {c:?}");
    for pair in line.windows(2) {
        assert!((distance(pair[0].position, pair[1].position) - 1.5 * width[0]).abs() < 1.0e-5);
    }
    // Four copies in a 16-column grid make one centred row, not a corner.
    // Extra layers stack up from the group's plane.
    for count in [4, 12, 16, 48] {
        let grid = rests(&group(CopyLayout::Grid, count, 1.1, 4), width);
        let c = centroid(&grid);
        assert!((c[0] - 1.0).abs() < 1.0e-5 && (c[2] - 3.0).abs() < 1.0e-5, "grid of {count} centred: {c:?}");
        assert_eq!(grid[0].position[1], 2.0);
        assert!((grid[1].position[0] - grid[0].position[0] - 1.1 * width[0]).abs() < 1.0e-5);
    }
    let wide = rests(&group(CopyLayout::Grid, 4, 1.1, 16), width);
    assert!((centroid(&wide)[0] - 1.0).abs() < 1.0e-5, "fewer copies than columns stay centred");
}

#[test]
fn every_layout_holds_its_copies_apart_at_spacing_one() {
    let width = platonic_width(1, 0.7);
    let half = [width[0] * 0.5, width[2] * 0.5];
    for (layout, counts) in [
        (CopyLayout::Line, &[2usize, 7][..]),
        (CopyLayout::Grid, &[9, 30][..]),
        (CopyLayout::Ring, &[2, 3, 4, 6, 12, 40][..]),
    ] {
        for &count in counts {
            let poses = rests(&group(layout, count, 1.0, 5), width);
            for (index, left) in poses.iter().enumerate() {
                for right in &poses[index + 1..] {
                    if (left.position[1] - right.position[1]).abs() >= width[1] {
                        continue;
                    }
                    assert!(
                        !boxes_overlap(left.position, yaw_of(left.rotation), right.position, yaw_of(right.rotation), half),
                        "{layout:?} x{count} overlaps"
                    );
                }
            }
        }
    }
    // Scatter turns its copies, so they keep a whole bounding sphere apart.
    let scatter = rests(&group(CopyLayout::Scatter, 256, 1.0, 16), width);
    let sphere = (width[0] * width[0] + width[1] * width[1] + width[2] * width[2]).sqrt();
    let mut closest = f32::MAX;
    for (index, left) in scatter.iter().enumerate() {
        for right in &scatter[index + 1..] {
            closest = closest.min(distance(left.position, right.position));
        }
    }
    assert!(closest >= sphere, "scatter closest {closest} < {sphere}");
}

#[test]
fn ring_neighbours_sit_one_spacing_apart_once_their_corners_clear() {
    let width = platonic_width(1, 1.0);
    let step = 1.5 * width[0];
    let poses = rests(&group(CopyLayout::Ring, 2, 1.5, 16), width);
    assert!((distance(poses[0].position, poses[1].position) - step).abs() < 1.0e-5);
    let poses = rests(&group(CopyLayout::Ring, 64, 4.0, 16), width);
    let step = 4.0 * width[0];
    let gap = distance(poses[0].position, poses[1].position);
    assert!(gap >= step - 1.0e-4 && gap < step * 1.05, "{gap} vs {step}");
    assert!(distance(centroid(&poses), [1.0, 2.0, 3.0]) < 1.0e-4);
}

#[test]
fn the_group_transform_turns_and_scales_the_layout_as_one() {
    for layout in [CopyLayout::Grid, CopyLayout::Scatter, CopyLayout::Line, CopyLayout::Ring] {
        let flat = group(layout, 12, 1.2, 4);
        let turned = Arrangement { rotation: [0.3, -0.7, 0.2], ..flat };
        let width = platonic_width(1, 0.6);
        let (a, b) = (rests(&flat, width), rests(&turned, width));
        // Pairwise distances are kept: a rigid placement.
        for i in 0..12 {
            for j in i + 1..12 {
                let (before, after) = (distance(a[i].position, a[j].position), distance(b[i].position, b[j].position));
                assert!((before - after).abs() < 1.0e-4, "{layout:?} {i}-{j}");
            }
        }
        assert_ne!(a[1].position, b[1].position, "{layout:?} turns");
        // Scale is uniform and moves every copy out from the origin with it.
        let big = rests(&flat, platonic_width(1, 1.2));
        for i in 1..12 {
            let small = distance(a[i].position, a[0].position);
            let large = distance(big[i].position, big[0].position);
            assert!((large - 2.0 * small).abs() < 1.0e-4, "{layout:?} scales");
        }
    }
}

/// A floor, one falling cube and a copy group whose spacing changes at one
/// second, run at `fps` live or in export. Returns the group's poses and the
/// cube's at every sixth of a second (a frame at each rate) and at the end.
fn spacing_edit_run(fps: u32, export: bool) -> Vec<Vec<Transform>> {
    let mut bodies = vec![None; 4];
    bodies[0] = Some(RigidBody { kind: 0, ..cube([0.0, -0.5, 0.0], 1.0) });
    bodies[0].as_mut().unwrap().transform.scale = [40.0, 1.0, 40.0];
    bodies[1] = Some(cube([6.0, 3.0, 0.0], 0.7));
    let step = if export { SimStep::export(FIXED_TICK) } else { SimStep::live(FIXED_TICK) };
    let mut simulation = RigidSimulation::default().with_step(step);
    let mut seen = Vec::new();
    for frame in 0..=2 * fps {
        let time = f64::from(frame) / f64::from(fps);
        let spacing = if time >= 1.0 { 1.6 } else { 1.1 };
        let layout = if time >= 1.0 { 3.0 } else { 0.0 };
        let scene = copies_scene(&bodies, Some(cube([0.0, 2.0, 0.0], 0.5)), 16.0, spacing, 4.0, layout, GRAVITY, None, &[]);
        simulation.advance_scene(&scene, PlayedTime::from_played(time), 1.0, 0.0).unwrap();
        if frame * 6 % fps == 0 {
            let mut poses = simulation.group_poses(0).to_vec();
            poses.push(simulation.poses[1]);
            seen.push(poses);
        }
    }
    seen
}

#[test]
fn a_layout_edit_lands_on_the_same_tick_at_any_frame_rate_live_and_in_export() {
    let reference = spacing_edit_run(60, false);
    for fps in [30, 60, 120, 144] {
        for export in [false, true] {
            assert_eq!(spacing_edit_run(fps, export), reference, "fps {fps} export {export}");
        }
    }
    // At one second the copies stand in their new ring, at rest.
    let ring = &reference[6];
    let arrangement = Arrangement { layout: CopyLayout::Ring, ..group(CopyLayout::Ring, 16, 1.6, 4) };
    let arrangement = Arrangement { origin: [0.0, 2.0, 0.0], ..arrangement };
    let width = platonic_width(1, 0.5);
    for (index, pose) in ring[..16].iter().enumerate() {
        assert_eq!(pose.pos, arrangement.rest(width, index).position, "copy {index}");
    }
    assert!(reference[5][0].pos[1] < 1.0, "the grid had fallen before the edit");
}

#[test]
fn laid_out_copies_rest_and_other_bodies_keep_their_motion() {
    let run = |edit: bool| {
        let mut bodies = vec![None; 2];
        bodies[0] = Some(cube([8.0, 4.0, 0.0], 0.5));
        let mut simulation = RigidSimulation::default().with_step(SimStep::export(FIXED_TICK));
        let mut velocities = Vec::new();
        for frame in 0..=60 {
            let time = f64::from(frame) / 60.0;
            let count = if edit && frame >= 30 { 10.0 } else { 20.0 };
            let scene = copies_scene(&bodies, Some(cube([0.0, 3.0, 0.0], 0.5)), count, 1.1, 4.0, 0.0, GRAVITY, None, &[]);
            simulation.advance_scene(&scene, PlayedTime::from_played(time), 1.0, 0.0).unwrap();
            if edit && frame == 30 {
                let world = simulation.native_world().unwrap();
                for handle in simulation.native_copy_handles(0).iter().flatten() {
                    velocities.push((world.linear_velocity(*handle).unwrap(), world.angular_velocity(*handle).unwrap()));
                }
            }
        }
        (simulation.poses[0], velocities, simulation.impulse_epoch)
    };
    let (still, _, epoch) = run(false);
    let (edited, velocities, edited_epoch) = run(true);
    assert_eq!(edited, still, "a Count edit leaves another body's motion untouched");
    assert_eq!(edited_epoch, epoch, "and builds no new world");
    assert_eq!(velocities.len(), 10);
    assert!(velocities.iter().all(|(linear, angular)| *linear == [0.0; 3] && *angular == [0.0; 3]), "{velocities:?}");
}

#[test]
fn count_edits_below_the_high_water_mark_allocate_nothing() {
    let bodies = vec![None; 1];
    let mut simulation = RigidSimulation::default().with_step(SimStep::export(FIXED_TICK));
    let scene = |count: f32| copies_scene(&bodies, Some(cube([0.0, 3.0, 0.0], 0.5)), count, 1.1, 4.0, 1.0, [0.0; 3], None, &[]);
    simulation.advance_scene(&scene(40.0), PlayedTime::ZERO, 1.0, 0.0).unwrap();
    let state = &simulation.groups[0];
    let capacities = (state.handles.capacity(), state.poses.capacity(), simulation.field_handles.capacity(), simulation.authored_bodies.stride());
    let epoch = simulation.impulse_epoch;
    for (frame, count) in [(1, 12.0), (2, 1.0), (3, 39.0), (4, 40.0), (5, 7.0)] {
        simulation.advance_scene(&scene(count), PlayedTime::from_played(f64::from(frame) / 60.0), 1.0, 0.0).unwrap();
        assert_eq!(simulation.active_copies(0), count as usize);
        assert_eq!(simulation.group_poses(0).len(), count as usize);
        let state = &simulation.groups[0];
        assert_eq!(
            (state.handles.capacity(), state.poses.capacity(), simulation.field_handles.capacity(), simulation.authored_bodies.stride()),
            capacities,
            "count {count}"
        );
    }
    assert_eq!(simulation.impulse_epoch, epoch, "no rebuild");
    assert_eq!(simulation.groups[0].handles.len(), 40, "tail bodies stay installed, disabled");
    // A new high installs only the bodies past the mark.
    simulation.advance_scene(&scene(64.0), PlayedTime::from_played(6.0 / 60.0), 1.0, 0.0).unwrap();
    assert_eq!(simulation.active_copies(0), 64);
    assert_eq!(simulation.impulse_epoch, epoch);
}

#[test]
fn reset_returns_the_copies_to_their_layout() {
    let mut bodies = vec![None; 1];
    bodies[0] = Some(RigidBody { kind: 0, transform: Transform { scale: [30.0, 1.0, 30.0], pos: [0.0, -0.5, 0.0], ..Transform::default() }, ..RigidBody::default() });
    let scene = copies_scene(&bodies, Some(cube([0.0, 2.0, 0.0], 0.5)), 24.0, 1.3, 4.0, 1.0, GRAVITY, None, &[]);
    let mut simulation = RigidSimulation::default();
    simulation.advance_scene(&scene, PlayedTime::ZERO, 1.0, 0.0).unwrap();
    let formation = simulation.group_poses(0).to_vec();
    for frame in 1..=90 {
        simulation.advance_scene(&scene, PlayedTime::from_played(f64::from(frame) / 60.0), 1.0, 0.0).unwrap();
    }
    assert_ne!(simulation.group_poses(0), formation.as_slice(), "the copies fell");
    simulation.advance_scene(&scene, PlayedTime::from_played(1.5), 1.0, 1.0).unwrap();
    assert_eq!(simulation.group_poses(0), formation.as_slice());
}

#[test]
fn a_group_with_physics_off_is_drawn_at_rest_with_no_bodies() {
    let prototype = RigidBody { enabled: false, ..cube([0.0, 2.0, 0.0], 0.5) };
    let scene = |spacing: f32| copies_scene(&[None], Some(prototype.clone()), 9.0, spacing, 3.0, 2.0, GRAVITY, None, &[]);
    let mut simulation = RigidSimulation::default();
    for frame in 0..30 {
        simulation.advance_scene(&scene(1.1), PlayedTime::from_played(f64::from(frame) / 60.0), 1.0, 0.0).unwrap();
    }
    assert!(simulation.native_copy_handles(0).is_empty());
    assert_eq!(simulation.copies_not_made(), 0);
    let width = platonic_width(1, 0.5);
    let line = Arrangement::of(&scene(1.1).groups[0].clone().unwrap(), 9);
    assert_eq!(simulation.group_poses(0).len(), 9);
    for (index, pose) in simulation.group_poses(0).iter().enumerate() {
        assert_eq!(pose.pos, line.rest(width, index).position, "still at rest after half a second");
    }
    simulation.advance_scene(&scene(2.0), PlayedTime::from_played(0.5), 1.0, 0.0).unwrap();
    let wider = Arrangement { spacing: 2.0, ..line };
    assert_eq!(simulation.group_poses(0)[8].pos, wider.rest(width, 8).position, "edits show at once");
}

#[test]
fn the_shared_pool_reports_its_shortfall_and_rebalances_on_a_count_edit() {
    let group = |count: f32| RigidBody { spawn: CopySpawn { count, ..CopySpawn::default() }, ..cube([0.0, 50.0, 0.0], 0.2) };
    let mut scene = RigidSceneInputs { bodies: vec![None], groups: vec![Some(group(3_000.0)), Some(group(2_000.0))], gravity: [0.0; 3], ..RigidSceneInputs::default() };
    scene.targeted_fields = vec![None; 3];
    let mut simulation = RigidSimulation::default().with_step(SimStep::export(FIXED_TICK));
    simulation.advance_scene(&scene, PlayedTime::ZERO, 1.0, 0.0).unwrap();
    assert_eq!((simulation.active_copies(0), simulation.active_copies(1)), (3_000, 1_000));
    assert_eq!(simulation.copies_not_made(), 1_000);
    let epoch = simulation.impulse_epoch;
    scene.groups[0] = Some(group(1_000.0));
    simulation.advance_scene(&scene, PlayedTime::from_played(1.0 / 60.0), 1.0, 0.0).unwrap();
    assert_eq!((simulation.active_copies(0), simulation.active_copies(1)), (1_000, 2_000));
    assert_eq!(simulation.copies_not_made(), 0);
    assert_eq!(simulation.impulse_epoch, epoch);
}

#[test]
fn a_held_edit_between_ticks_shows_its_formation_and_lands_on_the_next_tick() {
    let bodies = vec![None; 1];
    let scene = |spacing: f32| copies_scene(&bodies, Some(cube([0.0, 2.0, 0.0], 0.5)), 6.0, spacing, 3.0, 2.0, [0.0; 3], None, &[]);
    let mut simulation = RigidSimulation::default().with_step(SimStep::live(FIXED_TICK));
    simulation.advance_scene(&scene(1.1), PlayedTime::ZERO, 1.0, 0.0).unwrap();
    // Paused between ticks: transport stands at 1.5 ticks.
    simulation.advance_scene(&scene(1.1), PlayedTime::from_played(1.5 / 60.0), 1.0, 0.0).unwrap();
    simulation.advance_scene(&scene(3.0), PlayedTime::from_played(1.5 / 60.0), 0.0, 0.0).unwrap();
    let width = platonic_width(1, 0.5);
    let wide = Arrangement::of(&scene(3.0).groups[0].clone().unwrap(), 6);
    assert_eq!(simulation.group_poses(0)[5].pos, wide.rest(width, 5).position, "shown at once");
    let tight = Arrangement::of(&scene(1.1).groups[0].clone().unwrap(), 6);
    assert_eq!(simulation.groups[0].arrangement, Some(tight), "the bodies wait for the next tick start");
    simulation.advance_scene(&scene(3.0), PlayedTime::from_played(2.5 / 60.0), 1.0, 0.0).unwrap();
    assert_eq!(simulation.groups[0].arrangement, Some(wide));
}

#[test]
fn scatter_unit_is_one_formula_with_the_migration() {
    let width = platonic_width(1, 0.8);
    let unit = spacing_unit(true, width);
    let pile = Arrangement { layout: CopyLayout::Scatter, spacing: 1.0, ..group(CopyLayout::Scatter, 8, 1.0, 16) };
    let pose = pile.rest(width, 1);
    let lattice_x = pile.origin[0] + 0.5 * unit[0];
    assert!((pose.position[0] - lattice_x).abs() <= 0.04 * unit[0] + 1.0e-5);
}

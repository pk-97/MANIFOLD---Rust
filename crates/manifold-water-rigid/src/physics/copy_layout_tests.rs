//! Live copy-group layouts: where copies rest, and how layout, group and
//! Count edits land in a running world.

use super::copy_layout::{Arrangement, CopyLayout, copy_width};
use super::test_scene::copies_scene;
use super::*;
use manifold_core::copies_body::{PLATONIC_HALF_WIDTH, platonic_width, spacing_unit};
use manifold_node_engine::scene::transform::Transform;

const GRAVITY: [f32; 3] = [0.0, -9.81, 0.0];

fn cube(position: [f32; 3], scale: f32) -> RigidBody {
    RigidBody {
        transform: Transform {
            pos: position,
            scale: [scale; 3],
            ..Transform::default()
        },
        ..RigidBody::default()
    }
}

fn group(layout: CopyLayout, count: usize, spacing: f32, columns: usize) -> Arrangement {
    Arrangement {
        layout,
        count,
        columns,
        spacing,
        turn: [0.0; 3],
        origin: [1.0, 2.0, 3.0],
        rotation: [0.0; 3],
    }
}

fn rests(arrangement: &Arrangement, width: [f32; 3]) -> Vec<manifold_core::copy_layout::CopyPose> {
    (0..arrangement.count)
        .map(|index| arrangement.rest(width, index))
        .collect()
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3)
        .map(|axis| (a[axis] - b[axis]).powi(2))
        .sum::<f32>()
        .sqrt()
}

fn centroid(poses: &[manifold_core::copy_layout::CopyPose]) -> [f32; 3] {
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
    let axes = [
        yaw([1.0, 0.0], a),
        yaw([0.0, 1.0], a),
        yaw([1.0, 0.0], b),
        yaw([0.0, 1.0], b),
    ];
    let d = [q[0] - p[0], q[2] - p[2]];
    axes.iter().all(|axis| {
        let project = |angle: f32| {
            let x = yaw([1.0, 0.0], angle);
            let z = yaw([0.0, 1.0], angle);
            half[0] * (x[0] * axis[0] + x[1] * axis[1]).abs()
                + half[1] * (z[0] * axis[0] + z[1] * axis[1]).abs()
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
        let body = RigidBody {
            collider: Some(platonic_solid(shape)),
            ..cube([0.0; 3], 0.8)
        };
        assert_eq!(
            copy_width(&body),
            platonic_width(shape, 0.8),
            "shape {shape}"
        );
        let half = PLATONIC_HALF_WIDTH[shape as usize];
        let max = manifold_node_engine::platonic::platonic_points(shape)
            .iter()
            .map(|p| p[0])
            .fold(f32::MIN, f32::max);
        assert_eq!(max.to_bits(), half.to_bits(), "shape {shape}");
    }
}

/// The rest box is measured in the group's own frame, so a group an LFO or
/// a parent moves or turns every frame never measures it again; a Spacing
/// drag does, on the frame it lands, and the box follows it.
#[test]
fn a_moving_group_never_measures_its_rest_box_again_but_a_spacing_drag_does() {
    let mut body = cube([0.0; 3], 1.0);
    body.spawn = CopySpawn {
        count: 12.0,
        columns: 4.0,
        layout: 1.0,
        ..CopySpawn::default()
    };
    let mut rest = RestBox::default();
    let (_, still) = rest.of(&body);
    for frame in 0..100 {
        let t = frame as f32 / 60.0;
        body.transform.pos = [3.0 * t.sin(), 2.0 * t, -t];
        body.transform.rot_euler = [0.1 * t, 0.7 * t, 0.0];
        assert_eq!(
            rest.of(&body).1,
            still,
            "frame {frame}: the box in the group's frame stays put"
        );
    }
    assert_eq!(rest.measures, 1, "100 frames of motion measured nothing");
    let length =
        |corners: Option<([f32; 3], [f32; 3])>| corners.map_or(0.0, |(lo, hi)| hi[2] - lo[2]);
    for spacing in [1.5, 2.0, 2.5] {
        body.spawn.spacing = spacing;
        let before = length(rest.of(&body).1);
        body.spawn.spacing = spacing + 0.25;
        assert!(
            length(rest.of(&body).1) > before,
            "Spacing {spacing}: the box grows on the frame it changes"
        );
    }
    assert_eq!(rest.measures, 1 + 6, "one measure per Spacing value");
}

/// Line and Grid grow one way from the first copy, which sits on the group's
/// origin, as Blender's Array does: no fill from the centre, and a Count
/// change never moves the copies already there.
#[test]
fn line_and_grid_grow_one_way_from_the_first_copy_spaced_by_the_copy_size() {
    let width = platonic_width(1, 0.5);
    let origin = [1.0, 2.0, 3.0];
    let step = 1.5 * width[0];
    let line = rests(&group(CopyLayout::Line, 5, 1.5, 16), width);
    for (index, pose) in line.iter().enumerate() {
        let want = [origin[0] + index as f32 * step, origin[1], origin[2]];
        assert!(
            distance(pose.position, want) < 1.0e-5,
            "line copy {index} at {:?}, want {want:?}",
            pose.position
        );
    }
    // Columns copies along X, then the next row along Z, then the next layer up.
    let step = [1.1 * width[0], 1.1 * width[1], 1.1 * width[2]];
    let grid = rests(&group(CopyLayout::Grid, 48, 1.1, 4), width);
    for (index, pose) in grid.iter().enumerate() {
        let cell = [index % 4, index / 16, (index / 4) % 4];
        let want: [f32; 3] =
            std::array::from_fn(|axis| origin[axis] + cell[axis] as f32 * step[axis]);
        assert!(
            distance(pose.position, want) < 1.0e-5,
            "grid copy {index} at {:?}, want {want:?}",
            pose.position
        );
    }
}

#[test]
fn a_copy_rests_in_the_same_place_whatever_the_count() {
    let width = platonic_width(1, 0.6);
    for layout in [
        CopyLayout::Grid,
        CopyLayout::Scatter,
        CopyLayout::Line,
        CopyLayout::Spiral,
        CopyLayout::Random,
    ] {
        let few = rests(&group(layout, 10, 1.2, 4), width);
        let many = rests(&group(layout, 1_000, 1.2, 4), width);
        assert_eq!(few[..], many[..10], "{layout:?}");
        // The places are all different.
        for (index, left) in many.iter().enumerate() {
            for right in &many[index + 1..] {
                assert!(
                    distance(left.position, right.position) > 1.0e-3,
                    "{layout:?} doubles a place"
                );
            }
        }
    }
}

/// A Ring spreads its copies evenly round the whole circle, up to Columns a
/// ring, as Cinema 4D's radial Cloner does: Count changes slide them round a
/// ring of the same size, and full rings stay put.
#[test]
fn a_ring_spreads_its_copies_evenly_round_the_whole_circle() {
    let width = platonic_width(1, 0.6);
    let around = |pose: &manifold_core::copy_layout::CopyPose| {
        (pose.position[2] - 3.0).atan2(pose.position[0] - 1.0)
    };
    let full = rests(&group(CopyLayout::Ring, 8, 1.2, 8), width);
    let radius = distance(full[0].position, [1.0, 2.0, 3.0]);
    for count in [1usize, 2, 3, 5, 8] {
        let ring = rests(&group(CopyLayout::Ring, count, 1.2, 8), width);
        for (index, pose) in ring.iter().enumerate() {
            assert!(
                (distance(pose.position, [1.0, 2.0, 3.0]) - radius).abs() < 1.0e-4,
                "{count}: the ring keeps its size"
            );
            let want = std::f32::consts::TAU * index as f32 / count as f32;
            let off = (around(pose) - want).rem_euclid(std::f32::consts::TAU);
            assert!(
                off.min(std::f32::consts::TAU - off) < 1.0e-4,
                "{count}: copy {index} a 1/{count} turn on"
            );
        }
    }
    // Twelve at eight a ring: the first ring full, four spread round the next.
    let tower = rests(&group(CopyLayout::Ring, 12, 1.2, 8), width);
    assert_eq!(tower[..8], full[..], "a full ring stays put");
    let quarter = (around(&tower[9]) - around(&tower[8])).rem_euclid(std::f32::consts::TAU);
    assert!(
        (quarter - std::f32::consts::FRAC_PI_2).abs() < 1.0e-4,
        "four round the second ring: {quarter}"
    );
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
                    // Stacked a whole width apart, they touch.
                    if (left.position[1] - right.position[1]).abs() >= width[1] - 1.0e-5 {
                        continue;
                    }
                    assert!(
                        !boxes_overlap(
                            left.position,
                            yaw_of(left.rotation),
                            right.position,
                            yaw_of(right.rotation),
                            half
                        ),
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
    let poses = rests(&group(CopyLayout::Ring, 2, 1.5, 2), width);
    assert!((distance(poses[0].position, poses[1].position) - step).abs() < 1.0e-5);
    let poses = rests(&group(CopyLayout::Ring, 64, 4.0, 64), width);
    let step = 4.0 * width[0];
    let gap = distance(poses[0].position, poses[1].position);
    assert!(gap >= step - 1.0e-4 && gap < step * 1.05, "{gap} vs {step}");
    assert!(distance(centroid(&poses), [1.0, 2.0, 3.0]) < 1.0e-4);
    // Columns copies make a ring; more stack the next ring a step up.
    let tower = rests(&group(CopyLayout::Ring, 8, 1.5, 4), width);
    assert!((tower[4].position[1] - tower[0].position[1] - 1.5 * width[1]).abs() < 1.0e-5);
    assert!(
        distance(
            [tower[4].position[0], 0.0, tower[4].position[2]],
            [tower[0].position[0], 0.0, tower[0].position[2]]
        ) < 1.0e-5
    );
}

#[test]
fn the_group_transform_turns_and_scales_the_layout_as_one() {
    for layout in [
        CopyLayout::Grid,
        CopyLayout::Scatter,
        CopyLayout::Line,
        CopyLayout::Ring,
        CopyLayout::Spiral,
        CopyLayout::Random,
    ] {
        let flat = group(layout, 12, 1.2, 4);
        let turned = Arrangement {
            rotation: [0.3, -0.7, 0.2],
            ..flat
        };
        let width = platonic_width(1, 0.6);
        let (a, b) = (rests(&flat, width), rests(&turned, width));
        // Pairwise distances are kept: a rigid placement.
        for i in 0..12 {
            for j in i + 1..12 {
                let (before, after) = (
                    distance(a[i].position, a[j].position),
                    distance(b[i].position, b[j].position),
                );
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
    bodies[0] = Some(RigidBody {
        kind: 0,
        ..cube([0.0, -0.5, 0.0], 1.0)
    });
    bodies[0].as_mut().unwrap().transform.scale = [40.0, 1.0, 40.0];
    bodies[1] = Some(cube([6.0, 3.0, 0.0], 0.7));
    let step = if export {
        SimStep::export(FIXED_TICK)
    } else {
        SimStep::live(FIXED_TICK)
    };
    let mut simulation = RigidSimulation::default().with_step(step);
    let mut seen = Vec::new();
    for frame in 0..=2 * fps {
        let time = f64::from(frame) / f64::from(fps);
        let spacing = if time >= 1.0 { 1.6 } else { 1.1 };
        let layout = if time >= 1.0 { 3.0 } else { 0.0 };
        let scene = copies_scene(
            &bodies,
            Some(cube([0.0, 2.0, 0.0], 0.5)),
            16.0,
            spacing,
            4.0,
            layout,
            GRAVITY,
            None,
            &[],
        );
        simulation
            .advance_scene(&scene, PlayedTime::from_played(time), 1.0, 0.0)
            .unwrap();
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
            assert_eq!(
                spacing_edit_run(fps, export),
                reference,
                "fps {fps} export {export}"
            );
        }
    }
    // At one second the copies stand in their new ring, at rest.
    let ring = &reference[6];
    let arrangement = Arrangement {
        layout: CopyLayout::Ring,
        ..group(CopyLayout::Ring, 16, 1.6, 4)
    };
    let arrangement = Arrangement {
        origin: [0.0, 2.0, 0.0],
        ..arrangement
    };
    let width = platonic_width(1, 0.5);
    for (index, pose) in ring[..16].iter().enumerate() {
        assert_eq!(
            pose.pos,
            arrangement.rest(width, index).position,
            "copy {index}"
        );
    }
    assert!(
        reference[5][0].pos[1] < 1.0,
        "the grid had fallen before the edit"
    );
}

/// What [`count_edit_run`] saw: every copy's pose and the first ten
/// copies' velocities at one second, every copy's pose on the edit's frame,
/// the loose cube's pose and the world's epoch.
struct CountEditRun {
    poses: Vec<Transform>,
    velocities: Vec<[f32; 3]>,
    at_edit: Vec<Transform>,
    cube: Transform,
    epoch: Option<u64>,
}

/// A copy group falling free beside a loose cube: Count 10 for half a
/// second, then 1,000 when `grow`.
fn count_edit_run(grow: bool) -> CountEditRun {
    let mut bodies = vec![None; 2];
    bodies[0] = Some(cube([30.0, 4.0, 0.0], 0.5));
    let mut simulation = RigidSimulation::default().with_step(SimStep::export(FIXED_TICK));
    let mut at_edit = Vec::new();
    for frame in 0..=60 {
        let time = f64::from(frame) / 60.0;
        let count = if grow && frame >= 30 { 1_000.0 } else { 10.0 };
        let scene = copies_scene(
            &bodies,
            Some(cube([0.0, 3.0, 0.0], 0.5)),
            count,
            1.1,
            16.0,
            0.0,
            GRAVITY,
            None,
            &[],
        );
        simulation
            .advance_scene(&scene, PlayedTime::from_played(time), 1.0, 0.0)
            .unwrap();
        if frame == 30 {
            at_edit = simulation.group_poses(0).to_vec();
        }
    }
    let world = simulation.native_world().unwrap();
    let velocities = simulation.native_copy_handles(0)[..10]
        .iter()
        .map(|handle| world.linear_velocity(handle.unwrap()).unwrap())
        .collect();
    CountEditRun {
        poses: simulation.group_poses(0).to_vec(),
        velocities,
        at_edit,
        cube: simulation.poses[0],
        epoch: simulation.impulse_epoch,
    }
}

#[test]
fn a_count_edit_keeps_the_copies_moving_and_adds_new_ones_at_rest() {
    let still = count_edit_run(false);
    let grown = count_edit_run(true);
    assert_eq!(grown.epoch, still.epoch, "no new world");
    assert_eq!(grown.poses.len(), 1_000);
    assert!(
        still.velocities.iter().all(|velocity| velocity[1] < -5.0),
        "the ten are falling: {:?}",
        still.velocities
    );
    assert_eq!(
        &grown.poses[..10],
        &still.poses[..10],
        "the first ten keep falling as if Count never moved"
    );
    assert_eq!(grown.velocities, still.velocities);
    assert_eq!(grown.cube, still.cube, "a loose body's motion is untouched");
    let at_edit = grown.at_edit;
    // On the edit's frame the old ten are where their fall had taken them,
    // and the new copies stand at rest in their places.
    let scene = copies_scene(
        &[None, None],
        Some(cube([0.0, 3.0, 0.0], 0.5)),
        1_000.0,
        1.1,
        16.0,
        0.0,
        GRAVITY,
        None,
        &[],
    );
    let arrangement = arrangement_of(&scene.groups[0].clone().unwrap(), 1_000);
    let width = platonic_width(1, 0.5);
    assert_eq!(at_edit.len(), 1_000);
    for (index, pose) in at_edit.iter().enumerate().skip(10) {
        assert_eq!(
            pose.pos,
            arrangement.rest(width, index).position,
            "copy {index}"
        );
    }
    for (index, pose) in at_edit[..10].iter().enumerate() {
        assert!(
            pose.pos[1] < 2.0,
            "copy {index} kept falling: {:?}",
            pose.pos
        );
    }
    // Back down to ten parks the tail; the ten still fall.
    let mut simulation = RigidSimulation::default().with_step(SimStep::export(FIXED_TICK));
    for frame in 0..=40 {
        let count = if (10..20).contains(&frame) {
            1_000.0
        } else {
            10.0
        };
        let scene = copies_scene(
            &[None, None],
            Some(cube([0.0, 3.0, 0.0], 0.5)),
            count,
            1.1,
            16.0,
            0.0,
            GRAVITY,
            None,
            &[],
        );
        simulation
            .advance_scene(
                &scene,
                PlayedTime::from_played(f64::from(frame) / 60.0),
                1.0,
                0.0,
            )
            .unwrap();
    }
    assert_eq!(simulation.active_copies(0), 10);
    // Re-laid at a third of a second they would stand near 2.5 m.
    assert!(
        simulation
            .group_poses(0)
            .iter()
            .all(|pose| pose.pos[1] < 1.5),
        "the ten kept falling through both edits"
    );
}

/// Body copies falling onto a floor; at half a second `edit` changes one
/// layout row. Returns the copies' poses and velocities one tick later.
fn row_edit_run(edit: impl Fn(&mut CopySpawn)) -> (Vec<Transform>, Vec<[f32; 3]>, Arrangement) {
    let mut bodies = vec![None; 1];
    bodies[0] = Some(RigidBody {
        kind: 0,
        ..cube([0.0, -0.5, 0.0], 1.0)
    });
    bodies[0].as_mut().unwrap().transform.scale = [40.0, 1.0, 40.0];
    let mut simulation = RigidSimulation::default().with_step(SimStep::export(FIXED_TICK));
    let mut last = None;
    for frame in 0..=31 {
        let mut scene = copies_scene(
            &bodies,
            Some(cube([0.0, 3.0, 0.0], 0.5)),
            12.0,
            1.1,
            4.0,
            0.0,
            GRAVITY,
            None,
            &[],
        );
        if frame >= 30 {
            edit(&mut scene.groups[0].as_mut().unwrap().spawn);
        }
        simulation
            .advance_scene(
                &scene,
                PlayedTime::from_played(f64::from(frame) / 60.0),
                1.0,
                0.0,
            )
            .unwrap();
        last = scene.groups[0].clone();
    }
    let world = simulation.native_world().unwrap();
    let velocities = simulation.native_copy_handles(0)[..12]
        .iter()
        .map(|handle| world.linear_velocity(handle.unwrap()).unwrap())
        .collect();
    (
        simulation.group_poses(0).to_vec(),
        velocities,
        arrangement_of(&last.unwrap(), 12),
    )
}

/// A layout row's name and its edit.
type RowEdit = (&'static str, fn(&mut CopySpawn));

#[test]
fn layout_spacing_columns_and_turn_re_lay_body_copies_while_dragged() {
    let width = platonic_width(1, 0.5);
    let edits: [RowEdit; 4] = [
        ("layout", |spawn| spawn.layout = 3.0),
        ("spacing", |spawn| spawn.spacing = 2.0),
        ("columns", |spawn| spawn.columns = 3.0),
        ("turn", |spawn| spawn.turn = [0.0, 0.7, 0.0]),
    ];
    for (row, edit) in edits {
        let (poses, velocities, arrangement) = row_edit_run(edit);
        // Half a second in, the copies fall at about 5 m/s, 1.2 m below their
        // places. One tick after the edit they are back in their new places,
        // falling again from rest.
        for (index, pose) in poses.iter().enumerate() {
            let rest = arrangement.rest(width, index).position;
            assert!(
                distance(pose.pos, rest) < 0.01,
                "{row}: copy {index} at {:?}, its place {rest:?}",
                pose.pos
            );
        }
        // Turned copies that overlap their neighbours push apart: an effect.
        assert!(
            velocities
                .iter()
                .all(|velocity| velocity.iter().all(|axis| axis.abs() < 0.5)),
            "{row}: the pile starts again at rest: {velocities:?}"
        );
    }
}

#[test]
fn count_edits_below_the_high_water_mark_allocate_nothing() {
    let bodies = vec![None; 1];
    let mut simulation = RigidSimulation::default().with_step(SimStep::export(FIXED_TICK));
    let scene = |count: f32| {
        copies_scene(
            &bodies,
            Some(cube([0.0, 3.0, 0.0], 0.5)),
            count,
            1.1,
            4.0,
            1.0,
            [0.0; 3],
            None,
            &[],
        )
    };
    simulation
        .advance_scene(&scene(40.0), PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    let state = &simulation.groups[0];
    let capacities = (
        state.handles.capacity(),
        state.poses.capacity(),
        simulation.field_handles.capacity(),
        simulation.authored_bodies.stride(),
    );
    let epoch = simulation.impulse_epoch;
    for (frame, count) in [(1, 12.0), (2, 1.0), (3, 39.0), (4, 40.0), (5, 7.0)] {
        simulation
            .advance_scene(
                &scene(count),
                PlayedTime::from_played(f64::from(frame) / 60.0),
                1.0,
                0.0,
            )
            .unwrap();
        assert_eq!(simulation.active_copies(0), count as usize);
        assert_eq!(simulation.group_poses(0).len(), count as usize);
        let state = &simulation.groups[0];
        assert_eq!(
            (
                state.handles.capacity(),
                state.poses.capacity(),
                simulation.field_handles.capacity(),
                simulation.authored_bodies.stride()
            ),
            capacities,
            "count {count}"
        );
    }
    assert_eq!(simulation.impulse_epoch, epoch, "no rebuild");
    assert_eq!(
        simulation.groups[0].handles.len(),
        40,
        "tail bodies stay installed, disabled"
    );
    // A new high installs only the bodies past the mark.
    simulation
        .advance_scene(&scene(64.0), PlayedTime::from_played(6.0 / 60.0), 1.0, 0.0)
        .unwrap();
    assert_eq!(simulation.active_copies(0), 64);
    assert_eq!(simulation.impulse_epoch, epoch);
}

#[test]
fn reset_returns_the_copies_to_their_layout() {
    let mut bodies = vec![None; 1];
    bodies[0] = Some(RigidBody {
        kind: 0,
        transform: Transform {
            scale: [30.0, 1.0, 30.0],
            pos: [0.0, -0.5, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    let scene = copies_scene(
        &bodies,
        Some(cube([0.0, 2.0, 0.0], 0.5)),
        24.0,
        1.3,
        4.0,
        1.0,
        GRAVITY,
        None,
        &[],
    );
    let mut simulation = RigidSimulation::default();
    simulation
        .advance_scene(&scene, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    let formation = simulation.group_poses(0).to_vec();
    for frame in 1..=90 {
        simulation
            .advance_scene(
                &scene,
                PlayedTime::from_played(f64::from(frame) / 60.0),
                1.0,
                0.0,
            )
            .unwrap();
    }
    assert_ne!(
        simulation.group_poses(0),
        formation.as_slice(),
        "the copies fell"
    );
    simulation
        .advance_scene(&scene, PlayedTime::from_played(1.5), 1.0, 1.0)
        .unwrap();
    assert_eq!(simulation.group_poses(0), formation.as_slice());
}

#[test]
fn a_group_with_physics_off_is_drawn_at_rest_with_no_bodies() {
    let prototype = RigidBody {
        enabled: false,
        ..cube([0.0, 2.0, 0.0], 0.5)
    };
    let scene = |spacing: f32| {
        copies_scene(
            &[None],
            Some(prototype.clone()),
            9.0,
            spacing,
            3.0,
            2.0,
            GRAVITY,
            None,
            &[],
        )
    };
    let mut simulation = RigidSimulation::default();
    for frame in 0..30 {
        simulation
            .advance_scene(
                &scene(1.1),
                PlayedTime::from_played(f64::from(frame) / 60.0),
                1.0,
                0.0,
            )
            .unwrap();
    }
    assert!(simulation.native_copy_handles(0).is_empty());
    assert_eq!(simulation.copies_not_made(), 0);
    let width = platonic_width(1, 0.5);
    let line = arrangement_of(&scene(1.1).groups[0].clone().unwrap(), 9);
    assert_eq!(simulation.group_poses(0).len(), 9);
    for (index, pose) in simulation.group_poses(0).iter().enumerate() {
        assert_eq!(
            pose.pos,
            line.rest(width, index).position,
            "still at rest after half a second"
        );
    }
    simulation
        .advance_scene(&scene(2.0), PlayedTime::from_played(0.5), 1.0, 0.0)
        .unwrap();
    let wider = Arrangement {
        spacing: 2.0,
        ..line
    };
    assert_eq!(
        simulation.group_poses(0)[8].pos,
        wider.rest(width, 8).position,
        "edits show at once"
    );
}

#[test]
fn the_shared_pool_reports_its_shortfall_and_rebalances_on_a_count_edit() {
    let group = |count: f32| RigidBody {
        spawn: CopySpawn {
            count,
            ..CopySpawn::default()
        },
        ..cube([0.0, 50.0, 0.0], 0.2)
    };
    let mut scene = RigidSceneInputs {
        bodies: vec![None],
        groups: vec![
            Some(group((MAX_COPIES - 1_000) as f32)),
            Some(group(2_000.0)),
        ],
        gravity: [0.0; 3],
        ..RigidSceneInputs::default()
    };
    scene.targeted_fields = vec![None; 3];
    let mut simulation = RigidSimulation::default().with_step(SimStep::export(FIXED_TICK));
    simulation
        .advance_scene(&scene, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    assert_eq!(
        (simulation.active_copies(0), simulation.active_copies(1)),
        (MAX_COPIES - 1_000, 1_000)
    );
    assert_eq!(simulation.copies_not_made(), 1_000);
    let epoch = simulation.impulse_epoch;
    scene.groups[0] = Some(group((MAX_COPIES - 3_000) as f32));
    simulation
        .advance_scene(&scene, PlayedTime::from_played(1.0 / 60.0), 1.0, 0.0)
        .unwrap();
    assert_eq!(
        (simulation.active_copies(0), simulation.active_copies(1)),
        (MAX_COPIES - 3_000, 2_000)
    );
    assert_eq!(simulation.copies_not_made(), 0);
    assert_eq!(simulation.impulse_epoch, epoch);
}

#[test]
fn a_held_edit_between_ticks_shows_its_formation_and_lands_on_the_next_tick() {
    let bodies = vec![None; 1];
    let scene = |spacing: f32| {
        copies_scene(
            &bodies,
            Some(cube([0.0, 2.0, 0.0], 0.5)),
            6.0,
            spacing,
            3.0,
            2.0,
            [0.0; 3],
            None,
            &[],
        )
    };
    let mut simulation = RigidSimulation::default().with_step(SimStep::live(FIXED_TICK));
    simulation
        .advance_scene(&scene(1.1), PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    // Paused between ticks: transport stands at 1.5 ticks.
    simulation
        .advance_scene(&scene(1.1), PlayedTime::from_played(1.5 / 60.0), 1.0, 0.0)
        .unwrap();
    simulation
        .advance_scene(&scene(3.0), PlayedTime::from_played(1.5 / 60.0), 0.0, 0.0)
        .unwrap();
    let width = platonic_width(1, 0.5);
    let wide = arrangement_of(&scene(3.0).groups[0].clone().unwrap(), 6);
    assert_eq!(
        simulation.group_poses(0)[5].pos,
        wide.rest(width, 5).position,
        "shown at once"
    );
    let tight = arrangement_of(&scene(1.1).groups[0].clone().unwrap(), 6);
    assert_eq!(
        simulation.groups[0].arrangement,
        Some(tight),
        "the bodies wait for the next tick start"
    );
    simulation
        .advance_scene(&scene(3.0), PlayedTime::from_played(2.5 / 60.0), 1.0, 0.0)
        .unwrap();
    assert_eq!(simulation.groups[0].arrangement, Some(wide));
}

#[test]
fn scatter_unit_is_one_formula_with_the_migration() {
    let width = platonic_width(1, 0.8);
    let unit = spacing_unit(true, width);
    let pile = Arrangement {
        layout: CopyLayout::Scatter,
        spacing: 1.0,
        ..group(CopyLayout::Scatter, 8, 1.0, 16)
    };
    let pose = pile.rest(width, 1);
    let lattice_x = pile.origin[0] + unit[0];
    assert!((pose.position[0] - lattice_x).abs() <= 0.04 * unit[0] + 1.0e-5);
}

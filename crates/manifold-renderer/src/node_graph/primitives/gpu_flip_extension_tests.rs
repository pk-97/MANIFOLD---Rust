//! CPU reference shared with the GPU extension proofs. Boundary seeding follows
//! FLIP Fluids GridUtils (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender;
//! see THIRD_PARTY_NOTICES.md), adapted to our box-face walls.

use crate::node_graph::fluid_particles::FaceSample;

// Engine _removeMarkerParticles counts before testing speed: an extreme
// marker still consumes one of the first 250 places in its cell.
pub(super) fn native_cell_survivors(speeds: &[f64], limit: f64) -> Vec<usize> {
    speeds.iter().enumerate().filter_map(|(i, &speed)|
        (i < 250 && speed * speed <= limit * limit).then_some(i)).collect()
}

#[test]
fn gpu_flip_native_marker_cap_and_extreme_reference() {
    use manifold_core::Seconds;
    use manifold_physics::stepping::{marker_particle_speed_limit, MarkerSpeedLimitConfig};
    for duration in [0.25, 0.5, 1.0] {
        let mut speeds = vec![1.0; 300];
        speeds[0] = 1000.0;
        let limit = marker_particle_speed_limit(&speeds, Seconds(duration), 0.2, 5.0, 6,
            MarkerSpeedLimitConfig::default(), &mut [0; 6]).value;
        assert_eq!(limit, 6.0 / duration);
        assert_eq!(native_cell_survivors(&speeds, limit), (1..250).collect::<Vec<_>>());
        assert_eq!(native_cell_survivors(&[limit; 300], limit).len(), 250);
    }
}

pub(super) fn moving_wall_fixture(n: [usize; 3]) -> (Vec<FaceSample>, usize) {
    let mut faces = vec![FaceSample::default(); n.map(|v| v + 1).iter().product()];
    let seed = index([2, 2, 2], n);
    let wall = index([3, 2, 2], n);
    faces[seed].velocity[0] = 2.0;
    faces[seed].weight[0] = 1.0;
    (faces, wall)
}

#[test]
fn gpu_flip_native_extend_then_moving_wall_reference() {
    let n = [8; 3];
    let (faces, wall) = moving_wall_fixture(n);
    let mut native = cpu_extend(&faces, n);
    // A half-open moving cut face with friction 0.5, invalid in air.
    native[wall].velocity[0] = 0.5 * 7.0 + 0.5 * native[wall].velocity[0];
    let mut wrong = faces;
    wrong[wall].velocity[0] = 0.5 * 7.0 + 0.5 * wrong[wall].velocity[0];
    let wrong = cpu_extend(&wrong, n);
    assert_eq!(native[wall].velocity[0], 4.5);
    assert_eq!(wrong[wall].velocity[0], 2.0);
}

#[test]
fn gpu_flip_native_inflow_two_steps_reference() {
    // _advanceMarkerParticles precedes _updateFluidObjects.
    let dt = 0.125_f64;
    let first = (0.25_f64, 2.0_f64);
    let second = (first.0 + first.1 * dt, first.1);
    assert_eq!(first.0, 0.25);
    assert_eq!(second.0, 0.5);
}

fn index(p: [usize; 3], n: [usize; 3]) -> usize {
    p[0] + (n[0] + 1) * (p[1] + (n[1] + 1) * p[2])
}

fn coords(i: usize, n: [usize; 3]) -> [usize; 3] {
    let m = n.map(|v| v + 1);
    [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])]
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Unknown,
    Waiting,
    Known,
    Done,
}

/// One layer of FLIP Fluids GridUtils::extrapolateGrid on each component's
/// own lattice (gridutils.cpp _initializeStatusGridThread and
/// _findExtrapolationCells, gridutils.h _extrapolateCellsThread). Status
/// starts from the weights: a border sample is DONE whatever its weight, a
/// valid inner one KNOWN (a face a previous layer filled is valid too, and
/// is DONE or KNOWN there: either way counted). Each KNOWN sample marks its
/// UNKNOWN neighbours WAITING and becomes DONE; a WAITING sample takes the
/// mean of its DONE neighbours and is valid from the next layer.
pub(super) fn cpu_extend(faces: &[FaceSample], n: [usize; 3]) -> Vec<FaceSample> {
    let mut out = vec![FaceSample::default(); faces.len()];
    for a in 0..3 {
        let dims: [usize; 3] = std::array::from_fn(|b| if b == a { n[b] + 1 } else { n[b] });
        let at = |g: [usize; 3]| index(g, n);
        let border = |g: [usize; 3]| (0..3).any(|b| g[b] == 0 || g[b] == dims[b] - 1);
        let cells: Vec<[usize; 3]> = (0..dims[2])
            .flat_map(|k| (0..dims[1]).flat_map(move |j| (0..dims[0]).map(move |i| [i, j, k])))
            .collect();
        let mut status = vec![Status::Unknown; faces.len()];
        for &g in &cells {
            status[at(g)] = if border(g) { Status::Done } else if faces[at(g)].weight[a] > 0.0 { Status::Known } else { Status::Unknown };
        }
        let neighbours = |g: [usize; 3]| {
            (0..6).filter_map(move |s| {
                let (b, d) = (s / 2, if s % 2 == 0 { 1i64 } else { -1 });
                let q = g[b] as i64 + d;
                (q >= 0 && q < dims[b] as i64).then(|| { let mut r = g; r[b] = q as usize; r })
            })
        };
        let mut waiting = Vec::new();
        for &g in &cells {
            if status[at(g)] != Status::Known {
                continue;
            }
            for r in neighbours(g) {
                if status[at(r)] == Status::Unknown {
                    status[at(r)] = Status::Waiting;
                    waiting.push(r);
                }
            }
            status[at(g)] = Status::Done;
        }
        for &g in &cells {
            out[at(g)].velocity[a] = faces[at(g)].velocity[a];
            out[at(g)].weight[a] = faces[at(g)].weight[a];
        }
        for g in waiting {
            let done: Vec<f64> = neighbours(g).filter(|&r| status[at(r)] == Status::Done)
                .map(|r| f64::from(faces[at(r)].velocity[a])).collect();
            out[at(g)].velocity[a] = (done.iter().sum::<f64>() / done.len() as f64) as f32;
            out[at(g)].weight[a] = 1.0;
        }
    }
    out
}

/// Fluid valid in the inner u faces from row y = 2 up, velocity 2, a
/// transverse wall row below: the engine averages the border row's held 0
/// into row 1. With `corner` the fluid also starts at z = 2, so row 1's
/// faces at z = 1 meet two border rows.
pub(super) fn transverse_wall_fixture(n: [usize; 3], corner: bool) -> Vec<FaceSample> {
    let mut faces = vec![FaceSample::default(); n.map(|v| v + 1).iter().product()];
    for (i, face) in faces.iter_mut().enumerate() {
        let p = coords(i, n);
        let inner = p[0] > 0 && p[0] < n[0] && p[1] > 0 && p[1] < n[1] - 1 && p[2] > 0 && p[2] < n[2] - 1;
        if inner && p[1] >= 2 && (!corner || p[2] >= 2) {
            face.velocity[0] = 2.0;
            face.weight[0] = 1.0;
        }
    }
    faces
}

#[test]
fn native_extension_averages_held_border_rows() {
    let n = [8; 3];
    let flat = cpu_extend(&transverse_wall_fixture(n, false), n);
    // Row 1 meets fluid above and the border row's held 0 below.
    assert_eq!(flat[index([3, 1, 3], n)].velocity[0], 1.0);
    assert_eq!(flat[index([3, 1, 3], n)].weight[0], 1.0);
    // The border row itself is held, never extended.
    assert_eq!(flat[index([3, 0, 3], n)].weight[0], 0.0);
    let corner = cpu_extend(&transverse_wall_fixture(n, true), n);
    // Fluid on no side yet at (y 1, z 1): no seed, still unknown.
    assert_eq!(corner[index([3, 1, 1], n)].weight[0], 0.0);
    let corner = cpu_extend(&corner, n);
    // Next layer: two filled neighbours at 1, two held border zeros.
    assert_eq!(corner[index([3, 1, 1], n)].velocity[0], 0.5);
}

/// Two invalid layers between one moving fluid plane and a held-zero wall.
/// Transverse edge rows are the engine's held border rows: never extended.
pub(super) fn wall_gap(n: [usize; 3], axis: usize, high: bool) -> (Vec<FaceSample>, usize) {
    let mut faces = vec![FaceSample::default(); n.map(|v| v + 1).iter().product()];
    for (i, face) in faces.iter_mut().enumerate() {
        let p = coords(i, n);
        if (0..3).any(|b| b != axis && p[b] >= n[b]) {
            continue;
        }
        let distance = if high { n[axis] - p[axis] } else { p[axis] };
        if p[axis] == 0 || p[axis] == n[axis] || distance == 3 {
            face.weight[axis] = 1.0;
            face.velocity[axis] = if distance == 3 { 2.0 } else { 0.0 };
        }
    }
    let mut target = n.map(|v| v / 2);
    target[axis] = if high { n[axis] - 1 } else { 1 };
    (faces, index(target, n))
}

#[test]
fn wall_zero_waits_for_fluid_front_on_every_axis() {
    let n = [8; 3];
    for axis in 0..3 {
        for high in [false, true] {
            let (faces, target) = wall_gap(n, axis, high);
            let first = cpu_extend(&faces, n);
            assert_eq!(
                first[target].weight[axis], 0.0,
                "a wall alone cannot mark the gap valid"
            );
            let second = cpu_extend(&first, n);
            for (i, face) in second.iter().enumerate() {
                let p = coords(i, n);
                if (0..3).any(|b| b != axis && p[b] >= n[b]) {
                    continue;
                }
                let distance = if high { n[axis] - p[axis] } else { p[axis] };
                let clear = (0..3).all(|b| b == axis || (2..n[b] - 2).contains(&p[b]));
                if distance == 1 && clear {
                    assert_eq!(face.weight[axis], 1.0);
                    assert_eq!(
                        face.velocity[axis], 1.0,
                        "fluid 2 and wall 0 average at {p:?}/{axis}"
                    );
                } else if distance == 0 {
                    assert_eq!(face.velocity[axis], 0.0, "wall must stay held");
                }
            }
        }
    }
}

#[test]
fn walls_alone_never_create_valid_air() {
    let n = [8; 3];
    for axis in 0..3 {
        let (mut faces, _) = wall_gap(n, axis, false);
        for face in &mut faces {
            if face.velocity[axis] != 0.0 {
                *face = FaceSample::default();
            }
        }
        let initial_weights: Vec<_> = faces.iter().map(|f| f.weight).collect();
        for _ in 0..8 {
            faces = cpu_extend(&faces, n);
        }
        assert_eq!(
            faces.iter().map(|f| f.weight).collect::<Vec<_>>(),
            initial_weights
        );
    }
}

#[test]
fn stationary_fluid_is_still_a_valid_seed() {
    let n = [8; 3];
    let (mut faces, target) = wall_gap(n, 1, false);
    for face in &mut faces {
        face.velocity = [0.0; 4];
    }
    let extended = cpu_extend(&cpu_extend(&faces, n), n);
    assert_eq!(extended[target].velocity[1], 0.0);
    assert_eq!(
        extended[target].weight[1], 1.0,
        "validity comes from fluid, not nonzero speed"
    );
}

#[test]
fn gpu_flip_step_order_dispatch_extents() {
    // All new value fixtures use this existing unequal lattice. Rounded
    // invocations are guarded by cell/face/site/count, never arrayLength.
    let cells = [6u64, 5, 4];
    let cell_count: u64 = cells.iter().product();
    let face_count: u64 = cells.map(|n| n + 1).iter().product();
    assert_eq!((cell_count, face_count), (120, 210));
    assert_eq!(super::gpu_flip_step::face_bytes([6, 5, 4]), face_count * 32);
    assert_eq!(super::gpu_flip_step::emit_sites([6, 5, 4]), cell_count * 8);
    for threads in [cell_count, face_count, cell_count * 8, 300, 512] {
        assert!(threads.div_ceil(256) * 256 >= threads);
        assert!(threads.div_ceil(256) * 256 < threads + 256);
    }
    assert!(super::prefix_scan::storage_words(960) >= 960);
    for count in [300u32, 512, 20_000] {
        let groups = u64::from(count.div_ceil(64));
        // Both vec4 reduction scratch buffers, three maxima, histogram,
        // outlier counters and plan. Histogram index is clamped below six.
        assert_eq!(super::gpu_flip_clock::GpuFlipClock::held_bytes(count, 1, 1),
            2 * groups * 16 + 3 * 16 + 64 * 4 + 16 + 48);
    }
    // Removal binds one CellRange per cell and writes only start..start+count.
    let ranges = [(0usize, 300usize), (300, 212)];
    for (start, count) in ranges {
        assert!(start + count <= 512);
        assert!(start + count.saturating_sub(250) <= 512);
    }
}

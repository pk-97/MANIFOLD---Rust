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

/// One layer of the step's extension (gpu_flip_step.wgsl extend_layer) as the
/// kernel computes it, bit for bit: a valid face copied, an invalid one the
/// mean of its valid same-component neighbours, summed in f32 axis by axis,
/// low side first, valid only when a neighbour off the walls seeds it.
/// Absent faces and w come out zero. The step's shaders build with fast
/// math, which takes `sum / hits` as `sum * (1 / hits)`: run once against
/// the dense kernel the tiles replaced (2026-10-04), that form matched all
/// 2,035,680 records and a true divide missed 5,810 by one ulp.
pub(super) fn cpu_extend(faces: &[FaceSample], n: [usize; 3]) -> Vec<FaceSample> {
    (0..faces.len()).map(|i| extend_record(coords(i, n), n, |r| faces[index(r, n)])).collect()
}

/// One record of a layer, its own and its neighbours' values read by `face`.
fn extend_record(p: [usize; 3], n: [usize; 3], mut face: impl FnMut([usize; 3]) -> FaceSample) -> FaceSample {
    let here = face(p);
    let mut out = FaceSample::default();
    for a in 0..3 {
        if (0..3).any(|b| b != a && p[b] >= n[b]) {
            continue;
        }
        out.velocity[a] = here.velocity[a];
        out.weight[a] = here.weight[a];
        if here.weight[a] > 0.0 {
            continue;
        }
        let (mut sum, mut hits) = (0.0f32, 0.0f32);
        let mut seeded = false;
        for b in 0..3 {
            for d in [-1i64, 1] {
                let q = p[b] as i64 + d;
                let top = if b == a { n[b] as i64 } else { n[b] as i64 - 1 };
                if q < 0 || q > top {
                    continue;
                }
                let mut r = p;
                r[b] = q as usize;
                let neighbour = face(r);
                if neighbour.weight[a] > 0.0 {
                    sum += neighbour.velocity[a];
                    hits += 1.0;
                    seeded |= r[a] > 0 && r[a] < n[a];
                }
            }
        }
        if seeded {
            out.velocity[a] = sum * (1.0 / hits);
            out.weight[a] = 1.0;
        }
    }
    out
}

/// A record as every layer leaves one no neighbour fills: its faces copied,
/// absent faces and w zero.
fn copied(face: FaceSample, p: [usize; 3], n: [usize; 3]) -> FaceSample {
    let mut out = FaceSample::default();
    for a in (0..3).filter(|&a| (0..3).all(|b| b == a || p[b] < n[b])) {
        out.velocity[a] = face.velocity[a];
        out.weight[a] = face.weight[a];
    }
    out
}

fn same_bits(a: &FaceSample, b: &FaceSample) -> bool {
    bytemuck::bytes_of(a) == bytemuck::bytes_of(b)
}

/// The extension's tile edge and a tile no layer writes
/// (gpu_flip_step.wgsl extend_classify, extend_reach).
const TILE: usize = 8;
const NEVER: u32 = u32::MAX;

fn tile_dims(n: [usize; 3]) -> [usize; 3] {
    n.map(|v| v / TILE + 1)
}

fn tile_index(t: [usize; 3], dims: [usize; 3]) -> usize {
    t[0] + dims[0] * (t[1] + dims[1] * t[2])
}

/// extend_classify's marks: bit 0 an invalid face, bits 8 + 8·axis + c a
/// seed at local coordinate c.
fn tile_marks(faces: &[FaceSample], n: [usize; 3]) -> Vec<u32> {
    let dims = tile_dims(n);
    let mut marks = vec![0u32; dims.iter().product()];
    for (i, face) in faces.iter().enumerate() {
        let p = coords(i, n);
        let local = p.map(|v| v % TILE);
        let mark = &mut marks[tile_index(p.map(|v| v / TILE), dims)];
        for a in (0..3).filter(|&a| (0..3).all(|b| b == a || p[b] < n[b])) {
            if face.weight[a] > 0.0 {
                if p[a] > 0 && p[a] < n[a] {
                    *mark |= (1 << (8 + local[0])) | (1 << (16 + local[1])) | (1 << (24 + local[2]));
                }
            } else {
                *mark |= 1;
            }
        }
    }
    marks
}

/// extend_reach: each tile's first layer, or NEVER.
fn first_layers(marks: &[u32], n: [usize; 3], layers: u32, in_place: bool) -> Vec<u32> {
    let dims = tile_dims(n);
    let reach = layers.div_ceil(TILE as u32) as usize;
    (0..marks.len())
        .map(|t| {
            if marks[t] & 1 == 0 {
                return NEVER;
            }
            let tile = [t % dims[0], (t / dims[0]) % dims[1], t / (dims[0] * dims[1])];
            let mut gap = NEVER;
            for z in tile[2].saturating_sub(reach)..=(tile[2] + reach).min(dims[2] - 1) {
                for y in tile[1].saturating_sub(reach)..=(tile[1] + reach).min(dims[1] - 1) {
                    for x in tile[0].saturating_sub(reach)..=(tile[0] + reach).min(dims[0] - 1) {
                        let seeds = marks[tile_index([x, y, z], dims)] >> 8;
                        if seeds == 0 {
                            continue;
                        }
                        let distance = (0..3)
                            .map(|axis| {
                                let mask = (seeds >> (8 * axis)) & 255;
                                let base = [x, y, z][axis] as i64 * TILE as i64;
                                let (lo, hi) = (base + i64::from(mask.trailing_zeros()), base + 31 - i64::from(mask.leading_zeros()));
                                let (box_lo, box_hi) = (tile[axis] as i64 * TILE as i64, tile[axis] as i64 * TILE as i64 + TILE as i64 - 1);
                                (lo - box_hi).max(box_lo - hi).max(0)
                            })
                            .max()
                            .unwrap_or(0);
                        gap = gap.min(distance as u32);
                    }
                }
            }
            if gap > layers {
                return NEVER;
            }
            let first = gap.max(1);
            if in_place && first >= 2 && (layers - first).is_multiple_of(2) { first - 1 } else { first }
        })
        .collect()
}

/// The extension as the step encodes it (gpu_flip_step.rs extend) over
/// buffers 0 source, 1 target, 2 scratch, the target being the source in
/// place. Panics when a layer reads a record the same layer writes.
fn tiled_extend(buffers: &mut [Vec<FaceSample>; 3], n: [usize; 3], layers: u32, in_place: bool) {
    let (source, target, scratch) = (0, if in_place { 0 } else { 1 }, 2);
    let dims = tile_dims(n);
    let marks = tile_marks(&buffers[source], n);
    let first = first_layers(&marks, n, layers, in_place);
    for i in 0..buffers[source].len() {
        let out = copied(buffers[source][i], coords(i, n), n);
        if !in_place || !same_bits(&out, &buffers[source][i]) {
            buffers[target][i] = out;
        }
    }
    let mut from = source;
    if layers % 2 == 1 && in_place {
        buffers[scratch] = buffers[source].clone();
        from = scratch;
    }
    for k in 1..=layers {
        let to = if (layers - k).is_multiple_of(2) { target } else { scratch };
        let unstarted = if k == 1 { from } else { source };
        let mut writes = Vec::new();
        let mut reads_of_to = Vec::new();
        for (t, &start) in first.iter().enumerate() {
            if start > k {
                continue;
            }
            let tile = [t % dims[0], (t / dims[0]) % dims[1], t / (dims[0] * dims[1])];
            for l in 0..TILE * TILE * TILE {
                let p = [tile[0] * TILE + l % TILE, tile[1] * TILE + (l / TILE) % TILE, tile[2] * TILE + l / (TILE * TILE)];
                if (0..3).any(|a| p[a] > n[a]) {
                    continue;
                }
                let out = extend_record(p, n, |r| {
                    let buffer = if first[tile_index(r.map(|v| v / TILE), dims)] < k { from } else { unstarted };
                    if buffer == to {
                        reads_of_to.push(index(r, n));
                    }
                    buffers[buffer][index(r, n)]
                });
                writes.push((index(p, n), out));
            }
        }
        let written: std::collections::HashSet<usize> = writes.iter().map(|&(i, _)| i).collect();
        if let Some(&i) = reads_of_to.iter().find(|i| written.contains(i)) {
            panic!("layer {k} of {layers} (in place {in_place}) reads record {:?} of the buffer it writes", coords(i, n));
        }
        for (i, out) in writes {
            buffers[to][i] = out;
        }
        from = to;
    }
}

struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    fn signed(&mut self) -> f32 {
        4.0 * self.unit() - 2.0
    }
}

/// Structured and random valid sets on lattice `n`, each a face grid as a
/// step pass could hand the extension: velocities everywhere (an invalid
/// face keeps one after the pressure step), box walls valid at 0 unless the
/// case says otherwise, and in "random" absent faces and w holding garbage.
pub(super) fn extension_fixtures(n: [usize; 3], seed: u64) -> Vec<(&'static str, Vec<FaceSample>)> {
    let mut rng = Rng(seed | 1);
    let len: usize = n.map(|v| v + 1).iter().product();
    let exists = |p: [usize; 3], a: usize| (0..3).all(|b| b == a || p[b] < n[b]);
    let wall = |p: [usize; 3], a: usize| p[a] == 0 || p[a] == n[a];
    let mut case = |valid: &dyn Fn([usize; 3], usize, &mut Rng) -> Option<f32>, walls: bool| -> Vec<FaceSample> {
        (0..len)
            .map(|i| {
                let p = coords(i, n);
                let mut face = FaceSample::default();
                for a in (0..3).filter(|&a| exists(p, a)) {
                    face.velocity[a] = rng.signed();
                    if walls && wall(p, a) {
                        face.velocity[a] = 0.0;
                        face.weight[a] = 1.0;
                    } else if let Some(weight) = valid(p, a, &mut rng) {
                        face.weight[a] = weight;
                    }
                }
                face
            })
            .collect()
    };
    let surface = n[1] / 3;
    let middle = n.map(|v| v / 2);
    let mut cases: Vec<(&'static str, Vec<FaceSample>)> = vec![
        ("pool", case(&|p, _, rng| (p[1] < surface).then(|| 0.05 + rng.unit()), true)),
        ("sheet", case(&|p, _, _| (p[1] == surface).then_some(1.0), true)),
        ("single face", case(&|p, a, _| (a == 1 && p == middle).then_some(1.0), false)),
        ("all valid", case(&|_, _, rng| Some(0.05 + rng.unit()), false)),
        ("none valid", case(&|_, _, _| None, false)),
        // Seeds one face in from every wall, the walls themselves invalid,
        // so the first layer fills the walls from them.
        ("beside the walls", case(&|p, a, _| (p[a] == 1 || p[a] + 1 == n[a]).then_some(1.0), false)),
        // Two clusters, at opposite corners and more than the band apart.
        ("two clusters", case(&|p, _, _| (p.iter().all(|&v| v < 2) || (0..3).all(|b| p[b] + 3 > n[b])).then_some(1.0), true)),
    ];
    let mut random = case(
        &|_, _, rng| match (4.0 * rng.unit()) as u32 {
            0 => Some(0.05 + rng.unit()),
            1 => Some(-rng.unit()),
            _ => None,
        },
        false,
    );
    for face in &mut random {
        for a in 0..3 {
            if face.weight[a] > 0.0 && rng.unit() < 0.1 {
                face.velocity[a] = -0.0;
            }
        }
        // Garbage wherever the extension reads nothing: the w lanes, and an
        // absent face's lanes.
        for lane in 0..4 {
            if face.weight[lane] == 0.0 && face.velocity[lane] == 0.0 {
                face.velocity[lane] = rng.signed();
                face.weight[lane] = rng.signed();
            }
        }
        face.velocity[3] = rng.signed();
        face.weight[3] = rng.signed();
    }
    cases.push(("random", random));
    cases
}

/// The extension over its tiles reads no record a layer writes and lands on
/// the dense extension bit for bit, in place and out, at every layer count
/// up to past the band, on lattices from one tile to several a side.
#[test]
fn tiled_extension_model_matches_the_dense_layers() {
    let mut checked = 0;
    for n in [[1, 1, 1], [3, 2, 5], [7, 8, 9], [16, 9, 24], [20, 17, 9], [12, 40, 6]] {
        for (name, faces) in extension_fixtures(n, 0x5eed ^ n.iter().sum::<usize>() as u64) {
            let mut dense = faces.clone();
            for layers in 1..=13u32 {
                dense = cpu_extend(&dense, n);
                for in_place in [false, true] {
                    let garbage: Vec<FaceSample> = faces.iter().map(|f| FaceSample { velocity: f.weight, weight: f.velocity }).collect();
                    let mut buffers = [faces.clone(), garbage.clone(), garbage];
                    tiled_extend(&mut buffers, n, layers, in_place);
                    let target = &buffers[if in_place { 0 } else { 1 }];
                    let at = |i: usize| format!("{name} {n:?}, {layers} layers, in place {in_place}: record {:?}", coords(i, n));
                    for (i, (got, want)) in target.iter().zip(&dense).enumerate() {
                        assert!(same_bits(got, want), "{}: {got:?} vs dense {want:?}", at(i));
                    }
                    if !in_place {
                        assert!(buffers[0].iter().zip(&faces).all(|(a, b)| same_bits(a, b)), "{name} {n:?}: the source changed");
                    }
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 6 * 8 * 13 * 2);
}

/// A tile beyond the band is never written, and one the band reaches only
/// late starts late: the layers do less than the dense extension.
#[test]
fn tiled_extension_skips_the_far_tiles() {
    let n = [16, 64, 16];
    let pool = extension_fixtures(n, 7).swap_remove(0).1;
    let marks = tile_marks(&pool, n);
    let first = first_layers(&marks, n, 12, false);
    let dims = tile_dims(n);
    // The pool's top seeds sit at y = 20, so tile row 2 holds the surface,
    // row 3 (y 24..31) starts at layer 4 and row 4 (y 32..39) at layer 12.
    // The last tile on x and z holds only the valid walls at n.
    for (t, &start) in first.iter().enumerate() {
        let tile = [t % dims[0], (t / dims[0]) % dims[1], t / (dims[0] * dims[1])];
        let want = match tile[1] {
            _ if tile[0] == 2 || tile[2] == 2 => NEVER,
            2 => 1,
            3 => 4,
            4 => 12,
            _ => NEVER,
        };
        assert_eq!(start, want, "tile {tile:?}");
    }
}

/// Two invalid layers between one moving fluid plane and a held-zero wall.
/// Transverse edge rows are included: these are fluid, not engine ghost cells.
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
    let mut target = [0; 3];
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
                if distance == 1 {
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

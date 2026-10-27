//! GPU proofs for `node.surface_weights` against a brute-force CPU reference,
//! on world tiles. Each box of nodes is a pooled field under the table of the
//! tiles it touches, its first node at world node `origin` below zero on x so
//! the box straddles tiles -1 and 0; the particles are sorted into the display
//! cells on the CPU, as `node.sort_particles_into_cells` does over the same
//! table.

use super::*;
use manifold_node_engine::scene::scene_object::FIELD_FAR_CELLS;
use manifold_node_engine::scene::world_tiles::pool_len;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::testkit::world_box::BoxTable;

/// The display cell, and the simulation cell the density is given per.
const H: f32 = 0.0625;

/// A box of nodes in world nodes, and the particle density per simulation cell.
struct Case {
    origin: [i32; 3],
    dims: [u32; 3],
    particles_per_cell: f32,
}

struct Outcome {
    uniforms: WeightUniforms,
    sorted: Vec<FluidParticle>,
    counts: Vec<f32>,
    /// Per box node, x fastest.
    weights: Vec<f32>,
    velocity: Vec<[f32; 3]>,
}

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / (1u32 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
}

fn particle(position: [f32; 3], velocity: [f32; 3], id: u32) -> FluidParticle {
    FluidParticle {
        position_radius: [position[0], position[1], position[2], 0.01],
        velocity,
        id,
    }
}

impl Case {
    fn node_count(&self) -> usize {
        self.dims.iter().map(|&n| n as usize).product()
    }

    /// Box node `ijk` in metres: world node p is at (p - 1/2) cells.
    fn position(&self, ijk: [u32; 3]) -> [f32; 3] {
        std::array::from_fn(|a| ((self.origin[a] + ijk[a] as i32) as f32 - 0.5) * H)
    }

    fn ijk(&self, n: usize) -> [u32; 3] {
        let n = n as u32;
        [
            n % self.dims[0],
            n / self.dims[0] % self.dims[1],
            n / (self.dims[0] * self.dims[1]),
        ]
    }
}

/// Bin and order the particles as the pooled `node.sort_particles_into_cells`
/// does: a particle's cell is `floor((p + h/2) * (1 / h))`, its pool index the
/// table's, input order kept within a cell, unused records past the live ones
/// and in no range.
fn sort_on_cpu(
    particles: &[FluidParticle],
    table: &BoxTable,
) -> (Vec<FluidParticle>, Vec<CellRange>) {
    let inv = 1.0 / H;
    let key = |p: &FluidParticle| {
        let cell: [u32; 3] = std::array::from_fn(|a| {
            let c = ((p.position_radius[a] + 0.5 * H) * inv).floor() as i32 - table.origin[a];
            assert!(
                (0..table.cells[a] as i32).contains(&c),
                "a particle lies outside the box"
            );
            c as u32
        });
        table.pool_index(cell)
    };
    let (live, unused): (Vec<_>, Vec<_>) = particles
        .iter()
        .copied()
        .partition(|p| p.position_radius[3] > 0.0);
    let mut order: Vec<usize> = (0..live.len()).collect();
    order.sort_by_key(|&i| key(&live[i]));
    let mut sorted: Vec<FluidParticle> = order.iter().map(|&i| live[i]).collect();
    let mut ranges = vec![CellRange::default(); pool_len(table.pool_tiles) as usize];
    for (slot, p) in sorted.iter().enumerate() {
        let range = &mut ranges[key(p)];
        if range.count == 0 {
            range.start = slot as u32;
        }
        range.count += 1;
    }
    sorted.extend(unused);
    (sorted, ranges)
}

/// `node.surface_weights` over `particles` and the box's `reference`
/// distances; the box's weights and velocities read back through the table.
fn run_case(case: &Case, particles: &[FluidParticle], reference: &[f32]) -> Outcome {
    assert_eq!(reference.len(), case.node_count());
    let table = BoxTable::new(case.origin, case.dims);
    let (sorted, ranges) = sort_on_cpu(particles, &table);
    let len = pool_len(table.pool_tiles) as usize;
    let uniforms = weight_uniforms(
        TilePool {
            pool_tiles: table.pool_tiles,
            cell_size: H,
        },
        case.particles_per_cell,
        H,
        sorted.len() as u32,
    )
    .unwrap();

    let mut harness = Harness::new();
    let mut node = SurfaceWeights::new();
    let (particle_slot, _) = harness.array(&sorted, sorted.len());
    let (range_slot, _) = harness.array(&ranges, ranges.len());
    let far = FIELD_FAR_CELLS * H;
    let (reference_slot, _) = harness.array(&table.to_pool(reference, far), len);
    let (tiles_slot, _) = harness.array(&table.words, table.words.len());
    let (weights_slot, _) = harness.array::<f32>(&[], 1);
    let (velocity_slot, _) = harness.array::<f32>(&[], 1);
    let run_params = params(&[
        ("cell_size", H),
        ("sim_cell_size", H),
        ("particles_per_cell", case.particles_per_cell),
        ("pool_tiles", table.pool_tiles as f32),
    ]);
    let (_, errors) = harness.run(
        &mut node,
        &[
            ("particles", particle_slot),
            ("cell_ranges", range_slot),
            ("reference", reference_slot),
            ("tiles", tiles_slot),
        ],
        &[("weights", weights_slot), ("velocity", velocity_slot)],
        &run_params,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let weights: Vec<f32> = read(&harness.buffer(weights_slot), len);
    let velocity: Vec<[f32; 3]> = read::<f32>(&harness.buffer(velocity_slot), 3 * len)
        .chunks_exact(3)
        .map(|v| [v[0], v[1], v[2]])
        .collect();
    Outcome {
        uniforms,
        counts: read(node.counts.as_ref().expect("counts buffer"), sorted.len()),
        weights: table.from_pool(&weights),
        velocity: table.from_pool(&velocity),
        sorted,
    }
}

struct Expected {
    counts: Vec<f32>,
    weights: Vec<f32>,
    velocity: Vec<[f32; 3]>,
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn offset(from: [f32; 3], to: [f32; 3]) -> [f32; 3] {
    [to[0] - from[0], to[1] - from[1], to[2] - from[2]]
}

/// Brute force: every particle against every particle, every box node against
/// every particle, the same formulas as the shader.
fn cpu_reference(
    case: &Case,
    particles: &[FluidParticle],
    reference: &[f32],
    u: &WeightUniforms,
) -> Expected {
    let position = |p: &FluidParticle| {
        [
            p.position_radius[0],
            p.position_radius[1],
            p.position_radius[2],
        ]
    };
    let live = |p: &FluidParticle| p.position_radius[3] > 0.0;
    let count_reach2 = u.count_reach * u.count_reach;
    let counts: Vec<f32> = particles
        .iter()
        .enumerate()
        .map(|(k, pk)| {
            if !live(pk) {
                return 0.0;
            }
            particles
                .iter()
                .enumerate()
                .filter(|&(j, pj)| {
                    j != k && live(pj) && {
                        let d = offset(position(pk), position(pj));
                        dot(d, d) < count_reach2
                    }
                })
                .count() as f32
        })
        .collect();

    let carry2 = u.carry_reach * u.carry_reach;
    let n_total = case.node_count();
    let mut weights = vec![0.0; n_total];
    let mut velocity = vec![[0.0; 3]; n_total];
    for n in 0..n_total {
        if reference[n].abs() >= u.band {
            continue;
        }
        let at = case.position(case.ijk(n));
        let (mut sw, mut sn, mut sv) = (0.0f64, 0.0f64, [0.0f64; 3]);
        for (k, p) in particles.iter().enumerate() {
            if !live(p) {
                continue;
            }
            let d = offset(at, position(p));
            let r2 = dot(d, d) / carry2;
            if r2 >= 1.0 {
                continue;
            }
            let t = 1.0 - r2;
            let w = f64::from(t * t * t);
            sw += w;
            sn += w * f64::from(counts[k]);
            for (sum, v) in sv.iter_mut().zip(p.velocity) {
                *sum += w * f64::from(v);
            }
        }
        if sw > 0.0 {
            let nbar = (sn / sw) as f32;
            weights[n] = ((nbar - u.fade_start) / (u.fade_end - u.fade_start)).clamp(0.0, 1.0);
            velocity[n] = std::array::from_fn(|c| (sv[c] / sw) as f32);
        }
    }
    Expected {
        counts,
        weights,
        velocity,
    }
}

fn close(got: f32, want: f32) -> bool {
    (got - want).abs() <= 1.0e-5 * got.abs().max(want.abs()).max(1.0)
}

fn assert_all_close(label: &str, got: &[f32], want: &[f32]) {
    assert_eq!(got.len(), want.len(), "{label} length");
    for (i, (&g, &w)) in got.iter().zip(want).enumerate() {
        assert!(close(g, w), "{label}[{i}]: gpu {g} vs cpu {w}");
    }
}

fn flat(v: &[[f32; 3]]) -> Vec<f32> {
    v.iter().flatten().copied().collect()
}

fn node_index(case: &Case, ijk: [u32; 3]) -> usize {
    (ijk[0] + case.dims[0] * (ijk[1] + case.dims[1] * ijk[2])) as usize
}

#[test]
fn bulk_block_gets_full_weight() {
    // x from -12: tiles -2, -1, 0 and 1; y and z cross tile faces too.
    let case = Case {
        origin: [-12, 3, -4],
        dims: [25; 3],
        particles_per_cell: 8.0,
    };
    // Eight particles per field cell (0.0625): a grid at half a cell,
    // jittered by a quarter of its pitch, filling a cube round the middle node.
    let pitch = 0.03125;
    let centre = case.position([12, 12, 12]);
    let mut rng = Rng(0x9e37_79b9);
    let mut particles = Vec::new();
    let per_axis = 32;
    for k in 0..per_axis {
        for j in 0..per_axis {
            for i in 0..per_axis {
                let at = [i, j, k].map(|v| -0.5 + (v as f32 + 0.5) * pitch);
                let position: [f32; 3] =
                    std::array::from_fn(|a| centre[a] + at[a] + rng.range(-0.25, 0.25) * pitch);
                particles.push(particle(
                    position,
                    [0.5, -0.25, 1.0],
                    particles.len() as u32 + 1,
                ));
            }
        }
    }
    let outcome = run_case(&case, &particles, &vec![0.0; case.node_count()]);
    let mut checked = 0;
    for k in 0..case.dims[2] {
        for j in 0..case.dims[1] {
            for i in 0..case.dims[0] {
                let ijk = [i, j, k];
                let from_centre = ijk.map(|v| (v as f32 - 12.0) * H);
                if from_centre.iter().all(|v| v.abs() <= 0.25) {
                    let n = node_index(&case, ijk);
                    let v = outcome.velocity[n];
                    assert!(
                        outcome.weights[n] >= 1.0 - 1.0e-6,
                        "node {ijk:?}: weight {}",
                        outcome.weights[n]
                    );
                    assert!(close(v[0], 0.5), "node {ijk:?}: vx {}", v[0]);
                    assert!(
                        close(v[1], -0.25) && close(v[2], 1.0),
                        "node {ijk:?}: {v:?}"
                    );
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 9 * 9 * 9);
    // The corner of the box has no water near it.
    assert_eq!(outcome.weights[0], 0.0);
}

#[test]
fn one_particle_sheet_gets_no_weight() {
    let case = Case {
        origin: [-10, -9, -6],
        dims: [25; 3],
        particles_per_cell: 8.0,
    };
    // One particle thick, one per field cell face: eight neighbours each, far
    // under the fade-in of a bulk count of about 113. The sheet sits a third of
    // a cell off the cell faces.
    let centre = case.position([12, 12, 12]);
    let mut particles = Vec::new();
    let per_axis = 23;
    for j in 0..per_axis {
        for i in 0..per_axis {
            let position = [
                centre[0] - 0.6875 + 0.3 * H + i as f32 * 0.0625,
                centre[1] - 0.6875 + 0.3 * H + j as f32 * 0.0625,
                centre[2] + 0.01,
            ];
            particles.push(particle(
                position,
                [1.0, 2.0, 3.0],
                particles.len() as u32 + 1,
            ));
        }
    }
    let outcome = run_case(&case, &particles, &vec![0.0; case.node_count()]);
    assert!(
        outcome.weights.iter().all(|&w| w == 0.0),
        "a sheet must not be smoothed"
    );
    assert!(outcome.counts.iter().all(|&c| c <= 8.0));
    // The water's velocity is still carried at the sheet.
    let v = outcome.velocity[node_index(&case, [12, 12, 12])];
    assert!(
        close(v[0], 1.0) && close(v[1], 2.0) && close(v[2], 3.0),
        "{v:?}"
    );
}

/// One particle each side of the face between tiles -1 and 0 count each other
/// and carry to the node between them, as a hand count says.
#[test]
fn particles_either_side_of_a_tile_face_reach_each_other() {
    let case = Case {
        origin: [-6, 0, 0],
        dims: [14, 12, 12],
        // The density at which two neighbours, a mean count of 1, fade to half.
        particles_per_cell: (1.0
            / (WEIGHT_FADE_START + 0.5 * (WEIGHT_FADE_END - WEIGHT_FADE_START)))
            / expected_bulk_neighbours(1.0),
    };
    // A in cell -1 (tile -1), B in cell 0 (tile 0), 0.6 cells apart, both
    // 0.3 cells from node (0, 5, 4) in x.
    let a = particle([-0.8 * H, 4.7 * H, 3.8 * H], [1.0, 2.0, 3.0], 1);
    let b = particle([-0.2 * H, 4.7 * H, 3.8 * H], [3.0, 2.0, -1.0], 2);
    let outcome = run_case(&case, &[a, b], &vec![0.0; case.node_count()]);
    assert_eq!(outcome.counts, [1.0, 1.0]);
    let between = node_index(&case, [6, 5, 4]);
    // Equal distances, equal weights: the plain mean, and a mean count of 1.
    let v = outcome.velocity[between];
    assert!(
        close(v[0], 2.0) && close(v[1], 2.0) && close(v[2], 1.0),
        "{v:?}"
    );
    let w = outcome.weights[between];
    assert!((w - 0.5).abs() < 1.0e-4, "weight {w}");
    let reference = vec![0.0; case.node_count()];
    let want = cpu_reference(&case, &outcome.sorted, &reference, &outcome.uniforms);
    assert_all_close("weights", &outcome.weights, &want.weights);
    assert_all_close("velocity", &flat(&outcome.velocity), &flat(&want.velocity));
}

#[test]
fn random_cloud_matches_the_cpu_reference() {
    let case = Case {
        origin: [-9, -4, 3],
        dims: [31, 21, 27],
        particles_per_cell: 2.0,
    };
    let centre = case.position([15, 10, 13]);
    let mut rng = Rng(0x1357_9bdf);
    let mut particles = Vec::new();
    // A dense core, so counts span the fade, and a sparse spray over the box.
    for i in 0..2000u32 {
        let (lo, hi) = if i < 1500 {
            ([-0.4, -0.3, -0.5], [0.4, 0.3, 0.5])
        } else {
            ([-0.9, -0.6, -0.8], [0.9, 0.6, 0.8])
        };
        let position: [f32; 3] = std::array::from_fn(|a| centre[a] + rng.range(lo[a], hi[a]));
        let velocity = [
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
        ];
        particles.push(particle(position, velocity, i + 1));
    }
    // Unused slots with stray positions and velocities contribute nothing.
    for i in 0..37u32 {
        let mut stray = particle(centre, [9.0; 3], 3000 + i);
        stray.position_radius[3] = 0.0;
        particles.push(stray);
    }
    // Some nodes outside the field's band, some inside.
    let reference: Vec<f32> = (0..case.node_count())
        .map(|n| {
            if n % 5 == 0 {
                1.0
            } else {
                (n % 7) as f32 * 0.01 - 0.03
            }
        })
        .collect();
    let outcome = run_case(&case, &particles, &reference);
    let want = cpu_reference(&case, &outcome.sorted, &reference, &outcome.uniforms);
    assert_all_close("counts", &outcome.counts, &want.counts);
    assert_all_close("weights", &outcome.weights, &want.weights);
    assert_all_close("velocity", &flat(&outcome.velocity), &flat(&want.velocity));
    assert!(
        want.weights.iter().any(|&w| w > 0.0 && w < 1.0),
        "the cloud must exercise the fade"
    );
    assert!(want.weights.contains(&1.0));
    assert!(
        want.weights
            .iter()
            .enumerate()
            .any(|(n, &w)| w == 0.0 && n % 5 == 0)
    );
}

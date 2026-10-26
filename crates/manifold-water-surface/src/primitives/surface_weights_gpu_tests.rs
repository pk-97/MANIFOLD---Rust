//! GPU proofs for `node.surface_weights` against a brute-force CPU reference.

use super::*;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::fluid_particles::bin_counts;

/// A lattice box, the sort that bins over it, and the particle density per
/// field cell (the node passes it as a simulation cell of the field's size).
struct Case {
    center: [f32; 3],
    size: [f32; 3],
    nodes: [u32; 3],
    sort_cell: f32,
    particles_per_cell: f32,
}

struct Outcome {
    uniforms: WeightUniforms,
    sorted: Vec<FluidParticle>,
    counts: Vec<f32>,
    weights: Vec<f32>,
    velocity: Vec<f32>,
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

/// Bin and order the particles as `node.sort_particles_into_cells` does: bin
/// `floor((p - min) * (1 / cell))` clamped to the grid, input order kept
/// within a bin, unused records past the live ones.
fn sort_on_cpu(
    particles: &[FluidParticle],
    lattice_min: [f32; 3],
    sort_cell: f32,
    bins: [u32; 3],
) -> (Vec<FluidParticle>, Vec<CellRange>) {
    let inv = 1.0 / sort_cell;
    let bin_of = |p: &FluidParticle| {
        let b: [usize; 3] = std::array::from_fn(|a| {
            let v = ((p.position_radius[a] - lattice_min[a]) * inv).floor() as i64;
            v.clamp(0, i64::from(bins[a]) - 1) as usize
        });
        b[0] + bins[0] as usize * (b[1] + bins[1] as usize * b[2])
    };
    let total = bins.iter().map(|&n| n as usize).product::<usize>();
    let (live, unused): (Vec<_>, Vec<_>) = particles
        .iter()
        .copied()
        .partition(|p| p.position_radius[3] > 0.0);
    let mut order: Vec<usize> = (0..live.len()).collect();
    order.sort_by_key(|&i| bin_of(&live[i]));
    let mut sorted: Vec<FluidParticle> = order.iter().map(|&i| live[i]).collect();
    let mut ranges = vec![CellRange::default(); total];
    for (slot, p) in sorted.iter().enumerate() {
        let range = &mut ranges[bin_of(p)];
        if range.count == 0 {
            range.start = slot as u32;
        }
        range.count += 1;
    }
    sorted.extend(unused);
    (sorted, ranges)
}

fn run_case(case: &Case, particles: &[FluidParticle], reference: &[f32]) -> Outcome {
    let nodes_f = case.nodes.map(|n| n as f32);
    // The field cell is the smallest node spacing; the case's density is per
    // field cell, so it is passed as a simulation cell of that same size.
    let field_cell = (0..3)
        .map(|a| case.size[a] / (case.nodes[a] - 1) as f32)
        .fold(f32::INFINITY, f32::min);
    let bins = bin_counts(case.size, case.sort_cell);
    let lattice_min: [f32; 3] = std::array::from_fn(|a| case.center[a] - 0.5 * case.size[a]);
    let (sorted, ranges) = sort_on_cpu(particles, lattice_min, case.sort_cell, bins);
    let uniforms = weight_uniforms(
        case.center,
        case.size,
        nodes_f,
        case.sort_cell,
        bins,
        sorted.len() as u32,
        case.particles_per_cell,
        field_cell,
    )
    .unwrap();
    assert_eq!(reference.len(), uniforms.node_count as usize);

    let mut harness = Harness::new();
    let mut node = SurfaceWeights::new();
    let (particle_slot, _) = harness.array(&sorted, sorted.len());
    let (range_slot, _) = harness.array(&ranges, ranges.len());
    let (reference_slot, _) = harness.array(reference, reference.len());
    let (weights_slot, _) = harness.array::<f32>(&[], 1);
    let (velocity_slot, _) = harness.array::<f32>(&[], 1);
    let run_params = params(&[
        ("center_x", case.center[0]),
        ("center_y", case.center[1]),
        ("center_z", case.center[2]),
        ("size_x", case.size[0]),
        ("size_y", case.size[1]),
        ("size_z", case.size[2]),
        ("nodes_x", nodes_f[0]),
        ("nodes_y", nodes_f[1]),
        ("nodes_z", nodes_f[2]),
        ("cell_size", case.sort_cell),
        ("bins_x", bins[0] as f32),
        ("bins_y", bins[1] as f32),
        ("bins_z", bins[2] as f32),
        ("particles_per_cell", case.particles_per_cell),
        ("sim_cell_size", field_cell),
    ]);
    let (_, errors) = harness.run(
        &mut node,
        &[
            ("particles", particle_slot),
            ("cell_ranges", range_slot),
            ("reference", reference_slot),
        ],
        &[("weights", weights_slot), ("velocity", velocity_slot)],
        &run_params,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let nodes_total = uniforms.node_count as usize;
    Outcome {
        uniforms,
        counts: read(node.counts.as_ref().expect("counts buffer"), sorted.len()),
        weights: read(&harness.buffer(weights_slot), nodes_total),
        velocity: read(&harness.buffer(velocity_slot), 3 * nodes_total),
        sorted,
    }
}

struct Expected {
    counts: Vec<f32>,
    weights: Vec<f32>,
    velocity: Vec<f32>,
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn offset(from: [f32; 3], to: [f32; 3]) -> [f32; 3] {
    [to[0] - from[0], to[1] - from[1], to[2] - from[2]]
}

/// Brute force: every particle against every particle, every node against
/// every particle, the same formulas as the shader.
fn cpu_reference(particles: &[FluidParticle], reference: &[f32], u: &WeightUniforms) -> Expected {
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
    let n_total = u.node_count as usize;
    let mut weights = vec![0.0; n_total];
    let mut velocity = vec![0.0; 3 * n_total];
    for n in 0..n_total {
        if reference[n].abs() >= u.band {
            continue;
        }
        let nx = u.nodes[0] as usize;
        let ny = u.nodes[1] as usize;
        let ijk = [n % nx, (n / nx) % ny, n / (nx * ny)];
        let at: [f32; 3] = std::array::from_fn(|a| u.lattice_min[a] + ijk[a] as f32 * u.spacing[a]);
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
            for c in 0..3 {
                velocity[3 * n + c] = (sv[c] / sw) as f32;
            }
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

fn node_index(u: &WeightUniforms, ijk: [u32; 3]) -> usize {
    (ijk[0] + u.nodes[0] * (ijk[1] + u.nodes[1] * ijk[2])) as usize
}

#[test]
fn bulk_block_gets_full_weight() {
    let case = Case {
        center: [0.3, -0.2, 0.5],
        size: [1.5; 3],
        nodes: [25; 3],
        sort_cell: 0.125,
        particles_per_cell: 8.0,
    };
    // Eight particles per field cell (0.0625): a grid at half a cell,
    // jittered by a quarter of its pitch, filling the middle of the lattice.
    let pitch = 0.03125;
    let mut rng = Rng(0x9e37_79b9);
    let mut particles = Vec::new();
    let per_axis = 32;
    for k in 0..per_axis {
        for j in 0..per_axis {
            for i in 0..per_axis {
                let at = [i, j, k].map(|v| -0.5 + (v as f32 + 0.5) * pitch);
                let position: [f32; 3] = std::array::from_fn(|a| {
                    case.center[a] + at[a] + rng.range(-0.25, 0.25) * pitch
                });
                particles.push(particle(
                    position,
                    [0.5, -0.25, 1.0],
                    particles.len() as u32 + 1,
                ));
            }
        }
    }
    let nodes_total = case.nodes.iter().product::<u32>() as usize;
    let outcome = run_case(&case, &particles, &vec![0.0; nodes_total]);
    let u = &outcome.uniforms;
    let mut checked = 0;
    for k in 0..case.nodes[2] {
        for j in 0..case.nodes[1] {
            for i in 0..case.nodes[0] {
                let ijk = [i, j, k];
                let from_center: [f32; 3] =
                    std::array::from_fn(|a| -0.75 + ijk[a] as f32 * u.spacing[a]);
                if from_center.iter().all(|v| v.abs() <= 0.25) {
                    let n = node_index(u, ijk);
                    assert!(
                        outcome.weights[n] >= 1.0 - 1.0e-6,
                        "node {ijk:?}: weight {}",
                        outcome.weights[n]
                    );
                    assert!(
                        close(outcome.velocity[3 * n], 0.5),
                        "node {ijk:?}: vx {}",
                        outcome.velocity[3 * n]
                    );
                    assert!(close(outcome.velocity[3 * n + 2], 1.0));
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 9 * 9 * 9);
    // The corner of the lattice has no water near it.
    assert_eq!(outcome.weights[0], 0.0);
}

#[test]
fn one_particle_sheet_gets_no_weight() {
    let case = Case {
        center: [0.0, 0.25, -0.5],
        size: [1.5; 3],
        nodes: [25; 3],
        sort_cell: 0.125,
        particles_per_cell: 8.0,
    };
    // One particle thick, one per field cell face: eight neighbours each, far
    // under the fade-in of a bulk count of about 113.
    let mut particles = Vec::new();
    let per_axis = 23;
    for j in 0..per_axis {
        for i in 0..per_axis {
            let position = [
                case.center[0] - 0.6875 + i as f32 * 0.0625,
                case.center[1] - 0.6875 + j as f32 * 0.0625,
                case.center[2] + 0.01,
            ];
            particles.push(particle(
                position,
                [1.0, 2.0, 3.0],
                particles.len() as u32 + 1,
            ));
        }
    }
    let nodes_total = case.nodes.iter().product::<u32>() as usize;
    let outcome = run_case(&case, &particles, &vec![0.0; nodes_total]);
    assert!(
        outcome.weights.iter().all(|&w| w == 0.0),
        "a sheet must not be smoothed"
    );
    assert!(outcome.counts.iter().all(|&c| c <= 8.0));
    // The water's velocity is still carried at the sheet.
    let centre = node_index(&outcome.uniforms, [12, 12, 12]);
    assert!(close(outcome.velocity[3 * centre], 1.0));
    assert!(close(outcome.velocity[3 * centre + 1], 2.0));
    assert!(close(outcome.velocity[3 * centre + 2], 3.0));
}

#[test]
fn random_cloud_matches_the_cpu_reference() {
    let case = Case {
        center: [0.25, -0.5, 1.0],
        size: [2.0, 1.5, 2.5],
        nodes: [33, 25, 41],
        sort_cell: 0.125,
        particles_per_cell: 2.0,
    };
    let mut rng = Rng(0x1357_9bdf);
    let mut particles = Vec::new();
    // A dense core, so counts span the fade, and a sparse spray over the box.
    for i in 0..2000u32 {
        let (lo, hi) = if i < 1500 {
            ([-0.4, -0.3, -0.5], [0.4, 0.3, 0.5])
        } else {
            ([-1.0, -0.75, -1.25], [1.0, 0.75, 1.25])
        };
        let position: [f32; 3] = std::array::from_fn(|a| case.center[a] + rng.range(lo[a], hi[a]));
        let velocity = [
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
        ];
        particles.push(particle(position, velocity, i + 1));
    }
    // Unused slots with stray positions and velocities contribute nothing.
    for i in 0..37u32 {
        let mut stray = particle(
            [case.center[0], case.center[1], case.center[2]],
            [9.0; 3],
            3000 + i,
        );
        stray.position_radius[3] = 0.0;
        particles.push(stray);
    }
    let nodes_total = case.nodes.iter().product::<u32>() as usize;
    // Some nodes outside the re-distanced band, some inside.
    let reference: Vec<f32> = (0..nodes_total)
        .map(|n| {
            if n % 5 == 0 {
                1.0
            } else {
                (n % 7) as f32 * 0.01 - 0.03
            }
        })
        .collect();
    let outcome = run_case(&case, &particles, &reference);
    let want = cpu_reference(&outcome.sorted, &reference, &outcome.uniforms);
    assert_all_close("counts", &outcome.counts, &want.counts);
    assert_all_close("weights", &outcome.weights, &want.weights);
    assert_all_close("velocity", &outcome.velocity, &want.velocity);
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

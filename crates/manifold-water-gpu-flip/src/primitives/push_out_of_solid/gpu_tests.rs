//! GPU proofs for `node.push_out_of_solid` on a box of world tiles across the
//! origin: the pooled push matches a dense CPU port of the same march, a plane
//! pushes a particle exactly to its surface across a tile face, and a solid
//! with no usable normal leaves the particle alone.

use super::*;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::testkit::world_box::BoxTable;

const H: f32 = 0.1;
/// What a node in a tile the table does not hold reads.
const FAR: f32 = 1.0e30;

/// Nodes -6..=7 in x, -5..=6 in y and -7..=4 in z: two tiles on every axis,
/// split at the origin.
fn table() -> BoxTable {
    BoxTable::new([-6, -5, -7], [14, 12, 12])
}

fn particle(position: [f32; 3], id: u32) -> FluidParticle {
    FluidParticle {
        position_radius: [position[0], position[1], position[2], 0.04],
        velocity: [1.0, -2.0, 0.5],
        id,
    }
}

/// Where world node `n` sits.
fn node_position(n: [i32; 3]) -> [f32; 3] {
    n.map(|c| (c as f32 - 0.5) * H)
}

/// The solid on every node of `table`'s box (x fastest) from `distance`.
fn dense_solid(table: &BoxTable, distance: impl Fn([f32; 3]) -> f32) -> Vec<f32> {
    let [nx, ny, nz] = table.cells;
    let mut dense = Vec::new();
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let n = [x, y, z].map(|c| c as i32);
                dense.push(distance(node_position(std::array::from_fn(|a| {
                    n[a] + table.origin[a]
                }))));
            }
        }
    }
    dense
}

/// The pushed particles for `dense` solid on `table`.
fn pushed(table: &BoxTable, dense: &[f32], particles: &[FluidParticle]) -> Vec<FluidParticle> {
    let pool = table.to_pool(dense, FAR);
    let mut harness = Harness::new();
    let mut node = PushOutOfSolid::new();
    let (particle_slot, _) = harness.array(particles, particles.len());
    let (solid_slot, _) = harness.array(&pool, pool.len());
    let (tiles_slot, _) = harness.array(&table.words, table.words.len());
    let (out_slot, _) = harness.array::<FluidParticle>(&[], particles.len());
    let run_params = params(&[("cell_size", H), ("pool_tiles", table.pool_tiles as f32)]);
    let (_, errors) = harness.run(
        &mut node,
        &[
            ("particles", particle_slot),
            ("solid", solid_slot),
            ("tiles", tiles_slot),
        ],
        &[("out", out_slot)],
        &run_params,
    );
    assert!(errors.is_empty(), "{errors:?}");
    read(&harness.buffer(out_slot), particles.len())
}

/// The dense CPU port of the shader's march: the solid is the box's nodes
/// and `FAR` outside it.
struct Reference<'a> {
    table: &'a BoxTable,
    dense: &'a [f32],
}

impl Reference<'_> {
    fn node(&self, n: [i32; 3]) -> f32 {
        let c: [i32; 3] = std::array::from_fn(|a| n[a] - self.table.origin[a]);
        let inside = (0..3).all(|a| c[a] >= 0 && c[a] < self.table.cells[a] as i32);
        if !inside {
            return FAR;
        }
        let [nx, ny, _] = self.table.cells.map(|c| c as i32);
        self.dense[(c[0] + nx * (c[1] + ny * c[2])) as usize]
    }

    fn sample(&self, q: [f32; 3]) -> f32 {
        let base = q.map(f32::floor);
        let f: [f32; 3] = std::array::from_fn(|a| q[a] - base[a]);
        let base = base.map(|c| c as i32);
        let mut value = 0.0;
        for corner in 0..8 {
            let offset: [i32; 3] = std::array::from_fn(|a| (corner >> a) & 1);
            let weight: f32 = (0..3)
                .map(|a| if offset[a] == 1 { f[a] } else { 1.0 - f[a] })
                .product();
            value += weight * self.node(std::array::from_fn(|a| base[a] + offset[a]));
        }
        value
    }

    fn gradient(&self, q: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|axis| {
            let shifted = |sign: f32| {
                let mut p = q;
                p[axis] += 0.5 * sign;
                self.sample(p)
            };
            (shifted(1.0) - shifted(-1.0)) / H
        })
    }

    fn push(&self, mut position: [f32; 3]) -> [f32; 3] {
        for _ in 0..4 {
            let q = position.map(|c| c / H + 0.5);
            let phi = self.sample(q);
            if phi.is_nan() || phi >= 0.0 {
                break;
            }
            let g = self.gradient(q);
            let length = g.iter().map(|c| c * c).sum::<f32>().sqrt();
            if !(length > 1.0e-6 && length < f32::MAX) {
                break;
            }
            position = std::array::from_fn(|a| position[a] - phi * g[a] / length);
        }
        position
    }
}

fn assert_close(got: &FluidParticle, position: [f32; 3], was: &FluidParticle) {
    for a in 0..3 {
        assert!(
            (got.position_radius[a] - position[a]).abs() < 1.0e-4,
            "axis {a}: {got:?} vs {position:?}"
        );
    }
    assert_eq!(got.position_radius[3], was.position_radius[3]);
    assert_eq!(got.velocity, was.velocity);
    assert_eq!(got.id, was.id);
}

#[test]
fn a_plane_pushes_a_particle_to_its_surface_across_a_tile_face() {
    // Solid below y = 0.2; y nodes -1 and 0 (y = -0.15, -0.05) are either
    // side of the tile face between node -1 and node 0.
    let table = table();
    let dense = dense_solid(&table, |x| x[1] - 0.2);
    let particles = [
        particle([0.03, -0.3, 0.02], 7),
        particle([-0.12, 0.05, 0.1], 8),
        particle([0.12, 0.1, -0.2], 9),
        // Out in the air: untouched.
        particle([0.0, 0.3, 0.0], 10),
    ];
    let got = pushed(&table, &dense, &particles);
    for (got, was) in got.iter().zip(&particles).take(3) {
        let surface = [was.position_radius[0], 0.2, was.position_radius[2]];
        assert_close(got, surface, was);
    }
    assert_eq!(got[3], particles[3]);
}

#[test]
fn a_curved_solid_matches_the_dense_march_and_ends_near_the_surface() {
    let table = table();
    let radius = 0.25;
    let sphere = |x: [f32; 3]| (x[0] * x[0] + x[1] * x[1] + x[2] * x[2]).sqrt() - radius;
    let dense = dense_solid(&table, sphere);
    let reference = Reference {
        table: &table,
        dense: &dense,
    };
    // Inside the sphere, round the origin and the tile faces through it.
    let particles = [
        particle([0.12, 0.05, -0.08], 1),
        particle([-0.09, 0.14, 0.07], 2),
        particle([0.04, -0.15, 0.16], 3),
        particle([-0.18, -0.06, -0.1], 4),
        particle([0.0, 0.0, 0.2], 5),
        // Outside: untouched.
        particle([0.3, 0.2, 0.1], 6),
    ];
    let got = pushed(&table, &dense, &particles);
    for (got, was) in got.iter().zip(&particles) {
        let want = reference.push([0, 1, 2].map(|a| was.position_radius[a]));
        assert_close(got, want, was);
    }
    for (got, was) in got.iter().zip(&particles).take(5) {
        let q = got.position_radius;
        let depth = radius - (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt();
        assert!(
            depth <= 0.25 * H,
            "particle {} is still {depth} m inside",
            was.id
        );
    }
    assert_eq!(got[5], particles[5]);
}

#[test]
fn a_solid_with_no_usable_normal_leaves_the_particle_alone() {
    let table = table();
    let particles = [particle([0.1, -0.2, 0.1], 11)];
    // Negative everywhere: no direction out.
    let flat = dense_solid(&table, |_| -0.5);
    let got = pushed(&table, &flat, &particles);
    assert_eq!(got[0], particles[0]);

    // An infinite node only the gradient's half-cell steps reach: the
    // particle's own value is negative and finite, its gradient is not.
    let mut spiked = flat.clone();
    let infinite = [2, 0, 0];
    let [nx, ny, _] = table.cells.map(|c| c as i32);
    let c: [i32; 3] = std::array::from_fn(|a| infinite[a] - table.origin[a]);
    spiked[(c[0] + nx * (c[1] + ny * c[2])) as usize] = f32::INFINITY;
    // At node coordinate (0.8, 0, 0): its own corners are nodes 0 and 1 in x,
    // the half-cell step in +x reaches node 2.
    let at = particle([(0.8 - 0.5) * H, -0.5 * H, -0.5 * H], 12);
    let got = pushed(&table, &spiked, &[at]);
    assert_eq!(got[0], at);
    assert!(got[0].position_radius.iter().all(|c| c.is_finite()));
}

#[test]
fn a_particle_in_a_tile_the_table_does_not_hold_stays_put() {
    // The box ends at tile 0 in x: a particle at x = 3 m is in tile 3, where
    // the solid reads the canonical 1e30.
    let table = table();
    let flat = dense_solid(&table, |_| -0.5);
    let particles = [particle([3.0, 0.0, 0.0], 13)];
    let got = pushed(&table, &flat, &particles);
    assert_eq!(got[0], particles[0]);
}

//! GPU proofs for `node.spray_mist` on a box of world tiles across the origin:
//! the mist matches the dense reference of its kernel node for node, a lone
//! droplet follows the closed form across a tile face, and a footprint node in
//! a tile the table does not hold takes nothing.

use super::*;
use manifold_node_engine::scene::world_tiles::pool_len;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::testkit::world_box::BoxTable;

/// A particle's size at full fade, and the fade most of the test particles
/// carry: a particle's size is the node's radius times its fade, its `w`.
const RADIUS: f32 = 0.02;
const FADE: f32 = 0.5;
const H: f32 = 0.05;
/// The kernel's smoothing length: two display cells.
const SMOOTHING: f32 = 2.0 * H;
const KERNEL_PEAK: f32 = 1.0 / (std::f32::consts::PI * SMOOTHING * SMOOTHING * SMOOTHING);

/// Nodes -6..=7 in x, -5..=6 in y and -7..=4 in z: two tiles on every axis,
/// split at the origin.
fn table() -> BoxTable {
    BoxTable::new([-6, -5, -7], [14, 12, 12])
}

fn droplet_area(radius: f32) -> f32 {
    2.0 * std::f32::consts::PI * radius * radius
}

fn particle(position: [f32; 3], fade: f32, id: u32) -> FluidParticle {
    FluidParticle {
        position_radius: [position[0], position[1], position[2], fade],
        velocity: [0.0; 3],
        id,
    }
}

/// A particle at node coordinate `q` (node n is at (n - 1/2)·h).
fn at_node(q: [f32; 3], fade: f32, id: u32) -> FluidParticle {
    particle(q.map(|c| (c - 0.5) * H), fade, id)
}

/// Where box node `c` sits.
fn node_position(table: &BoxTable, c: [u32; 3]) -> [f32; 3] {
    std::array::from_fn(|a| ((table.origin[a] + c[a] as i32) as f32 - 0.5) * H)
}

/// The whole pool's mist for `particles` on `table`.
fn mist_pool(table: &BoxTable, particles: &[FluidParticle], strength: f32) -> Vec<f32> {
    let mut harness = Harness::new();
    let mut node = SprayMist::new();
    let (particle_slot, _) = harness.array(particles, particles.len());
    let (tiles_slot, _) = harness.array(&table.words, table.words.len());
    let (mist_slot, _) = harness.array::<f32>(&[], 1);
    let run_params = params(&[
        ("strength", strength),
        ("radius", RADIUS),
        ("cell_size", H),
        ("pool_tiles", table.pool_tiles as f32),
    ]);
    let (_, errors) = harness.run(
        &mut node,
        &[("particles", particle_slot), ("tiles", tiles_slot)],
        &[("mist", mist_slot)],
        &run_params,
    );
    assert!(errors.is_empty(), "{errors:?}");
    read(
        &harness.buffer(mist_slot),
        pool_len(table.pool_tiles) as usize,
    )
}

/// The 3D cubic-spline shape of the kernel (Monaghan 1992).
fn spline(q: f64) -> f64 {
    if q < 1.0 {
        1.0 - 1.5 * q * q + 0.75 * q * q * q
    } else if q < 2.0 {
        0.25 * (2.0 - q).powi(3)
    } else {
        0.0
    }
}

/// The dense reference: the extinction at `node` summed over every live droplet.
fn reference_mist(particles: &[FluidParticle], node: [f32; 3]) -> f64 {
    let h = f64::from(SMOOTHING);
    particles
        .iter()
        .filter(|p| p.position_radius[3] > 0.0)
        .map(|p| {
            let x = p.position_radius;
            let d = (0..3)
                .map(|a| (f64::from(node[a]) - f64::from(x[a])).powi(2))
                .sum::<f64>()
                .sqrt();
            let area = f64::from(droplet_area(RADIUS * x[3]));
            let kernel = 1.0 / (std::f64::consts::PI * h * h * h) * spline(d / h);
            if d >= 2.0 * h { 0.0 } else { area * kernel }
        })
        .sum()
}

/// Droplets on both sides of every tile face, in the corners, and one empty slot.
fn cloud() -> Vec<FluidParticle> {
    [
        ([-0.5, 0.0, 0.0], FADE),
        ([-0.2, -0.5, 0.5], 0.6),
        ([3.3, 2.7, -2.4], 0.4),
        ([-3.2, -3.9, -4.6], FADE),
        ([-0.45, 0.1, 0.05], FADE),
        ([4.4, -4.1, 3.2], 0.75),
        ([0.0, 0.0, -0.5], 0.0),
        ([1.6, 1.4, 1.8], FADE),
    ]
    .iter()
    .enumerate()
    .map(|(id, &(q, fade))| at_node(q, fade, id as u32))
    .collect()
}

#[test]
fn the_pooled_mist_matches_the_dense_reference_across_tile_faces() {
    let table = table();
    let particles = cloud();
    for strength in [1.0, 2.0] {
        let pool = mist_pool(&table, &particles, strength);
        let dense = table.from_pool(&pool);
        let mut misty = 0;
        for (index, &got) in dense.iter().enumerate() {
            let c = [
                index as u32 % table.cells[0],
                index as u32 / table.cells[0] % table.cells[1],
                index as u32 / (table.cells[0] * table.cells[1]),
            ];
            let want = f64::from(strength) * reference_mist(&particles, node_position(&table, c));
            assert!(
                (f64::from(got) - want).abs() <= 2e-5 * f64::from(strength),
                "strength {strength} node {c:?}: {got}, expected {want}"
            );
            misty += usize::from(want > 0.05);
        }
        assert!(misty > 20, "the cloud reaches {misty} nodes");
    }
}

#[test]
fn one_droplet_follows_the_kernel_across_a_tile_face() {
    let table = table();
    // Node (-1, 0, -1) is the last of tile -1 in x and z; its +x neighbour is
    // the first of tile 0.
    let on = [-1, 0, -1];
    let pool = mist_pool(&table, &[at_node(on.map(|n| n as f32), FADE, 0)], 1.0);
    let dense = table.from_pool(&pool);
    let at = |n: [i32; 3]| {
        let c: [u32; 3] = std::array::from_fn(|a| (n[a] - table.origin[a]) as u32);
        dense[(c[0] + table.cells[0] * (c[1] + table.cells[1] * c[2])) as usize]
    };
    let peak = droplet_area(RADIUS * FADE) * KERNEL_PEAK;
    let own = at(on);
    assert!(
        (own - peak).abs() <= 1e-5 * peak,
        "own node: {own}, expected {peak}"
    );
    // One spacing away, half a smoothing length, the kernel is 23/32 of its
    // peak, on either side of the face. The fixed-point sum rounds to 1 part
    // in 2^20.
    for step in [
        [1, 0, 0],
        [-1, 0, 0],
        [0, 1, 0],
        [0, -1, 0],
        [0, 0, 1],
        [0, 0, -1],
    ] {
        let n = std::array::from_fn(|a| on[a] + step[a]);
        let got = at(n);
        assert!(
            (got - 0.71875 * peak).abs() <= 1e-4 * peak,
            "node {n:?}: {got}, expected {}",
            0.71875 * peak
        );
    }
    // Four nodes away on any axis the kernel is out of reach: exactly 0.
    for z in 0..table.cells[2] {
        for y in 0..table.cells[1] {
            for x in 0..table.cells[0] {
                let n = [x, y, z].map(|c| c as i32);
                let apart: Vec<i32> = (0..3)
                    .map(|a| (n[a] + table.origin[a] - on[a]).abs())
                    .collect();
                if apart.iter().any(|&d| d >= 4) {
                    let index = (x + table.cells[0] * (y + table.cells[1] * z)) as usize;
                    assert_eq!(dense[index], 0.0, "node {n:?}");
                }
            }
        }
    }
}

#[test]
fn a_node_in_a_tile_the_table_does_not_hold_takes_nothing() {
    // The droplet sits between nodes 7 and 8: node 7 is the last of tile 0,
    // and tile 1 is not in the table, so nodes 8 to 11 of its footprint are
    // lost.
    let table = table();
    let particles = [at_node([7.6, 0.3, 0.3], FADE, 0)];
    let pool = mist_pool(&table, &particles, 1.0);
    assert_eq!(pool[pool.len() - 1], 0.0, "the canonical element stays 0");
    let dense = table.from_pool(&pool);
    let mut checked = 0;
    for (index, &got) in dense.iter().enumerate() {
        let c = [
            index as u32 % table.cells[0],
            index as u32 / table.cells[0] % table.cells[1],
            index as u32 / (table.cells[0] * table.cells[1]),
        ];
        let want = reference_mist(&particles, node_position(&table, c));
        assert!(
            (f64::from(got) - want).abs() <= 2e-5,
            "node {c:?}: {got}, expected {want}"
        );
        checked += usize::from(want > 0.05);
    }
    assert!(checked >= 4, "{checked} misty nodes inside the table");
}

#[test]
fn empty_slots_make_no_mist() {
    let table = table();
    let empty: Vec<_> = (0..64)
        .map(|id| at_node([0.0, 0.0, 0.0], 0.0, id))
        .collect();
    let pool = mist_pool(&table, &empty, 1.0);
    assert!(pool.iter().all(|&m| m == 0.0));
}

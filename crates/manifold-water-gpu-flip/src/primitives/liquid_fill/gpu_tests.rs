//! The fill's sites are world half cells: a box on whole world cells puts
//! each site at (J/2 - 1/4)·h, J the world half cell, whatever the box's
//! corner.

use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::lattice::PADDING_NODES;

use super::LiquidFill;

/// The box's sites in the kernel's order: the pool's layers over the whole
/// floor, then the box clipped above the pool and to the lattice.
fn enumerated(cells: [u32; 3], pool: u32, sites: [[u32; 2]; 3]) -> Vec<[u32; 3]> {
    let n = cells.map(|c| 2 * c);
    let mut out = Vec::new();
    for z in 0..n[2] {
        for y in 0..pool.min(n[1]) {
            for x in 0..n[0] {
                out.push([x, y, z]);
            }
        }
    }
    for z in sites[2][0]..sites[2][1].min(n[2]) {
        for y in sites[1][0].max(pool)..sites[1][1].min(n[1]) {
            for x in sites[0][0]..sites[0][1].min(n[0]) {
                out.push([x, y, z]);
            }
        }
    }
    out
}

/// Runs the fill over a box whose first cell is world cell `box_cell` and
/// checks every particle sits at its world site.
fn assert_world_sites(
    box_cell: [i32; 3],
    cells: [u32; 3],
    h: f32,
    pool: u32,
    sites: [[u32; 2]; 3],
) {
    let mut harness = Harness::new();
    let out = harness.array::<FluidParticle>(&[], 1);
    let count = harness.scalar();
    let pad = PADDING_NODES;
    let lattice_min: [f32; 3] = std::array::from_fn(|a| (box_cell[a] - pad as i32) as f32 * h);
    let step = params(&[
        ("nodes_x", (cells[0] + 1 + 2 * pad) as f32),
        ("nodes_y", (cells[1] + 1 + 2 * pad) as f32),
        ("nodes_z", (cells[2] + 1 + 2 * pad) as f32),
        ("cell_size", h),
        ("lattice_min_x", lattice_min[0]),
        ("lattice_min_y", lattice_min[1]),
        ("lattice_min_z", lattice_min[2]),
        ("pool_sites", pool as f32),
        ("box_x0", sites[0][0] as f32),
        ("box_x1", sites[0][1] as f32),
        ("box_y0", sites[1][0] as f32),
        ("box_y1", sites[1][1] as f32),
        ("box_z0", sites[2][0] as f32),
        ("box_z1", sites[2][1] as f32),
    ]);
    let mut fill = LiquidFill::new();
    let (_, errors) = harness.run(
        &mut fill,
        &[],
        &[("particles", out.0), ("count", count)],
        &step,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let want = enumerated(cells, pool, sites);
    let got: Vec<FluidParticle> = read(&harness.buffer(out.0), want.len());
    for (i, (particle, site)) in got.iter().zip(&want).enumerate() {
        for a in 0..3 {
            // Half cell J of the world, counted from the origin cell's
            // centre: 2·p + 1 + j for the box's site j.
            let half = 2 * box_cell[a] + 1 + site[a] as i32;
            let world = (0.5 * half as f32 - 0.25) * h;
            assert!(
                (particle.position_radius[a] - world).abs() < 1e-6,
                "particle {i} axis {a}: {} vs {world}",
                particle.position_radius[a]
            );
        }
    }
}

#[test]
fn liquid_fill_places_sites_at_world_half_cells() {
    // A box across the origin on every axis but y: cells -5.., -2.., 3..
    // of 0.125 m, 12 x 10 x 8 sites, a one-layer pool and a box clipped
    // above it.
    assert_world_sites([-5, -2, 3], [6, 5, 4], 0.125, 1, [[2, 8], [0, 4], [3, 9]]);
    // The Dam Break's lattice (4 m at 64 cells, the box from -2 m): its
    // first site is 1/4 cell in from the corner.
    assert_world_sites([-32, 0, -32], [64, 64, 64], 0.0625, 1, [[0; 2]; 3]);
}

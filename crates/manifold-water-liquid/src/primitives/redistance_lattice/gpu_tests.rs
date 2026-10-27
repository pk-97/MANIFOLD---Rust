use super::RedistanceLattice;
use crate::testkit::world_box::{BoxTable, from_pool_under, table_words_of};
use manifold_node_engine::scene::world_tiles::{TILE_CELLS, pool_len};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};

const RADIUS: f32 = 0.6;
/// The world node of a sphere box's first node: x from -6 puts tiles -1 and 0
/// in the box, with a tile face through the sphere; y and z start inside
/// their tiles.
const ORIGIN: [i32; 3] = [-6, 3, -5];
const BAND: f32 = 4.0;
/// Nodes a node's search reads past itself: the band's cells, and the far
/// corner of the last cell.
const REACH: usize = BAND as usize + 1;

/// Node-centred sphere signed distance on a box of `nodes` nodes `cell` apart.
fn sphere_field(nodes: [u32; 3], cell: f32) -> Vec<f32> {
    let centre = nodes.map(|n| (n - 1) as f32 * cell * 0.5);
    let mut field = Vec::new();
    for z in 0..nodes[2] {
        for y in 0..nodes[1] {
            for x in 0..nodes[0] {
                let p = [x, y, z].map(|a| a as f32 * cell);
                let r = (0..3)
                    .map(|a| (p[a] - centre[a]).powi(2))
                    .sum::<f32>()
                    .sqrt();
                field.push(r - RADIUS);
            }
        }
    }
    field
}

/// `dense` as a pool of `table`'s tiles and one free slot after them: `canon`
/// in every node of the tiles outside the box and in the canonical element,
/// `junk` in the free slot. With the words of its table and its tiles.
fn pooled_with_free_slot(
    table: &BoxTable,
    dense: &[f32],
    canon: f32,
    junk: f32,
) -> (Vec<f32>, Vec<u32>, u32) {
    let pool_tiles = table.pool_tiles + 1;
    let mut pool = table.to_pool(dense, canon);
    pool.pop();
    pool.extend(vec![junk; TILE_CELLS as usize]);
    pool.push(canon);
    assert_eq!(pool.len() as u64, pool_len(pool_tiles));
    (pool, table_words_of(pool_tiles, &table.held()), pool_tiles)
}

/// The redistanced pool of `field` placed in `table`, and the box's nodes out
/// of it.
fn redistance(h: &mut Harness, table: &BoxTable, cell: f32, field: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let (pool, words, pool_tiles) = pooled_with_free_slot(table, field, 3.0 * cell, -1.0);
    let len = pool.len();
    let (levelset, _) = h.array(&pool, len);
    let (tiles, _) = h.array(&words, words.len());
    let (out, out_buf) = h.array::<f32>(&[], len);
    let (_, errors) = h.run(
        &mut RedistanceLattice::new(),
        &[("levelset", levelset), ("tiles", tiles)],
        &[("out", out)],
        &params(&[
            ("cell_size", cell),
            ("band_cells", BAND),
            ("pool_tiles", pool_tiles as f32),
        ]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let pooled = read::<f32>(&out_buf, len);
    let dense = from_pool_under(&pooled, &words, pool_tiles, table.origin, table.cells);
    (pooled, dense)
}

#[test]
fn band_in_cells_gives_the_same_depth_in_cells_on_two_lattices() {
    let mut h = Harness::new();
    for (nodes, cell) in [([21_u32, 17, 25], 0.1_f32), ([41, 33, 49], 0.05)] {
        let field = sphere_field(nodes, cell);
        let table = BoxTable::new(ORIGIN, nodes);
        let (pooled, out) = redistance(&mut h, &table, cell, &field);
        let (mut saturated, mut exact) = (0, 0);
        for (i, (&got, &sdf)) in out.iter().zip(&field).enumerate() {
            let nx = nodes[0] as usize;
            let ny = nodes[1] as usize;
            let at = [i % nx, (i / nx) % ny, i / (nx * ny)];
            // Past the search's reach from the box's edge, so every cell it
            // reads is the box's; nearer, it reads the canonical level set.
            if (0..3).any(|a| at[a] < REACH || at[a] + REACH >= nodes[a] as usize) {
                continue;
            }
            let cells = sdf.abs() / cell;
            // The rebuilt surface is off the true sphere by a few hundredths
            // of a cell (see below), so test saturation clear of the band edge.
            if cells > BAND + 0.2 {
                saturated += 1;
                assert!(
                    (got.abs() - BAND * cell).abs() < 1e-5 * cell,
                    "{nodes:?} node {at:?}: {got} is not +-{}",
                    BAND * cell
                );
                assert_eq!(got < 0.0, sdf < 0.0, "{nodes:?} node {at:?} sign");
            } else if cells < BAND - 1.0 {
                exact += 1;
                // Marching cubes places its vertices by linear interpolation
                // along cell edges, so on a coarse lattice the rebuilt
                // sphere is a few hundredths of a cell off the true one.
                assert!(
                    (got - sdf).abs() < 0.1 * cell,
                    "{nodes:?} node {at:?}: {got} != {sdf}"
                );
            }
        }
        assert!(saturated > 0 && exact > 0, "{nodes:?}: {saturated} {exact}");
        // The canonical element and the free slot (which held water on the
        // way in) come out as the canonical level set.
        let far = 3.0 * cell;
        let free = table.pool_tiles as usize * TILE_CELLS as usize;
        assert_eq!(pooled[pooled.len() - 1], far, "{nodes:?} canonical element");
        assert!(
            pooled[free..free + TILE_CELLS as usize]
                .iter()
                .all(|&v| v == far),
            "{nodes:?} free slot"
        );
    }
}

/// Water out to the edge of its tiles: a node past the box in a tile the table
/// holds, or in one it does not, is the canonical level set, 3 cells away, so
/// the zero surface lies `1/7` cell past the last water node (water -1/2 cell
/// deep, linearly to +3). Nothing clamps the box to its edge.
#[test]
fn a_tile_the_table_does_not_hold_reads_as_far_water() {
    let cell = 0.1_f32;
    // World nodes -6..=7 on x: tile -1 holds the box from -6 and two empty
    // nodes before it, tile 1 past node 7 is absent. y and z fill tiles 0
    // and 1, with absent tiles on both sides.
    let nodes = [14_u32, 16, 16];
    let table = BoxTable::new([-6, 0, 0], nodes);
    let field = vec![-0.5 * cell; nodes.iter().product::<u32>() as usize];
    let mut h = Harness::new();
    let (_, out) = redistance(&mut h, &table, cell, &field);
    let t = 0.5 / 3.5;
    let mut checked = 0;
    for (i, &got) in out.iter().enumerate() {
        let x = i % 14;
        let y = (i / 14) % 16;
        let z = i / (14 * 16);
        // The y and z faces are 5 or more nodes off, past the band.
        if !(5..=10).contains(&y) || !(5..=10).contains(&z) {
            continue;
        }
        let planes = x.min(13 - x) as f32 + t;
        let want = -(planes * cell).min(BAND * cell);
        assert!(
            (got - want).abs() < 1e-4 * cell,
            "node {:?}: {got} != {want}",
            [x, y, z]
        );
        checked += 1;
    }
    assert_eq!(checked, 14 * 6 * 6);
}

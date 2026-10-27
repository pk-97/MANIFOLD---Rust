//! GPU proofs for `node.carry_liquid_into_solids` on world tiles, with a wall
//! and a floor inside the box. The box of nodes is a pooled field under the
//! table of the tiles it touches, its first node at world node `ORIGIN` so it
//! straddles tiles -1 and 0 on x, and the wall stands on a tile face.

use super::*;
use manifold_node_engine::scene::scene_object::FIELD_FAR_CELLS;
use manifold_node_engine::scene::world_tiles::pool_len;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::testkit::world_box::{BoxTable, table_words_of};

const H: f32 = 0.1;
const NODES: [u32; 3] = [26, 20, 22];
/// World node of the box's first node: x from -6 is tile -1 for six nodes and
/// tiles 0, 1 and 2 after; y and z cross tile faces too.
const ORIGIN: [i32; 3] = [-6, 3, -5];
/// The wall fills x above it, off every node; nodes at x 0.65 and 0.75 are
/// inside the two carried cells, the nodes from 0.85 on are deeper. The face
/// between tiles 0 and 1 is at x = 0.75, between those two nodes' columns.
const WALL_X: f32 = 0.62;
const POOL_Y: f32 = 1.23;
/// An undrawn floor below this height.
const FLOOR_Y: f32 = 0.52;
const NO_SOLID: f32 = 1.0e30;

fn table() -> BoxTable {
    BoxTable::new(ORIGIN, NODES)
}

/// Box node `ijk` in metres: world node p is at (p - 1/2) cells.
fn position(ijk: [u32; 3]) -> [f32; 3] {
    std::array::from_fn(|a| ((ORIGIN[a] + ijk[a] as i32) as f32 - 0.5) * H)
}

fn index(ijk: [u32; 3]) -> usize {
    (ijk[0] + NODES[0] * (ijk[1] + NODES[1] * ijk[2])) as usize
}

fn field(value: impl Fn([f32; 3]) -> f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(NODES.iter().product::<u32>() as usize);
    for z in 0..NODES[2] {
        for y in 0..NODES[1] {
            for x in 0..NODES[0] {
                out.push(value(position([x, y, z])));
            }
        }
    }
    out
}

/// The wall plane's distance, negative beyond x = `WALL_X`.
fn wall(p: [f32; 3]) -> f32 {
    WALL_X - p[0]
}

fn floor(p: [f32; 3]) -> f32 {
    p[1] - FLOOR_Y
}

fn pool(p: [f32; 3]) -> f32 {
    p[1] - POOL_Y
}

fn wobbly_pool(p: [f32; 3]) -> f32 {
    p[1] - POOL_Y + 0.05 * (3.0 * p[0]).sin() + 0.03 * (5.0 * p[2]).cos()
}

/// The node over pooled arrays under the table `words` of `pool_tiles` slots;
/// every output element is written or reads back as NaN.
fn run_pool(
    words: &[u32],
    pool_tiles: u32,
    levelset: &[f32],
    solid: &[f32],
    drawn: Option<&[f32]>,
    carry_cells: f32,
) -> Vec<f32> {
    let len = pool_len(pool_tiles) as usize;
    let mut harness = Harness::new();
    let (levelset_slot, _) = harness.array(levelset, len);
    let (solid_slot, _) = harness.array(solid, len);
    let (tiles_slot, _) = harness.array(words, words.len());
    let (out_slot, out_buffer) = harness.array(&vec![f32::NAN; len], len);
    let drawn_slot = drawn.map(|d| harness.array(d, len).0);
    let run_params = params(&[
        ("carry_cells", carry_cells),
        ("cell_size", H),
        ("pool_tiles", pool_tiles as f32),
    ]);
    let mut inputs = vec![
        ("levelset", levelset_slot),
        ("solid", solid_slot),
        ("tiles", tiles_slot),
    ];
    if let Some(slot) = drawn_slot {
        inputs.push(("drawn_solid", slot));
    }
    let (_, errors) = harness.run(
        &mut CarryLiquidIntoSolids::new(),
        &inputs,
        &[("out", out_slot)],
        &run_params,
    );
    assert!(errors.is_empty(), "{errors:?}");
    read(&out_buffer, len)
}

/// The node over the box's dense arrays; the box's values read back.
fn run(levelset: &[f32], solid: &[f32], drawn: Option<&[f32]>, carry_cells: f32) -> Vec<f32> {
    let table = table();
    let far = FIELD_FAR_CELLS * H;
    let drawn = drawn.map(|d| table.to_pool(d, NO_SOLID));
    table.from_pool(&run_pool(
        &table.words,
        table.pool_tiles,
        &table.to_pool(levelset, far),
        &table.to_pool(solid, NO_SOLID),
        drawn.as_deref(),
        carry_cells,
    ))
}

#[test]
fn carried_pool_meets_the_wall_flush() {
    let levelset = field(pool);
    let solid = field(|p| wall(p).min(floor(p)));
    let drawn = field(wall);
    let out = run(&levelset, &solid, Some(&drawn), 2.0);
    // Local columns 13 and 14 are world nodes 7 and 8, 0.03 and 0.13 inside the
    // wall: within the two carried cells, the second across the tile face from
    // the water it takes. Away from the floor and the box faces, each column's
    // sign change is the pool surface.
    for i in [13u32, 14] {
        for k in 3..=18u32 {
            let column: Vec<f32> = (0..NODES[1]).map(|j| out[index([i, j, k])]).collect();
            let j = (4..=16usize)
                .find(|&j| column[j] < 0.0 && column[j + 1] >= 0.0)
                .unwrap_or_else(|| panic!("no zero crossing at i {i} k {k}: {column:?}"));
            let t = -column[j] / (column[j + 1] - column[j]);
            let crossing = ((ORIGIN[1] + j as i32) as f32 - 0.5 + t) * H;
            assert!(
                (crossing - POOL_Y).abs() < 0.01 * H,
                "i {i} k {k}: crossing {crossing}"
            );
        }
    }
    // Deeper than the carry, the solid is air.
    let air = FIELD_BAND_CELLS * H;
    for i in 15..NODES[0] {
        for j in 0..NODES[1] {
            for k in 3..=18u32 {
                assert_eq!(out[index([i, j, k])], air, "node {:?}", [i, j, k]);
            }
        }
    }
    // The undrawn floor only clips; the rest, off the wall, is kept.
    for i in 0..=12u32 {
        for j in 0..NODES[1] {
            for k in 0..NODES[2] {
                let at = index([i, j, k]);
                let want = if solid[at] < 0.0 {
                    levelset[at].max(0.0)
                } else {
                    levelset[at]
                };
                assert_eq!(out[at], want, "node {:?}", [i, j, k]);
            }
        }
    }
    assert!(levelset.iter().zip(&out).any(|(v, o)| v != o));
}

#[test]
fn undrawn_solid_gets_no_carried_water() {
    let levelset = field(wobbly_pool);
    let solid = field(|p| wall(p).min(floor(p)));
    let drawn = vec![1.0f32; solid.len()];
    let out = run(&levelset, &solid, Some(&drawn), 2.0);
    for (at, &o) in out.iter().enumerate() {
        let want = if solid[at] < 0.0 {
            levelset[at].max(0.0)
        } else {
            levelset[at]
        };
        assert_eq!(o, want, "node {at}");
    }
    assert!(solid.iter().filter(|&&s| s < 0.0).count() > 1000);
}

// The zero-carry output is compared with the plain clip, max(value, 0) inside
// a solid, written out on the CPU.
#[test]
fn zero_carry_equals_the_clamp_bit_for_bit() {
    let levelset = field(wobbly_pool);
    let solid = field(|p| wall(p).min(floor(p)));
    let drawn = field(wall);
    let expected: Vec<f32> = solid
        .iter()
        .zip(&levelset)
        .map(|(&s, &v)| if s < 0.0 { v.max(0.0) } else { v })
        .collect();
    for drawn in [None, Some(drawn.as_slice())] {
        let out = run(&levelset, &solid, drawn, 0.0);
        assert!(expected.contains(&0.0), "the wall must clip something");
        assert_eq!(
            out.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }
}

/// A free slot and the canonical element hold three cells whatever their
/// slots' contents; held slots keep their values outside a solid.
#[test]
fn free_slots_and_the_canonical_element_read_far() {
    // Three slots: tiles (0,0,0) and (1,0,0) held, slot 1 free.
    let words = table_words_of(3, &[([0, 0, 0], 0), ([1, 0, 0], 2)]);
    let len = pool_len(3) as usize;
    let out = run_pool(&words, 3, &vec![7.0; len], &vec![NO_SOLID; len], None, 2.0);
    let far = FIELD_FAR_CELLS * H;
    for (e, &o) in out.iter().enumerate() {
        let held = e < 512 || (1024..1536).contains(&e);
        assert_eq!(o, if held { 7.0 } else { far }, "element {e}");
    }
}

/// A solid node beside a tile the table does not hold reads that tile's
/// solid as far, not as 1e30: the normal and the carried value stay finite.
#[test]
fn a_solid_beside_an_absent_tile_stays_finite() {
    let words = table_words_of(1, &[([0, 0, 0], 0)]);
    let len = pool_len(1) as usize;
    let far = FIELD_FAR_CELLS * H;
    // The solid fills local x of 5 and up, 0.05, 0.15 and 0.25 inside; the
    // tile's every face touches an absent tile.
    let mut solid = vec![NO_SOLID; len];
    for (e, s) in solid.iter_mut().enumerate().take(512) {
        *s = (4.5 - (e & 7) as f32) * H;
    }
    let out = run_pool(&words, 1, &vec![far; len], &solid, None, 2.0);
    for (e, &o) in out.iter().enumerate().take(512) {
        // Kept, carried (the level set is far everywhere, so any sample is
        // far) and air.
        let want = if e & 7 == 7 {
            FIELD_BAND_CELLS * H
        } else {
            far
        };
        // A trilinear sample of a constant is it, to rounding.
        assert!((o - want).abs() < 1.0e-6, "element {e}: {o} vs {want}");
    }
    assert_eq!(out[512], far, "the canonical element");
}

//! GPU proofs for `node.carry_liquid_into_solids` on a non-cubic lattice with
//! a wall inside the box.

use super::*;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};

const NODES: [usize; 3] = [25, 21, 29];
const SOLID_NODES: [usize; 3] = [13, 11, 15];
const SIZE: [f32; 3] = [2.4, 2.0, 2.8];
const WALL_X: f32 = 1.65;
const POOL_Y: f32 = 0.8;

fn center() -> [f32; 3] {
    SIZE.map(|s| 0.5 * s)
}

fn node_position(ijk: [usize; 3], nodes: [usize; 3]) -> [f32; 3] {
    std::array::from_fn(|a| ijk[a] as f32 * SIZE[a] / (nodes[a] - 1) as f32)
}

fn index(ijk: [usize; 3], nodes: [usize; 3]) -> usize {
    ijk[0] + nodes[0] * (ijk[1] + nodes[1] * ijk[2])
}

fn field(nodes: [usize; 3], value: impl Fn([usize; 3], [f32; 3]) -> f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(nodes.iter().product());
    for z in 0..nodes[2] {
        for y in 0..nodes[1] {
            for x in 0..nodes[0] {
                out.push(value([x, y, z], node_position([x, y, z], nodes)));
            }
        }
    }
    out
}

/// The wall plane distance, negative beyond x = 1.65.
fn wall(_: [usize; 3], p: [f32; 3]) -> f32 {
    WALL_X - p[0]
}

/// The wall plus a negative one-node ring around the box.
fn wall_and_padding(ijk: [usize; 3], p: [f32; 3]) -> f32 {
    let on_border = (0..3).any(|a| ijk[a] == 0 || ijk[a] == SOLID_NODES[a] - 1);
    if on_border {
        wall(ijk, p).min(-0.1)
    } else {
        wall(ijk, p)
    }
}

fn pool(_: [usize; 3], p: [f32; 3]) -> f32 {
    p[1] - POOL_Y
}

fn wobbly_pool(_: [usize; 3], p: [f32; 3]) -> f32 {
    p[1] - POOL_Y + 0.05 * (3.0 * p[0]).sin() + 0.03 * (5.0 * p[2]).cos()
}

/// Level-set nodes strictly inside the wall and away from every box face, so
/// the padding never reaches them.
fn interior_nodes() -> impl Iterator<Item = [usize; 3]> {
    (4..=16usize)
        .flat_map(|j| (4..=24usize).flat_map(move |k| (17..=20usize).map(move |i| [i, j, k])))
}

fn run(levelset: &[f32], solid: &[f32], drawn: Option<&[f32]>, carry_cells: f32) -> Vec<f32> {
    let mut harness = Harness::new();
    let total = NODES.iter().product::<usize>();
    let (levelset_slot, _) = harness.array(levelset, total);
    let (solid_slot, _) = harness.array(solid, solid.len());
    let (out_slot, out_buf) = harness.array::<f32>(&[], total);
    let drawn_slot = drawn.map(|d| harness.array(d, d.len()).0);
    let c = center();
    let run_params = params(&[
        ("center_x", c[0]),
        ("center_y", c[1]),
        ("center_z", c[2]),
        ("size_x", SIZE[0]),
        ("size_y", SIZE[1]),
        ("size_z", SIZE[2]),
        ("nodes_x", NODES[0] as f32),
        ("nodes_y", NODES[1] as f32),
        ("nodes_z", NODES[2] as f32),
        ("solid_nodes_x", SOLID_NODES[0] as f32),
        ("solid_nodes_y", SOLID_NODES[1] as f32),
        ("solid_nodes_z", SOLID_NODES[2] as f32),
        ("carry_cells", carry_cells),
    ]);
    let mut inputs = vec![("levelset", levelset_slot), ("solid", solid_slot)];
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
    read::<f32>(&out_buf, total)
}

#[test]
fn carried_pool_meets_the_wall_flush() {
    let levelset = field(NODES, pool);
    let solid = field(SOLID_NODES, wall_and_padding);
    let drawn = field(SOLID_NODES, wall);
    let out = run(&levelset, &solid, Some(&drawn), 2.0);
    let spacing_y = SIZE[1] / (NODES[1] - 1) as f32;
    // Nodes 17 and 18 sit 0.05 m and 0.15 m inside the wall: within the two carried cells.
    for [i, _, k] in interior_nodes().filter(|n| n[1] == 4 && n[0] <= 18) {
        // Walk the column in y and find where the output changes sign.
        let column: Vec<f32> = (0..NODES[1])
            .map(|j| out[index([i, j, k], NODES)])
            .collect();
        let j = (4..=16)
            .find(|&j| column[j] < 0.0 && column[j + 1] >= 0.0)
            .unwrap_or_else(|| panic!("no zero crossing at i {i} k {k}: {column:?}"));
        let t = -column[j] / (column[j + 1] - column[j]);
        let crossing = (j as f32 + t) * spacing_y;
        assert!(
            (crossing - POOL_Y).abs() < 0.1 * spacing_y,
            "i {i} k {k}: crossing {crossing}"
        );
    }
}

#[test]
fn undrawn_solid_gets_no_carried_water() {
    let levelset = field(NODES, wobbly_pool);
    let solid = field(SOLID_NODES, wall_and_padding);
    let drawn = vec![1.0f32; solid.len()];
    let out = run(&levelset, &solid, Some(&drawn), 2.0);
    for ijk in interior_nodes() {
        let at = index(ijk, NODES);
        assert_eq!(out[at], levelset[at].max(0.0), "node {ijk:?}");
    }
}

/// The trilinear solid sample the shader's clip uses, on the CPU.
fn solid_at(solid: &[f32], p: [f32; 3]) -> f32 {
    let g: [f32; 3] = std::array::from_fn(|a| {
        (p[a] / (SIZE[a] / (SOLID_NODES[a] - 1) as f32)).clamp(0.0, (SOLID_NODES[a] - 1) as f32)
    });
    let base: [usize; 3] = std::array::from_fn(|a| (g[a].floor() as usize).min(SOLID_NODES[a] - 2));
    let f: [f32; 3] = std::array::from_fn(|a| g[a] - base[a] as f32);
    let mut value = 0.0;
    for corner in 0..8usize {
        let o = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
        let w: f32 = (0..3)
            .map(|a| if o[a] == 1 { f[a] } else { 1.0 - f[a] })
            .product();
        value += w * solid[index(
            [base[0] + o[0], base[1] + o[1], base[2] + o[2]],
            SOLID_NODES,
        )];
    }
    value
}

// The zero-carry output is compared with the plain clip, max(value, 0) inside
// a solid, written out on the CPU.
#[test]
fn zero_carry_equals_the_clamp_bit_for_bit() {
    let levelset = field(NODES, wobbly_pool);
    let solid = field(SOLID_NODES, wall_and_padding);
    let drawn = field(SOLID_NODES, wall);
    let expected: Vec<f32> = field(NODES, |ijk, p| {
        let v = levelset[index(ijk, NODES)];
        if solid_at(&solid, p) < 0.0 {
            v.max(0.0)
        } else {
            v
        }
    });
    for drawn in [None, Some(drawn.as_slice())] {
        let out = run(&levelset, &solid, drawn, 0.0);
        assert!(expected.contains(&0.0), "the wall must clip something");
        assert_eq!(
            out.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }
}

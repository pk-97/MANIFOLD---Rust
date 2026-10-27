//! The kernel's wall distance against its CPU twin, `bounds_distance`.

use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::bodies::{LiquidBody, LiquidShape};
use manifold_water_liquid::lattice::{PADDING_NODES, bounds_distance};

use super::LiquidSolidDistance;

/// Wall faces −X +Y −Z +Z (bits −X +X −Y +Y −Z +Z); +X and −Y are Open.
const MIXED_WALLS: u32 = 0b11_1001;

#[test]
fn liquid_solid_distance_walls_match_bounds_distance() {
    // The least lattice a domain publishes: 8 nodes a side, 0.25 m apart
    // from -1 m. The box is off the node grid and different on every axis,
    // so nodes fall inside it and past each of its faces.
    let (min, h, n) = (-1.0f32, 0.25f32, 2 + 2 * PADDING_NODES);
    let (lo, hi) = ([-0.6f32, -0.55, -0.7], [0.35f32, 0.4, 0.45]);
    let mut harness = Harness::new();
    let bodies = harness.array::<LiquidBody>(&[], 1);
    let shapes = harness.array::<LiquidShape>(&[], 1);
    let atlas = harness.array::<u32>(&[], 1);
    let out = harness.array::<f32>(&[], 1);
    let mut node = LiquidSolidDistance::new();
    for walls in [MIXED_WALLS, 0] {
        let set = params(&[
            ("lattice_min_x", min),
            ("lattice_min_y", min),
            ("lattice_min_z", min),
            ("cell_size", h),
            ("nodes_x", n as f32),
            ("nodes_y", n as f32),
            ("nodes_z", n as f32),
            ("closed_faces", walls as f32),
            ("bounds_lo_x", lo[0]),
            ("bounds_lo_y", lo[1]),
            ("bounds_lo_z", lo[2]),
            ("bounds_hi_x", hi[0]),
            ("bounds_hi_y", hi[1]),
            ("bounds_hi_z", hi[2]),
        ]);
        let (_, errors) = harness.run(
            &mut node,
            &[
                ("bodies", bodies.0),
                ("shapes", shapes.0),
                ("atlas", atlas.0),
            ],
            &[("solid", out.0)],
            &set,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let got: Vec<f32> = read(&harness.buffer(out.0), (n * n * n) as usize);
        let at = |i: u32, j: u32, k: u32| got[(i + n * (j + n * k)) as usize];
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let x = [i, j, k].map(|c| min + c as f32 * h);
                    let want = bounds_distance(x, lo, hi, walls);
                    let have = at(i, j, k);
                    assert!(
                        (have - want).abs() <= 1.0e-5 * want.abs().max(1.0),
                        "walls {walls:#b} node ({i}, {j}, {k}): {have} vs {want}"
                    );
                }
            }
        }
        if walls == 0 {
            assert_eq!(at(3, 3, 3), 1.0e30, "no Wall face reads free space");
        } else {
            // Node (0, 0, 3) at (-1, -1, -0.25) is past the −X Wall by 0.4 m
            // and past the −Y Open face: only the Wall counts.
            assert!((at(0, 0, 3) + 0.4).abs() < 1.0e-5, "{}", at(0, 0, 3));
            // Node (7, 3, 3) at (0.75, -0.25, -0.25) is past the +X Open
            // face and inside the Walls: free space, 0.45 m from the −Z Wall.
            assert!((at(7, 3, 3) - 0.45).abs() < 1.0e-5, "{}", at(7, 3, 3));
        }
    }
}

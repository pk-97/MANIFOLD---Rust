use super::RedistanceLattice;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};

const RADIUS: f32 = 0.6;

/// Node-centred sphere signed distance and the lattice's smallest spacing.
fn sphere_field(nodes: [usize; 3], size: [f32; 3]) -> (Vec<f32>, f32) {
    let spacing = [0, 1, 2].map(|a| size[a] / (nodes[a] - 1) as f32);
    let centre = size.map(|s| s * 0.5);
    let mut field = Vec::with_capacity(nodes[0] * nodes[1] * nodes[2]);
    for z in 0..nodes[2] {
        for y in 0..nodes[1] {
            for x in 0..nodes[0] {
                let d = [x, y, z].map(|a| a as f32);
                let r = (0..3)
                    .map(|a| (d[a] * spacing[a] - centre[a]).powi(2))
                    .sum::<f32>()
                    .sqrt();
                field.push(r - RADIUS);
            }
        }
    }
    (field, spacing[0].min(spacing[1]).min(spacing[2]))
}

fn lattice_params(
    nodes: [usize; 3],
    size: [f32; 3],
    extra: &[(&'static str, f32)],
) -> manifold_node_engine::exec::effect_node::ParamValues {
    let mut all = vec![
        ("nodes_x", nodes[0] as f32),
        ("nodes_y", nodes[1] as f32),
        ("nodes_z", nodes[2] as f32),
        ("size_x", size[0]),
        ("size_y", size[1]),
        ("size_z", size[2]),
    ];
    all.extend_from_slice(extra);
    params(&all)
}

fn redistance(
    h: &mut Harness,
    field: &[f32],
    nodes: [usize; 3],
    size: [f32; 3],
    extra: &[(&'static str, f32)],
) -> Vec<f32> {
    let (input, _) = h.array(field, field.len());
    let (out, out_buf) = h.array::<f32>(&[], field.len());
    let (_, errors) = h.run(
        &mut RedistanceLattice::new(),
        &[("levelset", input)],
        &[("out", out)],
        &lattice_params(nodes, size, extra),
    );
    assert!(errors.is_empty(), "{errors:?}");
    read::<f32>(&out_buf, field.len())
}

#[test]
fn band_in_cells_gives_the_same_depth_in_cells_on_two_lattices() {
    let mut h = Harness::new();
    let size = [2.0, 1.6, 2.4];
    for nodes in [[21, 17, 25], [41, 33, 49]] {
        let (field, spacing) = sphere_field(nodes, size);
        let out = redistance(&mut h, &field, nodes, size, &[("band_cells", 4.0)]);
        let (mut saturated, mut exact) = (0, 0);
        for (i, (&got, &sdf)) in out.iter().zip(&field).enumerate() {
            let cells = sdf.abs() / spacing;
            // The rebuilt surface is off the true sphere by a few hundredths
            // of a cell (see below), so test saturation clear of the band edge.
            if cells > 4.2 {
                saturated += 1;
                assert!(
                    (got.abs() - 4.0 * spacing).abs() < 1e-5 * spacing,
                    "{nodes:?} node {i}: {got} is not +-{}",
                    4.0 * spacing
                );
                assert_eq!(got < 0.0, sdf < 0.0, "{nodes:?} node {i} sign");
            } else if cells < 3.0 {
                exact += 1;
                // Marching cubes places its vertices by linear interpolation
                // along cell edges, so on a coarse lattice the rebuilt
                // sphere is a few hundredths of a cell off the true one.
                assert!(
                    (got - sdf).abs() < 0.1 * spacing,
                    "{nodes:?} node {i}: {got} != {sdf}"
                );
            }
        }
        assert!(saturated > 0 && exact > 0, "{nodes:?}: {saturated} {exact}");
    }
}

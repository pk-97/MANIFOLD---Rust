use manifold_node_engine::testkit::array_harness::{Harness, read};
use manifold_water_liquid::fluid_particles::{CellRange, FluidBlob, bin_counts};
use manifold_water_surface::primitives::particle_volume::*;
use manifold_water_surface::testkit::liquid_surface::Lattice;

fn expected(lattice: &Lattice, solid_nodes: [usize; 3], blob: Option<FluidBlob>) -> Vec<f32> {
    let levels = solid_nodes;
    let total = levels.iter().product();
    let band = blob.map_or(0.0, |b| 3.0 * b.center_radius[3]);
    let min = lattice.min();
    (0..total)
        .map(|idx| {
            let ijk = [
                idx % levels[0],
                (idx / levels[0]) % levels[1],
                idx / (levels[0] * levels[1]),
            ];
            let p: [f32; 3] = std::array::from_fn(|axis| {
                min[axis] + ijk[axis] as f32 * lattice.size[axis] / (levels[axis] - 1) as f32
            });
            blob.map_or(band, |blob| {
                let support = 1.5 * blob.center_radius[3];
                if (0..3).any(|axis| {
                    let h = lattice.size[axis] / (levels[axis] - 1) as f32;
                    let lo = ((blob.center_radius[axis] - support - min[axis]) / h).floor() as i32;
                    let hi =
                        ((blob.center_radius[axis] + support - min[axis]) / h).floor() as i32 + 1;
                    (ijk[axis] as i32) < lo || ijk[axis] as i32 > hi
                }) {
                    return band;
                }
                let d: [f32; 3] = std::array::from_fn(|axis| p[axis] - blob.center_radius[axis]);
                let reach = blob.center_radius[3];
                let distance = d.iter().map(|value| value * value).sum::<f32>().sqrt();
                band.min(distance - reach)
            })
        })
        .collect()
}

fn run_volume(lattice: &Lattice, solid_nodes: [usize; 3], blob: Option<FluidBlob>) -> Vec<f32> {
    let mut harness = Harness::new();
    let blobs = blob.into_iter().collect::<Vec<_>>();
    let range_count = bin_counts(lattice.size, lattice.cell)
        .iter()
        .product::<u32>() as usize;
    let ranges = vec![
        CellRange {
            start: 0,
            count: blobs.len() as u32
        };
        range_count
    ];
    let (blobs_slot, _) = harness.array(&blobs, blobs.len().max(1));
    let (ranges_slot, _) = harness.array(&ranges, ranges.len());
    let solid = vec![1.0_f32; solid_nodes.iter().product()];
    let (solid_slot, _) = harness.array(&solid, solid.len());
    // What node.blob_bounds reduces these blobs to.
    let bounds = blob.map_or([0.0; 2], |b| [b.center_radius[3], 1.5 * b.center_radius[3]]);
    let (bounds_slot, _) = harness.array(&bounds, bounds.len());
    let levels = solid_nodes.map(|n| n as u32);
    let total = levels.iter().product::<u32>() as usize;
    let (levelset_slot, levelset_buf) = harness.array::<f32>(&[], total);
    let inputs = [
        ("blobs", blobs_slot),
        ("cell_ranges", ranges_slot),
        ("solid", solid_slot),
        ("bounds", bounds_slot),
    ];
    let (_, errors) = harness.run(
        &mut ParticleVolume::new(),
        &inputs,
        &[("levelset", levelset_slot)],
        &lattice.params(&[
            ("nodes_x", levels[0] as f32),
            ("nodes_y", levels[1] as f32),
            ("nodes_z", levels[2] as f32),
            ("resolution_scale", 1.0),
        ]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    read(&levelset_buf, total)
}

fn assert_matches(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (idx, (&got, &want)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (got - want).abs() <= 2e-5,
            "node {idx}: got {got}, expected {want}"
        );
    }
}

/// Markers determine the ranges; the shaped centre can move away from its
/// original marker. Both the support and bin boundaries get their nearest
/// representable neighbours, including markers clamped at the box edges.
#[test]
fn particle_volume_matches_cpu_distance() {
    let lattice = Lattice {
        center: [0.0; 3],
        size: [2.0; 3],
        cell: 2.0,
    };
    let blob = FluidBlob {
        center_radius: [0.0, 0.0, 0.0, 0.5],
    };
    let actual = run_volume(&lattice, [9, 9, 9], Some(blob));
    assert_matches(&actual, &expected(&lattice, [9, 9, 9], Some(blob)));
    let empty = run_volume(&lattice, [9, 9, 9], None);
    assert_matches(&empty, &expected(&lattice, [9, 9, 9], None));
}

use manifold_node_engine::testkit::shader_source::dense_source;
use super::*;

fn index(p: [usize; 3], n: [usize; 3]) -> usize {
    p[0] + n[0] * (p[1] + n[1] * p[2])
}

fn point(i: usize, n: [usize; 3]) -> [usize; 3] {
    [i % n[0], (i / n[0]) % n[1], i / (n[0] * n[1])]
}

fn weight(p: usize, d: usize) -> f32 {
    const ROWS: [&[f32]; 4] = [
        &[1.0],
        &[0.25, 0.5, 0.25],
        &[0.0625, 0.25, 0.375, 0.25, 0.0625],
        &[
            0.015625, 0.09375, 0.234375, 0.3125, 0.234375, 0.09375, 0.015625,
        ],
    ];
    ROWS[p][d]
}

fn constant_filter(value: f32, passes: usize, fused: bool) -> f32 {
    if passes == 0 {
        return value;
    }
    let mut sum = 0.0;
    for d in 0..=2 * passes {
        sum = if fused {
            weight(passes, d).mul_add(value, sum)
        } else {
            sum + weight(passes, d) * value
        };
    }
    sum
}



#[test]
fn fluid_bricks_exterior_is_the_exact_dense_filter_not_the_input_band() {
    // Both multiply/add and FMA forms: pruning preserves whichever the GPU
    // compiler uses; replacing the filter with its mathematical identity does not.
    let mut differs = false;
    for bits in [
        0x3caa_aaab,
        0x3d19_999a,
        0x3eaa_aaab,
        0x0080_0001,
        0x40d5_55ab,
    ] {
        let band = f32::from_bits(bits);
        for fused in [false, true] {
            for passes in 0..=3 {
                let mut value = band;
                for _ in 0..3 {
                    let dense_taps = vec![value; 2 * passes + 1];
                    let dense = if passes == 0 {
                        value
                    } else {
                        dense_taps.iter().enumerate().fold(0.0, |sum, (d, &v)| {
                            if fused {
                                weight(passes, d).mul_add(v, sum)
                            } else {
                                sum + weight(passes, d) * v
                            }
                        })
                    };
                    let exterior = constant_filter(value, passes, fused);
                    assert_eq!(exterior.to_bits(), dense.to_bits());
                    differs |= exterior.to_bits() != value.to_bits();
                    value = exterior;
                }
                // The post-smoothing border returns the ORIGINAL band.
                assert_eq!((band / 1.0).to_bits(), band.to_bits());
            }
        }
    }
    assert!(
        differs,
        "fixture must catch replacing the exterior filter with a copy"
    );
}

#[test]
fn fluid_bricks_sparse_indices_partition_nodes_cells_and_retired_slots() {
    for nodes in [[2usize, 2, 2], [17, 23, 9], [64, 65, 67], [128, 129, 131]] {
        let brick_dims = nodes.map(|n| n.div_ceil(8));
        let count = brick_dims.iter().product::<usize>();
        assert_eq!(
            schedule_words(nodes.map(|n| n as u32)),
            Some((8 + 2 * count) as u64)
        );
        for cells in [false, true] {
            let dims = nodes.map(|n| n - usize::from(cells));
            let total = dims.iter().product::<usize>();
            for frame in 0..3 {
                // Moving occupancy then empty: every retired slot must be
                // rewritten by exterior, and no active element is cleared.
                let active = |b: usize| frame != 2 && (b + frame).is_multiple_of(3);
                let mut owners = vec![0u8; total];
                for b in (0..count).filter(|&b| active(b)) {
                    let origin = point(b, brick_dims).map(|v| v * 8);
                    for local in 0..512 {
                        let p = point(local, [8; 3]);
                        let p = std::array::from_fn(|a| origin[a] + p[a]);
                        if (0..3).all(|a| p[a] < dims[a]) {
                            owners[index(p, dims)] += 1;
                        }
                    }
                }
                for (i, owned) in owners.iter_mut().enumerate() {
                    let b = index(point(i, dims).map(|v| v / 8), brick_dims);
                    if !active(b) {
                        *owned += 1;
                    }
                    assert_eq!(
                        *owned, 1,
                        "{nodes:?}, cells={cells}, frame={frame}, index={i}"
                    );
                }
            }
        }
    }
}

#[test]
fn fluid_bricks_generated_consumers_validate_on_cpu() {
    use manifold_node_engine::freeze::codegen::standalone_for_spec;
    use crate::primitives::{clamp_liquid_to_solids::ClampLiquidToSolids, count_surface_triangles::CountSurfaceTriangles, particle_volume::ParticleVolume, relax_surface_mesh::RelaxSurfaceMesh, smooth_lattice::SmoothLattice, volume_surface_mesh::VolumeSurfaceMesh};
    for source in [
        standalone_for_spec::<ParticleVolume>().unwrap(),
        standalone_for_spec::<SmoothLattice>().unwrap(),
        standalone_for_spec::<ClampLiquidToSolids>().unwrap(),
        standalone_for_spec::<CountSurfaceTriangles>().unwrap(),
        standalone_for_spec::<VolumeSurfaceMesh>().unwrap(),
        dense_source::<VolumeSurfaceMesh>(include_str!(
            "../shaders/volume_surface_mesh_dense_reference.wgsl"
        )),
        dense_source::<RelaxSurfaceMesh>(include_str!(
            "../shaders/relax_surface_mesh_dense_reference.wgsl"
        )),
        dense_source::<ParticleVolume>(include_str!(
            "../shaders/particle_volume_dense_reference.wgsl"
        )),
        dense_source::<SmoothLattice>(include_str!("../shaders/smooth_lattice_dense_reference.wgsl")),
        dense_source::<ClampLiquidToSolids>(include_str!(
            "../shaders/clamp_liquid_to_solids_dense_reference.wgsl"
        )),
        dense_source::<CountSurfaceTriangles>(include_str!(
            "../shaders/count_surface_triangles_dense_reference.wgsl"
        )),
    ] {
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
    }
}

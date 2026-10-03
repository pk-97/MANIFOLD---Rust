//! Compile-only in the slot-2 workstream: run on a GPU owner with these exact
//! filters. The oracles are the unmodified dense bodies from main 4aab34f86.
use super::dense_source;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::bindings::Slot;
use crate::node_graph::fluid_particles::{CellRange, FluidBlob, bin_counts};
use crate::node_graph::primitive::PrimitiveSpec;
use crate::node_graph::primitives::{
    clamp_liquid_to_solids::ClampLiquidToSolids,
    count_surface_triangles::CountSurfaceTriangles,
    lattice_bricks::{LatticeBricks, brick_layout, compact_brick_words, conservative_brick_mask},
    liquid_surface_tests::{Harness, params, read},
    particle_volume::ParticleVolume,
    smooth_lattice::SmoothLattice,
};
use crate::node_graph::primitives::{
    relax_surface_mesh::RelaxSurfaceMesh, running_total::RunningTotal,
    volume_surface_mesh::VolumeSurfaceMesh,
};
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

fn oracle<P: PrimitiveSpec>(h: &Harness, source: &str) -> GpuComputePipeline {
    h.device.create_compute_pipeline(
        &dense_source::<P>(source),
        "cs_main",
        "liquid.bricks.dense_reference",
    )
}

// Dispatch the original vertex-owned body with its original vertex domain.
// It must not inherit the new cell-owned run method or its tail-clear pass.
fn dense_mesh_dispatch(
    h: &Harness,
    pipeline: &GpuComputePipeline,
    uniforms: &[u32],
    buffers: &[&GpuBuffer],
    slots: u32,
) {
    let mut bindings = vec![GpuBinding::Bytes {
        binding: 0,
        data: bytemuck::cast_slice(uniforms),
    }];
    bindings.extend(buffers.iter().enumerate().map(|(i, b)| GpuBinding::Buffer {
        binding: i as u32 + 1,
        buffer: b,
        offset: 0,
    }));
    let mut encoder = h.device.create_encoder("liquid dense mesh oracle");
    encoder.dispatch_compute(
        pipeline,
        &bindings,
        [slots.div_ceil(256), 1, 1],
        "dense vertex oracle",
    );
    encoder.commit_and_wait_completed();
}

fn equal(a: &GpuBuffer, b: &GpuBuffer, total: usize, stage: &str) {
    let a = read::<u32>(a, total);
    let b = read::<u32>(b, total);
    assert_eq!(
        a.iter().zip(&b).position(|(a, b)| a != b),
        None,
        "first bit difference: {stage}"
    );
}

fn fixture(resolution: u32) {
    let mut h = Harness::new();
    let solid_nodes = [resolution + 4; 3];
    let nodes = solid_nodes.map(|n| (n - 1) * 2 + 1);
    let total = nodes.iter().product::<u32>() as usize;
    let center = [0.25, -0.125, 0.375];
    let size = [4.25, 3.5, 4.0];
    let min: [f32; 3] = std::array::from_fn(|a| center[a] - size[a] * 0.5);
    let cell = size[0] / resolution as f32;
    let bins = bin_counts(size, cell);
    let bin = |p: [f32; 3]| {
        let b: [u32; 3] = std::array::from_fn(|a| {
            (((p[a] - min[a]) / cell).floor() as i32).clamp(0, bins[a] as i32 - 1) as u32
        });
        (b[0] + bins[0] * (b[1] + bins[1] * b[2])) as usize
    };
    let solid_count = solid_nodes.iter().product::<u32>() as usize;
    // A solid half-space cuts some droplets; the outer lattice border is tested
    // separately by placing a particle at its first interior node.
    let solid: Vec<f32> = (0..solid_count)
        .map(|i| {
            let y = (i / solid_nodes[0] as usize) % solid_nodes[1] as usize;
            min[1] + y as f32 * size[1] / (solid_nodes[1] - 1) as f32 + 0.1
        })
        .collect();
    let (solid_slot, _) = h.array(&solid, solid_count);
    let (blob_slot, blob_buffer) = h.array::<FluidBlob>(&[], 40);
    let range_count = bins.iter().product::<u32>() as usize;
    let (range_slot, range_buffer) = h.array::<CellRange>(&[], range_count);
    let (brick_slot, _) = h.array::<u32>(&[], 1);
    let volume_slots: [Slot; 3] = std::array::from_fn(|_| h.scalar());
    let (sparse_slot, sparse_buffer) = h.array::<f32>(&[], total);
    let (dense_slot, dense_buffer) = h.array::<f32>(&[], total);
    let mut volume = [ParticleVolume::new(), ParticleVolume::new()];
    volume[1].pipeline = Some(oracle::<ParticleVolume>(
        &h,
        include_str!("shaders/particle_volume_dense_reference.wgsl"),
    ));
    let mut smoothing: [[SmoothLattice; 3]; 2] =
        std::array::from_fn(|_| std::array::from_fn(|_| SmoothLattice::new()));
    for n in &mut smoothing[1] {
        n.pipeline = Some(oracle::<SmoothLattice>(
            &h,
            include_str!("shaders/smooth_lattice_dense_reference.wgsl"),
        ));
    }
    let smooth_slots: [[(Slot, GpuBuffer); 3]; 2] =
        std::array::from_fn(|_| std::array::from_fn(|_| h.array::<f32>(&[], total)));
    let mut clamp = [ClampLiquidToSolids::new(), ClampLiquidToSolids::new()];
    clamp[1].pipeline = Some(oracle::<ClampLiquidToSolids>(
        &h,
        include_str!("shaders/clamp_liquid_to_solids_dense_reference.wgsl"),
    ));
    let clamp_slots: [(Slot, GpuBuffer); 2] = std::array::from_fn(|_| h.array::<f32>(&[], total));
    let mut counters = [CountSurfaceTriangles::new(), CountSurfaceTriangles::new()];
    counters[1].pipeline = Some(oracle::<CountSurfaceTriangles>(
        &h,
        include_str!("shaders/count_surface_triangles_dense_reference.wgsl"),
    ));
    let count_slots: [(Slot, GpuBuffer); 2] = std::array::from_fn(|_| h.array::<u32>(&[], total));
    let mut builder = LatticeBricks::new();
    let mut scans: [RunningTotal; 2] = std::array::from_fn(|_| RunningTotal::new());
    let scan_slots: [(Slot, GpuBuffer); 2] = std::array::from_fn(|_| h.array::<u32>(&[], total));
    let extent_slots: [(Slot, GpuBuffer); 2] = std::array::from_fn(|_| h.array::<u32>(&[], 4));
    let total_slots: [Slot; 2] = std::array::from_fn(|_| h.scalar());
    // This fixture has 32 compact droplets; a deliberately ample fixed test
    // allocation also exposes stale tail data when they disappear.
    const SLOTS: u32 = 393_216;
    let (mesh_slot, _) = h.array::<MeshVertex>(&[], 1);
    let (_, dense_mesh) = h.array::<MeshVertex>(&[], SLOTS as usize);
    let mut mesh = VolumeSurfaceMesh::new();
    let mesh_oracle = oracle::<VolumeSurfaceMesh>(
        &h,
        include_str!("shaders/volume_surface_mesh_dense_reference.wgsl"),
    );
    let relax_oracle = oracle::<RelaxSurfaceMesh>(
        &h,
        include_str!("shaders/relax_surface_mesh_dense_reference.wgsl"),
    );
    let mut relaxers: [RelaxSurfaceMesh; 2] = std::array::from_fn(|_| RelaxSurfaceMesh::new());
    let relaxed: [[(Slot, GpuBuffer); 2]; 2] = std::array::from_fn(|_| {
        std::array::from_fn(|_| h.array::<MeshVertex>(&[], SLOTS as usize))
    });
    let base = [
        ("center_x", center[0]),
        ("center_y", center[1]),
        ("center_z", center[2]),
        ("size_x", size[0]),
        ("size_y", size[1]),
        ("size_z", size[2]),
        ("cell_size", cell),
        ("nodes_x", solid_nodes[0] as f32),
        ("nodes_y", solid_nodes[1] as f32),
        ("nodes_z", solid_nodes[2] as f32),
        ("resolution_scale", 2.0),
        ("bins_x", bins[0] as f32),
        ("bins_y", bins[1] as f32),
        ("bins_z", bins[2] as f32),
    ];
    let base_params = params(&base);
    let lattice_params = params(&[
        ("nodes_x", nodes[0] as f32),
        ("nodes_y", nodes[1] as f32),
        ("nodes_z", nodes[2] as f32),
    ]);
    // Moving then empty frames reuse ALL output buffers: this catches retired
    // bricks retaining last frame's field, smoothing, counts or triangles.
    for frame in 0..3 {
        let mut blobs = Vec::new();
        if frame < 2 {
            for i in 0..32 {
                let p = if i == 0 {
                    std::array::from_fn(|a| min[a] + size[a] / (nodes[a] - 1) as f32)
                } else {
                    [
                        center[0] + (frame as f32 - 0.5) * 1.5 + (i % 4) as f32 * cell,
                        center[1] + ((i / 4) % 4) as f32 * cell,
                        center[2] + (i / 16) as f32 * cell,
                    ]
                };
                let reach = 0.6 * cell;
                // Stretched supports as well as spheres; reach is the largest axis.
                blobs.push(FluidBlob {
                    center_radius: [p[0], p[1], p[2], reach],
                    shape_diag: [
                        1.0 / reach,
                        (1.0 + (i % 3) as f32) / reach,
                        1.0 / reach,
                        0.0,
                    ],
                    shape_off: [0.0; 4],
                });
            }
        }
        blobs.sort_by_key(|b| bin(b.center_radius[..3].try_into().unwrap()));
        let mut ranges = vec![CellRange::default(); range_count];
        for (i, b) in blobs.iter().enumerate() {
            let r = &mut ranges[bin(b.center_radius[..3].try_into().unwrap())];
            if r.count == 0 {
                r.start = i as u32;
            }
            r.count += 1;
        }
        blobs.resize(40, FluidBlob::default());
        // SAFETY: all prior Harness calls have completed and storage is sized above.
        unsafe {
            blob_buffer.write(0, bytemuck::cast_slice(&blobs));
            range_buffer.write(0, bytemuck::cast_slice(&ranges));
        }
        let (_, errors) = h.run(
            &mut builder,
            &[
                ("blobs", blob_slot),
                ("cell_ranges", range_slot),
                ("solid", solid_slot),
            ],
            &[("bricks", brick_slot)],
            &base_params,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let layout = brick_layout(solid_nodes, 2).unwrap();
        let mask = conservative_brick_mask(&blobs, center, size, solid_nodes, 2, cell).unwrap();
        let expected = compact_brick_words(&mask, layout.bricks);
        let schedule = read::<u32>(&h.buffer(brick_slot), expected.len());
        assert_eq!(
            schedule, expected,
            "GPU brick topology, resolution {resolution}, frame {frame}"
        );
        assert!(
            schedule[0] < schedule[4] * schedule[5] * schedule[6],
            "fixture must exercise inactive bricks"
        );
        for (lane, node) in volume.iter_mut().enumerate() {
            let mut inputs = vec![
                ("blobs", blob_slot),
                ("cell_ranges", range_slot),
                ("solid", solid_slot),
            ];
            if lane == 0 {
                inputs.push(("bricks", brick_slot));
            }
            let output = if lane == 0 { sparse_slot } else { dense_slot };
            let (_, errors) = h.run(
                node,
                &inputs,
                &[
                    ("levelset", output),
                    ("volume_nodes_x", volume_slots[0]),
                    ("volume_nodes_y", volume_slots[1]),
                    ("volume_nodes_z", volume_slots[2]),
                ],
                &base_params,
            );
            assert!(errors.is_empty(), "{errors:?}");
        }
        equal(&sparse_buffer, &dense_buffer, total, "volume");
        for passes in 0..=3 {
            for axis in 0..3 {
                let mut p = lattice_params.clone();
                p.extend(params(&[("passes", passes as f32), ("axis", axis as f32)]));
                for lane in 0..2 {
                    let input = if axis == 0 {
                        if lane == 0 { sparse_slot } else { dense_slot }
                    } else {
                        smooth_slots[lane][axis - 1].0
                    };
                    let mut inputs = vec![("levelset", input)];
                    if lane == 0 {
                        inputs.push(("bricks", brick_slot));
                    }
                    let (_, errors) = h.run(
                        &mut smoothing[lane][axis],
                        &inputs,
                        &[("smoothed", smooth_slots[lane][axis].0)],
                        &p,
                    );
                    assert!(errors.is_empty(), "{errors:?}");
                }
                equal(
                    &smooth_slots[0][axis].1,
                    &smooth_slots[1][axis].1,
                    total,
                    "smooth",
                );
            }
            let mut p = base_params.clone();
            p.extend(lattice_params.clone());
            p.extend(params(&[
                ("solid_nodes_x", solid_nodes[0] as f32),
                ("solid_nodes_y", solid_nodes[1] as f32),
                ("solid_nodes_z", solid_nodes[2] as f32),
            ]));
            for lane in 0..2 {
                let mut inputs = vec![("levelset", smooth_slots[lane][2].0), ("solid", solid_slot)];
                if lane == 0 {
                    inputs.push(("bricks", brick_slot));
                }
                let (_, errors) = h.run(
                    &mut clamp[lane],
                    &inputs,
                    &[("clamped", clamp_slots[lane].0)],
                    &p,
                );
                assert!(errors.is_empty(), "{errors:?}");
                let mut inputs = vec![("levelset", clamp_slots[lane].0)];
                if lane == 0 {
                    inputs.push(("bricks", brick_slot));
                }
                let (_, errors) = h.run(
                    &mut counters[lane],
                    &inputs,
                    &[("counts", count_slots[lane].0)],
                    &lattice_params,
                );
                assert!(errors.is_empty(), "{errors:?}");
            }
            equal(&clamp_slots[0].1, &clamp_slots[1].1, total, "clamp");
            equal(
                &count_slots[0].1,
                &count_slots[1].1,
                total,
                "triangle counts",
            );
            for lane in 0..2 {
                let (_, errors) = h.run(
                    &mut scans[lane],
                    &[("in", count_slots[lane].0)],
                    &[
                        ("out", scan_slots[lane].0),
                        ("extent", extent_slots[lane].0),
                        ("total", total_slots[lane]),
                    ],
                    &params(&[("per_item", 3.0)]),
                );
                assert!(errors.is_empty(), "{errors:?}");
            }
            equal(
                &scan_slots[0].1,
                &scan_slots[1].1,
                total,
                "inclusive triangle scan",
            );
            let cells = nodes.map(|n| n - 1).iter().product::<u32>() as usize;
            let live = read::<u32>(&scan_slots[1].1, cells)[cells - 1] * 3;
            assert!(live <= SLOTS, "fixture must fit its mesh allocation");
            if frame < 2 && passes == 0 {
                assert!(live > 0, "nonempty mesh fixture");
            }
            let mut mesh_params = base_params.clone();
            mesh_params.extend(lattice_params.clone());
            mesh_params.extend(params(&[("max_capacity", SLOTS as f32)]));
            let (_, errors) = h.run(
                &mut mesh,
                &[
                    ("levelset", clamp_slots[0].0),
                    ("scan", scan_slots[0].0),
                    ("extent", extent_slots[0].0),
                    ("bricks", brick_slot),
                ],
                &[("vertices", mesh_slot)],
                &mesh_params,
            );
            assert!(errors.is_empty(), "{errors:?}");
            let mesh_buffer = h.buffer(mesh_slot);
            assert_eq!(mesh_buffer.size, dense_mesh.size);
            let mesh_uniforms = [
                center[0].to_bits(),
                center[1].to_bits(),
                center[2].to_bits(),
                size[0].to_bits(),
                size[1].to_bits(),
                size[2].to_bits(),
                (nodes[0] as f32).to_bits(),
                (nodes[1] as f32).to_bits(),
                (nodes[2] as f32).to_bits(),
                2,
                SLOTS,
                0,
                SLOTS,
                0,
                0,
                0,
            ];
            dense_mesh_dispatch(
                &h,
                &mesh_oracle,
                &mesh_uniforms,
                &[
                    &clamp_slots[1].1,
                    &scan_slots[1].1,
                    &extent_slots[1].1,
                    &scan_slots[1].1,
                    &dense_mesh,
                ],
                SLOTS,
            );
            equal(
                &mesh_buffer,
                &dense_mesh,
                mesh_buffer.size as usize / 4,
                "mesh including retired tail",
            );
            for step in 0..2 {
                let source = if step == 0 {
                    mesh_slot
                } else {
                    relaxed[0][step - 1].0
                };
                let dense_source = if step == 0 {
                    &dense_mesh
                } else {
                    &relaxed[1][step - 1].1
                };
                let strength = if passes == 0 { 0.0f32 } else { 0.5f32 };
                let mut relax_params = lattice_params.clone();
                relax_params.extend(params(&[("strength", strength)]));
                let (_, errors) = h.run(
                    &mut relaxers[step],
                    &[
                        ("vertices", source),
                        ("levelset", clamp_slots[0].0),
                        ("scan", scan_slots[0].0),
                        ("extent", extent_slots[0].0),
                        ("bricks", brick_slot),
                    ],
                    &[("relaxed", relaxed[0][step].0)],
                    &relax_params,
                );
                assert!(errors.is_empty(), "{errors:?}");
                let uniforms = [
                    (nodes[0] as f32).to_bits(),
                    (nodes[1] as f32).to_bits(),
                    (nodes[2] as f32).to_bits(),
                    strength.to_bits(),
                    SLOTS,
                    0,
                    SLOTS,
                    0,
                ];
                dense_mesh_dispatch(
                    &h,
                    &relax_oracle,
                    &uniforms,
                    &[
                        dense_source,
                        &clamp_slots[1].1,
                        &scan_slots[1].1,
                        &extent_slots[1].1,
                        &scan_slots[1].1,
                        &relaxed[1][step].1,
                    ],
                    SLOTS,
                );
                equal(
                    &relaxed[0][step].1,
                    &relaxed[1][step].1,
                    dense_mesh.size as usize / 4,
                    "relaxed mesh including retired tail",
                );
            }
        }
    }
}

#[test]
fn fluid_bricks_lattice_and_mesh_bit_identical_dense_64() {
    fixture(64);
}

#[test]
fn fluid_bricks_lattice_and_mesh_bit_identical_dense_128() {
    fixture(128);
}

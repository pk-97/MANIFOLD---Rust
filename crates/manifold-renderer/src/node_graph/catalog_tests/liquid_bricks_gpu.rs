//! Sparse field proofs against independent dense gathers. The field oracle
//! follows native ParticleMesher support and production solid/border semantics.
use manifold_node_engine::water::primitives::testkit as water_nodes;
use manifold_node_engine::testkit::shader_source::dense_source;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::water::fluid_particles::{CellRange, FluidBlob, bin_counts};
use manifold_node_engine::freeze::codegen::ENTRY;
use manifold_node_engine::primitive::PrimitiveSpec;
use manifold_node_engine::water::primitives::{lattice_bricks::LatticeBricks, lattice_bricks::brick_layout, lattice_bricks::compact_brick_words, lattice_bricks::conservative_brick_mask, particle_volume::ParticleVolume};
use manifold_node_engine::water::primitives::volume_surface_mesh::VolumeSurfaceMesh;
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

/// Vertex buffers compare bit-for-bit except the two alignment pads after
/// `position` and `normal`: a whole-struct store copies whatever the kernel's
/// stack held there, which depends on how the shader compiler inlined the
/// body, and nothing reads it.
fn equal_vertices(a: &GpuBuffer, b: &GpuBuffer, total: usize, stage: &str) {
    const WORDS: usize = std::mem::size_of::<MeshVertex>() / 4;
    const PAD_LANES: [usize; 2] = [3, 7];
    let a = read::<u32>(a, total);
    let b = read::<u32>(b, total);
    assert_eq!(
        a.iter()
            .zip(&b)
            .enumerate()
            .position(|(i, (a, b))| a != b && !PAD_LANES.contains(&(i % WORDS))),
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
        include_str!("../../../../manifold-node-engine/src/water/primitives/shaders/particle_volume_dense_reference.wgsl"),
    ));
    let mut smoothing: [[_; 3]; 2] = std::array::from_fn(|lane| std::array::from_fn(|_| {
        water_nodes::smooth_lattice((lane == 1).then(|| water_nodes::dense_pipeline("smooth_lattice", &h.device,
            include_str!("../../../../manifold-node-engine/src/water/primitives/shaders/smooth_lattice_dense_reference.wgsl"))))
    }));
    let smooth_slots: [[(Slot, GpuBuffer); 3]; 2] =
        std::array::from_fn(|_| std::array::from_fn(|_| h.array::<f32>(&[], total)));
    let mut clamp = [water_nodes::clamp_liquid_to_solids(None), water_nodes::clamp_liquid_to_solids(Some(water_nodes::dense_pipeline("clamp_liquid_to_solids", &h.device,
        include_str!("../../../../manifold-node-engine/src/water/primitives/shaders/clamp_liquid_to_solids_dense_reference.wgsl"))))];
    let clamp_slots: [(Slot, GpuBuffer); 2] = std::array::from_fn(|_| h.array::<f32>(&[], total));
    let mut counters = [water_nodes::count_surface_triangles(None), water_nodes::count_surface_triangles(Some(water_nodes::dense_pipeline("count_surface_triangles", &h.device,
        include_str!("../../../../manifold-node-engine/src/water/primitives/shaders/count_surface_triangles_dense_reference.wgsl"))))];
    let count_slots: [(Slot, GpuBuffer); 2] = std::array::from_fn(|_| h.array::<u32>(&[], total));
    let mut builder = LatticeBricks::new();
    let (bounds_slot, _) = h.array::<f32>(&[], 2);
    let mut bounder = manifold_node_engine::water::primitives::blob_bounds::BlobBounds::new();
    manifold_node_engine::primitive::Primitive::prepare_pipelines(&mut bounder, &h.device);
    let mut scans: [_; 2] = std::array::from_fn(|_| water_nodes::running_total());
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
        include_str!("../../../../manifold-node-engine/src/water/primitives/shaders/volume_surface_mesh_dense_reference.wgsl"),
    );
    let relax_oracle = water_nodes::dense_pipeline("relax_surface_mesh",
        &h.device,
        include_str!("../../../../manifold-node-engine/src/water/primitives/shaders/relax_surface_mesh_dense_reference.wgsl"),
    );
    let mut relaxers: [_; 2] = std::array::from_fn(|_| water_nodes::relax_surface_mesh());
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
                // Native scale-3 radius exceeds the former two-thirds-bin cap.
                let reach = (3.0 * 0.31017524) * cell;
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
        let (_, errors) = h.run(&mut bounder, &[("blobs", blob_slot)], &[("bounds", bounds_slot)], &params(&[]));
        assert!(errors.is_empty(), "{errors:?}");
        let (_, errors) = h.run(
            &mut builder,
            &[
                ("blobs", blob_slot),
                ("bounds", bounds_slot),
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
                ("bounds", bounds_slot),
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
                0,
                0,
                0,
                2,
                SLOTS,
                0,
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
                    &scan_slots[1].1,
                    &scan_slots[1].1,
                    &dense_mesh,
                ],
                SLOTS,
            );
            equal_vertices(
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
                    0,
                    SLOTS,
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
                        &scan_slots[1].1,
                        &relaxed[1][step].1,
                    ],
                    SLOTS,
                );
                equal_vertices(
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

#[test]
fn fluid_smooth_clamp_dense_fusion_matches_unfused() {
    // This uses the same partitioner and installer as the live freeze path,
    // rather than proving a hand-built pair of atoms in isolation.
    let def = manifold_node_engine::water::primitives::gpu_flip_preset::render_def(
        manifold_node_engine::water::primitives::gpu_flip_preset::WaterScene::dam_break(64),
    );
    let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    let report = manifold_node_engine::freeze::fusion_report(&def, &registry);
    let smooth_clamp = ["node.smooth_lattice", "node.clamp_liquid_to_solids"];
    assert!(
        report.regions.iter().any(|region| {
            let members: Vec<_> = region
                .member_node_ids
                .iter()
                .map(|id| {
                    report
                        .nodes
                        .iter()
                        .find(|node| node.node_id == *id)
                        .expect("region member is in the fusion report")
                        .type_id
                        .as_str()
                })
                .collect();
            members.as_slice() == &smooth_clamp[..]
        }),
        "the live liquid surface chain must contain a smooth → clamp region: {:?}",
        report.regions,
    );
    let fused = manifold_node_engine::freeze::install::fuse_generator_view(&def, &registry)
        .expect("the liquid surface graph installs its fused region");
    assert!(
        fused
            .def
            .nodes
            .iter()
            .any(|node| node.type_id == "node.wgsl_compute"),
        "the installed graph must retain a generated fused node"
    );

    const NODES: [u32; 3] = [8; 3];
    const CENTER: [f32; 3] = [0.25, 1.0, -0.5];
    const SIZE: [f32; 3] = [2.0, 1.5, 2.5];
    const SOLID_NODES: [u32; 3] = [4; 3];
    const CELL_SIZE: f32 = 0.25;
    let total = NODES.iter().product::<u32>() as usize;
    let levelset: Vec<f32> = (0..total)
        .map(|idx| {
            let x = idx as u32 % NODES[0];
            let y = (idx as u32 / NODES[0]) % NODES[1];
            let z = idx as u32 / (NODES[0] * NODES[1]);
            ((x * 17 + y * 13 + z * 7) % 11) as f32 / 5.0 - 1.0
        })
        .collect();
    // A signed half-space intersects the interior; the clamp's separate border
    // rule is exercised by every face of this lattice.
    let solid_total = SOLID_NODES.iter().product::<u32>() as usize;
    let solid: Vec<f32> = (0..solid_total)
        .map(|idx| {
            let y = (idx as u32 / SOLID_NODES[0]) % SOLID_NODES[1];
            y as f32 - 1.5
        })
        .collect();

    let mut harness = Harness::new();
    let (levelset_slot, levelset_buffer) = harness.array(&levelset, total);
    let (solid_slot, solid_buffer) = harness.array(&solid, solid_total);
    let layout = brick_layout(NODES, 1).expect("8³ lattice has a brick layout");
    let mask = vec![1; layout.count as usize];
    let schedule = compact_brick_words(&mask, layout.bricks);
    let (brick_slot, _) = harness.array(&schedule, layout.words as usize);
    let (smooth_slot, _) = harness.array::<f32>(&[], total);
    let (clamp_slot, clamp_buffer) = harness.array::<f32>(&[], total);
    let (_, fused_buffer) = harness.array::<f32>(&[], total);

    let fused_source = fused
        .def
        .nodes
        .iter()
        .find(|node| {
            node.type_id == "node.wgsl_compute"
                && node.wgsl_source.as_deref().is_some_and(|source| {
                    source.contains("n0_passes") && source.contains("n1_cell_size")
                })
        })
        .and_then(|node| node.wgsl_source.as_deref())
        .expect("installed smooth → clamp node carries its generated WGSL");
    // Clamp's FromInput declaration must survive region construction: the
    // actual kernel dispatches from levelset capacity even when solid is 4³.
    assert!(
        fused_source.contains("@fused_output_capacity")
            && fused_source.contains("arrayLength(&src_0)"),
        "installed smooth → clamp kernel must preserve its levelset capacity expression"
    );

    let params_for = |passes: u32, axis: u32| {
        let mut values = params(&[
            ("nodes_x", NODES[0] as f32),
            ("nodes_y", NODES[1] as f32),
            ("nodes_z", NODES[2] as f32),
            ("passes", passes as f32),
            ("axis", axis as f32),
        ]);
        values.extend(params(&[
            ("center_x", CENTER[0]),
            ("center_y", CENTER[1]),
            ("center_z", CENTER[2]),
            ("size_x", SIZE[0]),
            ("size_y", SIZE[1]),
            ("size_z", SIZE[2]),
            ("nodes_x", NODES[0] as f32),
            ("nodes_y", NODES[1] as f32),
            ("nodes_z", NODES[2] as f32),
            ("solid_nodes_x", SOLID_NODES[0] as f32),
            ("solid_nodes_y", SOLID_NODES[1] as f32),
            ("solid_nodes_z", SOLID_NODES[2] as f32),
            ("cell_size", CELL_SIZE),
        ]));
        values
    };

    for field in [
        "n0_nodes_x",
        "n0_nodes_y",
        "n0_nodes_z",
        "n0_passes",
        "n0_axis",
        "n1_center_x",
        "n1_center_y",
        "n1_center_z",
        "n1_size_x",
        "n1_size_y",
        "n1_size_z",
        "n1_nodes_x",
        "n1_nodes_y",
        "n1_nodes_z",
        "n1_solid_nodes_x",
        "n1_solid_nodes_y",
        "n1_solid_nodes_z",
        "n1_cell_size",
    ] {
        assert!(
            fused_source.contains(field),
            "installed kernel missing `{field}`"
        );
    }
    let mut words = vec![
        (NODES[0] as f32).to_bits(),
        (NODES[1] as f32).to_bits(),
        (NODES[2] as f32).to_bits(),
        0.0f32.to_bits(),
        0,
        CENTER[0].to_bits(),
        CENTER[1].to_bits(),
        CENTER[2].to_bits(),
        SIZE[0].to_bits(),
        SIZE[1].to_bits(),
        SIZE[2].to_bits(),
        (NODES[0] as f32).to_bits(),
        (NODES[1] as f32).to_bits(),
        (NODES[2] as f32).to_bits(),
        (SOLID_NODES[0] as f32).to_bits(),
        (SOLID_NODES[1] as f32).to_bits(),
        (SOLID_NODES[2] as f32).to_bits(),
        CELL_SIZE.to_bits(),
    ];
    while !words.len().is_multiple_of(4) {
        words.push(0);
    }

    let pipeline = harness.device.create_compute_pipeline(
        fused_source,
        ENTRY,
        "liquid-smooth-clamp-dense-fused",
    );
    let mut smooth = water_nodes::smooth_lattice(None);
    let mut clamp = water_nodes::clamp_liquid_to_solids(None);
    for passes in 0..=3 {
        for axis in 0..3 {
            let p = params_for(passes, axis);
            let (_, errors) = harness.run(
                &mut smooth,
                &[("levelset", levelset_slot), ("bricks", brick_slot)],
                &[("smoothed", smooth_slot)],
                &p,
            );
            assert!(errors.is_empty(), "smooth {passes}/{axis}: {errors:?}");
            let (_, errors) = harness.run(
                &mut clamp,
                &[
                    ("levelset", smooth_slot),
                    ("solid", solid_slot),
                    ("bricks", brick_slot),
                ],
                &[("clamped", clamp_slot)],
                &p,
            );
            assert!(errors.is_empty(), "clamp {passes}/{axis}: {errors:?}");

            let mut fused_words = words.clone();
            fused_words[3] = (passes as f32).to_bits();
            fused_words[4] = axis as i32 as u32;
            let mut encoder = harness
                .device
                .create_encoder("liquid-smooth-clamp-dense-fused");
            encoder.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::cast_slice(&fused_words),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: &levelset_buffer,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: &solid_buffer,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: &fused_buffer,
                        offset: 0,
                    },
                ],
                [(total as u32).div_ceil(256), 1, 1],
                "liquid-smooth-clamp-dense-fused",
            );
            encoder.commit_and_wait_completed();

            let unfused = read::<f32>(&clamp_buffer, total);
            let fused = read::<f32>(&fused_buffer, total);
            for (idx, (got, want)) in fused.iter().zip(&unfused).enumerate() {
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "smooth passes {passes}, axis {axis}, element {idx}: fused {got} standalone {want}"
                );
            }
        }
    }
}

/// Particles for the scatter proofs in a lattice of any size: eight per cell
/// in a column filling the lower corner, a thin sheet thrown off it, and
/// loose drops. Without spray the frame is just the column.
fn scatter_fixture(
    lattice: &manifold_node_engine::testkit::liquid_surface::Lattice,
    resolution: u32,
    spray: bool,
    seed: u64,
) -> Vec<manifold_node_engine::water::fluid_particles::FluidParticle> {
    use manifold_node_engine::water::fluid_particles::FluidParticle;
    let min = lattice.min();
    let unit = lattice.size[0] / 4.0;
    let marker = 0.310_175_25 * lattice.cell;
    let mut state = seed;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 40) as f32 / (1u64 << 24) as f32
    };
    let mut particles = Vec::new();
    let mut push = |p: [f32; 3]| {
        let id = particles.len() as u32 + 1;
        particles.push(FluidParticle { position_radius: [p[0], p[1], p[2], marker], velocity: [0.0; 3], id });
    };
    for z in 0..(resolution * 2 / 5) * 2 {
        for y in 0..(resolution * 2 / 5) * 2 {
            for x in 0..(resolution / 4) * 2 {
                let at = [x, y, z].map(|i| (i as f32 + 0.5) * 0.5 * lattice.cell);
                push(std::array::from_fn(|a| min[a] + at[a] + (next() - 0.5) * 0.2 * lattice.cell));
            }
        }
    }
    if !spray {
        return particles;
    }
    for i in 0..6000 {
        let t = i as f32 / 6000.0;
        push([
            min[0] + unit * (1.2 + 1.5 * t),
            min[1] + unit * (1.0 + 0.6 * (t * 9.0).sin().abs()),
            min[2] + unit * (3.6 * next() + 0.2),
        ]);
    }
    // Few enough that empty bricks remain between them at Surface Detail 0.
    for _ in 0..60 {
        push(std::array::from_fn(|a| min[a] + unit * 0.05 + next() * (lattice.size[a] - unit * 0.1)));
    }
    particles
}

/// The per-blob scatter marks exactly the bricks the per-brick gather it
/// replaced marked: a settled block, a sheet thrown off it and stray drops,
/// sorted and shaped by the shipped nodes, with and without the distance
/// band, at Surface Detail 0 and 1. Each node instance runs a second frame
/// with only the block, so a hit word left from the first frame shows up as a
/// stale brick. The second lattice sits far from the origin, where world
/// coordinates round coarsely against the node spacing.
#[test]
fn fluid_bricks_scatter_mask_is_the_gather_mask() {
    use manifold_node_engine::testkit::liquid_surface::{Lattice, sort_and_shape};
use crate::node_graph::catalog_tests::liquid_surface::blob_bounds;

    let mut h = Harness::new();
    let resolution = 64u32;
    let gather = h.device.create_compute_pipeline(
        include_str!("../../../../manifold-node-engine/src/water/primitives/shaders/lattice_bricks_gather_reference.wgsl"),
        "mark_bricks",
        "liquid.bricks.gather_reference",
    );
    let lattices = [
        Lattice { center: [0.0, 0.5, 0.0], size: [4.0, 4.0, 4.0], cell: 4.0 / resolution as f32 },
        Lattice { center: [1000.0, -1000.0, 1000.0], size: [0.25, 0.25, 0.25], cell: 0.25 / resolution as f32 },
    ];
    for lattice in &lattices {
        let marker = 0.310_175_25 * lattice.cell;
        let frames: Vec<_> = [true, false].iter().map(|&spray| {
            let particles = scatter_fixture(lattice, resolution, spray, 0x9e37_79b9_7f4a_7c15);
            let count = particles.len();
            let (_, _, _, (_, ranges_slot, blobs_slot)) = sort_and_shape(&mut h, lattice, &particles, count, &[("particle_scale", 3.0)]);
            (ranges_slot, blobs_slot, blob_bounds(&mut h, blobs_slot))
        }).collect();
        let solid_nodes = [resolution + 4; 3];
        let (solid_slot, _) = h.array::<f32>(&[], solid_nodes.iter().product::<u32>() as usize);
        let bins = bin_counts(lattice.size, lattice.cell);
        for scale in [1u32, 2] {
            for band in [0.0, 3.0 * marker] {
                let layout = brick_layout(solid_nodes, scale).unwrap();
                let label = format!("lattice {:?} scale {scale} band {band}", lattice.center);
                let mut builder = LatticeBricks::new();
                let mut previous: Option<Vec<u32>> = None;
                for (frame, &(ranges_slot, blobs_slot, bounds_slot)) in frames.iter().enumerate() {
                    let (brick_slot, _) = h.array::<u32>(&[], 1);
                    let (_, errors) = h.run(
                        &mut builder,
                        &[("blobs", blobs_slot), ("bounds", bounds_slot), ("cell_ranges", ranges_slot), ("solid", solid_slot)],
                        &[("bricks", brick_slot)],
                        &lattice.params(&[
                            ("nodes_x", solid_nodes[0] as f32),
                            ("nodes_y", solid_nodes[1] as f32),
                            ("nodes_z", solid_nodes[2] as f32),
                            ("resolution_scale", scale as f32),
                            ("band_extra", band),
                        ]),
                    );
                    assert!(errors.is_empty(), "{label} frame {frame}: {errors:?}");
                    let scattered = read::<u32>(&h.buffer(brick_slot), layout.words as usize);

                    // The gather reads the same uniform layout; its last word was padding.
                    let f = f32::to_bits;
                    let uniforms: Vec<u32> = vec![
                        f(lattice.center[0]), f(lattice.center[1]), f(lattice.center[2]),
                        f(lattice.size[0]), f(lattice.size[1]), f(lattice.size[2]), f(lattice.cell),
                        layout.nodes[0], layout.nodes[1], layout.nodes[2], scale,
                        bins[0], bins[1], bins[2],
                        layout.bricks[0], layout.bricks[1], layout.bricks[2], layout.count,
                        f(band), 0,
                    ];
                    let prefix = h.device.create_buffer_shared(u64::from(layout.count.max(4)) * 4);
                    let gathered = h.device.create_buffer_shared(u64::from(layout.words) * 4);
                    let blobs = h.buffer(blobs_slot);
                    let ranges = h.buffer(ranges_slot);
                    let bounds = h.buffer(bounds_slot);
                    let mut encoder = h.device.create_encoder("liquid bricks gather oracle");
                    encoder.dispatch_compute(
                        &gather,
                        &[
                            GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(&uniforms) },
                            GpuBinding::Buffer { binding: 1, buffer: &blobs, offset: 0 },
                            GpuBinding::Buffer { binding: 2, buffer: &ranges, offset: 0 },
                            GpuBinding::Buffer { binding: 3, buffer: &prefix, offset: 0 },
                            GpuBinding::Buffer { binding: 4, buffer: &gathered, offset: 0 },
                            GpuBinding::Buffer { binding: 5, buffer: &bounds, offset: 0 },
                        ],
                        [layout.count.div_ceil(256), 1, 1],
                        "gather oracle mark",
                    );
                    encoder.commit_and_wait_completed();
                    let gathered = read::<u32>(&gathered, layout.words as usize);
                    let mask = 8..8 + layout.count as usize;
                    let interior = |id: u32| {
                        let b = [id % layout.bricks[0], (id / layout.bricks[0]) % layout.bricks[1], id / (layout.bricks[0] * layout.bricks[1])];
                        (0..3).all(|a| b[a] > 0 && b[a] + 1 < layout.bricks[a])
                    };
                    let active = |words: &[u32]| (0..layout.count).filter(|&id| words[8 + id as usize] == 1).count();
                    assert_eq!(
                        scattered[mask.clone()].iter().zip(&gathered[mask.clone()]).position(|(a, b)| a != b),
                        None,
                        "{label} frame {frame}: first brick where the scatter ({} active) and the gather ({} active) disagree, of {}",
                        active(&scattered), active(&gathered), layout.count
                    );
                    let interior_active = (0..layout.count).filter(|&id| interior(id) && scattered[8 + id as usize] == 1).count();
                    assert!(interior_active > 0, "{label} frame {frame}: the fixture must activate interior bricks");
                    assert!(scattered[mask.clone()].contains(&0), "{label} frame {frame}: the fixture must leave bricks inactive");
                    if let Some(previous) = &previous {
                        assert!(
                            (0..layout.count).any(|id| interior(id) && previous[8 + id as usize] == 1 && scattered[8 + id as usize] == 0),
                            "{label}: the second frame must retire bricks the first one marked"
                        );
                    }
                    assert_eq!(scattered, compact_brick_words(&scattered[mask], layout.bricks), "{label} frame {frame}: header and compact list");
                    previous = Some(scattered);
                }
            }
        }
    }
}

use manifold_node_engine::testkit::liquid_surface::{Harness, params, read};

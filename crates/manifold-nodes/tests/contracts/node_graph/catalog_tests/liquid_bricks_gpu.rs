//! Sparse field proofs against independent dense gathers. The field oracle
//! follows native ParticleMesher support and production solid/border semantics.
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::primitive::PrimitiveSpec;
use manifold_node_engine::testkit::shader_source::dense_source;
use manifold_water_liquid::fluid_particles::{CellRange, FluidBlob, bin_counts};
use manifold_water_liquid::primitives::testkit::liquid as liquid_nodes;
use manifold_water_surface::primitives::{
    lattice_bricks::LatticeBricks, lattice_bricks::brick_layout,
    lattice_bricks::compact_brick_words, lattice_bricks::conservative_brick_mask,
    particle_volume::ParticleVolume,
};

fn oracle<P: PrimitiveSpec>(h: &Harness, source: &str) -> GpuComputePipeline {
    h.device.create_compute_pipeline(
        &dense_source::<P>(source),
        "cs_main",
        "liquid.bricks.dense_reference",
    )
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
        manifold_water_surface::primitives::particle_volume::DENSE_REFERENCE,
    ));
    let mut smoothing: [[_; 3]; 2] = std::array::from_fn(|lane| {
        std::array::from_fn(|_| {
            liquid_nodes::smooth_lattice((lane == 1).then(|| {
                liquid_nodes::dense_pipeline(
                    "smooth_lattice",
                    &h.device,
                    manifold_water_liquid::primitives::smooth_lattice::DENSE_REFERENCE,
                )
            }))
        })
    });
    let smooth_slots: [[(Slot, GpuBuffer); 3]; 2] =
        std::array::from_fn(|_| std::array::from_fn(|_| h.array::<f32>(&[], total)));
    let mut builder = LatticeBricks::new();
    let (bounds_slot, _) = h.array::<f32>(&[], 2);
    let mut bounder = manifold_water_surface::primitives::blob_bounds::BlobBounds::new();
    manifold_node_engine::primitive::Primitive::prepare_pipelines(&mut bounder, &h.device);
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
    // bricks retaining last frame's field or smoothing.
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
                blobs.push(FluidBlob {
                    center_radius: [p[0], p[1], p[2], reach],
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
            &mut bounder,
            &[("blobs", blob_slot)],
            &[("bounds", bounds_slot)],
            &params(&[]),
        );
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
        if frame < 2 {
            assert!(
                read::<f32>(&sparse_buffer, total).iter().any(|v| *v < 0.0),
                "nonempty liquid fixture"
            );
        }
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
        }
    }
}

#[test]
fn fluid_bricks_field_bit_identical_dense_64() {
    fixture(64);
}

#[test]
fn fluid_bricks_field_bit_identical_dense_128() {
    fixture(128);
}

/// Particles for the scatter proofs in a lattice of any size: eight per cell
/// in a column filling the lower corner, a thin sheet thrown off it, and
/// loose drops. Without spray the frame is just the column.
fn scatter_fixture(
    lattice: &manifold_water_surface::testkit::liquid_surface::Lattice,
    resolution: u32,
    spray: bool,
    seed: u64,
) -> Vec<manifold_node_engine::particles::FluidParticle> {
    use manifold_node_engine::particles::FluidParticle;
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
        particles.push(FluidParticle {
            position_radius: [p[0], p[1], p[2], marker],
            velocity: [0.0; 3],
            id,
        });
    };
    for z in 0..(resolution * 2 / 5) * 2 {
        for y in 0..(resolution * 2 / 5) * 2 {
            for x in 0..(resolution / 4) * 2 {
                let at = [x, y, z].map(|i| (i as f32 + 0.5) * 0.5 * lattice.cell);
                push(std::array::from_fn(|a| {
                    min[a] + at[a] + (next() - 0.5) * 0.2 * lattice.cell
                }));
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
        push(std::array::from_fn(|a| {
            min[a] + unit * 0.05 + next() * (lattice.size[a] - unit * 0.1)
        }));
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
    use crate::contracts::node_graph::catalog_tests::liquid_surface::blob_bounds;
    use manifold_water_surface::testkit::liquid_surface::{Lattice, sort_and_shape};

    let mut h = Harness::new();
    let resolution = 64u32;
    let gather = h.device.create_compute_pipeline(
        manifold_water_surface::primitives::lattice_bricks::GATHER_REFERENCE,
        "mark_bricks",
        "liquid.bricks.gather_reference",
    );
    let lattices = [
        Lattice {
            center: [0.0, 0.5, 0.0],
            size: [4.0, 4.0, 4.0],
            cell: 4.0 / resolution as f32,
        },
        Lattice {
            center: [1000.0, -1000.0, 1000.0],
            size: [0.25, 0.25, 0.25],
            cell: 0.25 / resolution as f32,
        },
    ];
    for lattice in &lattices {
        let marker = 0.310_175_25 * lattice.cell;
        let frames: Vec<_> = [true, false]
            .iter()
            .map(|&spray| {
                let particles = scatter_fixture(lattice, resolution, spray, 0x9e37_79b9_7f4a_7c15);
                let count = particles.len();
                let (_, _, _, (_, ranges_slot, blobs_slot)) = sort_and_shape(
                    &mut h,
                    lattice,
                    &particles,
                    count,
                    &[("particle_scale", 3.0)],
                );
                (ranges_slot, blobs_slot, blob_bounds(&mut h, blobs_slot))
            })
            .collect();
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
                        &[
                            ("blobs", blobs_slot),
                            ("bounds", bounds_slot),
                            ("cell_ranges", ranges_slot),
                            ("solid", solid_slot),
                        ],
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
                        f(lattice.center[0]),
                        f(lattice.center[1]),
                        f(lattice.center[2]),
                        f(lattice.size[0]),
                        f(lattice.size[1]),
                        f(lattice.size[2]),
                        f(lattice.cell),
                        layout.nodes[0],
                        layout.nodes[1],
                        layout.nodes[2],
                        scale,
                        bins[0],
                        bins[1],
                        bins[2],
                        layout.bricks[0],
                        layout.bricks[1],
                        layout.bricks[2],
                        layout.count,
                        f(band),
                        0,
                    ];
                    let prefix = h
                        .device
                        .create_buffer_shared(u64::from(layout.count.max(4)) * 4);
                    let gathered = h.device.create_buffer_shared(u64::from(layout.words) * 4);
                    let blobs = h.buffer(blobs_slot);
                    let ranges = h.buffer(ranges_slot);
                    let bounds = h.buffer(bounds_slot);
                    let mut encoder = h.device.create_encoder("liquid bricks gather oracle");
                    encoder.dispatch_compute(
                        &gather,
                        &[
                            GpuBinding::Bytes {
                                binding: 0,
                                data: bytemuck::cast_slice(&uniforms),
                            },
                            GpuBinding::Buffer {
                                binding: 1,
                                buffer: &blobs,
                                offset: 0,
                            },
                            GpuBinding::Buffer {
                                binding: 2,
                                buffer: &ranges,
                                offset: 0,
                            },
                            GpuBinding::Buffer {
                                binding: 3,
                                buffer: &prefix,
                                offset: 0,
                            },
                            GpuBinding::Buffer {
                                binding: 4,
                                buffer: &gathered,
                                offset: 0,
                            },
                            GpuBinding::Buffer {
                                binding: 5,
                                buffer: &bounds,
                                offset: 0,
                            },
                        ],
                        [layout.count.div_ceil(256), 1, 1],
                        "gather oracle mark",
                    );
                    encoder.commit_and_wait_completed();
                    let gathered = read::<u32>(&gathered, layout.words as usize);
                    let mask = 8..8 + layout.count as usize;
                    let interior = |id: u32| {
                        let b = [
                            id % layout.bricks[0],
                            (id / layout.bricks[0]) % layout.bricks[1],
                            id / (layout.bricks[0] * layout.bricks[1]),
                        ];
                        (0..3).all(|a| b[a] > 0 && b[a] + 1 < layout.bricks[a])
                    };
                    let active = |words: &[u32]| {
                        (0..layout.count)
                            .filter(|&id| words[8 + id as usize] == 1)
                            .count()
                    };
                    assert_eq!(
                        scattered[mask.clone()]
                            .iter()
                            .zip(&gathered[mask.clone()])
                            .position(|(a, b)| a != b),
                        None,
                        "{label} frame {frame}: first brick where the scatter ({} active) and the gather ({} active) disagree, of {}",
                        active(&scattered),
                        active(&gathered),
                        layout.count
                    );
                    let interior_active = (0..layout.count)
                        .filter(|&id| interior(id) && scattered[8 + id as usize] == 1)
                        .count();
                    assert!(
                        interior_active > 0,
                        "{label} frame {frame}: the fixture must activate interior bricks"
                    );
                    assert!(
                        scattered[mask.clone()].contains(&0),
                        "{label} frame {frame}: the fixture must leave bricks inactive"
                    );
                    if let Some(previous) = &previous {
                        assert!(
                            (0..layout.count).any(|id| interior(id)
                                && previous[8 + id as usize] == 1
                                && scattered[8 + id as usize] == 0),
                            "{label}: the second frame must retire bricks the first one marked"
                        );
                    }
                    assert_eq!(
                        scattered,
                        compact_brick_words(&scattered[mask], layout.bricks),
                        "{label} frame {frame}: header and compact list"
                    );
                    previous = Some(scattered);
                }
            }
        }
    }
}

use manifold_node_engine::testkit::array_harness::{Harness, params, read};

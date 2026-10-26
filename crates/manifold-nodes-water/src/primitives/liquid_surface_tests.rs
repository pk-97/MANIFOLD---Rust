//! GPU value proofs for the liquid particle sort, the running total and the
//! level-set smoothing lattice (GPU_FLUID_SURFACE_DESIGN.md) against CPU f64
//! references. Each atom runs through its own `run()` on a real device with
//! pre-bound shared buffers; scalar inputs arrive as the params they shadow.

use manifold_gpu::GpuBuffer;

use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::fluid_particles::{CellRange, bin_counts};
use manifold_water_liquid::primitives::running_total::RunningTotal;
use manifold_water_liquid::primitives::sort_particles_into_cells::SortParticlesIntoCells;

use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_surface::testkit::liquid_surface::*;

#[test]
fn fluid_sort_particles_into_cells_is_a_binned_permutation() {
    let mut harness = Harness::new();
    let lattice = Lattice {
        center: [0.0, 1.0, 0.0],
        size: [2.0, 2.0, 2.0],
        cell: 0.25,
    };
    let bins = bin_counts(lattice.size, lattice.cell);
    let min = lattice.min();
    let mut rng = Rng::new(0x9e37_79b9_7f4a_7c15);
    // Positions sit well inside their bins (never near a boundary, where
    // fast-math reassociation could bin differently); every 97th lies half a
    // bin outside the box to exercise the border clamp; every tenth is
    // inactive.
    let particles: Vec<FluidParticle> = (0..5000u32)
        .map(|i| {
            let mut position: [f32; 3] = std::array::from_fn(|axis| {
                let bin = (rng.next_f32() * bins[axis] as f32)
                    .floor()
                    .min(bins[axis] as f32 - 1.0);
                min[axis] + (bin + 0.1 + 0.8 * rng.next_f32()) * lattice.cell
            });
            if i % 97 == 0 {
                position[0] = min[0] - 0.5 * lattice.cell;
            }
            particle(position, if i % 10 == 3 { 0.0 } else { 0.02 }, i + 1)
        })
        .collect();
    let count = 4800;
    let (sorted, ranges, _, _) = sort_and_shape(&mut harness, &lattice, &particles, count, &[]);

    let bin_total = bins.iter().product::<u32>() as usize;
    let mut expected: Vec<Vec<u32>> = vec![Vec::new(); bin_total];
    for p in &particles[..count] {
        if p.position_radius[3] > 0.0 {
            let [x, y, z, _] = p.position_radius;
            expected[lattice.bin([x, y, z])].push(p.id);
        }
    }
    let live: usize = expected.iter().map(Vec::len).sum();
    let mut next = 0u32;
    for (bin, ids) in expected.iter_mut().enumerate() {
        let range = ranges[bin];
        assert_eq!(
            range.start, next,
            "bin {bin} starts where the previous one ends"
        );
        assert_eq!(range.count as usize, ids.len(), "bin {bin} count");
        let mut actual: Vec<u32> = sorted
            [range.start as usize..(range.start + range.count) as usize]
            .iter()
            .map(|p| p.id)
            .collect();
        actual.sort_unstable();
        ids.sort_unstable();
        assert_eq!(&actual, ids, "bin {bin} members");
        next += range.count;
    }
    assert_eq!(next as usize, live);
    assert!(
        sorted[live..].iter().all(|p| p.position_radius[3] == 0.0),
        "tail is inactive"
    );

    // `order` names each sorted slot's input index; past the live total, none.
    let (input, _) = harness.array(&particles, particles.len());
    let (sorted_slot, sorted_buf) = harness.array::<FluidParticle>(&[], particles.len());
    let (ranges_slot, _) = harness.array::<CellRange>(&[], bin_total);
    let (order_slot, order_buf) = harness.array::<u32>(&[], particles.len());
    let count_slot = harness.scalar_input(count as f32);
    let (_, errors) = harness.run(
        &mut SortParticlesIntoCells::new(),
        &[("particles", input), ("count", count_slot)],
        &[
            ("sorted", sorted_slot),
            ("cell_ranges", ranges_slot),
            ("order", order_slot),
        ],
        &lattice.params(&[]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let sorted: Vec<FluidParticle> = read(&sorted_buf, particles.len());
    let order: Vec<u32> = read(&order_buf, particles.len());
    for (slot, &index) in order.iter().enumerate() {
        if slot < live {
            assert!((index as usize) < count, "slot {slot} names a sorted input");
            assert_eq!(
                bytemuck::bytes_of(&sorted[slot]),
                bytemuck::bytes_of(&particles[index as usize]),
                "slot {slot} holds input {index}"
            );
        } else {
            assert_eq!(
                index,
                u32::MAX,
                "slot {slot} past the live total names no input"
            );
        }
    }
}

/// The sort sizes its ranges from the bin grid it publishes, at any grid.
#[test]
fn fluid_sort_particles_into_cells_sizes_ranges_to_its_bins() {
    let mut harness = Harness::new();
    let (input, _) = harness.array(&[particle([0.0; 3], 0.02, 1)], 1);
    let (sorted, _) = harness.array::<FluidParticle>(&[], 1);
    let (ranges, _) = harness.array::<CellRange>(&[], 1);
    let bins_out: [Slot; 3] = std::array::from_fn(|_| harness.scalar());
    let mut sort = SortParticlesIntoCells::new();
    // 8³, then 67³ (the storage grows to it), then back down.
    for cell in [0.25_f32, 0.03, 0.5] {
        let lattice = Lattice {
            center: [0.0; 3],
            size: [2.0; 3],
            cell,
        };
        let bins = bin_counts(lattice.size, lattice.cell);
        let (scalars, errors) = harness.run(
            &mut sort,
            &[("particles", input)],
            &[
                ("sorted", sorted),
                ("cell_ranges", ranges),
                ("bins_x", bins_out[0]),
                ("bins_y", bins_out[1]),
                ("bins_z", bins_out[2]),
            ],
            &lattice.params(&[]),
        );
        assert!(errors.is_empty(), "{errors:?}");
        for (slot, n) in bins_out.iter().zip(bins) {
            assert!(
                scalars.contains(&(*slot, ParamValue::Float(n as f32))),
                "cell {cell}: {scalars:?}"
            );
        }
        let holds = harness.buffer(ranges).size / std::mem::size_of::<CellRange>() as u64;
        assert!(
            holds >= bins.iter().map(|&n| u64::from(n)).product::<u64>(),
            "cell {cell}: {holds} ranges"
        );
    }
}

/// Determinism is an invariant (bakes, bit-reproducible export): runs on the same
/// input give byte-identical outputs, and each bin lists its particles in input
/// order. Half-bin cells give crowded bins, quarter-bin cells sparse ones.
#[test]
fn fluid_sort_particles_into_cells_is_deterministic() {
    let mut harness = Harness::new();
    let mut rng = Rng::new(31);
    let particles: Vec<FluidParticle> = (0..4000u32)
        .map(|i| {
            particle(
                std::array::from_fn(|_| (rng.next_f32() - 0.5) * 1.9),
                if i % 11 == 0 { 0.0 } else { 0.02 },
                i + 1,
            )
        })
        .collect();
    let (input, _) = harness.array(&particles, particles.len());
    for cell in [0.5f32, 0.25] {
        let lattice = Lattice {
            center: [0.0; 3],
            size: [2.0; 3],
            cell,
        };
        let bins = bin_counts(lattice.size, lattice.cell)
            .iter()
            .product::<u32>() as usize;
        let mut runs = Vec::new();
        for _ in 0..3 {
            let (sorted, sorted_buf) = harness.array::<FluidParticle>(&[], particles.len());
            let (ranges, _) = harness.array::<CellRange>(&[], 1);
            let (order, order_buf) = harness.array::<u32>(&[], particles.len());
            let (_, errors) = harness.run(
                &mut SortParticlesIntoCells::new(),
                &[("particles", input)],
                &[
                    ("sorted", sorted),
                    ("cell_ranges", ranges),
                    ("order", order),
                ],
                &lattice.params(&[]),
            );
            assert!(errors.is_empty(), "{errors:?}");
            runs.push((
                read::<u8>(&sorted_buf, sorted_buf.size as usize),
                read::<u8>(&order_buf, order_buf.size as usize),
                read::<CellRange>(&harness.buffer(ranges), bins),
            ));
        }
        let (sorted, order, ranges) = &runs[0];
        assert!(
            runs.iter().all(|run| run.0 == *sorted && run.1 == *order),
            "cell {cell}: every run is byte-identical"
        );
        let order: &[u32] = bytemuck::cast_slice(order);
        let crowded = ranges.iter().map(|r| r.count).max().unwrap_or(0);
        assert!(
            if cell == 0.5 {
                crowded > 32
            } else {
                crowded <= 32
            },
            "cell {cell}: largest bin {crowded}"
        );
        for range in ranges {
            let members = &order[range.start as usize..(range.start + range.count) as usize];
            assert!(
                members.windows(2).all(|w| w[0] < w[1]),
                "cell {cell}: a bin lists its particles in input order"
            );
        }
    }
}

/// A consumer that wires only `order` and `cell_ranges` gets the same ranges and
/// the same members per bin as one that also wires `sorted`. Order within a bin
/// follows atomic ranks, so it may differ between any two runs.
#[test]
fn fluid_sort_particles_into_cells_runs_with_sorted_unwired() {
    let mut harness = Harness::new();
    let lattice = Lattice {
        center: [0.0; 3],
        size: [2.0; 3],
        cell: 0.25,
    };
    let bins = bin_counts(lattice.size, lattice.cell)
        .iter()
        .product::<u32>() as usize;
    let mut rng = Rng::new(23);
    let particles: Vec<FluidParticle> = (0..500u32)
        .map(|i| {
            particle(
                std::array::from_fn(|_| (rng.next_f32() - 0.5) * 1.8),
                if i % 7 == 0 { 0.0 } else { 0.02 },
                i + 1,
            )
        })
        .collect();
    let (input, _) = harness.array(&particles, particles.len());
    let run = |harness: &mut Harness, wire_sorted: bool| {
        let (sorted, _) = harness.array::<FluidParticle>(&[], particles.len());
        let (ranges, _) = harness.array::<CellRange>(&[], 1);
        let (order, order_buf) = harness.array::<u32>(&[], particles.len());
        let mut outputs = vec![("cell_ranges", ranges), ("order", order)];
        if wire_sorted {
            outputs.push(("sorted", sorted));
        }
        let (_, errors) = harness.run(
            &mut SortParticlesIntoCells::new(),
            &[("particles", input)],
            &outputs,
            &lattice.params(&[]),
        );
        assert!(errors.is_empty(), "{errors:?}");
        let ranges: Vec<CellRange> = read(&harness.buffer(ranges), bins);
        let order: Vec<u32> = read(&order_buf, particles.len());
        let members: Vec<Vec<u32>> = ranges
            .iter()
            .map(|r| {
                let mut bin = order[r.start as usize..(r.start + r.count) as usize].to_vec();
                bin.sort_unstable();
                bin
            })
            .collect();
        let live: u32 = ranges.iter().map(|r| r.count).sum();
        assert!(
            order[live as usize..].iter().all(|&i| i == u32::MAX),
            "no input past the live total"
        );
        (
            bytemuck::cast_slice::<CellRange, u8>(&ranges).to_vec(),
            members,
        )
    };
    let wired = run(&mut harness, true);
    assert!(
        wired.1.iter().any(|bin| !bin.is_empty()),
        "the fixture sorts live particles"
    );
    assert_eq!(
        run(&mut harness, false),
        wired,
        "ranges and bin members do not depend on sorted being wired"
    );
}

/// Matter points sort in place: ranges and order are byte-identical to sorting
/// liquid particle records at the same positions, where a point is live exactly
/// when its id is non-zero and its position finite. Wiring `sorted` for them is
/// a named error, since it holds liquid particle records.
#[test]
fn fluid_sort_particles_into_cells_sorts_matter_points_in_place() {
    use manifold_water_gpu_mpm::matter::MatterPoint;
    let mut harness = Harness::new();
    let lattice = Lattice {
        center: [0.0; 3],
        size: [2.0; 3],
        cell: 0.25,
    };
    let bins = bin_counts(lattice.size, lattice.cell)
        .iter()
        .product::<u32>() as usize;
    let mut rng = Rng::new(47);
    let points: Vec<MatterPoint> = (0..3000u32)
        .map(|i| {
            let mut position: [f32; 3] = std::array::from_fn(|_| (rng.next_f32() - 0.5) * 1.9);
            if i % 53 == 0 {
                position[1] = f32::NAN;
            }
            if i % 71 == 0 {
                position[2] = f32::INFINITY;
            }
            MatterPoint {
                position,
                id: if i % 9 == 4 { 0 } else { i + 1 },
                velocity: [1.0, 2.0, 3.0],
                volume_ratio: 1.0,
                affine_y: [0.0, 0.0, 0.0, 1e-5],
                ..MatterPoint::default()
            }
        })
        .collect();
    let particles: Vec<FluidParticle> = points
        .iter()
        .map(|p| {
            let live = p.id != 0 && p.position.iter().all(|v| v.is_finite());
            particle(p.position, if live { 0.02 } else { 0.0 }, p.id)
        })
        .collect();
    let count = 2900;
    let run = |harness: &mut Harness, input: Slot| {
        let (ranges, _) = harness.array::<CellRange>(&[], 1);
        let (order, order_buf) = harness.array::<u32>(&[], points.len());
        let count_slot = harness.scalar_input(count as f32);
        let (_, errors) = harness.run(
            &mut SortParticlesIntoCells::new(),
            &[("particles", input), ("count", count_slot)],
            &[("cell_ranges", ranges), ("order", order)],
            &lattice.params(&[]),
        );
        assert!(errors.is_empty(), "{errors:?}");
        let ranges_buf = harness.buffer(ranges);
        assert_eq!(
            ranges_buf.size,
            (bins * std::mem::size_of::<CellRange>()) as u64,
            "one range per bin"
        );
        (
            read::<u8>(&ranges_buf, ranges_buf.size as usize),
            read::<u8>(&order_buf, order_buf.size as usize),
        )
    };
    let (matter_input, _) = harness.array(&points, points.len());
    let (particle_input, _) = harness.array(&particles, particles.len());
    let matter = run(&mut harness, matter_input);
    let liquid = run(&mut harness, particle_input);
    let ranges: &[CellRange] = bytemuck::cast_slice(&liquid.0);
    let live: u32 = ranges.iter().map(|r| r.count).sum();
    assert!(
        live > 2000 && (live as usize) < count,
        "the fixture has live and dead points: {live}"
    );
    assert_eq!(
        matter, liquid,
        "matter points bin and order exactly as the equivalent liquid particles"
    );

    let (sorted, _) = harness.array::<FluidParticle>(&[], points.len());
    let (ranges, _) = harness.array::<CellRange>(&[], bins);
    let (_, errors) = harness.run(
        &mut SortParticlesIntoCells::new(),
        &[("particles", matter_input)],
        &[("sorted", sorted), ("cell_ranges", ranges)],
        &lattice.params(&[]),
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("sorted holds liquid particle records")),
        "{errors:?}"
    );
}

/// A record with no position the sort can find is a named error.
#[test]
fn fluid_sort_particles_into_cells_rejects_records_without_a_position() {
    let mut harness = Harness::new();
    let lattice = Lattice {
        center: [0.0; 3],
        size: [2.0; 3],
        cell: 0.25,
    };
    let (input, _) = harness.array(&[CellRange { start: 0, count: 1 }], 1);
    let (ranges, _) = harness.array::<CellRange>(&[], 512);
    let (_, errors) = harness.run(
        &mut SortParticlesIntoCells::new(),
        &[("particles", input)],
        &[("cell_ranges", ranges)],
        &lattice.params(&[]),
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("position_radius") && e.contains("position and id")),
        "{errors:?}"
    );
}

/// With `enabled` 0 the sort does nothing: after a changed input, every output
/// is byte-identical to the previous call's.
#[test]
fn fluid_sort_particles_into_cells_disabled_leaves_outputs_untouched() {
    let mut harness = Harness::new();
    let lattice = Lattice {
        center: [0.0; 3],
        size: [2.0; 3],
        cell: 0.25,
    };
    let bins = bin_counts(lattice.size, lattice.cell)
        .iter()
        .product::<u32>() as usize;
    let mut rng = Rng::new(11);
    let mut cloud = |seed: f32| -> Vec<FluidParticle> {
        (0..300u32)
            .map(|i| {
                particle(
                    std::array::from_fn(|_| (rng.next_f32() - 0.5) * 1.8 * seed),
                    0.02,
                    i + 1,
                )
            })
            .collect()
    };
    let (first, second) = (cloud(1.0), cloud(0.5));
    let (sorted, sorted_buf) = harness.array::<FluidParticle>(&[], first.len());
    let (ranges, _) = harness.array::<CellRange>(&[], 1);
    let (order, order_buf) = harness.array::<u32>(&[], first.len());
    let mut sort = SortParticlesIntoCells::new();
    let mut run = |harness: &mut Harness, particles: &[FluidParticle], enabled: f32| {
        let (input, _) = harness.array(particles, particles.len());
        let enabled = harness.scalar_input(enabled);
        let (_, errors) = harness.run(
            &mut sort,
            &[("particles", input), ("enabled", enabled)],
            &[
                ("sorted", sorted),
                ("cell_ranges", ranges),
                ("order", order),
            ],
            &lattice.params(&[]),
        );
        assert!(errors.is_empty(), "{errors:?}");
        let ranges_buf = harness.buffer(ranges);
        (
            read::<u8>(&sorted_buf, sorted_buf.size as usize),
            read::<u8>(&ranges_buf, bins * std::mem::size_of::<CellRange>()),
            read::<u8>(&order_buf, order_buf.size as usize),
        )
    };
    let enabled = run(&mut harness, &first, 1.0);
    assert_eq!(
        run(&mut harness, &second, 0.0),
        enabled,
        "a disabled sort leaves every output as it was"
    );
    assert_ne!(
        run(&mut harness, &second, 1.0),
        enabled,
        "re-enabled, it sorts the new particles"
    );
}

/// A producer without a frame yet is pending, so the executor holds this node;
/// a lattice that reaches it is real, and a broken one is an error even with
/// no particles.
#[test]
fn fluid_sort_particles_into_cells_rejects_a_broken_lattice_at_any_count() {
    let mut harness = Harness::new();
    let no_lattice = Lattice {
        center: [0.0; 3],
        size: [4.0; 3],
        cell: -4.0,
    };
    let (input, _) = harness.array(&[particle([0.0; 3], 0.02, 1)], 1);
    let (sorted, _) = harness.array::<FluidParticle>(&[], 1);
    let (ranges, _) = harness.array::<CellRange>(&[], 64);
    let mut sort = SortParticlesIntoCells::new();
    for (count, expect_error) in [(0.0, true), (1.0, true)] {
        let count_slot = harness.scalar_input(count);
        let (_, errors) = harness.run(
            &mut sort,
            &[("particles", input), ("count", count_slot)],
            &[("sorted", sorted), ("cell_ranges", ranges)],
            &no_lattice.params(&[]),
        );
        assert_eq!(
            errors.iter().any(|e| e.contains("box and cell size")),
            expect_error,
            "count {count}: {errors:?}"
        );
    }
}

#[test]
fn fluid_running_total_matches_cpu_scan_and_total_lags_one_frame() {
    let mut harness = Harness::new();
    let mut rng = Rng::new(7);
    // (1 << 24) + 3 needs four scan levels, as Surface Detail 2 at resolution 64 does.
    for size in [1usize, 255, 256, 257, (1 << 20) + 3, (1 << 24) + 3] {
        let values: Vec<u32> = (0..size).map(|_| (rng.next_f32() * 6.0) as u32).collect();
        let (input, _) = harness.array(&values, size);
        let (out_slot, out) = harness.array::<u32>(&[], size);
        let total_slot = harness.scalar();
        let mut node = RunningTotal::new();
        let run = |node: &mut RunningTotal, harness: &mut Harness| {
            harness.run(
                node,
                &[("in", input)],
                &[("out", out_slot), ("total", total_slot)],
                &ParamValues::default(),
            )
        };
        let (scalars, errors) = run(&mut node, &mut harness);
        assert!(errors.is_empty(), "{errors:?}");
        let first_total = scalars
            .iter()
            .find(|(slot, _)| *slot == total_slot)
            .map(|(_, v)| v.clone());
        assert_eq!(
            first_total,
            Some(ParamValue::Float(0.0)),
            "a fresh node reports nothing yet"
        );
        let mut running = 0u64;
        let expected: Vec<u32> = values
            .iter()
            .map(|&v| {
                running += u64::from(v);
                running as u32
            })
            .collect();
        let actual: Vec<u32> = read(&out, size);
        let first_wrong = expected.iter().zip(&actual).position(|(e, a)| e != a);
        assert_eq!(first_wrong, None, "size {size}");
        let (scalars, _) = run(&mut node, &mut harness);
        let lagged = scalars
            .iter()
            .find(|(slot, _)| *slot == total_slot)
            .map(|(_, v)| v.clone());
        assert_eq!(
            lagged,
            Some(ParamValue::Float(running as f32)),
            "size {size}: one frame late"
        );
    }
}

/// A total past the consumer's capacity is a named error on every frame it
/// lasts, never silent; at or under capacity there is none.
#[test]
fn fluid_running_total_names_a_total_past_its_capacity() {
    let mut harness = Harness::new();
    let flags = vec![1u32; 100];
    let (input, _) = harness.array(&flags, flags.len());
    let (out_slot, _) = harness.array::<u32>(&[], flags.len());
    for (capacity, expect_error) in [(100.0, false), (64.0, true)] {
        let mut node = RunningTotal::new();
        let mut params = ParamValues::default();
        params.insert("capacity".into(), ParamValue::Float(capacity));
        let mut run = || {
            harness
                .run(&mut node, &[("in", input)], &[("out", out_slot)], &params)
                .1
        };
        assert!(run().is_empty(), "the first frame has no total yet");
        for frame in 1..3 {
            let errors = run();
            if expect_error {
                assert!(
                    errors.iter().any(|e| e.contains("needs 100, holds 64")),
                    "frame {frame}: an overflow must be named every frame: {errors:?}"
                );
            } else {
                assert!(
                    errors.is_empty(),
                    "frame {frame}: capacity {capacity} holds 100: {errors:?}"
                );
            }
        }
    }
}

/// `extent` holds the total, then 256-thread groups covering max(total, last
/// frame's total) × per_item elements.
#[test]
fn fluid_running_total_extent_covers_this_and_last_frame() {
    let mut harness = Harness::new();
    let mut node = RunningTotal::new();
    let (out, _) = harness.array::<u32>(&[], 1000);
    let (extent_slot, extent) = harness.array::<u32>(&[], 4);
    let total = harness.scalar();
    let per_item = params(&[("per_item", 3.0)]);
    // (live items, expected groups): 300 elements → 2 groups; shrinking to 10
    // still covers last frame's 300; then 30 elements → 1 group.
    for (items, groups) in [(100u32, 2u32), (10, 2), (10, 1)] {
        let values: Vec<u32> = (0..1000).map(|i| u32::from(i < items)).collect();
        let (input, _) = harness.array(&values, 1000);
        let (_, errors) = harness.run(
            &mut node,
            &[("in", input)],
            &[("out", out), ("total", total), ("extent", extent_slot)],
            &per_item,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            read::<u32>(&extent, 4),
            vec![items, groups, 1, 1],
            "{items} live items"
        );
    }
}

// --- P6c: level-set smoothing ---------------------------------------------

use manifold_water_liquid::primitives::smooth_lattice::SmoothLattice;

/// f64 reference: the (2p + 1)³ binomial gather with edge-clamped indices.
fn reference_smooth(values: &[f32], nodes: [usize; 3], passes: usize) -> Vec<f64> {
    let row: Vec<f64> = {
        let n = 2 * passes;
        let mut c = vec![1.0f64; n + 1];
        for k in 1..n {
            c[k] = c[k - 1] * (n - k + 1) as f64 / k as f64;
        }
        c.iter().map(|w| w / 4f64.powi(passes as i32)).collect()
    };
    let p = passes as i64;
    let at = |x: i64, y: i64, z: i64| {
        let c = |v: i64, n: usize| v.clamp(0, n as i64 - 1) as usize;
        f64::from(values[c(x, nodes[0]) + nodes[0] * (c(y, nodes[1]) + nodes[1] * c(z, nodes[2]))])
    };
    let mut out = Vec::with_capacity(nodes.iter().product());
    for z in 0..nodes[2] as i64 {
        for y in 0..nodes[1] as i64 {
            for x in 0..nodes[0] as i64 {
                let mut sum = 0.0;
                for dz in -p..=p {
                    for dy in -p..=p {
                        for dx in -p..=p {
                            let w = row[(dz + p) as usize]
                                * row[(dy + p) as usize]
                                * row[(dx + p) as usize];
                            sum += w * at(x + dx, y + dy, z + dz);
                        }
                    }
                }
                out.push(sum);
            }
        }
    }
    out
}

#[test]
fn fluid_smooth_lattice_matches_binomial_reference_and_passes_through() {
    let mut harness = Harness::new();
    let nodes = [13usize, 11, 9];
    let total: usize = nodes.iter().product();
    let mut rng = Rng::new(0x5eed_5eed);
    let values: Vec<f32> = (0..total + 20)
        .map(|_| rng.next_f32() * 2.0 - 1.0)
        .collect();
    let (input, _) = harness.array(&values, values.len());
    let stages: Vec<(Slot, GpuBuffer)> = (0..3)
        .map(|_| harness.array::<f32>(&[], values.len()))
        .collect();
    let (out_slot, out) = stages[2].clone();
    for passes in 0..=3usize {
        // Axes x, y, z chained: input → stage 0 → stage 1 → stage 2.
        let mut source = input;
        for (axis, (stage, _)) in stages.iter().enumerate() {
            let lattice = [
                ("nodes_x", nodes[0] as f32),
                ("nodes_y", nodes[1] as f32),
                ("nodes_z", nodes[2] as f32),
                ("passes", passes as f32),
                ("axis", axis as f32),
            ];
            let (_, errors) = harness.run(
                &mut SmoothLattice::new(),
                &[("levelset", source)],
                &[("smoothed", *stage)],
                &params(&lattice),
            );
            assert!(errors.is_empty(), "{errors:?}");
            source = *stage;
        }
        let actual: Vec<f32> = read(&out, values.len());
        let expected: Vec<f64> = if passes == 0 {
            values[..total].iter().map(|&v| f64::from(v)).collect()
        } else {
            reference_smooth(&values, nodes, passes)
        };
        let worst = actual[..total]
            .iter()
            .zip(&expected)
            .map(|(a, e)| (f64::from(*a) - e).abs())
            .fold(0.0, f64::max);
        assert!(worst < 1e-5, "{passes} passes: worst difference {worst}");
        assert_eq!(
            &actual[total..],
            &values[total..],
            "{passes} passes: values past the lattice pass through"
        );
    }
    let (_, errors) = harness.run(
        &mut SmoothLattice::new(),
        &[("levelset", input)],
        &[("smoothed", out_slot)],
        &params(&[("nodes_x", 0.0), ("passes", 2.0)]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        read::<f32>(&out, values.len()),
        values,
        "no lattice copies the input"
    );
}

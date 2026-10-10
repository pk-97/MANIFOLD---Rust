//! Checked against FLIP Fluids polygonizer3d.cpp (MIT); see THIRD_PARTY_NOTICES.md.
//! GPU value proofs for the liquid-surface atoms (GPU_FLUID_SURFACE_DESIGN.md
//! P5–P6) against CPU f64 references. Each atom runs through its own `run()`
//! on a real device with pre-bound shared buffers; scalar inputs arrive as the
//! params they shadow.

use std::borrow::Cow;

use manifold_gpu::GpuBuffer;

use manifold_water_liquid::primitives::running_total::RunningTotal;
use manifold_water_liquid::primitives::sort_particles_into_cells::SortParticlesIntoCells;
use manifold_node_engine::exec::backend::Backend;
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::fluid_particles::{CellRange, bin_counts};
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::primitive::Primitive;

use crate::testkit::liquid_surface::*;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};



#[test]
fn fluid_sort_particles_into_cells_is_a_binned_permutation() {
    let mut harness = Harness::new();
    let lattice = Lattice { center: [0.0, 1.0, 0.0], size: [2.0, 2.0, 2.0], cell: 0.25 };
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
                let bin = (rng.next_f32() * bins[axis] as f32).floor().min(bins[axis] as f32 - 1.0);
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
        assert_eq!(range.start, next, "bin {bin} starts where the previous one ends");
        assert_eq!(range.count as usize, ids.len(), "bin {bin} count");
        let mut actual: Vec<u32> = sorted[range.start as usize..(range.start + range.count) as usize]
            .iter()
            .map(|p| p.id)
            .collect();
        actual.sort_unstable();
        ids.sort_unstable();
        assert_eq!(&actual, ids, "bin {bin} members");
        next += range.count;
    }
    assert_eq!(next as usize, live);
    assert!(sorted[live..].iter().all(|p| p.position_radius[3] == 0.0), "tail is inactive");

    // `order` names each sorted slot's input index; past the live total, none.
    let (input, _) = harness.array(&particles, particles.len());
    let (sorted_slot, sorted_buf) = harness.array::<FluidParticle>(&[], particles.len());
    let (ranges_slot, _) = harness.array::<CellRange>(&[], bin_total);
    let (order_slot, order_buf) = harness.array::<u32>(&[], particles.len());
    let count_slot = harness.scalar_input(count as f32);
    let (_, errors) = harness.run(
        &mut SortParticlesIntoCells::new(),
        &[("particles", input), ("count", count_slot)],
        &[("sorted", sorted_slot), ("cell_ranges", ranges_slot), ("order", order_slot)],
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
            assert_eq!(index, u32::MAX, "slot {slot} past the live total names no input");
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
        let lattice = Lattice { center: [0.0; 3], size: [2.0; 3], cell };
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
            assert!(scalars.contains(&(*slot, ParamValue::Float(n as f32))), "cell {cell}: {scalars:?}");
        }
        let holds = harness.buffer(ranges).size / std::mem::size_of::<CellRange>() as u64;
        assert!(holds >= bins.iter().map(|&n| u64::from(n)).product::<u64>(), "cell {cell}: {holds} ranges");
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
        .map(|i| particle(std::array::from_fn(|_| (rng.next_f32() - 0.5) * 1.9), if i % 11 == 0 { 0.0 } else { 0.02 }, i + 1))
        .collect();
    let (input, _) = harness.array(&particles, particles.len());
    for cell in [0.5f32, 0.25] {
        let lattice = Lattice { center: [0.0; 3], size: [2.0; 3], cell };
        let bins = bin_counts(lattice.size, lattice.cell).iter().product::<u32>() as usize;
        let mut runs = Vec::new();
        for _ in 0..3 {
            let (sorted, sorted_buf) = harness.array::<FluidParticle>(&[], particles.len());
            let (ranges, _) = harness.array::<CellRange>(&[], 1);
            let (order, order_buf) = harness.array::<u32>(&[], particles.len());
            let (_, errors) = harness.run(
                &mut SortParticlesIntoCells::new(),
                &[("particles", input)],
                &[("sorted", sorted), ("cell_ranges", ranges), ("order", order)],
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
        assert!(runs.iter().all(|run| run.0 == *sorted && run.1 == *order), "cell {cell}: every run is byte-identical");
        let order: &[u32] = bytemuck::cast_slice(order);
        let crowded = ranges.iter().map(|r| r.count).max().unwrap_or(0);
        assert!(if cell == 0.5 { crowded > 32 } else { crowded <= 32 }, "cell {cell}: largest bin {crowded}");
        for range in ranges {
            let members = &order[range.start as usize..(range.start + range.count) as usize];
            assert!(members.windows(2).all(|w| w[0] < w[1]), "cell {cell}: a bin lists its particles in input order");
        }
    }
}

/// A consumer that wires only `order` and `cell_ranges` gets the same ranges and
/// the same members per bin as one that also wires `sorted`. Order within a bin
/// follows atomic ranks, so it may differ between any two runs.
#[test]
fn fluid_sort_particles_into_cells_runs_with_sorted_unwired() {
    let mut harness = Harness::new();
    let lattice = Lattice { center: [0.0; 3], size: [2.0; 3], cell: 0.25 };
    let bins = bin_counts(lattice.size, lattice.cell).iter().product::<u32>() as usize;
    let mut rng = Rng::new(23);
    let particles: Vec<FluidParticle> = (0..500u32)
        .map(|i| particle(std::array::from_fn(|_| (rng.next_f32() - 0.5) * 1.8), if i % 7 == 0 { 0.0 } else { 0.02 }, i + 1))
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
        let (_, errors) = harness.run(&mut SortParticlesIntoCells::new(), &[("particles", input)], &outputs, &lattice.params(&[]));
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
        assert!(order[live as usize..].iter().all(|&i| i == u32::MAX), "no input past the live total");
        (bytemuck::cast_slice::<CellRange, u8>(&ranges).to_vec(), members)
    };
    let wired = run(&mut harness, true);
    assert!(wired.1.iter().any(|bin| !bin.is_empty()), "the fixture sorts live particles");
    assert_eq!(run(&mut harness, false), wired, "ranges and bin members do not depend on sorted being wired");
}

/// Matter points sort in place: ranges and order are byte-identical to sorting
/// liquid particle records at the same positions, where a point is live exactly
/// when its id is non-zero and its position finite. Wiring `sorted` for them is
/// a named error, since it holds liquid particle records.
#[test]
fn fluid_sort_particles_into_cells_sorts_matter_points_in_place() {
    use crate::matter::MatterPoint;
    let mut harness = Harness::new();
    let lattice = Lattice { center: [0.0; 3], size: [2.0; 3], cell: 0.25 };
    let bins = bin_counts(lattice.size, lattice.cell).iter().product::<u32>() as usize;
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
        assert_eq!(ranges_buf.size, (bins * std::mem::size_of::<CellRange>()) as u64, "one range per bin");
        (read::<u8>(&ranges_buf, ranges_buf.size as usize), read::<u8>(&order_buf, order_buf.size as usize))
    };
    let (matter_input, _) = harness.array(&points, points.len());
    let (particle_input, _) = harness.array(&particles, particles.len());
    let matter = run(&mut harness, matter_input);
    let liquid = run(&mut harness, particle_input);
    let ranges: &[CellRange] = bytemuck::cast_slice(&liquid.0);
    let live: u32 = ranges.iter().map(|r| r.count).sum();
    assert!(live > 2000 && (live as usize) < count, "the fixture has live and dead points: {live}");
    assert_eq!(matter, liquid, "matter points bin and order exactly as the equivalent liquid particles");

    let (sorted, _) = harness.array::<FluidParticle>(&[], points.len());
    let (ranges, _) = harness.array::<CellRange>(&[], bins);
    let (_, errors) = harness.run(
        &mut SortParticlesIntoCells::new(),
        &[("particles", matter_input)],
        &[("sorted", sorted), ("cell_ranges", ranges)],
        &lattice.params(&[]),
    );
    assert!(errors.iter().any(|e| e.contains("sorted holds liquid particle records")), "{errors:?}");
}

/// A record with no position the sort can find is a named error.
#[test]
fn fluid_sort_particles_into_cells_rejects_records_without_a_position() {
    let mut harness = Harness::new();
    let lattice = Lattice { center: [0.0; 3], size: [2.0; 3], cell: 0.25 };
    let (input, _) = harness.array(&[CellRange { start: 0, count: 1 }], 1);
    let (ranges, _) = harness.array::<CellRange>(&[], 512);
    let (_, errors) = harness.run(
        &mut SortParticlesIntoCells::new(),
        &[("particles", input)],
        &[("cell_ranges", ranges)],
        &lattice.params(&[]),
    );
    assert!(errors.iter().any(|e| e.contains("position_radius") && e.contains("position and id")), "{errors:?}");
}

/// With `enabled` 0 the sort does nothing: after a changed input, every output
/// is byte-identical to the previous call's.
#[test]
fn fluid_sort_particles_into_cells_disabled_leaves_outputs_untouched() {
    let mut harness = Harness::new();
    let lattice = Lattice { center: [0.0; 3], size: [2.0; 3], cell: 0.25 };
    let bins = bin_counts(lattice.size, lattice.cell).iter().product::<u32>() as usize;
    let mut rng = Rng::new(11);
    let mut cloud = |seed: f32| -> Vec<FluidParticle> {
        (0..300u32)
            .map(|i| particle(std::array::from_fn(|_| (rng.next_f32() - 0.5) * 1.8 * seed), 0.02, i + 1))
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
            &[("sorted", sorted), ("cell_ranges", ranges), ("order", order)],
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
    assert_eq!(run(&mut harness, &second, 0.0), enabled, "a disabled sort leaves every output as it was");
    assert_ne!(run(&mut harness, &second, 1.0), enabled, "re-enabled, it sorts the new particles");
}

/// A producer without a frame yet is pending, so the executor holds this node;
/// a lattice that reaches it is real, and a broken one is an error even with
/// no particles.
#[test]
fn fluid_sort_particles_into_cells_rejects_a_broken_lattice_at_any_count() {
    let mut harness = Harness::new();
    let no_lattice = Lattice { center: [0.0; 3], size: [4.0; 3], cell: -4.0 };
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
        let first_total = scalars.iter().find(|(slot, _)| *slot == total_slot).map(|(_, v)| v.clone());
        assert_eq!(first_total, Some(ParamValue::Float(0.0)), "a fresh node reports nothing yet");
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
        let lagged = scalars.iter().find(|(slot, _)| *slot == total_slot).map(|(_, v)| v.clone());
        assert_eq!(lagged, Some(ParamValue::Float(running as f32)), "size {size}: one frame late");
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
        let mut run = || harness.run(&mut node, &[("in", input)], &[("out", out_slot)], &params).1;
        assert!(run().is_empty(), "the first frame has no total yet");
        for frame in 1..3 {
            let errors = run();
            if expect_error {
                assert!(
                    errors.iter().any(|e| e.contains("needs 100, holds 64")),
                    "frame {frame}: an overflow must be named every frame: {errors:?}"
                );
            } else {
                assert!(errors.is_empty(), "frame {frame}: capacity {capacity} holds 100: {errors:?}");
            }
        }
    }
}

/// f64 mirror of `shape_particle_blobs_body.wgsl` by brute force over every
/// particle (equivalent to the 27-bin search: the kernel radius never passes
/// one bin). Returns (centre, G, bound) for active particles.
fn reference_blob(
    particles: &[FluidParticle],
    index: usize,
    _cell: f64,
    particle_scale: f64,
    stretch: f64,
    smoothing: f64,
    isolated_scale: f64,
    min_neighbours: usize,
) -> ([f64; 3], [[f64; 3]; 3], f64) {
    let p = particles[index].position_radius;
    let x = [p[0] as f64, p[1] as f64, p[2] as f64];
    let physical = p[3] as f64;
    let radius = particle_scale * physical;
    let (mut weight_sum, mut mean, mut neighbours, mut nearest) = (0.0, [0.0; 3], 0, f64::INFINITY);
    let mut members = Vec::new();
    for (k, other) in particles.iter().enumerate() {
        if other.position_radius[3] <= 0.0 {
            continue;
        }
        let o = [other.position_radius[0] as f64, other.position_radius[1] as f64, other.position_radius[2] as f64];
        let d = ((o[0] - x[0]).powi(2) + (o[1] - x[1]).powi(2) + (o[2] - x[2]).powi(2)).sqrt();
        if k != index {
            nearest = nearest.min(d);
        }
        if d < radius {
            let w = 1.0 - (d / radius).powi(3);
            weight_sum += w;
            for axis in 0..3 {
                mean[axis] += w * o[axis];
            }
            neighbours += 1;
            members.push((o, w));
        }
    }
    let mean = mean.map(|v| v / weight_sum);
    let centre: [f64; 3] = std::array::from_fn(|axis| x[axis] + smoothing * (mean[axis] - x[axis]));
    let t = ((nearest - 2.0 * physical) / physical).clamp(0.0, 1.0);
    let apart = t * t * (3.0 - 2.0 * t);
    let blob_radius = radius * (1.0 + (isolated_scale - 1.0) * apart);
    let mut axes = [blob_radius; 3];
    let mut basis = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    if neighbours >= min_neighbours {
        let mut c = [[0.0; 3]; 3];
        for (o, w) in &members {
            let off: [f64; 3] = std::array::from_fn(|axis| o[axis] - mean[axis]);
            for r in 0..3 {
                for col in 0..3 {
                    c[r][col] += w * off[r] * off[col] / weight_sum;
                }
            }
        }
        let (values, vectors) = jacobi(c);
        let largest = values.iter().cloned().fold(f64::MIN, f64::max);
        if largest > 1e-20 {
            let floor = largest / (stretch.max(1.0) * stretch.max(1.0));
            let spread = values.map(|v| v.max(floor).sqrt());
            let norm = (spread[0] * spread[1] * spread[2]).cbrt();
            axes = spread.map(|s| blob_radius * s / norm);
            basis = vectors;
        }
    }
    // G = V diag(1/a) Vᵀ with eigenvectors as columns of V (basis[row][col]).
    let g = std::array::from_fn(|r| {
        std::array::from_fn(|col| (0..3).map(|k| basis[r][k] * basis[col][k] / axes[k]).sum())
    });
    (centre, g, axes.iter().cloned().fold(0.0, f64::max))
}

/// Symmetric 3×3 eigen-decomposition; eigenvectors are the columns.
fn jacobi(mut a: [[f64; 3]; 3]) -> ([f64; 3], [[f64; 3]; 3]) {
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..30 {
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q].abs() < 1e-300 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let t = if theta == 0.0 { 1.0 } else { t };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            let mut r = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
            r[p][p] = c;
            r[q][q] = c;
            r[p][q] = s;
            r[q][p] = -s;
            let mul = |x: [[f64; 3]; 3], y: [[f64; 3]; 3]| -> [[f64; 3]; 3] {
                std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| x[i][k] * y[k][j]).sum()))
            };
            let rt = std::array::from_fn(|i| std::array::from_fn(|j| r[j][i]));
            a = mul(mul(rt, a), r);
            v = mul(v, r);
        }
    }
    ([a[0][0], a[1][1], a[2][2]], v)
}



#[test]
fn fluid_shape_particle_blobs_match_reference_shapes() {
    let mut harness = Harness::new();
    let lattice = Lattice { center: [0.0, 0.0, 0.0], size: [4.0, 4.0, 4.0], cell: 0.25 };
    let r = 0.05_f32;
    let mut particles = Vec::new();
    // A line along x (spacing 0.8 r): stretched along the line.
    for i in 0..9 {
        particles.push(particle([-1.0 + i as f32 * 0.8 * r, 0.5, 0.5], r, particles.len() as u32 + 1));
    }
    // A uniform cloud: near-isotropic in the middle.
    for i in 0..5 {
        for j in 0..5 {
            for k in 0..5 {
                let o = [i, j, k].map(|n| (n as f32 - 2.0) * 0.9 * r);
                particles.push(particle([1.0 + o[0], -1.0 + o[1], 0.3 + o[2]], r, particles.len() as u32 + 1));
            }
        }
    }
    // An isolated particle, and a pair 2.5 radii apart.
    let isolated = particles.len();
    particles.push(particle([-1.2, -1.2, -1.2], r, particles.len() as u32 + 1));
    let pair = particles.len();
    particles.push(particle([1.2, 1.2, -1.0], r, particles.len() as u32 + 1));
    particles.push(particle([1.2 + 2.5 * r, 1.2, -1.0], r, particles.len() as u32 + 1));

    let (scale, stretch, smoothing, iso, min_n) = (3.0_f32, 4.0_f32, 0.9_f32, 0.5_f32, 6);
    let shape = [
        ("particle_scale", scale),
        ("stretch", stretch),
        ("smoothing", smoothing),
        ("isolated_scale", iso),
        ("min_neighbours", min_n as f32),
    ];
    let (sorted, _, blobs, _) = sort_and_shape(&mut harness, &lattice, &particles, particles.len(), &shape);
    let by_id = |id: u32| sorted.iter().position(|p| p.id == id).expect("sorted id");
    let mut worst = 0.0_f64;
    for (index, blob) in blobs.iter().enumerate().take(particles.len()) {
        let (centre, g, bound) = reference_blob(
            &sorted,
            index,
            f64::from(lattice.cell),
            f64::from(scale),
            f64::from(stretch),
            f64::from(smoothing),
            f64::from(iso),
            min_n,
        );
        let actual_g = blob_matrix(blob);
        let unit = 1.0 / (f64::from(scale * r));
        for row in 0..3 {
            for col in 0..3 {
                worst = worst.max((actual_g[row][col] - g[row][col]).abs() / unit);
            }
            assert!((f64::from(blob.center_radius[row]) - centre[row]).abs() < 1e-5, "blob {index} centre");
        }
        assert!((f64::from(blob.center_radius[3]) - bound).abs() / bound < 1e-3, "blob {index} bound");
    }
    assert!(worst < 2e-3, "shape matrices differ from the reference by {worst} of 1/radius");

    // Line: the long axis is the line (G shrinks x least).
    let line = blob_matrix(&blobs[by_id(5)]);
    assert!(line[0][0] < 0.6 * line[1][1] && line[0][0] < 0.6 * line[2][2], "{line:?}");
    // Cloud centre: isotropic within 15%.
    let cloud = blob_matrix(&blobs[by_id(9 + 62 + 1)]);
    let mean_diag = (cloud[0][0] + cloud[1][1] + cloud[2][2]) / 3.0;
    for axis in 0..3 {
        assert!((cloud[axis][axis] / mean_diag - 1.0).abs() < 0.15, "{cloud:?}");
    }
    // Isolated: a sphere of isolated_scale × kernel radius.
    let lone = blobs[by_id(isolated as u32 + 1)];
    assert!((f64::from(lone.center_radius[3]) - f64::from(iso * scale * r)).abs() < 1e-6);
    // Pair at 2.5 r: halfway through the smoothstep, between both sizes.
    let paired = blobs[by_id(pair as u32 + 1)].center_radius[3];
    assert!(paired < scale * r && paired > iso * scale * r, "{paired}");
}

/// Stretch 1 skips the covariance sweep and the eigensolve: every well-populated
/// blob is written as an exact sphere (G = I / radius, off-diagonals zero), and
/// that sphere is the reference's answer for stretch 1 to rounding.
#[test]
fn fluid_shape_particle_blobs_stretch_one_is_an_exact_sphere() {
    let mut harness = Harness::new();
    let lattice = Lattice { center: [0.0, 0.0, 0.0], size: [4.0, 4.0, 4.0], cell: 0.25 };
    let r = 0.05_f32;
    let mut particles = Vec::new();
    // A line (anisotropic covariance) and a cloud (near-isotropic): both must
    // come out as spheres.
    for i in 0..9 {
        particles.push(particle([-1.0 + i as f32 * 0.8 * r, 0.5, 0.5], r, particles.len() as u32 + 1));
    }
    for i in 0..5 {
        for j in 0..5 {
            for k in 0..5 {
                let o = [i, j, k].map(|n| (n as f32 - 2.0) * 0.9 * r);
                particles.push(particle([1.0 + o[0], -1.0 + o[1], 0.3 + o[2]], r, particles.len() as u32 + 1));
            }
        }
    }
    let (scale, smoothing, iso, min_n) = (3.0_f32, 0.9_f32, 0.5_f32, 6);
    let shape = [
        ("particle_scale", scale),
        ("stretch", 1.0),
        ("smoothing", smoothing),
        ("isolated_scale", iso),
        ("min_neighbours", min_n as f32),
    ];
    let (sorted, _, blobs, _) = sort_and_shape(&mut harness, &lattice, &particles, particles.len(), &shape);
    let mut populated = 0;
    for (index, blob) in blobs.iter().enumerate().take(particles.len()) {
        let (centre, g, bound) = reference_blob(
            &sorted,
            index,
            f64::from(lattice.cell),
            f64::from(scale),
            1.0,
            f64::from(smoothing),
            f64::from(iso),
            min_n,
        );
        assert_eq!(&blob.shape_off[..3], &[0.0; 3], "blob {index} is not a sphere");
        let shift = (0..3).map(|a| (blob.center_radius[a] - sorted[index].position_radius[a]).powi(2)).sum::<f32>().sqrt();
        assert!((blob.shape_off[3] - shift).abs() < 1e-6, "blob {index} search displacement");
        let diag = blob.shape_diag;
        assert!(diag[0] == diag[1] && diag[1] == diag[2], "blob {index} axes differ: {diag:?}");
        assert!((f64::from(diag[0]) * f64::from(blob.center_radius[3]) - 1.0).abs() < 1e-5, "blob {index} G ≠ 1 / bound");
        let actual = blob_matrix(blob);
        for row in 0..3 {
            assert!((f64::from(blob.center_radius[row]) - centre[row]).abs() < 1e-5, "blob {index} centre");
            for col in 0..3 {
                assert!(
                    (actual[row][col] - g[row][col]).abs() * bound < 1e-5,
                    "blob {index} G[{row}][{col}] differs from the stretch-1 reference"
                );
            }
        }
        assert!((f64::from(blob.center_radius[3]) - bound).abs() / bound < 1e-5, "blob {index} bound");
        populated += 1;
    }
    assert_eq!(populated, particles.len());
}















// --- P6: marching cubes ---------------------------------------------------

use super::count_surface_edges::CountSurfaceEdges;
use super::count_surface_triangles::CountSurfaceTriangles;
use super::volume_surface_mesh::VolumeSurfaceMesh;
use manifold_node_engine::mesh::MeshVertex;

/// Upstream's triangle table, parsed from the vendored source so the packed
/// WGSL table is checked against its origin, not against itself.
fn upstream_triangle_table() -> Vec<[i32; 16]> {
    let source = include_str!("../../../manifold-fluids/native/flip_engine/polygonizer3d.cpp");
    let start = source.find("_triTable[256][16] = {").expect("upstream triangle table");
    source[start..]
        .lines()
        .skip(1)
        .filter(|line| line.trim_start().starts_with('{'))
        .take(256)
        .map(|line| {
            let values: Vec<i32> = line
                .trim()
                .trim_start_matches('{')
                .trim_end_matches([',', '}', ';', ' '])
                .split(',')
                .map(|v| v.trim().trim_end_matches('}').trim().parse().expect("table entry"))
                .collect();
            values.try_into().expect("sixteen entries")
        })
        .collect()
}

const CORNERS: [[u32; 3]; 8] = [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1], [0, 1, 0], [1, 1, 0], [1, 1, 1], [0, 1, 1]];
const EDGES: [(usize, usize); 12] =
    [(0, 1), (1, 2), (2, 3), (3, 0), (4, 5), (5, 6), (6, 7), (7, 4), (0, 4), (1, 5), (2, 6), (3, 7)];

struct SphereLevelSet {
    nodes: u32,
    min: f32,
    size: f32,
    center: [f32; 3],
    radius: f32,
    values: Vec<f32>,
}

impl SphereLevelSet {
    fn new(nodes: u32, radius: f32) -> Self {
        let (min, size, center) = (-1.0, 2.0, [0.05, -0.03, 0.02]);
        let h = size / (nodes - 1) as f32;
        let values = (0..nodes.pow(3))
            .map(|i| {
                let ijk = [i % nodes, (i / nodes) % nodes, i / (nodes * nodes)];
                let d: f32 = (0..3).map(|a| (min + ijk[a] as f32 * h - center[a]).powi(2)).sum::<f32>().sqrt();
                d - radius
            })
            .collect();
        Self { nodes, min, size, center, radius, values }
    }

    /// Foam: a random sign at every node, so the surface has far more area
    /// than the lattice's own box.
    fn foam(nodes: u32, seed: u64) -> Self {
        let mut level_set = Self::new(nodes, 0.5);
        let mut rng = Rng::new(seed);
        for value in &mut level_set.values {
            *value = rng.next_f32() - 0.5;
        }
        level_set
    }

    /// A crossing on the lattice boundary exercises the ownership rule's
    /// clamped neighbouring cells and its lowest-index edge convention.
    fn boundary(nodes: u32) -> Self {
        let min = -1.0;
        let size = 2.0;
        let values = (0..nodes.pow(3))
            .map(|i| if i % nodes == 0 { -1.0 } else { 1.0 })
            .collect();
        Self { nodes, min, size, center: [0.0; 3], radius: 0.0, values }
    }

    fn phi(&self, p: [u32; 3]) -> f64 {
        f64::from(self.values[(p[0] + self.nodes * (p[1] + self.nodes * p[2])) as usize])
    }

    /// CPU f64 marching cubes in the atoms' enumeration order: cells by index,
    /// triangles by table order.
    fn reference_positions(&self) -> Vec<[f64; 3]> {
        let table = upstream_triangle_table();
        let cells = self.nodes - 1;
        let h = f64::from(self.size) / f64::from(cells);
        let mut out = Vec::new();
        for c in 0..cells.pow(3) {
            let cell = [c % cells, (c / cells) % cells, c / (cells * cells)];
            let corner = |k: usize| [cell[0] + CORNERS[k][0], cell[1] + CORNERS[k][1], cell[2] + CORNERS[k][2]];
            let case = (0..8).filter(|&k| self.phi(corner(k)) < 0.0).fold(0usize, |acc, k| acc | (1 << k));
            for &edge in table[case].iter().take_while(|&&e| e >= 0) {
                let (a, b) = EDGES[edge as usize];
                let (pa, pb) = (corner(a), corner(b));
                let (fa, fb) = (self.phi(pa), self.phi(pb));
                let mu = (fa / (fa - fb)).clamp(0.0, 1.0);
                out.push(std::array::from_fn(|axis| {
                    f64::from(self.min) + (f64::from(pa[axis]) + mu * (f64::from(pb[axis]) - f64::from(pa[axis]))) * h
                }));
            }
        }
        out
    }
}

struct MeshRun {
    vertices: Vec<MeshVertex>,
    errors: Vec<String>,
    total: Option<ParamValue>,
    levelset: Slot,
    scan: Slot,
    vertices_slot: Slot,
}

fn run_marching_cubes(
    harness: &mut Harness,
    level_set: &SphereLevelSet,
    capacity: u32,
    reported_total: Option<f32>,
) -> MeshRun {
    run_marching_cubes_on(harness, &mut VolumeSurfaceMesh::new(), level_set, capacity, reported_total)
}

/// One frame of `mesh`, which keeps its grown buffer between calls. Reads
/// the whole buffer the node provides.
fn run_marching_cubes_on(
    harness: &mut Harness,
    mesh: &mut VolumeSurfaceMesh,
    level_set: &SphereLevelSet,
    capacity: u32,
    reported_total: Option<f32>,
) -> MeshRun {
    let (levelset_slot, _) = harness.array(&level_set.values, level_set.values.len());
    let (counts_slot, _) = harness.array::<u32>(&[], level_set.values.len());
    let n = level_set.nodes as f32;
    let nodes = params(&[("nodes_x", n), ("nodes_y", n), ("nodes_z", n)]);
    let (_, errors) = harness.run(
        &mut CountSurfaceTriangles::new(),
        &[("levelset", levelset_slot)],
        &[("counts", counts_slot)],
        &nodes,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let (scan_slot, _) = harness.array::<u32>(&[], level_set.values.len());
    let total_slot = harness.scalar();
    let mut running = RunningTotal::new();
    let mut total = None;
    // The second frame reports the first frame's total.
    for _ in 0..2 {
        let (scalars, errors) = harness.run(
            &mut running,
            &[("in", counts_slot)],
            &[("out", scan_slot), ("total", total_slot)],
            &ParamValues::default(),
        );
        assert!(errors.is_empty(), "{errors:?}");
        total = scalars.iter().find(|(slot, _)| *slot == total_slot).map(|(_, v)| v.clone());
    }
    let (vertices_slot, _) = harness.array::<MeshVertex>(&[], 3);
    let centre = level_set.min + 0.5 * level_set.size;
    let mut mesh_params = params(&[
        ("center_x", centre),
        ("center_y", centre),
        ("center_z", centre),
        ("size_x", level_set.size),
        ("size_y", level_set.size),
        ("size_z", level_set.size),
        ("nodes_x", n),
        ("nodes_y", n),
        ("nodes_z", n),
        ("resolution_scale", 1.0),
        ("max_capacity", capacity as f32),
    ]);
    if let Some(total) = reported_total {
        mesh_params.insert(Cow::Borrowed("total"), ParamValue::Float(total));
    }
    let (_, errors) = harness.run(
        mesh,
        &[("levelset", levelset_slot), ("scan", scan_slot)],
        &[("vertices", vertices_slot)],
        &mesh_params,
    );
    let provided = harness.buffer(vertices_slot);
    let slots = (provided.size / std::mem::size_of::<MeshVertex>() as u64) as usize;
    MeshRun {
        vertices: read(&provided, slots),
        errors,
        total,
        levelset: levelset_slot,
        scan: scan_slot,
        vertices_slot,
    }
}

struct IndexedMeshRun {
    vertices: Vec<MeshVertex>,
    indices: Vec<u32>,
    errors: Vec<String>,
    total: Option<ParamValue>,
    levelset: Slot,
    scan: Slot,
    edge_scan: Slot,
    vertices_slot: Slot,
    indices_slot: Slot,
    extent_slot: Option<Slot>,
}

/// The indexed sibling of `run_marching_cubes_on`: triangle counts still own
/// triangle order, while the second count/scan assigns one compact slot to
/// every crossed lattice edge.
fn run_indexed_marching_cubes_on(
    harness: &mut Harness,
    mesh: &mut VolumeSurfaceMesh,
    level_set: &SphereLevelSet,
    capacity: u32,
    reported_total: Option<f32>,
    with_extent: bool,
) -> IndexedMeshRun {
    let (levelset_slot, _) = harness.array(&level_set.values, level_set.values.len());
    let (counts_slot, _) = harness.array::<u32>(&[], level_set.values.len());
    let n = level_set.nodes as f32;
    let nodes = params(&[("nodes_x", n), ("nodes_y", n), ("nodes_z", n)]);
    let (_, errors) = harness.run(
        &mut CountSurfaceTriangles::new(),
        &[("levelset", levelset_slot)],
        &[("counts", counts_slot)],
        &nodes,
    );
    assert!(errors.is_empty(), "{errors:?}");

    let (scan_slot, _) = harness.array::<u32>(&[], level_set.values.len());
    let total_slot = harness.scalar();
    let extent = with_extent.then(|| harness.array::<u32>(&[], 4));
    let mut running = RunningTotal::new();
    let mut total = None;
    for _ in 0..2 {
        let mut outputs = vec![("out", scan_slot), ("total", total_slot)];
        if let Some((extent_slot, _)) = &extent {
            outputs.push(("extent", *extent_slot));
        }
        let (scalars, errors) = harness.run(
            &mut running,
            &[("in", counts_slot)],
            &outputs,
            &params(&[("per_item", 3.0)]),
        );
        assert!(errors.is_empty(), "{errors:?}");
        total = scalars.iter().find(|(slot, _)| *slot == total_slot).map(|(_, v)| v.clone());
    }

    let (edge_counts_slot, _) = harness.array::<u32>(&[], level_set.values.len());
    let (_, errors) = harness.run(
        &mut CountSurfaceEdges::new(),
        &[("levelset", levelset_slot)],
        &[("counts", edge_counts_slot)],
        &nodes,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let (edge_scan_slot, _) = harness.array::<u32>(&[], level_set.values.len());
    let (_, errors) = harness.run(
        &mut RunningTotal::new(),
        &[("in", edge_counts_slot)],
        &[("out", edge_scan_slot)],
        &ParamValues::default(),
    );
    assert!(errors.is_empty(), "{errors:?}");

    let (vertices_slot, _) = harness.array::<MeshVertex>(&[], capacity as usize);
    let (indices_slot, _) = harness.array::<u32>(&[], capacity as usize);
    let centre = level_set.min + 0.5 * level_set.size;
    let mut mesh_params = params(&[
        ("center_x", centre),
        ("center_y", centre),
        ("center_z", centre),
        ("size_x", level_set.size),
        ("size_y", level_set.size),
        ("size_z", level_set.size),
        ("nodes_x", n),
        ("nodes_y", n),
        ("nodes_z", n),
        ("resolution_scale", 1.0),
        ("max_capacity", capacity as f32),
    ]);
    if let Some(total) = reported_total {
        mesh_params.insert(Cow::Borrowed("total"), ParamValue::Float(total));
    }
    let mut mesh_inputs = vec![
        ("levelset", levelset_slot),
        ("scan", scan_slot),
        ("edge_scan", edge_scan_slot),
    ];
    if let Some((extent_slot, _)) = &extent {
        mesh_inputs.push(("extent", *extent_slot));
    }
    let (_, errors) = harness.run(
        mesh,
        &mesh_inputs,
        &[("vertices", vertices_slot), ("indices", indices_slot)],
        &mesh_params,
    );
    let vertices = harness.buffer(vertices_slot);
    let indices = harness.buffer(indices_slot);
    let vertex_slots = (vertices.size / std::mem::size_of::<MeshVertex>() as u64) as usize;
    let index_slots = (indices.size / std::mem::size_of::<u32>() as u64) as usize;
    IndexedMeshRun {
        vertices: read(&vertices, vertex_slots),
        indices: read(&indices, index_slots),
        errors,
        total,
        levelset: levelset_slot,
        scan: scan_slot,
        edge_scan: edge_scan_slot,
        vertices_slot,
        indices_slot,
        extent_slot: extent.as_ref().map(|(slot, _)| *slot),
    }
}

/// Res 64 at Surface Detail 2 is a 261³ lattice: more than 65,535 threadgroups
/// of 256 in one dimension. The only crossing sits in the last cell layer.
#[test]
fn fluid_count_surface_triangles_reaches_cells_past_65535_threadgroups() {
    let mut harness = Harness::new();
    let n = 262usize;
    let values: Vec<f32> = (0..n * n * n).map(|index| if index / (n * n) == n - 1 { -1.0 } else { 1.0 }).collect();
    let (levelset, _) = harness.array(&values, values.len());
    let (counts_slot, counts) = harness.array::<u32>(&[], values.len());
    let nodes = n as f32;
    let (_, errors) = harness.run(
        &mut CountSurfaceTriangles::new(),
        &[("levelset", levelset)],
        &[("counts", counts_slot)],
        &params(&[("nodes_x", nodes), ("nodes_y", nodes), ("nodes_z", nodes)]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let counts: Vec<u32> = read(&counts, values.len());
    let last_layer = (n - 1) * (n - 1) * (n - 2);
    assert!(last_layer > 65_535 * 256, "the fixture must cross the threadgroup boundary");
    let total: u64 = counts.iter().map(|&c| u64::from(c)).sum();
    assert_eq!(total, 2 * ((n - 1) * (n - 1)) as u64, "every cell of the last layer has two triangles");
}

fn triangle_area(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> f64 {
    let u: [f64; 3] = std::array::from_fn(|i| f64::from(b[i] - a[i]));
    let v: [f64; 3] = std::array::from_fn(|i| f64::from(c[i] - a[i]));
    let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
}

fn is_zero(vertex: &MeshVertex) -> bool {
    bytemuck::bytes_of(vertex).iter().all(|&b| b == 0)
}

fn sorted_triangle_bytes(vertices: &[MeshVertex]) -> Vec<Vec<u8>> {
    let mut triangles: Vec<Vec<u8>> = vertices
        .chunks_exact(3)
        .map(|triangle| bytemuck::cast_slice::<MeshVertex, u8>(triangle).to_vec())
        .collect();
    triangles.sort_unstable();
    triangles
}

/// Expand an indexed triangle list and compare the complete vertex records,
/// including normals, UVs, tangents, and colour. The indexed path owns one
/// record per crossed edge; its indices retain the original table order.
fn assert_indexed_expansion_matches(
    level_set: &SphereLevelSet,
    direct: &MeshRun,
    indexed: &IndexedMeshRun,
    radial_winding: bool,
) -> usize {
    assert!(direct.errors.is_empty(), "direct: {:?}", direct.errors);
    assert!(indexed.errors.is_empty(), "indexed: {:?}", indexed.errors);
    assert_eq!(indexed.total, direct.total, "triangle totals differ");
    let triangles = match direct.total.as_ref() {
        Some(ParamValue::Float(total)) => *total as usize,
        other => panic!("missing triangle total: {other:?}"),
    };
    let triangle_vertices = triangles * 3;
    assert!(triangle_vertices > 0, "fixture must cross the surface");
    assert!(direct.vertices.len() >= triangle_vertices);
    assert!(indexed.indices.len() >= triangle_vertices);

    let unique_live = indexed.indices[..triangle_vertices]
        .iter()
        .copied()
        .max()
        .map_or(0, |index| index as usize + 1);
    assert!(unique_live < triangle_vertices, "indexed mesh did not share vertices");
    for (triangle, &index) in indexed.indices[..triangle_vertices].iter().enumerate() {
        assert!((index as usize) < unique_live, "index {triangle} is out of bounds: {index} >= {unique_live}");
    }

    let expanded: Vec<MeshVertex> = indexed.indices[..triangle_vertices]
        .iter()
        .map(|&index| indexed.vertices[index as usize])
        .collect();
    assert_eq!(
        bytemuck::cast_slice::<MeshVertex, u8>(&expanded),
        bytemuck::cast_slice::<MeshVertex, u8>(&direct.vertices[..triangle_vertices]),
        "indexed expansion changes the triangle-list vertex records"
    );
    assert_eq!(sorted_triangle_bytes(&expanded), sorted_triangle_bytes(&direct.vertices[..triangle_vertices]));
    assert!(direct.vertices[triangle_vertices..].iter().all(is_zero), "direct vertex tail is not zero");
    assert!(indexed.vertices[unique_live..].iter().all(is_zero), "indexed vertex tail is not zero");
    assert!(indexed.indices[triangle_vertices..].iter().all(|&index| index == 0), "index tail is not zero");

    let expected = level_set.reference_positions();
    assert_eq!(expected.len(), triangle_vertices, "f64 reference triangle count");
    let worst = expanded
        .iter()
        .zip(expected)
        .flat_map(|(vertex, want)| vertex.position.into_iter().zip(want))
        .map(|(got, want)| (f64::from(got) - want).abs())
        .fold(0.0, f64::max);
    assert!(worst < 1e-5, "indexed positions differ from the independent f64 reference by {worst}");
    if radial_winding {
        for (triangle, vertices) in expanded.chunks_exact(3).enumerate() {
            let u: [f32; 3] = std::array::from_fn(|axis| vertices[1].position[axis] - vertices[0].position[axis]);
            let v: [f32; 3] = std::array::from_fn(|axis| vertices[2].position[axis] - vertices[0].position[axis]);
            let face = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let out: f32 = (0..3)
                .map(|axis| face[axis] * (vertices[0].position[axis] - level_set.center[axis]))
                .sum();
            assert!(out > 0.0, "triangle {triangle} winds inward");
        }
    }
    triangle_vertices
}

#[test]
fn fluid_volume_surface_mesh_matches_cpu_marching_cubes_on_a_sphere() {
    let mut harness = Harness::new();
    let sphere = SphereLevelSet::new(49, 0.7);
    let expected = sphere.reference_positions();
    assert_eq!(expected.len() % 3, 0);
    let run = run_marching_cubes(&mut harness, &sphere, 200_000, None);
    assert!(run.errors.is_empty(), "{:?}", run.errors);
    assert_eq!(run.total, Some(ParamValue::Float((expected.len() / 3) as f32)));
    let mut worst = 0.0_f64;
    for (index, (vertex, want)) in run.vertices.iter().zip(&expected).enumerate() {
        for (got, reference) in vertex.position.iter().zip(want) {
            worst = worst.max((f64::from(*got) - reference).abs());
        }
        let outward: [f32; 3] = std::array::from_fn(|a| vertex.position[a] - sphere.center[a]);
        let dot: f32 = (0..3).map(|a| outward[a] * vertex.normal[a]).sum::<f32>()
            / (0..3).map(|a| outward[a] * outward[a]).sum::<f32>().sqrt();
        assert!(dot > 0.99, "vertex {index}: normal {:?} is not outward", vertex.normal);
        assert_eq!(vertex.color, [1.0; 4]);
    }
    assert!(worst < 1e-5, "positions differ from the CPU reference by {worst} m");
    assert!(run.vertices[expected.len()..].iter().all(is_zero), "the tail past the live triangles is zeroed");
    let area: f64 = run.vertices[..expected.len()]
        .chunks_exact(3)
        .map(|t| triangle_area(t[0].position, t[1].position, t[2].position))
        .sum();
    let analytic = 4.0 * std::f64::consts::PI * f64::from(sphere.radius).powi(2);
    assert!((area / analytic - 1.0).abs() < 0.01, "area {area} vs {analytic}");
}

#[test]
fn fluid_mesh_grid_native_plane_crossing_matches_engine() {
    let mut harness = Harness::new();
    for resolution in [8, 16] {
        let layout = manifold_core::fluid_domain::domain_layout(None, 2.0, resolution).unwrap();
        let mesh = manifold_water_liquid::lattice::LiquidLattice::from_layout(&layout).surface();
        // Translate Y/Z to X's origin so the existing cubic MC harness can
        // exercise the native grid against the authored low wall at x=-1.
        let n = mesh.nodes()[0];
        let min = mesh.min()[0];
        let crossing = -1.0;
        let plane = SphereLevelSet {
            nodes: n, min, size: mesh.bounds().scale[0], center: [0.0; 3], radius: 0.0,
            values: (0..n.pow(3)).map(|i| min + (i % n) as f32 * mesh.cell_size() - crossing).collect(),
        };
        let count = plane.values.len();
        let (field, _) = harness.array(&plane.values, count);
        let (grown, grown_buffer) = harness.array::<f32>(&[], count);
        let offset = -0.25 * mesh.cell_size();
        let (_, errors) = harness.run(&mut manifold_water_liquid::primitives::offset_lattice::OffsetLattice::new(),
            &[("levelset", field)], &[("out", grown)], &params(&[("offset", offset)]));
        assert!(errors.is_empty(), "{errors:?}");
        let grown_values = read::<f32>(&grown_buffer, count);
        for (&got, &original) in grown_values.iter().zip(&plane.values) {
            assert_eq!(got, original + offset);
        }
        let (distance, distance_buffer) = harness.array::<f32>(&[], count);
        let band = 2.0 * mesh.cell_size();
        let (_, errors) = harness.run(&mut manifold_water_liquid::primitives::redistance_lattice::RedistanceLattice::new(),
            &[("levelset", grown)], &[("out", distance)], &params(&[
                ("nodes_x", n as f32), ("nodes_y", n as f32), ("nodes_z", n as f32),
                ("size_x", plane.size), ("size_y", plane.size), ("size_z", plane.size),
                ("band", band),
            ]));
        assert!(errors.is_empty(), "{errors:?}");
        for (got, want) in read::<f32>(&distance_buffer, count).into_iter().zip(grown_values) {
            assert!((got - want.clamp(-band, band)).abs() < 1e-6);
        }
        let mut source = field;
        for axis in 0..3 {
            let (stage, _) = harness.array::<f32>(&[], count);
            let (_, errors) = harness.run(&mut SmoothLattice::new(), &[("levelset", source)],
                &[("smoothed", stage)], &params(&[
                    ("nodes_x", n as f32), ("nodes_y", n as f32), ("nodes_z", n as f32),
                    ("passes", 1.0), ("axis", axis as f32),
                ]));
            assert!(errors.is_empty(), "{errors:?}");
            source = stage;
        }
        let expected_smooth = reference_smooth(&plane.values, [n as usize; 3], 1);
        for (got, want) in read::<f32>(&harness.buffer(source), count).into_iter().zip(expected_smooth) {
            assert!((f64::from(got) - want).abs() < 1e-6);
        }
        let expected = plane.reference_positions();
        let run = run_marching_cubes(&mut harness, &plane, 6 * (n - 1).pow(2), None);
        assert!(run.errors.is_empty(), "{:?}", run.errors);
        assert_eq!(run.total, Some(ParamValue::Float((expected.len() / 3) as f32)));
        for (vertex, want) in run.vertices.iter().zip(expected) {
            assert!((vertex.position[0] - crossing).abs() < 1e-6);
            for (got, reference) in vertex.position.iter().zip(want) {
                assert!((f64::from(*got) - reference).abs() < 1e-6);
            }
        }
    }
}

#[test]
fn fluid_mesh_grid_native_solid_and_clamp_match_engine() {
    use manifold_water_gpu_flip::primitives::liquid_solid_distance::LiquidSolidDistance;
    use manifold_water_liquid::bodies::{LiquidBody, LiquidShape};
    let mut harness = Harness::new();
    for resolution in [8, 16] {
        let layout = manifold_core::fluid_domain::domain_layout(None, 2.0, resolution).unwrap();
        let mesh = manifold_water_liquid::lattice::LiquidLattice::from_layout(&layout).surface();
        let n = mesh.nodes();
        let count = mesh.node_count() as usize;
        let (bodies, _) = harness.array::<LiquidBody>(&[], 1);
        let (shapes, _) = harness.array::<LiquidShape>(&[], 1);
        let (atlas, _) = harness.array::<u32>(&[], 1);
        let (solid, _) = harness.array::<f32>(&[], count);
        let settings = params(&[
            ("lattice_min_x", mesh.min()[0]), ("lattice_min_y", mesh.min()[1]), ("lattice_min_z", mesh.min()[2]),
            ("nodes_x", n[0] as f32), ("nodes_y", n[1] as f32), ("nodes_z", n[2] as f32),
            ("cell_size", mesh.cell_size()),
            ("wall_inset", manifold_water_liquid::lattice::FlipSolverGrid::from_lattice(
                manifold_water_liquid::lattice::LiquidLattice::from_layout(&layout)).wall_inset()),
            ("closed_faces", 63.0),
        ]);
        let (_, errors) = harness.run(&mut LiquidSolidDistance::new(),
            &[("bodies", bodies), ("shapes", shapes), ("atlas", atlas)], &[("solid", solid)], &settings);
        assert!(errors.is_empty(), "{errors:?}");
        let values = read::<f32>(&harness.buffer(solid), count);
        // Native node p = domain_min + (i - 1.5)h; distance to all six walls.
        for (i, &value) in values.iter().enumerate() {
            let q = [i as u32 % n[0], i as u32 / n[0] % n[1], i as u32 / (n[0] * n[1])];
            let distances = q.map(|v| {
                let x = (f64::from(v) - 1.5) * layout.cell_size;
                (x - 5e-5).min(2.0 - 5e-5 - x)
            });
            let outside = distances.map(|d| d.min(0.0).powi(2)).into_iter().sum::<f64>().sqrt();
            let expected = if outside > 0.0 { -outside } else { distances.into_iter().fold(f64::INFINITY, f64::min) };
            assert!((f64::from(value) - expected).abs() < 1e-6);
        }
        let simulation = manifold_water_liquid::lattice::LiquidLattice::from_layout(&layout);
        let (particles, _) = harness.array(&[particle([0.0, 1.0, 0.0], 0.1, 1)], 1);
        let (stats, _) = harness.array(&[0u32; manifold_water_liquid::primitives::liquid_stats::LIQUID_STATS_WORDS as usize], manifold_water_liquid::primitives::liquid_stats::LIQUID_STATS_WORDS as usize);
        // A frame publishes only beside a birth identity: liquid_state's seed
        // for this one particle is next id 2, epoch 0, no reservation, no reset.
        let (identity, _) = harness.array::<u32>(&[2, 0, 0, 0], 4);
        let (published, _) = harness.array::<f32>(&[], 1);
        let grid_nodes: [Slot; 3] = std::array::from_fn(|_| harness.scalar());
        let frame_params = params(&[
            ("lattice_min_x", simulation.min()[0]), ("lattice_min_y", simulation.min()[1]), ("lattice_min_z", simulation.min()[2]),
            ("nodes_x", simulation.nodes()[0] as f32), ("nodes_y", simulation.nodes()[1] as f32), ("nodes_z", simulation.nodes()[2] as f32),
            ("cell_size", simulation.cell_size()), ("count", 1.0), ("simulation_time", 1.0),
        ]);
        let mut frame = super::liquid_frame::LiquidFrame::new();
        frame.prepare_pipelines(&harness.device);
        let (scalars, errors) = harness.run(&mut frame,
            &[("particles", particles), ("stats", stats), ("identity", identity), ("solid", solid)],
            &[("solid_b", published), ("grid_nodes_x", grid_nodes[0]), ("grid_nodes_y", grid_nodes[1]), ("grid_nodes_z", grid_nodes[2])],
            &frame_params);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(harness.buffer(published).size, mesh.solid_bytes());
        assert_eq!(read::<f32>(&harness.buffer(published), count), values);
        for slot in grid_nodes {
            assert!(scalars.iter().any(|(s, value)| *s == slot && *value == ParamValue::Float(n[0] as f32)));
        }
        let (field, _) = harness.array(&vec![-0.25_f32; count], count);
        let (clamped, buffer) = harness.array::<f32>(&[], count);
        let (_, errors) = harness.run(&mut ClampLiquidToSolids::new(),
            &[("levelset", field), ("solid", published)], &[("clamped", clamped)],
            &clamp_params(mesh.bounds().pos, mesh.bounds().scale, n, n, mesh.cell_size()));
        assert!(errors.is_empty(), "{errors:?}");
        for (value, distance) in read::<f32>(&buffer, count).into_iter().zip(values) {
            assert_eq!(value, if distance < 0.0 { 0.0 } else { -0.25 });
        }
    }
}

#[test]
fn volume_surface_mesh_sphere_is_watertight() {
    let mut harness = Harness::new();
    let sphere = SphereLevelSet::new(33, 0.55);
    let run = run_marching_cubes(&mut harness, &sphere, 200_000, None);
    let Some(ParamValue::Float(triangles)) = run.total else { panic!("total") };
    let live = &run.vertices[..triangles as usize * 3];
    // Weld by position: vertices on a shared lattice edge are bit-identical.
    let mut ids = std::collections::HashMap::new();
    let welded: Vec<usize> = live
        .iter()
        .map(|v| {
            let next = ids.len();
            *ids.entry(v.position.map(f32::to_bits)).or_insert(next)
        })
        .collect();
    let mut directed = std::collections::HashMap::new();
    for (t, tri) in welded.chunks_exact(3).enumerate() {
        assert!(tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2], "triangle {t} is degenerate");
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            *directed.entry((a, b)).or_insert(0) += 1;
        }
        let p = [live[3 * t].position, live[3 * t + 1].position, live[3 * t + 2].position];
        let u: [f32; 3] = std::array::from_fn(|i| p[1][i] - p[0][i]);
        let w: [f32; 3] = std::array::from_fn(|i| p[2][i] - p[0][i]);
        let face = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
        let out: f32 = (0..3).map(|i| face[i] * (p[0][i] - sphere.center[i])).sum();
        assert!(out > 0.0, "triangle {t} winds inward");
    }
    for (&(a, b), &count) in &directed {
        assert_eq!(count, 1, "directed edge ({a}, {b}) repeats: inconsistent winding");
        assert_eq!(directed.get(&(b, a)), Some(&1), "edge ({a}, {b}) is a boundary: the mesh is open");
    }
}

/// A surface with no more area than the lattice's box fits the starting
/// buffer whatever Starting Mesh Capacity says: no frame waits on the total.
#[test]
fn volume_surface_mesh_starts_at_the_box_surface() {
    let mut harness = Harness::new();
    let sphere = SphereLevelSet::new(33, 0.55);
    let fixed = run_marching_cubes(&mut harness, &sphere, 200_000, None);
    let Some(ParamValue::Float(triangles)) = fixed.total else { panic!("total") };
    let live = triangles as usize * 3;
    let run = run_marching_cubes(&mut harness, &sphere, 0, None);
    assert!(run.errors.is_empty(), "{:?}", run.errors);
    assert_eq!(run.vertices.len(), 12 * 3 * 32 * 32, "the box surface of 32³ cells");
    assert_eq!(
        bytemuck::cast_slice::<MeshVertex, u8>(&run.vertices[..live]),
        bytemuck::cast_slice::<MeshVertex, u8>(&fixed.vertices[..live]),
        "the first frame is the whole mesh"
    );
}

/// Foam outgrows the starting buffer mid-run: the frame that overflows is an
/// empty mesh, never a truncated one; the next names the overflow with its
/// count and grows; and the grown mesh is the fixed-capacity mesh bit for bit.
#[test]
fn fluid_surface_mesh_grows_past_capacity_mid_run() {
    let mut harness = Harness::new();
    let foam = SphereLevelSet::foam(33, 0xf0a3_5eed);
    let fixed = run_marching_cubes(&mut harness, &foam, 2_000_000, None);
    let Some(ParamValue::Float(triangles)) = fixed.total else { panic!("total") };
    let live = triangles as usize * 3;
    let start = 999;
    assert!(live > start && fixed.vertices.len() >= live, "foam ({live} vertices) outgrows the start ({start})");

    let mut mesh = VolumeSurfaceMesh::new();
    let first = run_marching_cubes_on(&mut harness, &mut mesh, &foam, 999, None);
    assert!(first.errors.is_empty(), "nothing is reported before the total arrives: {:?}", first.errors);
    assert_eq!(first.vertices.len(), start, "an explicit Starting Mesh Capacity wins");
    assert!(first.vertices.iter().all(is_zero), "an overflow is an empty mesh, never a truncated one");

    let grown = run_marching_cubes_on(&mut harness, &mut mesh, &foam, 999, Some(triangles));
    assert!(
        grown.errors.iter().any(|e| e.contains(&format!("Mesh Capacity is {start}")) && e.contains(&live.to_string())),
        "{:?}",
        grown.errors
    );
    assert!(grown.vertices.len() >= 2 * live, "grown to {} for {live} live vertices", grown.vertices.len());
    assert_eq!(
        bytemuck::cast_slice::<MeshVertex, u8>(&grown.vertices[..live]),
        bytemuck::cast_slice::<MeshVertex, u8>(&fixed.vertices[..live]),
        "the grown mesh is the fixed-capacity mesh"
    );
    assert!(grown.vertices[live..].iter().all(is_zero), "the grown tail is zeroed");

    // Steady state: the same surface neither grows nor reports again.
    let steady = run_marching_cubes_on(&mut harness, &mut mesh, &foam, 999, Some(triangles));
    assert!(steady.errors.is_empty(), "{:?}", steady.errors);
    assert_eq!(steady.vertices.len(), grown.vertices.len(), "no allocation once the surface fits");
}

/// The compact edge-owned mesh expands to the existing triangle list exactly:
/// same complete records, table order and winding, with one vertex per shared
/// lattice edge. Sphere, random foam, and a boundary-only crossing cover the
/// ordinary, high-sharing, and clamped-edge cases at both small grids.
#[test]
fn fluid_indexed_surface_mesh_matches_triangle_list_on_spheres_foam_and_boundary() {
    for (nodes, level_set, radial_winding) in [
        (8, SphereLevelSet::new(8, 0.7), true),
        (16, SphereLevelSet::new(16, 0.7), true),
        (8, SphereLevelSet::foam(8, 0xf0a3_5eed), false),
        (16, SphereLevelSet::foam(16, 0x5eed_cafe), false),
        (8, SphereLevelSet::boundary(8), false),
        (16, SphereLevelSet::boundary(16), false),
    ] {
        let mut harness = Harness::new();
        let capacity = 200_000;
        let direct = run_marching_cubes(&mut harness, &level_set, capacity, None);
        let indexed = run_indexed_marching_cubes_on(
            &mut harness,
            &mut VolumeSurfaceMesh::new(),
            &level_set,
            capacity,
            None,
            true,
        );
        let triangle_vertices = assert_indexed_expansion_matches(&level_set, &direct, &indexed, radial_winding);
        let extent_slot = indexed.extent_slot.expect("indexed live extent");
        let extent_buffer = harness.buffer(extent_slot);
        assert!(
            harness
                .live_extents
                .iter()
                .any(|(slot, extent)| *slot == indexed.indices_slot && extent.counts.ptr_eq(&extent_buffer) && extent.per_item == 3),
            "triangle extent is published on indices"
        );
        assert!(
            harness
                .live_extents
                .iter()
                .any(|(slot, extent)| *slot == indexed.vertices_slot && extent.per_item == 1),
            "edge extent is published on compact vertices"
        );
        assert!(triangle_vertices > nodes as usize, "{nodes}³ fixture produced too little surface");
    }
}

/// A first frame whose triangle total exceeds the supplied starting capacity
/// clears both owned indexed outputs. The same path can then grow on a later
/// frame; this assertion protects the no-truncated-mesh contract.
#[test]
fn fluid_indexed_surface_mesh_overflow_is_empty_and_tails_are_zero() {
    let mut harness = Harness::new();
    let foam = SphereLevelSet::foam(16, 0xdead_beef);
    let run = run_indexed_marching_cubes_on(
        &mut harness,
        &mut VolumeSurfaceMesh::new(),
        &foam,
        3,
        None,
        false,
    );
    assert!(run.errors.is_empty(), "first frame has no late total: {:?}", run.errors);
    assert!(run.vertices.iter().all(is_zero), "overflowed indexed vertices must be empty");
    assert!(run.indices.iter().all(|&index| index == 0), "overflowed indices must be empty");
}

// --- P6b: live triangles only ---------------------------------------------

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
        assert_eq!(read::<u32>(&extent, 4), vec![items, groups, 1, 1], "{items} live items");
    }
}

/// One frame of count → running total → mesh with `extent` wired; returns the
/// live vertex count.
struct LiveMesh {
    count: CountSurfaceTriangles,
    running: RunningTotal,
    mesh: VolumeSurfaceMesh,
    counts: Slot,
    scan: Slot,
    total: Slot,
    extent: (Slot, GpuBuffer),
    vertices: (Slot, GpuBuffer),
    /// The level set the last frame meshed.
    levelset: Option<Slot>,
}

impl LiveMesh {
    fn new(harness: &mut Harness, nodes: usize, capacity: usize) -> Self {
        let counts = harness.array::<u32>(&[], nodes).0;
        let scan = harness.array::<u32>(&[], nodes).0;
        let total = harness.scalar();
        Self {
            count: CountSurfaceTriangles::new(),
            running: RunningTotal::new(),
            mesh: VolumeSurfaceMesh::new(),
            counts,
            scan,
            total,
            extent: harness.array::<u32>(&[], 4),
            vertices: harness.array::<MeshVertex>(&[], capacity),
            levelset: None,
        }
    }

    fn frame(&mut self, harness: &mut Harness, sphere: &SphereLevelSet, capacity: usize) -> usize {
        let (levelset, _) = harness.array(&sphere.values, sphere.values.len());
        let n = sphere.nodes as f32;
        let lattice = [("nodes_x", n), ("nodes_y", n), ("nodes_z", n)];
        harness.run(&mut self.count, &[("levelset", levelset)], &[("counts", self.counts)], &params(&lattice));
        harness.run(
            &mut self.running,
            &[("in", self.counts)],
            &[("out", self.scan), ("total", self.total), ("extent", self.extent.0)],
            &params(&[("per_item", 3.0)]),
        );
        let centre = sphere.min + 0.5 * sphere.size;
        let mut mesh_params = lattice.to_vec();
        mesh_params.extend([
            ("center_x", centre),
            ("center_y", centre),
            ("center_z", centre),
            ("size_x", sphere.size),
            ("size_y", sphere.size),
            ("size_z", sphere.size),
            ("resolution_scale", 1.0),
            ("max_capacity", capacity as f32),
        ]);
        let (_, errors) = harness.run(
            &mut self.mesh,
            &[("levelset", levelset), ("scan", self.scan), ("extent", self.extent.0)],
            &[("vertices", self.vertices.0)],
            &params(&mesh_params),
        );
        assert!(errors.is_empty(), "{errors:?}");
        // The executor hands the published extent to the mesh's consumers.
        if let Some((slot, extent)) = harness.live_extents.first().cloned() {
            Backend::set_live_extent(&mut harness.backend, slot, extent);
        }
        self.levelset = Some(levelset);
        read::<u32>(&self.extent.1, 1)[0] as usize * 3
    }
}

/// With `extent` wired the emit covers only this frame's and last frame's
/// vertices: a sentinel past that range survives, a shrinking surface clears
/// what it vacates, and the live extent is published with a warm-up bound.
#[test]
fn fluid_volume_surface_mesh_writes_only_live_and_last_frame_vertices() {
    let mut harness = Harness::new();
    let big = SphereLevelSet::new(33, 0.7);
    let small = SphereLevelSet::new(33, 0.35);
    let capacity = 120_000;
    let mut live = LiveMesh::new(&mut harness, big.values.len(), capacity);
    let first = live.frame(&mut harness, &big, capacity);
    let (slot, extent) = harness.live_extents.first().expect("the mesh publishes its live extent");
    assert_eq!(*slot, live.vertices.0);
    assert_eq!((extent.offset, extent.per_item, extent.bound), (0, 3, capacity as u32), "warm-up bound is the capacity");
    assert!(extent.counts.ptr_eq(&live.extent.1), "the count is the running total's extent");

    let sentinel_from = first.div_ceil(256) * 256 + 512;
    let sentinel = MeshVertex { position: [9.0; 3], _pad0: 0.0, normal: [1.0, 0.0, 0.0], _pad1: 0.0, uv: [0.0; 2], _pad2: [0.0; 2], tangent: [0.0; 4], color: [1.0; 4] };
    let tail = vec![sentinel; capacity - sentinel_from];
    // SAFETY: shared buffer, no GPU work in flight between harness runs.
    unsafe { harness.buffer(live.vertices.0).write((sentinel_from * std::mem::size_of::<MeshVertex>()) as u64, bytemuck::cast_slice(&tail)) };

    assert_eq!(live.frame(&mut harness, &big, capacity), first);
    let shrunk = live.frame(&mut harness, &small, capacity);
    assert!(shrunk < first, "the small sphere has fewer vertices ({shrunk} of {first})");
    live.frame(&mut harness, &small, capacity);
    let vertices: Vec<MeshVertex> = read(&harness.buffer(live.vertices.0), capacity);
    assert!(vertices[..shrunk].iter().all(|v| v.normal != [0.0; 3]), "live vertices are written");
    assert!(vertices[shrunk..sentinel_from].iter().all(is_zero), "vacated slots are cleared");
    assert!(
        vertices[sentinel_from..].iter().all(|v| v.position == [9.0; 3]),
        "slots past last frame's extent are never touched"
    );
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
                            let w = row[(dz + p) as usize] * row[(dy + p) as usize] * row[(dx + p) as usize];
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
    let values: Vec<f32> = (0..total + 20).map(|_| rng.next_f32() * 2.0 - 1.0).collect();
    let (input, _) = harness.array(&values, values.len());
    let stages: Vec<(Slot, GpuBuffer)> = (0..3).map(|_| harness.array::<f32>(&[], values.len())).collect();
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
            let (_, errors) = harness.run(&mut SmoothLattice::new(), &[("levelset", source)], &[("smoothed", *stage)], &params(&lattice));
            assert!(errors.is_empty(), "{errors:?}");
            source = *stage;
        }
        let actual: Vec<f32> = read(&out, values.len());
        let expected: Vec<f64> = if passes == 0 {
            values[..total].iter().map(|&v| f64::from(v)).collect()
        } else {
            reference_smooth(&values, nodes, passes)
        };
        let worst = actual[..total].iter().zip(&expected).map(|(a, e)| (f64::from(*a) - e).abs()).fold(0.0, f64::max);
        assert!(worst < 1e-5, "{passes} passes: worst difference {worst}");
        assert_eq!(&actual[total..], &values[total..], "{passes} passes: values past the lattice pass through");
    }
    let (_, errors) = harness.run(&mut SmoothLattice::new(), &[("levelset", input)], &[("smoothed", out_slot)], &params(&[("nodes_x", 0.0), ("passes", 2.0)]));
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(read::<f32>(&out, values.len()), values, "no lattice copies the input");
}

// --- BUG-koy0 (solid clamp before smoothing): the clamp after smoothing ---

use manifold_water_gpu_flip::primitives::clamp_liquid_to_solids::ClampLiquidToSolids;





#[test]
fn fluid_clamp_liquid_to_solids_matches_reference_and_passes_through() {
    let mut harness = Harness::new();
    let (center, size, cell) = ([0.25_f32, 1.0, -0.5], [2.0_f32, 1.5, 2.5], 0.25_f32);
    let solid_nodes = [6u32, 5, 7];
    let nodes = solid_nodes.map(|n| (n - 1) * 3 + 1);
    let total = nodes.iter().product::<u32>() as usize;
    let min: [f32; 3] = std::array::from_fn(|a| center[a] - 0.5 * size[a]);
    let mut rng = Rng::new(0xc1a3_9e11);
    let solid: Vec<f32> = (0..solid_nodes.iter().product::<u32>()).map(|_| rng.next_f32() * 2.0 - 0.8).collect();
    let values: Vec<f32> = (0..total + 20).map(|_| rng.next_f32() * 2.0 - 1.0).collect();
    let (solid_slot, _) = harness.array(&solid, solid.len());
    let (levelset_slot, _) = harness.array(&values, values.len());
    let (clamped_slot, clamped_buf) = harness.array::<f32>(&[], values.len());
    let run = |harness: &mut Harness, lattice: [u32; 3]| {
        harness.run(
            &mut ClampLiquidToSolids::new(),
            &[("levelset", levelset_slot), ("solid", solid_slot)],
            &[("clamped", clamped_slot)],
            &clamp_params(center, size, lattice, solid_nodes, cell),
        )
    };
    let (_, errors) = run(&mut harness, nodes);
    assert!(errors.is_empty(), "{errors:?}");
    let clamped: Vec<f32> = read(&clamped_buf, values.len());
    let h: [f64; 3] = std::array::from_fn(|a| f64::from(size[a]) / f64::from(nodes[a] - 1));
    let (mut border, mut raised, mut kept, mut ambiguous) = (0, 0, 0, 0);
    for (idx, (&value, &out)) in values[..total].iter().zip(&clamped).enumerate() {
        let ijk = [idx as u32 % nodes[0], (idx as u32 / nodes[0]) % nodes[1], idx as u32 / (nodes[0] * nodes[1])];
        if (0..3).any(|a| ijk[a] == 0 || ijk[a] == nodes[a] - 1) {
            border += 1;
        }
        let p: [f64; 3] = std::array::from_fn(|a| f64::from(min[a]) + f64::from(ijk[a]) * h[a]);
        let s = solid_sample(&solid, solid_nodes, min, size, p);
        // f32 on the GPU, f64 here: the sign of a sample this close to 0 is not the rule's to settle.
        if s.abs() < 1e-4 {
            ambiguous += 1;
            continue;
        }
        // ParticleMesher::_computeScalarField negates its distance, then
        // ScalarField::getScalarFieldValue clips positive-inside solid values.
        // Convert back to this graph's negative-inside convention; canonicalize
        // threshold zero because the GPU max returns positive zero.
        let mut native = -f64::from(value);
        if s < 0.0 && native > 0.0 {
            native = 0.0;
        }
        let expected = if native == 0.0 { 0.0 } else { -native as f32 };
        if s < 0.0 && value < 0.0 {
            raised += 1;
        } else if s > 0.0 {
            kept += 1;
        }
        assert_eq!(out.to_bits(), expected.to_bits(), "node {ijk:?}: solid {s}, in {value}, out {out}");
    }
    assert!(border > 100 && raised > 100 && kept > 100, "border {border}, raised {raised}, kept {kept}");
    assert!(ambiguous < 20, "{ambiguous} nodes sit on the solid boundary");
    assert_eq!(&clamped[total..], &values[total..], "values past the lattice pass through");

    let (_, errors) = run(&mut harness, [0, nodes[1], nodes[2]]);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(read::<f32>(&clamped_buf, values.len()), values, "no lattice copies the input");

    let (_, errors) = run(&mut harness, [nodes[0] * 2, nodes[1], nodes[2]]);
    assert!(errors.iter().any(|e| e.contains("larger than its level set")), "{errors:?}");
}





// --- Mesh relaxation (BUG-xwf1 (Liquid Surface mesh relaxation)) ----------

use super::relax_surface_mesh::RelaxSurfaceMesh;

/// One relax pass of `mesh`'s last frame from `input` into `output`.
fn relax_pass(
    harness: &mut Harness,
    relax: &mut RelaxSurfaceMesh,
    mesh: &LiveMesh,
    nodes: u32,
    input: Slot,
    output: Slot,
    strength: f32,
) {
    let n = nodes as f32;
    let levelset = mesh.levelset.expect("a meshed frame");
    let (_, errors) = harness.run(
        relax,
        &[("vertices", input), ("levelset", levelset), ("scan", mesh.scan), ("extent", mesh.extent.0)],
        &[("relaxed", output)],
        &params(&[("nodes_x", n), ("nodes_y", n), ("nodes_z", n), ("strength", strength)]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    if let Some((slot, extent)) = harness.live_extents.first().cloned() {
        Backend::set_live_extent(&mut harness.backend, slot, extent);
    }
}

/// Relax a mesh without a live extent. The optional edge scan switches the
/// same shader from triangle-list slots to compact shared-edge slots.
fn relax_pass_slots(
    harness: &mut Harness,
    relax: &mut RelaxSurfaceMesh,
    levelset: Slot,
    scan: Slot,
    edge_scan: Option<Slot>,
    nodes: u32,
    input: Slot,
    output: Slot,
    strength: f32,
) {
    let mut inputs = vec![("vertices", input), ("levelset", levelset), ("scan", scan)];
    if let Some(edge_scan) = edge_scan {
        inputs.push(("edge_scan", edge_scan));
    }
    let n = nodes as f32;
    let (_, errors) = harness.run(
        relax,
        &inputs,
        &[("relaxed", output)],
        &params(&[("nodes_x", n), ("nodes_y", n), ("nodes_z", n), ("strength", strength)]),
    );
    assert!(errors.is_empty(), "{errors:?}");
}

/// Welded vertex ids of a closed triangle list (shared vertices are
/// bit-identical) and each welded vertex's distinct neighbours.
fn weld(live: &[MeshVertex]) -> (Vec<usize>, Vec<Vec<usize>>) {
    let mut ids = std::collections::HashMap::new();
    let welded: Vec<usize> = live
        .iter()
        .map(|v| {
            let next = ids.len();
            *ids.entry(v.position.map(f32::to_bits)).or_insert(next)
        })
        .collect();
    let mut neighbours = vec![std::collections::BTreeSet::new(); ids.len()];
    for tri in welded.chunks_exact(3) {
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            neighbours[a].insert(b);
            neighbours[b].insert(a);
        }
    }
    (welded, neighbours.into_iter().map(|set| set.into_iter().collect()).collect())
}

/// f64 umbrella pass over welded positions: p + strength × (mean − p).
fn umbrella(points: &[[f64; 3]], neighbours: &[Vec<usize>], strength: f64) -> Vec<[f64; 3]> {
    points
        .iter()
        .zip(neighbours)
        .map(|(p, around)| {
            let n = around.len() as f64;
            std::array::from_fn(|a| {
                let mean = around.iter().map(|&j| points[j][a]).sum::<f64>() / n;
                p[a] + strength * (mean - p[a])
            })
        })
        .collect()
}

/// Mean angle (radians) between the faces on either side of each edge.
fn mean_dihedral(live: &[MeshVertex], welded: &[usize]) -> f64 {
    let face = |t: usize| {
        let p = [live[3 * t].position, live[3 * t + 1].position, live[3 * t + 2].position];
        let u: [f64; 3] = std::array::from_fn(|i| f64::from(p[1][i] - p[0][i]));
        let w: [f64; 3] = std::array::from_fn(|i| f64::from(p[2][i] - p[0][i]));
        let n = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        n.map(|c| c / len)
    };
    let mut by_edge = std::collections::HashMap::new();
    for (t, tri) in welded.chunks_exact(3).enumerate() {
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            by_edge.entry((a.min(b), a.max(b))).or_insert_with(Vec::new).push(t);
        }
    }
    let angles: Vec<f64> = by_edge
        .values()
        .map(|faces| {
            let (f, g) = (face(faces[0]), face(faces[1]));
            (f[0] * g[0] + f[1] * g[1] + f[2] * g[2]).clamp(-1.0, 1.0).acos()
        })
        .collect();
    angles.iter().sum::<f64>() / angles.len() as f64
}

/// Two chained passes match an f64 umbrella reference over the welded mesh,
/// keep shared vertices bit-identical (the mesh stays closed), leave normals
/// alone, zero the tail, and flatten the facets of a bumpy sphere. Strength 0
/// copies the input.
#[test]
fn fluid_relax_surface_mesh_matches_umbrella_reference_on_a_bumpy_sphere() {
    let mut harness = Harness::new();
    let mut sphere = SphereLevelSet::new(33, 0.55);
    // A quarter-cell of noise: the lumpy surface relaxation is for.
    let mut rng = Rng::new(0x9e37_79b9_7f4a_7c15);
    let cell = sphere.size / (sphere.nodes - 1) as f32;
    for value in &mut sphere.values {
        *value += 0.25 * cell * (rng.next_f32() - 0.5);
    }
    let capacity = 120_000;
    let mut mesh = LiveMesh::new(&mut harness, sphere.values.len(), capacity);
    let live_count = mesh.frame(&mut harness, &sphere, capacity);
    let raw: Vec<MeshVertex> = read(&harness.buffer(mesh.vertices.0), capacity);
    let (welded, neighbours) = weld(&raw[..live_count]);
    let mut points = vec![[0.0_f64; 3]; neighbours.len()];
    for (slot, &w) in welded.iter().enumerate() {
        points[w] = raw[slot].position.map(f64::from);
    }

    let strength = 0.5;
    let (first_slot, first_buf) = harness.array::<MeshVertex>(&[], capacity);
    let (second_slot, second_buf) = harness.array::<MeshVertex>(&[], capacity);
    let input = mesh.vertices.0;
    relax_pass(&mut harness, &mut RelaxSurfaceMesh::new(), &mesh, sphere.nodes, input, first_slot, strength);
    relax_pass(&mut harness, &mut RelaxSurfaceMesh::new(), &mesh, sphere.nodes, first_slot, second_slot, strength);
    let first: Vec<MeshVertex> = read(&first_buf, capacity);
    let second: Vec<MeshVertex> = read(&second_buf, capacity);

    let reference_first = umbrella(&points, &neighbours, f64::from(strength));
    let reference_second = umbrella(&reference_first, &neighbours, f64::from(strength));
    for (pass, (got, want)) in [(&first, &reference_first), (&second, &reference_second)].into_iter().enumerate() {
        let mut worst = 0.0_f64;
        let mut copies: Vec<Option<[u32; 3]>> = vec![None; neighbours.len()];
        for (slot, &w) in welded.iter().enumerate() {
            let v = &got[slot];
            for (&got, &want) in v.position.iter().zip(&want[w]) {
                worst = worst.max((f64::from(got) - want).abs());
            }
            let bits = v.position.map(f32::to_bits);
            assert_eq!(*copies[w].get_or_insert(bits), bits, "pass {pass}: copies of welded vertex {w} differ");
            assert_eq!(v.normal, raw[slot].normal, "pass {pass}: normals pass through");
            assert_eq!(v.uv, raw[slot].uv);
            assert_eq!(v.color, raw[slot].color);
        }
        assert!(worst < 1e-5, "pass {pass}: positions differ from the f64 umbrella by {worst} m");
        assert!(got[live_count..].iter().all(is_zero), "pass {pass}: the tail past the live triangles is zeroed");
    }

    let before = mean_dihedral(&raw[..live_count], &welded);
    let after = mean_dihedral(&second[..live_count], &welded);
    assert!(after < before, "relaxation must flatten the facets: {before} → {after} rad");

    let (still_slot, still_buf) = harness.array::<MeshVertex>(&[], capacity);
    relax_pass(&mut harness, &mut RelaxSurfaceMesh::new(), &mesh, sphere.nodes, input, still_slot, 0.0);
    let still: Vec<MeshVertex> = read(&still_buf, capacity);
    assert!(
        bytemuck::cast_slice::<MeshVertex, u8>(&still) == bytemuck::cast_slice::<MeshVertex, u8>(&raw),
        "strength 0 copies the mesh bit for bit"
    );
}

/// Cell-owned relaxation reads the same neighbours in the indexed and
/// triangle-list layouts. Two sequential passes at strength 0.5 and 0 are
/// compared record-for-record after expanding the compact indices.
#[test]
fn fluid_indexed_relaxation_matches_triangle_list_for_two_passes() {
    let mut harness = Harness::new();
    let sphere = SphereLevelSet::new(16, 0.7);
    let capacity = 200_000;
    let direct = run_marching_cubes(&mut harness, &sphere, capacity, None);
    let indexed = run_indexed_marching_cubes_on(
        &mut harness,
        &mut VolumeSurfaceMesh::new(),
        &sphere,
        capacity,
        None,
        false,
    );
    let triangle_vertices = assert_indexed_expansion_matches(&sphere, &direct, &indexed, true);

    for strength in [0.5, 0.0] {
        let mut direct_input = direct.vertices_slot;
        let mut indexed_input = indexed.vertices_slot;
        for pass in 0..2 {
            let (direct_output, direct_buffer) = harness.array::<MeshVertex>(&[], direct.vertices.len());
            let (indexed_output, indexed_buffer) = harness.array::<MeshVertex>(&[], indexed.vertices.len());
            relax_pass_slots(
                &mut harness,
                &mut RelaxSurfaceMesh::new(),
                direct.levelset,
                direct.scan,
                None,
                sphere.nodes,
                direct_input,
                direct_output,
                strength,
            );
            relax_pass_slots(
                &mut harness,
                &mut RelaxSurfaceMesh::new(),
                indexed.levelset,
                indexed.scan,
                Some(indexed.edge_scan),
                sphere.nodes,
                indexed_input,
                indexed_output,
                strength,
            );
            let direct_values: Vec<MeshVertex> = read(&direct_buffer, direct.vertices.len());
            let indexed_values: Vec<MeshVertex> = read(&indexed_buffer, indexed.vertices.len());
            let unique_live = indexed.indices[..triangle_vertices]
                .iter()
                .copied()
                .max()
                .map_or(0, |index| index as usize + 1);
            let expanded: Vec<MeshVertex> = indexed.indices[..triangle_vertices]
                .iter()
                .map(|&index| indexed_values[index as usize])
                .collect();
            assert_eq!(
                bytemuck::cast_slice::<MeshVertex, u8>(&expanded),
                bytemuck::cast_slice::<MeshVertex, u8>(&direct_values[..triangle_vertices]),
                "strength {strength}, pass {pass}: indexed relaxation differs after expansion"
            );
            assert!(direct_values[triangle_vertices..].iter().all(is_zero), "direct pass {pass} tail is not zero");
            assert!(indexed_values[unique_live..].iter().all(is_zero), "indexed pass {pass} tail is not zero");
            direct_input = direct_output;
            indexed_input = indexed_output;
        }
    }
}

/// With `extent` wired a relax pass forwards the mesh's live extent and,
/// after its first frame, writes only live and last frame's vertices: a
/// shrinking surface clears what it vacates and matches a fresh full pass.
#[test]
fn fluid_relax_surface_mesh_follows_the_live_extent() {
    let mut harness = Harness::new();
    let big = SphereLevelSet::new(33, 0.7);
    let small = SphereLevelSet::new(33, 0.35);
    let capacity = 120_000;
    let mut mesh = LiveMesh::new(&mut harness, big.values.len(), capacity);
    let mut relax = RelaxSurfaceMesh::new();
    let (relaxed_slot, relaxed_buf) = harness.array::<MeshVertex>(&[], capacity);
    let input = mesh.vertices.0;

    let first = mesh.frame(&mut harness, &big, capacity);
    relax_pass(&mut harness, &mut relax, &mesh, big.nodes, input, relaxed_slot, 0.5);
    let (slot, extent) = harness.live_extents.first().cloned().expect("the relax pass forwards the live extent");
    assert_eq!(slot, relaxed_slot);
    assert!(extent.counts.ptr_eq(&mesh.extent.1), "the count is the running total's extent");
    assert_eq!((extent.offset, extent.per_item), (0, 3));

    let shrunk = mesh.frame(&mut harness, &small, capacity);
    relax_pass(&mut harness, &mut relax, &mesh, small.nodes, input, relaxed_slot, 0.5);
    assert!(shrunk < first, "the small sphere has fewer vertices ({shrunk} of {first})");
    let relaxed: Vec<MeshVertex> = read(&relaxed_buf, capacity);
    assert!(relaxed[shrunk..first].iter().all(is_zero), "vacated slots are cleared");

    let (fresh_slot, fresh_buf) = harness.array::<MeshVertex>(&[], capacity);
    relax_pass(&mut harness, &mut RelaxSurfaceMesh::new(), &mesh, small.nodes, input, fresh_slot, 0.5);
    let fresh: Vec<MeshVertex> = read(&fresh_buf, capacity);
    assert!(
        bytemuck::cast_slice::<MeshVertex, u8>(&relaxed[..shrunk]) == bytemuck::cast_slice::<MeshVertex, u8>(&fresh[..shrunk]),
        "the indirect pass relaxes the live vertices as a full pass does"
    );
}

//! The GPU FLIP step's tile table (docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md
//! section 3 (The tile table)) against a CPU model: the kernels run on
//! their own over placed particles, every word compared; the allocator's
//! slots against the rings; then the step under the NaN poison (section 4
//! (The defined-value rule)).

use manifold_gpu::GpuBuffer;

use crate::contracts::water::primitives::gpu_flip_scene_tests::Run;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::testkit::array_harness::read;
use manifold_nodes_water::presets::gpu_flip::WaterScene;
use manifold_water_gpu_flip::primitives::gpu_flip_step::{
    CELL_REACH, ENGINE_CFL, FACE_VALID_LAYERS, StepParams, band_layers, dispatch_pass, ring_max,
    set_poison,
};
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::tile_storage::{TILE, tile_counts, tile_total};

/// The tile table's words as the CPU builds them for one step.
#[derive(Debug, PartialEq, Eq)]
struct Model {
    near: Vec<u32>,
    rank: Vec<u32>,
    by_ring: Vec<u32>,
    counts: Vec<u32>,
    args: Vec<u32>,
    retired: Vec<u32>,
}

fn argument_words(r: usize) -> usize {
    let triples = 3 * (r + 2);
    triples + triples % 2 + 2
}

/// `counts[c]` live particles at the centre of each cell of `n`, at cell
/// size 1 from the origin.
fn particles_of(counts: &[u32], n: [u32; 3]) -> Vec<FluidParticle> {
    let mut particles = Vec::new();
    for (i, &count) in counts.iter().enumerate() {
        let i = i as u32;
        let c = [i % n[0], (i / n[0]) % n[1], i / (n[0] * n[1])];
        for _ in 0..count {
            particles.push(FluidParticle {
                position_radius: [c[0] as f32 + 0.5, c[1] as f32 + 0.5, c[2] as f32 + 0.5, 0.1],
                velocity: [0.0; 3],
                id: particles.len() as u32 + 1,
            });
        }
    }
    particles
}

fn unflatten(t: u32, m: [u32; 3]) -> [i64; 3] {
    [
        i64::from(t % m[0]),
        i64::from((t / m[0]) % m[1]),
        i64::from(t / (m[0] * m[1])),
    ]
}

fn chebyshev(d: [i64; 3]) -> u32 {
    d.iter().map(|v| v.abs()).max().expect("three axes") as u32
}

fn rank(ring: u32, near: u32) -> u32 {
    if ring <= 1 {
        u32::from(near > CELL_REACH)
    } else {
        ring
    }
}

/// The CPU model of one step: `prev_rank` is the previous step's ranks
/// (zero before the first), `all` the oracle lever.
fn model(counts: &[u32], n: [u32; 3], r: u32, prev_rank: &[u32], all: bool) -> Model {
    let dims = tile_counts(n);
    let total = tile_total(n) as usize;
    let mut near = vec![CELL_REACH + 1; total];
    for (t, near) in near.iter_mut().enumerate() {
        if all {
            *near = 0;
            continue;
        }
        let origin = unflatten(t as u32, dims).map(|v| v * i64::from(TILE));
        let last: [i64; 3] =
            std::array::from_fn(|a| (origin[a] + i64::from(TILE) - 1).min(i64::from(n[a]) - 1));
        let reach = i64::from(CELL_REACH);
        for z in (origin[2] - reach).max(0)..=(last[2] + reach).min(i64::from(n[2]) - 1) {
            for y in (origin[1] - reach).max(0)..=(last[1] + reach).min(i64::from(n[1]) - 1) {
                for x in (origin[0] - reach).max(0)..=(last[0] + reach).min(i64::from(n[0]) - 1) {
                    if counts[(x + i64::from(n[0]) * (y + i64::from(n[1]) * z)) as usize] == 0 {
                        continue;
                    }
                    let c = [x, y, z];
                    let d: [i64; 3] =
                        std::array::from_fn(|a| (origin[a] - c[a]).max(c[a] - last[a]).max(0));
                    *near = (*near).min(chebyshev(d));
                }
            }
        }
    }
    let mut ring = vec![r + 1; total];
    for (t, ring) in ring.iter_mut().enumerate() {
        if all {
            *ring = 0;
            continue;
        }
        let p = unflatten(t as u32, dims);
        let rr = i64::from(r);
        for z in (p[2] - rr).max(0)..=(p[2] + rr).min(i64::from(dims[2]) - 1) {
            for y in (p[1] - rr).max(0)..=(p[1] + rr).min(i64::from(dims[1]) - 1) {
                for x in (p[0] - rr).max(0)..=(p[0] + rr).min(i64::from(dims[0]) - 1) {
                    let q = (x + i64::from(dims[0]) * (y + i64::from(dims[1]) * z)) as usize;
                    if near[q] == 0 {
                        *ring = (*ring).min(chebyshev([x - p[0], y - p[1], z - p[2]]));
                    }
                }
            }
        }
    }
    let ranks: Vec<u32> = (0..total).map(|t| rank(ring[t], near[t])).collect();
    let mut by_ring = Vec::with_capacity(total);
    let mut counts_out = Vec::with_capacity(r as usize + 4);
    for k in 0..=r + 1 {
        by_ring.extend((0..total as u32).filter(|&t| ranks[t as usize] == k));
        counts_out.push(by_ring.len() as u32);
    }
    let retired: Vec<u32> = (0..total as u32)
        .filter(|&t| prev_rank[t as usize] == 0 && ranks[t as usize] != 0)
        .collect();
    counts_out.push(retired.len() as u32);
    let mut args = Vec::new();
    for &count in &counts_out[..=r as usize] {
        args.extend([2 * count, 1, 1]);
    }
    args.extend([2 * retired.len() as u32, 1, 1]);
    args.resize(argument_words(r as usize) - 2, 0);
    args.extend([0, band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS)]);
    Model {
        near,
        rank: ranks,
        by_ring,
        counts: counts_out,
        args,
        retired,
    }
}

/// The tile buffers on the GPU, shared so the test reads them back.
struct Table {
    near: GpuBuffer,
    rank: GpuBuffer,
    by_ring: GpuBuffer,
    counts: GpuBuffer,
    args: GpuBuffer,
    retired: GpuBuffer,
    capped: GpuBuffer,
    /// `slot_of` per tile then `tile_of` per slot, as the step builds it:
    /// every tile absent.
    table: GpuBuffer,
    /// The free stack: every slot, the lowest on top.
    free_list: GpuBuffer,
    fresh: GpuBuffer,
    n: [u32; 3],
    total: usize,
    pool: usize,
    r: u32,
}

/// The pool's bookkeeping read back after an allocation.
#[derive(Debug, PartialEq, Eq)]
struct Held {
    slot_of: Vec<u32>,
    tile_of: Vec<u32>,
    free: Vec<u32>,
    fresh: Vec<u32>,
    exhausted: u32,
}

const NONE: u32 = u32::MAX;

impl Table {
    fn new(device: &manifold_gpu::GpuDevice, n: [u32; 3], r: u32) -> Self {
        Self::with_pool(device, n, r, tile_total(n) as usize)
    }

    /// A table whose pool holds `pool` slots, all free.
    fn with_pool(device: &manifold_gpu::GpuDevice, n: [u32; 3], r: u32, pool: usize) -> Self {
        let total = tile_total(n) as usize;
        let words = |w: usize| {
            let b = device.create_buffer_shared((w.max(1) * 4) as u64);
            b.zero_fill();
            b
        };
        let counts = words(r as usize + 7);
        let free_list = words(pool);
        let table = words(total + pool);
        let free: Vec<u32> = (0..pool as u32).rev().collect();
        // SAFETY: shared buffers sized for these words; no GPU work has
        // touched them yet.
        unsafe {
            table.write(0, bytemuck::cast_slice(&vec![NONE; total + pool]));
            free_list.write(0, bytemuck::cast_slice(&free));
            counts.write(
                (r as usize + 4) as u64 * 4,
                bytemuck::bytes_of(&(pool as u32)),
            );
        }
        Self {
            near: words(total),
            rank: words(2 * total),
            by_ring: words(total),
            counts,
            args: words(argument_words(r as usize) + 3),
            retired: words(total),
            capped: words(7),
            table,
            free_list,
            fresh: words(total),
            n,
            total,
            pool,
            r,
        }
    }

    /// One step's table kernels over synthetic particles: `counts[c]` live
    /// particles at the centre of cell `c` of the lattice `n` (cell size 1,
    /// corner at the origin: `params` is built that way).
    fn step(&self, device: &manifold_gpu::GpuDevice, params: &StepParams, counts: &[u32]) {
        self.step_with_clock(device, params, counts, None);
    }

    fn step_with_clock(
        &self,
        device: &manifold_gpu::GpuDevice,
        params: &StepParams,
        counts: &[u32],
        clock: Option<&GpuBuffer>,
    ) {
        self.step_particles(device, params, &particles_of(counts, self.n), clock);
    }

    /// One step's table kernels over `particles`: clear, mark, rings, lists.
    fn step_particles(
        &self,
        device: &manifold_gpu::GpuDevice,
        params: &StepParams,
        particles: &[FluidParticle],
        clock: Option<&GpuBuffer>,
    ) {
        let slots = device.create_buffer_shared((particles.len().max(1) * 32) as u64);
        slots.zero_fill();
        // SAFETY: a shared buffer sized for the particles; no GPU work has
        // touched it yet.
        unsafe { slots.write(0, bytemuck::cast_slice(particles)) };
        let params = params.with_capacity_for_test(particles.len() as u32);
        let threads = self.total as u64;
        let pass = |entry: &str, buffers: &[(u32, &GpuBuffer)], threads| {
            let mut buffers = buffers.to_vec();
            if let Some(clock) = clock {
                buffers.push((46, clock));
            }
            dispatch_pass(device, entry, &params, &buffers, threads);
        };
        pass(
            "tiles_clear",
            &[(27, &self.near), (30, &self.counts)],
            threads,
        );
        pass(
            "tiles_mark",
            &[(2, &slots), (27, &self.near)],
            particles.len() as u64,
        );
        pass(
            "tiles_rings",
            &[(27, &self.near), (28, &self.rank), (30, &self.counts)],
            threads,
        );
        pass(
            "tiles_lists",
            &[
                (27, &self.near),
                (28, &self.rank),
                (29, &self.by_ring),
                (30, &self.counts),
                (31, &self.args),
                (32, &self.retired),
                (22, &self.capped),
            ],
            1,
        );
    }

    /// The allocator after `step`'s marks and rings.
    fn allocate(&self, device: &manifold_gpu::GpuDevice, params: &StepParams) {
        dispatch_pass(
            device,
            "tiles_allocate",
            params,
            &[
                (28, &self.rank),
                (30, &self.counts),
                (31, &self.args),
                (52, &self.free_list),
                (53, &self.fresh),
                (54, &self.table),
            ],
            1,
        );
    }

    fn held(&self) -> Held {
        let r = self.r as usize;
        let counts = read::<u32>(&self.counts, r + 7);
        let words = read::<u32>(&self.table, self.total + self.pool);
        let free = read::<u32>(&self.free_list, self.pool);
        let fresh = read::<u32>(&self.fresh, self.total);
        Held {
            slot_of: words[..self.total].to_vec(),
            tile_of: words[self.total..].to_vec(),
            free: free[..counts[r + 4] as usize].to_vec(),
            fresh: fresh[..counts[r + 6] as usize].to_vec(),
            exhausted: counts[r + 5],
        }
    }

    /// Every table word, including inactive list tails and the previous rank
    /// half. The input ranges deliberately change during an inactive slot.
    fn words(&self) -> Vec<Vec<u32>> {
        [
            &self.near,
            &self.rank,
            &self.by_ring,
            &self.counts,
            &self.args,
            &self.retired,
            &self.capped,
        ]
        .into_iter()
        .map(|buffer| read(buffer, (buffer.size / 4) as usize))
        .collect()
    }

    fn parity(&self) -> u32 {
        read::<u32>(&self.counts, self.r as usize + 4)[self.r as usize + 3]
    }

    /// The GPU's words in the model's shape: the current rank half, the
    /// lists cut to their counts.
    fn model(&self) -> Model {
        let counts = read::<u32>(&self.counts, self.r as usize + 4);
        let rank = read::<u32>(&self.rank, 2 * self.total);
        let half = self.parity() as usize * self.total;
        let by_ring = read::<u32>(&self.by_ring, self.total);
        let retired = read::<u32>(&self.retired, self.total);
        Model {
            near: read(&self.near, self.total),
            rank: rank[half..half + self.total].to_vec(),
            by_ring: by_ring[..counts[self.r as usize + 1] as usize].to_vec(),
            counts: counts[..self.r as usize + 3].to_vec(),
            args: read(&self.args, argument_words(self.r as usize)),
            retired: retired[..counts[self.r as usize + 2] as usize].to_vec(),
        }
    }

    fn stats_word(&self) -> f32 {
        f32::from_bits(read::<u32>(&self.capped, 7)[6])
    }
}

fn assert_model(gpu: &Model, cpu: &Model, what: &str) {
    for (name, g, c) in [
        ("near", &gpu.near, &cpu.near),
        ("rank", &gpu.rank, &cpu.rank),
        ("by_ring", &gpu.by_ring, &cpu.by_ring),
        ("counts", &gpu.counts, &cpu.counts),
        ("args", &gpu.args, &cpu.args),
        ("retired", &gpu.retired, &cpu.retired),
    ] {
        if let Some(i) = (0..g.len().max(c.len())).find(|&i| g.get(i) != c.get(i)) {
            panic!(
                "{what}: {name} differs first at {i}: gpu {:?} cpu {:?} (lengths {} / {})",
                g.get(i),
                c.get(i),
                g.len(),
                c.len()
            );
        }
    }
}

#[test]
fn gpu_flip_tiles_parallel_nearness_matches_cpu_on_synthetic_masks() {
    let device = manifold_gpu::testkit::test_device();
    let extension_layers = band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS);
    let r = ring_max(extension_layers);
    for n in [
        [1, 1, 1],
        [7, 9, 5],
        [17, 10, 25],
        [64, 64, 64],
        [128, 128, 128],
    ] {
        let total = tile_total(n) as usize;
        let cells = n.iter().map(|&v| v as usize).product();
        let table = Table::new(&device, n, r);
        let clock = device.create_buffer_shared(48);
        let mut previous_rank = vec![0; total];
        let mut parity = 0;
        for (mask, all, inactive) in [
            (0, false, false),
            (1, false, false),
            (2, false, false),
            (3, false, false),
            (0, false, false),
            (0, true, false),
            (0, false, true),
            (2, false, false),
        ] {
            let mut counts = vec![u32::from(mask == 1); cells];
            let index = |p: [u32; 3]| (p[0] + n[0] * (p[1] + n[1] * p[2])) as usize;
            if mask == 2 {
                // One cell lies one past tile 0, inside its CELL_REACH halo.
                counts[index([8.min(n[0] - 1), 0, 0])] = 1;
            } else if mask == 3 {
                for p in [[0; 3], n.map(|v| v - 1), n.map(|v| 9.min(v - 1))] {
                    counts[index(p)] = 7;
                }
            }
            let params = StepParams::default()
                .with_grid_for_test(n, [0.0; 3], 1.0)
                .with_tiles_for_test(r, extension_layers, u32::from(all));
            let mut plan = [0u32; 12];
            plan[11] = 1;
            plan[0] = if inactive { 0.0_f32 } else { 1.0_f32 / 60.0 }.to_bits();
            // SAFETY: shared 48-byte plan; the prior table passes completed.
            unsafe { clock.write(0, bytemuck::cast_slice(&plan)) };
            let context = format!("{n:?}, mask {mask}, all_tiles {all}, inactive {inactive}");
            if inactive {
                let mut expected = table.words();
                assert!(
                    expected[0].iter().all(|&near| near == 0),
                    "the preceding all_tiles table is populated"
                );
                assert!(
                    counts.iter().all(|&count| count == 0),
                    "inactive input must differ from the prior table"
                );
                // Only execution decisions are cleared; both rank halves,
                // parity, lists, retired list and diagnostics remain intact.
                for triple in 0..r as usize + 2 {
                    expected[4][3 * triple] = 0;
                }
                expected[4][argument_words(r as usize) - 1] = 0;
                table.step_with_clock(&device, &params, &counts, Some(&clock));
                assert_eq!(
                    table.words(),
                    expected,
                    "every inactive table word: {context}"
                );
            } else {
                let expected = model(&counts, n, r, &previous_rank, all);
                table.step_with_clock(&device, &params, &counts, Some(&clock));
                parity = 1 - parity;
                assert_model(&table.model(), &expected, &context);
                assert_eq!(
                    table.parity(),
                    parity,
                    "one parity toggle per active dispatch: {context}"
                );
                assert_eq!(
                    table.stats_word(),
                    expected.counts[0] as f32 / total as f32,
                    "active fraction: {context}"
                );
                previous_rank = expected.rank;
            }
            assert_eq!(
                table.parity(),
                parity,
                "inactive slots must hold parity: {context}"
            );
        }
    }
}

/// A run of `dam_break(64)` whose every frame, the fill included, goes
/// through the `poison` lever.
struct Twin {
    run: Run,
    poison: bool,
}

impl Twin {
    fn new(poison: bool) -> Self {
        set_poison(poison);
        let run = Run::new(WaterScene::dam_break(64));
        set_poison(false);
        Self { run, poison }
    }

    fn frame(&mut self) {
        set_poison(self.poison);
        self.run.frame();
        set_poison(false);
    }
}

/// The first particle and, with `faces` (the step has run: not on the fill
/// frame, whose face output is unsized), the first face record that differ
/// between the two runs, as none or where and what.
fn first_difference(a: &Run, b: &Run, faces: bool) -> Option<String> {
    let (pa, pb) = (a.particles(), b.particles());
    if let Some(i) =
        (0..pa.len()).find(|&i| bytemuck::bytes_of(&pa[i]) != bytemuck::bytes_of(&pb[i]))
    {
        return Some(format!("particle {i}: {:?} vs {:?}", pa[i], pb[i]));
    }
    if !faces {
        return None;
    }
    let (fa, fb) = (a.faces(), b.faces());
    let m = a.n() + 1;
    if let Some(i) =
        (0..fa.len()).find(|&i| bytemuck::bytes_of(&fa[i]) != bytemuck::bytes_of(&fb[i]))
    {
        let p = [i % m, (i / m) % m, i / (m * m)];
        let tile = p.map(|v| v / TILE as usize);
        return Some(format!(
            "face record {i} at {p:?} (tile {tile:?}): {:?} vs {:?}",
            fa[i], fb[i]
        ));
    }
    None
}

/// NaN in every free slot and in the tiles outside rings 0 and 1 never
/// reaches the particles or the output faces: both bitwise equal to the
/// unpoisoned run over 4 frames (design section 4 (The defined-value rule)).
/// No pass writes the canonical block either: the pooled `faces` and
/// `distance` end with the 8 elements `pool_canon` wrote, which is what reads
/// of the absent tiles return.
#[test]
fn gpu_flip_sparse_step_survives_poison() {
    let mut clean = Twin::new(false);
    let mut poisoned = Twin::new(true);
    for frame in 0..=4 {
        if frame > 0 {
            clean.frame();
            poisoned.frame();
        }
        if let Some(diff) = first_difference(&clean.run, &poisoned.run, frame > 0) {
            panic!("frame {frame} differs under the poison: {diff}");
        }
        let particles = poisoned.run.particles();
        assert!(
            particles.iter().all(|p| p
                .position_radius
                .iter()
                .chain(&p.velocity)
                .all(|v| v.is_finite())),
            "frame {frame}: a poisoned particle"
        );
    }
    let (faces, distance, table) = poisoned.run.pooled_outputs();
    let h = WaterScene::dam_break(64).cell_size() as f32;
    for mask in 0..8usize {
        let expected = FaceSample {
            velocity: [0.0; 4],
            weight: std::array::from_fn(|a| f32::from(a < 3 && (mask >> a) & 1 == 1)),
        };
        assert_eq!(faces[faces.len() - 8 + mask], expected, "face canon {mask}");
        assert_eq!(
            distance[distance.len() - 8 + mask].to_bits(),
            (3.0 * h).to_bits(),
            "distance canon {mask}"
        );
    }
    let tiles = table.len() / 2;
    let absent = table[..tiles].iter().filter(|&&slot| slot == NONE).count();
    assert!(
        absent > 0 && absent < tiles,
        "{absent} of {tiles} tiles absent: the water holds some, not all"
    );
}

/// The tiles with a slot.
fn held_tiles(held: &Held) -> Vec<u32> {
    (0..held.slot_of.len() as u32)
        .filter(|&t| held.slot_of[t as usize] != NONE)
        .collect()
}

/// slot_of and tile_of are inverse on held slots, every slot is free or held
/// exactly once, and the free stack's live words are distinct.
fn assert_consistent(held: &Held, pool: usize, what: &str) {
    let mut seen = vec![0u32; pool];
    for (t, &slot) in held.slot_of.iter().enumerate() {
        if slot != NONE {
            assert!((slot as usize) < pool, "{what}: tile {t} holds slot {slot}");
            assert_eq!(held.tile_of[slot as usize], t as u32, "{what}: tile {t}");
            seen[slot as usize] += 1;
        }
    }
    for (slot, &tile) in held.tile_of.iter().enumerate() {
        if tile != NONE {
            assert_eq!(
                held.slot_of[tile as usize], slot as u32,
                "{what}: slot {slot}"
            );
        }
    }
    for &slot in &held.free {
        assert!((slot as usize) < pool, "{what}: free slot {slot}");
        assert_eq!(
            held.tile_of[slot as usize], NONE,
            "{what}: slot {slot} free and held"
        );
        seen[slot as usize] += 1;
    }
    assert!(
        seen.iter().all(|&count| count == 1),
        "{what}: a slot is neither free nor held exactly once: {:?}",
        seen.iter().position(|&count| count != 1)
    );
}

fn cells_with(n: [u32; 3], at: &[[u32; 3]]) -> Vec<u32> {
    let mut counts = vec![0u32; (n[0] * n[1] * n[2]) as usize];
    for p in at {
        counts[(p[0] + n[0] * (p[1] + n[1] * p[2])) as usize] = 3;
    }
    counts
}

/// Three steps of a moving synthetic mask through classify, rings and the
/// allocator; every step is checked against the CPU model's rings, and the
/// bookkeeping of each is returned.
fn allocation_sequence(device: &manifold_gpu::GpuDevice) -> Vec<Held> {
    let n = [96, 64, 80];
    let extension_layers = band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS);
    let r = ring_max(extension_layers);
    let total = tile_total(n) as usize;
    let params = StepParams::default()
        .with_grid_for_test(n, [0.0; 3], 1.0)
        .with_tiles_for_test(r, extension_layers, 0);
    let table = Table::new(device, n, r);
    let masks: [&[[u32; 3]]; 3] = [
        &[[4, 4, 4]],
        &[[60, 30, 40]],
        &[[60, 30, 40], [10, 50, 70], [90, 4, 40]],
    ];
    let mut previous: Vec<u32> = vec![0; total];
    let mut before: Vec<u32> = Vec::new();
    let mut out = Vec::new();
    for (step, mask) in masks.into_iter().enumerate() {
        let counts = cells_with(n, mask);
        table.step(device, &params, &counts);
        table.allocate(device, &params);
        let cpu = model(&counts, n, r, &previous, false);
        let held = table.held();
        let what = format!("step {step}");
        assert_consistent(&held, total, &what);
        assert_eq!(held.exhausted, 0, "{what}: a whole pool never runs out");
        let expected: Vec<u32> = (0..total as u32)
            .filter(|&t| cpu.rank[t as usize] <= r)
            .collect();
        assert_eq!(
            held_tiles(&held),
            expected,
            "{what}: held set is ring <= ring_max"
        );
        assert!(
            expected.len() < total,
            "{what}: the mask leaves tiles absent"
        );
        let mut fresh = held.fresh.clone();
        fresh.sort_unstable();
        let newly: Vec<u32> = expected
            .iter()
            .copied()
            .filter(|t| !before.contains(t))
            .collect();
        assert_eq!(fresh, newly, "{what}: fresh list is the newly held tiles");
        assert!(!newly.is_empty(), "{what}: the mask moved");
        previous = cpu.rank;
        before = expected;
        out.push(held);
    }
    out
}

/// The allocator holds exactly the tiles within ring_max of the water over a
/// moving mask, frees the rest, lists what is new, and gives the same slots
/// on a second run.
#[test]
fn gpu_flip_allocator_follows_the_water_deterministically() {
    let device = manifold_gpu::testkit::test_device();
    let first = allocation_sequence(&device);
    let second = allocation_sequence(&device);
    assert_eq!(first, second, "slot assignment depends only on the tiles");
}

/// A pool shorter than the water's reach keeps the nearest tiles: every tile
/// with ring <= 1 (the cell set C lies there) is held, the exhausted word is
/// set, the farther rings take what is left.
#[test]
fn gpu_flip_short_pool_keeps_the_nearest_tiles() {
    let device = manifold_gpu::testkit::test_device();
    let n = [96, 64, 80];
    let extension_layers = band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS);
    let r = ring_max(extension_layers);
    let counts = cells_with(n, &[[44, 30, 40]]);
    let cpu = model(&counts, n, r, &vec![0; tile_total(n) as usize], false);
    let near = cpu.counts[1] as usize;
    assert!(
        cpu.counts[2] > cpu.counts[1],
        "the mask has tiles past ring 1"
    );
    for pool in [near, near + 5] {
        let params = StepParams::default()
            .with_grid_for_test(n, [0.0; 3], 1.0)
            .with_tiles_for_test(r, extension_layers, 0);
        let table = Table::with_pool(&device, n, r, pool);
        table.step(&device, &params, &counts);
        table.allocate(&device, &params);
        let held = table.held();
        assert_consistent(&held, pool, &format!("pool {pool}"));
        assert_eq!(held.exhausted, 1, "pool {pool}");
        assert!(held.free.is_empty(), "pool {pool}: every slot taken");
        let tiles = held_tiles(&held);
        assert_eq!(tiles.len(), pool);
        assert!(
            (0..tile_total(n) as u32)
                .filter(|&t| cpu.rank[t as usize] <= 1)
                .all(|t| tiles.contains(&t)),
            "pool {pool}: a ring 0 or 1 tile is absent"
        );
        assert!(
            tiles.iter().all(|&t| cpu.rank[t as usize] <= r),
            "pool {pool}: a tile past ring_max is held"
        );
    }
}

//! The GPU FLIP step's tile table (docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md
//! section 3 (The tile table)) against a CPU model: the three kernels run on
//! their own over the Dam Break's particles, every word compared, then the
//! same classification read back through the step's stats word. Then the
//! sparse step against the dense one through the same kernels, bitwise, and
//! under the NaN poison (section 4 (The defined-value rule)).

use manifold_gpu::GpuBuffer;

use super::gpu_flip_preset::WaterScene;
use super::gpu_flip_scene_tests::Run;
use super::gpu_flip_step::{
    CELL_REACH, ENGINE_CFL, FACE_VALID_LAYERS, StepParams, TILE, band_layers, dispatch_pass, ring_max,
    set_all_tiles, set_poison, tile_counts, tile_total,
};
use super::liquid_surface_tests::read;
use crate::node_graph::fluid_particles::{CellRange, FluidParticle};

/// The step shader's poison entry: NaN into every cell array of the tiles
/// outside rings 0 and 1, after the retire. Named here only, so the step's
/// own source never spells it (a test below holds that).
pub(crate) const POISON_ENTRY: &str = "poison_inactive";

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

/// Particles per cell, binned as the sort bins them (clamped to the edge
/// cells, radius 0 is not live).
fn cell_counts(particles: &[FluidParticle], n: [u32; 3], min: [f32; 3], h: f32) -> Vec<u32> {
    let mut counts = vec![0u32; (n[0] * n[1] * n[2]) as usize];
    let inv = 1.0 / h;
    for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
        let c: [u32; 3] = std::array::from_fn(|a| {
            (((p.position_radius[a] - min[a]) * inv).floor() as i64).clamp(0, i64::from(n[a]) - 1) as u32
        });
        counts[(c[0] + n[0] * (c[1] + n[1] * c[2])) as usize] += 1;
    }
    counts
}

fn unflatten(t: u32, m: [u32; 3]) -> [i64; 3] {
    [i64::from(t % m[0]), i64::from((t / m[0]) % m[1]), i64::from(t / (m[0] * m[1]))]
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
        let last: [i64; 3] = std::array::from_fn(|a| (origin[a] + i64::from(TILE) - 1).min(i64::from(n[a]) - 1));
        let reach = i64::from(CELL_REACH);
        for z in (origin[2] - reach).max(0)..=(last[2] + reach).min(i64::from(n[2]) - 1) {
            for y in (origin[1] - reach).max(0)..=(last[1] + reach).min(i64::from(n[1]) - 1) {
                for x in (origin[0] - reach).max(0)..=(last[0] + reach).min(i64::from(n[0]) - 1) {
                    if counts[(x + i64::from(n[0]) * (y + i64::from(n[1]) * z)) as usize] == 0 {
                        continue;
                    }
                    let c = [x, y, z];
                    let d: [i64; 3] = std::array::from_fn(|a| (origin[a] - c[a]).max(c[a] - last[a]).max(0));
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
    let retired: Vec<u32> = (0..total as u32).filter(|&t| prev_rank[t as usize] == 0 && ranks[t as usize] != 0).collect();
    counts_out.push(retired.len() as u32);
    let mut args = Vec::new();
    for &count in &counts_out[..=r as usize] {
        args.extend([2 * count, 1, 1]);
    }
    args.extend([2 * retired.len() as u32, 1, 1]);
    Model { near, rank: ranks, by_ring, counts: counts_out, args, retired }
}

/// The tile buffers on the GPU, shared so the test reads them back.
struct Table {
    ranges: GpuBuffer,
    near: GpuBuffer,
    rank: GpuBuffer,
    by_ring: GpuBuffer,
    counts: GpuBuffer,
    args: GpuBuffer,
    retired: GpuBuffer,
    capped: GpuBuffer,
    total: usize,
    r: u32,
}

impl Table {
    fn new(device: &manifold_gpu::GpuDevice, n: [u32; 3], r: u32) -> Self {
        let total = tile_total(n) as usize;
        let words = |w: usize| {
            let b = device.create_buffer_shared((w.max(1) * 4) as u64);
            b.zero_fill();
            b
        };
        Self {
            ranges: words(2 * (n[0] * n[1] * n[2]) as usize),
            near: words(total),
            rank: words(2 * total),
            by_ring: words(total),
            counts: words(r as usize + 4),
            args: words(3 * (r as usize + 2)),
            retired: words(total),
            capped: words(7),
            total,
            r,
        }
    }

    /// One step's three kernels over `counts` (the sort's per-cell counts).
    fn step(&self, device: &manifold_gpu::GpuDevice, params: &StepParams, counts: &[u32]) {
        let ranges: Vec<CellRange> = counts.iter().map(|&count| CellRange { start: 0, count }).collect();
        let ptr = self.ranges.mapped_ptr().expect("shared");
        // SAFETY: the buffer holds two words per cell and no GPU work is in flight.
        unsafe { std::ptr::copy_nonoverlapping(ranges.as_ptr().cast::<u8>(), ptr, ranges.len() * 8) };
        let threads = self.total as u64;
        dispatch_pass(device, "tiles_classify", params, &[(1, &self.ranges), (27, &self.near), (30, &self.counts)], threads);
        dispatch_pass(device, "tiles_rings", params, &[(27, &self.near), (28, &self.rank), (30, &self.counts)], threads);
        dispatch_pass(
            device,
            "tiles_lists",
            params,
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
            args: read(&self.args, 3 * (self.r as usize + 2)),
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
            panic!("{what}: {name} differs first at {i}: gpu {:?} cpu {:?} (lengths {} / {})", g.get(i), c.get(i), g.len(), c.len());
        }
    }
}

/// `tile_near`, `tile_rank`, the lists, counts, triples, retired list and
/// parity equal the CPU model on `dam_break(64)` at frames 0, 30 and 60,
/// stepped through the same buffers so the parity flips and the retired
/// list sees the previous step; `all_tiles` lights everything; the step's
/// own stats word carries the same |C| / T³.
#[test]
fn gpu_flip_tiles_match_the_cpu_classification() {
    let scene = WaterScene::dam_break(64);
    let mut run = Run::new(scene);
    let device = crate::test_device();
    let layout = scene.layout();
    let (n, min, h) = (layout.cells, layout.min, layout.cell_size as f32);
    let r = ring_max(band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS));
    let total = tile_total(n) as usize;
    let params = StepParams { n, box_min: min, cell_size: h, ring_max: r, ..StepParams::default() };
    let table = Table::new(&device, n, r);
    let mut prev = vec![0u32; total];
    let mut frame = 0;
    for (step, target) in [0, 30, 60].into_iter().enumerate() {
        while frame < target {
            run.frame();
            frame += 1;
        }
        let counts = cell_counts(&run.particles(), n, min, h);
        table.step(&device, &params, &counts);
        let cpu = model(&counts, n, r, &prev, false);
        assert_model(&table.model(), &cpu, &format!("frame {target}"));
        assert_eq!(table.parity(), (step as u32 + 1) % 2, "parity after step {}", step + 1);
        assert_eq!(table.stats_word().to_bits(), (cpu.counts[0] as f32 / total as f32).to_bits(), "stats word at frame {target}");
        if target == 0 {
            // The standing column: 0.312 of the tiles (design section 6).
            assert!((i64::from(cpu.counts[0]) - 160).abs() <= 1, "frame 0 lights {} tiles of {total}", cpu.counts[0]);
            // The step itself classifies the same particles on its next tick.
            run.frame();
            frame += 1;
            let stats = run.liquid_stats().active_tiles;
            assert_eq!(stats.to_bits(), (cpu.counts[0] as f32 / total as f32).to_bits(), "the step's stats word {stats}");
        }
        prev = cpu.rank;
    }
    let dense = StepParams { all_tiles: 1, ..params };
    let counts = cell_counts(&run.particles(), n, min, h);
    table.step(&device, &dense, &counts);
    let cpu = model(&counts, n, r, &prev, true);
    assert!(cpu.near.iter().all(|&v| v == 0) && cpu.rank.iter().all(|&v| v == 0) && cpu.counts[0] == total as u32);
    assert_model(&table.model(), &cpu, "all_tiles");
    assert_eq!(table.stats_word(), 1.0);
    // The lever reaches the step: its stats word reads 1.0 on the next tick.
    set_all_tiles(true);
    run.frame();
    set_all_tiles(false);
    assert_eq!(run.liquid_stats().active_tiles, 1.0, "the step under all_tiles");
    run.frame();
    assert!(run.liquid_stats().active_tiles < 1.0, "the lever released");
}

/// A run of `dam_break(64)` whose every frame, the fill included, goes
/// through the levers `all` and `poison`.
struct Twin {
    run: Run,
    all: bool,
    poison: bool,
}

impl Twin {
    fn new(all: bool, poison: bool) -> Self {
        set_all_tiles(all);
        set_poison(poison);
        let run = Run::new(WaterScene::dam_break(64));
        set_all_tiles(false);
        set_poison(false);
        Self { run, all, poison }
    }

    fn frame(&mut self) {
        set_all_tiles(self.all);
        set_poison(self.poison);
        self.run.frame();
        set_all_tiles(false);
        set_poison(false);
    }
}

/// The first particle and, with `faces` (the step has run: not on the fill
/// frame, whose face output is unsized), the first face record that differ
/// between the two runs, as none or where and what.
fn first_difference(a: &Run, b: &Run, faces: bool) -> Option<String> {
    let (pa, pb) = (a.particles(), b.particles());
    if let Some(i) = (0..pa.len()).find(|&i| bytemuck::bytes_of(&pa[i]) != bytemuck::bytes_of(&pb[i])) {
        return Some(format!("particle {i}: {:?} vs {:?}", pa[i], pb[i]));
    }
    if !faces {
        return None;
    }
    let (fa, fb) = (a.faces(), b.faces());
    let m = a.n() + 1;
    if let Some(i) = (0..fa.len()).find(|&i| bytemuck::bytes_of(&fa[i]) != bytemuck::bytes_of(&fb[i])) {
        let p = [i % m, (i / m) % m, i / (m * m)];
        let tile = p.map(|v| v / TILE as usize);
        return Some(format!("face record {i} at {p:?} (tile {tile:?}): {:?} vs {:?}", fa[i], fb[i]));
    }
    None
}

/// The sparse step equals the dense one through the same kernels and lists
/// (design section 6 (Proof plan)): 60 frames of `dam_break(64)`, the
/// particles and the output faces bitwise after every frame.
#[test]
fn gpu_flip_sparse_step_matches_dense_bitwise() {
    let mut sparse = Twin::new(false, false);
    let mut dense = Twin::new(true, false);
    if let Some(diff) = first_difference(&sparse.run, &dense.run, false) {
        panic!("the fill frame differs: {diff}");
    }
    let mut active = Vec::with_capacity(60);
    for frame in 1..=60 {
        sparse.frame();
        dense.frame();
        active.push(sparse.run.liquid_stats().active_tiles);
        if let Some(diff) = first_difference(&sparse.run, &dense.run, true) {
            panic!("frame {frame} differs: {diff}");
        }
        // The lever reaches the solver's tile lists too: the same iterations.
        assert_eq!(sparse.run.solver(), dense.run.solver(), "frame {frame}: the solver words differ");
    }
    let mean = active.iter().map(|&a| f64::from(a)).sum::<f64>() / active.len() as f64;
    assert!(mean > 0.0 && mean < 1.0, "the sparse run lit {mean} of the tiles on average");
    println!("gpu_flip_sparse_step_matches_dense_bitwise: active tiles {:.3} at frame 1, {mean:.3} mean over 60", active[0]);
}

/// NaN in every cell array of the tiles outside rings 0 and 1 after the
/// retire never reaches the particles or the output faces: both bitwise
/// equal to the unpoisoned sparse run over 10 frames (design section 4 (The
/// defined-value rule)).
#[test]
fn gpu_flip_sparse_step_survives_poison() {
    let mut clean = Twin::new(false, false);
    let mut poisoned = Twin::new(false, true);
    for frame in 0..=10 {
        if frame > 0 {
            clean.frame();
            poisoned.frame();
        }
        if let Some(diff) = first_difference(&clean.run, &poisoned.run, frame > 0) {
            panic!("frame {frame} differs under the poison: {diff}");
        }
        let particles = poisoned.run.particles();
        assert!(
            particles.iter().all(|p| p.position_radius.iter().chain(&p.velocity).all(|v| v.is_finite())),
            "frame {frame}: a poisoned particle"
        );
    }
}

/// The poison entry is the proofs' alone: no source file outside the tests
/// spells its name (design section 4 (The defined-value rule)).
#[test]
fn gpu_flip_poison_entry_is_named_only_in_tests() {
    fn walk(dir: &std::path::Path, hits: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, hits);
            } else if path.extension().is_some_and(|e| e == "rs")
                && std::fs::read_to_string(&path).is_ok_and(|s| s.contains(POISON_ENTRY))
            {
                hits.push(path);
            }
        }
    }
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("crates/");
    let mut hits = Vec::new();
    walk(crates, &mut hits);
    let stray: Vec<_> = hits.iter().filter(|p| !p.to_string_lossy().ends_with("_tests.rs")).collect();
    assert!(stray.is_empty(), "{POISON_ENTRY} is named outside the tests: {stray:?}");
    assert!(!hits.is_empty(), "this file names it");
}

//! The GPU FLIP step's tile table (docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md
//! section 3 (The tile table)) against a CPU model: the three kernels run on
//! their own over the Dam Break's particles, every word compared, then the
//! same classification read back through the step's stats word. Then the
//! sparse step against the dense one through the same kernels, bitwise, and
//! under the NaN poison (section 4 (The defined-value rule)).

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use manifold_node_engine::water::primitives::gpu_flip_preset::WaterScene;
use super::gpu_flip_scene_tests::Run;
use manifold_node_engine::water::primitives::gpu_flip_step::{
    CELL_REACH, ENGINE_CFL, FACE_VALID_LAYERS, StepParams, TILE, band_layers, dispatch_pass,
    ring_max, set_all_tiles, set_poison, tile_counts, tile_total,
};
use manifold_node_engine::water::primitives::liquid_stats::with_stats_layout;
use manifold_node_engine::testkit::liquid_surface::read;
use manifold_node_engine::water::fluid_particles::{CellRange, FluidParticle};
use manifold_node_engine::water::liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE};
use manifold_node_engine::water::liquid::fields::LIQUID_FIELD;

fn gather_sources() -> [String; 2] {
    let source = include_str!("../../../manifold-node-engine/src/water/primitives/shaders/gpu_flip_step.wgsl");
    // Restore the original dynamic-axis gather only in the test oracle.
    // Keep its support decisions and floating-point accumulation verbatim.
    let inner = r#"                    for (var a = 0; a < 3; a = a + 1) {
                        if !exists[a] {
                            continue;
                        }
                        var face = vec3<f32>(p) + vec3<f32>(0.5);
                        face[a] = f32(p[a]);
                        let v = face - q;
                        let d2 = dot(v, v);
                        if !(d2 < rsq) {
                            continue;
                        }
                        let w = 1.0 - coef1 * d2 * d2 * d2 + coef2 * d2 * d2 - coef3 * d2;
                        weight[a] = weight[a] + w;
                        momentum[a] = momentum[a] + w * particle.velocity[a];
                    }"#;
    let mut unrolled = String::new();
    let mut centres = String::new();
    for axis in ["x", "y", "z"] {
        centres.push_str(&format!("    var face_{axis} = vec3<f32>(p) + vec3<f32>(0.5);\n    face_{axis}.{axis} = f32(p.{axis});\n"));
        unrolled.push_str(&format!(
            r#"                    if exists.{axis} {{
                        let v = face_{axis} - q;
                        let d2 = dot(v, v);
                        if d2 < rsq {{
                            let w = 1.0 - coef1 * d2 * d2 * d2 + coef2 * d2 * d2 - coef3 * d2;
                            weight.{axis} = weight.{axis} + w;
                            momentum.{axis} = momentum.{axis} + w * particle.velocity.{axis};
                        }}
                    }}
"#
        ));
    }
    let anchor = "    var weight = vec3<f32>(0.0);\n    var momentum = vec3<f32>(0.0);";
    assert_eq!(
        source.matches(anchor).count(),
        1,
        "gather accumulation anchor"
    );
    let unrolled = unrolled.trim_end();
    assert_eq!(
        source.matches(unrolled).count(),
        1,
        "production gather inner loop"
    );
    assert_eq!(
        source.matches(&centres).count(),
        1,
        "production gather centres"
    );
    let original = source.replace(unrolled, inner).replace(&centres, "");
    [original.as_str(), source].map(|shader| {
        with_stats_layout(&format!(
            "{LIQUID_POSE}\n{LIQUID_COLLIDER}\n{LIQUID_FIELD}\n{shader}"
        ))
    })
}

#[test]
fn gpu_flip_gather_unrolled_shader_validates() {
    for source in gather_sources() {
        let module = naga::front::wgsl::parse_str(&source).expect("gather shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("gather shader validates");
    }
}

fn gather_pipelines(device: &GpuDevice) -> [GpuComputePipeline; 2] {
    gather_sources().map(|source| {
        device.create_compute_pipeline(&source, "particles_to_faces", "gather-unroll-proof")
    })
}

fn gather_pass(
    device: &GpuDevice,
    pipeline: &GpuComputePipeline,
    p: &StepParams,
    buffers: &[(u32, &GpuBuffer)],
) -> f64 {
    assert_eq!(
        p.narrow_band_for_test(), 1,
        "direct dense face indexing, without band masking"
    );
    let mut bindings = vec![GpuBinding::Bytes {
        binding: 0,
        data: bytemuck::bytes_of(p),
    }];
    bindings.extend(buffers.iter().map(|&(binding, buffer)| GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }));
    let faces = p.cells_for_test().into_iter().map(|n| n + 1).product::<u32>();
    let mut enc = device.create_encoder("gather-unroll-proof");
    enc.dispatch_compute(
        pipeline,
        &bindings,
        [faces.div_ceil(256), 1, 1],
        "particles_to_faces",
    );
    enc.commit_and_wait_profiled(device).total_ms
}

fn gather_buffer<T: bytemuck::Pod>(device: &GpuDevice, values: &[T]) -> GpuBuffer {
    let buffer = device.create_buffer_shared((std::mem::size_of_val(values) as u64).max(32));
    buffer.zero_fill();
    // SAFETY: no GPU work is in flight and the buffer holds every supplied record.
    unsafe {
        buffer.write(0, bytemuck::cast_slice(values));
    }
    buffer
}

fn gather_population(
    n: [u32; 3],
    min: [f32; 3],
    h: f32,
    half: bool,
) -> (Vec<CellRange>, Vec<FluidParticle>) {
    let cells = n.into_iter().product::<u32>() as usize;
    let count = if half { cells / 2 } else { cells } * 8;
    let mut particles = Vec::with_capacity(count);
    let mut ranges = Vec::with_capacity(cells);
    for z in 0..n[2] {
        for y in 0..n[1] {
            for x in 0..n[0] {
                let start = particles.len() as u32;
                if !half || x < n[0] / 2 {
                    for site in 0..8 {
                        let cell = [x, y, z];
                        let q: [f32; 3] = std::array::from_fn(|a| {
                            cell[a] as f32 + if site & (1 << a) == 0 { 0.25 } else { 0.75 }
                        });
                        let world: [f32; 3] = std::array::from_fn(|a| min[a] + h * q[a]);
                        particles.push(FluidParticle {
                            position_radius: [world[0], world[1], world[2], 0.125 * h],
                            velocity: [
                                x as f32 * 0.125 - 1.0,
                                y as f32 * -0.25 + 0.5,
                                z as f32 * 0.0625 + site as f32 * 0.125,
                            ],
                            id: particles.len() as u32 + 1,
                        });
                    }
                }
                ranges.push(CellRange {
                    start,
                    count: particles.len() as u32 - start,
                });
            }
        }
    }
    (ranges, particles)
}

#[test]
fn gpu_flip_gather_unrolled_matches_original_every_face_word() {
    let device = manifold_gpu::testkit::test_device();
    let pipelines = gather_pipelines(&device);
    for (n, min, h) in [
        ([1, 1, 1], [0.0; 3], 1.0),
        ([7, 5, 3], [0.0; 3], 1.0),
        ([3, 7, 5], [-4.0, 8.0, -2.0], 0.125),
    ] {
        let (_, mut particles) = gather_population(n, min, h, false);
        // Face centres at both box walls, and adjacent f32 positions at the
        // Wyvill support edge. The original GPU is the arithmetic oracle.
        for axis in 0..3 {
            for edge in [0.0, n[axis] as f32] {
                let mut q = [0.5; 3];
                q[axis] = edge;
                let mut marker = particles[0];
                for (a, coordinate) in q.into_iter().enumerate() {
                    marker.position_radius[a] = min[a] + h * coordinate;
                }
                particles.push(marker);
            }
            // Three half-cell offsets give d2 == 0.75 exactly; vary one
            // coordinate by an adjacent f32 word on each side of support.
            let coordinate: f32 = if n[axis] > 1 { 1.5 } else { 0.5 };
            for edge in [
                f32::from_bits(coordinate.to_bits() - 1),
                coordinate,
                f32::from_bits(coordinate.to_bits() + 1),
            ] {
                let mut q = [1.0; 3];
                q[axis] = edge;
                let mut marker = particles[0];
                for (a, coordinate) in q.into_iter().enumerate() {
                    marker.position_radius[a] = min[a] + h * coordinate;
                }
                particles.push(marker);
            }
        }
        for kind in 0..6 {
            let mut marker = particles[0];
            match kind {
                0 => marker.position_radius[3] = 0.0,
                1 => marker.position_radius[3] = -1.0,
                2 => marker.position_radius[3] = f32::NAN,
                3 => marker.position_radius[0] = f32::NAN,
                4 => marker.velocity[1] = f32::from_bits(0x7fc0_1234),
                _ => marker.velocity[2] = f32::INFINITY,
            }
            particles.push(marker);
        }
        let bin = |marker: &FluidParticle| {
            let cell: [u32; 3] = std::array::from_fn(|a| {
                (((marker.position_radius[a] - min[a]) / h).floor() as i64)
                    .clamp(0, i64::from(n[a]) - 1) as u32
            });
            (cell[0] + n[0] * (cell[1] + n[1] * cell[2])) as usize
        };
        particles.sort_by_key(bin);
        let mut ranges = vec![CellRange::default(); n.into_iter().product::<u32>() as usize];
        for (i, marker) in particles.iter().enumerate() {
            let range = &mut ranges[bin(marker)];
            if range.count == 0 {
                range.start = i as u32;
            }
            range.count += 1;
        }
        let p = StepParams::default().with_grid_for_test(n, min, h)
            .with_gather_for_test(particles.len() as u32, 1, 1.0 / 60.0);
        let ranges = gather_buffer(&device, &ranges);
        let sorted = gather_buffer(&device, &particles);
        let words = n.into_iter().map(|v| v as usize + 1).product::<usize>() * 8;
        let faces = [
            device.create_buffer_shared(words as u64 * 4),
            device.create_buffer_shared(words as u64 * 4),
        ];
        // These bindings remain in the reflected entry, but modes 0 and 2
        // cannot read them here. Full cell extent keeps the storage valid.
        let unused = gather_buffer(
            &device,
            &vec![0u32; n.into_iter().product::<u32>() as usize],
        );
        let clock = device.create_buffer_shared(48);
        let seed: Vec<u32> = (0..words)
            .map(|i| 0x7fc0_0000 | (i as u32 & 0x003f_ffff))
            .collect();
        for active in [true, false, true] {
            let mut plan = [0u32; 12];
            plan[11] = 1;
            plan[0] = if active { p.step_dt_for_test() } else { 0.0 }.to_bits();
            unsafe {
                clock.write(0, bytemuck::cast_slice(&plan));
            }
            for (pipeline, output) in pipelines.iter().zip(&faces) {
                unsafe {
                    output.write(0, bytemuck::cast_slice(&seed));
                }
                gather_pass(
                    &device,
                    pipeline,
                    &p,
                    &[
                        (1, &ranges),
                        (2, &sorted),
                        (4, output),
                        (29, &unused),
                        (45, &unused),
                        (46, &clock),
                    ],
                );
            }
            let old = read::<u32>(&faces[0], words);
            let new = read::<u32>(&faces[1], words);
            assert_eq!(
                old, new,
                "{n:?}, min {min:?}, h {h}, active {active}: every face word"
            );
            if active {
                assert_ne!(old, seed, "active and resumed gather overwrites faces");
                assert!(
                    old.chunks_exact(8).all(|face| face[3] == 0 && face[7] == 0),
                    "every active face record overwrites its seeded padding"
                );
            } else {
                assert_eq!(old, seed, "inactive gather preserves seeded face words");
            }
        }
    }
}

#[test]
#[cfg(feature = "water-race-probes")]
fn gpu_flip_gather_unrolled_bounded_timing() {
    let device = manifold_gpu::testkit::test_device();
    let pipelines = gather_pipelines(&device);
    for side in [64, 128] {
        let n = [side; 3];
        let (ranges, particles) = gather_population(n, [0.0; 3], 0.125, true);
        let p = StepParams::default().with_grid_for_test(n, [0.0; 3], 0.125)
            .with_gather_for_test(particles.len() as u32, 1, 1.0 / 60.0);
        let ranges = gather_buffer(&device, &ranges);
        let sorted = gather_buffer(&device, &particles);
        drop(particles);
        let unused = gather_buffer(&device, &vec![0u32; side.pow(3) as usize]);
        let mut plan = [0u32; 12];
        plan[0] = p.step_dt_for_test().to_bits();
        plan[11] = 1;
        let clock = gather_buffer(&device, &plan);
        let words = (side as usize + 1).pow(3) * 8;
        let faces = [
            device.create_buffer_shared(words as u64 * 4),
            device.create_buffer_shared(words as u64 * 4),
        ];
        let mut samples = [Vec::with_capacity(8), Vec::with_capacity(8)];
        for sample in 0..12 {
            for i in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
                let millis = gather_pass(
                    &device,
                    &pipelines[i],
                    &p,
                    &[
                        (1, &ranges),
                        (2, &sorted),
                        (4, &faces[i]),
                        (29, &unused),
                        (45, &unused),
                        (46, &clock),
                    ],
                );
                if sample >= 4 {
                    samples[i].push(millis);
                }
            }
            assert_eq!(
                read::<u32>(&faces[0], words),
                read::<u32>(&faces[1], words),
                "{side}³, sample {sample}: exact face words"
            );
        }
        for times in &mut samples {
            times.sort_by(f64::total_cmp);
        }
        let median = |times: &[f64]| (times[3] + times[4]) * 0.5;
        eprintln!(
            "GATHER_UNROLL n={side} markers={} half_volume=true warm=4 measured=8 old_median_ms={:.6} unrolled_median_ms={:.6} exact=true",
            p.capacity_for_test(),
            median(&samples[0]),
            median(&samples[1])
        );
    }
}

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

fn argument_words(r: usize) -> usize {
    let triples = 3 * (r + 3);
    triples + triples % 2 + 2
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
    let faces = n.into_iter().map(|side| side + 1).product::<u32>();
    args.extend([faces.div_ceil(256), 1, 1]);
    args.resize(argument_words(r as usize) - 2, 0);
    args.extend([0, band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS)]);
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
            args: words(argument_words(r as usize)),
            retired: words(total),
            capped: words(7),
            total,
            r,
        }
    }

    /// One step's three kernels over `counts` (the sort's per-cell counts).
    fn step(&self, device: &manifold_gpu::GpuDevice, params: &StepParams, counts: &[u32]) {
        self.step_with_clock(device, params, counts, None);
    }

    fn step_with_clock(&self, device: &manifold_gpu::GpuDevice, params: &StepParams, counts: &[u32], clock: Option<&GpuBuffer>) {
        let ranges: Vec<CellRange> = counts.iter().map(|&count| CellRange { start: 0, count }).collect();
        let ptr = self.ranges.mapped_ptr().expect("shared");
        // SAFETY: the buffer holds two words per cell and no GPU work is in flight.
        unsafe { std::ptr::copy_nonoverlapping(ranges.as_ptr().cast::<u8>(), ptr, ranges.len() * 8) };
        let threads = self.total as u64;
        let pass = |entry: &str, buffers: &[(u32, &GpuBuffer)], threads| {
            let mut buffers = buffers.to_vec();
            if let Some(clock) = clock { buffers.push((46, clock)); }
            dispatch_pass(device, entry, params, &buffers, threads);
        };
        pass("tiles_classify", &[(1, &self.ranges), (27, &self.near), (30, &self.counts)], threads * 32);
        pass("tiles_rings", &[(27, &self.near), (28, &self.rank), (30, &self.counts)], threads);
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

    /// Every table word, including inactive list tails and the previous rank
    /// half. The input ranges deliberately change during an inactive slot.
    fn words(&self) -> Vec<Vec<u32>> {
        [&self.near, &self.rank, &self.by_ring, &self.counts, &self.args, &self.retired, &self.capped]
            .into_iter().map(|buffer| read(buffer, (buffer.size / 4) as usize)).collect()
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
            panic!("{what}: {name} differs first at {i}: gpu {:?} cpu {:?} (lengths {} / {})", g.get(i), c.get(i), g.len(), c.len());
        }
    }
}

#[test]
fn gpu_flip_tiles_parallel_nearness_matches_cpu_on_synthetic_masks() {
    let device = manifold_gpu::testkit::test_device();
    let extension_layers = band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS);
    let r = ring_max(extension_layers);
    for n in [[1, 1, 1], [7, 9, 5], [17, 10, 25], [64, 64, 64], [128, 128, 128]] {
        let total = tile_total(n) as usize;
        let cells = n.iter().map(|&v| v as usize).product();
        let table = Table::new(&device, n, r);
        let clock = device.create_buffer_shared(48);
        let mut previous_rank = vec![0; total];
        let mut parity = 0;
        for (mask, all, inactive) in [
            (0, false, false), (1, false, false), (2, false, false),
            (3, false, false), (0, false, false), (0, true, false),
            (0, false, true), (2, false, false),
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
            let params = StepParams::default().with_grid_for_test(n, [0.0; 3], 0.0).with_tiles_for_test(r, extension_layers, u32::from(all));
            let mut plan = [0u32; 12];
            plan[11] = 1;
            plan[0] = if inactive { 0.0_f32 } else { 1.0_f32 / 60.0 }.to_bits();
            // SAFETY: shared 48-byte plan; the prior table passes completed.
            unsafe { clock.write(0, bytemuck::cast_slice(&plan)) };
            let context = format!("{n:?}, mask {mask}, all_tiles {all}, inactive {inactive}");
            if inactive {
                let mut expected = table.words();
                assert!(expected[0].iter().all(|&near| near == 0), "the preceding all_tiles table is populated");
                assert!(counts.iter().all(|&count| count == 0), "inactive input must differ from the prior table");
                // Only execution decisions are cleared; both rank halves,
                // parity, lists, retired list and diagnostics remain intact.
                for triple in 0..r as usize + 3 { expected[4][3 * triple] = 0; }
                *expected[4].last_mut().unwrap() = 0;
                table.step_with_clock(&device, &params, &counts, Some(&clock));
                assert_eq!(table.words(), expected, "every inactive table word: {context}");
            } else {
                let expected = model(&counts, n, r, &previous_rank, all);
                table.step_with_clock(&device, &params, &counts, Some(&clock));
                parity = 1 - parity;
                assert_model(&table.model(), &expected, &context);
                assert_eq!(table.parity(), parity, "one parity toggle per active dispatch: {context}");
                assert_eq!(table.stats_word(), expected.counts[0] as f32 / total as f32, "active fraction: {context}");
                previous_rank = expected.rank;
            }
            assert_eq!(table.parity(), parity, "inactive slots must hold parity: {context}");
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
    let device = manifold_gpu::testkit::test_device();
    let layout = scene.layout();
    // The step classifies tiles on the native solver grid.
    let grid = manifold_node_engine::water::liquid::lattice::FlipSolverGrid::from_lattice(
        manifold_node_engine::water::liquid::lattice::LiquidLattice::from_layout(&layout));
    let (n, min, h) = (grid.cells(), grid.min(), layout.cell_size as f32);
    let r = ring_max(band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS));
    let total = tile_total(n) as usize;
    let params = StepParams::default().with_grid_for_test(n, min, h).with_tiles_for_test(r, band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS), 0);
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
            // The standing column on the native grid: 209 of its 729 tiles
            // (160 of 512 on the authored grid, design section 6).
            assert!((i64::from(cpu.counts[0]) - 209).abs() <= 1, "frame 0 lights {} tiles of {total}", cpu.counts[0]);
            // The step itself classifies the same particles on its next tick.
            run.frame();
            frame += 1;
            let stats = run.liquid_stats().active_tiles;
            assert_eq!(stats.to_bits(), (cpu.counts[0] as f32 / total as f32).to_bits(), "the step's stats word {stats}");
        }
        prev = cpu.rank;
    }
    let dense = params.with_tiles_for_test(r, band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS), 1);
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

    // An inactive slot must actually zero both execution decisions; unchanged
    // particle results alone could hide full-grid launches that return early.
    let clock = device.create_buffer_shared(48);
    let mut plan = [0u32; 12];
    plan[11] = 1; // live mode, no accepted duration
    // SAFETY: the shared buffer has exactly these 48 bytes and is not in flight.
    unsafe { std::ptr::copy_nonoverlapping(plan.as_ptr().cast::<u8>(), clock.mapped_ptr().expect("shared"), 48) };
    dispatch_pass(&device, "tiles_lists", &dense, &[
        (27, &table.near), (28, &table.rank), (29, &table.by_ring),
        (30, &table.counts), (31, &table.args), (32, &table.retired),
        (22, &table.capped), (46, &clock),
    ], 1);
    let mut inactive = cpu;
    for triple in 0..r as usize + 3 {
        inactive.args[3 * triple] = 0;
    }
    *inactive.args.last_mut().expect("extension range length") = 0;
    assert_model(&table.model(), &inactive, "inactive slot gates triples and replay, preserving the table");
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

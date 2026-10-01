//! The GPU FLIP pressure solve end to end on the seven saved Dam Break
//! problems (docs/GPU_FLIP_PRESSURE_SOLVE.md): the true residual of the
//! masked Poisson equation per problem against the f64 reference
//! (`scripts/mgpcg_reference.py --coarse-sweeps 8`, numpy in f64), the
//! iteration trend, and where one solve's GPU time goes.

use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuBuffer, GpuTextureFormat};

use super::gpu_flip_preset::{PressureShape, TREND_ITERATIONS, pressure_def};
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
use crate::node_graph::{
    Backend, EffectGraphDefExt, ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, NodeInstanceId,
    PrimitiveRegistry, ResourceId, StateStore, compile, pre_allocate_resources,
};

/// One saved problem: water cells and the divergence f (zero in air).
struct Problem {
    frame: u32,
    water: Vec<bool>,
    f: Vec<f32>,
}

/// The fixture: "SWFX", version 1, nx, ny, nz, count; then per problem the
/// frame, the water count, n³/8 bytes of water bits (LSB first, cell
/// x + nx·(y + ny·z)) and f32 f per water cell in cell order.
fn load_problems() -> (usize, Vec<Problem>) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dambreak_pressure_problems.bin.zst");
    let raw = zstd::decode_all(std::fs::File::open(path).expect("fixture opens")).expect("fixture decodes");
    let word = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().expect("four bytes"));
    assert_eq!(&raw[..4], b"SWFX");
    assert_eq!(word(4), 1, "fixture version");
    let (n, count) = (word(8) as usize, word(20) as usize);
    assert!(word(12) as usize == n && word(16) as usize == n, "cubic fixture");
    let cells = n * n * n;
    let mut at = 24;
    let problems = (0..count)
        .map(|_| {
            let (frame, wet) = (word(at), word(at + 4) as usize);
            at += 8;
            let water: Vec<bool> = (0..cells).map(|c| raw[at + c / 8] >> (c % 8) & 1 == 1).collect();
            at += cells / 8;
            let mut values = raw[at..at + 4 * wet].chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap()));
            at += 4 * wet;
            let f = water.iter().map(|&w| if w { values.next().expect("one f per water cell") } else { 0.0 }).collect();
            assert!(values.next().is_none(), "frame {frame}: more f values than water cells");
            Problem { frame, water, f }
        })
        .collect();
    (n, problems)
}

/// Each cell becomes k³ with the same water flag and f, as the reference refines.
fn refine(p: &Problem, n: usize, k: usize) -> Problem {
    if k == 1 {
        return Problem { frame: p.frame, water: p.water.clone(), f: p.f.clone() };
    }
    let m = n * k;
    let source = |c: usize| {
        let (x, y, z) = (c % m, (c / m) % m, c / (m * m));
        x / k + n * (y / k + n * (z / k))
    };
    Problem {
        frame: p.frame,
        water: (0..m * m * m).map(|c| p.water[source(c)]).collect(),
        f: (0..m * m * m).map(|c| p.f[source(c)]).collect(),
    }
}

/// |masked Laplacian(p) − f| / |f|: air neighbours hold zero pressure,
/// neighbours past the box walls are missing.
fn residual(p: &[f32], water: &[bool], f: &[f32], n: usize, h: f64) -> f64 {
    let (mut miss, mut size) = (0.0, 0.0);
    for c in (0..n * n * n).filter(|&c| water[c]) {
        let at = [c % n, (c / n) % n, c / (n * n)];
        let stride = [1, n, n * n];
        let centre = f64::from(p[c]);
        let mut sum = 0.0;
        for a in 0..3 {
            for next in [at[a].checked_sub(1), Some(at[a] + 1).filter(|&q| q < n)].into_iter().flatten() {
                let neighbour = c + next * stride[a] - at[a] * stride[a];
                let value = if water[neighbour] { f64::from(p[neighbour]) } else { 0.0 };
                sum += value - centre;
            }
        }
        let f = f64::from(f[c]);
        miss += (sum / (h * h) - f).powi(2);
        size += f * f;
    }
    (miss / size).sqrt()
}

pub(super) fn node_named(graph: &Graph, name: &str) -> NodeInstanceId {
    graph.nodes().find(|n| n.node_id.as_str() == name).map(|n| n.id).unwrap_or_else(|| panic!("no node {name}"))
}

pub(super) fn output_of(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    let step = plan.steps().iter().find(|s| s.node == node).expect("node compiled");
    step.outputs.iter().find(|(name, _)| *name == port).map(|&(_, r)| r).expect("output port")
}

/// The solve graph compiled and bound once; problems are loaded between frames.
struct Solver {
    shape: PressureShape,
    device: crate::TestDevice,
    graph: Graph,
    plan: ExecutionPlan,
    exec: Executor,
    state: StateStore,
    water: GpuBuffer,
    f: GpuBuffer,
    pressure: ResourceId,
    frames: i64,
}

impl Solver {
    fn new(shape: PressureShape) -> Self {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        let graph = pressure_def(shape).into_graph(&registry, &Default::default()).expect("pressure def builds");
        let plan = compile(&graph).expect("pressure def compiles");
        let device = crate::test_device();
        let mut backend = MetalBackend::new(device.arc(), 64, 64, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&graph, &plan, &device, &mut backend).expect("pre-allocate");
        let buffer = |name: &str| {
            let res = output_of(&plan, node_named(&graph, name), "out");
            let slot = backend.slot_for(res).expect("source bound");
            Backend::array_buffer(&backend, slot).expect("source buffer").clone()
        };
        let (water, f) = (buffer("water"), buffer("f"));
        let cells = shape.cells() as u64 * 4;
        assert!(water.size >= cells && f.size >= cells, "sources hold the lattice");
        let cg = node_named(&graph, "cg");
        let pressure = output_of(&plan, cg, "solution");
        let mut exec = Executor::new(Box::new(backend));
        exec.set_dump_set(Some(std::iter::once(cg).collect()));
        Self { shape, device, graph, plan, exec, state: StateStore::new(), water, f, pressure, frames: 0 }
    }

    fn load(&mut self, water: &[bool], f: &[f32]) {
        let water: Vec<f32> = water.iter().map(|&w| f32::from(u8::from(w))).collect();
        assert!(water.len() == self.shape.cells() && f.len() == self.shape.cells());
        // SAFETY: shared-storage buffers sized for the lattice (checked in
        // new); the previous frame has completed. The sources are written
        // before every frame: the planner recycles their storage after their
        // last reader.
        unsafe {
            self.water.write(0, bytemuck::cast_slice(&water));
            self.f.write(0, bytemuck::cast_slice(f));
        }
    }

    fn frame_time(&self) -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(self.frames as f64 / 60.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: self.frames,
        }
    }

    /// One solve in its own command buffer; its GPU milliseconds.
    fn run(&mut self, water: &[bool], f: &[f32]) -> f64 {
        self.load(water, f);
        let mut enc = self.device.create_encoder("gpu-flip-pressure-solve");
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            let time = self.frame_time();
            self.exec.execute_frame_with_state(&mut self.graph, &self.plan, time, &mut gpu, &mut self.state, 0);
            self.frames += 1;
        }
        enc.commit_and_wait_profiled(&self.device).total_ms
    }

    /// One solve with a GPU timestamp per dispatch, each tagged with its step.
    fn profile(&mut self, water: &[bool], f: &[f32]) -> manifold_gpu::GpuFrameProfile {
        self.load(water, f);
        let sampler = self.device.create_timestamp_sampler(8192).expect("timestamp sampling");
        let mut enc = self.device.create_encoder("gpu-flip-pressure-profile");
        enc.enable_dispatch_profiling(sampler, &self.device);
        self.exec.set_profiling(true);
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            let time = self.frame_time();
            self.exec.execute_frame_with_state(&mut self.graph, &self.plan, time, &mut gpu, &mut self.state, 0);
            self.frames += 1;
        }
        self.exec.set_profiling(false);
        enc.commit_and_wait_profiled(&self.device)
    }

    fn pressure(&self) -> Vec<f32> {
        let cells = self.shape.cells();
        let backend = self.exec.backend();
        let buffer = backend.array_buffer(backend.slot_for(self.pressure).expect("pressure bound")).expect("pressure buffer");
        assert!(buffer.size as usize >= cells * 4);
        let ptr = buffer.mapped_ptr().expect("shared pressure buffer");
        // SAFETY: the frame completed; the buffer holds `cells` floats.
        unsafe { std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), cells) }.to_vec()
    }

    fn residual_of(&self, water: &[bool], f: &[f32]) -> f64 {
        residual(&self.pressure(), water, f, self.shape.n, self.shape.cell_size())
    }
}

/// The f64 reference's residual per problem at 64³: (frame, after 3
/// iterations, after 8), and the retired FFT solve's pinned f64 residual at
/// its shipped 24 passes.
const PINNED_64: [(u32, f64, f64, f64); 7] = [
    (0, 1.325e-02, 5.885e-07, 8.1700e-05),
    (15, 2.100e-02, 3.249e-06, 5.4525e-05),
    (30, 5.145e-02, 8.009e-06, 1.3805e-04),
    (45, 2.583e-02, 7.732e-06, 1.1544e-03),
    (60, 2.099e-02, 7.111e-06, 4.5739e-03),
    (90, 1.112e-02, 3.351e-06, 8.2168e-03),
    (120, 1.241e-02, 5.829e-06, 5.1982e-03),
];

/// The same problems refined to 128³.
const PINNED_128: [(u32, f64, f64, f64); 7] = [
    (0, 8.370e-03, 2.293e-07, 1.68e-03),
    (15, 2.544e-02, 3.689e-06, 2.16e-03),
    (30, 3.528e-02, 5.281e-06, 4.60e-03),
    (45, 2.702e-02, 3.320e-06, 1.34e-02),
    (60, 2.361e-02, 3.171e-06, 3.31e-02),
    (90, 1.209e-02, 2.266e-06, 4.49e-02),
    (120, 1.467e-02, 3.586e-06, 2.27e-02),
];

/// The residual f32 arithmetic holds on these problems: past it the f64
/// reference keeps falling and the GPU cannot follow.
const F32_FLOOR: f64 = 3e-5;

/// Solve every problem at `refine_by`× the fixture. At 3 iterations the GPU
/// runs the reference's algorithm step for step, so its residual is within
/// 10% of the f64 one. At the shipped 8 it is within 2× the reference or
/// under the f32 floor, and never worse than the retired FFT solve.
fn check_against_reference(refine_by: usize, pinned: &[(u32, f64, f64, f64)]) {
    let (n, problems) = load_problems();
    let mut three = Solver::new(PressureShape { iterations: 3, ..PressureShape::at(n * refine_by) });
    let mut shipped = Solver::new(PressureShape::at(n * refine_by));
    assert_eq!(shipped.shape.iterations, 8);
    let mut failures = Vec::new();
    for (problem, &(frame, at3, at8, fft)) in problems.iter().zip(pinned) {
        assert_eq!(problem.frame, frame);
        let problem = refine(problem, n, refine_by);
        three.run(&problem.water, &problem.f);
        let got3 = three.residual_of(&problem.water, &problem.f);
        shipped.run(&problem.water, &problem.f);
        let got8 = shipped.residual_of(&problem.water, &problem.f);
        let mut times: Vec<f64> = (0..5).map(|_| shipped.run(&problem.water, &problem.f)).collect();
        times.sort_by(f64::total_cmp);
        let again = shipped.residual_of(&problem.water, &problem.f);
        println!(
            "GPU FLIP {}³ frame {frame:3}: 3 iterations {got3:.3e} (f64 {at3:.3e}, {:.3}×); 8 iterations {got8:.3e} (f64 {at8:.3e}; FFT {fft:.3e}), {:.2} ms GPU per solve (median of 5)",
            shipped.shape.n,
            got3 / at3,
            times[2]
        );
        assert!((got8 - again).abs() <= 1e-3 * got8, "frame {frame}: repeat solves differ, {got8} then {again}");
        if !(got3 / at3 - 1.0).abs().lt(&0.1) {
            failures.push(format!("frame {frame}: 3 iterations {got3:.3e} against {at3:.3e}"));
        }
        if got8.is_nan() || got8 > (2.0 * at8).max(F32_FLOOR) || got8 > fft {
            failures.push(format!("frame {frame}: 8 iterations {got8:.3e} against {at8:.3e} (FFT {fft:.3e})"));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn gpu_flip_solve_matches_reference() {
    check_against_reference(1, &PINNED_64);
}

#[test]
fn gpu_flip_solve_matches_reference_refined() {
    check_against_reference(2, &PINNED_128);
}

/// Which stage of the solve a node belongs to.
fn stage_of(name: &str) -> &'static str {
    match name {
        _ if name.contains("_pre") || name.contains("_post") => "smooth",
        _ if name.ends_with("_residual") => "residual",
        _ if name.ends_with("_restrict") || name.ends_with("_prolong") => "transfer",
        _ if name.ends_with("_solve") => "coarse",
        "cg" | "rz" | "beta" | "direction" | "minus_lp" | "p_dot_s" | "alpha" | "solution" | "residual" => "vectors",
        _ => "setup",
    }
}

const STAGES: [&str; 6] = ["smooth", "residual", "transfer", "coarse", "vectors", "setup"];

/// Where one solve's GPU time goes on the largest Dam Break problem (frame
/// 90), after the GPU clock has ramped. Per-dispatch timing adds encoder
/// switches, so stages are reported as shares of the untimed solve.
fn stage_split(refine_by: usize) {
    let (n, problems) = load_problems();
    let problem = refine(problems.iter().find(|p| p.frame == 90).expect("frame 90"), n, refine_by);
    let mut solver = Solver::new(PressureShape::at(n * refine_by));
    for _ in 0..20 {
        solver.run(&problem.water, &problem.f);
    }
    let mut times: Vec<f64> = (0..7).map(|_| solver.run(&problem.water, &problem.f)).collect();
    times.sort_by(f64::total_cmp);
    let profile = solver.profile(&problem.water, &problem.f);
    assert_eq!(profile.failed_command_buffers, 0);
    let names: Vec<String> = solver
        .plan
        .steps()
        .iter()
        .map(|step| solver.graph.nodes().find(|n| n.id == step.node).map_or(String::new(), |n| n.node_id.as_str().to_string()))
        .collect();
    let step_of = |tag: &str| tag.rsplit_once(":s").and_then(|(_, idx)| idx.parse::<usize>().ok());
    let mut spans: Vec<_> = profile.spans.iter().collect();
    spans.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));
    let mut stages: Vec<(&str, f64)> = STAGES.iter().map(|&s| (s, 0.0)).collect();
    let mut end = 0.0_f64;
    for span in spans {
        let stage = step_of(&span.tag).map_or("setup", |idx| stage_of(&names[idx]));
        let charged = span.millis + (span.start_ms - end).max(0.0);
        end = end.max(span.start_ms + span.millis);
        stages.iter_mut().find(|(s, _)| *s == stage).expect("known stage").1 += charged;
    }
    let profiled: f64 = stages.iter().map(|(_, ms)| ms).sum();
    let solve = times[0];
    println!(
        "GPU FLIP {}³ solve: {solve:.3} ms GPU (fastest of 7; median {:.3}); {} spans, {} unattributed",
        solver.shape.n,
        times[3],
        profile.spans.len(),
        profile.overflow + profile.invalid
    );
    for (stage, ms) in &stages {
        println!("GPU FLIP {}³   {stage:9} {:5.1}%  {:.3} ms", solver.shape.n, 100.0 * ms / profiled, solve * ms / profiled);
    }
}

#[test]
fn gpu_flip_solve_stage_split() {
    stage_split(1);
}

#[test]
fn gpu_flip_solve_stage_split_refined() {
    stage_split(2);
}

/// Median and worst residual and the time per solve over the seven problems
/// at each iteration count of the trend.
fn iteration_trend(refine_by: usize) {
    let (n, problems) = load_problems();
    let problems: Vec<Problem> = problems.iter().map(|p| refine(p, n, refine_by)).collect();
    for iterations in TREND_ITERATIONS {
        let mut solver = Solver::new(PressureShape { iterations, ..PressureShape::at(n * refine_by) });
        let (mut residuals, mut times) = (Vec::new(), Vec::new());
        for problem in &problems {
            solver.run(&problem.water, &problem.f);
            residuals.push(solver.residual_of(&problem.water, &problem.f));
            // Back-to-back, fastest of the last few: the GPU clock ramps under load.
            times.push((0..8).map(|_| solver.run(&problem.water, &problem.f)).skip(3).fold(f64::INFINITY, f64::min));
        }
        residuals.sort_by(f64::total_cmp);
        times.sort_by(f64::total_cmp);
        println!(
            "GPU FLIP {}³ {iterations:2} iterations: median residual {:.2e}, worst {:.2e}, median {:.3} ms GPU per solve",
            solver.shape.n,
            residuals[3],
            residuals[6],
            times[3]
        );
    }
}

#[test]
fn gpu_flip_iteration_trend() {
    iteration_trend(1);
}

#[test]
fn gpu_flip_iteration_trend_refined() {
    iteration_trend(2);
}

/// Solves dumped from a running Dam Break (per record: u32 frame, step,
/// kind 0 main / 1 density, n; then water, f and the FFT solve's pressure,
/// n³ f32 each), read from the file `GPU_FLIP_DUMP` names: the GPU residual
/// at the shipped iterations beside the FFT pressure's. No-op unset, like the
/// other opt-in probes.
#[cfg(feature = "water-race-probes")]
#[test]
fn gpu_flip_real_frames_against_fft() {
    let Ok(path) = std::env::var("GPU_FLIP_DUMP") else {
        return;
    };
    let raw = std::fs::read(&path).expect("dump reads");
    let word = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().expect("four bytes")) as usize;
    let floats = |at: usize, count: usize| -> Vec<f32> {
        raw[at..at + 4 * count].chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect()
    };
    let mut solvers: Vec<Solver> = Vec::new();
    let mut at = 0;
    while at < raw.len() {
        let (frame, step, kind, n) = (word(at), word(at + 4), word(at + 8), word(at + 12));
        at += 16;
        let cells = n * n * n;
        let water: Vec<bool> = floats(at, cells).iter().map(|&w| w > 0.5).collect();
        let f: Vec<f32> = floats(at + 4 * cells, cells).iter().zip(&water).map(|(&f, &w)| if w { f } else { 0.0 }).collect();
        let fft = floats(at + 8 * cells, cells);
        at += 12 * cells;
        let iterations = if kind == 0 { super::gpu_flip_preset::PRESSURE_ITERATIONS } else { super::gpu_flip_preset::DENSITY_ITERATIONS };
        let shape = PressureShape { iterations, ..PressureShape::at(n) };
        let index = match solvers.iter().position(|s| s.shape.n == n && s.shape.iterations == iterations) {
            Some(index) => index,
            None => {
                solvers.push(Solver::new(shape));
                solvers.len() - 1
            }
        };
        let solver = &mut solvers[index];
        solver.run(&water, &f);
        let got = solver.residual_of(&water, &f);
        let old = residual(&fft, &water, &f, n, shape.cell_size());
        let kind = ["main", "density"][kind];
        println!("GPU FLIP real {n}³ frame {frame} step {step} {kind:7}: {iterations} iterations {got:.3e}, FFT {old:.3e}");
        assert!(got <= old, "{n}³ frame {frame} step {step} {kind}: {got:.3e} is worse than the FFT solve's {old:.3e}");
    }
}

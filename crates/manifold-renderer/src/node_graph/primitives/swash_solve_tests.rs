//! The FFT water pressure solve end to end on the seven saved Dam Break
//! problems (docs/FFT_WATER_SOLVER_DESIGN.md P1): the true residual of the
//! masked Poisson equation per problem against the f64 reference, and the
//! time per solve. The reference is `scripts/swash_reference.py`
//! (`--weights abs --sheets runs-1 --smooth 3`), numpy in f64.

use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuBuffer, GpuTextureFormat};

use super::swash_preset::{PressureShape, pressure_def};
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
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/swash_dambreak_problems.bin.zst");
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
fn residual(p: &[f32], problem: &Problem, n: usize, h: f64) -> f64 {
    let (mut miss, mut size) = (0.0, 0.0);
    for c in (0..n * n * n).filter(|&c| problem.water[c]) {
        let at = [c % n, (c / n) % n, c / (n * n)];
        let stride = [1, n, n * n];
        let centre = f64::from(p[c]);
        let mut sum = 0.0;
        for a in 0..3 {
            for next in [at[a].checked_sub(1), Some(at[a] + 1).filter(|&q| q < n)].into_iter().flatten() {
                let neighbour = c + next * stride[a] - at[a] * stride[a];
                let value = if problem.water[neighbour] { f64::from(p[neighbour]) } else { 0.0 };
                sum += value - centre;
            }
        }
        let f = f64::from(problem.f[c]);
        miss += (sum / (h * h) - f).powi(2);
        size += f * f;
    }
    (miss / size).sqrt()
}

fn collar_count(problem: &Problem, n: usize) -> usize {
    let wet = |x: i64, y: i64, z: i64| {
        let inside = [x, y, z].iter().all(|&v| (0..n as i64).contains(&v));
        inside && problem.water[x as usize + n * (y as usize + n * z as usize)]
    };
    (0..n * n * n)
        .filter(|&c| {
            let (x, y, z) = ((c % n) as i64, ((c / n) % n) as i64, (c / (n * n)) as i64);
            !problem.water[c]
                && [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)]
                    .iter()
                    .any(|&(dx, dy, dz)| wet(x + dx, y + dy, z + dz))
        })
        .count()
}

/// The solve graph compiled and bound once; problems are loaded between frames.
struct Solver {
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

pub(super) fn node_named(graph: &Graph, name: &str) -> NodeInstanceId {
    graph.nodes().find(|n| n.node_id.as_str() == name).map(|n| n.id).unwrap_or_else(|| panic!("no node {name}"))
}

pub(super) fn output_of(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    let step = plan.steps().iter().find(|s| s.node == node).expect("node compiled");
    step.outputs.iter().find(|(name, _)| *name == port).map(|&(_, r)| r).expect("output port")
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
        let pressure_node = node_named(&graph, "pressure");
        let pressure = output_of(&plan, pressure_node, "out");
        let mut exec = Executor::new(Box::new(backend));
        exec.set_dump_set(Some(std::iter::once(pressure_node).collect()));
        Self { device, graph, plan, exec, state: StateStore::new(), water, f, pressure, frames: 0 }
    }

    /// One solve in its own command buffer; its GPU milliseconds. The
    /// sources are written before every frame: the planner recycles their
    /// storage after their last reader, as it does for any upstream atom's
    /// output.
    fn run(&mut self, problem: &Problem) -> f64 {
        self.run_timed(problem).0
    }

    /// GPU milliseconds and CPU milliseconds spent encoding the solve.
    fn run_timed(&mut self, problem: &Problem) -> (f64, f64) {
        let water: Vec<f32> = problem.water.iter().map(|&w| f32::from(u8::from(w))).collect();
        // SAFETY: shared-storage buffers sized for the lattice (checked in
        // new); the previous frame has completed.
        unsafe {
            self.water.write(0, bytemuck::cast_slice(&water));
            self.f.write(0, bytemuck::cast_slice(&problem.f));
        }
        let mut enc = self.device.create_encoder("swash-pressure-solve");
        let cpu_ms;
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            let time = FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(self.frames as f64 / 60.0),
                delta: Seconds(1.0 / 60.0),
                frame_count: self.frames,
            };
            let start = std::time::Instant::now();
            self.exec.execute_frame_with_state(&mut self.graph, &self.plan, time, &mut gpu, &mut self.state, 0);
            cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
            self.frames += 1;
        }
        (enc.commit_and_wait_profiled(&self.device).total_ms, cpu_ms)
    }

    /// One solve with a GPU timestamp per dispatch, each tagged with its step.
    fn profile(&mut self, problem: &Problem) -> manifold_gpu::GpuFrameProfile {
        let water: Vec<f32> = problem.water.iter().map(|&w| f32::from(u8::from(w))).collect();
        // SAFETY: as in `run`.
        unsafe {
            self.water.write(0, bytemuck::cast_slice(&water));
            self.f.write(0, bytemuck::cast_slice(&problem.f));
        }
        let sampler = self.device.create_timestamp_sampler(4096).expect("timestamp sampling");
        let mut enc = self.device.create_encoder("swash-pressure-profile");
        enc.enable_dispatch_profiling(sampler, &self.device);
        self.exec.set_profiling(true);
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            let time = FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(self.frames as f64 / 60.0),
                delta: Seconds(1.0 / 60.0),
                frame_count: self.frames,
            };
            self.exec.execute_frame_with_state(&mut self.graph, &self.plan, time, &mut gpu, &mut self.state, 0);
            self.frames += 1;
        }
        self.exec.set_profiling(false);
        enc.commit_and_wait_profiled(&self.device)
    }

    fn pressure(&self, cells: usize) -> Vec<f32> {
        let backend = self.exec.backend();
        let buffer = backend.array_buffer(backend.slot_for(self.pressure).expect("pressure bound")).expect("pressure buffer");
        assert!(buffer.size as usize >= cells * 4);
        let ptr = buffer.mapped_ptr().expect("shared pressure buffer");
        // SAFETY: the frame completed; the buffer holds `cells` floats.
        unsafe { std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), cells) }.to_vec()
    }
}

/// True residual per problem at 24 passes, 64³, from swash_reference.py.
const PINNED_64: [(u32, f64); 7] = [
    (0, 8.1700e-05),
    (15, 5.4525e-05),
    (30, 1.3805e-04),
    (45, 1.1544e-03),
    (60, 4.5739e-03),
    (90, 8.2168e-03),
    (120, 5.1982e-03),
];

/// The same problems refined to 128³, 24 passes.
const PINNED_128: [(u32, f64); 7] = [
    (0, 1.68e-03),
    (15, 2.16e-03),
    (30, 4.60e-03),
    (45, 1.34e-02),
    (60, 3.31e-02),
    (90, 4.49e-02),
    (120, 2.27e-02),
];

/// Solve every problem at `refine`× the fixture, check each residual is
/// within 2× the pinned f64 number, and report the time per solve.
fn check_against_reference(refine_by: usize, pinned: &[(u32, f64)]) {
    let (n, problems) = load_problems();
    let shape = PressureShape::at(n * refine_by);
    let cells = shape.cells();
    let mut solver = Solver::new(shape);
    let mut failures = Vec::new();
    for (problem, &(frame, want)) in problems.iter().zip(pinned) {
        assert_eq!(problem.frame, frame);
        let problem = if refine_by > 1 { refine(problem, n, refine_by) } else { Problem { frame, water: problem.water.clone(), f: problem.f.clone() } };
        let collar = collar_count(&problem, shape.n);
        assert!(collar <= shape.capacity, "frame {frame}: collar {collar} over capacity {}", shape.capacity);
        solver.run(&problem);
        let got = residual(&solver.pressure(cells), &problem, shape.n, shape.cell_size());
        let mut times: Vec<f64> = (0..5).map(|_| solver.run(&problem)).collect();
        times.sort_by(f64::total_cmp);
        let got_again = residual(&solver.pressure(cells), &problem, shape.n, shape.cell_size());
        println!(
            "SWASH {}³ frame {frame:3}: collar {collar:6}, residual {got:.3e} (f64 {want:.3e}, {:.2}×), {:.2} ms GPU per solve (median of 5)",
            shape.n,
            got / want,
            times[2]
        );
        assert!((got - got_again).abs() <= 1e-3 * got, "frame {frame}: repeat solves differ, {got} then {got_again}");
        if got.is_nan() || got > 2.0 * want {
            failures.push(format!("frame {frame}: {got:.3e} against {want:.3e}"));
        }
    }
    assert!(failures.is_empty(), "residuals over 2× the f64 reference: {failures:?}");
}

/// Which part of the solve a node belongs to: one pass's helper, box solve or
/// Krylov update, or the work outside the passes.
fn part_of(name: &str) -> &'static str {
    match name {
        _ if name.starts_with("helper_") => "helper",
        _ if name.starts_with("pass_box_") => "box",
        "pass_source" | "sum_z" | "w" => "box",
        "h1" | "w1" | "h2" | "w2" | "norm" | "next" | "givens" | "krylov" => "krylov",
        _ => "outside",
    }
}

/// Where one solve's GPU time goes, on the largest Dam Break collar
/// (frame 90). Every dispatch carries a timestamp; the vendor FFT does not,
/// so the gap before the next timed dispatch is charged to that dispatch's
/// part, which is the FFT's own. Per-dispatch timing adds encoder switches,
/// so the parts are reported as shares of the untimed solve.
fn pass_split(refine_by: usize) {
    let (n, problems) = load_problems();
    let shape = PressureShape::at(n * refine_by);
    let problem = problems.iter().find(|p| p.frame == 90).expect("frame 90");
    let problem = if refine_by > 1 { refine(problem, n, refine_by) } else { Problem { frame: 90, water: problem.water.clone(), f: problem.f.clone() } };
    let mut solver = Solver::new(shape);
    // Back-to-back solves first, so the GPU clock has ramped as it has in a show.
    for _ in 0..20 {
        solver.run(&problem);
    }
    let (mut times, mut encode): (Vec<f64>, Vec<f64>) = (0..7).map(|_| solver.run_timed(&problem)).unzip();
    times.sort_by(f64::total_cmp);
    encode.sort_by(f64::total_cmp);
    let profile = solver.profile(&problem);
    assert_eq!(profile.failed_command_buffers, 0);
    let names: Vec<&str> = solver
        .plan
        .steps()
        .iter()
        .map(|step| solver.graph.nodes().find(|n| n.id == step.node).map_or("", |n| n.node_id.as_str()))
        .collect();
    let step_of = |tag: &str| tag.rsplit_once(":s").and_then(|(_, idx)| idx.parse::<usize>().ok());
    let mut spans: Vec<_> = profile.spans.iter().collect();
    spans.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));
    let mut parts: Vec<(&str, f64)> = ["helper", "box", "krylov", "outside"].iter().map(|&p| (p, 0.0)).collect();
    let mut end = 0.0_f64;
    for span in spans {
        let part = step_of(&span.tag).map_or("outside", |idx| part_of(names[idx]));
        let charged = span.millis + (span.start_ms - end).max(0.0);
        end = end.max(span.start_ms + span.millis);
        parts.iter_mut().find(|(p, _)| *p == part).expect("known part").1 += charged;
    }
    let profiled: f64 = parts.iter().map(|(_, ms)| ms).sum();
    let solve = times[0];
    println!(
        "SWASH {}³ solve: {solve:.2} ms GPU (fastest of 7; median {:.2}), {:.2} ms CPU to encode (median); timed run {:.2} ms, {} spans, {} unattributed",
        shape.n,
        times[3],
        encode[3],
        profile.total_ms,
        profile.spans.len(),
        profile.overflow + profile.invalid
    );
    for (part, ms) in &parts {
        let share = ms / profiled;
        let per_pass = if *part == "outside" { String::new() } else { format!(", {:.3} ms per pass", solve * share / shape.passes as f64) };
        println!("SWASH {}³   {part:8} {:5.1}% of the solve{per_pass}", shape.n, 100.0 * share);
    }
}

/// Median residual and time per solve over the seven problems at each pass
/// count of the trend (the f64 reference at 64³: 7.1e-2, 2.2e-2, 1.2e-3 and
/// 5.3e-5 at 12, 16, 24 and 32 passes).
fn pass_trend(refine_by: usize) {
    let (n, problems) = load_problems();
    let problems: Vec<Problem> = problems
        .iter()
        .map(|p| if refine_by > 1 { refine(p, n, refine_by) } else { Problem { frame: p.frame, water: p.water.clone(), f: p.f.clone() } })
        .collect();
    for passes in super::swash_preset::TREND_PASSES {
        let shape = PressureShape { passes, ..PressureShape::at(n * refine_by) };
        let mut solver = Solver::new(shape);
        let (mut residuals, mut times) = (Vec::new(), Vec::new());
        for problem in &problems {
            solver.run(problem);
            residuals.push(residual(&solver.pressure(shape.cells()), problem, shape.n, shape.cell_size()));
            // Back-to-back, fastest of the last few: the GPU clock ramps under load.
            let fastest = (0..8).map(|_| solver.run(problem)).skip(3).fold(f64::INFINITY, f64::min);
            times.push(fastest);
        }
        residuals.sort_by(f64::total_cmp);
        times.sort_by(f64::total_cmp);
        println!(
            "SWASH {}³ {passes:2} passes: median residual {:.2e}, worst {:.2e}, median {:.2} ms GPU per solve",
            shape.n,
            residuals[3],
            residuals[6],
            times[3]
        );
    }
}

#[test]
fn fft_water_pass_trend() {
    pass_trend(1);
}

#[test]
fn fft_water_pass_trend_refined() {
    pass_trend(2);
}

#[test]
fn fft_water_pass_cost_split() {
    pass_split(1);
}

#[test]
fn fft_water_pass_cost_split_refined() {
    pass_split(2);
}

#[test]
fn fft_water_matches_reference() {
    check_against_reference(1, &PINNED_64);
}

#[test]
fn fft_water_matches_reference_refined() {
    check_against_reference(2, &PINNED_128);
}

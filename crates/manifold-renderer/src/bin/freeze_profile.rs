//! `freeze-profile` — Phase 0 GPU profiling bench for the freeze/fusion
//! compiler initiative.
//!
//! Two passes:
//! - **Effects (texture domain):** build each stateless effect preset's
//!   graph, drive it through `Executor::execute_frame_with_gpu`, time per
//!   frame with `commit_and_wait_completed` (wall-time ≈ GPU time). Reports
//!   dispatch (step) count, ms/frame, and ms/step — the per-node texture
//!   round-trip cost a fusion pass collapses.
//! - **Generators (buffer/particle domain):** drive each generator preset
//!   through the production `Generator::render` path (state-aware), time
//!   per frame the same way. Reports ms/frame. This is where per-particle
//!   integrate chains live — the buffer-domain fusion target.
//!
//! Stateful effects (Bloom, Watercolor, Feedback, AutoGain, DoF, Wireframe)
//! need `execute_frame_with_state` and are omitted from the effect pass.
//! Each generator (preset, resolution) is wrapped in `catch_unwind` so one
//! failing preset can't abort an unattended run.
//!
//! Run: `cargo run --release -p manifold-renderer --bin freeze-profile`

use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::generator::Generator;
use manifold_renderer::generator_context::{GeneratorContext, MAX_GEN_PARAMS};
use manifold_renderer::generators::json_graph_generator::JsonGraphGenerator;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::primitives::Gain;
use manifold_renderer::node_graph::{
    EffectGraphDefExt, EffectNode, ExecutionPlan, Executor, FinalOutput, FrameTime, Graph,
    MetalBackend, NodeInstanceId, PrimitiveRegistry, ResourceId, Source, compile,
};
use manifold_renderer::render_target::RenderTarget;

const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;
const WARMUP: u32 = 8;
const FRAMES: u32 = 120;
const GEN_WARMUP: u32 = 5;
const GEN_FRAMES: u32 = 60;

const RESOLUTIONS: &[(u32, u32)] = &[(1920, 1080), (3840, 2160)];

/// Stateless effect presets, spanning pure-pointwise (ColorGrade,
/// Infrared) to UV-warp/boundary-heavy (Kaleidoscope, ChromaticAberration).
const PRESETS: &[&str] = &[
    "ColorGrade",
    "Infrared",
    "HdrBoost",
    "InvertColors",
    "Dither",
    "Glitch",
    "ChromaticAberration",
    "Kaleidoscope",
    "QuadMirror",
    "EdgeStretch",
    "VoronoiPrism",
];

/// Generator presets spanning particle sims (OilyFluid, FluidSimulation),
/// an array_math geometry gen (DigitalPlants), a cellular gen (StarField),
/// and a single-dispatch baseline (Plasma).
const GEN_PRESETS: &[&str] = &[
    "Plasma",
    "StarField",
    "DigitalPlants",
    "OilyFluid",
    "FluidSimulation",
];

/// Asset roots resolved against the crate manifest dir (baked in at compile
/// time), so the bench finds presets regardless of the shell's CWD — it can
/// be built/run from a worktree via `--manifest-path` and still read that
/// worktree's assets.
const EFFECT_PRESETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/effect-presets");
const GENERATOR_PRESETS_DIR: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets");

fn preset_path(name: &str) -> String {
    format!("{EFFECT_PRESETS_DIR}/{name}.json")
}

/// Replicates the parity harness helper: find the `ResourceId` a given
/// node's output port writes to, by scanning the compiled plan's steps.
fn resource_for_output(
    plan: &ExecutionPlan,
    node: NodeInstanceId,
    port: &str,
) -> Option<ResourceId> {
    for step in plan.steps() {
        if step.node == node {
            for &(name, id) in &step.outputs {
                if name == port {
                    return Some(id);
                }
            }
        }
    }
    None
}

fn main() {
    let registry = PrimitiveRegistry::with_builtin();
    let device = GpuDevice::new();
    let source_type_id = Source::new().type_id().as_str().to_string();

    println!(
        "freeze-profile — REAL GPU time (MTLCommandBuffer GPUStartTime/EndTime), avg over {FRAMES} frames\n"
    );
    println!("--- effects (texture domain) ---");
    println!(
        "{:<16} {:>6} {:>7} {:>11} {:>11}",
        "preset", "res", "steps", "ms/frame", "ms/step"
    );
    println!("{}", "-".repeat(54));

    for name in PRESETS {
        let path = preset_path(name);
        let bytes = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skip {name}: read {e}");
                continue;
            }
        };
        let def: EffectGraphDef = match serde_json::from_str(&bytes) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("skip {name}: parse {e}");
                continue;
            }
        };

        for &(w, h) in RESOLUTIONS {
            let mut graph = match def.clone().into_graph(&registry) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("skip {name}@{w}x{h}: build {e}");
                    continue;
                }
            };
            let plan = match compile(&graph) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("skip {name}@{w}x{h}: compile {e}");
                    continue;
                }
            };
            let steps = plan.steps().len();

            let Some(source_id) = graph
                .nodes()
                .find(|n| n.node.type_id().as_str() == source_type_id)
                .map(|n| n.id)
            else {
                eprintln!("skip {name}@{w}x{h}: no Source node");
                continue;
            };
            let Some(source_res) = resource_for_output(&plan, source_id, "out") else {
                eprintln!("skip {name}@{w}x{h}: Source.out unresolved");
                continue;
            };

            let input_rt = RenderTarget::new(&device, w, h, FORMAT, "freeze-profile-input");
            let mut backend = MetalBackend::new(&device, w, h, FORMAT);
            backend.pre_bind_texture_2d(source_res, input_rt);
            let mut exec = Executor::new(Box::new(backend));

            let frame_time = FrameTime {
                beats: Beats(1.0),
                seconds: Seconds(1.0),
                delta: Seconds(1.0 / 60.0),
                frame_count: 0,
            };

            for _ in 0..WARMUP {
                let mut enc = device.create_encoder("freeze-profile-warmup");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
                }
                enc.commit_and_wait_completed();
            }

            let mut gpu_secs = 0.0_f64;
            for _ in 0..FRAMES {
                let mut enc = device.create_encoder("freeze-profile-timed");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
                }
                gpu_secs += enc.commit_and_wait_completed_timed();
            }
            let ms_frame = gpu_secs * 1000.0 / f64::from(FRAMES);
            let ms_step = if steps > 0 {
                ms_frame / steps as f64
            } else {
                0.0
            };
            println!(
                "{:<16} {:>5}p {:>7} {:>11.3} {:>11.4}",
                name, h, steps, ms_frame, ms_step
            );
        }
    }

    println!("\nms/step ≈ per-dispatch texture round-trip cost a fusion pass collapses.");
    println!("Fusing a pointwise run of length L recovers ~(L-1) × ms/step per frame.");

    profile_synthetic_pointwise(&device);
    profile_generators(&registry, &device);
    profile_fluidsim_particle_sweep(&registry, &device);
}

/// Synthetic per-pass measurement: `Source → Gain×N → FinalOutput` at 4K,
/// real GPU time per N. N=1 is the true cost of ONE full-canvas pointwise
/// dispatch; the marginal (N→N+1) is what a fusion pass removes per
/// collapsed pointwise node. This is an *isolated, measured* per-pass cost
/// — not total ÷ step-count — and it sidesteps ColorGrade's branched
/// topology (it forks at `mix`, so linear prefix-truncation would be
/// ambiguous). Gain runs at its default (identity) but still does a full
/// read+math+write pass, which is exactly the bandwidth cost being measured.
fn profile_synthetic_pointwise(device: &GpuDevice) {
    let (w, h) = (3840u32, 2160u32);
    println!(
        "\n--- synthetic pointwise chains (Source → Gain×N → FinalOutput) @ {w}x{h}, real GPU time ---"
    );
    println!("{:<10} {:>11} {:>16}", "N passes", "ms/frame", "marginal/pass");
    println!("{}", "-".repeat(40));

    // Port names from a throwaway probe (avoid hardcoding "in"/"out").
    let probe = Gain::new();
    let in_port = probe.inputs()[0].name;
    let out_port = probe.outputs()[0].name;
    drop(probe);

    let mut prev = 0.0_f64;
    for n in 1..=6u32 {
        let mut graph = Graph::new();
        let source = graph.add_node(Box::new(Source::new()));
        let mut last = source;
        let mut last_out: &'static str = "out";
        for _ in 0..n {
            let g = graph.add_node(Box::new(Gain::new()));
            graph.connect((last, last_out), (g, in_port)).unwrap();
            last = g;
            last_out = out_port;
        }
        let fout = graph.add_node(Box::new(FinalOutput::new()));
        graph.connect((last, last_out), (fout, "in")).unwrap();

        let plan = compile(&graph).expect("synthetic graph compiles");
        let source_res =
            resource_for_output(&plan, source, "out").expect("Source.out resolves");
        let input_rt = RenderTarget::new(device, w, h, FORMAT, "syn-input");
        let mut backend = MetalBackend::new(device, w, h, FORMAT);
        backend.pre_bind_texture_2d(source_res, input_rt);
        let mut exec = Executor::new(Box::new(backend));
        let frame_time = FrameTime {
            beats: Beats(1.0),
            seconds: Seconds(1.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };

        for _ in 0..WARMUP {
            let mut enc = device.create_encoder("syn-warmup");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
            }
            enc.commit_and_wait_completed();
        }
        let mut gpu_secs = 0.0_f64;
        for _ in 0..FRAMES {
            let mut enc = device.create_encoder("syn-timed");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
            }
            gpu_secs += enc.commit_and_wait_completed_timed();
        }
        let ms = gpu_secs * 1000.0 / f64::from(FRAMES);
        let marginal = if n == 1 { ms } else { ms - prev };
        prev = ms;
        println!("{:<10} {:>11.4} {:>16.4}", n, ms, marginal);
    }
    println!("marginal/pass ≈ true cost of one full-canvas pointwise dispatch (what fusion removes).");
}

/// Profile generator presets (the BUFFER / particle domain) via the
/// production `Generator::render` path (state-aware), timing one frame
/// with `commit_and_wait_completed`. Each (preset, resolution) is wrapped
/// in `catch_unwind` so one failing preset can't abort the sweep.
fn profile_generators(registry: &PrimitiveRegistry, device: &GpuDevice) {
    println!("\n--- generators (buffer/particle domain), avg over {GEN_FRAMES} frames ---");
    println!("{:<16} {:>6} {:>11}", "generator", "res", "ms/frame");
    println!("{}", "-".repeat(36));

    for name in GEN_PRESETS {
        let path = format!("{GENERATOR_PRESETS_DIR}/{name}.json");
        let bytes = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skip {name}: read {e}");
                continue;
            }
        };
        let def: EffectGraphDef = match serde_json::from_str(&bytes) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("skip {name}: parse {e}");
                continue;
            }
        };

        for &(w, h) in RESOLUTIONS {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut generator = JsonGraphGenerator::from_def_with_device(
                    def.clone(),
                    registry,
                    device,
                    w,
                    h,
                    FORMAT,
                )
                .map_err(|e| e.to_string())?;
                let target = RenderTarget::new(device, w, h, FORMAT, "freeze-profile-gen");
                let mk_ctx = |t: f64| GeneratorContext {
                    time: t,
                    beat: t * 2.0,
                    dt: 1.0_f32 / 60.0,
                    width: w,
                    height: h,
                    output_width: w,
                    output_height: h,
                    aspect: w as f32 / h as f32,
                    anim_progress: 0.0,
                    trigger_count: 0,
                    params: [0.0; MAX_GEN_PARAMS],
                    param_count: 0,
                };

                for i in 0..GEN_WARMUP {
                    let mut enc = device.create_encoder("freeze-profile-gen-warmup");
                    {
                        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                        generator.render(&mut gpu, &target.texture, &mk_ctx(f64::from(i) / 60.0));
                    }
                    enc.commit_and_wait_completed();
                }

                let mut gpu_secs = 0.0_f64;
                for i in 0..GEN_FRAMES {
                    let mut enc = device.create_encoder("freeze-profile-gen-timed");
                    {
                        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                        generator.render(
                            &mut gpu,
                            &target.texture,
                            &mk_ctx(f64::from(GEN_WARMUP + i) / 60.0),
                        );
                    }
                    gpu_secs += enc.commit_and_wait_completed_timed();
                }
                Ok::<f64, String>(gpu_secs * 1000.0 / f64::from(GEN_FRAMES))
            }));

            match result {
                Ok(Ok(ms)) => println!("{:<16} {:>5}p {:>11.3}", name, h, ms),
                Ok(Err(e)) => println!("{:<16} {:>5}p   load-err: {e}", name, h),
                Err(_) => println!("{:<16} {:>5}p   FAILED (panicked)", name, h),
            }
        }
    }
}

/// FluidSim cost-decomposition sweep. FluidSimulation's per-frame GPU time
/// is the sum of two orthogonal workloads:
///   - **per-particle** (buffer domain, fusible): seed / noise-force /
///     sample / euler-integrate / wrap / anti-clump / scatter — all sized by
///     `active_count`, independent of canvas resolution.
///   - **per-pixel** (texture domain): 4× gaussian_blur, downsample,
///     resolve_accumulator, tonemap, final output — sized by canvas, flat in
///     particle count.
///
/// The earlier resolution sweep (1080p→2160p) isolates the per-pixel slope.
/// THIS sweep fixes resolution at 1080p and varies the particle-pool size.
/// NOTE: `active_count` is only a logical "alive" gate read inside the
/// kernels — the per-particle dispatches are sized by the pool CAPACITY
/// (`max_capacity` on the seed node), so sweeping `active_count` alone leaves
/// GPU time flat. We sweep `max_capacity` (and set `active_count` to match, so
/// the pool is full) from 1M up to the preset's 8M ceiling. The slope
/// ms/particle is the buffer-domain cost — the part buffer fusion targets; the
/// intercept (extrapolated to 0) is the texture-domain + fixed/per-dispatch
/// overhead. This decomposes the 11 ms by measurement, not inference, and
/// answers "does buffer fusion help FluidSim, or does it need a different
/// lever."
fn profile_fluidsim_particle_sweep(registry: &PrimitiveRegistry, device: &GpuDevice) {
    const COUNTS: &[i32] = &[1_000_000, 2_000_000, 4_000_000, 8_000_000];
    let (w, h) = (1920u32, 1080u32);

    println!(
        "\n--- FluidSim pool-capacity sweep @ {w}x{h} (resolution fixed; max_capacity + \
         active_count tracked together), avg over {GEN_FRAMES} frames ---"
    );
    println!("{:<14} {:>11} {:>16}", "pool size", "ms/frame", "ns/particle");
    println!("{}", "-".repeat(44));

    let path = format!("{GENERATOR_PRESETS_DIR}/FluidSimulation.json");
    let bytes = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("skip fluidsim sweep: read {e}");
            return;
        }
    };
    let base_def: EffectGraphDef = match serde_json::from_str(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skip fluidsim sweep: parse {e}");
            return;
        }
    };

    let mut points: Vec<(f64, f64)> = Vec::new(); // (count, ms)
    for &count in COUNTS {
        // Track pool capacity AND active count together, so the dispatch grid
        // (sized by `max_capacity`) and the alive-gate (`active_count`) both
        // scale — a full pool at every sweep point.
        let mut def = base_def.clone();
        for node in &mut def.nodes {
            if node.params.contains_key("active_count") {
                node.params
                    .insert("active_count".to_string(), SerializedParamValue::Int { value: count });
            }
            if node.params.contains_key("max_capacity") {
                node.params
                    .insert("max_capacity".to_string(), SerializedParamValue::Int { value: count });
            }
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut generator =
                JsonGraphGenerator::from_def_with_device(def, registry, device, w, h, FORMAT)
                    .map_err(|e| e.to_string())?;
            let target = RenderTarget::new(device, w, h, FORMAT, "fluidsweep-gen");
            let mk_ctx = |t: f64| GeneratorContext {
                time: t,
                beat: t * 2.0,
                dt: 1.0_f32 / 60.0,
                width: w,
                height: h,
                output_width: w,
                output_height: h,
                aspect: w as f32 / h as f32,
                anim_progress: 0.0,
                trigger_count: 0,
                params: [0.0; MAX_GEN_PARAMS],
                param_count: 0,
            };

            for i in 0..GEN_WARMUP {
                let mut enc = device.create_encoder("fluidsweep-warmup");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                    generator.render(&mut gpu, &target.texture, &mk_ctx(f64::from(i) / 60.0));
                }
                enc.commit_and_wait_completed();
            }
            let mut gpu_secs = 0.0_f64;
            for i in 0..GEN_FRAMES {
                let mut enc = device.create_encoder("fluidsweep-timed");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &mk_ctx(f64::from(GEN_WARMUP + i) / 60.0),
                    );
                }
                gpu_secs += enc.commit_and_wait_completed_timed();
            }
            Ok::<f64, String>(gpu_secs * 1000.0 / f64::from(GEN_FRAMES))
        }));

        match result {
            Ok(Ok(ms)) => {
                let ns_per = ms * 1e6 / f64::from(count);
                println!("{:<14} {:>11.3} {:>16.4}", count, ms, ns_per);
                points.push((f64::from(count), ms));
            }
            Ok(Err(e)) => println!("{:<14}   load-err: {e}", count),
            Err(_) => println!("{:<14}   FAILED (panicked)", count),
        }
    }

    // Least-squares fit ms = slope·count + intercept across the sweep.
    // slope = per-particle GPU cost (buffer-domain, fusible);
    // intercept = texture-domain + fixed overhead at this resolution.
    if points.len() >= 2 {
        let n = points.len() as f64;
        let sx: f64 = points.iter().map(|p| p.0).sum();
        let sy: f64 = points.iter().map(|p| p.1).sum();
        let sxx: f64 = points.iter().map(|p| p.0 * p.0).sum();
        let sxy: f64 = points.iter().map(|p| p.0 * p.1).sum();
        let denom = n * sxx - sx * sx;
        if denom.abs() > f64::EPSILON {
            let slope = (n * sxy - sx * sy) / denom;
            let intercept = (sy - slope * sx) / n;
            let ceiling = 8_000_000.0_f64; // preset's shipped pool size
            let at_ceiling = slope * ceiling + intercept;
            let particle_share = if at_ceiling > 0.0 {
                (slope * ceiling) / at_ceiling * 100.0
            } else {
                0.0
            };
            println!("\nlinear fit @ {w}x{h}:");
            println!("  per-particle slope : {:.4} ns/particle", slope * 1e6);
            println!(
                "  intercept (0 particles, texture-domain + fixed/per-dispatch) : {:.3} ms",
                intercept
            );
            println!(
                "  @ 8,000,000 (shipped pool): {:.3} ms total, of which ~{:.0}% is per-particle (buffer-domain, fusible)",
                at_ceiling, particle_share
            );
        }
    }
}

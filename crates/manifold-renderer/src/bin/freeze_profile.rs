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

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_core::params::ParamManifest;
use manifold_renderer::preset_context::PresetContext;
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
    "HighlightBoost",
    "Invert",
    "Dither",
    "Glitch",
    "ChromaticAberration",
    "Kaleidoscope",
    "QuadMirror",
    "EdgeStretch",
    "VoronoiPrism",
];

/// Generator presets spanning particle sims (OilyFluid, FluidSim2D),
/// an array_math geometry gen (DigitalPlants), a cellular gen (StarField),
/// and a single-dispatch baseline (Plasma).
const GEN_PRESETS: &[&str] = &[
    "Plasma",
    "StarField",
    "DigitalPlants",
    "OilyFluid",
    "FluidSim2D",
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
    let device = std::sync::Arc::new(GpuDevice::new());

    // `attribute [names…]` → per-node GPU/CPU attribution via counter
    // sampling (fast path, skips the sweeps).
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("attribute") {
        let names: Vec<&str> = args[1..].iter().map(String::as_str).collect();
        profile_attribution(&registry, &device, &names);
        return;
    }
    // `poolstats` → synthetic reproduction of the texture-pool canvas-change leak
    // + the evict_resolution_mismatch fix, with before/after bytes. Headless, so
    // it can't drive the real Liveschool fixture (that's MANIFOLD_POOL_STATS=1 on
    // the app); this demonstrates the mechanism and the reclaim on a known
    // working set. The pool report is the lasting diagnostic either path surfaces.
    if args.first().map(String::as_str) == Some("poolstats") {
        profile_pool_stats(&device);
        return;
    }
    // `scene [glb-path] [param-substr]` → BUG-035 measurement gate: drive a
    // real imported glTF scene through the production PresetRuntime::render
    // path, static params vs one card param swept per frame (the LFO case),
    // and report per-frame CPU encode wall time vs real GPU time — series
    // stats, not just averages, so a sawtooth is distinguishable from a
    // steady cost.
    if args.first().map(String::as_str) == Some("scene") {
        let rest: Vec<&str> = args[1..].iter().map(String::as_str).collect();
        profile_scene(&registry, &device, &rest);
        return;
    }

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
            let mut backend = MetalBackend::new(std::sync::Arc::clone(&device), w, h, FORMAT);
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

    // `perdispatch` arg → run ONLY the per-dispatch breakdown (fast iteration).
    if std::env::args().any(|a| a == "perdispatch") {
        profile_per_dispatch(&registry, &device);
        return;
    }
    // `reconcile` arg → diagnose the FluidSim 0.49ms(prefix) vs 11.7ms(prod) gap.
    if std::env::args().any(|a| a == "reconcile") {
        reconcile_fluidsim(&registry, &device);
        return;
    }

    profile_synthetic_pointwise(&device);
    profile_fused_colorgrade(&registry, &device);
    profile_auto_fused_colorgrade(&registry, &device);
    profile_generators(&registry, &device);
    profile_fluidsim_particle_sweep(&registry, &device);
    profile_synthetic_buffer(&device);
    profile_per_dispatch(&registry, &device);
}

/// Reconcile the FluidSim anomaly: the per-dispatch prefix sweep sums to
/// ~0.5ms, but production (`profile_generators` / the particle sweep) reads
/// ~11.7ms. Same def, same 1080p, same `execute_frame_with_state` underneath.
/// We vary the two things that differ — warmup count (state/particle pool
/// accumulation) and the build path (raw `into_graph` vs `PresetRuntime`) — to
/// localize the 11ms.
fn reconcile_fluidsim(registry: &PrimitiveRegistry, device: &std::sync::Arc<GpuDevice>) {
    use manifold_renderer::node_graph::StateStore;
    let (w, h) = (1920u32, 1080u32);
    let path = format!("{GENERATOR_PRESETS_DIR}/FluidSim2D.json");
    let def: EffectGraphDef = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    println!("=== reconcile FluidSim2D @ {w}x{h} ===\n");

    // A) Raw executor, FULL plan, accumulating state, vary warmup. If the cost
    //    climbs with warmup -> it's particle-pool/state accumulation in the
    //    graph. If it stays ~0.5ms -> the 11ms is NOT in the graph dispatches.
    println!("--- A) raw executor, full plan, accumulating state, vary warmup ---");
    println!("{:<10} {:>11} {:>11}", "warmup", "no-prealloc", "prealloc");
    for &warm in &[5u32, 30, 120] {
      let mut row = [0.0f64; 2];
      for (col, prealloc) in [false, true].into_iter().enumerate() {
        let mut graph = def.clone().into_graph(registry).unwrap();
        let plan = compile(&graph).unwrap();
        let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FORMAT);
        if prealloc {
            manifold_renderer::node_graph::pre_allocate_resources(&graph, &plan, device, &mut backend).unwrap();
        }
        let mut exec = Executor::new(Box::new(backend));
        let mut state = StateStore::new();
        let ft = FrameTime { beats: Beats(1.0), seconds: Seconds(1.0), delta: Seconds(1.0/60.0), frame_count: 0 };
        for i in 0..warm {
            let mut enc = device.create_encoder("rec-warm");
            { let mut gpu = RendererGpuEncoder::new(&mut enc, device);
              // advance time so animated seeding evolves
              let ft = FrameTime { seconds: Seconds(f64::from(i)/60.0), beats: Beats(f64::from(i)/30.0), ..ft };
              exec.execute_frame_with_state(&mut graph, &plan, ft, &mut gpu, &mut state, 0); }
            enc.commit_and_wait_completed();
        }
        let mut secs = 0.0;
        for i in 0..30u32 {
            let mut enc = device.create_encoder("rec-timed");
            { let mut gpu = RendererGpuEncoder::new(&mut enc, device);
              let ft = FrameTime { seconds: Seconds(f64::from(warm+i)/60.0), beats: Beats(f64::from(warm+i)/30.0), ..ft };
              exec.execute_frame_with_state(&mut graph, &plan, ft, &mut gpu, &mut state, 0); }
            secs += enc.commit_and_wait_completed_timed();
        }
        row[col] = secs * 1000.0 / 30.0;
      }
      println!("{:<10} {:>11.4} {:>11.4}", warm, row[0], row[1]);
    }

    // B) Production path: PresetRuntime::from_def_with_device + render, exactly
    //    like profile_generators (the 11.7ms number).
    println!("\n--- B) PresetRuntime::render (production path) ---");
    {
        let mut generator = PresetRuntime::from_def_with_device(def.clone(), registry, std::sync::Arc::clone(device), w, h, FORMAT, None).unwrap();
        let target = RenderTarget::new(device, w, h, FORMAT, "rec-prod");
        let mk = |t: f64| PresetContext { time: t, beat: t*2.0, dt: 1.0/60.0, width: w, height: h, output_width: w, output_height: h, aspect: w as f32 / h as f32, owner_key: 0, is_clip_level: false, frame_count: 0, anim_progress: 0.0, trigger_count: 0 };
        let params = ParamManifest::default();
        for i in 0..30 { let mut enc = device.create_encoder("rec-prod-warm"); { let mut gpu = RendererGpuEncoder::new(&mut enc, device); generator.render(&mut gpu, &target.texture, &mk(f64::from(i)/60.0), &params); } enc.commit_and_wait_completed(); }
        let mut secs = 0.0;
        for i in 0..30u32 { let mut enc = device.create_encoder("rec-prod-timed"); { let mut gpu = RendererGpuEncoder::new(&mut enc, device); generator.render(&mut gpu, &target.texture, &mk(f64::from(30+i)/60.0), &params); } secs += enc.commit_and_wait_completed_timed(); }
        println!("PresetRuntime::render: {:.4} ms/frame", secs * 1000.0 / 30.0);
    }
}

/// Per-dispatch GPU-time breakdown for the heavy generators — the measurement
/// that answers "why does a dozen fixed-grid dispatches cost FluidSim 11.7ms:
/// is it one pathological step, per-dispatch overhead, or real bandwidth?"
///
/// Method: compile the generator's plan, then for each prefix length `k =
/// 1..=N` run the REAL executor on `plan.truncated(k)` (forcing all steps live
/// so the prefix executes exactly steps `[0..k]`), timing real GPU time. The
/// marginal `time[k] - time[k-1]` is step `k`'s added cost. This reuses the
/// production executor — real cross-dispatch overlap, real stateful late-capture
/// — rather than a hand-rolled per-step fork that could drift from the live
/// path. Marginals can be small/negative where the scheduler overlaps adjacent
/// dispatches (flagged); the cumulative curve and the per-type rollup are the
/// robust signals.
fn profile_per_dispatch(registry: &PrimitiveRegistry, device: &std::sync::Arc<GpuDevice>) {
    use manifold_renderer::node_graph::StateStore;
    use std::collections::BTreeMap;

    const GENS: &[&str] = &["FluidSim2D", "OilyFluid", "MetallicGlass"];
    const PROF_WARMUP: u32 = 5;
    const PROF_FRAMES: u32 = 30;
    let (w, h) = (1920u32, 1080u32);
    let gen_input_type = "system.generator_input";

    // Time one truncated plan against a freshly built graph (fresh state), pre-
    // binding a black input to the generator-input boundary when it's live.
    let time_prefix = |def: &EffectGraphDef, k: usize| -> Option<f64> {
        let mut graph = def.clone().into_graph(registry).ok()?;
        let full = compile(&graph).ok()?;
        let plan = full.truncated(k);
        let input_res = graph
            .nodes()
            .find(|n| n.node.type_id().as_str() == gen_input_type)
            .map(|n| n.id)
            .and_then(|id| resource_for_output(&full, id, "out"));

        let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FORMAT);
        if let Some(res) = input_res {
            backend.pre_bind_texture_2d(res, RenderTarget::new(device, w, h, FORMAT, "pd-input"));
        }
        // Allocate the full-size Array (particle) + Texture3D buffers, exactly
        // like the production generator path — without this the particle
        // dispatches run on empty buffers and read as ~free.
        manifold_renderer::node_graph::pre_allocate_resources(&graph, &full, device, &mut backend)
            .ok()?;
        let mut exec = Executor::new(Box::new(backend));
        exec.set_profile_force_all_live(true);
        let mut state = StateStore::new();
        let ft = FrameTime {
            beats: Beats(1.0),
            seconds: Seconds(1.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        for _ in 0..PROF_WARMUP {
            let mut enc = device.create_encoder("pd-warmup");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                exec.execute_frame_with_state(&mut graph, &plan, ft, &mut gpu, &mut state, 0);
            }
            enc.commit_and_wait_completed();
        }
        let mut secs = 0.0_f64;
        for _ in 0..PROF_FRAMES {
            let mut enc = device.create_encoder("pd-timed");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                exec.execute_frame_with_state(&mut graph, &plan, ft, &mut gpu, &mut state, 0);
            }
            secs += enc.commit_and_wait_completed_timed();
        }
        Some(secs * 1000.0 / f64::from(PROF_FRAMES))
    };

    for name in GENS {
        let path = format!("{GENERATOR_PRESETS_DIR}/{name}.json");
        let Ok(bytes) = std::fs::read_to_string(&path) else {
            eprintln!("skip {name}: read");
            continue;
        };
        let Ok(def) = serde_json::from_str::<EffectGraphDef>(&bytes) else {
            eprintln!("skip {name}: parse");
            continue;
        };
        // Step → node-type label from one canonical build.
        let Ok(graph0) = def.clone().into_graph(registry) else {
            eprintln!("skip {name}: build");
            continue;
        };
        let Ok(plan0) = compile(&graph0) else {
            eprintln!("skip {name}: compile");
            continue;
        };
        let n = plan0.steps().len();
        let labels: Vec<String> = plan0
            .steps()
            .iter()
            .map(|s| {
                graph0
                    .get_node(s.node)
                    .map(|inst| inst.node.type_id().as_str().to_string())
                    .unwrap_or_else(|| "?".to_string())
            })
            .collect();

        println!(
            "\n=== per-dispatch breakdown: {name} @ {w}x{h} ({n} steps), avg over {PROF_FRAMES} frames ==="
        );
        println!("{:>4} {:<34} {:>11} {:>11}", "step", "node type", "marginal", "cumulative");
        println!("{}", "-".repeat(64));

        let mut prev = 0.0_f64;
        let mut per_type: BTreeMap<String, f64> = BTreeMap::new();
        for k in 1..=n {
            let Some(ms) = time_prefix(&def, k) else {
                eprintln!("  step {k}: timing failed");
                continue;
            };
            let marginal = ms - prev;
            prev = ms;
            let ty = &labels[k - 1];
            *per_type.entry(ty.clone()).or_default() += marginal.max(0.0);
            let flag = if marginal < 0.0 { " (overlap/noise)" } else { "" };
            println!("{:>4} {:<34} {:>10.4} {:>11.4}{}", k, ty, marginal, ms, flag);
        }
        println!("  full-frame cumulative: {prev:.3} ms");

        // Rollup: which node TYPES own the frame (sum of positive marginals).
        let mut rolled: Vec<(String, f64)> = per_type.into_iter().collect();
        rolled.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        println!("  --- cost by node type (sum of positive marginals) ---");
        for (ty, ms) in rolled.iter().take(12) {
            let pct = if prev > 0.0 { ms / prev * 100.0 } else { 0.0 };
            println!("  {:<34} {:>9.4} ms  {:>5.1}%", ty, ms, pct);
        }
    }
}

/// Buffer-domain fusion BREAK-EVEN (the analog of profile_synthetic_pointwise +
/// profile_fused_colorgrade for the particle lane). A storage buffer of M
/// particles (vec4) is stepped by a representative per-particle op (a force +
/// integrate-ish block: 3 trig + FMAs). We time the SAME op chain two ways:
///   - separate: N in-place dispatches of the 1-op kernel (Metal hazard-tracks
///     the shared buffer, so the N dispatches serialise — each reads what the
///     prior wrote, exactly like a real particle pipeline's stages);
///   - fused: ONE dispatch of an N-op kernel (read particle once, N ops in
///     registers, write once).
///
/// The question this answers (design §11.E / Phase-0): buffer chains are
/// IN-PLACE ALIASED, so unlike textures there is no fresh-VRAM round-trip to
/// eliminate — fusion only saves the (N-1) re-reads/re-writes of the SAME
/// buffer + per-dispatch overhead, and risks lower occupancy from register
/// pressure. If speedup stays ~flat, buffer fusion does NOT pay (build a
/// different lever); if it climbs with N, it does. MEASURED, not inferred.
fn profile_synthetic_buffer(device: &GpuDevice) {
    use manifold_gpu::GpuBinding;

    const M: u64 = 2_000_000; // particles
    let buf_bytes = M * 16; // vec4<f32> each
    println!(
        "\n--- synthetic buffer chains ({M} particles, vec4, in-place): N separate dispatches \
         vs 1 fused kernel, real GPU time ---"
    );
    println!("{:<8} {:>13} {:>13} {:>10}", "N ops", "separate ms", "fused ms", "speedup");
    println!("{}", "-".repeat(48));

    // Representative per-particle step (force + integrate): non-trivial compute so
    // the bench isn't purely memory-bound (which would overstate the fusion win).
    let op_block = "    p = p * 1.001 + vec4<f32>(0.0001, 0.0001, 0.0001, 0.0);\n\
                        p.x = p.x + sin(p.y * 6.28318) * 0.01;\n\
                        p.y = p.y + cos(p.z * 6.28318) * 0.01;\n\
                        p.z = p.z + sin(p.x * 3.14159) * 0.01;\n";

    let make_kernel = |n: u32| -> String {
        let mut body = String::new();
        for _ in 0..n {
            body.push_str(op_block);
        }
        format!(
            "@group(0) @binding(0) var<storage, read_write> particles: array<vec4<f32>>;\n\
@compute @workgroup_size(256)\n\
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{\n\
    let i = gid.x;\n\
    if i >= arrayLength(&particles) {{ return; }}\n\
    var p = particles[i];\n\
{body}    particles[i] = p;\n\
}}\n"
        )
    };

    let groups = (M as u32).div_ceil(256);
    let buffer = device.create_buffer(buf_bytes);
    let step_pipe = device.create_compute_pipeline(&make_kernel(1), "cs_main", "syn-buf-step");

    for n in 1..=6u32 {
        // separate: N in-place dispatches of the 1-op kernel.
        for _ in 0..WARMUP {
            let mut enc = device.create_encoder("syn-buf-sep-warmup");
            for _ in 0..n {
                enc.dispatch_compute(
                    &step_pipe,
                    &[GpuBinding::Buffer { binding: 0, buffer: &buffer, offset: 0 }],
                    [groups, 1, 1],
                    "syn-buf-step",
                );
            }
            enc.commit_and_wait_completed();
        }
        let mut sep_secs = 0.0_f64;
        for _ in 0..FRAMES {
            let mut enc = device.create_encoder("syn-buf-sep-timed");
            for _ in 0..n {
                enc.dispatch_compute(
                    &step_pipe,
                    &[GpuBinding::Buffer { binding: 0, buffer: &buffer, offset: 0 }],
                    [groups, 1, 1],
                    "syn-buf-step",
                );
            }
            sep_secs += enc.commit_and_wait_completed_timed();
        }
        let sep_ms = sep_secs * 1000.0 / f64::from(FRAMES);

        // fused: ONE dispatch of the N-op kernel.
        let fused_pipe =
            device.create_compute_pipeline(&make_kernel(n), "cs_main", "syn-buf-fused");
        for _ in 0..WARMUP {
            let mut enc = device.create_encoder("syn-buf-fused-warmup");
            enc.dispatch_compute(
                &fused_pipe,
                &[GpuBinding::Buffer { binding: 0, buffer: &buffer, offset: 0 }],
                [groups, 1, 1],
                "syn-buf-fused",
            );
            enc.commit_and_wait_completed();
        }
        let mut fus_secs = 0.0_f64;
        for _ in 0..FRAMES {
            let mut enc = device.create_encoder("syn-buf-fused-timed");
            enc.dispatch_compute(
                &fused_pipe,
                &[GpuBinding::Buffer { binding: 0, buffer: &buffer, offset: 0 }],
                [groups, 1, 1],
                "syn-buf-fused",
            );
            fus_secs += enc.commit_and_wait_completed_timed();
        }
        let fus_ms = fus_secs * 1000.0 / f64::from(FRAMES);

        let speedup = if fus_ms > 0.0 { sep_ms / fus_ms } else { 0.0 };
        println!("{:<8} {:>13.4} {:>13.4} {:>9.2}x", n, sep_ms, fus_ms, speedup);
    }
    println!(
        "speedup climbing with N → buffer fusion pays (saves re-reads/writes + dispatch overhead); \
         ~flat → in-place aliasing leaves no round-trip to remove (per design Phase-0)."
    );
}

/// BUG-035 measurement gate: is the per-frame cost of an ANIMATED 3D scene
/// CPU encode (incremental command encoding would fix it), GPU render
/// (optimize the render instead), or spiky/sawtooth (pool churn or
/// scheduling — different fix again)?
///
/// Drives the production import door (`assemble_import_graph`) + production
/// `PresetRuntime::render`, two arms A/B:
///   - **static**: card params untouched every frame (time still advances,
///     exactly like a playing clip with no LFO);
///   - **animated**: ONE card param (default `cam_orbit`) swept every frame
///     through the manifest, exactly what an LFO binding does.
///
/// Per frame we record the CPU wall time of `render()` (= full param apply +
/// uniform rebuild + command encoding; commit excluded) and the real GPU
/// time (`commit_and_wait_completed_timed`). Reports p50/p95/max for both
/// arms plus a spike census, so a steady delta and a sawtooth read
/// differently. Fresh runtime per arm (no state bleed).
fn profile_scene(registry: &PrimitiveRegistry, device: &std::sync::Arc<GpuDevice>, args: &[&str]) {
    use manifold_core::params::Param;
    use manifold_renderer::node_graph::gltf_import::assemble_import_graph;

    const SCENE_WARMUP: u32 = 60;
    let scene_frames: usize =
        args.get(2).and_then(|s| s.parse().ok()).unwrap_or(300);
    let default_glb = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb"
    );
    let glb = args.first().copied().unwrap_or(default_glb);
    let param_pref = args.get(1).copied().unwrap_or("cam_orbit");

    let (def, _report) = match assemble_import_graph(std::path::Path::new(glb)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("scene: import failed for {glb}: {e}");
            return;
        }
    };
    let Some(metadata) = def.preset_metadata.clone() else {
        eprintln!("scene: imported def carries no metadata/card params");
        return;
    };
    let manifest_base = ParamManifest::from_params(
        metadata.params.iter().cloned().map(Param::bundled).collect(),
    );
    let swept_id = manifest_base
        .iter()
        .find(|p| p.id().contains(param_pref))
        .map(|p| p.id().to_string());
    let Some(swept_id) = swept_id else {
        eprintln!(
            "scene: no card param matching '{param_pref}' (have: {})",
            manifest_base.iter().map(|p| p.id().to_string()).collect::<Vec<_>>().join(", ")
        );
        return;
    };

    println!(
        "=== BUG-035 scene bench: {} ({} nodes), sweeping '{swept_id}' in the animated arm ===",
        glb.rsplit('/').next().unwrap_or(glb),
        def.nodes.len(),
    );

    // Raw f16 readback: fraction of non-black pixels + the raw bytes, via a
    // separate encoder (same technique as gltf_import's PNG-faithfulness
    // test). The scene converges ASYNCHRONOUSLY (background texture decode),
    // so callers must loop until non-black before trusting any number.
    let read_frame = |target: &RenderTarget, w: u32, h: u32| -> (f64, Vec<u8>) {
        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let buf = device.create_buffer_shared(total_bytes);
        let mut enc = device.create_encoder("scene-readback");
        enc.copy_texture_to_buffer(&target.texture, &buf, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared readback");
        let bytes: Vec<u8> = unsafe {
            std::slice::from_raw_parts(ptr.cast::<u8>(), (w * h * 8) as usize).to_vec()
        };
        let halves: &[u16] = unsafe {
            std::slice::from_raw_parts(bytes.as_ptr().cast::<u16>(), (w * h * 4) as usize)
        };
        let non_black = halves
            .chunks_exact(4)
            .filter(|px| px[0] != 0 || px[1] != 0 || px[2] != 0)
            .count();
        (non_black as f64 / f64::from(w * h), bytes)
    };

    // Sweep sanity: prove the swept param actually reaches the GPU — render
    // at param-min until the scene converges (non-black), then compare
    // against param-MID (not max: for angle params the extremes can be the
    // same pose — orbit −180° == +180°). Zero differing bytes = the animated
    // arm is a no-op and its numbers are void.
    {
        let (w, h) = (640u32, 360u32);
        let mut generator =
            match PresetRuntime::from_def_with_device(def.clone(), registry, std::sync::Arc::clone(device), w, h, FORMAT, None)
            {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("scene: sanity runtime build failed: {e}");
                    return;
                }
            };
        let target = RenderTarget::new(device, w, h, FORMAT, "scene-sanity");
        let mut params = ParamManifest::from_params(
            metadata.params.iter().cloned().map(Param::bundled).collect(),
        );
        let (pmin, pmax) = {
            let p = params.get(&swept_id).unwrap();
            (p.spec.min, p.spec.max)
        };
        let mut shot = |params: &ParamManifest, frame: u32| {
            let mut enc = device.create_encoder("scene-sanity");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                let ctx = PresetContext {
                    time: f64::from(frame) / 60.0,
                    beat: f64::from(frame) / 30.0,
                    dt: 1.0 / 60.0,
                    width: w,
                    height: h,
                    output_width: w,
                    output_height: h,
                    aspect: w as f32 / h as f32,
                    owner_key: 0,
                    is_clip_level: false,
                    frame_count: 0,
                    anim_progress: 0.0,
                    trigger_count: 0,
                };
                generator.render(&mut gpu, &target.texture, &ctx, params);
            }
            enc.commit_and_wait_completed();
        };
        params.get_mut(&swept_id).unwrap().value = pmin;
        let mut fraction = 0.0f64;
        let mut converged_at = None;
        for i in 0..200u32 {
            shot(&params, i);
            let (f, _) = read_frame(&target, w, h);
            fraction = f;
            if f > 0.02 {
                converged_at = Some(i);
                break;
            }
        }
        let Some(converged_at) = converged_at else {
            println!(
                "sweep sanity: scene NEVER converged (non-black {fraction:.4} after 200 \
                 frames) — ALL numbers below are an empty-scene render, void"
            );
            return;
        };
        let (_, a) = read_frame(&target, w, h);
        params.get_mut(&swept_id).unwrap().value = (pmin + pmax) * 0.5;
        shot(&params, converged_at + 1);
        let (_, b) = read_frame(&target, w, h);
        let differing = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count();
        println!(
            "sweep sanity @640x360: converged frame {converged_at} (non-black {fraction:.3}); \
             '{swept_id}' min→mid changes {differing}/{} bytes {}",
            a.len(),
            if differing == 0 {
                "— SWEEP IS A NO-OP, animated arm void"
            } else {
                "(sweep reaches the GPU)"
            },
        );
        if scene_frames == 0 {
            return; // sanity-only run
        }
    }

    for &(w, h) in RESOLUTIONS {
        for animated in [false, true] {
            let mut generator = match PresetRuntime::from_def_with_device(
                def.clone(),
                registry,
                std::sync::Arc::clone(device),
                w,
                h,
                FORMAT,
                None,
            ) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("scene@{w}x{h}: runtime build failed: {e}");
                    return;
                }
            };
            let target = RenderTarget::new(device, w, h, FORMAT, "scene-bench");
            let mut params = manifest_base.clone();
            let (sweep_min, sweep_max) = {
                let p = params.get(&swept_id).unwrap();
                (p.spec.min, p.spec.max)
            };
            let mk = |i: u32| PresetContext {
                time: f64::from(i) / 60.0,
                beat: f64::from(i) / 30.0,
                dt: 1.0 / 60.0,
                width: w,
                height: h,
                output_width: w,
                output_height: h,
                aspect: w as f32 / h as f32,
                owner_key: 0,
                is_clip_level: false,
                frame_count: 0,
                anim_progress: 0.0,
                trigger_count: 0,
            };

            // Warm until the scene actually renders content (async texture
            // decode) — an empty-scene measurement is void.
            let mut warm_frames = 0u32;
            let mut warm_fraction;
            loop {
                for _ in 0..SCENE_WARMUP {
                    let mut enc = device.create_encoder("scene-warmup");
                    {
                        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                        generator.render(&mut gpu, &target.texture, &mk(warm_frames), &params);
                    }
                    enc.commit_and_wait_completed();
                    warm_frames += 1;
                }
                let (f, _) = read_frame(&target, w, h);
                warm_fraction = f;
                if f > 0.02 || warm_frames >= 600 {
                    break;
                }
            }
            if warm_fraction <= 0.02 {
                println!(
                    "{w}x{h}: scene never converged in {warm_frames} warmup frames \
                     (non-black {warm_fraction:.4}) — skipping this arm"
                );
                continue;
            }

            let mut cpu_ms = vec![0.0f64; scene_frames];
            let mut gpu_ms = vec![0.0f64; scene_frames];
            for i in 0..scene_frames {
                if animated {
                    // 0.25 Hz sinusoidal sweep across the param's full range —
                    // the shape an LFO binding produces.
                    let phase = (i as f64 / 60.0) * 0.25 * std::f64::consts::TAU;
                    let t = 0.5 + 0.5 * phase.sin();
                    let p = params.get_mut(&swept_id).unwrap();
                    p.value = sweep_min + (sweep_max - sweep_min) * t as f32;
                }
                let mut enc = device.create_encoder("scene-timed");
                let t0 = std::time::Instant::now();
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &mk(SCENE_WARMUP + i as u32),
                        &params,
                    );
                }
                cpu_ms[i] = t0.elapsed().as_secs_f64() * 1000.0;
                gpu_ms[i] = enc.commit_and_wait_completed_timed() * 1000.0;
            }

            let stats = |xs: &[f64]| {
                let mut s = xs.to_vec();
                s.sort_by(|a, b| a.partial_cmp(b).unwrap());
                (s[s.len() / 2], s[s.len() * 95 / 100], s[s.len() - 1])
            };
            let (c50, c95, cmax) = stats(&cpu_ms);
            let (g50, g95, gmax) = stats(&gpu_ms);
            let arm = if animated { "animated" } else { "static  " };
            println!(
                "{w}x{h} {arm}  cpu-encode p50 {c50:>7.3} p95 {c95:>7.3} max {cmax:>7.3} ms | \
                 gpu p50 {g50:>7.3} p95 {g95:>7.3} max {gmax:>7.3} ms"
            );
            // Full spike census (CPU > 1ms, ~20× the p50) with indices, so
            // periodicity is visible: an every-Nth-frame sawtooth reads
            // differently from one-off stragglers.
            let spikes: Vec<String> = (0..scene_frames)
                .filter(|&i| cpu_ms[i] > 1.0)
                .map(|i| format!("f{i}: {:.2}+{:.2}", cpu_ms[i], gpu_ms[i]))
                .collect();
            println!(
                "    cpu spikes >1ms: {} of {scene_frames} — {}",
                spikes.len(),
                spikes.iter().take(24).cloned().collect::<Vec<_>>().join("  "),
            );
        }
    }
    println!(
        "\nReading: animated-vs-static CPU delta ≫ GPU delta → re-encode/param-apply is the \
         cost (incremental encoding pays). GPU delta dominates → the render itself is the \
         cost. High p95/max over p50 → sawtooth (pool churn / allocation), not steady encode."
    );
}

/// Synthetic reproduction of the texture-pool canvas-change leak and the
/// `evict_resolution_mismatch` fix. Acquires + releases a realistic 4K working
/// set into the pool, then simulates a canvas change to 1080p (a fresh 1080p
/// working set) WITHOUT eviction — the report then shows the dead 4K entries
/// sitting alongside the live 1080p ones (the bug). Then applies the fix and
/// reports the bytes reclaimed. Real-fixture numbers come from
/// `MANIFOLD_POOL_STATS=1` on the running app; this is the headless mechanism +
/// before/after I can produce without the GUI.
fn profile_pool_stats(device: &GpuDevice) {
    use manifold_gpu::GpuTextureUsage;

    let pool = device.create_texture_pool(3);
    let usage = GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ;

    // A representative 4K working set: a handful of full-res rgba16float
    // intermediates (the compositor + a fused chain's scratch) plus a couple of
    // half-res HDR intermediates. Acquire then release so they land in the free
    // pool exactly as a real frame leaves them.
    let acquire_release = |w: u32, h: u32, fmt: GpuTextureFormat, n: usize| {
        let mut held = Vec::with_capacity(n);
        for _ in 0..n {
            held.push(pool.acquire(w, h, fmt, usage, "poolstats"));
        }
        for t in held {
            pool.release(t);
        }
    };

    println!("\n--- TexturePool: synthetic canvas-change leak + evict fix ---");
    pool.begin_frame();
    acquire_release(3840, 2160, GpuTextureFormat::Rgba16Float, 16);
    acquire_release(1920, 1080, GpuTextureFormat::Rgba16Float, 8);
    println!("\n[1] After rendering at 4K (the live working set):");
    print!("{}", pool.report());
    let bytes_4k = pool.cached_bytes();

    // Canvas change to 1080p. begin_frame so the new entries are recyclable; the
    // OLD 4K entries can never be (acquire now keys on 1080p) — they're dead.
    pool.begin_frame();
    acquire_release(1920, 1080, GpuTextureFormat::Rgba16Float, 16);
    acquire_release(960, 540, GpuTextureFormat::Rgba16Float, 8);
    println!("\n[2] After canvas change to 1080p, BEFORE the fix (4K entries now dead):");
    print!("{}", pool.report());
    let bytes_leaked = pool.cached_bytes();

    let (freed_n, freed_bytes) = pool.evict_resolution_mismatch(1920, 1080);
    println!("\n[3] After evict_resolution_mismatch(1920x1080): freed {freed_n} textures");
    print!("{}", pool.report());

    println!(
        "\n  4K live set: {:.1} MiB | leaked after change: {:.1} MiB | reclaimed: {:.1} MiB ({:.0}%)",
        bytes_4k as f64 / (1024.0 * 1024.0),
        bytes_leaked as f64 / (1024.0 * 1024.0),
        freed_bytes as f64 / (1024.0 * 1024.0),
        100.0 * freed_bytes as f64 / bytes_leaked.max(1) as f64,
    );
}

/// The PRODUCTION number: time the unfused ColorGrade graph against the
/// **auto-generated** fused graph the install path actually ships
/// ([`fused_view_by_id`] → `node.wgsl_compute` carrying the codegen kernel),
/// BOTH driven through the real `Executor` — so this includes the WgslCompute
/// introspection + dispatch overhead the live chain pays, not the bare
/// hand-kernel dispatch `profile_fused_colorgrade` measures. This is the speedup
/// that lands on screen.
fn profile_auto_fused_colorgrade(registry: &PrimitiveRegistry, device: &std::sync::Arc<GpuDevice>) {
    use manifold_renderer::node_graph::freeze::install::fused_view_by_id;

    println!(
        "\n--- ColorGrade: unfused graph vs AUTO-fused graph, both via executor (production path) ---"
    );
    println!("{:<8} {:>13} {:>13} {:>10}", "res", "unfused ms", "fused ms", "speedup");
    println!("{}", "-".repeat(48));

    let source_type_id = Source::new().type_id().as_str().to_string();
    let json = match std::fs::read_to_string(format!("{EFFECT_PRESETS_DIR}/ColorGrade.json")) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("skip auto-fused-colorgrade: read {e}");
            return;
        }
    };
    let unfused_def: EffectGraphDef = match serde_json::from_str(&json) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skip auto-fused-colorgrade: parse {e}");
            return;
        }
    };
    let Some(fused_view) = fused_view_by_id(&PresetTypeId::new("ColorGrade")) else {
        eprintln!("skip auto-fused-colorgrade: ColorGrade has no fused view");
        return;
    };

    // Time one def through the executor at (w, h): warmup, then avg real GPU
    // time over FRAMES. Returns None if the graph can't be built/compiled.
    let time_def = |def: &EffectGraphDef, w: u32, h: u32, label: &str| -> Option<f64> {
        let mut graph = def.clone().into_graph(registry).ok()?;
        let plan = compile(&graph).ok()?;
        let source_id = graph
            .nodes()
            .find(|n| n.node.type_id().as_str() == source_type_id)
            .map(|n| n.id)?;
        let source_res = resource_for_output(&plan, source_id, "out")?;
        let input_rt = RenderTarget::new(device, w, h, FORMAT, "auto-cg-input");
        let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FORMAT);
        backend.pre_bind_texture_2d(source_res, input_rt);
        let mut exec = Executor::new(Box::new(backend));
        let frame_time = FrameTime {
            beats: Beats(1.0),
            seconds: Seconds(1.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        for _ in 0..WARMUP {
            let mut enc = device.create_encoder("auto-cg-warmup");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
            }
            enc.commit_and_wait_completed();
        }
        let mut secs = 0.0_f64;
        for _ in 0..FRAMES {
            let mut enc = device.create_encoder(label);
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
            }
            secs += enc.commit_and_wait_completed_timed();
        }
        Some(secs * 1000.0 / f64::from(FRAMES))
    };

    for &(w, h) in RESOLUTIONS {
        let unfused_ms = match time_def(&unfused_def, w, h, "auto-cg-unfused-timed") {
            Some(ms) => ms,
            None => {
                eprintln!("skip auto-fused-colorgrade@{w}x{h}: unfused build");
                continue;
            }
        };
        let fused_ms = match time_def(&fused_view.canonical_def, w, h, "auto-cg-fused-timed") {
            Some(ms) => ms,
            None => {
                eprintln!("skip auto-fused-colorgrade@{w}x{h}: fused build");
                continue;
            }
        };
        let speedup = if fused_ms > 0.0 { unfused_ms / fused_ms } else { 0.0 };
        println!("{:>5}p {:>13.3} {:>13.3} {:>9.2}x", h, unfused_ms, fused_ms, speedup);
    }
}

/// The headline fusion number: time the SHIPPED ColorGrade preset (unfused, 9
/// graph steps) against the hand-fused single kernel
/// ([`reference::dispatch_fused_colorgrade`]), both as real GPU time on the
/// same run. This answers the §11.E question the synthetic per-pass number
/// can't: the unfused baseline gets the GPU's free cross-dispatch overlap that
/// fusion forfeits, so the real speedup may sit below the naive
/// steps-×-ms/step projection. Measured, not projected.
fn profile_fused_colorgrade(registry: &PrimitiveRegistry, device: &std::sync::Arc<GpuDevice>) {
    use manifold_renderer::node_graph::freeze::reference::{
        ColorGradeParams, colorgrade_pipeline, dispatch_fused_colorgrade,
    };

    println!(
        "\n--- ColorGrade: unfused graph (9 steps) vs hand-fused 1 kernel, real GPU time ---"
    );
    println!("{:<8} {:>13} {:>13} {:>10}", "res", "unfused ms", "fused ms", "speedup");
    println!("{}", "-".repeat(48));

    let source_type_id = Source::new().type_id().as_str().to_string();
    let json = match std::fs::read_to_string(format!("{EFFECT_PRESETS_DIR}/ColorGrade.json")) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("skip fused-colorgrade: read {e}");
            return;
        }
    };
    let def: EffectGraphDef = match serde_json::from_str(&json) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skip fused-colorgrade: parse {e}");
            return;
        }
    };
    let pipeline = colorgrade_pipeline(device);
    // Non-trivial params so the chain does real work (timing is essentially
    // value-independent, but this is representative).
    let params = ColorGradeParams {
        gain: 1.15,
        sat_s: 1.3,
        hue_deg: 25.0,
        sat_h: 1.2,
        val_h: 1.0,
        contrast: 1.2,
        col_amount: 0.4,
        col_hue: 210.0,
        col_sat: 0.8,
        col_focus: 0.6,
        mix_amount: 1.0,
        mix_mode: 0,
        clamp_min: 0.0,
        clamp_max: 65000.0,
        _pad0: 0.0,
        _pad1: 0.0,
    };
    let frame_time = FrameTime {
        beats: Beats(1.0),
        seconds: Seconds(1.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    };

    for &(w, h) in RESOLUTIONS {
        // --- unfused: the shipped graph through the executor ---
        let mut graph = match def.clone().into_graph(registry) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("skip fused-colorgrade@{w}x{h}: build {e}");
                continue;
            }
        };
        let plan = match compile(&graph) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("skip fused-colorgrade@{w}x{h}: compile {e}");
                continue;
            }
        };
        let Some(source_id) = graph
            .nodes()
            .find(|n| n.node.type_id().as_str() == source_type_id)
            .map(|n| n.id)
        else {
            eprintln!("skip fused-colorgrade@{w}x{h}: no Source");
            continue;
        };
        let Some(source_res) = resource_for_output(&plan, source_id, "out") else {
            continue;
        };
        let input_rt = RenderTarget::new(device, w, h, FORMAT, "cg-unfused-input");
        let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FORMAT);
        backend.pre_bind_texture_2d(source_res, input_rt);
        let mut exec = Executor::new(Box::new(backend));
        for _ in 0..WARMUP {
            let mut enc = device.create_encoder("cg-unfused-warmup");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
            }
            enc.commit_and_wait_completed();
        }
        let mut un_secs = 0.0_f64;
        for _ in 0..FRAMES {
            let mut enc = device.create_encoder("cg-unfused-timed");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
            }
            un_secs += enc.commit_and_wait_completed_timed();
        }
        let unfused_ms = un_secs * 1000.0 / f64::from(FRAMES);

        // --- fused: one kernel (input content is irrelevant to timing) ---
        let input = RenderTarget::new(device, w, h, FORMAT, "cg-fused-input");
        let output = RenderTarget::new(device, w, h, FORMAT, "cg-fused-output");
        for _ in 0..WARMUP {
            let mut enc = device.create_encoder("cg-fused-warmup");
            dispatch_fused_colorgrade(&mut enc, &pipeline, &input.texture, &output.texture, &params);
            enc.commit_and_wait_completed();
        }
        let mut f_secs = 0.0_f64;
        for _ in 0..FRAMES {
            let mut enc = device.create_encoder("cg-fused-timed");
            dispatch_fused_colorgrade(&mut enc, &pipeline, &input.texture, &output.texture, &params);
            f_secs += enc.commit_and_wait_completed_timed();
        }
        let fused_ms = f_secs * 1000.0 / f64::from(FRAMES);

        let speedup = if fused_ms > 0.0 { unfused_ms / fused_ms } else { 0.0 };
        println!("{:>5}p {:>13.3} {:>13.3} {:>9.2}x", h, unfused_ms, fused_ms, speedup);
    }
}

/// Per-node attribution via Metal counter sampling: every dispatch /
/// render pass / blit in a frame gets boundary GPU timestamps, tagged with
/// the executor step that encoded it, plus the step's CPU encode cost. The
/// measurement that replaces the slow prefix-truncation method — exact
/// per-step GPU time inside ONE command buffer, for both the unfused def and
/// (when one exists) the fused def.
///
/// Caveat: profiled frames split encoders per dispatch (Apple silicon only
/// samples at stage boundaries), so absolute ms run slightly above
/// production; the per-node SHARES are the signal. Work the spans can't see
/// (MPS/MetalFX internal encoders) shows as "unattributed".
fn profile_attribution(registry: &PrimitiveRegistry, device: &std::sync::Arc<GpuDevice>, names: &[&str]) {
    use manifold_renderer::node_graph::freeze::install;

    const DEFAULT_NAMES: &[&str] = &[
        "FluidSim2D",
        "OilyFluid",
        "ParticleText",
        "MetallicGlass",
        "Glitch",
        "Bloom",
    ];
    let names: Vec<&str> = if names.is_empty() {
        DEFAULT_NAMES.to_vec()
    } else {
        names.to_vec()
    };

    let Some(sampler) = device.create_timestamp_sampler(2048) else {
        eprintln!("attribute: device does not support GPU counter sampling");
        return;
    };
    println!(
        "=== per-node attribution @ 1920x1080 (counter-sampled GPU spans + CPU encode cost) ==="
    );
    println!(
        "profiled frames split encoders per dispatch — absolute ms run a little above \
         production; per-node shares are the signal.\n"
    );

    for name in &names {
        let gen_path = format!("{GENERATOR_PRESETS_DIR}/{name}.json");
        let eff_path = format!("{EFFECT_PRESETS_DIR}/{name}.json");
        let (path, is_gen) = if std::path::Path::new(&gen_path).exists() {
            (gen_path, true)
        } else {
            (eff_path, false)
        };
        let Ok(bytes) = std::fs::read_to_string(&path) else {
            eprintln!("skip {name}: no preset JSON in generator/effect dirs");
            continue;
        };
        let def: EffectGraphDef = match serde_json::from_str(&bytes) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("skip {name}: parse {e}");
                continue;
            }
        };

        attribute_def(registry, device, &sampler, &def, &format!("{name} — unfused"));

        if is_gen {
            match install::fused_generator_def_for(&def) {
                Some(fused) => {
                    attribute_def(registry, device, &sampler, &fused, &format!("{name} — fused"));
                }
                None => println!("{name} — fused: no fusable region (renders unfused)\n"),
            }
        } else {
            // `PresetTypeId::new` wants `&'static str`; leaking a few CLI
            // names in a profiling bin is fine.
            let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
            match install::fused_view_by_id(&PresetTypeId::new(static_name)) {
                Some(view) => attribute_def(
                    registry,
                    device,
                    &sampler,
                    &view.canonical_def,
                    &format!("{name} — fused"),
                ),
                None => println!("{name} — fused: no fusable region (renders unfused)\n"),
            }
        }
    }
}

/// Drive one def through the real executor with per-dispatch profiling and
/// print the per-step table. Generators and effects both run through
/// `execute_frame_with_state` with their boundary input pre-bound.
fn attribute_def(
    registry: &PrimitiveRegistry,
    device: &std::sync::Arc<GpuDevice>,
    sampler: &manifold_gpu::GpuTimestampSampler,
    def: &EffectGraphDef,
    title: &str,
) {
    use manifold_renderer::node_graph::StateStore;
    use std::collections::BTreeMap;

    const ATTR_WARMUP: u32 = 8;
    const ATTR_FRAMES: u32 = 30;
    let (w, h) = (1920u32, 1080u32);

    let mut graph = match def.clone().into_graph(registry) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{title}: build failed: {e}");
            return;
        }
    };
    let plan = match compile(&graph) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{title}: compile failed: {e}");
            return;
        }
    };

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FORMAT);
    for boundary in ["system.source", "system.generator_input"] {
        if let Some(id) = graph
            .nodes()
            .find(|n| n.node.type_id().as_str() == boundary)
            .map(|n| n.id)
            && let Some(res) = resource_for_output(&plan, id, "out")
        {
            backend
                .pre_bind_texture_2d(res, RenderTarget::new(device, w, h, FORMAT, "attr-input"));
        }
    }
    // Full-size Array/Texture3D allocation, like the production generator
    // path — without it particle dispatches run on empty buffers.
    let _ = manifold_renderer::node_graph::pre_allocate_resources(
        &graph,
        &plan,
        device,
        &mut backend,
    );
    let mut exec = Executor::new(Box::new(backend));
    let mut state = StateStore::new();
    let ft = |i: u32| FrameTime {
        seconds: Seconds(f64::from(i) / 60.0),
        beats: Beats(f64::from(i) / 30.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: i64::from(i),
    };

    for i in 0..ATTR_WARMUP {
        let mut enc = device.create_encoder("attr-warmup");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(&mut graph, &plan, ft(i), &mut gpu, &mut state, 0);
        }
        enc.commit_and_wait_completed();
    }

    #[derive(Default)]
    struct Acc {
        type_id: String,
        gpu_ms: f64,
        cpu_ns: u64,
        dispatches: u64,
    }
    let mut per_step: BTreeMap<usize, Acc> = BTreeMap::new();
    let mut total_ms = 0.0_f64;
    let mut unattributed_ms = 0.0_f64;
    let mut untagged_ms = 0.0_f64;
    let mut overflow = 0usize;

    for i in 0..ATTR_FRAMES {
        let mut enc = device.create_encoder("attr-timed");
        enc.enable_dispatch_profiling(sampler.clone(), device);
        exec.set_profiling(true);
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(
                &mut graph,
                &plan,
                ft(ATTR_WARMUP + i),
                &mut gpu,
                &mut state,
                0,
            );
        }
        let profile = enc.commit_and_wait_profiled(device);
        total_ms += profile.total_ms;
        unattributed_ms += profile.total_ms - profile.attributed_ms();
        overflow += profile.overflow;
        for span in &profile.spans {
            match span.tag.strip_prefix('s').and_then(|s| s.parse::<usize>().ok()) {
                Some(idx) => {
                    let acc = per_step.entry(idx).or_default();
                    acc.gpu_ms += span.millis;
                    acc.dispatches += 1;
                }
                None => untagged_ms += span.millis,
            }
        }
        for sp in exec.take_step_profiles() {
            let acc = per_step.entry(sp.step_idx).or_default();
            acc.cpu_ns += sp.cpu_nanos;
            acc.type_id = sp.type_id;
        }
    }

    let frames = f64::from(ATTR_FRAMES);
    let cpu_total_us: f64 =
        per_step.values().map(|a| a.cpu_ns as f64).sum::<f64>() / frames / 1000.0;
    println!(
        "--- {title}: {:.3} ms/frame GPU ({} steps), {:.1} µs/frame CPU encode ---",
        total_ms / frames,
        plan.steps().len(),
        cpu_total_us,
    );
    if overflow > 0 {
        println!("    WARNING: {overflow} dispatches ran unprofiled (sample buffer full)");
    }

    let mut rows: Vec<(usize, Acc)> = per_step.into_iter().collect();
    rows.sort_by(|a, b| b.1.gpu_ms.partial_cmp(&a.1.gpu_ms).unwrap_or(std::cmp::Ordering::Equal));
    println!(
        "{:>5} {:<38} {:>9} {:>6} {:>9} {:>6}",
        "step", "node type", "gpu ms", "%gpu", "cpu µs", "disp"
    );
    let grand_gpu: f64 = rows.iter().map(|(_, a)| a.gpu_ms).sum();
    let mut shown_gpu = 0.0_f64;
    for (rank, (idx, acc)) in rows.iter().enumerate() {
        // Top 24 rows covers every real preset; tail collapses below.
        if rank >= 24 {
            let rest_gpu = (grand_gpu - shown_gpu) / frames;
            println!("      … {} more steps, {rest_gpu:.4} ms", rows.len() - rank);
            break;
        }
        shown_gpu += acc.gpu_ms;
        let gpu_ms = acc.gpu_ms / frames;
        let pct = if grand_gpu > 0.0 { acc.gpu_ms / grand_gpu * 100.0 } else { 0.0 };
        println!(
            "{:>5} {:<38} {:>9.4} {:>5.1}% {:>9.1} {:>6}",
            idx,
            acc.type_id,
            gpu_ms,
            pct,
            acc.cpu_ns as f64 / frames / 1000.0,
            acc.dispatches / u64::from(ATTR_FRAMES),
        );
    }

    // Rollup by node type — which vocabulary owns the frame.
    let mut by_type: BTreeMap<String, f64> = BTreeMap::new();
    for (_, acc) in &rows {
        *by_type.entry(acc.type_id.clone()).or_default() += acc.gpu_ms;
    }
    let mut rolled: Vec<(String, f64)> = by_type.into_iter().collect();
    rolled.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    println!("    --- by node type ---");
    for (ty, ms) in rolled.iter().take(8) {
        let pct = if grand_gpu > 0.0 { ms / grand_gpu * 100.0 } else { 0.0 };
        println!("    {:<38} {:>9.4} ms {:>5.1}%", ty, ms / frames, pct);
    }
    println!(
        "    untagged (outside steps): {:.4} ms   unattributed (MPS/uncovered): {:.4} ms\n",
        untagged_ms / frames,
        unattributed_ms / frames,
    );
}

/// Synthetic per-pass measurement: `Source → Gain×N → FinalOutput` at 4K,
/// real GPU time per N. N=1 is the true cost of ONE full-canvas pointwise
/// dispatch; the marginal (N→N+1) is what a fusion pass removes per
/// collapsed pointwise node. This is an *isolated, measured* per-pass cost
/// — not total ÷ step-count — and it sidesteps ColorGrade's branched
/// topology (it forks at `mix`, so linear prefix-truncation would be
/// ambiguous). Gain runs at its default (identity) but still does a full
/// read+math+write pass, which is exactly the bandwidth cost being measured.
fn profile_synthetic_pointwise(device: &std::sync::Arc<GpuDevice>) {
    let (w, h) = (3840u32, 2160u32);
    println!(
        "\n--- synthetic pointwise chains (Source → Gain×N → FinalOutput) @ {w}x{h}, real GPU time ---"
    );
    println!("{:<10} {:>11} {:>16}", "N passes", "ms/frame", "marginal/pass");
    println!("{}", "-".repeat(40));

    // Port names from a throwaway probe (avoid hardcoding "in"/"out").
    let probe = Gain::new();
    let in_port: &'static str = manifold_renderer::node_graph::intern_name(&probe.inputs()[0].name);
    let out_port: &'static str =
        manifold_renderer::node_graph::intern_name(&probe.outputs()[0].name);
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
        let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FORMAT);
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
fn profile_generators(registry: &PrimitiveRegistry, device: &std::sync::Arc<GpuDevice>) {
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
                let mut generator = PresetRuntime::from_def_with_device(
                    def.clone(),
                    registry,
                    std::sync::Arc::clone(device),
                    w,
                    h,
                    FORMAT,
                    None,
                )
                .map_err(|e| e.to_string())?;
                let target = RenderTarget::new(device, w, h, FORMAT, "freeze-profile-gen");
                let mk_ctx = |t: f64| PresetContext {
                    time: t,
                    beat: t * 2.0,
                    dt: 1.0_f32 / 60.0,
                    width: w,
                    height: h,
                    output_width: w,
                    output_height: h,
                    aspect: w as f32 / h as f32,
                    owner_key: 0,
                    is_clip_level: false,
                    frame_count: 0,
                    anim_progress: 0.0,
                    trigger_count: 0,
                };

                for i in 0..GEN_WARMUP {
                    let mut enc = device.create_encoder("freeze-profile-gen-warmup");
                    {
                        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                        generator.render(&mut gpu, &target.texture, &mk_ctx(f64::from(i) / 60.0), &ParamManifest::default());
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
                            &ParamManifest::default(),
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

/// FluidSim cost-decomposition sweep. FluidSim2D's per-frame GPU time
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
fn profile_fluidsim_particle_sweep(registry: &PrimitiveRegistry, device: &std::sync::Arc<GpuDevice>) {
    const COUNTS: &[i32] = &[1_000_000, 2_000_000, 4_000_000, 8_000_000];
    let (w, h) = (1920u32, 1080u32);

    println!(
        "\n--- FluidSim pool-capacity sweep @ {w}x{h} (resolution fixed; max_capacity + \
         active_count tracked together), avg over {GEN_FRAMES} frames ---"
    );
    println!("{:<14} {:>11} {:>16}", "pool size", "ms/frame", "ns/particle");
    println!("{}", "-".repeat(44));

    let path = format!("{GENERATOR_PRESETS_DIR}/FluidSim2D.json");
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
                PresetRuntime::from_def_with_device(def, registry, std::sync::Arc::clone(device), w, h, FORMAT, None)
                    .map_err(|e| e.to_string())?;
            let target = RenderTarget::new(device, w, h, FORMAT, "fluidsweep-gen");
            let mk_ctx = |t: f64| PresetContext {
                time: t,
                beat: t * 2.0,
                dt: 1.0_f32 / 60.0,
                width: w,
                height: h,
                output_width: w,
                output_height: h,
                aspect: w as f32 / h as f32,
                owner_key: 0,
                is_clip_level: false,
                frame_count: 0,
                anim_progress: 0.0,
                trigger_count: 0,
            };

            for i in 0..GEN_WARMUP {
                let mut enc = device.create_encoder("fluidsweep-warmup");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, device);
                    generator.render(&mut gpu, &target.texture, &mk_ctx(f64::from(i) / 60.0), &ParamManifest::default());
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
                        &ParamManifest::default(),
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

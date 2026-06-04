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

use manifold_core::EffectTypeId;
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
    profile_fused_colorgrade(&registry, &device);
    profile_auto_fused_colorgrade(&registry, &device);
    profile_perf_gate(&device);
    profile_generators(&registry, &device);
    profile_fluidsim_particle_sweep(&registry, &device);
    profile_synthetic_buffer(&device);
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

/// Exercise the production perf gate end-to-end: run the startup tuner on this
/// device (measure fused vs unfused for every fusable effect, decide per the
/// §12.5 margin) and report its verdicts — the same path `LayerCompositor::new`
/// runs at launch. Confirms the gate produces a FUSE verdict for ColorGrade on
/// this hardware (and would veto a non-paying fusion on another).
fn profile_perf_gate(device: &GpuDevice) {
    use manifold_renderer::node_graph::freeze::perf_gate;

    println!("\n--- perf gate: startup tune verdicts (device: {}) ---", device.device_name());
    perf_gate::tune_all(device);
    println!("  tuned: {}", perf_gate::is_tuned());
    println!(
        "  ColorGrade -> {}",
        if perf_gate::should_fuse(&EffectTypeId::new("ColorGrade")) {
            "FUSE"
        } else {
            "keep unfused"
        }
    );
}

/// The PRODUCTION number: time the unfused ColorGrade graph against the
/// **auto-generated** fused graph the install path actually ships
/// ([`fused_view_by_id`] → `node.wgsl_compute` carrying the codegen kernel),
/// BOTH driven through the real `Executor` — so this includes the WgslCompute
/// introspection + dispatch overhead the live chain pays, not the bare
/// hand-kernel dispatch `profile_fused_colorgrade` measures. This is the speedup
/// that lands on screen.
fn profile_auto_fused_colorgrade(registry: &PrimitiveRegistry, device: &GpuDevice) {
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
    let Some(fused_view) = fused_view_by_id(&EffectTypeId::new("ColorGrade")) else {
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
        let fused_ms = match time_def(fused_view.canonical_def, w, h, "auto-cg-fused-timed") {
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
fn profile_fused_colorgrade(registry: &PrimitiveRegistry, device: &GpuDevice) {
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
        let mut backend = MetalBackend::new(device, w, h, FORMAT);
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

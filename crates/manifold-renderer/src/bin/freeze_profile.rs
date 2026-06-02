//! `freeze-profile` — Phase 0 GPU profiling bench for the freeze/fusion
//! compiler initiative.
//!
//! Loads effect presets, builds + compiles each graph at performance
//! resolution, and drives the graph runtime headlessly through the
//! `Executor`, timing GPU work per frame via `commit_and_wait_completed`
//! (the wait makes the GPU work synchronous, so wall-time ≈ GPU time).
//!
//! Reports per-preset: dispatch (step) count, avg GPU ms/frame, and
//! implied ms/dispatch — the per-node texture round-trip cost a fusion
//! pass would collapse. A fusable run of length L recovers ~(L-1) ×
//! ms/step per frame.
//!
//! v1 profiles STATELESS effect presets only (they have a `Source` and
//! drive cleanly through `execute_frame_with_gpu`). Stateful effects
//! (Bloom, Watercolor, Feedback, AutoGain, DoF, WireframeDepth) need the
//! `execute_frame_with_state` path and are deferred. Generators (the
//! buffer/particle path) need the `JsonGraphGenerator` drive — deferred.
//!
//! Run: `cargo run -p manifold-renderer --bin freeze-profile --release`

use std::time::Instant;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::{
    EffectGraphDefExt, EffectNode, ExecutionPlan, Executor, FrameTime, MetalBackend,
    NodeInstanceId, PrimitiveRegistry, ResourceId, Source, compile,
};
use manifold_renderer::render_target::RenderTarget;

const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;
const WARMUP: u32 = 8;
const FRAMES: u32 = 120;

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

fn preset_path(name: &str) -> String {
    format!("crates/manifold-renderer/assets/effect-presets/{name}.json")
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

    println!("freeze-profile — graph-runtime GPU cost per preset (avg over {FRAMES} frames)\n");
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

            // Canvas-sized input; content is irrelevant for timing.
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

            let start = Instant::now();
            for _ in 0..FRAMES {
                let mut enc = device.create_encoder("freeze-profile-timed");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
                }
                enc.commit_and_wait_completed();
            }
            let elapsed = start.elapsed();
            let ms_frame = elapsed.as_secs_f64() * 1000.0 / f64::from(FRAMES);
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
}

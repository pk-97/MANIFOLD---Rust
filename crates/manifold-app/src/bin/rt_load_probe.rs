//! Probe: load a saved .manifold project, build scene generators, and run 90
//! frames checking whether RT (ray tracing) actually engages after load.
//! Tests every link: manifest values, build-time node params, per-frame apply,
//! accel-build progression, and the dispatch_shadow_rays call gate.
//!
//! Disposable diagnostic — delete after the bug is fixed.
//!
//! Usage:
//!   MANIFOLD_RT_PROBE=1 cargo run -p manifold-app --bin rt-load-probe -- /path/to/project.manifold 2>/dev/null

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use manifold_core::preset_def::PresetKind;
use manifold_core::project::EmbeddedPreset;
use manifold_core::PresetTypeId;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::generators::registry::GeneratorRegistry;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::node_graph::primitives::{
    RT_PROBE_ENABLED, RT_PROBE_RT_READY, RT_PROBE_HAS_CASTERS, RT_PROBE_ACCEL_BUILT,
    RT_PROBE_UNIFORM_SCENE_W, RT_PROBE_UNIFORM_RT_FLAGS, RT_PROBE_ENTERED_RT_BLOCK,
    RT_PROBE_DISPATCH_COUNT, RT_PROBE_BUILD_COUNT, RT_PROBE_BUILD_ENQUEUED,
    RT_PROBE_DISPATCH_FIRED, RT_PROBE_TOPO_KEY, RT_PROBE_PENDING_KEY,
};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const TOTAL_FRAMES: u32 = 90;

/// Frame indices to print detailed stats for.
const SAMPLE_FRAMES: &[u32] = &[0, 1, 2, 3, 5, 10, 30, 60, 89];

fn main() {
    // Enable render_scene probe statics before any GPU work.
    RT_PROBE_ENABLED.store(true, Ordering::Relaxed);

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().collect();
    let project_path = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        eprintln!("usage: rt_load_probe <path/to/project.manifold>");
        std::process::exit(1);
    };

    // ── Step 1: load the project ──────────────────────────────────────
    println!("=== STEP 1: Load project ===");
    println!("path: {}", project_path.display());

    let project = match manifold_io::loader::load_project_with(
        &project_path,
        |presets: &[EmbeddedPreset]| {
            let mut effect = Vec::new();
            let mut generator = Vec::new();
            for p in presets {
                let Some(id) = p.id() else { continue };
                let Ok(json) = serde_json::to_string(&p.def) else {
                    log::error!("[probe] failed to serialize embedded preset `{id}`");
                    continue;
                };
                match p.kind {
                    PresetKind::Effect => {
                        effect.push((id.as_str().to_string(), json, p.origin));
                    }
                    PresetKind::Generator => {
                        generator.push((id.as_str().to_string(), json, p.origin));
                    }
                }
            }
            manifold_renderer::preset_loader::set_project_presets(effect, generator);
        },
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAILED to load project: {e}");
            std::process::exit(1);
        }
    };

    println!("project name: {}", project.project_name);
    println!("layers: {}", project.timeline.layers.len());

    // ── Step 2: inspect each layer's gen_params manifest ──────────────
    println!();
    println!("=== STEP 2: Layer manifest inspection ===");

    struct LayerInfo {
        index: usize,
        name: String,
        gen_type: Option<PresetTypeId>,
        rt_enabled: Option<f32>,
        temporal_upscale: Option<f32>,
        rt_reflections: Option<f32>,
        has_graph_override: bool,
    }

    let layer_infos: Vec<LayerInfo> = project
        .timeline
        .layers
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let gp = l.gen_params();
            let gen_type = gp.map(|g| g.generator_type().clone());
            let manifest = gp.map(|g| &g.params);
            let rt_enabled = manifest
                .and_then(|m| m.get("8_rt_enabled"))
                .map(|p| p.value);
            let temporal_upscale = manifest
                .and_then(|m| m.get("8_temporal_upscale"))
                .map(|p| p.value);
            let rt_reflections = manifest
                .and_then(|m| m.get("8_rt_reflections"))
                .map(|p| p.value);
            let has_graph_override = gp.and_then(|g| g.graph_def().as_ref()).is_some();
            LayerInfo {
                index: i,
                gen_type,
                name: l.name.clone(),
                rt_enabled,
                temporal_upscale,
                rt_reflections,
                has_graph_override,
            }
        })
        .collect();

    for li in &layer_infos {
        println!(
            "  layer[{}] name={:?} gen_type={:?} graph_override={} rt_enabled={:?} temporal_upscale={:?} rt_reflections={:?}",
            li.index,
            li.name,
            li.gen_type.as_ref().map(|t| t.as_str()),
            li.has_graph_override,
            li.rt_enabled,
            li.temporal_upscale,
            li.rt_reflections,
        );
    }

    // Collect all scene layers (those with gen_params).
    let scene_layers: Vec<&LayerInfo> = layer_infos.iter().filter(|li| li.gen_type.is_some()).collect();

    if scene_layers.is_empty() {
        eprintln!("No layers with generators found.");
        std::process::exit(0);
    }

    // Build shared GPU state.
    let device = Arc::new(GpuDevice::new());
    let format = GpuTextureFormat::Rgba16Float;

    // Run for each scene layer.
    for sli in &scene_layers {
        println!();
        println!("================================================================");
        println!("=== TESTING LAYER[{}]: {} ===", sli.index, sli.name);
        println!("    gen_type={:?}", sli.gen_type.as_ref().map(|t| t.as_str()));
        println!("    rt_enabled={:?} rt_reflections={:?} temporal_upscale={:?}",
            sli.rt_enabled, sli.rt_reflections, sli.temporal_upscale);

        // Step 3: Build generator for this layer.
        let layer = &project.timeline.layers[sli.index];
        let gp = layer.gen_params().unwrap();
        let gen_type = gp.generator_type().clone();
        let override_def = gp.graph_def();
        let manifest = &gp.params;

        let registry = GeneratorRegistry::new(format);
        let Some(mut runtime) = registry.create_with_override(
            Arc::clone(&device),
            &gen_type,
            override_def.as_ref(),
            WIDTH,
            HEIGHT,
            false,  // is_watched
            Some(manifest),
            None,   // relight
        ) else {
            eprintln!("  FAILED to build generator for type {}", gen_type.as_str());
            continue;
        };
        println!("  Generator built OK");

        // Step 4: Print initial render_scene node params.
        println!();
        println!("  === Build-time inner node params ===");
        for node in runtime.graph.nodes() {
            if node.params.contains_key("rt_enabled") {
                let rt_enabled = format!("{:?}", node.params.get("rt_enabled"));
                let rt_ref = format!("{:?}", node.params.get("rt_reflections"));
                let tu = format!("{:?}", node.params.get("temporal_upscale"));
                println!("    render_scene: rt_enabled={} rt_reflections={} temporal_upscale={}",
                    rt_enabled, rt_ref, tu);
            }
        }

        // Step 5: Run 90 frames.
        println!();
        println!("  === Rendering {TOTAL_FRAMES} frames ===");

        let target = RenderTarget::new(&device, WIDTH, HEIGHT, format, "rt-probe-target");

        for frame in 0..TOTAL_FRAMES {
            let ctx = PresetContext {
                time: 0.0,
                beat: 0.0,
                dt: 1.0 / 60.0,
                width: WIDTH,
                height: HEIGHT,
                output_width: WIDTH,
                output_height: HEIGHT,
                aspect: WIDTH as f32 / HEIGHT as f32,
                owner_key: 0,
                is_clip_level: false,
                frame_count: frame as i64,
                anim_progress: 1.0,
                trigger_count: 0,
            };

            // Reset per-frame probe statics (dispatch-fired is one-shot, keep it).
            if !SAMPLE_FRAMES.contains(&frame) {
                RT_PROBE_ENTERED_RT_BLOCK.store(0, Ordering::Relaxed);
                RT_PROBE_DISPATCH_COUNT.store(0, Ordering::Relaxed);
            }

            let mut enc = device.create_encoder(&format!("probe-frame-{frame}"));
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                runtime.render(&mut gpu, &target.texture, &ctx, manifest);
            }
            enc.commit_and_wait_completed();

            if SAMPLE_FRAMES.contains(&frame) {
                print_frame_diagnostics(&runtime, frame);
            }
        }

        // Print final accumulated totals.
        println!();
        println!("  === Final totals after {TOTAL_FRAMES} frames ===");
        print_final_diagnostics();
    }

    println!();
    println!("=== DONE ===");
}

fn print_frame_diagnostics(runtime: &manifold_renderer::preset_runtime::PresetRuntime, frame: u32) {
    println!();
    println!("  ------ Frame {frame} ------");

    // Node params.
    for node in runtime.graph.nodes() {
        if node.params.contains_key("rt_enabled") {
            let rt_enabled = format!("{:?}", node.params.get("rt_enabled"));
            let rt_reflections = format!("{:?}", node.params.get("rt_reflections"));
            let tu = format!("{:?}", node.params.get("temporal_upscale"));
            println!("    node rt_enabled={} rt_reflections={} temporal_upscale={}",
                rt_enabled, rt_reflections, tu);
        }
    }

    // Probe statics.
    let rt_ready = RT_PROBE_RT_READY.load(Ordering::Relaxed);
    let accel_built = RT_PROBE_ACCEL_BUILT.load(Ordering::Relaxed);
    let has_casters = RT_PROBE_HAS_CASTERS.load(Ordering::Relaxed);
    let scene_w = RT_PROBE_UNIFORM_SCENE_W.load(Ordering::Relaxed);
    let rt_flags = RT_PROBE_UNIFORM_RT_FLAGS.load(Ordering::Relaxed);
    let entered_rt = RT_PROBE_ENTERED_RT_BLOCK.load(Ordering::Relaxed);
    let dispatch_count = RT_PROBE_DISPATCH_COUNT.load(Ordering::Relaxed);
    let build_count = RT_PROBE_BUILD_COUNT.load(Ordering::Relaxed);
    let build_enqueued = RT_PROBE_BUILD_ENQUEUED.load(Ordering::Relaxed);
    let dispatch_fired = RT_PROBE_DISPATCH_FIRED.load(Ordering::Relaxed);
    let topo_key = RT_PROBE_TOPO_KEY.load(Ordering::Relaxed);
    let pending_key = RT_PROBE_PENDING_KEY.load(Ordering::Relaxed);

    println!("    rt_accel_built={accel_built}  rt_ready={rt_ready}  has_casters={has_casters}");
    println!("    uniform scene_params.w={scene_w}  rt_flags.x={rt_flags}");
    println!("    entered_rt_block_this_frame={entered_rt}  dispatch_count_this_frame={dispatch_count}");
    println!("    total_builds={build_count}  build_enqueued={build_enqueued} dispatch_ever_fired={dispatch_fired}");
    println!("    topo_key=0x{topo_key:x}  pending_key=0x{pending_key:x}");
}

fn print_final_diagnostics() {
    let dispatch_fired = RT_PROBE_DISPATCH_FIRED.load(Ordering::Relaxed);
    let build_enqueued = RT_PROBE_BUILD_ENQUEUED.load(Ordering::Relaxed);

    if dispatch_fired {
        println!("  VERDICT: RT dispatch_shadow_rays EXECUTED. Load path is sound.");
    } else if build_enqueued {
        println!("  VERDICT: RT build_accel enqueued but dispatch_shadow_rays NEVER fired after {TOTAL_FRAMES} frames.");
        println!("    RT accel build may still be in flight or rt_ready never latched true.");
    } else {
        println!("  VERDICT: RT build_accel NEVER enqueued after {TOTAL_FRAMES} frames.");
        let has_casters = RT_PROBE_HAS_CASTERS.load(Ordering::Relaxed);
        let rt_ready = RT_PROBE_RT_READY.load(Ordering::Relaxed);
        let scene_w = RT_PROBE_UNIFORM_SCENE_W.load(Ordering::Relaxed);
        println!("    Gates at frame 89: rt_ready={rt_ready} has_casters={has_casters} scene_params.w={scene_w}");
        if !has_casters {
            println!("    BLOCKED AT: has_casters=false — no lights cast shadows in this scene.");
        } else if !rt_ready {
            println!("    BLOCKED AT: rt_ready=false — accel structure never latched ready.");
            let pending_key = RT_PROBE_PENDING_KEY.load(Ordering::Relaxed);
            let topo_key = RT_PROBE_TOPO_KEY.load(Ordering::Relaxed);
            println!("      pending_key=0x{pending_key:x} topo_key=0x{topo_key:x} (u64::MAX={})", u64::MAX);
        }
    }
}

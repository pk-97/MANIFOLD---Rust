//! Probe: load a saved .manifold project, build the scene generator,
//! and check whether RT params (rt_enabled, rt_reflections, temporal_upscale)
//! survive the load path and flow into the render_scene node.
//!
//! Disposable diagnostic — delete after the bug is fixed.
//!
//! Usage:
//!   cargo run -p manifold-app --bin rt-load-probe -- /path/to/project.manifold

use std::path::PathBuf;
use std::sync::Arc;

use manifold_core::preset_def::PresetKind;
use manifold_core::project::EmbeddedPreset;
use manifold_core::PresetTypeId;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::generators::registry::GeneratorRegistry;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::render_target::RenderTarget;

fn main() {
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

    // Mirror `install_embedded_presets` from project_io.rs exactly.
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

    // Find scene layer (the one with rt params).
    let scene_layer = layer_infos.iter().find(|li| li.rt_enabled.is_some());
    let scene_idx = match scene_layer {
        Some(li) => li.index,
        None => {
            eprintln!("No layer with 8_rt_enabled found in manifest. Dumping all manifest params:");
            for li in &layer_infos {
                if let Some(gp) = project.timeline.layers[li.index].gen_params() {
                    println!("  layer[{}] manifest entries:", li.index);
                    for p in gp.params.iter() {
                        println!("    {} = {} (spec id={})", p.id(), p.value, p.spec.id);
                    }
                }
            }
            std::process::exit(0);
        }
    };

    let layer = &project.timeline.layers[scene_idx];
    let gp = layer.gen_params().expect("scene layer has gen_params");
    let gen_type = gp.generator_type().clone();
    let override_def = gp.graph_def();
    let manifest = &gp.params;

    println!();
    println!("=== STEP 3: Generator build ===");

    let device = Arc::new(GpuDevice::new());
    let width = 1920u32;
    let height = 1080u32;
    let format = GpuTextureFormat::Rgba16Float;

    let registry = GeneratorRegistry::new(format);
    let is_watched = false;
    let relight = false;
    let relight_params = Default::default();

    let Some(mut runtime) = registry.create_with_override(
        Arc::clone(&device),
        &gen_type,
        override_def.as_ref(),
        width,
        height,
        is_watched,
        Some(manifest),
        relight.then_some(&relight_params),
    ) else {
        eprintln!("FAILED to build generator for type {}", gen_type.as_str());
        std::process::exit(1);
    };

    println!("Generator built OK for type={}", gen_type.as_str());

    // ── Step 4: check inner node params after build ───────────────────
    println!();
    println!("=== STEP 4: Inner node params AFTER build ===");

    for node in runtime.graph.nodes() {
        let type_label = format!(
            "node_id={:?} title={:?}",
            node.node_id, node.title,
        );
        if node.params.contains_key("rt_enabled") {
            let rt_enabled = format!("{:?}", node.params.get("rt_enabled"));
            let rt_ref = format!("{:?}", node.params.get("rt_reflections"));
            let tu = format!("{:?}", node.params.get("temporal_upscale"));
            println!(
                "  render_scene node found: {} rt_enabled={} rt_reflections={} temporal_upscale={}",
                type_label, rt_enabled, rt_ref, tu,
            );
        } else {
            let keys: Vec<String> = node.params.keys().map(|k| k.to_string()).take(5).collect();
            if !keys.is_empty() {
                println!("  non-RT node: {} keys={:?}", type_label, keys);
            }
        }
    }

    // ── Step 5: render 3 frames and check rt accel logs ───────────────
    println!();
    println!("=== STEP 5: Render 3 frames ===");

    let target = RenderTarget::new(&device, width, height, format, "rt-load-probe-target");

    for frame in 0..3 {
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width,
            height,
            output_width: width,
            output_height: height,
            aspect: width as f32 / height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame as i64,
            anim_progress: 1.0,
            trigger_count: 0,
        };
        let mut enc = device.create_encoder(&format!("rt-load-probe-frame-{frame}"));
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            runtime.render(&mut gpu, &target.texture, &ctx, manifest);
        }
        enc.commit_and_wait_completed();
        println!("  Frame {frame} rendered OK");
    }

    // ── Step 6: check inner node params AFTER render ──────────────────
    println!();
    println!("=== STEP 6: Inner node params AFTER render ===");

    for node in runtime.graph.nodes() {
        if node.params.contains_key("rt_enabled") {
            let rt_enabled = format!("{:?}", node.params.get("rt_enabled"));
            let rt_reflections = format!("{:?}", node.params.get("rt_reflections"));
            let temporal_upscale = format!("{:?}", node.params.get("temporal_upscale"));
            println!(
                "  AFTER 3 frames: rt_enabled={} rt_reflections={} temporal_upscale={}",
                rt_enabled, rt_reflections, temporal_upscale,
            );

            // Dump ALL params to identify what's where.
            println!("  All params on render_scene node:");
            let mut keys: Vec<String> = node.params.keys().map(|k| k.to_string()).collect();
            keys.sort();
            for key in keys {
                let val = format!("{:?}", node.params.get(key.as_str()));
                if val.len() > 100 {
                    println!("    {key} = (len={})", val.len());
                } else {
                    println!("    {key} = {val}");
                }
            }
        }
    }

    println!();
    println!("=== DONE ===");
}

#![cfg(all(
    test,
    feature = "journey-proofs",
    feature = "perf-soak",
    target_os = "macos"
))]

//! Bounded held-out RT production measurement for an explicitly supplied project.
//!
//! This is intentionally a thin acceptance harness around the existing
//! headless content thread, warm-project journey, and GPU profiler. It does
//! not create a second renderer or timing loop.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Instant;

use manifold_core::clip::TimelineClip;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, SerializedParamValue};
use manifold_core::effects::{ParamId, ParameterDriver};
use manifold_core::project::Project;
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::{Beats, Bpm, GraphTarget, LayerId, PresetTypeId, cold_touch::ColdTouchKind};
use manifold_renderer::frame_status::FrameRenderStatus;
use serde_json::{Value, json};

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::headless_harness::headless_content_thread;
use crate::scene_modifier_edit::SceneModifierAction;

const PROJECT_PATH: &str = "/Users/peterkiemann/Downloads/TimmyBottle.manifold";
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const WARMUP_FRAMES: usize = 16;
const MEASURED_FRAMES: usize = 120;
const PROFILE_SAMPLER_MAX_SPANS: usize = 8192;
const REFERENCE_TRIANGLES: u64 = 65_536;
const REFERENCE_JSON: &str = include_str!(
    "../../manifold-renderer/tests/fixtures/scene-modifiers/rt_dynamic_reference.json"
);

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn stable_file_hash(path: &Path) -> Result<(String, u64), String> {
    let bytes = std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok((hash_bytes(&bytes), bytes.len() as u64))
}

fn source_triangles(graph: &EffectGraphDef) -> Option<u64> {
    fn walk(nodes: &[manifold_core::effect_graph_def::EffectGraphNode], vertices: &mut u64) {
        for node in nodes {
            if let Some(SerializedParamValue::Int { value }) =
                node.params.get("source_vertex_count")
                && *value >= 0
            {
                *vertices = vertices.saturating_add(*value as u64);
            }
            if let Some(group) = node.group.as_ref() {
                walk(&group.nodes, vertices);
            }
        }
    }
    let mut vertices = 0;
    walk(&graph.nodes, &mut vertices);
    (vertices > 0).then_some(vertices / 3)
}

fn has_surface_waves(graph: &EffectGraphDef) -> bool {
    fn walk(nodes: &[manifold_core::effect_graph_def::EffectGraphNode]) -> bool {
        nodes.iter().any(|node| {
            node.type_id == "node.normal_wave_mesh"
                || node.group.as_ref().is_some_and(|group| walk(&group.nodes))
        })
    }
    walk(&graph.nodes)
        || graph
            .scene_modifiers
            .iter()
            .any(|modifier| has_surface_waves(&modifier.graph))
}

fn generator_layer(project: &Project) -> Result<LayerId, String> {
    project
        .timeline
        .layers
        .iter()
        .find(|layer| {
            layer.gen_params().is_some()
                && project
                    .graph_target_owner(&GraphTarget::Generator(layer.layer_id.clone()))
                    .and_then(|owner| owner.graph_def().as_ref())
                    .is_some()
        })
        .map(|layer| layer.layer_id.clone())
        .ok_or_else(|| "held-out project has no generator layer with a graph".to_string())
}

fn generator_graph<'a>(
    project: &'a Project,
    layer_id: &LayerId,
) -> Result<&'a EffectGraphDef, String> {
    project
        .graph_target_owner(&GraphTarget::Generator(layer_id.clone()))
        .and_then(|owner| owner.graph_def().as_ref())
        .ok_or_else(|| format!("generator {} has no graph", layer_id.as_str()))
}

fn set_generator_param(
    ct: &mut crate::content_thread::ContentThread,
    layer_id: &LayerId,
    suffix: &str,
    value: f32,
) -> Result<(), String> {
    let target = GraphTarget::Generator(layer_id.clone());
    let (param_id, old) = {
        let project = ct
            .engine
            .project()
            .ok_or_else(|| "content engine has no project".to_string())?;
        let instance = project
            .preset_instance(&target)
            .ok_or_else(|| "generator instance is unavailable".to_string())?;
        instance
            .params
            .iter()
            .find(|param| param.id() == suffix || param.id().ends_with(suffix))
            .map(|param| (param.id().to_string(), instance.get_base_param(param.id())))
            .ok_or_else(|| format!("generator has no parameter matching `{suffix}`"))?
    };
    if (old - value).abs() <= f32::EPSILON {
        return Ok(());
    }
    if ct.handle_command(ContentCommand::Execute(Box::new(
        manifold_editing::commands::effects::ChangeGraphParamCommand::new(
            target, param_id, old, value,
        ),
    ))) {
        return Err("content thread shut down while setting generator parameter".into());
    }
    ct.graph_edit_diagnostic
        .take()
        .map_or(Ok(()), |diagnostic| Err(diagnostic.message))
}

fn ensure_surface_waves_driver(
    ct: &mut crate::content_thread::ContentThread,
    layer_id: &LayerId,
) -> Result<(), String> {
    let needs_surface_waves = !has_surface_waves(generator_graph(
        ct.engine.project().ok_or("content engine has no project")?,
        layer_id,
    )?);
    if needs_surface_waves {
        if ct.handle_command(ContentCommand::SceneModifier(SceneModifierAction::Add(
            layer_id.clone(),
            "SurfaceWaves".into(),
        ))) {
            return Err("content thread shut down while adding SurfaceWaves".into());
        }
        if let Some(diagnostic) = ct.graph_edit_diagnostic.take() {
            return Err(format!("SurfaceWaves add rejected: {}", diagnostic.message));
        }
    }

    let project = ct
        .engine
        .project()
        .ok_or_else(|| "content engine has no project".to_string())?;
    let graph = generator_graph(project, layer_id)?;
    let modifier = graph
        .scene_modifiers
        .iter()
        .find(|instance| has_surface_waves(&instance.graph))
        .ok_or_else(|| "SurfaceWaves modifier is absent after production edit".to_string())?;
    let driver_id = graph
        .preset_metadata
        .as_ref()
        .and_then(|metadata| {
            metadata.bindings.iter().find_map(|binding| {
                matches!(
                    &binding.target,
                    BindingTarget::SceneModifier { modifier_id, param_id }
                        if modifier_id == &modifier.id && param_id.as_str() == "phase"
                )
                .then(|| binding.id.clone())
            })
        })
        .ok_or_else(|| "SurfaceWaves exposes no numeric host parameter for a driver".to_string())?;
    let already_driven = project
        .preset_instance(&GraphTarget::Generator(layer_id.clone()))
        .and_then(|instance| instance.drivers.as_ref())
        .is_some_and(|drivers| {
            drivers
                .iter()
                .any(|driver| driver.param_id.as_ref() == driver_id.as_str())
        });
    if !already_driven {
        ct.handle_command(ContentCommand::Execute(Box::new(
            manifold_editing::commands::drivers::AddDriverCommand::new(
                manifold_editing::commands::effect_target::DriverTarget::GeneratorParam {
                    layer_id: layer_id.clone(),
                },
                ParameterDriver::new(
                    ParamId::from(driver_id),
                    BeatDivision::Quarter,
                    DriverWaveform::Sine,
                ),
            ),
        )));
    }
    Ok(())
}

fn distribution(values: &[f64]) -> Value {
    if values.is_empty() {
        return json!({"count": 0, "p50Ms": Value::Null, "p95Ms": Value::Null, "maxMs": Value::Null});
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let quantile = |fraction: f64| sorted[((sorted.len() - 1) as f64 * fraction).round() as usize];
    json!({
        "count": values.len(),
        "p50Ms": quantile(0.50),
        "p95Ms": quantile(0.95),
        "maxMs": sorted.last().copied(),
    })
}

fn reference_project() -> (Project, LayerId, String, u64) {
    let source_bytes = REFERENCE_JSON.as_bytes();
    let mut document: Value = serde_json::from_str(REFERENCE_JSON)
        .expect("reference RT graph fixture must be valid JSON");
    document["presetMetadata"] = json!({
        "id": "RtDynamicReference",
        "displayName": "RT Dynamic Reference",
        "category": "Geometry",
        "oscPrefix": "rt_dynamic_reference",
        "params": [
            {"id": "phase", "name": "Phase", "min": -100.0, "max": 100.0, "defaultValue": 0.0},
            {"id": "rt_enabled", "name": "RT Enabled", "min": 0.0, "max": 1.0, "defaultValue": 1.0}
        ],
        "bindings": [
            {"id": "phase", "label": "Phase", "defaultValue": 0.0, "target": {"kind": "node", "nodeId": "wave", "param": "phase"}},
            {"id": "rt_enabled", "label": "RT Enabled", "defaultValue": 1.0, "target": {"kind": "node", "nodeId": "scene", "param": "rt_enabled"}}
        ]
    });
    let graph: EffectGraphDef =
        serde_json::from_value(document).expect("reference RT graph metadata must deserialize");
    let layer_id = LayerId::new("rt-dynamic-reference");
    let mut layer = manifold_core::layer::Layer::new_generator(
        "RT Dynamic Reference".into(),
        PresetTypeId::new("PhotoscanBaseline"),
        0,
    );
    layer.layer_id = layer_id.clone();
    layer.gen_params_or_init().graph = Some(graph);
    layer.gen_params_or_init().refresh_manifest_from_graph();
    layer
        .gen_params_or_init()
        .drivers_mut()
        .push(ParameterDriver::new(
            ParamId::from("phase"),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        ));
    layer
        .clips
        .push(TimelineClip::new_generator(Beats::ZERO, Beats(16.0)));
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    project.settings.output_width = WIDTH as i32;
    project.settings.output_height = HEIGHT as i32;
    project.timeline.layers.push(layer);
    (
        project,
        layer_id,
        hash_bytes(source_bytes),
        source_bytes.len() as u64,
    )
}

fn setup_held_out(
    ct: &mut crate::content_thread::ContentThread,
    layer_id: &LayerId,
) -> Result<(), String> {
    ensure_surface_waves_driver(ct, layer_id)?;
    set_generator_param(ct, layer_id, "rt_enabled", 1.0)
}

fn setup_reference(
    ct: &mut crate::content_thread::ContentThread,
    layer_id: &LayerId,
) -> Result<(), String> {
    set_generator_param(ct, layer_id, "rt_enabled", 1.0)
}

fn measure_project(
    project: Project,
    provenance: (Value, String, u64),
    layer_id: LayerId,
    start_beat: Beats,
    generated_triangles: Option<u64>,
    setup: fn(&mut crate::content_thread::ContentThread, &LayerId) -> Result<(), String>,
) -> Result<Value, String> {
    let (path, project_hash, project_bytes) = provenance;
    let source_triangles = generator_graph(&project, &layer_id)
        .ok()
        .and_then(source_triangles);
    let mut ct = headless_content_thread(project, WIDTH, HEIGHT);
    let (state_tx, state_rx) = crossbeam_channel::unbounded::<ContentState>();
    let drain = std::thread::Builder::new()
        .name("rt-dynamic-content-drain".into())
        .spawn(move || while state_rx.recv().is_ok() {})
        .map_err(|error| format!("spawn state drain: {error}"))?;
    ct.timer.set_frame_clocked(true);
    setup(&mut ct, &layer_id)?;
    ct.content_pipeline
        .set_profiling(true, PROFILE_SAMPLER_MAX_SPANS);
    if !ct.content_pipeline.profiling_sampler_ready() {
        return Err("native GPU timestamp profiling is unavailable".into());
    }
    crate::scene_modifier_journey::warm_project(&mut ct, &state_tx);
    ct.handle_command(ContentCommand::Pause);
    ct.handle_command(ContentCommand::SeekToBeat(start_beat));
    ct.handle_command(ContentCommand::Play);
    for _ in 0..WARMUP_FRAMES {
        ct.tick_frame(&state_tx);
        let _ = ct.content_pipeline.take_gpu_profiles();
        let _ = ct.content_pipeline.take_step_profiles();
    }
    let device = ct
        .content_pipeline
        .native_device_handle()
        .ok_or_else(|| "content pipeline has no native device".to_string())?;
    let allocation_after_warmup = device.allocation_counts();
    let pipeline_touches_after_warmup =
        manifold_core::cold_touch::cold_touch_count(ColdTouchKind::PipelineCompile);
    let faults_before = manifold_gpu::gpu_fault::fault_count();
    let mut cpu_wall = Vec::with_capacity(MEASURED_FRAMES);
    let mut cpu_profiled = Vec::with_capacity(MEASURED_FRAMES);
    let mut gpu_frame = Vec::with_capacity(MEASURED_FRAMES);
    let mut rt_updates = manifold_gpu::raytrace::RtAccelUpdate::default();
    let mut rt_dispatches = 0u32;
    let mut rt_history_resets = 0u32;
    let mut complete_frames = 0usize;
    let mut profile_overflow = 0usize;
    let mut profile_invalid = 0usize;
    let mut profile_failed = 0usize;
    ct.handle_command(ContentCommand::SeekToBeat(start_beat));
    ct.handle_command(ContentCommand::Play);
    for _ in 0..MEASURED_FRAMES {
        let start = Instant::now();
        ct.tick_frame(&state_tx);
        cpu_wall.push(start.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(
            ct.content_pipeline.frame_render_status(),
            FrameRenderStatus::Complete,
            "RT reference frame is invalid"
        );
        complete_frames += 1;
        let (update, dispatches, resets) = ct.content_pipeline.frame_rt_observation();
        rt_updates.blas_builds += update.blas_builds;
        rt_updates.blas_refits += update.blas_refits;
        rt_updates.tlas_builds += update.tlas_builds;
        rt_updates.tlas_refits += update.tlas_refits;
        rt_updates.emissive_refreshes += update.emissive_refreshes;
        rt_dispatches += dispatches;
        rt_history_resets += resets;
        let profiles = ct.content_pipeline.take_gpu_profiles();
        if profiles.is_empty() {
            return Err("missing GPU profiler samples for a measured frame".into());
        }
        let mut gpu_total = 0.0;
        for (_, profile) in profiles {
            if !profile.total_ms.is_finite() || profile.total_ms <= 0.0 {
                return Err("invalid GPU profiler sample for a measured frame".into());
            }
            gpu_total += profile.total_ms;
            profile_overflow += profile.overflow;
            profile_invalid += profile.invalid;
            profile_failed += profile.failed_command_buffers;
        }
        gpu_frame.push(gpu_total);
        let mut cpu_total_nanos = ct
            .content_pipeline
            .take_step_profiles()
            .into_iter()
            .map(|profile| profile.cpu_nanos)
            .sum::<u64>();
        for renderer in ct.engine.renderers_mut() {
            if let Some(generator) = renderer
                .as_any_mut()
                .downcast_mut::<manifold_renderer::generator_renderer::GeneratorRenderer>(
            ) {
                cpu_total_nanos += generator
                    .take_step_profiles()
                    .into_iter()
                    .map(|profile| profile.cpu_nanos)
                    .sum::<u64>();
            }
        }
        cpu_profiled.push(cpu_total_nanos as f64 / 1_000_000.0);
    }
    let faults_after = manifold_gpu::gpu_fault::fault_count();
    let allocation_after = device.allocation_counts();
    let pipeline_touches_after =
        manifold_core::cold_touch::cold_touch_count(ColdTouchKind::PipelineCompile);
    let modifier_chain = ct
        .engine
        .project()
        .and_then(|project| generator_graph(project, &layer_id).ok())
        .map(|graph| {
            graph
                .scene_modifiers
                .iter()
                .map(|modifier| {
                    modifier
                        .graph
                        .preset_metadata
                        .as_ref()
                        .map(|metadata| metadata.id.clone())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    drop(state_tx);
    drain
        .join()
        .map_err(|_| "state drain thread panicked".to_string())?;
    let stable_buffers = allocation_after[0] == allocation_after_warmup[0];
    let stable_acceleration = allocation_after[2] == allocation_after_warmup[2];
    let profiling_clean = profile_overflow == 0 && profile_invalid == 0 && profile_failed == 0;
    Ok(json!({
        "status": "measured",
        "project": path,
        "projectHash": project_hash,
        "projectHashAlgorithm": "SHA-256",
        "projectBytes": project_bytes,
        "resolution": [WIDTH, HEIGHT],
        "quality": "realtime_default",
        "layerId": layer_id,
        "sourceTriangles": source_triangles,
        "generatedTriangles": generated_triangles,
        "modifierChain": modifier_chain,
        "startBeat": start_beat.0,
        "warmupFrames": WARMUP_FRAMES,
        "measuredFrames": MEASURED_FRAMES,
        "completeFrames": complete_frames,
        "cpuWall": distribution(&cpu_wall),
        "cpuProfiled": distribution(&cpu_profiled),
        "gpuFrame": distribution(&gpu_frame),
        "rtOperations": {
            "blasBuilds": rt_updates.blas_builds,
            "blasRefits": rt_updates.blas_refits,
            "tlasBuilds": rt_updates.tlas_builds,
            "tlasRefits": rt_updates.tlas_refits,
            "emissiveRefreshes": rt_updates.emissive_refreshes,
            "dispatches": rt_dispatches,
            "historyResets": rt_history_resets,
        },
        "memory": {
            "allocationCountsAfterWarmup": allocation_after_warmup,
            "allocationCountsAfterMeasured": allocation_after,
            "postWarmupBufferAllocations": allocation_after[0].saturating_sub(allocation_after_warmup[0]),
            "postWarmupTextureAllocations": allocation_after[1].saturating_sub(allocation_after_warmup[1]),
            "postWarmupAccelerationStructureAllocations": allocation_after[2].saturating_sub(allocation_after_warmup[2]),
            "modifierSnapshot": device.modifier_memory_snapshot().map(|snapshot| snapshot.current_allocated_bytes),
        },
        "profiling": {
            "overflowDispatches": profile_overflow,
            "invalidSamples": profile_invalid,
            "failedCommandBuffers": profile_failed,
            "pipelineCompilesAfterWarmup": pipeline_touches_after.saturating_sub(pipeline_touches_after_warmup),
        },
        "gpuFaults": faults_after.saturating_sub(faults_before),
        "enforced": {
            "completeFrames": complete_frames == MEASURED_FRAMES,
            "noGpuFaults": faults_after == faults_before,
            "zeroPostWarmupBufferAllocations": stable_buffers,
            "zeroPostWarmupAccelerationStructureAllocations": stable_acceleration,
            "rtDispatched": rt_dispatches > 0,
            "profilingClean": profiling_clean,
        },
        "blocked": ["No historical baseline or pass/fail performance judgement is embedded."],
    }))
}

fn run_held_out() -> Result<Value, String> {
    let path = std::env::var_os("MANIFOLD_RT_HELD_OUT_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(PROJECT_PATH));
    if !path.is_file() {
        return Err(format!(
            "held-out project does not exist: {}",
            path.display()
        ));
    }
    let (project_hash, project_bytes) = stable_file_hash(&path)?;
    let mut project =
        manifold_io::loader::load_project_with(&path, crate::project_io::install_embedded_presets)
            .map_err(|error| format!("load {}: {error}", path.display()))?;
    project.settings.output_width = WIDTH as i32;
    project.settings.output_height = HEIGHT as i32;
    project.settings.frame_rate = 60.0;
    let layer_id = generator_layer(&project)?;
    let start_beat = project
        .timeline
        .layers
        .iter()
        .find(|layer| layer.layer_id == layer_id)
        .and_then(|layer| {
            layer
                .clips
                .iter()
                .max_by(|a, b| a.duration_beats.0.total_cmp(&b.duration_beats.0))
        })
        .map(|clip| clip.start_beat)
        .ok_or("held-out generator has no clip")?;
    measure_project(
        project,
        (
            Value::String(path.display().to_string()),
            project_hash,
            project_bytes,
        ),
        layer_id,
        start_beat,
        None,
        setup_held_out,
    )
}

#[test]
fn rt_dynamic_held_out() {
    let report =
        run_held_out().unwrap_or_else(|error| panic!("held-out RT measurement failed: {error}"));
    let path = std::env::var_os("MANIFOLD_RT_HELD_OUT_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/manifold-rt-dynamic/held-out.json"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("held-out report directory");
    }
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&report).expect("held-out report serialization"),
    )
    .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    assert_eq!(report["enforced"]["completeFrames"], true);
    assert_eq!(report["enforced"]["noGpuFaults"], true);
    assert_eq!(report["enforced"]["zeroPostWarmupBufferAllocations"], true);
    assert_eq!(
        report["enforced"]["zeroPostWarmupAccelerationStructureAllocations"],
        true
    );
    assert_eq!(report["enforced"]["rtDispatched"], true);
    assert_eq!(report["enforced"]["profilingClean"], true);
    eprintln!("[rt dynamic held-out] report written to {}", path.display());
}

#[test]
fn rt_dynamic_reference_content() {
    let (project, layer_id, project_hash, project_bytes) = reference_project();
    let report = measure_project(
        project,
        (
            Value::String("embedded:rt_dynamic_reference.json".into()),
            project_hash,
            project_bytes,
        ),
        layer_id,
        Beats::ZERO,
        Some(REFERENCE_TRIANGLES),
        setup_reference,
    )
    .unwrap_or_else(|error| panic!("reference RT measurement failed: {error}"));
    let path = std::env::var_os("MANIFOLD_RT_REFERENCE_CONTENT_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/manifold-rt-dynamic/reference-content.json"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("reference report directory");
    }
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&report).expect("reference report serialization"),
    )
    .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    for key in [
        "completeFrames",
        "noGpuFaults",
        "zeroPostWarmupBufferAllocations",
        "zeroPostWarmupAccelerationStructureAllocations",
        "rtDispatched",
        "profilingClean",
    ] {
        assert_eq!(report["enforced"][key], true, "reference invariant {key}");
    }
    eprintln!(
        "[rt dynamic reference] report written to {}",
        path.display()
    );
}

#![cfg(all(
    test,
    feature = "journey-proofs",
    feature = "perf-soak",
    target_os = "macos"
))]

//! Bounded V8 performance qualification for imported photoscans.
//!
//! This is a measurement harness, not a soak: each source/variant receives a
//! separate preparation window followed by exactly 240 profiled frames at
//! 1024x1024. It uses the production content thread, Metal timestamp sampler,
//! and structural scene-modifier commands. No synthetic GPU timings are used.

use std::path::{Path, PathBuf};
use std::time::Instant;

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, SerializedParamValue};
use manifold_core::effects::{ParamId, ParameterDriver};
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::{
    Beats, Bpm, LayerId, NodeId, PresetTypeId,
    cold_touch::{ColdTouchKind, cold_touch_count, reset_cold_touch_counts},
};
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use serde::Serialize;

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::headless_harness::headless_content_thread;
use crate::scene_modifier_edit::SceneModifierAction;

const WIDTH: u32 = 1024;
const HEIGHT: u32 = 1024;
const MEASURED_FRAMES: usize = 240;
const PREPARATION_FRAMES: usize = 8;
const PROFILE_SAMPLER_MAX_SPANS: usize = 8192;
const CPU_FRAME_BUDGET_MS: f64 = 20.0;
const GPU_P95_BUDGET_MS: f64 = 16.67;

const MUSHROOM_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0___mushroom.glb"
);
const THISTLE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0__japanese_thistle_cirsium_japonicum.glb"
);

#[derive(Clone, Copy)]
struct Fixture {
    name: &'static str,
    path: &'static str,
    layer_id: &'static str,
}

const FIXTURES: [Fixture; 2] = [
    Fixture {
        name: "mushroom",
        path: MUSHROOM_FIXTURE,
        layer_id: "v8-performance-mushroom",
    },
    Fixture {
        name: "japanese-thistle",
        path: THISTLE_FIXTURE,
        layer_id: "v8-performance-thistle",
    },
];

const MODIFIER_VARIANTS: [&str; 4] = [
    "ElasticSculpture",
    "SurfacePeel",
    "VortexFragments",
    "stack",
];

#[derive(Debug, Clone, Serialize)]
struct Distribution {
    median_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
struct CaseMetrics {
    fixture: String,
    variant: String,
    source_objects: usize,
    source_triangles: Option<u64>,
    prepared_bytes: Option<u64>,
    prepared_bytes_status: &'static str,
    resolution: [u32; 2],
    preparation_frames: usize,
    measured_frames: usize,
    clip_active_after_preparation: bool,
    output_finite_after_preparation: bool,
    output_visible_pixels_after_preparation: usize,
    output_rgb_range_after_preparation: f32,
    cpu_wall_ms: Distribution,
    gpu_generators_compositor_ms: Distribution,
    gpu_p95_delta_from_baseline_ms: Option<f64>,
    pipeline_compile_cold_touches_preparation: u64,
    pipeline_compile_cold_touches_live: u64,
    profiling_overflow_dispatches: usize,
    profiling_invalid_samples: usize,
    profiling_failed_command_buffers: usize,
}

#[derive(Debug)]
struct MeasureFailure {
    source_objects: usize,
    source_triangles: Option<u64>,
    reason: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum CaseRecord {
    Measured(CaseMetrics),
    Unsupported {
        fixture: String,
        variant: String,
        source_objects: usize,
        source_triangles: Option<u64>,
        reason: String,
    },
}

fn fixture_project(fixture: Fixture) -> (Project, usize, Option<u64>) {
    let (graph, report) = assemble_import_graph(Path::new(fixture.path))
        .unwrap_or_else(|error| panic!("{} import failed: {error}", fixture.path));
    let triangle_count = source_triangles(&graph);
    assert!(
        report.object_count > 0,
        "{} has no imported objects",
        fixture.path
    );
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    project.settings.output_width = WIDTH as i32;
    project.settings.output_height = HEIGHT as i32;
    let mut layer = Layer::new_generator(
        fixture.name.into(),
        PresetTypeId::new("PhotoscanBaseline"),
        0,
    );
    layer.layer_id = LayerId::new(fixture.layer_id);
    layer.gen_params_or_init().graph = Some(graph);
    layer.gen_params_or_init().refresh_manifest_from_graph();
    let mut clip = manifold_core::clip::TimelineClip::new_generator(Beats(0.0), Beats(16.0));
    clip.layer_id = layer.layer_id.clone();
    layer.clips.push(clip);
    project.timeline.layers.push(layer);
    (project, report.object_count, triangle_count)
}

fn generator_graph<'a>(project: &'a Project, layer_id: &LayerId) -> &'a EffectGraphDef {
    project
        .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
        .and_then(|owner| owner.graph_def().as_ref())
        .expect("performance generator graph")
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

fn enable_profiling(ct: &mut crate::content_thread::ContentThread) {
    ct.content_pipeline
        .set_profiling(true, PROFILE_SAMPLER_MAX_SPANS);
    assert!(
        ct.content_pipeline.profiling_sampler_ready(),
        "V8 requires native GPU timestamp profiling; this device does not support it"
    );
    for renderer in ct.engine.renderers_mut() {
        if let Some(generator) = renderer
            .as_any_mut()
            .downcast_mut::<manifold_renderer::generator_renderer::GeneratorRenderer>(
        ) {
            generator.set_profiling(true);
        }
    }
}

fn apply_variant(
    ct: &mut crate::content_thread::ContentThread,
    layer_id: &LayerId,
    variant: &str,
) -> Result<(), String> {
    let presets: Vec<&str> = match variant {
        // Two SurfacePeel instances are the explicit supported stack for the
        // full scans: each consumes one prepared buffer and stays within the
        // 256 MiB modifier admission cap. Elastic + SurfacePeel is outside
        // this explicit stack shape and is not measured here.
        "stack" => vec!["SurfacePeel", "SurfacePeel"],
        name => vec![name],
    };
    for preset in presets {
        ct.graph_edit_diagnostic = None;
        if ct.handle_command(ContentCommand::SceneModifier(SceneModifierAction::Add(
            layer_id.clone(),
            preset.into(),
        ))) {
            return Err("content thread shut down while applying modifier".into());
        }
        if let Some(diagnostic) = ct.graph_edit_diagnostic.take() {
            return Err(format!("{preset}: {}", diagnostic.message));
        }
    }

    let graph = generator_graph(ct.engine.project().expect("loaded project"), layer_id);
    let modifier_ids: Vec<NodeId> = graph
        .scene_modifiers
        .iter()
        .map(|instance| instance.id.clone())
        .collect();
    assert!(
        !modifier_ids.is_empty(),
        "variant {variant} has no modifiers"
    );
    let metadata = graph.preset_metadata.as_ref().expect("host metadata");
    for modifier_id in modifier_ids {
        assert!(
            metadata.bindings.iter().any(|binding| matches!(
                &binding.target,
                BindingTarget::SceneModifier { modifier_id: target, .. } if target == &modifier_id
            )),
            "modifier {modifier_id} has no explicit SceneModifier host binding"
        );
    }
    Ok(())
}

fn attach_driver(
    ct: &mut crate::content_thread::ContentThread,
    layer_id: &LayerId,
) -> Result<(), String> {
    let graph = generator_graph(ct.engine.project().expect("loaded project"), layer_id);
    let instance = graph
        .scene_modifiers
        .first()
        .ok_or_else(|| "driver target has no scene modifier".to_string())?;
    let enabled_param = instance
        .graph
        .preset_metadata
        .as_ref()
        .and_then(|metadata| metadata.scene_modifier.as_ref())
        .map(|recipe| recipe.enabled_param.as_str());
    let binding_id = graph
        .preset_metadata
        .as_ref()
        .and_then(|metadata| {
            metadata.bindings.iter().find_map(|binding| {
                matches!(
                    &binding.target,
                    BindingTarget::SceneModifier { modifier_id, param_id }
                        if modifier_id == &instance.id && Some(param_id.as_str()) != enabled_param
                )
                .then(|| binding.id.clone())
            })
        })
        .ok_or_else(|| "scene modifier has no numeric host binding for driver".to_string())?;
    let driver_layer_id = layer_id.clone();
    ct.handle_command(ContentCommand::MutateProject(Box::new(move |project| {
        let owner = project
            .graph_target_owner_mut(&manifold_core::GraphTarget::Generator(driver_layer_id))
            .expect("driver generator owner");
        owner.drivers_mut().push(ParameterDriver::new(
            ParamId::from(binding_id),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        ));
    })));
    Ok(())
}

fn prepared_modifier_bytes(
    ct: &crate::content_thread::ContentThread,
    layer_id: &LayerId,
) -> Result<Option<u64>, String> {
    let (graph, params) = {
        let host = ct
            .engine
            .project()
            .and_then(|project| {
                project.graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            })
            .ok_or_else(|| "prepared-byte query has no generator owner".to_string())?;
        (
            host.graph_def()
                .clone()
                .ok_or_else(|| "prepared-byte query has no graph".to_string())?,
            host.params.clone(),
        )
    };
    let runtime = manifold_renderer::preset_runtime::PresetRuntime::from_def(
        graph,
        &manifold_renderer::node_graph::PrimitiveRegistry::with_builtin(),
        Some(&params),
    )
    .map_err(|error| format!("prepared-byte query failed: {error}"))?;
    let usage = runtime
        .prepared_modifier_buffer_usage((WIDTH, HEIGHT))
        .map_err(|error| format!("prepared-byte query failed: {error}"))?;
    Ok(usage.map(|usage| usage.modifier_bytes.values().copied().sum()))
}

fn percentile(values: &[f64], quantile: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite timing"));
    let index = ((sorted.len() as f64 * quantile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len().saturating_sub(1));
    sorted[index]
}

fn distribution(values: &[f64]) -> Distribution {
    assert!(!values.is_empty(), "no timing samples");
    Distribution {
        median_ms: percentile(values, 0.5),
        p95_ms: percentile(values, 0.95),
        max_ms: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    }
}

fn inspect_output(ct: &crate::content_thread::ContentThread) -> (bool, usize, f32) {
    let device = ct
        .content_pipeline
        .native_device()
        .expect("native performance device");
    let texture = ct.content_pipeline.export_output_texture();
    assert_eq!(texture.format, manifold_gpu::GpuTextureFormat::Rgba16Float);
    let row_bytes = texture.width * texture.format.bytes_per_pixel();
    let size = row_bytes as usize * texture.height as usize;
    let buffer = device.create_buffer_shared(size as u64);
    let mut encoder = device.create_encoder("v8-performance-readback");
    encoder.copy_texture_to_buffer(texture, &buffer, texture.width, texture.height, row_bytes);
    encoder.commit_and_wait_completed();
    let ptr = buffer.mapped_ptr().expect("performance readback mapping");
    let bytes = unsafe { std::slice::from_raw_parts(ptr, size) };
    let mut finite = true;
    let mut visible = 0;
    let mut min_rgb = f32::INFINITY;
    let mut max_rgb = f32::NEG_INFINITY;
    for pixel in bytes.chunks_exact(8) {
        let mut channels = [0.0_f32; 4];
        for (channel, value) in channels.iter_mut().enumerate() {
            let bits = u16::from_le_bytes([pixel[channel * 2], pixel[channel * 2 + 1]]);
            *value = half::f16::from_bits(bits).to_f32();
            finite &= value.is_finite();
        }
        for value in &channels[..3] {
            min_rgb = min_rgb.min(*value);
            max_rgb = max_rgb.max(*value);
        }
        if channels[3] > 0.0 && channels[..3].iter().any(|value| value.abs() > 1.0e-6) {
            visible += 1;
        }
    }
    (finite, visible, max_rgb - min_rgb)
}

fn output_failure(metrics: &CaseMetrics) -> Option<String> {
    if !metrics.clip_active_after_preparation {
        return Some("no active clip after preparation".into());
    }
    if !metrics.output_finite_after_preparation {
        return Some("native output contains non-finite values".into());
    }
    if metrics.output_visible_pixels_after_preparation == 0 {
        return Some("native output has no visible RGB pixels".into());
    }
    if metrics.output_rgb_range_after_preparation <= 1.0e-4 {
        return Some(format!(
            "native output is uniform RGB (range {:.6})",
            metrics.output_rgb_range_after_preparation
        ));
    }
    None
}

fn measure_case(
    ct: &mut crate::content_thread::ContentThread,
    state_tx: &crossbeam_channel::Sender<ContentState>,
    fixture: Fixture,
    variant: &str,
) -> Result<CaseMetrics, MeasureFailure> {
    reset_cold_touch_counts();
    let (project, source_objects, source_triangles) = fixture_project(fixture);
    ct.handle_command(ContentCommand::LoadProject(Box::new(project)));
    ct.timer.set_frame_clocked(true);
    enable_profiling(ct);
    let layer_id = LayerId::new(fixture.layer_id);
    if variant != "baseline" {
        if let Err(reason) = apply_variant(ct, &layer_id, variant) {
            return Err(MeasureFailure {
                source_objects,
                source_triangles,
                reason,
            });
        }
        if let Err(reason) = attach_driver(ct, &layer_id) {
            return Err(MeasureFailure {
                source_objects,
                source_triangles,
                reason,
            });
        }
    }
    let prepared_bytes = match prepared_modifier_bytes(ct, &layer_id) {
        Ok(bytes) => bytes,
        Err(reason) => {
            return Err(MeasureFailure {
                source_objects,
                source_triangles,
                reason,
            });
        }
    };
    // Preparation is explicitly reported. It is not subtracted from the
    // measured wall times or used to hide first-live pipeline touches.
    ct.handle_command(ContentCommand::Pause);
    ct.handle_command(ContentCommand::SeekToBeat(Beats(0.0)));
    crate::scene_modifier_journey::warm_project(ct, state_tx);
    ct.handle_command(ContentCommand::Play);
    for _ in 0..PREPARATION_FRAMES {
        ct.tick_frame(state_tx);
        let _ = ct.content_pipeline.take_gpu_profiles();
    }
    let preparation_touches = cold_touch_count(ColdTouchKind::PipelineCompile);
    let clip_active_after_preparation = ct.engine.active_clip_count() > 0;
    let (
        output_finite_after_preparation,
        output_visible_pixels_after_preparation,
        output_rgb_range_after_preparation,
    ) = inspect_output(ct);

    ct.handle_command(ContentCommand::SeekToBeat(Beats(0.0)));
    ct.handle_command(ContentCommand::Play);
    let live_start_touches = preparation_touches;
    let mut cpu_ms = Vec::with_capacity(MEASURED_FRAMES);
    let mut gpu_ms = Vec::with_capacity(MEASURED_FRAMES);
    let mut overflow = 0;
    let mut invalid = 0;
    let mut failed_command_buffers = 0;
    for _ in 0..MEASURED_FRAMES {
        let start = Instant::now();
        ct.tick_frame(state_tx);
        cpu_ms.push(start.elapsed().as_secs_f64() * 1_000.0);
        let profiles = ct.content_pipeline.take_gpu_profiles();
        let mut generators = None;
        let mut compositor = None;
        for (label, profile) in profiles {
            assert!(profile.total_ms.is_finite() && profile.total_ms > 0.0);
            overflow += profile.overflow;
            invalid += profile.invalid;
            failed_command_buffers += profile.failed_command_buffers;
            match label {
                "Generators" => generators = Some(profile.total_ms),
                "Compositor" => compositor = Some(profile.total_ms),
                _ => {}
            }
        }
        gpu_ms.push(
            generators.expect("missing Generators GPU profile")
                + compositor.expect("missing Compositor GPU profile"),
        );
    }
    let live_touches = cold_touch_count(ColdTouchKind::PipelineCompile);
    assert_eq!(overflow, 0, "GPU profile sampler overflowed");
    assert_eq!(invalid, 0, "GPU profile contained invalid samples");
    assert_eq!(failed_command_buffers, 0, "GPU command buffer failed");
    assert_eq!(
        live_touches, live_start_touches,
        "live 240-frame sequence created a pipeline after preparation"
    );

    Ok(CaseMetrics {
        fixture: fixture.name.into(),
        variant: variant.into(),
        source_objects,
        source_triangles,
        prepared_bytes,
        prepared_bytes_status: if prepared_bytes.is_some() {
            "measured via shared admission API"
        } else {
            "no modifier buffers in baseline"
        },
        resolution: [WIDTH, HEIGHT],
        preparation_frames: PREPARATION_FRAMES,
        measured_frames: MEASURED_FRAMES,
        clip_active_after_preparation,
        output_finite_after_preparation,
        output_visible_pixels_after_preparation,
        output_rgb_range_after_preparation,
        cpu_wall_ms: distribution(&cpu_ms),
        gpu_generators_compositor_ms: distribution(&gpu_ms),
        gpu_p95_delta_from_baseline_ms: None,
        pipeline_compile_cold_touches_preparation: preparation_touches,
        pipeline_compile_cold_touches_live: live_touches.saturating_sub(live_start_touches),
        profiling_overflow_dispatches: overflow,
        profiling_invalid_samples: invalid,
        profiling_failed_command_buffers: failed_command_buffers,
    })
}

fn write_report(cases: &[CaseRecord], hardware: &str) {
    let output_dir = PathBuf::from("target/journey-proofs/f8_performance");
    std::fs::create_dir_all(&output_dir).expect("performance report directory");
    let report = serde_json::json!({
        "mode": "scene_modifier_v8_performance",
        "hardware": hardware,
        "resolution": [WIDTH, HEIGHT],
        "sample_count_per_case": MEASURED_FRAMES,
        "cpu_wall_includes_profiling_waits": true,
        "budgets_ms": {"cpu_frame_max": CPU_FRAME_BUDGET_MS, "gpu_frame_p95": GPU_P95_BUDGET_MS},
        "cases": cases,
        "unmeasured": [
            "new-hotpath CPU allocations",
            "full hardware matrix",
            "graph rebuild counter (no public counter available)"
        ],
    });
    let path = output_dir.join("photoscan-performance.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&report).expect("serialize report"),
    )
    .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    eprintln!("[v8 performance] report written to {}", path.display());
}

#[test]
fn f8_photoscan_modifier_performance_240_frames() {
    let mut ct = headless_content_thread(Project::default(), WIDTH, HEIGHT);
    let hardware = ct
        .content_pipeline
        .native_device()
        .expect("native performance device")
        .device_name();
    let (state_tx, _state_rx) = crossbeam_channel::unbounded::<ContentState>();
    let mut cases: Vec<CaseRecord> = Vec::new();

    // Each fixture is loaded and measured sequentially on this one content
    // thread/device. A variant is attempted only after its fixture baseline
    // satisfies the stated CPU/GPU admission budgets.
    for fixture in FIXTURES {
        let baseline =
            measure_case(&mut ct, &state_tx, fixture, "baseline").unwrap_or_else(|failure| {
                panic!("{} baseline failed: {}", fixture.name, failure.reason)
            });
        if let Some(reason) = output_failure(&baseline) {
            cases.push(CaseRecord::Measured(baseline));
            write_report(&cases, &hardware);
            panic!("{} baseline output gate failed: {reason}", fixture.name);
        }
        if baseline.cpu_wall_ms.max_ms > CPU_FRAME_BUDGET_MS
            || baseline.gpu_generators_compositor_ms.p95_ms > GPU_P95_BUDGET_MS
        {
            let cpu = baseline.cpu_wall_ms.max_ms;
            let gpu = baseline.gpu_generators_compositor_ms.p95_ms;
            cases.push(CaseRecord::Measured(baseline));
            write_report(&cases, &hardware);
            panic!(
                "{} baseline exceeds budget (CPU max {cpu:.3}ms, GPU p95 {gpu:.3}ms); report is inconclusive",
                fixture.name
            );
        }
        let baseline_p95_ms = baseline.gpu_generators_compositor_ms.p95_ms;
        cases.push(CaseRecord::Measured(baseline));

        for variant in MODIFIER_VARIANTS {
            match measure_case(&mut ct, &state_tx, fixture, variant) {
                Ok(mut metrics) => {
                    metrics.gpu_p95_delta_from_baseline_ms =
                        Some(metrics.gpu_generators_compositor_ms.p95_ms - baseline_p95_ms);
                    let failure = output_failure(&metrics)
                        .or_else(|| {
                            (metrics.cpu_wall_ms.max_ms > CPU_FRAME_BUDGET_MS).then(|| {
                                format!(
                                    "CPU wall max {:.3}ms exceeds {:.2}ms",
                                    metrics.cpu_wall_ms.max_ms, CPU_FRAME_BUDGET_MS
                                )
                            })
                        })
                        .or_else(|| {
                            (metrics.gpu_generators_compositor_ms.p95_ms > GPU_P95_BUDGET_MS).then(
                                || {
                                    format!(
                                        "GPU p95 {:.3}ms exceeds {:.2}ms",
                                        metrics.gpu_generators_compositor_ms.p95_ms,
                                        GPU_P95_BUDGET_MS
                                    )
                                },
                            )
                        });
                    cases.push(CaseRecord::Measured(metrics));
                    if let Some(reason) = failure {
                        write_report(&cases, &hardware);
                        panic!(
                            "{} {variant} performance gate failed: {reason}",
                            fixture.name
                        );
                    }
                }
                Err(failure) => cases.push(CaseRecord::Unsupported {
                    fixture: fixture.name.into(),
                    variant: variant.into(),
                    source_objects: failure.source_objects,
                    source_triangles: failure.source_triangles,
                    reason: failure.reason,
                }),
            }
        }
    }
    write_report(&cases, &hardware);
}

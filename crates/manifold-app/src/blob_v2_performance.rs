//! Bounded Blob Tracking V2 performance evidence on the real headless app path.
#![cfg(all(
    test,
    target_os = "macos",
    feature = "journey-proofs",
    feature = "perf-soak"
))]

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::content_thread::ContentThread;
use crate::headless_harness::headless_content_thread;
use crossbeam_channel::{Receiver, Sender};
use manifold_core::clip::TimelineClip;
use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::effects::PresetInstance;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::{Beats, Bpm, PresetTypeId};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::ffi::c_void;
use std::time::Instant;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MallocStatistics {
    blocks_in_use: u64,
    size_in_use: u64,
    max_size_in_use: u64,
    size_allocated: u64,
}
unsafe extern "C" {
    fn malloc_default_zone() -> *mut c_void;
    fn malloc_zone_statistics(zone: *mut c_void, statistics: *mut MallocStatistics);
}
fn malloc_snapshot() -> Option<MallocStatistics> {
    // Process-wide live allocations include the app and OpenCV. They are a
    // retained-memory bound, not a transient allocation-event counter.
    unsafe {
        let zone = malloc_default_zone();
        if zone.is_null() {
            return None;
        }
        let mut statistics = MallocStatistics::default();
        malloc_zone_statistics(zone, &mut statistics);
        Some(statistics)
    }
}

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const WARMUP_FRAMES: usize = 60;
const MEASURED_FRAMES: usize = 600;
const PROFILE_SAMPLER_MAX_SPANS: usize = 8192;
const REPORT_DIR: &str = "target/blob-v2-demo";

#[derive(Clone, Copy)]
struct CaseSpec {
    name: &'static str,
    preset: &'static str,
    analysis_max_dim: i64,
    max_blobs: i32,
}
const CASES: [CaseSpec; 5] = [
    CaseSpec {
        name: "legacy_default",
        preset: "BlobTracking",
        analysis_max_dim: 0,
        max_blobs: 0,
    },
    CaseSpec {
        name: "v2_default",
        preset: "BlobTrackingV2",
        analysis_max_dim: 320,
        max_blobs: 8,
    },
    CaseSpec {
        name: "v2_stress",
        preset: "BlobTrackingV2",
        analysis_max_dim: 1024,
        max_blobs: 32,
    },
    CaseSpec {
        name: "motion_default",
        preset: "BlobTrackingV2Motion",
        analysis_max_dim: 320,
        max_blobs: 8,
    },
    CaseSpec {
        name: "motion_stress",
        preset: "BlobTrackingV2Motion",
        analysis_max_dim: 1024,
        max_blobs: 32,
    },
];

#[derive(Debug, Clone, Serialize)]
struct Distribution {
    count: usize,
    median_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}
#[derive(Debug, Clone, Serialize)]
struct FrameDistribution {
    count: usize,
    median_frames: f64,
    p95_frames: f64,
    max_frames: u64,
}
#[derive(Debug, Clone, Serialize)]
struct StageMetrics {
    cpu: Option<Distribution>,
    gpu: Option<Distribution>,
    status: &'static str,
}
#[derive(Debug, Clone, Serialize)]
struct MemoryMetrics {
    samples: usize,
    first_bytes: Option<u64>,
    min_bytes: Option<u64>,
    max_bytes: Option<u64>,
    last_bytes: Option<u64>,
    status: &'static str,
}
#[derive(Debug, Clone, Serialize)]
struct CpuRetainedMemory {
    first_bytes: u64,
    last_bytes: u64,
    net_bytes: i64,
    first_blocks: u64,
    last_blocks: u64,
    net_blocks: i64,
    status: &'static str,
}
#[derive(Debug, Clone, Serialize)]
struct CaseMetrics {
    name: String,
    preset: String,
    resolution: [u32; 2],
    analysis_max_dim: Option<i64>,
    max_blobs: Option<i32>,
    warmup_frames: usize,
    measured_frames: usize,
    wall_ms: Distribution,
    clocked_frame_interval_ms: Distribution,
    gpu_total_ms: Distribution,
    gpu_by_queue_ms: BTreeMap<String, Distribution>,
    stages: BTreeMap<String, StageMetrics>,
    region_worker_ms: Option<Distribution>,
    region_readback_age_frames: Option<FrameDistribution>,
    region_fixed_lag_wait_ms: Option<Distribution>,
    region_capture_to_output_frames: Option<FrameDistribution>,
    region_runs: usize,
    region_missing_mask: usize,
    region_missing_regions: usize,
    region_invalid_dims: usize,
    memory: MemoryMetrics,
    cpu_retained_memory: Option<CpuRetainedMemory>,
    pipeline_compile_cold_touches: u64,
    profile_overflow_dispatches: usize,
    profile_invalid_samples: usize,
    profile_failed_command_buffers: usize,
    output_finite: bool,
    output_visible_pixels: usize,
    output_rgb_range: f32,
}
#[derive(Default)]
struct Samples {
    wall_ms: Vec<f64>,
    clocked_frame_interval_ms: Vec<f64>,
    gpu_total_ms: Vec<f64>,
    gpu_by_queue_ms: BTreeMap<String, Vec<f64>>,
    stage_cpu_ms: BTreeMap<String, Vec<f64>>,
    stage_gpu_ms: BTreeMap<String, Vec<f64>>,
    profile_overflow_dispatches: usize,
    profile_invalid_samples: usize,
    profile_failed_command_buffers: usize,
    memory: Vec<u64>,
}

fn percentile(values: &[f64], q: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite timing"));
    sorted[((sorted.len() as f64 * q).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1)]
}
fn distribution(values: &[f64]) -> Distribution {
    assert!(!values.is_empty(), "performance proof has no samples");
    Distribution {
        count: values.len(),
        median_ms: percentile(values, 0.5),
        p95_ms: percentile(values, 0.95),
        max_ms: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    }
}
fn sampled_distribution(values: &[u64], scale: f64) -> Option<Distribution> {
    (!values.is_empty()).then(|| {
        distribution(
            &values
                .iter()
                .map(|&value| value as f64 * scale)
                .collect::<Vec<_>>(),
        )
    })
}
fn frame_distribution(values: &[u64]) -> Option<FrameDistribution> {
    (!values.is_empty()).then(|| {
        let frames = values.iter().map(|&value| value as f64).collect::<Vec<_>>();
        FrameDistribution {
            count: values.len(),
            median_frames: percentile(&frames, 0.5),
            p95_frames: percentile(&frames, 0.95),
            max_frames: *values.iter().max().unwrap(),
        }
    })
}
fn stage_for_type(type_id: &str) -> Option<&'static str> {
    if type_id.contains("detect_regions") || type_id.contains("region_mask") {
        Some("analysis")
    } else if type_id.contains("track_regions") {
        Some("tracking")
    } else if type_id.contains("optical_flow") {
        Some("fixed_lag")
    } else if type_id.contains("blob_tracker") {
        Some("legacy_analysis")
    } else {
        None
    }
}
fn stage_metrics(samples: &Samples) -> BTreeMap<String, StageMetrics> {
    ["analysis", "legacy_analysis", "tracking", "fixed_lag"]
        .into_iter()
        .map(|name| {
            let cpu = samples.stage_cpu_ms.get(name).map(|v| distribution(v));
            let gpu = samples.stage_gpu_ms.get(name).map(|v| distribution(v));
            let status = if cpu.is_some() || gpu.is_some() {
                "measured from compositor step profiles"
            } else {
                "unavailable: no matching profiled step was emitted"
            };
            (name.to_string(), StageMetrics { cpu, gpu, status })
        })
        .collect()
}
fn memory_metrics(values: &[u64]) -> MemoryMetrics {
    MemoryMetrics {
        samples: values.len(),
        first_bytes: values.first().copied(),
        min_bytes: values.iter().copied().min(),
        max_bytes: values.iter().copied().max(),
        last_bytes: values.last().copied(),
        status: if values.is_empty() {
            "unavailable: native modifier memory snapshot returned None"
        } else {
            "measured: native device currentAllocatedSize"
        },
    }
}
fn set_graph_analysis_resolution(instance: &mut PresetInstance, max_dim: i64) {
    if max_dim == 0 {
        return;
    }
    let mut graph = manifold_renderer::node_graph::bundled_preset_def(instance.effect_type())
        .cloned()
        .unwrap_or_else(|| panic!("missing bundled graph for {}", instance.effect_type()));
    fn visit(nodes: &mut [manifold_core::effect_graph_def::EffectGraphNode], max_dim: i64) {
        for node in nodes {
            if node.node_id.as_str() == "resize" {
                node.params.insert(
                    "max_dim".into(),
                    SerializedParamValue::Int {
                        value: max_dim as i32,
                    },
                );
            }
            if let Some(group) = node.group.as_deref_mut() {
                visit(&mut group.nodes, max_dim);
            }
        }
    }
    visit(&mut graph.nodes, max_dim);
    instance.graph = Some(graph);
    instance.bump_graph_structure_version();
    instance.bump_graph_version();
}
fn performance_effect(spec: CaseSpec) -> PresetInstance {
    let mut instance =
        manifold_core::preset_definition_registry::create_default(&PresetTypeId::new(spec.preset));
    if spec.max_blobs != 0 {
        instance.set_base_param("max_blobs", spec.max_blobs as f32);
    }
    set_graph_analysis_resolution(&mut instance, spec.analysis_max_dim);
    instance
}
fn fixture_project(spec: CaseSpec) -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    project.settings.output_width = WIDTH as i32;
    project.settings.output_height = HEIGHT as i32;
    let mut layer =
        Layer::new_generator("Blob V2 performance source".into(), PresetTypeId::PLASMA, 0);
    layer
        .clips
        .push(TimelineClip::new_generator(Beats::ZERO, Beats(64.0)));
    project.timeline.layers.push(layer);
    project
        .settings
        .master_effects
        .push(performance_effect(spec));
    project.reconcile_param_manifests();
    project
}
fn drain_state(rx: &Receiver<ContentState>) -> Option<ContentState> {
    rx.try_iter().last()
}
fn enable_profiling(ct: &mut ContentThread) {
    ct.content_pipeline
        .set_profiling(true, PROFILE_SAMPLER_MAX_SPANS);
    assert!(
        ct.content_pipeline.profiling_sampler_ready(),
        "native GPU timestamp profiling is required"
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
fn inspect_output(ct: &ContentThread) -> (bool, usize, f32) {
    let device = ct
        .content_pipeline
        .native_device()
        .expect("native performance device");
    let texture = ct.content_pipeline.export_output_texture();
    assert_eq!(texture.format, manifold_gpu::GpuTextureFormat::Rgba16Float);
    let row_bytes = texture.width * texture.format.bytes_per_pixel();
    let size = row_bytes as usize * texture.height as usize;
    let buffer = device.create_buffer_shared(size as u64);
    let mut encoder = device.create_encoder("blob-v2-performance-output");
    encoder.copy_texture_to_buffer(texture, &buffer, texture.width, texture.height, row_bytes);
    encoder.commit_and_wait_completed();
    let ptr = buffer.mapped_ptr().expect("performance readback mapping");
    let bytes = unsafe { std::slice::from_raw_parts(ptr, size) };
    let mut finite = true;
    let mut visible = 0;
    let mut min_rgb = f32::INFINITY;
    let mut max_rgb = f32::NEG_INFINITY;
    for pixel in bytes.chunks_exact(8) {
        let mut c = [0.0_f32; 4];
        for (channel, value) in c.iter_mut().enumerate() {
            *value = half::f16::from_bits(u16::from_le_bytes([
                pixel[channel * 2],
                pixel[channel * 2 + 1],
            ]))
            .to_f32();
            finite &= value.is_finite();
        }
        for value in &c[..3] {
            min_rgb = min_rgb.min(*value);
            max_rgb = max_rgb.max(*value);
        }
        if c[3] > 0.0 && c[..3].iter().any(|value| value.abs() > 1.0e-6) {
            visible += 1;
        }
    }
    (finite, visible, max_rgb - min_rgb)
}
fn measure_case(
    ct: &mut ContentThread,
    state_tx: &Sender<ContentState>,
    state_rx: &Receiver<ContentState>,
    spec: CaseSpec,
) -> CaseMetrics {
    let cold_touch_start = manifold_core::cold_touch::cold_touch_count(
        manifold_core::cold_touch::ColdTouchKind::PipelineCompile,
    );
    ct.handle_command(ContentCommand::LoadProject(Box::new(fixture_project(spec))));
    ct.timer.set_frame_clocked(true);
    ct.handle_command(ContentCommand::Pause);
    ct.handle_command(ContentCommand::SeekToBeat(Beats::ZERO));
    ct.handle_command(ContentCommand::Play);
    for _ in 0..WARMUP_FRAMES {
        ct.tick_frame(state_tx);
        let _ = drain_state(state_rx);
        let _ = ct.content_pipeline.take_gpu_profiles();
        let _ = ct.content_pipeline.take_step_profiles();
    }
    let (output_finite, output_visible_pixels, output_rgb_range) = inspect_output(ct);
    assert!(
        output_finite,
        "{} warmup output contains non-finite values",
        spec.name
    );
    assert!(
        output_visible_pixels > 0,
        "{} warmup output has no visible pixels",
        spec.name
    );
    let mut samples = Samples {
        wall_ms: Vec::with_capacity(MEASURED_FRAMES),
        clocked_frame_interval_ms: Vec::with_capacity(MEASURED_FRAMES),
        gpu_total_ms: Vec::with_capacity(MEASURED_FRAMES),
        ..Samples::default()
    };
    let malloc_before = malloc_snapshot();
    manifold_renderer::node_graph::primitives::start_region_perf_samples();
    for _ in 0..MEASURED_FRAMES {
        let start = Instant::now();
        ct.tick_frame(state_tx);
        samples
            .wall_ms
            .push(start.elapsed().as_secs_f64() * 1_000.0);
        if let Some(state) = drain_state(state_rx) {
            samples
                .clocked_frame_interval_ms
                .push(state.content_frame_time_ms as f64);
        }
        let step_profiles = ct.content_pipeline.take_step_profiles();
        let mut type_by_tag = HashMap::with_capacity(step_profiles.len());
        for profile in &step_profiles {
            if let Some(stage) = stage_for_type(&profile.type_id) {
                samples
                    .stage_cpu_ms
                    .entry(stage.into())
                    .or_default()
                    .push(profile.cpu_nanos as f64 / 1_000_000.0);
            }
            type_by_tag.insert(profile.tag.as_str(), profile.type_id.as_str());
        }
        let gpu_profiles = ct.content_pipeline.take_gpu_profiles();
        let mut total_gpu = 0.0;
        for (queue, profile) in gpu_profiles {
            assert!(profile.total_ms.is_finite() && profile.total_ms >= 0.0);
            total_gpu += profile.total_ms;
            samples
                .gpu_by_queue_ms
                .entry(queue.into())
                .or_default()
                .push(profile.total_ms);
            samples.profile_overflow_dispatches += profile.overflow;
            samples.profile_invalid_samples += profile.invalid;
            samples.profile_failed_command_buffers += profile.failed_command_buffers;
            for span in profile.spans {
                if let Some(type_id) = type_by_tag.get(span.tag.as_str())
                    && let Some(stage) = stage_for_type(type_id)
                {
                    samples
                        .stage_gpu_ms
                        .entry(stage.into())
                        .or_default()
                        .push(span.millis);
                }
            }
        }
        samples.gpu_total_ms.push(total_gpu);
        if let Some(snapshot) = ct
            .content_pipeline
            .native_device()
            .and_then(manifold_gpu::GpuDevice::modifier_memory_snapshot)
        {
            samples.memory.push(snapshot.current_allocated_bytes);
        }
    }
    let region_perf = manifold_renderer::node_graph::primitives::take_region_perf_samples();
    let malloc_after = malloc_snapshot();
    if spec.name != "legacy_default" {
        assert_eq!(
            region_perf.runs, MEASURED_FRAMES,
            "{} detector was skipped",
            spec.name
        );
        assert_eq!(
            region_perf.missing_mask + region_perf.missing_regions + region_perf.invalid_dims,
            0,
            "{} detector received an incomplete or oversized graph input",
            spec.name
        );
        assert!(
            !region_perf.worker_ns.is_empty(),
            "{} detector worker did not run",
            spec.name
        );
        assert!(
            !region_perf.readback_age_frames.is_empty(),
            "{} detector readback did not complete",
            spec.name
        );
    }
    assert_eq!(samples.wall_ms.len(), MEASURED_FRAMES);
    assert_eq!(samples.clocked_frame_interval_ms.len(), MEASURED_FRAMES);
    assert_eq!(samples.gpu_total_ms.len(), MEASURED_FRAMES);
    assert_eq!(
        samples.profile_overflow_dispatches, 0,
        "GPU profiler overflowed"
    );
    assert_eq!(
        samples.profile_invalid_samples, 0,
        "GPU profiler had invalid samples"
    );
    assert_eq!(
        samples.profile_failed_command_buffers, 0,
        "GPU command buffer failed"
    );
    CaseMetrics {
        name: spec.name.into(),
        preset: spec.preset.into(),
        resolution: [WIDTH, HEIGHT],
        analysis_max_dim: (spec.analysis_max_dim != 0).then_some(spec.analysis_max_dim),
        max_blobs: (spec.max_blobs != 0).then_some(spec.max_blobs),
        warmup_frames: WARMUP_FRAMES,
        measured_frames: MEASURED_FRAMES,
        wall_ms: distribution(&samples.wall_ms),
        clocked_frame_interval_ms: distribution(&samples.clocked_frame_interval_ms),
        gpu_total_ms: distribution(&samples.gpu_total_ms),
        gpu_by_queue_ms: samples
            .gpu_by_queue_ms
            .iter()
            .map(|(n, v)| (n.clone(), distribution(v)))
            .collect(),
        stages: stage_metrics(&samples),
        region_worker_ms: sampled_distribution(&region_perf.worker_ns, 1.0e-6),
        region_readback_age_frames: frame_distribution(&region_perf.readback_age_frames),
        region_fixed_lag_wait_ms: sampled_distribution(&region_perf.fixed_lag_wait_ns, 1.0e-6),
        region_capture_to_output_frames: frame_distribution(&region_perf.capture_to_output_frames),
        region_runs: region_perf.runs,
        region_missing_mask: region_perf.missing_mask,
        region_missing_regions: region_perf.missing_regions,
        region_invalid_dims: region_perf.invalid_dims,
        memory: memory_metrics(&samples.memory),
        cpu_retained_memory: malloc_before.zip(malloc_after).map(|(first, last)| {
            CpuRetainedMemory {
                first_bytes: first.size_in_use,
                last_bytes: last.size_in_use,
                net_bytes: last.size_in_use as i64 - first.size_in_use as i64,
                first_blocks: first.blocks_in_use,
                last_blocks: last.blocks_in_use,
                net_blocks: last.blocks_in_use as i64 - first.blocks_in_use as i64,
                status: "measured: process-wide malloc-zone live allocations after warmup and measurement; includes app, Metal and OpenCV",
            }
        }),
        pipeline_compile_cold_touches: manifold_core::cold_touch::cold_touch_count(
            manifold_core::cold_touch::ColdTouchKind::PipelineCompile,
        )
        .saturating_sub(cold_touch_start),
        profile_overflow_dispatches: samples.profile_overflow_dispatches,
        profile_invalid_samples: samples.profile_invalid_samples,
        profile_failed_command_buffers: samples.profile_failed_command_buffers,
        output_finite,
        output_visible_pixels,
        output_rgb_range,
    }
}
fn write_report(cases: &[CaseMetrics], hardware: &str) {
    let legacy_p95 = cases
        .iter()
        .find(|c| c.name == "legacy_default")
        .map(|c| c.gpu_total_ms.p95_ms);
    let comparisons = cases.iter().map(|case| (case.name.clone(), serde_json::json!({
        "gpu_total_p95_delta_from_legacy_ms": legacy_p95.map(|base| case.gpu_total_ms.p95_ms - base),
        "app_tick_p95_delta_from_legacy_ms": cases.iter().find(|c| c.name == "legacy_default").map(|base| case.wall_ms.p95_ms - base.wall_ms.p95_ms),
    }))).collect::<BTreeMap<_, _>>();
    let report = serde_json::json!({
        "mode": "blob_v2_headless_performance", "hardware": hardware, "source": "Plasma generator, fixed 64-beat clip",
        "measurement": "wall_ms measures ContentThread::tick_frame processing; clocked_frame_interval_ms is the frame-clock interval and is not processing cost. Region worker duration, readback age, fixed-lag blocking and capture-to-output age are sampled at the detector seam. Stage CPU profiles exclude the detector worker.",
        "same_device_and_harness": true, "cases": cases, "comparisons": comparisons,
        "unmeasured": {
            "optical_flow_worker_ms": "unavailable: the existing optical-flow worker is not instrumented by this detector probe",
            "gpu_readback_copy_ms": "unavailable: only submission-to-consumption age in frames is sampled",
            "allocation_events": "unavailable: no allocation event counter is exposed; memory snapshot is reported instead",
            "retained_blob_state_bytes": "unavailable separately: cpu_retained_memory reports process-wide net live bytes, not detector-only state",
        },
        "native_abi_reference": {"script": "scripts/blob_v2_native_bench.py", "allocation_events": "unmeasured by the referenced benchmark"},
    });
    let path = std::path::Path::new(REPORT_DIR).join("blob_v2_performance.json");
    std::fs::create_dir_all(REPORT_DIR).expect("Blob V2 report directory");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&report).expect("serialize Blob V2 report"),
    )
    .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    eprintln!("[blob-v2 performance] report written to {}", path.display());
}
fn write_unavailable_report(reason: &str) {
    let report = serde_json::json!({
        "mode": "blob_v2_headless_performance",
        "run_status": "unavailable",
        "resolution": [WIDTH, HEIGHT],
        "warmup_frames": WARMUP_FRAMES,
        "measured_frames": MEASURED_FRAMES,
        "reason": reason,
        "unmeasured": {
            "all_gpu_and_content_timings": "unavailable: headless native device could not be created",
            "analysis_worker_ms": "unavailable",
            "readback_ms": "unavailable",
            "fixed_lag_queue_depth": "unavailable",
            "allocation_events": "unavailable",
            "retained_blob_state_bytes": "unavailable"
        }
    });
    let path = std::path::Path::new(REPORT_DIR).join("blob_v2_performance.json");
    std::fs::create_dir_all(REPORT_DIR).expect("Blob V2 report directory");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&report).expect("serialize unavailable report"),
    )
    .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    eprintln!(
        "[blob-v2 performance] unavailable report written to {}",
        path.display()
    );
}

#[test]
fn blob_v2_performance_proof_60_warmup_600_measured() {
    let mut ct: ContentThread = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        headless_content_thread(Project::default(), WIDTH, HEIGHT)
    })) {
        Ok(ct) => ct,
        Err(_) => {
            let reason = "native Metal device unavailable: headless_content_thread could not construct GpuDevice";
            write_unavailable_report(reason);
            panic!("{reason}");
        }
    };
    enable_profiling(&mut ct);
    let hardware = ct
        .content_pipeline
        .native_device()
        .expect("native performance device")
        .device_name();
    let (state_tx, state_rx) = crossbeam_channel::unbounded::<ContentState>();
    let mut cases = Vec::with_capacity(CASES.len());
    for spec in CASES {
        cases.push(measure_case(&mut ct, &state_tx, &state_rx, spec));
    }
    write_report(&cases, &hardware);
    let legacy = cases
        .iter()
        .find(|case| case.name == "legacy_default")
        .unwrap();
    let v2 = cases.iter().find(|case| case.name == "v2_default").unwrap();
    assert!(
        v2.wall_ms.p95_ms - legacy.wall_ms.p95_ms <= 2.0,
        "default V2 app-tick p95 added more than 2 ms over legacy"
    );
    assert!(
        v2.wall_ms.max_ms <= 20.0,
        "default V2 app tick exceeded 20 ms"
    );
}

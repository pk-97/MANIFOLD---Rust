//! SCENE_MODIFIER_RT_ACCEPTANCE.md A9/P8 — one bounded RT maintenance
//! measurement. The harness intentionally measures the real Metal BLAS/TLAS
//! commands on a deterministic 65,536-triangle grid. It records the actual
//! update and trace timings, operation counters, memory snapshots, and idle
//! behavior. A separate production path below renders the same workload
//! through `PresetRuntime` at 1280x720; held-out parsing remains explicit
//! when the project format is unavailable to this bounded proof.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use manifold_core::NodeId;
use manifold_foundation::cold_touch::{ColdTouchKind, cold_touch_count};
use manifold_gpu::raytrace::{
    DebugRayQueryRay, MetalShadowRayTracer, RtAccelUpdate, RtGeometryChange, RtObjectGeometry,
    ShadowRayTracer, ensure_normal_sources,
};
use manifold_gpu::{GpuBuffer, GpuDevice, GpuTextureFormat};
use manifold_renderer::frame_status::FrameRenderStatus;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::{ParamValue, PrimitiveRegistry};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::harness;

const WARMUP_FRAMES: usize = 16;
const MEASURED_FRAMES: usize = 120;
const TOTAL_FRAMES: usize = WARMUP_FRAMES + MEASURED_FRAMES;
const GRID_TRIANGLES: usize = 65_536;
const GRID_COLS: usize = 257;
const GRID_ROWS: usize = 129;

#[repr(C)]
#[derive(Clone, Copy)]
struct PackedVertex {
    position: [f32; 4],
    normal: [f32; 4],
    uv: [f32; 2],
}

const VERTEX_STRIDE: u32 = std::mem::size_of::<PackedVertex>() as u32;
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

#[derive(Clone, Copy)]
enum Configuration {
    Off,
    Static,
    DynamicRefit,
    FreshBuild,
}

impl Configuration {
    fn name(self) -> &'static str {
        match self {
            Self::Off => "rt_off",
            Self::Static => "static_rt",
            Self::DynamicRefit => "dynamic_selective_refit",
            Self::FreshBuild => "fresh_build_reference",
        }
    }
}

fn grid_vertices() -> Vec<PackedVertex> {
    let mut vertices = Vec::with_capacity(GRID_TRIANGLES * 3);
    for row in 0..GRID_ROWS - 1 {
        for col in 0..GRID_COLS - 1 {
            let point = |x: usize, y: usize| PackedVertex {
                position: [x as f32 / 128.0 - 1.0, y as f32 / 64.0 - 1.0, 0.0, 1.0],
                normal: [0.0, 0.0, 1.0, 0.0],
                uv: [x as f32 / 256.0, y as f32 / 128.0],
            };
            let p00 = point(col, row);
            let p10 = point(col + 1, row);
            let p01 = point(col, row + 1);
            let p11 = point(col + 1, row + 1);
            vertices.extend_from_slice(&[p00, p10, p01, p01, p10, p11]);
        }
    }
    assert_eq!(vertices.len() / 3, GRID_TRIANGLES);
    vertices
}

fn write_vertices(device: &GpuDevice, vertices: &[PackedVertex]) -> GpuBuffer {
    let buffer = device.create_buffer_shared(std::mem::size_of_val(vertices) as u64);
    let ptr = buffer.mapped_ptr().expect("grid buffer must be CPU-mapped");
    unsafe {
        std::ptr::copy_nonoverlapping(
            vertices.as_ptr(),
            ptr.cast::<PackedVertex>(),
            vertices.len(),
        )
    };
    buffer
}

fn deform_grid(buffer: &GpuBuffer, frame: usize) {
    let ptr = buffer
        .mapped_ptr()
        .expect("grid buffer must be CPU-mapped")
        .cast::<PackedVertex>();
    let phase = frame as f32 * 0.07;
    for row in 0..GRID_ROWS - 1 {
        for col in 0..GRID_COLS - 1 {
            let wave =
                ((col as f32 * 0.09 + phase).sin() * (row as f32 * 0.05 + phase).cos()) * 0.08;
            let first = (row * (GRID_COLS - 1) + col) * 6;
            for index in [first, first + 1, first + 2, first + 3, first + 4, first + 5] {
                unsafe { (*ptr.add(index)).position[2] = wave };
            }
        }
    }
}

fn small_triangle(device: &GpuDevice) -> GpuBuffer {
    let vertices = [
        PackedVertex {
            position: [1.5, -0.25, 0.0, 1.0],
            normal: [0.0, 0.0, 1.0, 0.0],
            uv: [0.0, 0.0],
        },
        PackedVertex {
            position: [1.9, -0.25, 0.0, 1.0],
            normal: [0.0, 0.0, 1.0, 0.0],
            uv: [1.0, 0.0],
        },
        PackedVertex {
            position: [1.7, 0.25, 0.0, 1.0],
            normal: [0.0, 0.0, 1.0, 0.0],
            uv: [0.5, 1.0],
        },
    ];
    write_vertices(device, &vertices)
}

fn object<'a>(buffer: &'a GpuBuffer, triangles: u32) -> RtObjectGeometry<'a> {
    RtObjectGeometry { material_attributes: Default::default(),
        vertex_buffer: buffer,
        vertex_stride: VERTEX_STRIDE,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: triangles,
        transform: IDENTITY,
        normal_offset: 16,
        uv_offset: 32,
        alpha_mask: false,
        translucent: false,
        alpha_cutoff: 0.5,
        base_color_texture: None,
        mr_texture: None,
        normal_texture: None,
        emissive_texture: None,
        extra_material_textures: [None; 3],
        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
        emissive_uv_t: [0.0, 0.0],
        cast_shadows: true,
        instances_addr: 0,
        instances_buffer: None,
        instance_slots: 0,
        appearance_weights: None,
        appearance_gain: 1.0,
        base_color_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        mr_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        normal_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        normal_scale: 1.0,
        base_color_alpha: 1.0,
        tangent_offset: u32::MAX,
    }
}

fn percentile(samples: &[f64], fraction: f64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted.get(index).copied()
}

fn timing_summary(samples: &[f64], missing: usize) -> Value {
    json!({
        "count": samples.len(),
        "missing": missing,
        "p50Ms": percentile(samples, 0.50),
        "p95Ms": percentile(samples, 0.95),
        "maxMs": samples.iter().copied().max_by(f64::total_cmp),
    })
}

fn update_json(update: RtAccelUpdate) -> Value {
    json!({
        "blasBuilds": update.blas_builds,
        "blasRefits": update.blas_refits,
        "tlasBuilds": update.tlas_builds,
        "tlasRefits": update.tlas_refits,
        "emissiveRefreshes": update.emissive_refreshes,
    })
}

fn timed_commit(encoder: manifold_gpu::GpuEncoder) -> Option<f64> {
    let (sender, receiver) = mpsc::channel();
    encoder.add_gpu_time_handler(move |seconds| {
        let _ = sender.send(seconds * 1000.0);
    });
    encoder.commit_and_wait_completed();
    receiver
        .recv_timeout(Duration::from_secs(1))
        .ok()
        .filter(|millis| millis.is_finite() && *millis > 0.0)
}

fn run_configuration(device: &GpuDevice, configuration: Configuration) -> Value {
    let grid = grid_vertices();
    let grid_buffer = write_vertices(device, &grid);
    let static_buffer = small_triangle(device);
    let objects = [
        object(&grid_buffer, GRID_TRIANGLES as u32),
        object(&static_buffer, 1),
    ];

    let before = device.modifier_memory_snapshot();
    if matches!(configuration, Configuration::Off) {
        let mut cpu_samples = Vec::with_capacity(MEASURED_FRAMES);
        for _ in 0..TOTAL_FRAMES {
            let start = Instant::now();
            std::hint::black_box(&objects);
            if cpu_samples.len() < MEASURED_FRAMES {
                cpu_samples.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        return json!({
            "name": configuration.name(),
            "status": "blocked",
            "reason": "RT-off control has no renderer frame target in this direct AS harness",
            "cpuEncode": timing_summary(&cpu_samples, 0),
            "gpuFrame": timing_summary(&[], MEASURED_FRAMES),
            "gpuAsMaintenance": timing_summary(&[], MEASURED_FRAMES),
            "operations": update_json(RtAccelUpdate::default()),
            "memory": {"before": before.map(|s| s.current_allocated_bytes), "after": before.map(|s| s.current_allocated_bytes)},
        });
    }

    let tracer = MetalShadowRayTracer::new(device);
    let plan = tracer
        .plan_accel(device, None, &objects)
        .expect("perf plan");
    let admitted_bytes = plan.additional_peak_bytes();
    let mut resident = None;
    tracer
        .prepare_accel(device, &mut resident, plan)
        .expect("perf prepare");
    let mut accel = resident.expect("perf resident");
    let after_prepare = device.modifier_memory_snapshot();

    let mut normal_slot = None;
    let mut normal_capacity = 0usize;
    ensure_normal_sources(&mut normal_slot, &mut normal_capacity, device, &objects);
    let normal_sources = normal_slot.expect("perf normal sources");
    let ray = DebugRayQueryRay {
        origin: [0.0, 0.0, 2.0],
        direction: [0.0, 0.0, -1.0],
        min_distance: 0.0,
        max_distance: 10.0,
    };

    let mut cpu_samples = Vec::with_capacity(MEASURED_FRAMES);
    let mut maintenance_samples = Vec::with_capacity(MEASURED_FRAMES);
    let mut trace_samples = Vec::with_capacity(MEASURED_FRAMES);
    let mut missing_maintenance = 0usize;
    let mut missing_trace = 0usize;
    let mut operation_totals = RtAccelUpdate::default();
    let mut idle_frames = 0usize;
    let faults_before = manifold_gpu::gpu_fault::fault_count();

    for frame in 0..TOTAL_FRAMES {
        if matches!(
            configuration,
            Configuration::DynamicRefit | Configuration::FreshBuild
        ) && frame > 0
        {
            deform_grid(&grid_buffer, frame);
        }
        let change = if frame == 0 {
            RtGeometryChange::Rebuild
        } else {
            match configuration {
                Configuration::Static => RtGeometryChange::Reuse,
                Configuration::DynamicRefit => RtGeometryChange::Refit,
                Configuration::FreshBuild => RtGeometryChange::Rebuild,
                Configuration::Off => unreachable!(),
            }
        };
        let mut update_encoder = device.create_encoder("rt-dynamic-perf-maintenance");
        let encode_start = Instant::now();
        let update = tracer
            .encode_accel_update(
                device,
                &mut update_encoder,
                &mut accel,
                &objects,
                &[change, RtGeometryChange::Reuse],
                &[],
                false,
                false,
            )
            .expect("perf update");
        let encode_ms = encode_start.elapsed().as_secs_f64() * 1000.0;
        let maintenance_ms = timed_commit(update_encoder);

        let mut trace_encoder = device.create_encoder("rt-dynamic-perf-trace");
        let hits = tracer.debug_ray_query(
            device,
            &mut trace_encoder,
            &accel,
            &normal_sources,
            &[ray],
            None,
            0,
            0,
        );
        let trace_ms = timed_commit(trace_encoder);
        let hit_ptr = hits.mapped_ptr().expect("perf hit buffer must be mapped");
        let hit = unsafe {
            hit_ptr
                .cast::<manifold_gpu::raytrace::DebugRayQueryHit>()
                .read_unaligned()
        };
        assert_eq!(
            hit.hit, 1,
            "perf ray must hit the dynamic grid at frame {frame}"
        );

        if frame >= WARMUP_FRAMES {
            cpu_samples.push(encode_ms);
            match maintenance_ms {
                Some(millis) => maintenance_samples.push(millis),
                None => missing_maintenance += 1,
            }
            match trace_ms {
                Some(millis) => trace_samples.push(millis),
                None => missing_trace += 1,
            }
            operation_totals.blas_builds += update.blas_builds;
            operation_totals.blas_refits += update.blas_refits;
            operation_totals.tlas_builds += update.tlas_builds;
            operation_totals.tlas_refits += update.tlas_refits;
            operation_totals.emissive_refreshes += update.emissive_refreshes;
            if update == RtAccelUpdate::default() {
                idle_frames += 1;
            }
        }
    }
    let faults_after = manifold_gpu::gpu_fault::fault_count();
    let memory_after = device.modifier_memory_snapshot();
    let current_peak = memory_after
        .as_ref()
        .zip(before.as_ref())
        .map(|(after, before)| {
            after
                .current_allocated_bytes
                .max(before.current_allocated_bytes)
        });
    let expected_idle = matches!(configuration, Configuration::Static);
    if expected_idle {
        assert_eq!(
            idle_frames, MEASURED_FRAMES,
            "static RT must be idle after warmup"
        );
    }
    assert_eq!(
        faults_after, faults_before,
        "performance run must fault no GPU command buffers"
    );

    json!({
        "name": configuration.name(),
        "status": if missing_maintenance == 0 && missing_trace == 0 { "measured_as_only" } else { "blocked_missing_gpu_timing" },
        "referenceResolution": [1280, 720],
        "warmupFrames": WARMUP_FRAMES,
        "measuredFrames": MEASURED_FRAMES,
        "geometry": {"objects": 2, "dynamicTriangles": GRID_TRIANGLES, "staticTriangles": 1},
        "cpuEncode": timing_summary(&cpu_samples, 0),
        "gpuAsMaintenance": timing_summary(&maintenance_samples, missing_maintenance),
        "gpuTraceProbe": timing_summary(&trace_samples, missing_trace),
        "operations": update_json(operation_totals),
        "idleFrames": idle_frames,
        "expectedIdleFrames": if expected_idle { MEASURED_FRAMES } else { 0 },
        "memory": {
            "beforeBytes": before.map(|s| s.current_allocated_bytes),
            "afterPrepareBytes": after_prepare.map(|s| s.current_allocated_bytes),
            "afterBytes": memory_after.map(|s| s.current_allocated_bytes),
            "peakObservedBytes": current_peak,
            "admittedBytes": admitted_bytes,
        },
        "blocked": [
            "This direct AS harness does not provide a full 1280x720 renderer frame or RtQualityColumn trace.",
            "Held-out project metadata and static pre-change baseline are supplied by the perf runner, not embedded in the proof.",
        ],
    })
}

/// The production-frame fixture is deliberately separate from the direct AS
/// fixture above. It exercises the same `PresetRuntime`/`render_scene` path
/// used by the renderer at the requested 1280x720 output size: a generated
/// 257x129 grid (65,536 triangles), a one-triangle emissive object, and a sun.
fn production_scene_json(rt_enabled: bool) -> String {
    let mut graph: Value = serde_json::from_str(include_str!(
        "../fixtures/scene-modifiers/rt_dynamic_reference.json"
    ))
    .unwrap();
    let scene = graph["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|node| node["nodeId"] == "scene")
        .unwrap();
    scene["params"]["rt_enabled"]["value"] = rt_enabled.into();
    graph.to_string()
}

fn run_production_configuration(device: &Arc<GpuDevice>, configuration: Configuration) -> Value {
    let registry = PrimitiveRegistry::with_builtin();
    let rt_enabled = !matches!(configuration, Configuration::Off);
    let scene_json = production_scene_json(rt_enabled);
    let make_runtime = || {
        PresetRuntime::from_json_str_with_device(
            &scene_json,
            &registry,
            Arc::clone(device),
            1280,
            720,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("production RT perf scene must compile")
    };
    let mut runtime = make_runtime();
    let target = RenderTarget::new(
        device,
        1280,
        720,
        GpuTextureFormat::Rgba16Float,
        "rt-dynamic-perf-production",
    );
    let mut cpu_samples = Vec::with_capacity(MEASURED_FRAMES);
    let mut gpu_samples = Vec::with_capacity(MEASURED_FRAMES);
    let mut missing_gpu = 0usize;
    let mut status_counts = [0usize; 3];
    let mut operation_totals = RtAccelUpdate::default();
    let mut dispatch_frames = 0usize;
    let mut any_dispatch = false;
    let mut observed_triangles = Vec::new();
    let mut idle_frames = 0usize;
    let faults_before = manifold_gpu::gpu_fault::fault_count();
    let allocation_before = device.allocation_counts();
    let cold_before = cold_touch_count(ColdTouchKind::PipelineCompile);
    let mut cold_after_warmup = cold_before;
    let mut allocation_after_warmup = allocation_before;

    for frame in 0..TOTAL_FRAMES {
        if matches!(
            configuration,
            Configuration::DynamicRefit | Configuration::FreshBuild
        ) {
            let instance = runtime
                .graph
                .instance_by_node_id(&NodeId::new("wave"))
                .expect("production wave node must exist");
            runtime
                .graph
                .set_param(instance, "phase", ParamValue::Float(frame as f32 * 0.07))
                .expect("production wave phase must be mutable");
        }
        let ctx = PresetContext {
            time: frame as f64 / 60.0,
            beat: frame as f64 / 30.0,
            dt: 1.0 / 60.0,
            width: 1280,
            height: 720,
            output_width: 1280,
            output_height: 720,
            aspect: 1280.0 / 720.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame as i64,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let cpu_start = Instant::now();
        let mut encoder = device.create_encoder("rt-dynamic-perf-production");
        let (status, updates, dispatches) = {
            let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
            gpu.force_rt_rebuild = matches!(configuration, Configuration::FreshBuild);
            gpu.capture_rt_geometry = rt_enabled && frame == WARMUP_FRAMES - 1;
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
            (gpu.frame_status(), gpu.rt_updates, gpu.rt_dispatches)
        };
        let cpu_ms = cpu_start.elapsed().as_secs_f64() * 1000.0;
        let gpu_ms = timed_commit(encoder);
        if frame == WARMUP_FRAMES - 1 {
            if rt_enabled {
                observed_triangles = runtime
                    .rt_probe_scene()
                    .expect("actual RT geometry")
                    .objects
                    .iter()
                    .map(|object| object.triangle_count)
                    .collect();
                assert_eq!(observed_triangles, vec![GRID_TRIANGLES as u32, 2]);
            }
            cold_after_warmup = cold_touch_count(ColdTouchKind::PipelineCompile);
            allocation_after_warmup = device.allocation_counts();
        }
        any_dispatch |= dispatches > 0;
        if frame >= WARMUP_FRAMES {
            cpu_samples.push(cpu_ms);
            match gpu_ms {
                Some(ms) => gpu_samples.push(ms),
                None => missing_gpu += 1,
            }
            match status {
                FrameRenderStatus::Complete => status_counts[0] += 1,
                FrameRenderStatus::PendingGeometry => status_counts[1] += 1,
                FrameRenderStatus::Failed(_) => status_counts[2] += 1,
            }
            operation_totals.blas_builds += updates.blas_builds;
            operation_totals.blas_refits += updates.blas_refits;
            operation_totals.tlas_builds += updates.tlas_builds;
            operation_totals.tlas_refits += updates.tlas_refits;
            operation_totals.emissive_refreshes += updates.emissive_refreshes;
            if updates == RtAccelUpdate::default() {
                idle_frames += 1;
            }
            if dispatches > 0 {
                dispatch_frames += 1;
            }
        }
    }
    let faults_after = manifold_gpu::gpu_fault::fault_count();
    let allocation_after = device.allocation_counts();
    let cold_after = cold_touch_count(ColdTouchKind::PipelineCompile);
    let memory = device.modifier_memory_snapshot();
    let status_ok = status_counts[0] == MEASURED_FRAMES && status_counts[2] == 0;
    let gpu_ok = missing_gpu == 0;
    let dispatch_ok = if matches!(configuration, Configuration::Off) {
        dispatch_frames == 0
    } else {
        dispatch_frames == MEASURED_FRAMES
    };
    let allocations_stable = allocation_after[0] == allocation_after_warmup[0]
        && allocation_after[2] == allocation_after_warmup[2];
    let allocation_ok = allocations_stable;
    let execution_ok = faults_after == faults_before && cold_after == cold_after_warmup;
    json!({
        "name": configuration.name(),
        "status": if status_ok && gpu_ok && dispatch_ok && allocation_ok && execution_ok { "measured_production_frame" } else { "blocked_incomplete_production_frame" },
        "resolution": [1280, 720],
        "warmupFrames": WARMUP_FRAMES,
        "measuredFrames": MEASURED_FRAMES,
        "geometry": {"observedTriangleCounts": observed_triangles, "objects": 2, "dynamicTriangles": GRID_TRIANGLES, "staticTriangles": 2, "dynamicModifier": "SurfaceWaves/normal_wave_mesh"},
        "cpuEncode": timing_summary(&cpu_samples, 0),
        "gpuFrame": timing_summary(&gpu_samples, missing_gpu),
        "frameStatus": {"complete": status_counts[0], "pendingGeometry": status_counts[1], "failed": status_counts[2]},
        "operations": update_json(operation_totals),
        "dispatchFrames": dispatch_frames,
        "anyDispatch": any_dispatch,
        "idleFrames": idle_frames,
        "memory": {
            "snapshot": memory.map(|s| s.current_allocated_bytes),
            "allocationCountsBefore": allocation_before,
            "allocationCountsAfter": allocation_after,
            "afterWarmup": allocation_after_warmup,
            "postWarmupBufferAllocations": allocation_after[0].saturating_sub(allocation_after_warmup[0]),
            "postWarmupTextureAllocations": allocation_after[1].saturating_sub(allocation_after_warmup[1]),
            "postWarmupAccelerationStructureAllocations": allocation_after[2].saturating_sub(allocation_after_warmup[2]),
        },
        "pipelineCompiles": {
            "before": cold_before,
            "throughWarmup": cold_after_warmup.saturating_sub(cold_before),
            "afterWarmup": cold_after.saturating_sub(cold_after_warmup),
        },
        "gpuFaults": faults_after.saturating_sub(faults_before),
        "blocked": if status_ok && gpu_ok && dispatch_ok && allocation_ok && execution_ok { Vec::<String>::new() } else { vec!["Production frame did not provide complete status, GPU timestamps, RT dispatch evidence, and post-warmup allocation stability for every requested mode.".to_string()] },
    })
}

#[test]
fn rt_dynamic_perf_bounded_a9() {
    let harness = harness::shared();
    let device = &harness.device;
    let reports = [
        run_configuration(device, Configuration::Off),
        run_configuration(device, Configuration::Static),
        run_configuration(device, Configuration::DynamicRefit),
        run_configuration(device, Configuration::FreshBuild),
    ];
    let production_reports = [
        run_production_configuration(&harness.device, Configuration::Off),
        run_production_configuration(&harness.device, Configuration::Static),
        run_production_configuration(&harness.device, Configuration::DynamicRefit),
        run_production_configuration(&harness.device, Configuration::FreshBuild),
    ];
    let report = json!({
        "schemaVersion": 1,
        "mode": "perf",
        "hardware": {"gpu": device.device_name(), "nativeMetal": true},
        "settings": {
            "resolution": [1280, 720],
            "shadows": 1,
            "ao": 4,
            "gi": 4,
            "reflections": 8,
            "rayResolution": "Half",
            "spatialDenoise": "Medium",
            "validationLayers": std::env::var_os("MANIFOLD_GPU_VALIDATION").is_some(),
        },
        "referenceScene": {"status": "measured_production_frame", "dynamicTriangles": GRID_TRIANGLES, "objects": 2, "resolution": [1280, 720]},
        "referenceProjectHash": format!("{:x}", Sha256::digest(include_bytes!("../fixtures/scene-modifiers/rt_dynamic_reference.json"))),
        "configurations": reports,
        "productionConfigurations": production_reports,
        "gates": {
            "fullGpuFrameP95Ms": production_reports[2]["gpuFrame"]["p95Ms"].clone(),
            "rendererCpuEncodeP95Ms": production_reports[2]["cpuEncode"]["p95Ms"].clone(),
            "dynamicAsP95Ms": reports[2]["gpuAsMaintenance"]["p95Ms"].clone(),
            "staticRegression": "compare productionConfigurations.static_rt with the saved report on the same GPU",
            "status": "measured_without_historical_baseline",
        },
    });
    let path = std::env::var_os("MANIFOLD_RT_PERF_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/manifold-rt-dynamic/rt_dynamic_perf.json"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("perf report directory must be creatable");
    }
    fs::write(
        &path,
        serde_json::to_vec_pretty(&report).expect("perf report must serialize"),
    )
    .expect("perf report must be writable");
    println!("RT_DYNAMIC_PERF_REPORT {}", path.display());
    assert!(
        production_reports
            .iter()
            .all(|case| case["status"] == "measured_production_frame"),
        "production RT performance correctness/resource gate failed: {}",
        path.display()
    );
    println!(
        "{}",
        serde_json::to_string(&report).expect("perf report must serialize")
    );
}

//! GPU_FLUID_SURFACE_DESIGN.md D20 (surface budget) / P6 — GPU time of the
//! Liquid Surface group on "Water — Dam Break (GPU Surface)" at 1920×1080.
//! Per simulation resolution the preset runs 90 offline ticks, then holds the
//! transport at tick 90 and steps Surface Detail 0 → 1 → 2 live through its
//! card binding (resolution scale 2 → 3 → 4, the performer gesture: the
//! surface sharpens without restarting the simulation). Each step renders 16
//! warm-up and 120 measured frames with per-dispatch GPU timestamps.
//! Gate: p95 of the group's summed GPU time ≤ 6.0 ms at resolution 64,
//! scale 2 with the preset's look (M4 Max). Every other configuration is
//! reported, not gated, including level-set smoothing at 1 and 3 passes at
//! res 64 scale 2 (the group's `smoothing_passes`; the preset uses 2) and
//! the isotropic blob look (FLIP's sphere union) at res 64 scales 2 and 3.

use std::collections::BTreeMap;
use std::process::Command;

use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::{GpuDevice, GpuTextureFormat, GpuTimestampSampler};
use manifold_renderer::frame_status::FrameRenderStatus;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::{PrimitiveRegistry, physics::PhysicsStepScope};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;
use serde_json::Value;

use crate::harness;

const PRESET: &str = include_str!("../fixtures/cpu-flip/WaterDamBreakGpu.json");
const SURFACE_ATOMS: [&str; 8] = [
    "node.sort_particles_into_cells",
    "node.shape_particle_blobs",
    "node.particle_volume",
    "node.smooth_lattice",
    "node.clamp_liquid_to_solids",
    "node.count_surface_triangles",
    "node.running_total",
    "node.volume_surface_mesh",
];
const WARMUP_FRAMES: usize = 16;
const MEASURED_FRAMES: usize = 120;
const TICKS: u32 = 90;
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
/// GPU_FLUID_SURFACE_DESIGN.md D20, re-baselined from the unmeasured 3 ms (2026-09-30).
const BUDGET_MS: f64 = 6.0;

/// One simulation run: a resolution, Liquid Surface group params and Particle
/// Blobs params that differ from the preset's, and the Surface Details
/// stepped live at tick 90.
struct Run {
    resolution: u32,
    group: &'static [(&'static str, f64)],
    blobs: &'static [(&'static str, f64)],
    details: &'static [u32],
}

/// FLIP's sphere union through the same atoms (GPU_FLUID_SURFACE_DESIGN.md
/// P6e, BUG-4snj (Liquid Surface default look)).
const ISOTROPIC: &[(&str, f64)] = &[("stretch", 1.0), ("smoothing", 0.0), ("isolated_scale", 1.0)];

const RUNS: [Run; 6] = [
    Run { resolution: 32, group: &[], blobs: &[], details: &[0, 1, 2] },
    Run { resolution: 48, group: &[], blobs: &[], details: &[0, 1, 2] },
    Run { resolution: 64, group: &[], blobs: &[], details: &[0, 1, 2] },
    Run { resolution: 64, group: &[("smoothing_passes", 1.0)], blobs: &[], details: &[0] },
    Run { resolution: 64, group: &[("smoothing_passes", 3.0)], blobs: &[], details: &[0] },
    Run { resolution: 64, group: &[], blobs: ISOTROPIC, details: &[0, 1] },
];

/// The preset at `run`'s resolution and group params. Card bindings overwrite
/// node params at build, so the resolution card moves with the node param.
fn preset(run: &Run) -> Value {
    let mut json: Value = serde_json::from_str(PRESET).expect("GPU dam break preset parses");
    let cards = [("resolution", f64::from(run.resolution))];
    for node in json["nodes"].as_array_mut().expect("nodes") {
        if node["nodeId"] == "fluid_surface" {
            node["params"]["resolution"]["value"] = Value::from(run.resolution);
        }
        if node["nodeId"] == "liquid_surface" {
            for &(name, value) in run.group {
                let param = &mut node["params"][name];
                param["value"] = if param["type"] == "Int" { Value::from(value as i64) } else { Value::from(value) };
            }
            for inner in node["group"]["nodes"].as_array_mut().expect("group nodes") {
                if inner["nodeId"] == "liquid_blobs" {
                    for &(name, value) in run.blobs {
                        inner["params"][name]["value"] = Value::from(value);
                    }
                }
            }
        }
    }
    for (list, key) in [("params", "id"), ("bindings", "id")] {
        for entry in json["presetMetadata"][list].as_array_mut().expect("card params") {
            for (card, value) in &cards {
                if entry[key] == *card {
                    entry["defaultValue"] = Value::from(*value);
                }
            }
        }
    }
    json
}

fn manifest(json: &Value, surface_detail: f32) -> ParamManifest {
    let specs: Vec<ParamSpecDef> =
        serde_json::from_value(json["presetMetadata"]["params"].clone()).expect("card params");
    let mut manifest = ParamManifest::from_params(specs.into_iter().map(Param::bundled).collect());
    let detail = manifest.get_mut("surface_detail").expect("Surface Detail card param");
    detail.value = surface_detail;
    detail.base = surface_detail;
    manifest
}

fn context(frame: i64, tick: u32) -> PresetContext {
    let time = f64::from(tick) / 60.0;
    PresetContext {
        time,
        beat: time * 2.0,
        dt: 1.0 / 60.0,
        width: WIDTH,
        height: HEIGHT,
        output_width: WIDTH,
        output_height: HEIGHT,
        aspect: WIDTH as f32 / HEIGHT as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: frame,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

/// One frame; with a sampler, returns (whole-frame GPU ms, per-atom GPU ms).
fn render(
    runtime: &mut PresetRuntime,
    device: &GpuDevice,
    target: &RenderTarget,
    ctx: &PresetContext,
    params: &ParamManifest,
    sampler: Option<&GpuTimestampSampler>,
) -> Option<(f64, BTreeMap<String, f64>)> {
    let mut encoder = device.create_encoder("fluid surface perf");
    if let Some(sampler) = sampler {
        encoder.enable_dispatch_profiling(sampler.clone(), device);
    }
    runtime.set_profiling(sampler.is_some());
    let status = {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
        runtime.render(&mut gpu, &target.texture, ctx, params);
        gpu.frame_status()
    };
    assert_eq!(status, FrameRenderStatus::Complete, "frame {} did not complete", ctx.frame_count);
    let profile = encoder.commit_and_wait_profiled(device);
    assert_eq!(profile.failed_command_buffers, 0, "frame {} failed on the GPU", ctx.frame_count);
    sampler?;
    assert_eq!(profile.overflow, 0, "every dispatch must be timed");
    let steps: BTreeMap<String, String> = runtime
        .take_step_profiles()
        .into_iter()
        .map(|step| (step.tag, step.type_id))
        .collect();
    let mut per_atom = BTreeMap::new();
    for span in &profile.spans {
        let Some(type_id) = steps.get(&span.tag) else {
            continue;
        };
        *per_atom.entry(type_id.clone()).or_insert(0.0) += span.millis;
    }
    Some((profile.total_ms, per_atom))
}

/// The Liquid Surface mesh's vertex capacity, as the preset sets it.
fn mesh_capacity(json: &Value) -> f64 {
    let mut found = None;
    visit_nodes(json, &mut |node| {
        // The group also names the node in its port map, without params.
        if node["nodeId"] == "liquid_mesh"
            && let Some(capacity) = node["params"]["max_capacity"]["value"].as_f64()
        {
            found = Some(capacity);
        }
    });
    found.expect("liquid_mesh max_capacity")
}

fn visit_nodes(value: &Value, visit: &mut dyn FnMut(&Value)) {
    match value {
        Value::Object(map) => {
            if map.contains_key("nodeId") {
                visit(value);
            }
            map.values().for_each(|child| visit_nodes(child, visit));
        }
        Value::Array(items) => items.iter().for_each(|child| visit_nodes(child, visit)),
        _ => {}
    }
}

/// Triangles the surface needs this frame: the running total's `total`,
/// read through the preview on one unmeasured frame (it lags one frame).
fn surface_triangles(
    runtime: &mut PresetRuntime,
    device: &GpuDevice,
    target: &RenderTarget,
    ctx: &PresetContext,
    params: &ParamManifest,
) -> f64 {
    runtime.set_preview_node(Some(&manifold_core::NodeId::from("liquid_offsets")));
    render(runtime, device, target, ctx, params, None);
    let (_, outputs) = runtime.preview_scalar_io();
    let total = outputs
        .iter()
        .find_map(|(port, value)| (port == "total").then_some(f64::from(*value)))
        .unwrap_or(0.0);
    runtime.set_preview_node(None);
    total
}

fn percentile(samples: &[f64], fraction: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() - 1) as f64 * fraction).round() as usize]
}

fn shell(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_default()
}

#[test]
fn fluid_surface_perf() {
    let harness = harness::shared();
    let device = &harness.device;
    let sampler = device.create_timestamp_sampler(4096).expect("GPU timestamp sampler");
    let _offline = PhysicsStepScope::for_render(true);
    let target = RenderTarget::new(device, WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, "fluid-surface-perf");
    println!(
        "fluid_surface_perf: {} / {} / macOS {} / load {}",
        device.device_name(),
        shell("sysctl", &["-n", "machdep.cpu.brand_string"]),
        shell("sw_vers", &["-productVersion"]),
        shell("uptime", &[]),
    );
    let mut gated = None;
    for run in &RUNS {
        let resolution = run.resolution;
        let look: String =
            run.group.iter().chain(run.blobs).map(|(name, value)| format!(" {name} {value}")).collect();
        let json = preset(run);
        let mut runtime = PresetRuntime::from_json_str_with_device(
            &json.to_string(),
            &PrimitiveRegistry::with_cpu_flip_reference(),
            std::sync::Arc::clone(device),
            WIDTH,
            HEIGHT,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("GPU dam break builds");
        let mut frame = 0i64;
        let start = manifest(&json, 0.0);
        // Asset IO (the environment map) settles before the clock moves.
        let warmup_started = std::time::Instant::now();
        loop {
            frame += 1;
            render(&mut runtime, device, &target, &context(frame, 0), &start, None);
            if !runtime.warmup_pending() {
                break;
            }
            assert!(warmup_started.elapsed().as_secs() < 30, "asset warmup did not settle");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // The running total's `total` (triangles, one frame late) proves the
        // surface exists on every simulated frame.
        runtime.set_preview_node(Some(&manifold_core::NodeId::from("liquid_offsets")));
        for tick in 0..=TICKS {
            frame += 1;
            render(&mut runtime, device, &target, &context(frame, tick), &start, None);
            let (_, outputs) = runtime.preview_scalar_io();
            let triangles = outputs
                .iter()
                .find_map(|(port, value)| (port == "total").then_some(*value))
                .unwrap_or(0.0);
            assert!(
                tick < 3 || triangles > 0.0,
                "res {resolution}: tick {tick} produced no surface triangles"
            );
        }
        runtime.set_preview_node(None);
        for &detail in run.details {
            let params = manifest(&json, detail as f32);
            let mut surface = Vec::with_capacity(MEASURED_FRAMES);
            let mut whole = Vec::with_capacity(MEASURED_FRAMES);
            let mut per_atom: BTreeMap<String, Vec<f64>> = BTreeMap::new();
            for index in 0..WARMUP_FRAMES + MEASURED_FRAMES {
                frame += 1;
                let measured = index >= WARMUP_FRAMES;
                let result = render(
                    &mut runtime,
                    device,
                    &target,
                    &context(frame, TICKS),
                    &params,
                    measured.then_some(&sampler),
                );
                let Some((total, atoms)) = result else { continue };
                whole.push(total);
                surface.push(SURFACE_ATOMS.iter().map(|atom| atoms.get(*atom).copied().unwrap_or(0.0)).sum());
                for atom in SURFACE_ATOMS {
                    per_atom.entry(atom.to_owned()).or_default().push(atoms.get(atom).copied().unwrap_or(0.0));
                }
            }
            let scale = detail + 2;
            let p95 = percentile(&surface, 0.95);
            frame += 1;
            let vertices = 3.0
                * surface_triangles(&mut runtime, device, &target, &context(frame, TICKS), &params);
            let capacity = mesh_capacity(&json);
            println!(
                "res {resolution:>2} scale {scale}{look}: surface p50 {:.3} ms p95 {p95:.3} ms | 1080p frame p50 {:.3} ms p95 {:.3} ms | {vertices:.0} of {capacity:.0} vertices{}",
                percentile(&surface, 0.5),
                percentile(&whole, 0.5),
                percentile(&whole, 0.95),
                if vertices > capacity { " (OVERFLOW: mesh empty, emit time not representative)" } else { "" },
            );
            for (atom, samples) in &per_atom {
                println!(
                    "    {atom:<34} p50 {:.3} ms  p95 {:.3} ms",
                    percentile(samples, 0.5),
                    percentile(samples, 0.95)
                );
            }
            assert!(
                surface.iter().all(|ms| *ms > 0.0),
                "res {resolution} scale {scale}: every measured frame ran the surface atoms"
            );
            if resolution == 64 && scale == 2 && run.group.is_empty() && run.blobs.is_empty() {
                assert!(
                    vertices <= capacity,
                    "res 64 scale 2 needs {vertices:.0} vertices; the preset's Mesh Capacity is {capacity:.0}, so the gate would time an empty mesh"
                );
                gated = Some(p95);
            }
        }
    }
    let p95 = gated.expect("res 64 scale 2 measured");
    assert!(
        p95 <= BUDGET_MS,
        "Liquid Surface p95 {p95:.3} ms at res 64 scale 2 exceeds the {BUDGET_MS} ms budget on {}",
        device.device_name()
    );
}

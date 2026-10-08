//! Capture a CPU FLIP preset for bounded offline/preview timing.
//!
//! The frame CSVs keep render CPU, command-buffer wait, whole-buffer GPU, and
//! native solver metrics separate. Offline is a complete-step replay using the
//! native fixed 60 Hz simulation; output frames may use a lower capture FPS.
//! preview records the latest completed native snapshot, so repeated scalar
//! values are expected when the worker is behind. Each stepped offline frame
//! contributes one raw RGBA frame to `offline.rgba`; readback and file writes
//! happen after that frame's timer and are reported as `capture_ms`.
//!
//! Build with:
//!
//! `cargo build --profile test --features gpu-proofs --example fluid_capture`
//!
//! Run with one output directory and optional `--preset`, `--width`, `--height`,
//! `--frames`, `--fps`, `--offline-only`, `--max-seconds`, `--stills-every`,
//! `--linear`, `--cinematic`, `--supersample` and `--gpu-surface` flags. `--gpu-surface`
//! is for presets meshed by the Liquid Surface group (e.g. WaterDamBreakGpu): each
//! offline frame reads back the GPU mesh, fails on a non-finite vertex or an empty
//! surface, and reports its live vertices as `vertex_count`. A preset with its own
//! `node.tone_map` is always read back as `--linear` (sRGB of the graph output, what
//! the app shows); otherwise defaults preserve the shipped Water Basin workflow.
//!
//! A preset may run any liquid on the solver seam: the FLIP Fluids engine on the
//! CPU (`node.fluid_surface`) or a GPU solver (GPU FLIP, MLS-MPM) that publishes
//! through a frame node. The capture finds the liquid by the seam, not by solver:
//! the one liquid domain and the one node whose outputs carry the particle frame.
//! A GPU solver has no CPU mesh and reports its time in `gpu_ms`. `--look-metrics` records the particle look
//! metrics of GPU_MPM_SOLVER_DESIGN.md section 7 (look gates) from each offline
//! frame's published particles, through the same `matter::look` code the gate
//! tests use, into `look_metrics.csv` and `look_summary.txt`.

use std::error::Error;
use manifold_nodes as _;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use manifold_core::NodeId;
use manifold_core::liquid_domain::{FLIP_DOMAIN_TYPE_ID, is_liquid_domain};
use manifold_core::params::ParamManifest;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::gpu::headless_readback::{encode_rgba8_png, readback_srgb_rgba8, readback_tonemapped_rgba8};
use manifold_node_engine::water::fluid::domain_layout;
use manifold_node_engine::water::fluid_particles::FluidParticle;
use {manifold_node_engine::water::matter, manifold_node_engine::water::matter::look::Cells, manifold_node_engine::water::matter::look::LookRecorder};
use manifold_node_engine::{exec::effect_node::EffectNode, parameters::ParamValue, persistence::PrimitiveRegistry, water::physics::PhysicsStepScope};
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::gpu::render_target::RenderTarget;
use serde::Serialize;

const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;
const DEFAULT_FRAMES: u32 = 480;
const DEFAULT_FPS: u32 = 60;
const DEFAULT_MAX_SECONDS: f64 = 180.0;
const FIXED_HZ: f64 = 60.0;
const PREVIEW_SECONDS: f64 = 8.0;
const PREVIEW_MAX_SECONDS: f64 = 10.0;
const MAX_WIDTH: u32 = 3840;
const MAX_HEIGHT: u32 = 2160;
const MAX_FRAMES: u32 = 900;
const MAX_FPS: u32 = 60;
const MAX_SECONDS: f64 = 3600.0;
const CSV_HEADER: &str = "frame,authored_time,simulation_time,lag_seconds,render_cpu_ms,submit_wait_ms,gpu_ms,frame_ms,simulation_ms,meshing_ms,particle_count,vertex_count,capture_ms,presentation_interval_ms,foam_count,bubble_count,spray_count,upload_ms";
const METRIC_NAMES: [&str; 10] = [
    "simulation_time",
    "lag_seconds",
    "simulation_ms",
    "meshing_ms",
    "particle_count",
    "vertex_count",
    "foam_count",
    "bubble_count",
    "spray_count",
    "upload_ms",
];

type CaptureResult<T> = Result<T, Box<dyn Error>>;

#[derive(Clone, Debug)]
struct CaptureOptions {
    output_dir: PathBuf,
    preset_path: PathBuf,
    width: u32,
    height: u32,
    frames: u32,
    fps: u32,
    offline_only: bool,
    max_seconds: f64,
    stills_every: Option<u32>,
    linear: bool,
    cinematic: bool,
    supersample: u32,
    /// The preset meshes on the GPU (the Liquid Surface group): the fluid node
    /// publishes particle frames and its CPU mesh is off by design.
    gpu_surface: bool,
    /// On still frames, write each surface mesh's live vertices (position then
    /// normal, six little-endian f32 each) to `mesh/` for offline roughness
    /// measurement.
    dump_mesh: bool,
    /// Record the particle look metrics (GPU_MPM_SOLVER_DESIGN.md section 7
    /// (look gates) A1, A2, A5, A6) from each offline frame's particles.
    look_metrics: bool,
    /// Set from the preset in `run`.
    solver: Solver,
    /// Set from the preset in `run`.
    frame_node: String,
}

/// How the preset's liquid publishes its particle frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Solver {
    /// The FLIP Fluids engine on the CPU (`node.fluid_surface`): the domain
    /// publishes the frame itself, with the engine's timings, counts and mesh.
    Flip,
    /// A GPU solver (GPU FLIP, MLS-MPM, any later one): the domain publishes
    /// through a frame node, and its time is in the frame's `gpu_ms`.
    Gpu,
    /// No liquid (an ocean, a scene): frames are rendered and timed, and the
    /// liquid columns stay 0.
    None,
}

/// The particle-frame contract every liquid publishes
/// (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.1 (Particle frame)). A node
/// whose outputs carry all of these is a frame publisher, whatever its type.
const FRAME_PORTS: [&str; 5] = ["particles_a", "particles_b", "count_a", "count_b", "grid_bounds"];

#[derive(Clone, Debug)]
struct PresetSettings {
    solver: Solver,
    /// The nodeId of the node that publishes the particle frame: the scalar
    /// outputs the capture reads, and the `particles_b` it dumps.
    frame_node: String,
    resolution: u32,
    domain_size: f64,
    surface_detail: u32,
    viscosity: f64,
    surface_tension: f64,
    points_per_cell: u32,
    /// The domain's Simulation Speed: each fixed transport tick advances the
    /// liquid by speed × 1/60 s, so offline water time is speed × tick time.
    speed: f64,
}

#[derive(Clone, Copy, Debug)]
struct FluidMetrics {
    simulation_time: f64,
    lag_seconds: f64,
    simulation_ms: f64,
    meshing_ms: f64,
    particle_count: f64,
    vertex_count: f64,
    foam_count: f64,
    bubble_count: f64,
    spray_count: f64,
    upload_ms: f64,
}

#[derive(Clone, Copy, Debug)]
struct FrameTimings {
    render_cpu_ms: f64,
    submit_wait_ms: f64,
    gpu_ms: f64,
    frame_ms: f64,
}

impl FrameTimings {
    fn add_assign(&mut self, other: Self) {
        self.render_cpu_ms += other.render_cpu_ms;
        self.submit_wait_ms += other.submit_wait_ms;
        self.gpu_ms += other.gpu_ms;
        self.frame_ms += other.frame_ms;
    }
}

#[derive(Clone, Copy, Debug)]
struct MetricRow {
    frame: u32,
    authored_time: f64,
    fluid: FluidMetrics,
    timings: FrameTimings,
    capture_ms: f64,
    presentation_interval_ms: f64,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct InitialFrameTimings {
    render_cpu_ms: f64,
    submit_wait_ms: f64,
    gpu_ms: f64,
    frame_ms: f64,
}

#[derive(Clone, Debug, Serialize)]
struct PassMetadata {
    frames: u32,
    fps: u32,
    initial_frame: InitialFrameTimings,
    start_simulation_time: f64,
    end_simulation_time: f64,
    capture_ms: f64,
    output_frame_generation_ms: f64,
    sample_count: u32,
    shutter_interval_s: f64,
    spatial_sampling: u32,
}

#[derive(Clone, Debug, Serialize)]
struct PreviewMetadata {
    elapsed_s: f64,
    actual_frames: u32,
    fps: u32,
    initial_frame: InitialFrameTimings,
    accepted_fluid_frames: u32,
    total_sim_ticks_advanced: u64,
    start_simulation_time: f64,
    end_simulation_time: f64,
    final_authored_time: f64,
    final_lag_seconds: f64,
}

#[derive(Clone, Debug, Serialize)]
struct Metadata {
    preset_path: String,
    gpu_device_name: String,
    width: u32,
    height: u32,
    render_width: u32,
    render_height: u32,
    grid: [u32; 3],
    cell_size: f64,
    surface_detail: u32,
    viscosity: f64,
    surface_tension: f64,
    min_substeps: u32,
    max_substeps: u32,
    cfl: u32,
    adaptive_obstacles: bool,
    native_fixed_hz: f64,
    capture_fps: u32,
    display_transform: &'static str,
    cinematic: bool,
    sample_count: u32,
    shutter_interval_s: f64,
    spatial_sampling: u32,
    offline_only: bool,
    stills_every: Option<u32>,
    offline: PassMetadata,
    preview: Option<PreviewMetadata>,
}

fn default_preset_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("cpu-flip")
        .join("WaterBasin.json")
}

fn read_preset(path: &Path) -> CaptureResult<String> {
    fs::read_to_string(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("read preset {}: {error}", path.display()),
        )
        .into()
    })
}

fn preset_number(node: &serde_json::Value, name: &str) -> CaptureResult<f64> {
    node["params"][name]["value"].as_f64().ok_or_else(|| {
        io::Error::other(format!(
            "{} parameter `{name}` is missing or non-numeric",
            node["nodeId"]
        ))
        .into()
    })
}

/// A numeric param of `node`: the preset's value, else the type's default.
fn param_or_default(node: &serde_json::Value, built: &dyn EffectNode, name: &str) -> CaptureResult<f64> {
    if let Some(value) = node["params"][name]["value"].as_f64() {
        return Ok(value);
    }
    match built.parameters().iter().find(|param| param.name == name).map(|param| &param.default) {
        Some(ParamValue::Float(value)) => Ok(f64::from(*value)),
        Some(ParamValue::Enum(value)) => Ok(f64::from(*value)),
        _ => Err(io::Error::other(format!("{} has no numeric parameter `{name}`", node["typeId"])).into()),
    }
}

/// Every node, at any group depth, that `matches`.
fn find_preset_nodes<'a>(
    nodes: &'a serde_json::Value,
    matches: &dyn Fn(&serde_json::Value) -> bool,
    out: &mut Vec<&'a serde_json::Value>,
) {
    for node in nodes.as_array().into_iter().flatten() {
        if matches(node) {
            out.push(node);
        }
        find_preset_nodes(&node["group"]["nodes"], matches, out);
    }
}

/// The first node, at any group depth, that `matches`.
fn find_preset_node<'a>(
    nodes: &'a serde_json::Value,
    matches: &dyn Fn(&serde_json::Value) -> bool,
) -> Option<&'a serde_json::Value> {
    nodes.as_array()?.iter().find_map(|node| {
        if matches(node) {
            Some(node)
        } else {
            find_preset_node(&node["group"]["nodes"], matches)
        }
    })
}

/// The preset's liquid, found through the solver seam: its one liquid domain
/// (`is_liquid_domain`) and the one node whose outputs carry the particle
/// frame. A domain that publishes the frame itself is the CPU engine; any
/// other publishes through a frame node and is a GPU solver.
fn preset_settings(json: &str) -> CaptureResult<PresetSettings> {
    let document: serde_json::Value = serde_json::from_str(json)?;
    let registry = PrimitiveRegistry::with_cpu_flip_reference();
    let built = |node: &serde_json::Value| node["typeId"].as_str().and_then(|type_id| registry.construct(type_id));
    let mut domains = Vec::new();
    find_preset_nodes(&document["nodes"], &|node| node["typeId"].as_str().is_some_and(is_liquid_domain), &mut domains);
    if domains.is_empty() {
        return Ok(PresetSettings {
            solver: Solver::None,
            frame_node: String::new(),
            resolution: 0,
            domain_size: 0.0,
            surface_detail: 0,
            viscosity: 0.0,
            surface_tension: 0.0,
            points_per_cell: 0,
            speed: 1.0,
        });
    }
    let [domain] = domains[..] else {
        return Err(io::Error::other(format!("preset needs one liquid domain, found {}", domains.len())).into());
    };
    let domain_type = built(domain).ok_or_else(|| io::Error::other(format!("{} is not a registered type", domain["typeId"])))?;
    let speed = param_or_default(domain, domain_type.as_ref(), "speed")?;
    if !speed.is_finite() || speed <= 0.0 {
        return Err(io::Error::other("preset liquid Simulation Speed must be above 0 to capture").into());
    }
    let mut frames = Vec::new();
    find_preset_nodes(
        &document["nodes"],
        &|node| {
            built(node).is_some_and(|built| {
                FRAME_PORTS.iter().all(|port| built.outputs().iter().any(|output| output.name == *port))
            })
        },
        &mut frames,
    );
    let [frame] = frames[..] else {
        let found: Vec<String> = frames.iter().map(|node| node["nodeId"].to_string()).collect();
        return Err(io::Error::other(format!("preset needs one particle-frame publisher, found [{}]", found.join(", "))).into());
    };
    let frame_node = frame["nodeId"]
        .as_str()
        .ok_or_else(|| io::Error::other("the particle-frame publisher has no nodeId"))?
        .to_string();
    if frame["id"] != domain["id"] || frame["nodeId"] != domain["nodeId"] {
        // Shared names across domains (GPU_MPM_SOLVER_DESIGN.md D17).
        let resolution = param_or_default(domain, domain_type.as_ref(), "resolution")?;
        let domain_size = param_or_default(domain, domain_type.as_ref(), "domain_size")?;
        if !(resolution.is_finite() && resolution >= 1.0 && domain_size.is_finite() && domain_size > 0.0) {
            return Err(io::Error::other("preset liquid domain settings are invalid").into());
        }
        // The look metrics' sample spacing: MPM's Points Per Cell enum (0 is
        // 2 per axis, 1 is 3); a solver without it seeds 8 a cell.
        let points_per_cell = match domain["params"]["points_per_cell"]["value"].as_u64() {
            Some(1) => 27,
            _ => 8,
        };
        return Ok(PresetSettings {
            solver: Solver::Gpu,
            frame_node,
            resolution: resolution.round() as u32,
            domain_size,
            surface_detail: 0,
            viscosity: 0.0,
            surface_tension: 0.0,
            points_per_cell,
            speed,
        });
    }
    let fluid = domain;
    let resolution = preset_number(fluid, "resolution")?;
    let domain_size = preset_number(fluid, "domain_size")?;
    let surface_detail = preset_number(fluid, "surface_subdivisions")?;
    let optional_coefficient = |name: &str| -> CaptureResult<f64> {
        if fluid["params"].get(name).is_none() {
            return Ok(0.0);
        }
        let value = preset_number(fluid, name)?;
        if !value.is_finite() || value < 0.0 {
            return Err(io::Error::other(format!("invalid liquid coefficient {name}")).into());
        }
        Ok(value)
    };
    if !resolution.is_finite()
        || !domain_size.is_finite()
        || !surface_detail.is_finite()
        || resolution < 1.0
        || domain_size <= 0.0
        || surface_detail < 0.0
    {
        return Err(io::Error::other("preset fluid settings are invalid").into());
    }
    Ok(PresetSettings {
        solver: Solver::Flip,
        frame_node,
        resolution: resolution.round() as u32,
        domain_size,
        surface_detail: surface_detail.round() as u32,
        viscosity: optional_coefficient("viscosity")?,
        surface_tension: optional_coefficient("surface_tension")?,
        points_per_cell: 8,
        speed,
    })
}

fn instrument_preset(
    json: &str,
    cinematic: bool,
    supersample: u32,
    solver: Solver,
    frame_node: &str,
) -> CaptureResult<String> {
    let mut instrumented: serde_json::Value = serde_json::from_str(json)?;
    let mut nodes = instrumented["nodes"]
        .take()
        .as_array()
        .cloned()
        .ok_or_else(|| io::Error::other("preset nodes must be an array"))?;
    let mut next_id = nodes
        .iter()
        .filter_map(|node| node["id"].as_u64())
        .max()
        .unwrap_or(0)
        + 1;
    let mut wires = instrumented["wires"]
        .take()
        .as_array()
        .cloned()
        .ok_or_else(|| io::Error::other("preset wires must be an array"))?;
    // The engine's metric outputs get consumers so the CSV columns are
    // computed; a GPU frame's count is already consumed by the surface.
    if solver == Solver::Flip {
        let fluid_id = nodes
            .iter()
            .find(|node| node["nodeId"] == frame_node)
            .and_then(|node| node["id"].as_u64())
            .ok_or_else(|| io::Error::other(format!("preset node {frame_node} is not at the top level")))?;
        for (index, name) in METRIC_NAMES.iter().enumerate() {
            nodes.push(serde_json::json!({
                "id": next_id + index as u64,
                "nodeId": format!("capture_metric_{name}"),
                "typeId": "node.math",
                "handle": format!("Capture {name}"),
            }));
            wires.push(serde_json::json!({
                "fromNode": fluid_id, "fromPort": name,
                "toNode": next_id + index as u64, "toPort": "a",
            }));
        }
        next_id += METRIC_NAMES.len() as u64;
    }

    if cinematic {
        let scene_id = nodes
            .iter()
            .find(|node| node["typeId"] == "node.render_scene")
            .and_then(|node| node["id"].as_u64())
            .ok_or_else(|| io::Error::other("cinematic preset has no render_scene node"))?;
        let tone_id = nodes
            .iter()
            .find(|node| node["typeId"] == "node.tone_map")
            .and_then(|node| node["id"].as_u64())
            .ok_or_else(|| io::Error::other("cinematic preset has no tone_map node"))?;
        let scene_to_tone = wires
            .iter()
            .position(|wire| {
                wire["fromNode"] == scene_id
                    && wire["fromPort"] == "color"
                    && wire["toNode"] == tone_id
                    && wire["toPort"] == "in"
            })
            .ok_or_else(|| {
                io::Error::other("tone_map is not wired directly from render_scene color")
            })?;
        wires.remove(scene_to_tone);

        let feedback_id = next_id;
        let mix_id = next_id + 1;
        next_id += 2;
        nodes.push(serde_json::json!({
            "id": feedback_id,
            "nodeId": "cinematic_feedback",
            "typeId": "node.feedback",
            "handle": "Cinematic Temporal Sample",
            "params": {"copy_capture": {"type": "Bool", "value": true}},
        }));
        nodes.push(serde_json::json!({
            "id": mix_id,
            "nodeId": "cinematic_temporal_mix",
            "typeId": "node.mix",
            "handle": "Cinematic Temporal Average",
            "params": {
                "amount": {"type": "Float", "value": 0.5},
                "mode": {"type": "Enum", "value": 0},
            },
        }));
        wires.push(serde_json::json!({"fromNode": scene_id, "fromPort": "color", "toNode": feedback_id, "toPort": "in"}));
        wires.push(serde_json::json!({"fromNode": scene_id, "fromPort": "color", "toNode": mix_id, "toPort": "a"}));
        wires.push(serde_json::json!({"fromNode": feedback_id, "fromPort": "out", "toNode": mix_id, "toPort": "b"}));
        let mut tone_input = mix_id;
        if supersample == 2 {
            let downsample_id = next_id;
            nodes.push(serde_json::json!({
                "id": downsample_id,
                "nodeId": "cinematic_downsample",
                "typeId": "node.downsample",
                "handle": "Cinematic Spatial Downsample",
                "params": {"factor": {"type": "Enum", "value": 0}},
            }));
            wires.push(serde_json::json!({"fromNode": mix_id, "fromPort": "out", "toNode": downsample_id, "toPort": "in"}));
            tone_input = downsample_id;
        }
        wires.push(serde_json::json!({"fromNode": tone_input, "fromPort": "out", "toNode": tone_id, "toPort": "in"}));
    }
    instrumented["nodes"] = serde_json::Value::Array(nodes);
    instrumented["wires"] = serde_json::Value::Array(wires);
    serde_json::to_string(&instrumented).map_err(Into::into)
}

fn build_runtime(
    instrumented_json: &str,
    device: &Arc<GpuDevice>,
    width: u32,
    height: u32,
    frame_node: &str,
) -> CaptureResult<PresetRuntime> {
    let registry = PrimitiveRegistry::with_cpu_flip_reference();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        instrumented_json,
        &registry,
        Arc::clone(device),
        width,
        height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .map_err(|error| io::Error::other(format!("build preset runtime: {error}")))?;
    if !frame_node.is_empty() {
        runtime.set_preview_node(Some(&NodeId::from(frame_node)));
    }
    Ok(runtime)
}

// RenderScene uses the graph canvas size; per-port scale overrides do not resize it.
fn render_dimensions(width: u32, height: u32, supersample: u32) -> (u32, u32) {
    (width * supersample, height * supersample)
}

fn verify_cinematic_dimensions(
    runtime: &PresetRuntime,
    options: &CaptureOptions,
) -> CaptureResult<()> {
    let render = render_dimensions(options.width, options.height, options.supersample);
    let mut expected = vec![
        ("node.render_scene", "color", render),
        ("node.feedback", "out", render),
        ("node.mix", "out", render),
        ("node.tone_map", "out", (options.width, options.height)),
    ];
    if options.supersample == 2 {
        expected.push(("node.downsample", "out", (options.width, options.height)));
    }
    let textures = runtime.dump_textures_all();
    let mut verified = Vec::new();
    for (type_id, port, dimensions) in expected {
        let (name, _, _, texture) = textures
            .iter()
            .find(|(_, p, t, _)| p == port && t == type_id)
            .ok_or_else(|| {
                io::Error::other(format!("missing cinematic texture {type_id}.{port}"))
            })?;
        if (texture.width, texture.height) != dimensions {
            return Err(io::Error::other(format!(
                "{name}.{port} is {}x{}, expected {}x{}",
                texture.width, texture.height, dimensions.0, dimensions.1
            ))
            .into());
        }
        verified.push(serde_json::json!({
            "node": name, "port": port, "width": texture.width, "height": texture.height,
        }));
    }
    fs::write(
        options.output_dir.join("verified-dimensions.json"),
        serde_json::to_vec_pretty(&verified)?,
    )?;
    Ok(())
}

fn context(frame: u32, authored_time: f64, dt: f64, options: &CaptureOptions) -> PresetContext {
    let (width, height) = render_dimensions(options.width, options.height, options.supersample);
    PresetContext {
        time: authored_time,
        beat: authored_time * 2.0,
        dt: dt as f32,
        width,
        height,
        output_width: options.width,
        output_height: options.height,
        aspect: width as f32 / height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: i64::from(frame),
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn read_fluid_metrics(runtime: &PresetRuntime, solver: Solver) -> CaptureResult<FluidMetrics> {
    if solver == Solver::None {
        return Ok(FluidMetrics {
            simulation_time: 0.0,
            lag_seconds: 0.0,
            simulation_ms: 0.0,
            meshing_ms: 0.0,
            particle_count: 0.0,
            vertex_count: 0.0,
            foam_count: 0.0,
            bubble_count: 0.0,
            spray_count: 0.0,
            upload_ms: 0.0,
        });
    }
    let (inputs, outputs) = runtime.preview_scalar_io();
    if solver == Solver::Gpu {
        // The GPU solver's time is in the frame's gpu_ms; the engine's
        // solve/mesh/upload columns and whitewater counts stay 0.
        let read = |list: &[(String, f32)], port: &str| -> CaptureResult<f64> {
            let value = list
                .iter()
                .find_map(|(name, value)| (name == port).then_some(*value))
                .ok_or_else(|| io::Error::other(format!("the frame's `{port}` is missing")))?;
            if !value.is_finite() {
                return Err(io::Error::other(format!("the frame's `{port}` is non-finite: {value}")).into());
            }
            Ok(f64::from(value))
        };
        return Ok(FluidMetrics {
            simulation_time: read(&inputs, "simulation_time")?,
            lag_seconds: 0.0,
            simulation_ms: 0.0,
            meshing_ms: 0.0,
            // The count of the frame the preset presents: B, or A for a
            // coupled preset (frame A carries the display-time bodies).
            particle_count: read(&outputs, "count_b").or_else(|_| read(&outputs, "count_a"))?,
            vertex_count: 0.0,
            foam_count: 0.0,
            bubble_count: 0.0,
            spray_count: 0.0,
            upload_ms: 0.0,
        });
    }
    let mut values = [0.0; METRIC_NAMES.len()];
    for (index, name) in METRIC_NAMES.iter().enumerate() {
        let value = outputs
            .iter()
            .find_map(|(port, value)| (port == name).then_some(*value))
            .ok_or_else(|| io::Error::other(format!("fluid_surface output `{name}` is missing")))?;
        if !value.is_finite() {
            return Err(io::Error::other(format!(
                "fluid_surface output `{name}` is non-finite: {value}"
            ))
            .into());
        }
        values[index] = f64::from(value);
    }
    Ok(FluidMetrics {
        simulation_time: values[0],
        lag_seconds: values[1],
        simulation_ms: values[2],
        meshing_ms: values[3],
        particle_count: values[4],
        vertex_count: values[5],
        foam_count: values[6],
        bubble_count: values[7],
        spray_count: values[8],
        upload_ms: values[9],
    })
}

fn render_frame(
    runtime: &mut PresetRuntime,
    target: &RenderTarget,
    device: &GpuDevice,
    frame: u32,
    authored_time: f64,
    dt: f64,
    options: &CaptureOptions,
    warming: bool,
) -> CaptureResult<(FrameTimings, FluidMetrics)> {
    let frame_started = Instant::now();
    let mut encoder = device.create_encoder("fluid-capture");
    let render_started = Instant::now();
    let status = {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
        runtime.render(
            &mut gpu,
            &target.texture,
            &context(frame, authored_time, dt, options),
            &ParamManifest::default(),
        );
        gpu.frame_status()
    };
    let render_cpu_ms = render_started.elapsed().as_secs_f64() * 1000.0;
    // Warm-up frames may still be preparing assets (FrameRenderStatus's
    // contract); a captured frame must be complete.
    let acceptable = status == FrameRenderStatus::Complete
        || (warming && status == FrameRenderStatus::PendingGeometry);
    if !acceptable {
        return Err(io::Error::other(format!(
            "Water Basin frame {frame} failed with status {status:?}"
        ))
        .into());
    }
    let submit_started = Instant::now();
    let profile = encoder.commit_and_wait_profiled(device);
    let submit_wait_ms = submit_started.elapsed().as_secs_f64() * 1000.0;
    if profile.failed_command_buffers != 0 {
        return Err(io::Error::other(format!(
            "Water Basin frame {frame} had {} failed command buffers",
            profile.failed_command_buffers
        ))
        .into());
    }
    let timings = FrameTimings {
        render_cpu_ms,
        submit_wait_ms,
        gpu_ms: profile.total_ms,
        frame_ms: frame_started.elapsed().as_secs_f64() * 1000.0,
    };
    for (label, value) in [
        ("render_cpu_ms", timings.render_cpu_ms),
        ("submit_wait_ms", timings.submit_wait_ms),
        ("gpu_ms", timings.gpu_ms),
        ("frame_ms", timings.frame_ms),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(io::Error::other(format!(
                "Water Basin frame {frame} has invalid {label}: {value}"
            ))
            .into());
        }
    }
    // Keep scalar-output copying outside the frame timer. The executor's
    // internal preview capture remains part of runtime.render above.
    let fluid = read_fluid_metrics(runtime, options.solver)?;
    Ok((timings, fluid))
}

/// The newest particle frame the solver published on the last dumped frame:
/// its first `count` records.
fn published_particles(
    runtime: &PresetRuntime,
    device: &GpuDevice,
    frame_node: &str,
    count: f64,
) -> CaptureResult<Vec<FluidParticle>> {
    let arrays = runtime.dump_arrays_all();
    let frame = arrays
        .iter()
        .find(|array| array.name == frame_node && array.port == "particles_b")
        .ok_or_else(|| io::Error::other(format!("--look-metrics: no {frame_node}.particles_b this frame")))?;
    let record = std::mem::size_of::<FluidParticle>() as u64;
    let count = (count.max(0.0) as u64).min(frame.buffer.size() / record);
    let bytes = (count * record).max(record);
    let staging = device.create_buffer_shared(bytes);
    let mut encoder = device.create_encoder("look-metrics-readback");
    encoder.copy_buffer_to_buffer(frame.buffer, &staging, bytes.min(frame.buffer.size()));
    encoder.commit_and_wait_completed();
    let ptr = staging.mapped_ptr().expect("shared staging buffer");
    // SAFETY: the copy has completed and nothing else writes the staging buffer.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, (count * record) as usize) };
    Ok(bytemuck::cast_slice(bytes).to_vec())
}

/// Write the per-frame look samples and the A1, A2, A5, A6 summary.
fn write_look_metrics(dir: &Path, look: &LookRecorder) -> CaptureResult<()> {
    let mut csv = BufWriter::new(File::create(dir.join("look_metrics.csv"))?);
    writeln!(csv, "time,mean_speed,surface_height,detached_fraction,sheet_fraction")?;
    for &(time, speed, height) in &look.samples {
        let at = |series: &[(f32, f32)]| {
            series.iter().find(|s| (s.0 - time).abs() < 1e-4).map_or(String::new(), |s| s.1.to_string())
        };
        writeln!(csv, "{time},{speed},{height},{},{}", at(&look.splash), at(&look.sheets))?;
    }
    csv.flush()?;
    let settle = look.settling();
    let mut summary = BufWriter::new(File::create(dir.join("look_summary.txt"))?);
    writeln!(summary, "A1 lattice alignment (largest bin over mean, per axis; fails above 1.5):")?;
    for (time, ratio, interior) in &look.alignment {
        writeln!(summary, "  t {time:.2} s: {ratio:?} over {interior} interior points")?;
    }
    writeln!(
        summary,
        "A2 settle (mean speed below {} m/s): {:?} s; ringing over {} s windows: {:.4} m",
        matter::look::SETTLE_SPEED,
        settle.settle_time,
        matter::look::RINGING_WINDOW,
        settle.ringing
    )?;
    writeln!(summary, "A5 detached fraction, max over t in [0.5, 3] s: {:.4}", look.max_splash())?;
    writeln!(summary, "A6 sheet fraction, max over t in [0.5, 3] s: {:.4}", look.max_sheets())?;
    summary.flush()?;
    Ok(())
}

/// Live vertices of the Liquid Surface mesh on the last dumped frame. Slots past
/// the live triangles are zero, so a live vertex is one with a nonzero normal.
fn gpu_surface_vertices(runtime: &PresetRuntime, device: &GpuDevice, frame: u32) -> CaptureResult<u64> {
    let arrays = runtime.dump_arrays_all();
    let mesh = arrays
        .iter()
        .find(|array| array.type_id == "node.volume_surface_mesh" && array.port == "vertices")
        .ok_or_else(|| io::Error::other("--gpu-surface: the preset has no node.volume_surface_mesh"))?;
    let size = mesh.buffer.size();
    let staging = device.create_buffer_shared(size);
    let mut encoder = device.create_encoder("gpu-surface-readback");
    encoder.copy_buffer_to_buffer(mesh.buffer, &staging, size);
    encoder.commit_and_wait_completed();
    let ptr = staging.mapped_ptr().expect("shared staging buffer");
    // SAFETY: the copy has completed and nothing else writes the staging buffer.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, size as usize) };
    let mut live = 0u64;
    for (index, chunk) in bytes.chunks_exact(std::mem::size_of::<MeshVertex>()).enumerate() {
        let vertex: MeshVertex = bytemuck::pod_read_unaligned(chunk);
        let values = vertex.position.iter().chain(&vertex.normal).chain(&vertex.uv);
        if values.clone().any(|value| !value.is_finite()) {
            return Err(io::Error::other(format!(
                "offline frame {frame}: Liquid Surface vertex {index} is non-finite: {:?} {:?} {:?}",
                vertex.position, vertex.normal, vertex.uv
            ))
            .into());
        }
        if vertex.normal != [0.0; 3] {
            live += 1;
        }
    }
    Ok(live)
}

/// Writes the live vertices of every surface mesh in the graph: each GPU
/// Liquid Surface (`gpu`, live = nonzero normal) and the fluid node's CPU mesh
/// (`cpu`, live = its first `cpu_count` vertices). A GPU mesh node other than
/// the preset's `liquid_mesh` is tagged `gpu-<node id>`, so one run can mesh
/// the same particles through several surface variants.
fn dump_surface_meshes(
    runtime: &PresetRuntime,
    device: &GpuDevice,
    frame: u32,
    cpu_count: usize,
    fluid: &FluidMetrics,
    frame_node: &str,
    dir: &Path,
) -> CaptureResult<()> {
    for array in runtime.dump_arrays_all() {
        let tag = match (array.type_id.as_str(), array.port.as_str()) {
            ("node.volume_surface_mesh", "vertices") if array.name == "liquid_mesh" => "gpu".to_string(),
            ("node.volume_surface_mesh", "vertices") => {
                let name: String =
                    array.name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
                format!("gpu-{name}")
            }
            (FLIP_DOMAIN_TYPE_ID, "vertices") => "cpu".to_string(),
            (_, "particles_b") if array.name == frame_node => "particles".to_string(),
            _ => continue,
        };
        let size = array.buffer.size();
        let staging = device.create_buffer_shared(size);
        let mut encoder = device.create_encoder("surface-mesh-dump");
        encoder.copy_buffer_to_buffer(array.buffer, &staging, size);
        encoder.commit_and_wait_completed();
        let ptr = staging.mapped_ptr().expect("shared staging buffer");
        // SAFETY: the copy has completed and nothing else writes the staging buffer.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, size as usize) };
        if tag == "particles" {
            // Raw FluidParticle slots; the name carries the live count.
            let live = fluid.particle_count as usize;
            fs::write(dir.join(format!("frame_{frame:06}_particles_{live}.bin")), bytes)?;
            continue;
        }
        let mut out = Vec::new();
        for (index, chunk) in bytes.chunks_exact(std::mem::size_of::<MeshVertex>()).enumerate() {
            let vertex: MeshVertex = bytemuck::pod_read_unaligned(chunk);
            let live = if tag.starts_with("gpu") { vertex.normal != [0.0; 3] } else { index < cpu_count };
            if live {
                for value in vertex.position.iter().chain(&vertex.normal) {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        fs::write(dir.join(format!("frame_{frame:06}_{tag}.f32")), out)?;
    }
    Ok(())
}

fn render_output_frame(
    runtime: &mut PresetRuntime,
    target: &RenderTarget,
    device: &GpuDevice,
    frame: u32,
    authored_time: f64,
    dt: f64,
    options: &CaptureOptions,
) -> CaptureResult<(FrameTimings, FluidMetrics)> {
    let sample_count = if options.cinematic { 2 } else { 1 };
    let mut total_timings = FrameTimings {
        render_cpu_ms: 0.0,
        submit_wait_ms: 0.0,
        gpu_ms: 0.0,
        frame_ms: 0.0,
    };
    let mut final_fluid = None;
    let mut simulation_ms = 0.0;
    let mut meshing_ms = 0.0;
    let mut upload_ms = 0.0;
    let temporal_samples = temporal_samples(authored_time);
    for (sample, &temporal_sample) in temporal_samples.iter().enumerate().take(sample_count) {
        let (sample_time, sample_dt) = if options.cinematic {
            temporal_sample
        } else {
            (authored_time, dt)
        };
        let sample_frame = if options.cinematic {
            frame.saturating_mul(2).saturating_sub(1) + sample as u32
        } else {
            frame
        };
        let (timings, fluid) = render_frame(
            runtime,
            target,
            device,
            sample_frame,
            sample_time,
            sample_dt,
            options,
            false,
        )?;
        total_timings.add_assign(timings);
        simulation_ms += fluid.simulation_ms;
        meshing_ms += fluid.meshing_ms;
        upload_ms += fluid.upload_ms;
        final_fluid = Some(fluid);
    }
    let mut fluid = final_fluid.expect("output frame renders at least one sample");
    fluid.simulation_ms = simulation_ms;
    fluid.meshing_ms = meshing_ms;
    fluid.upload_ms = upload_ms;
    Ok((total_timings, fluid))
}

fn temporal_samples(authored_time: f64) -> [(f64, f64); 2] {
    let first_time = (authored_time - 1.0 / FIXED_HZ).max(0.0);
    let first_dt = if first_time == 0.0 {
        0.0
    } else {
        1.0 / FIXED_HZ
    };
    [(first_time, first_dt), (authored_time, 1.0 / FIXED_HZ)]
}

fn metric_row(
    frame: u32,
    authored_time: f64,
    timings: FrameTimings,
    fluid: FluidMetrics,
    presentation_interval_ms: f64,
) -> CaptureResult<MetricRow> {
    let row = MetricRow {
        frame,
        authored_time,
        fluid,
        timings,
        capture_ms: 0.0,
        presentation_interval_ms,
    };
    if !authored_time.is_finite() || !presentation_interval_ms.is_finite() {
        return Err(io::Error::other(format!(
            "frame {frame} has invalid authored or presentation time"
        ))
        .into());
    }
    Ok(row)
}

struct CsvWriter {
    writer: BufWriter<File>,
}

impl CsvWriter {
    fn create(path: &Path) -> CaptureResult<Self> {
        let mut writer = BufWriter::new(File::create(path)?);
        writeln!(writer, "{CSV_HEADER}")?;
        Ok(Self { writer })
    }

    fn write_row(&mut self, row: &MetricRow) -> CaptureResult<()> {
        writeln!(
            self.writer,
            "{},{:.9},{:.9},{:.9},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.3},{:.3},{:.6},{:.6},{:.0},{:.0},{:.0},{:.6}",
            row.frame,
            row.authored_time,
            row.fluid.simulation_time,
            row.fluid.lag_seconds,
            row.timings.render_cpu_ms,
            row.timings.submit_wait_ms,
            row.timings.gpu_ms,
            row.timings.frame_ms,
            row.fluid.simulation_ms,
            row.fluid.meshing_ms,
            row.fluid.particle_count,
            row.fluid.vertex_count,
            row.capture_ms,
            row.presentation_interval_ms,
            row.fluid.foam_count,
            row.fluid.bubble_count,
            row.fluid.spray_count,
            row.fluid.upload_ms,
        )?;
        self.writer.flush()?;
        Ok(())
    }

    fn flush(&mut self) -> CaptureResult<()> {
        self.writer.flush()?;
        Ok(())
    }
}

fn write_csv(path: &Path, rows: &[MetricRow]) -> CaptureResult<()> {
    let mut writer = CsvWriter::create(path)?;
    for row in rows {
        writer.write_row(row)?;
    }
    writer.flush()
}

fn ensure_wall_limit(started: Instant, label: &str, max_seconds: f64) -> CaptureResult<()> {
    let elapsed = started.elapsed().as_secs_f64();
    if elapsed > max_seconds {
        return Err(io::Error::other(format!(
            "{label} exceeded wall limit of {max_seconds:.0}s ({elapsed:.3}s)"
        ))
        .into());
    }
    Ok(())
}

fn parse_u32(value: &str, flag: &str) -> CaptureResult<u32> {
    value
        .parse()
        .map_err(|_| io::Error::other(format!("{flag} expects an unsigned integer")).into())
}

fn parse_options() -> CaptureResult<CaptureOptions> {
    let mut args = std::env::args().skip(1);
    let mut output_dir = None;
    let mut options = CaptureOptions {
        output_dir: PathBuf::new(),
        preset_path: default_preset_path(),
        width: DEFAULT_WIDTH,
        height: DEFAULT_HEIGHT,
        frames: DEFAULT_FRAMES,
        fps: DEFAULT_FPS,
        offline_only: false,
        max_seconds: DEFAULT_MAX_SECONDS,
        stills_every: None,
        linear: false,
        cinematic: false,
        supersample: 1,
        gpu_surface: false,
        dump_mesh: false,
        look_metrics: false,
        solver: Solver::Flip,
        frame_node: String::new(),
    };
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| -> CaptureResult<String> {
            args.next()
                .ok_or_else(|| io::Error::other(format!("{flag} requires a value")).into())
        };
        match arg.as_str() {
            "--preset" => options.preset_path = PathBuf::from(value("--preset")?),
            "--width" => options.width = parse_u32(&value("--width")?, "--width")?,
            "--height" => options.height = parse_u32(&value("--height")?, "--height")?,
            "--frames" => options.frames = parse_u32(&value("--frames")?, "--frames")?,
            "--fps" => options.fps = parse_u32(&value("--fps")?, "--fps")?,
            "--max-seconds" => {
                let raw = value("--max-seconds")?;
                options.max_seconds = raw
                    .parse()
                    .map_err(|_| io::Error::other("--max-seconds expects a number"))?;
            }
            "--stills-every" => {
                let every = parse_u32(&value("--stills-every")?, "--stills-every")?;
                options.stills_every = Some(every);
            }
            "--offline-only" => options.offline_only = true,
            "--linear" => options.linear = true,
            "--cinematic" => options.cinematic = true,
            "--gpu-surface" => options.gpu_surface = true,
            "--dump-mesh" => options.dump_mesh = true,
            "--look-metrics" => options.look_metrics = true,
            "--supersample" => {
                options.supersample = parse_u32(&value("--supersample")?, "--supersample")?
            }
            flag if flag.starts_with('-') => {
                return Err(io::Error::other(format!("unsupported argument `{flag}`")).into());
            }
            path if output_dir.is_none() => output_dir = Some(PathBuf::from(path)),
            path => {
                return Err(
                    io::Error::other(format!("unexpected positional argument `{path}`")).into(),
                );
            }
        }
    }
    options.output_dir = output_dir.ok_or_else(|| {
        io::Error::other(
            "usage: fluid_capture OUTPUT_DIR [--preset PATH] [--width N] [--height N] [--frames N] [--fps N] [--offline-only] [--max-seconds N] [--stills-every N] [--linear] [--cinematic] [--supersample 1|2] [--gpu-surface] [--look-metrics] [--dump-mesh]",
        )
    })?;
    if options.width == 0
        || options.width > MAX_WIDTH
        || options.height == 0
        || options.height > MAX_HEIGHT
    {
        return Err(io::Error::other(format!(
            "dimensions must be positive and at most {MAX_WIDTH}x{MAX_HEIGHT}"
        ))
        .into());
    }
    if options.frames == 0 || options.frames > MAX_FRAMES {
        return Err(io::Error::other(format!("frames must be in 1..={MAX_FRAMES}")).into());
    }
    if options.fps == 0 || options.fps > MAX_FPS {
        return Err(io::Error::other(format!("fps must be in 1..={MAX_FPS}")).into());
    }
    if options.supersample != 1 && options.supersample != 2 {
        return Err(io::Error::other("supersample must be 1 or 2").into());
    }
    if options.cinematic && (options.fps != 30 || !options.linear) {
        return Err(io::Error::other("--cinematic requires --fps 30 and --linear").into());
    }
    if options.supersample == 2 && !options.cinematic {
        return Err(io::Error::other("--supersample 2 requires --cinematic").into());
    }
    let (render_width, render_height) =
        render_dimensions(options.width, options.height, options.supersample);
    if render_width > MAX_WIDTH || render_height > MAX_HEIGHT {
        return Err(io::Error::other("supersampled render must fit within 3840x2160").into());
    }
    if !options.max_seconds.is_finite()
        || options.max_seconds <= 0.0
        || options.max_seconds > MAX_SECONDS
    {
        return Err(
            io::Error::other(format!("max-seconds must be in (0,{MAX_SECONDS:.0}]")).into(),
        );
    }
    if options.stills_every == Some(0) {
        return Err(io::Error::other("stills-every must be positive").into());
    }
    Ok(options)
}

fn readback_rgba(
    device: &GpuDevice,
    target: &RenderTarget,
    width: u32,
    height: u32,
    linear: bool,
) -> Vec<u8> {
    if linear {
        readback_srgb_rgba8(device, &target.texture, width, height)
    } else {
        readback_tonemapped_rgba8(device, &target.texture, width, height)
    }
}

fn warmup_assets(
    runtime: &mut PresetRuntime,
    target: &RenderTarget,
    device: &GpuDevice,
    options: &CaptureOptions,
    overall_started: Instant,
) -> CaptureResult<()> {
    let started = Instant::now();
    while runtime.warmup_pending() {
        ensure_wall_limit(started, "asset warmup", 30.0)?;
        ensure_wall_limit(overall_started, "capture", options.max_seconds)?;
        thread::sleep(Duration::from_millis(10));
        // Asset IO must settle without advancing the fluid or authored clock.
        render_frame(runtime, target, device, 0, 0.0, 0.0, options, true)?;
    }
    Ok(())
}

fn run(options: &CaptureOptions) -> CaptureResult<()> {
    fs::create_dir_all(&options.output_dir)?;
    let json = read_preset(&options.preset_path)?;
    fs::write(options.output_dir.join("preset.json"), &json)?;
    let preset = preset_settings(&json)?;
    // A graph with its own display transform is read back as the app presents
    // it; a second Reinhard curve on top darkens it and makes stills misleading.
    let tone_mapped = find_preset_node(&serde_json::from_str::<serde_json::Value>(&json)?["nodes"], &|node| {
        node["typeId"] == "node.tone_map"
    })
    .is_some();
    let options = &CaptureOptions {
        solver: preset.solver,
        frame_node: preset.frame_node.clone(),
        linear: options.linear || tone_mapped,
        ..options.clone()
    };
    if options.solver == Solver::None && (options.gpu_surface || options.look_metrics || options.dump_mesh) {
        return Err(io::Error::other("--gpu-surface, --look-metrics and --dump-mesh need a liquid in the preset").into());
    }
    let instrumented_json =
        instrument_preset(&json, options.cinematic, options.supersample, options.solver, &options.frame_node)?;
    if options.cinematic {
        fs::write(
            options.output_dir.join("preset.instrumented.json"),
            &instrumented_json,
        )?;
    }
    let frame_dt = 1.0 / f64::from(options.fps);
    let device = Arc::new(GpuDevice::new_queued("fluid_capture"));
    // The wall limit bounds the capture, not the wait for another holder of the GPU.
    let overall_started = Instant::now();
    let offline_target = RenderTarget::new(
        &device,
        options.width,
        options.height,
        GpuTextureFormat::Rgba16Float,
        "fluid-capture-offline",
    );
    let offline_scope = PhysicsStepScope::for_render(true);
    let (render_width, render_height) =
        render_dimensions(options.width, options.height, options.supersample);
    let mut offline_runtime =
        build_runtime(&instrumented_json, &device, render_width, render_height, &options.frame_node)?;
    let dump_arrays = options.gpu_surface || options.look_metrics || options.dump_mesh;
    offline_runtime.set_dump_all(options.cinematic || dump_arrays);
    let (initial_timings, initial_fluid) = render_frame(
        &mut offline_runtime,
        &offline_target,
        &device,
        0,
        0.0,
        frame_dt,
        options,
        true,
    )?;
    warmup_assets(
        &mut offline_runtime,
        &offline_target,
        &device,
        options,
        overall_started,
    )?;
    if options.cinematic {
        verify_cinematic_dimensions(&offline_runtime, options)?;
        offline_runtime.set_dump_all(dump_arrays);
    }
    if initial_fluid.simulation_time.abs() > 1e-4 {
        return Err(io::Error::other(format!(
            "offline initial simulation time is not zero: {}",
            initial_fluid.simulation_time
        ))
        .into());
    }

    let mut offline_rows = Vec::with_capacity(options.frames as usize);
    let mut offline_csv = CsvWriter::create(&options.output_dir.join("offline.csv"))?;
    let mut offline_rgba_writer = match options.stills_every {
        None => Some(BufWriter::new(File::create(
            options.output_dir.join("offline.rgba"),
        )?)),
        Some(_) => None,
    };
    if options.stills_every.is_some() {
        fs::create_dir_all(options.output_dir.join("stills"))?;
    }
    if options.dump_mesh {
        fs::create_dir_all(options.output_dir.join("mesh"))?;
    }
    let mut capture_ms = 0.0;
    let mut look = options.look_metrics.then(|| {
        let layout = domain_layout(None, preset.domain_size as f32, preset.resolution)
            .expect("preset domain settings were validated");
        let spacing = layout.cell_size as f32 / (preset.points_per_cell as f32).cbrt();
        LookRecorder::new(Cells::from_layout(&layout), spacing)
    });
    for frame in 1..=options.frames {
        ensure_wall_limit(overall_started, "offline pass", options.max_seconds)?;
        let authored_time = f64::from(frame) * frame_dt;
        let (timings, mut fluid) = render_output_frame(
            &mut offline_runtime,
            &offline_target,
            &device,
            frame,
            authored_time,
            frame_dt,
            options,
        )?;
        let cpu_vertex_count = fluid.vertex_count as usize;
        if options.gpu_surface {
            if fluid.particle_count < 1.0 {
                return Err(io::Error::other(format!(
                    "offline frame {frame} published no liquid particles"
                ))
                .into());
            }
            fluid.vertex_count = gpu_surface_vertices(&offline_runtime, &device, frame)? as f64;
            if fluid.vertex_count < 3.0 {
                let arrays: Vec<String> = offline_runtime
                    .dump_arrays_all()
                    .iter()
                    .map(|array| format!("{}.{} ({} bytes)", array.type_id, array.port, array.buffer.size()))
                    .collect();
                return Err(io::Error::other(format!(
                    "offline frame {frame}: the Liquid Surface mesh is empty; arrays this frame: {}",
                    arrays.join(", ")
                ))
                .into());
            }
        } else if options.solver == Solver::Gpu {
            if fluid.particle_count < 1.0 {
                return Err(io::Error::other(format!(
                    "offline frame {frame} published no liquid particles"
                ))
                .into());
            }
        } else if options.solver == Solver::Flip && fluid.vertex_count < 3.0 {
            return Err(io::Error::other(format!(
                "offline frame {frame} produced an empty fluid mesh"
            ))
            .into());
        }
        if let Some(look) = look.as_mut() {
            let particles = published_particles(&offline_runtime, &device, &options.frame_node, fluid.particle_count)?;
            look.observe(authored_time as f32, &particles);
        }
        let expected_time = preset.speed * (authored_time * FIXED_HZ + 1e-8).floor() / FIXED_HZ;
        if options.solver != Solver::None && (fluid.simulation_time - expected_time).abs() > 1e-4 {
            return Err(io::Error::other(format!(
                "offline frame {frame} simulation time {:.9} does not reach fixed-tick time {:.9}",
                fluid.simulation_time, expected_time
            ))
            .into());
        }
        let mut row = metric_row(frame, authored_time, timings, fluid, 0.0)?;
        let should_capture = options
            .stills_every
            .is_none_or(|every| frame % every == 0 || frame == options.frames || frame == 1);
        if should_capture && options.dump_mesh {
            dump_surface_meshes(
                &offline_runtime,
                &device,
                frame,
                cpu_vertex_count,
                &fluid,
                &options.frame_node,
                &options.output_dir.join("mesh"),
            )?;
        }
        if should_capture {
            let capture_started = Instant::now();
            let rgba = readback_rgba(
                &device,
                &offline_target,
                options.width,
                options.height,
                options.linear,
            );
            let expected_rgba_len = (options.width as usize)
                .checked_mul(options.height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .expect("offline RGBA size fits usize");
            if rgba.len() != expected_rgba_len {
                return Err(io::Error::other(format!(
                    "offline frame {frame} readback has {} bytes, expected {expected_rgba_len}",
                    rgba.len()
                ))
                .into());
            }
            if let Some(writer) = offline_rgba_writer.as_mut() {
                writer.write_all(&rgba)?;
            } else {
                let png = encode_rgba8_png(&rgba, options.width, options.height);
                let path = options
                    .output_dir
                    .join("stills")
                    .join(format!("frame_{frame:06}.png"));
                fs::write(path, png)?;
            }
            row.capture_ms = capture_started.elapsed().as_secs_f64() * 1000.0;
            if !row.capture_ms.is_finite() || row.capture_ms < 0.0 {
                return Err(io::Error::other(format!(
                    "offline frame {frame} capture timing is invalid: {}",
                    row.capture_ms
                ))
                .into());
            }
            capture_ms += row.capture_ms;
        }
        offline_csv.write_row(&row)?;
        offline_rows.push(row);
    }
    ensure_wall_limit(overall_started, "offline pass", options.max_seconds)?;
    if let Some(mut writer) = offline_rgba_writer {
        writer.flush()?;
    }
    offline_csv.flush()?;
    let offline_end_simulation_time = offline_rows
        .last()
        .expect("offline rows contain all stepped frames")
        .fluid
        .simulation_time;
    if let Some(look) = &look {
        write_look_metrics(&options.output_dir, look)?;
    }
    drop(offline_target);
    drop(offline_runtime);
    drop(offline_scope);

    let preview_metadata = if options.offline_only {
        None
    } else {
        let preview_target = RenderTarget::new(
            &device,
            options.width,
            options.height,
            GpuTextureFormat::Rgba16Float,
            "fluid-capture-preview",
        );
        let mut preview_runtime =
            build_runtime(&instrumented_json, &device, render_width, render_height, &options.frame_node)?;
        let preview_initial_timings;
        {
            let preview_warmup_scope = PhysicsStepScope::for_render(true);
            let (warmup_timings, warmup_fluid) = render_frame(
                &mut preview_runtime,
                &preview_target,
                &device,
                0,
                0.0,
                frame_dt,
                options,
                true,
            )?;
            warmup_assets(
                &mut preview_runtime,
                &preview_target,
                &device,
                options,
                overall_started,
            )?;
            if warmup_fluid.simulation_time.abs() > 1e-4 {
                return Err(io::Error::other(format!(
                    "preview warmup simulation time is not zero: {}",
                    warmup_fluid.simulation_time
                ))
                .into());
            }
            preview_initial_timings = InitialFrameTimings {
                render_cpu_ms: warmup_timings.render_cpu_ms,
                submit_wait_ms: warmup_timings.submit_wait_ms,
                gpu_ms: warmup_timings.gpu_ms,
                frame_ms: warmup_timings.frame_ms,
            };
            drop(preview_warmup_scope);
        }

        let preview_scope = PhysicsStepScope::for_render(false);
        let preview_started = Instant::now();
        let mut preview_rows = Vec::with_capacity(options.frames as usize);
        let mut next_deadline = 0.0f64;
        let mut previous_authored_time = 0.0f64;
        let mut previous_frame_begin: Option<Instant> = None;
        let mut last_simulation_time = 0.0f64;
        let mut accepted_fluid_frames = 0u32;
        let mut total_sim_ticks_advanced = 0u64;

        while preview_started.elapsed().as_secs_f64() < PREVIEW_SECONDS
            && preview_rows.len() < options.frames as usize
        {
            let now = preview_started.elapsed().as_secs_f64();
            if now < next_deadline {
                thread::sleep(Duration::from_secs_f64(next_deadline - now));
            }
            let frame_begin = Instant::now();
            let authored_time = preview_started.elapsed().as_secs_f64();
            let dt = authored_time - previous_authored_time;
            let presentation_interval_ms = previous_frame_begin
                .map(|previous| frame_begin.duration_since(previous).as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            let frame = (preview_rows.len() + 1) as u32;
            let (timings, fluid) = render_output_frame(
                &mut preview_runtime,
                &preview_target,
                &device,
                frame,
                authored_time,
                dt,
                options,
            )?;
            preview_rows.push(metric_row(
                frame,
                authored_time,
                timings,
                fluid,
                presentation_interval_ms,
            )?);
            if fluid.simulation_time > last_simulation_time + 1e-9 {
                accepted_fluid_frames += 1;
                let advanced = ((fluid.simulation_time - last_simulation_time) * FIXED_HZ).round();
                if advanced.is_finite() && advanced > 0.0 {
                    total_sim_ticks_advanced =
                        total_sim_ticks_advanced.saturating_add(advanced as u64);
                }
                last_simulation_time = fluid.simulation_time;
            }
            previous_authored_time = authored_time;
            previous_frame_begin = Some(frame_begin);
            let elapsed = preview_started.elapsed().as_secs_f64();
            next_deadline += frame_dt;
            if next_deadline < elapsed {
                next_deadline = (elapsed / frame_dt).floor() * frame_dt + frame_dt;
            }
            let preview_max_seconds = PREVIEW_MAX_SECONDS.min(options.max_seconds);
            if elapsed > preview_max_seconds {
                return Err(io::Error::other(format!(
                    "preview pass exceeded wall limit of {preview_max_seconds:.0}s ({elapsed:.3}s)"
                ))
                .into());
            }
        }
        let preview_elapsed_s = preview_started.elapsed().as_secs_f64();
        let preview_max_seconds = PREVIEW_MAX_SECONDS.min(options.max_seconds);
        if preview_elapsed_s > preview_max_seconds {
            return Err(io::Error::other(format!(
            "preview pass exceeded wall limit of {preview_max_seconds:.0}s ({preview_elapsed_s:.3}s)"
        ))
        .into());
        }
        let preview_final = preview_rows.last().ok_or_else(|| {
            io::Error::other("preview pass delivered no frames before its deadline")
        })?;
        let preview_metadata = PreviewMetadata {
            elapsed_s: preview_elapsed_s,
            actual_frames: preview_rows.len() as u32,
            fps: options.fps,
            initial_frame: preview_initial_timings,
            accepted_fluid_frames,
            total_sim_ticks_advanced,
            start_simulation_time: 0.0,
            end_simulation_time: preview_final.fluid.simulation_time,
            final_authored_time: preview_final.authored_time,
            final_lag_seconds: preview_final.fluid.lag_seconds,
        };
        drop(preview_scope);
        drop(preview_target);
        drop(preview_runtime);
        ensure_wall_limit(overall_started, "capture", options.max_seconds)?;
        write_csv(&options.output_dir.join("preview.csv"), &preview_rows)?;
        Some(preview_metadata)
    };

    if options.offline_only {
        write_csv(&options.output_dir.join("preview.csv"), &[])?;
    }
    let sample_count = if options.cinematic { 2 } else { 1 };
    let shutter_interval_s = if options.cinematic {
        1.0 / FIXED_HZ
    } else {
        0.0
    };
    let output_frame_generation_ms = offline_rows
        .iter()
        .map(|row| row.timings.frame_ms + row.capture_ms)
        .sum();
    let metadata = Metadata {
        preset_path: options.preset_path.display().to_string(),
        gpu_device_name: device.device_name(),
        width: options.width,
        height: options.height,
        render_width,
        render_height,
        grid: [preset.resolution; 3],
        cell_size: preset.domain_size / f64::from(preset.resolution),
        surface_detail: preset.surface_detail,
        viscosity: preset.viscosity,
        surface_tension: preset.surface_tension,
        min_substeps: manifold_fluids::TimeStepOptions::default().min_substeps,
        max_substeps: manifold_fluids::TimeStepOptions::default().max_substeps,
        cfl: manifold_fluids::TimeStepOptions::default().cfl,
        adaptive_obstacles: manifold_fluids::TimeStepOptions::default().adaptive_obstacles,
        native_fixed_hz: FIXED_HZ,
        capture_fps: options.fps,
        display_transform: if options.linear {
            "linear_srgb"
        } else {
            "reinhard"
        },
        cinematic: options.cinematic,
        sample_count,
        shutter_interval_s,
        spatial_sampling: options.supersample,
        offline_only: options.offline_only,
        stills_every: options.stills_every,
        offline: PassMetadata {
            frames: options.frames,
            fps: options.fps,
            initial_frame: InitialFrameTimings {
                render_cpu_ms: initial_timings.render_cpu_ms,
                submit_wait_ms: initial_timings.submit_wait_ms,
                gpu_ms: initial_timings.gpu_ms,
                frame_ms: initial_timings.frame_ms,
            },
            start_simulation_time: 0.0,
            end_simulation_time: offline_end_simulation_time,
            capture_ms,
            output_frame_generation_ms,
            sample_count,
            shutter_interval_s,
            spatial_sampling: options.supersample,
        },
        preview: preview_metadata,
    };
    let mut metadata_writer =
        BufWriter::new(File::create(options.output_dir.join("metadata.json"))?);
    serde_json::to_writer_pretty(&mut metadata_writer, &metadata)?;
    writeln!(metadata_writer)?;
    metadata_writer.flush()?;
    ensure_wall_limit(overall_started, "capture", options.max_seconds)?;
    Ok(())
}

fn main() -> CaptureResult<()> {
    let options = parse_options()?;
    run(&options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_liquid_on_the_seam_is_found_by_its_frame() {
        let matter = preset_settings(include_str!(
            "../assets/generator-presets/WaterDamBreakMatter.json"
        ))
        .unwrap();
        assert_eq!((matter.solver, matter.frame_node.as_str()), (Solver::Gpu, "matter_frame"));
        assert_eq!((matter.resolution, matter.domain_size, matter.points_per_cell), (64, 4.0, 8));
        let gpu_flip = preset_settings(include_str!(
            "../assets/generator-presets/WaterDamBreakGpuFlip.json"
        ))
        .unwrap();
        assert_eq!((gpu_flip.solver, gpu_flip.frame_node.as_str()), (Solver::Gpu, "frame"));
        assert_eq!((gpu_flip.resolution, gpu_flip.domain_size, gpu_flip.speed), (64, 4.0, 1.0));
        let sea_wall = preset_settings(include_str!(
            "../assets/generator-presets/WaterSeaWallGpuFlip.json"
        ))
        .unwrap();
        assert_eq!(sea_wall.speed, 0.5);
        let flip = preset_settings(include_str!("../tests/fixtures/cpu-flip/WaterDamBreak.json")).unwrap();
        assert_eq!((flip.solver, flip.frame_node.as_str()), (Solver::Flip, "fluid_surface"));
        let ocean = preset_settings(include_str!("../assets/generator-presets/Ocean.json")).unwrap();
        assert_eq!((ocean.solver, ocean.frame_node.as_str()), (Solver::None, ""));
    }

    #[test]
    fn temporal_samples_cover_two_fixed_ticks() {
        for frame in [1, 2, 450] {
            let samples = temporal_samples(f64::from(frame) / 30.0);
            assert!((samples[0].0 * 60.0 - f64::from(frame * 2 - 1)).abs() < 1e-10);
            assert!((samples[1].0 * 60.0 - f64::from(frame * 2)).abs() < 1e-10);
            assert!((samples[0].1 - 1.0 / 60.0).abs() < 1e-12);
            assert!((samples[1].1 - 1.0 / 60.0).abs() < 1e-12);
        }
    }

    #[test]
    fn cinematic_graph_inserts_average_and_spatial_resolve() {
        let source = serde_json::json!({
            "nodes": [
                {"id": 0, "nodeId": "fluid_surface", "typeId": FLIP_DOMAIN_TYPE_ID, "params": {
                    "resolution": {"type": "Int", "value": 24},
                    "domain_size": {"type": "Float", "value": 4.0},
                    "surface_subdivisions": {"type": "Int", "value": 0}
                }},
                {"id": 1, "nodeId": "scene", "typeId": "node.render_scene"},
                {"id": 2, "nodeId": "tone", "typeId": "node.tone_map"}
            ],
            "wires": [{"fromNode": 1, "fromPort": "color", "toNode": 2, "toPort": "in"}]
        });
        let instrumented = serde_json::from_str::<serde_json::Value>(
            &instrument_preset(&source.to_string(), true, 2, Solver::Flip, "fluid_surface").unwrap(),
        )
        .unwrap();
        let nodes = instrumented["nodes"].as_array().unwrap();
        let feedback = nodes
            .iter()
            .find(|node| node["nodeId"] == "cinematic_feedback")
            .unwrap();
        let mix = nodes
            .iter()
            .find(|node| node["nodeId"] == "cinematic_temporal_mix")
            .unwrap();
        let downsample = nodes
            .iter()
            .find(|node| node["nodeId"] == "cinematic_downsample")
            .unwrap();
        assert_eq!(feedback["params"]["copy_capture"]["value"], true);
        assert_eq!(mix["params"]["amount"]["value"], 0.5);
        assert!(feedback.get("outputCanvasScales").is_none());
        assert!(mix.get("outputCanvasScales").is_none());
        assert_eq!(render_dimensions(1920, 1080, 2), (3840, 2160));
        assert_eq!(downsample["params"]["factor"]["value"], 0);
        assert!(
            instrumented["wires"]
                .as_array()
                .unwrap()
                .iter()
                .any(|wire| {
                    wire["fromNode"] == downsample["id"]
                        && wire["toNode"] == 2
                        && wire["toPort"] == "in"
                })
        );
    }
}

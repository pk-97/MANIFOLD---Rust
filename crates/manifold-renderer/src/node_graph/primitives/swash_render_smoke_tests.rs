//! SWASH end to end through the render graph the app shows (`render_def`):
//! particles → GPU Liquid Surface → water material with volume optics → tone
//! map → frames, run by `PresetRuntime` as the app runs a generator. A long
//! run watches for GPU faults, non-finite particles, collar or mesh past
//! capacity, a mesh leaving the tank, frame-time creep and memory growth; it
//! splits each frame's GPU and CPU time by stage from timestamped frames, and
//! checks what the transport can do to a liquid with no clock: pause, reset
//! by trigger, `clear_state`. `fft_water_rendered_scenes_cover_every_dispatch`
//! proves every array these graphs allocate at each lattice run here, and
//! each run first checks its arrays fit the device. Hours long at the large
//! lattices, so opt-in: `--features water-race-probes`.
//!
//! `SWASH_SMOKE_DIR` names the output directory (stills, mp4, timing CSV);
//! `SWASH_SMOKE_FRAMES` the run length (900 when unset).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use manifold_core::NodeId;
use manifold_core::params::ParamManifest;
use manifold_gpu::GpuTextureFormat;

use super::swash_extent_tests::rendered_scene_bytes;
use super::swash_preset::{DAM_MIN, WaterScene, render_def};
use super::swash_solve_tests::output_of;
use crate::frame_status::FrameRenderStatus;
use crate::generators::mesh_common::MeshVertex;
use crate::gpu_encoder::GpuEncoder;
use crate::headless_readback::{encode_rgba8_png, readback_srgb_rgba8};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
use crate::node_graph::{NodeInstanceId, PrimitiveRegistry};
use crate::preset_context::PresetContext;
use crate::preset_runtime::PresetRuntime;
use crate::render_target::RenderTarget;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
/// Peter watches on his phone; its upload limit is 30 MB.
const PHONE_LIMIT_BYTES: u64 = 25 * 1024 * 1024;
const TANK: f64 = 4.0;
/// Every this many frames is timestamped per dispatch for the stage split;
/// those frames are left out of the whole-frame timing.
const PROFILE_EVERY: usize = 25;
const STILLS: [usize; 4] = [90, 240, 600, 900];

/// Stages in the order the table prints them.
const STAGES: [&str; 23] = [
    "fill + particle state",
    "particle sort",
    "classify cells",
    "particle→face",
    "gravity + walls",
    "extrapolation",
    "divergence",
    "solve setup (collar, charts, rhs box)",
    "solve helper (passes)",
    "solve box (passes)",
    "solve Krylov (passes)",
    "solve finish (λ, final box, p)",
    "pressure gradient",
    "density solve (source, passes, spread)",
    "face→particle + advect",
    "surface sort",
    "surface blobs",
    "surface volume",
    "surface smoothing",
    "surface marching cubes",
    "scene setup (env, lights, objects)",
    "scene render",
    "tone map + other",
];

/// Which stage a node belongs to, by its graph name.
fn stage(name: &str) -> &'static str {
    let in_step = name.split_once('.').filter(|(p, _)| p.len() > 1 && p.starts_with('s') && p[1..].parse::<u32>().is_ok());
    if let Some((_, local)) = in_step {
        if local.starts_with("density.") {
            return "density solve (source, passes, spread)";
        }
        return match local {
            "sort" => "particle sort",
            "water" => "classify cells",
            "faces" => "particle→face",
            "gravity" => "gravity + walls",
            "divergence" => "divergence",
            "project" => "pressure gradient",
            "move" => "face→particle + advect",
            l if l.starts_with("old_extend") || l.starts_with("new_extend") => "extrapolation",
            l if l.starts_with("helper_") => "solve helper (passes)",
            l if l.starts_with("pass_box_") || matches!(l, "pass_source" | "sum_z" | "w") => "solve box (passes)",
            "h1" | "w1" | "h2" | "w2" | "norm" | "next" | "givens" | "krylov" => "solve Krylov (passes)",
            l if l.starts_with("final_helper_") || l.starts_with("final_box_") => "solve finish (λ, final box, p)",
            "final_source" | "y" | "u" | "pressure" => "solve finish (λ, final box, p)",
            _ => "solve setup (collar, charts, rhs box)",
        };
    }
    match name {
        "fill" | "state" => "fill + particle state",
        "scene" => "scene render",
        "filmic_display" => "tone map + other",
        n if n.ends_with("liquid_sort") => "surface sort",
        n if n.ends_with("liquid_blobs") => "surface blobs",
        n if n.ends_with("liquid_volume") => "surface volume",
        n if n.contains("liquid_smooth_") => "surface smoothing",
        n if n.ends_with("liquid_count") || n.ends_with("liquid_offsets") || n.ends_with("liquid_mesh") => {
            "surface marching cubes"
        }
        _ => "scene setup (env, lights, objects)",
    }
}

fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("SWASH_SMOKE_DIR").map_or_else(|| std::env::temp_dir().join("swash-smoke"), PathBuf::from);
    std::fs::create_dir_all(&dir).expect("output directory");
    dir
}

fn frames() -> usize {
    std::env::var("SWASH_SMOKE_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(900)
}

fn host_rss_mb() -> f64 {
    let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output();
    out.ok().and_then(|o| String::from_utf8(o.stdout).ok()).and_then(|s| s.trim().parse::<f64>().ok()).map_or(f64::NAN, |kb| kb / 1024.0)
}

/// Health of one particle frame.
#[derive(Debug, Default, Clone, Copy)]
struct ParticleHealth {
    live: usize,
    non_finite: usize,
    outside_tank: usize,
    fastest: f64,
}

fn particle_health(particles: &[FluidParticle]) -> ParticleHealth {
    let mut h = ParticleHealth::default();
    for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
        h.live += 1;
        if !p.position_radius.iter().chain(&p.velocity).all(|v| v.is_finite()) {
            h.non_finite += 1;
            continue;
        }
        let local: [f64; 3] = std::array::from_fn(|a| f64::from(p.position_radius[a]) - DAM_MIN[a]);
        if local.iter().any(|&x| !(-1e-4..=TANK + 1e-4).contains(&x)) {
            h.outside_tank += 1;
        }
        let v = p.velocity.map(f64::from);
        h.fastest = h.fastest.max((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt());
    }
    h
}

/// One scene through the render graph, frame by frame.
struct Smoke {
    device: crate::TestDevice,
    runtime: PresetRuntime,
    target: RenderTarget,
    scene: WaterScene,
    sampler: manifold_gpu::GpuTimestampSampler,
    /// Graph name of each plan step, for the stage split.
    step_names: Vec<String>,
    solid: NodeInstanceId,
    frame_count: i64,
    time: f64,
    trigger: u32,
    critical: Vec<String>,
}

struct FrameResult {
    gpu_ms: f64,
    cpu_ms: f64,
    status: FrameRenderStatus,
    /// Stage → (GPU ms, CPU ms) on a profiled frame.
    stages: Option<Vec<(f64, f64)>>,
    profiled_total: f64,
    unattributed_spans: usize,
}

impl Smoke {
    fn new(scene: WaterScene) -> Self {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        let device = crate::test_device();
        let runtime = PresetRuntime::from_def_with_device(
            render_def(scene),
            &registry,
            device.arc(),
            WIDTH,
            HEIGHT,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("render def builds on the device");
        let target = RenderTarget::new(&device, WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, "swash-smoke");
        let sampler = device.create_timestamp_sampler(32_768).expect("timestamp sampling");
        let name_of = |id: NodeInstanceId| {
            runtime.graph.nodes().find(|n| n.id == id).map_or_else(String::new, |n| n.node_id.as_str().to_string())
        };
        let step_names = runtime.plan.steps().iter().map(|s| name_of(s.node)).collect();
        let solid = runtime.graph.nodes().find(|n| n.node_id.as_str() == "solid").expect("solid source").id;
        let mut smoke = Self {
            device,
            runtime,
            target,
            scene,
            sampler,
            step_names,
            solid,
            frame_count: 0,
            time: 0.0,
            trigger: 0,
            critical: Vec::new(),
        };
        // Hold what is read after each frame past it.
        let last = scene.steps - 1;
        let mut watched: Vec<String> = vec!["solid".into(), "fill".into(), format!("s{last}.move")];
        watched.extend((0..scene.steps).map(|k| format!("s{k}.collar_total")));
        for suffix in ["liquid_offsets", "liquid_mesh"] {
            let found = smoke.runtime.graph.nodes().find(|n| n.node_id.as_str().ends_with(suffix)).expect("surface node");
            watched.push(found.node_id.as_str().to_string());
        }
        let ids: Vec<NodeId> = watched.iter().map(|name| NodeId::from(name.as_str())).collect();
        smoke.runtime.set_dump_visible(None, &ids);
        smoke
    }

    /// The surface's solid lattice holds no solid; the planner may recycle a
    /// source's storage, so it is written before every frame.
    fn clear_solid(&self) {
        let resource = output_of(&self.runtime.plan, self.solid, "out");
        let backend = self.runtime.backend_for_test();
        let buffer = backend.array_buffer(backend.slot_for(resource).expect("solid bound")).expect("solid buffer");
        let nodes = self.scene.surface_nodes().pow(3);
        assert!(buffer.size as usize >= nodes * 4, "the solid source holds the corner lattice");
        // SAFETY: shared storage of at least `nodes` floats; no frame is in flight.
        unsafe { buffer.write(0, bytemuck::cast_slice(&vec![0.0_f32; nodes])) };
    }

    /// One rendered frame. `dt` 0 with an unchanged clock is a paused
    /// transport that still renders. Metal's autoreleased objects drain per
    /// frame, as the content thread drains them.
    fn frame(&mut self, dt: f64, profile: bool) -> FrameResult {
        objc2::rc::autoreleasepool(|_| self.frame_inner(dt, profile))
    }

    fn readback(&self) -> Vec<u8> {
        objc2::rc::autoreleasepool(|_| readback_srgb_rgba8(&self.device, &self.target.texture, WIDTH, HEIGHT))
    }

    fn frame_inner(&mut self, dt: f64, profile: bool) -> FrameResult {
        self.clear_solid();
        if dt > 0.0 {
            self.time += dt;
            self.frame_count += 1;
        }
        let ctx = PresetContext {
            time: self.time,
            beat: self.time * 2.0,
            dt: dt as f32,
            width: WIDTH,
            height: HEIGHT,
            output_width: WIDTH,
            output_height: HEIGHT,
            aspect: WIDTH as f32 / HEIGHT as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: self.frame_count,
            anim_progress: 0.0,
            trigger_count: self.trigger,
        };
        let mut enc = self.device.create_encoder("swash-smoke");
        if profile {
            enc.enable_dispatch_profiling(self.sampler.clone(), &self.device);
        }
        self.runtime.set_profiling(profile);
        let start = Instant::now();
        let status = {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            self.runtime.render(&mut gpu, &self.target.texture, &ctx, &ParamManifest::default());
            gpu.frame_status()
        };
        let cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
        let result = enc.commit_and_wait_profiled(&self.device);
        assert_eq!(
            result.failed_command_buffers, 0,
            "CRITICAL: frame {} failed on the GPU ({} command buffers)",
            self.frame_count, result.failed_command_buffers
        );
        let mut stages = None;
        let mut unattributed = 0;
        if profile {
            let steps = self.runtime.take_step_profiles();
            let mut split = vec![(0.0, 0.0); STAGES.len()];
            let index = |name: &str| STAGES.iter().position(|s| *s == stage(name)).expect("known stage");
            for step in &steps {
                split[index(&self.step_names[step.step_idx])].1 += step.cpu_nanos as f64 / 1e6;
            }
            let step_of = |tag: &str| tag.rsplit_once(":s").and_then(|(_, idx)| idx.parse::<usize>().ok());
            let mut spans: Vec<_> = result.spans.iter().collect();
            spans.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));
            let mut end = 0.0_f64;
            for span in spans {
                // Untimed vendor work (the MPSGraph FFTs) shows as the gap
                // before the next timed dispatch; it is that dispatch's stage.
                let charged = span.millis + (span.start_ms - end).max(0.0);
                end = end.max(span.start_ms + span.millis);
                match step_of(&span.tag) {
                    Some(idx) if idx < self.step_names.len() => split[index(&self.step_names[idx])].0 += charged,
                    _ => {
                        unattributed += 1;
                        split[STAGES.len() - 1].0 += charged;
                    }
                }
            }
            unattributed += result.overflow + result.invalid;
            stages = Some(split);
        }
        self.runtime.set_profiling(false);
        FrameResult { gpu_ms: result.total_ms, cpu_ms, status, stages, profiled_total: result.total_ms, unattributed_spans: unattributed }
    }

    fn dumped<T: bytemuck::Pod>(&self, name: &str, port: &str, len: usize) -> Vec<T> {
        let arrays = self.runtime.dump_arrays_all();
        let array = arrays
            .iter()
            .find(|a| a.name == name && a.port == port)
            .unwrap_or_else(|| panic!("{name}.{port} is not held; held: {:?}", arrays.iter().map(|a| format!("{}.{}", a.name, a.port)).collect::<Vec<_>>()));
        assert!(array.buffer.size() as usize >= len * std::mem::size_of::<T>(), "{name}.{port} is shorter than {len} records");
        let ptr = array.buffer.mapped_ptr().expect("shared storage");
        // SAFETY: the frame completed and the buffer holds `len` records.
        unsafe { std::slice::from_raw_parts(ptr.cast::<T>().cast_const(), len) }.to_vec()
    }

    fn surface_name(&self, suffix: &str) -> String {
        self.runtime.graph.nodes().find(|n| n.node_id.as_str().ends_with(suffix)).expect("surface node").node_id.as_str().to_string()
    }

    fn particles(&self) -> Vec<FluidParticle> {
        self.dumped(&format!("s{}.move", self.scene.steps - 1), "out", self.scene.particles() as usize)
    }

    fn collar(&self, step: usize) -> u32 {
        let cells = self.scene.pressure.cells();
        *self.dumped::<u32>(&format!("s{step}.collar_total"), "out", cells).last().expect("lattice")
    }

    /// Live triangles and, over their vertices, the bounding box and how many
    /// are not finite.
    fn mesh(&self) -> (u32, [f64; 3], [f64; 3], usize) {
        let triangles = self.dumped::<u32>(&self.surface_name("liquid_offsets"), "extent", 1)[0];
        let capacity = self.mesh_capacity();
        let live = (3 * triangles as usize).min(capacity);
        let vertices: Vec<MeshVertex> = self.dumped(&self.surface_name("liquid_mesh"), "vertices", live);
        let (mut low, mut high, mut bad) = ([f64::MAX; 3], [f64::MIN; 3], 0);
        for v in &vertices {
            if !v.position.iter().chain(&v.normal).all(|x| x.is_finite()) {
                bad += 1;
                continue;
            }
            for a in 0..3 {
                low[a] = low[a].min(f64::from(v.position[a]));
                high[a] = high[a].max(f64::from(v.position[a]));
            }
        }
        (triangles, low, high, bad)
    }

    fn mesh_capacity(&self) -> usize {
        let name = self.surface_name("liquid_mesh");
        let node = self.runtime.graph.nodes().find(|n| n.node_id.as_str() == name).expect("mesh node");
        match node.params.get("max_capacity") {
            Some(crate::node_graph::parameters::ParamValue::Float(v)) => (v.clamp(3.0, 16_777_215.0) as usize / 3) * 3,
            other => panic!("mesh max_capacity is {other:?}"),
        }
    }

    fn still(&self, path: &Path) {
        let rgba = self.readback();
        std::fs::write(path, encode_rgba8_png(&rgba, WIDTH, HEIGHT)).expect("still written");
    }

    fn memory_mb(&self) -> f64 {
        self.device.modifier_memory_snapshot().map_or(f64::NAN, |m| m.current_allocated_bytes as f64 / (1 << 20) as f64)
    }
}

/// A 1080p H.264 copy under the phone's upload limit, raising the CRF until
/// it fits.
fn phone_copy(source: &Path, phone: &Path) {
    for crf in [26, 28, 30, 32, 35] {
        let status = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-i"])
            .arg(source)
            .args(["-vf", "scale=-2:1080", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", &crf.to_string(), "-movflags", "+faststart"])
            .arg(phone)
            .status();
        let size = std::fs::metadata(phone).map_or(u64::MAX, |m| m.len());
        println!("SMOKE phone copy {} at CRF {crf}: {:.1} MB", phone.display(), size as f64 / 1048576.0);
        if status.is_ok_and(|s| s.success()) && size < PHONE_LIMIT_BYTES {
            return;
        }
    }
    println!("SMOKE phone copy {}: could not fit under the limit", phone.display());
}

/// The largest move of any one particle between two frames, matched by id:
/// every step re-sorts the array, so slots do not follow particles.
fn max_diff(a: &[FluidParticle], b: &[FluidParticle]) -> f64 {
    let mut at = vec![None; a.len() + 1];
    for p in a {
        if let Some(slot) = at.get_mut(p.id as usize) {
            *slot = Some(p.position_radius);
        }
    }
    b.iter()
        .filter_map(|q| {
            let p = at.get(q.id as usize).copied().flatten()?;
            Some((0..3).map(|i| f64::from((p[i] - q.position_radius[i]).abs())).fold(0.0, f64::max))
        })
        .fold(0.0, f64::max)
}

/// Ids outside 1..=n or seen twice: a particle lost or copied by a step.
fn id_faults(particles: &[FluidParticle]) -> usize {
    let mut seen = vec![false; particles.len() + 1];
    let mut faults = 0;
    for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
        match seen.get_mut(p.id as usize) {
            Some(s) if p.id > 0 && !*s => *s = true,
            _ => faults += 1,
        }
    }
    faults
}

/// The long run of one scene at one lattice. Panics at the first GPU fault,
/// non-finite particle or collar past capacity (a collar past capacity would
/// solve the wrong problem and read past the collar vectors).
fn run(scene: WaterScene, label: &str, transport: bool) {
    let n = scene.pressure.n;
    let dir = out_dir();
    let frames = frames();
    let tag = format!("{label}_{n}");
    println!("SMOKE {tag}: {} particles, collar capacity {}, {WIDTH}x{HEIGHT}", scene.particles(), scene.pressure.capacity);
    // The CPU census gates the run: a scene whose arrays don't fit what the
    // device can still hold is refused by name, never tried.
    let needs = rendered_scene_bytes(scene);
    let snapshot = crate::test_device().modifier_memory_snapshot().expect("Metal reports its memory");
    let free = snapshot.recommended_max_working_set_bytes.saturating_sub(snapshot.current_allocated_bytes);
    println!("SMOKE {tag}: arrays need {:.2} GB; the device holds {:.2} GB more", needs as f64 / 1e9, free as f64 / 1e9);
    if needs > free {
        println!("SMOKE {tag}: refused, device memory: needs {:.2} GB, holds {:.2} GB", needs as f64 / 1e9, free as f64 / 1e9);
        return;
    }
    let mem_before = snapshot.current_allocated_bytes as f64 / 1048576.0;
    let build = Instant::now();
    let mut smoke = Smoke::new(scene);
    println!("SMOKE {tag}: runtime built in {:.2} s, GPU memory {mem_before:.0} → {:.0} MB", build.elapsed().as_secs_f64(), smoke.memory_mb());
    let dt = 1.0 / 60.0;

    // The first frame after the build starts from the fill; it is what every
    // reset must reproduce.
    let first_frame = smoke.frame(dt, false);
    let first = smoke.particles();
    let mut warmups = 0;
    while smoke.runtime.warmup_pending() && warmups < 600 {
        smoke.frame(dt, false);
        warmups += 1;
    }
    println!("SMOKE {tag}: first frame status {:?}, {warmups} warm-up frames", first_frame.status);
    // Restart from the fill by the generator trigger, as a clip relaunch does.
    smoke.trigger += 1;
    let restart = smoke.frame(dt, false);
    let restarted = smoke.particles();
    println!("SMOKE {tag}: trigger restart before the run: status {:?}, max |Δx| against the first frame {:.3e} m", restart.status, max_diff(&first, &restarted));

    let ffmpeg = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{WIDTH}x{HEIGHT}"), "-r", "60", "-i", "-"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20"])
        .arg(dir.join(format!("{tag}.mp4")))
        .stdin(std::process::Stdio::piped())
        .spawn();
    let mut ffmpeg = ffmpeg.ok();

    let mut csv = std::fs::File::create(dir.join(format!("{tag}_frames.csv"))).expect("csv");
    writeln!(csv, "frame,profiled,gpu_ms,cpu_ms,collar0,collar1,triangles,mem_mb").unwrap();
    let mut gpu: Vec<(usize, f64)> = Vec::new();
    let mut cpu: Vec<(usize, f64)> = Vec::new();
    let mut stage_gpu: Vec<Vec<f64>> = vec![Vec::new(); STAGES.len()];
    let mut stage_cpu: Vec<Vec<f64>> = vec![Vec::new(); STAGES.len()];
    let (mut profiled_totals, mut unattributed) = (Vec::new(), 0usize);
    let (mut collar_peak, mut tri_peak, mut tri_low) = (0u32, 0u32, u32::MAX);
    let (mut box_low, mut box_high) = ([f64::MAX; 3], [f64::MIN; 3]);
    let mut memory: Vec<(usize, f64)> = Vec::new();
    let mut rss: Vec<(usize, f64)> = Vec::new();
    let (mut fastest, mut bucket_fastest) = (0.0_f64, 0.0_f64);
    let capacity = scene.pressure.capacity as u32;
    let mesh_capacity = smoke.mesh_capacity();
    let wall = Instant::now();
    for frame in 1..=frames {
        let profile = frame % PROFILE_EVERY == 0;
        let r = smoke.frame(dt, profile);
        if r.status != FrameRenderStatus::Complete {
            smoke.critical.push(format!("frame {frame}: status {:?}", r.status));
        }
        if let Some(split) = &r.stages {
            for (i, (g, c)) in split.iter().enumerate() {
                stage_gpu[i].push(*g);
                stage_cpu[i].push(*c);
            }
            profiled_totals.push(r.profiled_total);
            unattributed += r.unattributed_spans;
        } else {
            gpu.push((frame, r.gpu_ms));
            cpu.push((frame, r.cpu_ms));
        }
        let collars: Vec<u32> = (0..scene.steps).map(|k| smoke.collar(k)).collect();
        for (k, &c) in collars.iter().enumerate() {
            collar_peak = collar_peak.max(c);
            assert!(c <= capacity, "CRITICAL: frame {frame} step {k}: collar {c} past capacity {capacity}");
        }
        let (triangles, low, high, bad_vertices) = smoke.mesh();
        tri_peak = tri_peak.max(triangles);
        tri_low = tri_low.min(triangles);
        if 3 * triangles as usize > mesh_capacity {
            smoke.critical.push(format!("frame {frame}: mesh needs {} vertices, capacity {mesh_capacity}", 3 * triangles));
        }
        if triangles == 0 {
            smoke.critical.push(format!("frame {frame}: empty mesh"));
        }
        if bad_vertices > 0 {
            smoke.critical.push(format!("frame {frame}: {bad_vertices} non-finite mesh vertices"));
        }
        for a in 0..3 {
            box_low[a] = box_low[a].min(low[a]);
            box_high[a] = box_high[a].max(high[a]);
            if triangles > 0 && (low[a] < DAM_MIN[a] - 1e-3 || high[a] > DAM_MIN[a] + TANK + 1e-3) {
                smoke.critical.push(format!("frame {frame}: mesh axis {a} spans {:.4}..{:.4}, outside the tank", low[a], high[a]));
            }
        }
        let mem = smoke.memory_mb();
        memory.push((frame, mem));
        writeln!(
            csv,
            "{frame},{},{:.3},{:.3},{},{},{triangles},{mem:.1}",
            u8::from(profile),
            r.gpu_ms,
            r.cpu_ms,
            collars[0],
            collars.get(1).copied().unwrap_or(0)
        )
        .unwrap();
        if frame % 10 == 0 || frame == 1 {
            let particles = smoke.particles();
            let h = particle_health(&particles);
            fastest = fastest.max(h.fastest);
            bucket_fastest = bucket_fastest.max(h.fastest);
            let faults = id_faults(&particles);
            if faults > 0 {
                smoke.critical.push(format!("frame {frame}: {faults} particle ids missing or repeated"));
            }
            assert_eq!(h.non_finite, 0, "CRITICAL: frame {frame}: {} particles not finite", h.non_finite);
            if h.live != scene.particles() as usize {
                smoke.critical.push(format!("frame {frame}: {} live particles of {}", h.live, scene.particles()));
            }
            if h.outside_tank > 0 {
                smoke.critical.push(format!("frame {frame}: {} particles outside the tank", h.outside_tank));
            }
        }
        if frame % 100 == 0 {
            rss.push((frame, host_rss_mb()));
            println!(
                "SMOKE {tag} frame {frame}: {:.2} ms GPU, {:.2} ms CPU, collar {collars:?}, {triangles} triangles, GPU mem {mem:.0} MB, fastest {bucket_fastest:.2} m/s over the last 100, {:.0} s wall",
                r.gpu_ms,
                r.cpu_ms,
                wall.elapsed().as_secs_f64()
            );
            bucket_fastest = 0.0;
        }
        if STILLS.contains(&frame) || frame == frames {
            smoke.still(&dir.join(format!("{tag}_frame{frame:04}.png")));
        }
        if let Some(child) = ffmpeg.as_mut() {
            let rgba = smoke.readback();
            if child.stdin.as_mut().is_some_and(|stdin| stdin.write_all(&rgba).is_err()) {
                println!("SMOKE {tag}: ffmpeg closed its input at frame {frame}");
                ffmpeg = None;
            }
        }
    }
    if let Some(mut child) = ffmpeg.take() {
        drop(child.stdin.take());
        let _ = child.wait();
        phone_copy(&dir.join(format!("{tag}.mp4")), &dir.join(format!("{tag}_phone.mp4")));
    }

    // Whole-frame timing in 100-frame buckets (profiled frames left out).
    println!("SMOKE {tag} timing (unprofiled frames), bucket: GPU p50/p95 | CPU encode p50/p95 | GPU mem MB at bucket end");
    for bucket in 0..frames.div_ceil(100) {
        let range = bucket * 100 + 1..=(bucket + 1) * 100;
        let g: Vec<f64> = gpu.iter().filter(|(f, _)| range.contains(f)).map(|&(_, v)| v).collect();
        let c: Vec<f64> = cpu.iter().filter(|(f, _)| range.contains(f)).map(|&(_, v)| v).collect();
        let m = memory.iter().filter(|(f, _)| range.contains(f)).map(|&(_, v)| v).next_back().unwrap_or(f64::NAN);
        println!(
            "SMOKE {tag}   {:>4}-{:<4} {:7.2} {:7.2} | {:6.2} {:6.2} | {m:.0}",
            range.start(),
            range.end(),
            percentile(&g, 0.5),
            percentile(&g, 0.95),
            percentile(&c, 0.5),
            percentile(&c, 0.95)
        );
    }
    let all_g: Vec<f64> = gpu.iter().map(|&(_, v)| v).collect();
    let all_c: Vec<f64> = cpu.iter().map(|&(_, v)| v).collect();
    println!(
        "SMOKE {tag} whole run: GPU p50 {:.2} p95 {:.2} max {:.2} ms; CPU encode p50 {:.2} p95 {:.2} ms",
        percentile(&all_g, 0.5),
        percentile(&all_g, 0.95),
        percentile(&all_g, 1.0),
        percentile(&all_c, 0.5),
        percentile(&all_c, 0.95)
    );
    println!("SMOKE {tag} host RSS MB by frame: {rss:?}");
    let mem_first = memory.first().map_or(f64::NAN, |m| m.1);
    let mem_max = memory.iter().map(|m| m.1).fold(f64::MIN, f64::max);
    println!("SMOKE {tag} GPU memory: frame 1 {mem_first:.0} MB, max {mem_max:.0} MB, last {:.0} MB", memory.last().map_or(f64::NAN, |m| m.1));
    println!(
        "SMOKE {tag} capacity: collar peak {collar_peak} of {capacity} ({:.1}%); triangles {tri_low}..{tri_peak}, vertices peak {} of {mesh_capacity} ({:.2}%)",
        100.0 * f64::from(collar_peak) / f64::from(capacity),
        3 * tri_peak,
        100.0 * 3.0 * f64::from(tri_peak) / mesh_capacity as f64
    );
    println!("SMOKE {tag} mesh bounds over the run: {box_low:.3?} .. {box_high:.3?} (tank {DAM_MIN:?} + {TANK} m); fastest particle {fastest:.2} m/s");

    // The stage split: median over the timestamped frames, then scaled to the
    // median unprofiled frame (per-dispatch timing adds encoder switches).
    let profiled_median = percentile(&profiled_totals, 0.5);
    let unprofiled_median = percentile(&all_g, 0.5);
    let scale = unprofiled_median / profiled_median;
    println!(
        "SMOKE {tag} stage split over {} timestamped frames (median {profiled_median:.2} ms timestamped, {unprofiled_median:.2} ms plain; {unattributed} unattributed spans):",
        profiled_totals.len()
    );
    println!("SMOKE {tag}   {:<40} {:>9} {:>9} {:>9}", "stage", "GPU ms", "scaled", "CPU ms");
    let mut stage_rows = String::from("stage,gpu_ms_timestamped,gpu_ms_scaled,cpu_ms\n");
    for (i, name) in STAGES.iter().enumerate() {
        let (g, c) = (percentile(&stage_gpu[i], 0.5), percentile(&stage_cpu[i], 0.5));
        println!("SMOKE {tag}   {name:<40} {g:9.3} {:9.3} {c:9.3}", g * scale);
        stage_rows.push_str(&format!("{name},{g:.4},{:.4},{c:.4}\n", g * scale));
    }
    std::fs::write(dir.join(format!("{tag}_stages.csv")), stage_rows).expect("stage csv");

    if transport {
        // Paused transport that keeps rendering: no time, no frame count.
        let before = smoke.particles();
        let image = smoke.readback();
        for _ in 0..3 {
            smoke.frame(0.0, false);
        }
        let paused = smoke.particles();
        let paused_image = smoke.readback();
        let changed = image.chunks_exact(4).zip(paused_image.chunks_exact(4)).filter(|(a, b)| a != b).count();
        println!(
            "SMOKE {tag} transport: 3 paused renders moved particles by up to {:.3e} m and changed {changed} of {} pixels (a clocked liquid holds at 0)",
            max_diff(&before, &paused),
            WIDTH * HEIGHT
        );
        smoke.still(&dir.join(format!("{tag}_paused.png")));
        for _ in 0..30 {
            smoke.frame(dt, false);
        }
        let resumed = particle_health(&smoke.particles());
        println!("SMOKE {tag} transport: resumed 30 frames, {} live, {} not finite", resumed.live, resumed.non_finite);
        smoke.trigger += 1;
        smoke.frame(dt, false);
        let by_trigger = smoke.particles();
        println!("SMOKE {tag} transport: trigger restart mid-run, max |Δx| against the first frame {:.3e} m", max_diff(&first, &by_trigger));
        for _ in 0..30 {
            smoke.frame(dt, false);
        }
        smoke.runtime.clear_state();
        smoke.frame(dt, false);
        let cleared = smoke.particles();
        println!("SMOKE {tag} transport: clear_state mid-run, max |Δx| against the first frame {:.3e} m", max_diff(&first, &cleared));
    }
    println!("SMOKE {tag} critical: {:?}", smoke.critical);
    assert!(smoke.critical.is_empty(), "CRITICAL at {tag}: {:?}", smoke.critical);
}

#[test]
fn swash_render_smoke_32() {
    run(WaterScene::dam_break(32), "dam_break", true);
}

#[test]
fn swash_render_smoke_64() {
    run(WaterScene::dam_break(64), "dam_break", true);
    run(WaterScene::still_pool(64), "still_pool", true);
}

#[test]
fn swash_render_smoke_128() {
    run(WaterScene::dam_break(128), "dam_break", true);
}

/// 256³ as a player would run it, with a coarser surface (the shipped
/// surface lattice would be 769³), beside 128³ at the same surface scale.
/// Refused by name when the census says the device can't hold it.
#[test]
fn swash_render_smoke_256() {
    run(WaterScene::dam_break(128).with_surface_scale(1), "dam_break_surface1", false);
    run(WaterScene::dam_break(256).with_surface_scale(1), "dam_break_surface1", true);
    run(WaterScene::dam_break(256).with_surface_scale(2), "dam_break_surface2", false);
}

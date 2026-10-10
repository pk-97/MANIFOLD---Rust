//! The GPU FLIP water step run on whole scenes (docs/GPU_FLIP_PRESSURE_SOLVE.md
//! section 1 (the step)): the momentum, still-pool and meshed-volume proofs, and the scene
//! runner the other water scene proofs share.
//! `gpu_flip_scenes_cover_every_dispatch` proves every array these graphs
//! allocate before any of them runs here.

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;

#[cfg(test)]
use manifold_nodes_water::primitives::gpu_flip_preset::{DAM_FILL_HEIGHT, REST_PER_CELL, DAM_OBSTACLE};
#[cfg(test)]
use manifold_nodes_water::primitives::gpu_flip_volume::VolumeDrift;
#[cfg(test)]
use manifold_nodes_water::primitives::liquid_stats::SOLVER_WORDS;

use manifold_nodes_water::primitives::gpu_flip_preset::{FACE_NODES, STEP_NODE, WaterScene, water_def};
use manifold_nodes_water::liquid::grid::face_len;
#[cfg(test)]
use manifold_nodes_water::primitives::gpu_flip_volume::volume_and_area;
#[cfg(test)]
use manifold_nodes_water::primitives::liquid_stats::{LIQUID_STATS_WORDS, LiquidTickStats};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::particles::FluidParticle;
#[cfg(test)]
use manifold_nodes_water::fluid_particles::FaceSample;
use manifold_node_engine::testkit::substep_nodes::register_substep_test_nodes;
use manifold_node_engine::{persistence::EffectGraphDefExt, exec::execution_plan::ExecutionPlan, exec::execution::Executor, exec::effect_node::FrameTime, graph::Graph, exec::metal_backend::MetalBackend, persistence::PrimitiveRegistry, state_store::StateStore, exec::execution_plan::compile, load::graph_loader::pre_allocate_resources};

#[cfg(test)]
const G: f64 = 9.81;

/// Command-buffer timestamps across every ordinary chunk, without dispatch
/// profiling or changes to replay. Reused after the preceding frame completes.
struct FrameGpuTime {
    span: std::sync::Arc<[std::sync::atomic::AtomicU64; 2]>,
    tap: std::sync::Arc<dyn Fn(f64, f64) + Send + Sync>,
}

impl FrameGpuTime {
    fn new() -> Self {
        use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
        let span = Arc::new([AtomicU64::new(u64::MAX), AtomicU64::new(0)]);
        let tapped = Arc::clone(&span);
        let tap = Arc::new(move |start: f64, end: f64| {
            if start.is_finite() && end.is_finite() && start > 0.0 && end >= start {
                // Positive finite f64 bit patterns have their numeric ordering.
                tapped[0].fetch_min(start.to_bits(), Ordering::Relaxed);
                tapped[1].fetch_max(end.to_bits(), Ordering::Relaxed);
            }
        });
        Self { span, tap }
    }

    fn reset(&self) {
        use std::sync::atomic::Ordering;
        self.span[0].store(u64::MAX, Ordering::Relaxed);
        self.span[1].store(0, Ordering::Relaxed);
    }

    fn millis(&self) -> f64 {
        use std::sync::atomic::Ordering;
        let start = f64::from_bits(self.span[0].load(Ordering::Relaxed));
        let end = f64::from_bits(self.span[1].load(Ordering::Relaxed));
        (end - start) * 1000.0
    }
}





/// The node whose id ends with `name`: the flattened surface group's nodes
/// carry the group's path before their own id.
fn node_ending(graph: &Graph, name: &str) -> manifold_node_engine::exec::effect_node::NodeInstanceId {
    let mut found = graph.nodes().filter(|n| n.node_id.as_str().ends_with(name));
    let node = found.next().unwrap_or_else(|| panic!("no node ending {name}"));
    assert!(found.next().is_none(), "two nodes end {name}");
    node.id
}

/// A scene's graph compiled and bound once, run frame by frame.
pub struct Run {
    device: manifold_gpu::testkit::TestDevice,
    graph: Graph,
    plan: ExecutionPlan,
    exec: Executor,
    state: StateStore,
    scene: WaterScene,
    frames: i64,
    /// Frames per second of the project clock; every tick is one frame.
    fps: f64,
    /// The particles the frame's tick started from: the state's, read
    /// before the frame runs.
    entering: Vec<FluidParticle>,
    gpu_time: Option<FrameGpuTime>,
    /// Live particles the fill seeded. The engine seeds no site a solid holds,
    /// walls included, so this can be under the fill's slot count.
    pub(super) seeded: usize,
}

impl Run {
    pub fn new(scene: WaterScene) -> Self {
        Self::posed(scene, &[])
    }

    /// `new` with each `(node, param, value)` set before the fill frame, so
    /// the fill and the first tick share that pose. A body posed after the
    /// fill crosses the whole move in its first tick, at the move's speed.
    pub(super) fn posed(scene: WaterScene, params: &[(&str, &str, f64)]) -> Self {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        let mut graph = water_def(scene).into_graph(&registry, &Default::default()).expect("water def builds");
        for &(node, name, value) in params {
            let node = node_named(&graph, node);
            graph.set_param(node, name, manifold_node_engine::parameters::ParamValue::Float(value as f32)).expect(name);
        }
        Self::with_graph(scene, graph)
    }

    fn with_graph(scene: WaterScene, mut graph: Graph) -> Self {
        // Every array read after a frame keeps its own storage.
        let mut read = vec![(node_named(&graph, "state"), "out")];
        let step = node_named(&graph, STEP_NODE);
        read.extend([(step, "out"), (step, "faces"), (step, "capped"), (node_named(&graph, "stats"), "stats_out")]);
        if scene.surface {
            read.push((node_ending(&graph, "liquid_offsets"), "extent"));
            read.push((node_ending(&graph, "liquid_mesh"), "vertices"));
            read.push((node_ending(&graph, "liquid_mesh"), "indices"));
            read.push((node_ending(&graph, "liquid_normals"), "out"));
        }
        if scene.faces {
            read.extend(FACE_NODES.map(|name| (node_named(&graph, name), "out")));
        }
        for &(node, port) in &read {
            graph.add_external_output(node, port).expect("a read port exists");
        }
        let plan = compile(&graph).expect("water def compiles");
        let device = manifold_gpu::testkit::test_device();
        let mut backend = MetalBackend::new(device.arc(), 64, 64, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&mut graph, &plan, &device, &mut backend).expect("pre-allocate");
        let exec = Executor::new(Box::new(backend));
        let mut run = Self { device, graph, plan, exec, state: StateStore::new(), scene, frames: 0, fps: 60.0, entering: Vec::new(), gpu_time: None, seeded: 0 };
        // The domain's clock restarts on its first frame and ticks none: the
        // state takes the fill. Every later frame is one tick.
        run.frame();
        run.seeded = particle_stats(&run.particles()).live;
        run
    }

    /// The rendered surface's live triangles, after smoothing. The shipped
    /// surface shares vertices across triangles; the index buffer supplies
    /// topology, not consecutive triples of unique vertices.
    #[cfg(test)]
    pub(super) fn surface(&self) -> Vec<[[f32; 3]; 3]> {
        // The running total's `extent` starts with the grand total: triangles.
        let extent: Vec<u32> = self.read_at(node_ending(&self.graph, "liquid_offsets"), "extent", 1);
        let mesh = node_ending(&self.graph, "liquid_mesh");
        let count = 3 * extent[0] as usize;
        let indexed = self.graph.wires_into(mesh).any(|wire| wire.to.1 == "edge_scan");
        let indices: Vec<u32> = if indexed {
            self.read_at(mesh, "indices", count)
        } else {
            (0..count as u32).collect()
        };
        let vertex_count = indices.iter().max().map_or(0, |&index| index as usize + 1);
        let vertices: Vec<manifold_node_engine::mesh::MeshVertex> =
            self.read_at(node_ending(&self.graph, "liquid_normals"), "out", vertex_count);
        indices.chunks_exact(3).map(|triangle| [0, 1, 2].map(|i| vertices[triangle[i] as usize].position)).collect()
    }

    /// The volume the surface mesh holds in the tank and its free surface's area.
    #[cfg(test)]
    pub(super) fn surface_measure(&self) -> (f64, f64) {
        volume_and_area(self.surface().into_iter(), self.scene.min(), self.scene.size)
    }

    /// The particles' own volume: `REST_PER_CELL` fill a cell.
    #[cfg(test)]
    pub(super) fn particle_volume(&self) -> f64 {
        self.scene.particles() as f64 * self.scene.cell_size().powi(3) / REST_PER_CELL
    }

    /// One frame in its own command buffer: GPU ms and CPU encode ms.
    pub fn frame(&mut self) -> (f64, f64) {
        self.frame_with_timing(false)
    }

    #[cfg(test)]
    fn timed_frame(&mut self) -> (f64, f64) {
        self.frame_with_timing(true)
    }

    fn frame_with_timing(&mut self, timed: bool) -> (f64, f64) {
        self.entering = self.particles();
        let mut enc = self.device.create_encoder("gpu-flip-scene");
        if timed {
            let timing = self.gpu_time.get_or_insert_with(FrameGpuTime::new);
            timing.reset();
            enc.tap_gpu_time(std::sync::Arc::clone(&timing.tap));
        }
        let cpu_ms;
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            let time = FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(self.frames as f64 / self.fps),
                delta: Seconds(1.0 / self.fps),
                frame_count: self.frames,
            };
            let start = std::time::Instant::now();
            self.exec.execute_frame_with_state(&mut self.graph, &self.plan, time, &mut gpu, &mut self.state, 0);
            cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
            self.frames += 1;
        }
        if timed {
            // Register the tap for the final nonempty chunk; the following
            // blocking completion waits behind every chunk on this queue.
            enc.commit_and_continue(&self.device);
        }
        let profile = enc.commit_and_wait_profiled(&self.device);
        assert_eq!(profile.failed_command_buffers, 0, "frame {} failed on the GPU", self.frames - 1);
        let gpu_ms = if timed { self.gpu_time.as_ref().expect("frame timing enabled").millis() } else { profile.total_ms };
        (gpu_ms, cpu_ms)
    }

    fn read<T: bytemuck::Pod>(&self, node: &str, port: &str, len: usize) -> Vec<T> {
        self.read_at(node_named(&self.graph, node), port, len)
    }

    fn read_at<T: bytemuck::Pod>(&self, node: manifold_node_engine::exec::effect_node::NodeInstanceId, port: &str, len: usize) -> Vec<T> {
        let resource = output_of(&self.plan, node, port);
        let buffer = self
            .exec
            .host_array_buffer(&self.graph, &self.plan, resource)
            .unwrap_or_else(|| panic!("{node:?}.{port} is not declared as a host read in with_graph"));
        assert!(buffer.size as usize >= len * std::mem::size_of::<T>(), "{node:?}.{port} is shorter than {len} records");
        let ptr = buffer.mapped_ptr().expect("shared storage");
        // SAFETY: the frame completed and the buffer holds `len` records.
        unsafe { std::slice::from_raw_parts(ptr.cast::<T>().cast_const(), len) }.to_vec()
    }

    pub fn n(&self) -> usize {
        self.solver_grid().cells()[0] as usize
    }

    fn solver_grid(&self) -> manifold_nodes_water::liquid::lattice::FlipSolverGrid {
        manifold_nodes_water::liquid::lattice::FlipSolverGrid::from_lattice(
            manifold_nodes_water::liquid::lattice::LiquidLattice::from_layout(&self.scene.layout()),
        )
    }

    /// The force hook: the domain's uniform acceleration (Gravity X and Y),
    /// m/s², from the next frame on. A uniform force field and gravity enter
    /// the step identically, at every face.
    #[cfg(test)]
    pub(super) fn set_gravity(&mut self, x: f64, y: f64) {
        let domain = node_named(&self.graph, "domain");
        self.graph.set_param(domain, "gravity_x", manifold_node_engine::parameters::ParamValue::Float(x as f32)).expect("gravity_x");
        self.graph.set_param(domain, "gravity", manifold_node_engine::parameters::ParamValue::Float(y as f32)).expect("gravity");
    }

    /// Switch the executor's encode replay; the fill frame already ran
    /// (it ticks no step), every later frame honours the switch.
    #[cfg(test)]
    pub(super) fn set_encode_replay(&mut self, on: bool) {
        self.exec.set_encode_replay(on);
    }

    #[cfg(test)]
    pub(super) fn replay_stats(&self) -> manifold_gpu::GpuReplayStats {
        self.exec.replay_stats()
    }

    pub(super) fn particles(&self) -> Vec<FluidParticle> {
        self.read("state", "out", self.scene.particles() as usize)
    }

    /// The last tick's water cells, 1 or 0: φ < 0 over the particles it
    /// started from, so a cell whose centre is within √3·h/2 of one (the eps
    /// snap makes an exact touch water). Bodies are not counted. A later
    /// substep's start stays inside the node, so this reads Steps 1 only.
    pub fn water(&self) -> Vec<f32> {
        assert_eq!(self.scene.steps, 1, "a substep's water mask is read at Steps 1");
        self.water_of(&self.entering)
    }

    /// The water cells, 1 or 0, of `particles`: φ < 0 as the step builds it.
    pub(super) fn water_of(&self, particles: &[FluidParticle]) -> Vec<f32> {
        let started = particles;
        let (n, h, min) = (self.n(), self.scene.cell_size(), self.solver_grid().min().map(f64::from));
        let radius = 0.866_025_4 * h;
        let mut water = vec![0.0; n.pow(3)];
        for p in started.iter().filter(|p| p.position_radius[3] > 0.0) {
            let q: [f64; 3] = std::array::from_fn(|a| f64::from(p.position_radius[a]));
            let c: [i64; 3] = std::array::from_fn(|a| ((q[a] - min[a]) / h).floor() as i64);
            for dz in -1..=1 {
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let cell = [c[0] + dx, c[1] + dy, c[2] + dz];
                        if cell.iter().any(|&i| i < 0 || i >= n as i64) {
                            continue;
                        }
                        let d2: f64 = (0..3).map(|a| (min[a] + (cell[a] as f64 + 0.5) * h - q[a]).powi(2)).sum();
                        if d2 <= radius * radius {
                            water[cell[0] as usize + n * (cell[1] as usize + n * cell[2] as usize)] = 1.0;
                        }
                    }
                }
            }
        }
        water
    }

    /// The last tick's node.liquid_stats words.
    #[cfg(test)]
    pub(super) fn liquid_stats(&self) -> LiquidTickStats {
        LiquidTickStats::from_words(&self.read::<u32>("stats", "stats_out", LIQUID_STATS_WORDS as usize))
    }

    /// The last tick's solver words: pressure and density iterations over
    /// every substep, and solves that reached the cap unconverged.
    #[cfg(test)]
    pub(super) fn solver(&self) -> [u32; 3] {
        let n = 2 * self.scene.particles() as usize;
        let words: Vec<u32> = self.read(STEP_NODE, "capped", n + 3);
        [words[n], words[n + 1], words[n + 2]]
    }

    /// The last substep's face grid: projected, constrained to the solids
    /// and extended.
    #[cfg(test)]
    pub(super) fn faces(&self) -> Vec<FaceSample> {
        self.read(STEP_NODE, "faces", (self.n() + 1).pow(3))
    }

    /// The seam face grid of the frame's last tick, x, y and z (a scene built
    /// with `faces`).
    pub fn face_grid(&self) -> [Vec<f32>; 3] {
        let cells = [self.n() as u32; 3];
        std::array::from_fn(|axis| self.read(FACE_NODES[axis], "out", face_len(cells, axis) as usize))
    }

    /// Water cells in the step's lattice: the size of its pressure solve.
    #[cfg(test)]
    pub(super) fn water_cells(&self) -> u32 {
        self.water().iter().filter(|&&w| w > 0.5).count() as u32
    }
}

/// Live particles, how many are not finite, the fastest speed, the mean
/// velocity and the mean height.
pub(super) struct ParticleStats {
    pub live: usize,
    pub bad: usize,
    pub fastest: f64,
    pub mean_velocity: [f64; 3],
    pub mean_height: f64,
}

pub(super) fn particle_stats(particles: &[FluidParticle]) -> ParticleStats {
    let mut stats = ParticleStats { live: 0, bad: 0, fastest: 0.0, mean_velocity: [0.0; 3], mean_height: 0.0 };
    for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
        stats.live += 1;
        if !(p.position_radius.iter().chain(&p.velocity).all(|v| v.is_finite())) {
            stats.bad += 1;
            continue;
        }
        let v = p.velocity.map(f64::from);
        stats.fastest = stats.fastest.max(v.iter().map(|c| c * c).sum::<f64>().sqrt());
        for (sum, component) in stats.mean_velocity.iter_mut().zip(v) {
            *sum += component;
        }
        stats.mean_height += f64::from(p.position_radius[1]);
    }
    let good = (stats.live - stats.bad).max(1) as f64;
    stats.mean_velocity = stats.mean_velocity.map(|v| v / good);
    stats.mean_height /= good;
    stats
}

/// RMS and max |divergence| (1/s) over the water cells of a face grid: what
/// the projection left undone. The density solve's spread moves particles
/// only, so the projected field is asked for zero.
#[cfg(test)]
pub(super) fn divergence(faces: &[FaceSample], water: &[f32], n: usize, h: f64) -> (f64, f64) {
    let m = n + 1;
    let pad = |i: usize, j: usize, k: usize| i + m * (j + m * k);
    let (mut sum, mut max, mut count) = (0.0, 0.0_f64, 0usize);
    for k in 0..n {
        for j in 0..n {
            for i in 0..n {
                let c = i + n * (j + n * k);
                if water[c] <= 0.5 {
                    continue;
                }
                let at = |p: usize, a: usize| f64::from(faces[p].velocity[a]);
                let d = (at(pad(i + 1, j, k), 0) - at(pad(i, j, k), 0) + at(pad(i, j + 1, k), 1) - at(pad(i, j, k), 1)
                    + at(pad(i, j, k + 1), 2)
                    - at(pad(i, j, k), 2))
                    / h;
                sum += d * d;
                max = max.max(d.abs());
                count += 1;
            }
        }
    }
    ((sum / count.max(1) as f64).sqrt(), max)
}

/// Momentum: a block of water in free fall, clear of the walls for the
/// whole run, falls at g. After 0.2 s its mean velocity is −g·t within 1%
/// and no particle is lost.
#[test]
fn gpu_flip_free_fall_keeps_g() {
    let scene = WaterScene::free_fall(64);
    let mut run = Run::new(scene);
    let frames = 12;
    for _ in 0..frames {
        run.frame();
    }
    let stats = particle_stats(&run.particles());
    let t = f64::from(frames) / 60.0;
    let want = -G * t;
    println!(
        "GPU FLIP free fall {}³: {} particles, mean velocity {:?} m/s after {t:.3} s (−g·t = {want:.4}), mean height {:.4} m, water cells {}",
        run.n(),
        stats.live,
        stats.mean_velocity,
        stats.mean_height,
        run.water_cells()
    );
    assert_eq!((stats.live, stats.bad), (scene.particles() as usize, 0), "every particle lives and stays finite");
    assert!((stats.mean_velocity[1] - want).abs() <= 0.01 * want.abs(), "fall speed {} against {want}", stats.mean_velocity[1]);
    assert!(stats.mean_velocity[0].abs().max(stats.mean_velocity[2].abs()) <= 0.01 * want.abs(), "no sideways drift");
}

/// The tick hands the state its last step's extended faces, bit for bit, and
/// the face components gather the seam's arrays from them: at 32 first, then
/// at 64.
#[test]
fn gpu_flip_face_grid_is_the_last_ticks_faces() {
    for n in [32, 64] {
        let scene = WaterScene::dam_break(n).with_faces();
        let mut run = Run::new(scene);
        for _ in 0..12 {
            run.frame();
        }
        let records = (run.n() + 1).pow(3);
        let state: Vec<FaceSample> = run.read("state", "faces", records);
        let last = run.faces();
        let differ = state.iter().zip(&last).filter(|(a, b)| bytemuck::bytes_of(*a) != bytemuck::bytes_of(*b)).count();
        let moving = state.iter().filter(|s| s.velocity.iter().any(|v| *v != 0.0)).count();
        let grid = run.face_grid();
        let expected = manifold_nodes_water::liquid::conformance::gpu_flip_faces(bytemuck::cast_slice(&state), run.solver_grid().cells());
        let gathered = (0..3)
            .map(|axis| grid[axis].iter().zip(&expected[axis]).filter(|(a, b)| a.to_bits() != b.to_bits()).count())
            .sum::<usize>();
        println!("GPU FLIP face grid {n}³: {moving} of {records} face samples moving; state differs at {differ}, gather at {gathered}");
        assert!(moving > records / 100, "{n}³: the faces barely move");
        assert_eq!(differ, 0, "{n}³: the state's faces are not the last step's");
        assert_eq!(gathered, 0, "{n}³: the face components differ from the state's faces");
    }
}

/// The native engine's peak fastest particle in the same 64³ still pool at
/// the 10-frame checkpoints from frame 59 to 119 (a native reference run, now in git history).
/// It never settles: 10 to 19 mm/s throughout.
#[cfg(test)]
const NATIVE_STILL_POOL_FASTEST: f64 = 1.889e-2;

/// The largest |GPU − native| fastest particle at those checkpoints, both
/// run from the native engine's captured seed (a native reference run, now in git history).
#[cfg(test)]
const STILL_POOL_GPU_GAP: f64 = 4.209e-4;

/// The native engine's largest inner vertical face speed in the 64³ still
/// pool at the 30-frame checkpoints from frame 29 to 299, as a share of
/// g·dt, measured with inner_vertical_face_speed's mask
/// (a native reference run from the native engine's seed, now in git history).
#[cfg(test)]
const NATIVE_HYDROSTATIC_FACE_SHARE: f64 = 0.02852;

/// The largest |GPU − native| of that measure at those checkpoints, both
/// run from the native engine's seed, as a share of g·dt: they agree to
/// the printed 1e-5 at every checkpoint.
#[cfg(test)]
const HYDROSTATIC_GPU_GAP_SHARE: f64 = 1e-5;

/// I5: a pool at rest stays at rest as the engine's does. From 1 s on the
/// particle count is the fill's and the fastest particle stays under the
/// engine's own peak in the same pool plus five times the largest gap the
/// GPU showed from the engine's seed: 21.0 mm/s. The margin covers this
/// fill's unjittered seed, which the gap run did not use.
#[test]
fn gpu_flip_still_pool() {
    let scene = WaterScene::still_pool(64);
    let mut run = Run::new(scene);
    // The engine seeds a site only where the solid distance, walls included,
    // is positive: the inset walls hold the pool's four floor-corner sites.
    assert_eq!(run.seeded, scene.particles() as usize - 4, "the walls hold exactly the four floor-corner sites");
    let mut fastest = Vec::new();
    for frame in 0..120 {
        run.frame();
        println!("GPU FLIP still pool frame {frame:3}: dry floor holes {}", run.liquid_stats().dry_floor_cells);
        if frame % 10 == 9 {
            let stats = particle_stats(&run.particles());
            let (rms, max) = divergence(&run.faces(), &run.water(), run.n(), scene.cell_size());
            println!(
                "GPU FLIP still pool {}³ frame {frame:3}: fastest {:.2e} m/s, mean height {:.5} m, divergence rms {rms:.2e} max {max:.2e} /s, water cells {}",
                run.n(),
                stats.fastest,
                stats.mean_height,
                run.water_cells()
            );
            assert_eq!((stats.live, stats.bad), (run.seeded, 0), "frame {frame}: particles lost or not finite");
            if frame >= 59 {
                fastest.push(stats.fastest);
            }
        }
    }
    let peak = fastest.iter().copied().fold(0.0, f64::max);
    let bound = NATIVE_STILL_POOL_FASTEST + 5.0 * STILL_POOL_GPU_GAP;
    assert!(peak < bound, "fastest particle {peak} m/s from 1 s on, past {bound} (native peaks at {NATIVE_STILL_POOL_FASTEST})");
}

/// An open −X face drains the pool: the engine removes every particle within
/// 2 cells of an open face, so the water pours out through it. Over 3 s the
/// live count, read from the particles and from node.liquid_stats, never
/// rises and ends below half the fill, and every particle record stays
/// finite and inside the tank.
#[test]
fn gpu_flip_open_face_drains_the_pool() {
    let scene = WaterScene::still_pool(64).with_closed_faces(63 & !1);
    let mut run = Run::new(scene);
    let (min, size) = (scene.min(), scene.size);
    let fill = scene.particles() as usize;
    let mut last = fill;
    for frame in 0..180 {
        run.frame();
        let particles = run.particles();
        let stats = particle_stats(&particles);
        let counted = run.liquid_stats();
        assert_eq!(stats.bad, 0, "frame {frame}: a non-finite particle");
        assert_eq!(counted.live as usize, stats.live, "frame {frame}: liquid_stats counts {} live, the particles {}", counted.live, stats.live);
        assert!(stats.live <= last, "frame {frame}: live count rose from {last} to {}", stats.live);
        for p in &particles {
            let inside = (0..3).all(|a| {
                let x = f64::from(p.position_radius[a]);
                x >= min[a] && x <= min[a] + size
            });
            assert!(inside, "frame {frame}: a particle at {:?} is outside the tank", p.position_radius);
        }
        if frame % 30 == 29 {
            println!("GPU FLIP open −X frame {frame:3}: {} of {fill} live, mean height {:.4} m", stats.live, stats.mean_height);
        }
        last = stats.live;
    }
    assert!(last < fill / 2, "the open face drained only {} of {fill} particles", fill - last);
}

/// The largest |v| on a vertical face between two water cells at or below
/// `deep`, and how many faces it read: `face_v(i, j, k)` is the face under
/// cell (i, j, k), `water` the step's water mask from its entering
/// particles. Panics on a non-finite face, which a max would skip.
#[cfg(test)]
fn inner_vertical_face_speed(face_v: impl Fn(usize, usize, usize) -> f32, water: &[f32], n: usize, deep: usize) -> (f64, Vec<u32>) {
    let mut worst = 0.0_f64;
    let mut selected = Vec::new();
    for k in 0..n {
        for j in 1..=deep.min(n - 1) {
            for i in 0..n {
                if water[i + n * ((j - 1) + n * k)] > 0.5 && water[i + n * (j + n * k)] > 0.5 {
                    let v = face_v(i, j, k);
                    assert!(v.is_finite(), "inner vertical face ({i}, {j}, {k}) is {v}");
                    worst = worst.max(f64::from(v).abs());
                    selected.push((i + n * (j + n * k)) as u32);
                }
            }
        }
    }
    assert!(!selected.is_empty(), "no inner vertical face under the water");
    (worst, selected)
}

/// Cells wholly under the seeded top, one layer of margin below it.
#[cfg(test)]
fn hydrostatic_depth(scene: WaterScene) -> usize {
    ((scene.fill_height / scene.cell_size()).floor() as usize).saturating_sub(2)
}

/// The closed wall at rest: a 1 m pool for 300 frames. Every box wall face of
/// the projected grid is exactly 0. Inner vertical faces between water
/// cells keep within the native engine's own residual on the same faces,
/// mask and checkpoints, plus five times the largest gap the GPU showed
/// from the engine's seed (a native reference run, now in git history). This is parity
/// with the engine's hydrostatic residual, not a bound on p = ρgh: the
/// engine itself leaves several percent of g·dt there.
#[test]
fn gpu_flip_hydrostatic_column_rests() {
    let scene = WaterScene::still_pool(64);
    let mut run = Run::new(scene);
    let n = run.n();
    let m = n + 1;
    let g_dt = G * scene.step_dt();
    let deep = hydrostatic_depth(scene);
    let bound = NATIVE_HYDROSTATIC_FACE_SHARE + 5.0 * HYDROSTATIC_GPU_GAP_SHARE;
    for frame in 0..300 {
        run.frame();
        if frame % 30 != 29 {
            continue;
        }
        let faces = run.faces();
        let mut wall = 0.0_f64;
        for (idx, face) in faces.iter().enumerate() {
            let p = [idx % m, (idx / m) % m, idx / (m * m)];
            for a in 0..3 {
                if (0..3).all(|b| b == a || p[b] < n) && (p[a] == 0 || p[a] == n) {
                    assert!(face.velocity[a].is_finite(), "frame {frame}: wall face {p:?}/{a} is not finite");
                    wall = wall.max(f64::from(face.velocity[a]).abs());
                }
            }
        }
        let (worst, _) = inner_vertical_face_speed(|i, j, k| faces[i + m * (j + m * k)].velocity[1], &run.water(), n, deep);
        let stats = particle_stats(&run.particles());
        println!(
            "GPU FLIP hydrostatic {n}³ frame {frame:3}: wall faces max |v| {wall:.1e}, inner vertical faces max |v| {worst:.2e} m/s = {:.3}% of g·dt, fastest particle {:.2e} m/s",
            100.0 * worst / g_dt,
            stats.fastest
        );
        assert_eq!(wall, 0.0, "frame {frame}: a wall face moves");
        assert!(worst <= bound * g_dt, "frame {frame}: inner vertical faces at {:.3}% of g·dt, past {:.3}% (native {:.3}%)", 100.0 * worst / g_dt, 100.0 * bound, 100.0 * NATIVE_HYDROSTATIC_FACE_SHARE);
        assert_eq!((stats.live, stats.bad), (run.seeded, 0), "frame {frame}: particles lost or not finite");
    }
}

/// Mechanical energy per unit particle mass summed over the live particles,
/// KE + PE with the floor at `floor` (J/kg).
#[cfg(test)]
pub(super) fn energy(particles: &[FluidParticle], floor: f64) -> f64 {
    particles
        .iter()
        .filter(|p| p.position_radius[3] > 0.0)
        .map(|p| {
            let v2: f64 = p.velocity.iter().map(|&c| f64::from(c).powi(2)).sum();
            0.5 * v2 + G * (f64::from(p.position_radius[1]) - floor)
        })
        .sum()
}

/// The Dam Break never gains energy: an inviscid solver with closed walls
/// can only lose KE + PE (PIC blending, the projection, wall stops), so no
/// frame may sit above the energy it started with past float noise
/// (1e-4 of it). A high run-up that passes this is physics, not a source.
/// 32 cells over the first 1.5 s: a source shows by the first run-up.
#[test]
fn gpu_flip_dam_break_energy_never_rises() {
    for steps in [1, 2] {
        let scene = WaterScene::dam_break(32).with_steps(steps);
        let mut run = Run::new(scene);
        let floor = scene.min()[1];
        let e0 = energy(&run.particles(), floor);
        let mut worst = f64::NEG_INFINITY;
        let (mut most, mut unconverged) = ([0u32; 2], 0u32);
        for frame in 0..90 {
            run.frame();
            let [pressure, density, cap] = run.solver();
            most = [most[0].max(pressure), most[1].max(density)];
            unconverged += cap;
            let e = energy(&run.particles(), floor) / e0;
            worst = worst.max(e - 1.0);
            if frame % 15 == 14 {
                println!("GPU FLIP energy {steps} steps frame {frame:3}: E/E0 {e:.5}");
            }
        }
        println!("GPU FLIP energy {steps} steps: most above E0 {:+.2e}", worst);
        // The splash frames converge on the engine's tolerance, well under
        // the cap, which is the physics claim asserted below.
        println!("GPU FLIP energy {steps} steps: most iterations a tick, pressure {} density {}, unconverged solves {unconverged}", most[0], most[1]);
        assert_eq!(unconverged, 0, "{steps} steps: solves reached the cap");
        // A regression guard, not physics. The measured maximum is 17 at 1
        // step on the 64-cell Dam Break with the obstacle box; the ceiling is about
        // twice that, so a broken preconditioner is caught while the
        // splash-dependent variation is not.
        assert!(most[0] > 0 && most[0] as usize <= 32 * steps, "{steps} steps: pressure iterations a tick {most:?}");
        assert_eq!(most[1], 0, "the default must not run density iterations");
        assert!(worst <= 1e-4, "{steps} steps: energy rose {worst:.2e} of E0 above its start");
    }
}

/// The tank's first standing wave: 4 m long, 1 m deep, so wavelength 8 m and
/// linear theory's period 2π/√(gk·tanh kd) = 2.795 s at k = π/4. The water's
/// centre of mass in x swings with it (the step's third harmonic weighs 1/27
/// in that measure); its zero crossings over 3 s to 9 s give the period, which
/// must sit within 3% of 2.80 s. Water too narrow in the pressure solve lags
/// the swing; too wide, it leads.
#[test]
fn gpu_flip_standing_wave_keeps_its_period() {
    let scene = WaterScene::slosh(64);
    let mut run = Run::new(scene);
    let mut centre = Vec::new();
    for frame in 0..600 {
        run.frame();
        let live: Vec<f64> =
            run.particles().iter().filter(|p| p.position_radius[3] > 0.0).map(|p| f64::from(p.position_radius[0])).collect();
        centre.push(live.iter().sum::<f64>() / live.len() as f64);
        if frame % 60 == 59 {
            let stats = particle_stats(&run.particles());
            println!("GPU FLIP slosh frame {frame:3}: centre x {:+.4} m, fastest {:.3} m/s", centre[frame], stats.fastest);
            assert_eq!((stats.live, stats.bad), (run.seeded, 0), "frame {frame}: particles lost or not finite");
        }
    }
    // Crossings of the rest centre, x = 0, interpolated between frames.
    let crossings: Vec<f64> = centre
        .windows(2)
        .enumerate()
        .filter(|(_, w)| (w[0] < 0.0) != (w[1] < 0.0))
        .map(|(i, w)| (i as f64 + w[0] / (w[0] - w[1])) / 60.0)
        .filter(|t| (3.0..9.0).contains(t))
        .collect();
    println!("GPU FLIP slosh crossings (s): {crossings:.3?}");
    assert!(crossings.len() >= 3, "only {} crossings between 3 s and 9 s", crossings.len());
    let period = 2.0 * (crossings[crossings.len() - 1] - crossings[0]) / (crossings.len() - 1) as f64;
    println!("GPU FLIP slosh period {period:.3} s, {:+.2}% off 2.80 s", 100.0 * (period / 2.80 - 1.0));
    assert!((period / 2.80 - 1.0).abs() <= 0.03, "standing-wave period {period:.3} s, outside 3% of 2.80 s");
}

/// How deep `p` sits inside the obstacle box at `pos` (m), negative outside.
#[cfg(test)]
fn obstacle_depth(p: &FluidParticle, pos: [f64; 3]) -> f64 {
    let scale = DAM_OBSTACLE[1];
    (0..3).map(|a| 0.5 * scale[a] - (f64::from(p.position_radius[a]) - pos[a]).abs()).fold(f64::INFINITY, f64::min)
}

/// The deepest live particle inside the obstacle at `pos`, and the live count.
#[cfg(test)]
fn deepest_in_obstacle(particles: &[FluidParticle], pos: [f64; 3]) -> (f64, usize) {
    let live = particles.iter().filter(|p| p.position_radius[3] > 0.0);
    live.fold((f64::NEG_INFINITY, 0), |(deepest, count), p| (deepest.max(obstacle_depth(p, pos)), count + 1))
}

/// The Dam Break's box as a Collider: the fill leaves the box's sites dead,
/// the collapsing column flows round the box and over it, no live particle
/// gets more than half a cell inside it, nothing outruns the column without
/// the box by much, and the step removes under 0.5% of the water.
#[test]
fn gpu_flip_dam_break_flows_around_the_obstacle() {
    let scene = WaterScene::dam_break(64).with_obstacle();
    let h = scene.cell_size();
    let mut run = Run::new(scene);
    let pos = DAM_OBSTACLE[0];
    let filled = run.particles();
    let (deepest, live) = deepest_in_obstacle(&filled, pos);
    let dead_inside = filled.iter().filter(|p| p.position_radius[3] == 0.0 && obstacle_depth(p, pos) > 0.0).count();
    let missed = filled.iter().filter(|p| p.position_radius[3] > 0.0 && obstacle_depth(p, pos) > h).count();
    println!("GPU FLIP obstacle fill: {live} live of {}, {dead_inside} dead inside the box, deepest live {:.3} cells", filled.len(), deepest / h);
    assert!(dead_inside > 1000, "the fill left the box's sites alive");
    assert_eq!(missed, 0, "a site a cell inside the box is alive");
    let mut beside = 0;
    for frame in 0..90 {
        run.frame();
        let particles = run.particles();
        let stats = particle_stats(&particles);
        let (deepest, _) = deepest_in_obstacle(&particles, pos);
        // Water above the pool beside the box, in the box's x span.
        let [lo, hi] = [pos[0] - 0.5 * DAM_OBSTACLE[1][0], pos[0] + 0.5 * DAM_OBSTACLE[1][0]];
        let half_z = 0.5 * DAM_OBSTACLE[1][2];
        beside = beside.max(
            particles
                .iter()
                .filter(|p| p.position_radius[3] > 0.0)
                .filter(|p| {
                    let [x, y, z] = [0, 1, 2].map(|a| f64::from(p.position_radius[a]));
                    (lo..hi).contains(&x) && (z - pos[2]).abs() > half_z && y > DAM_FILL_HEIGHT + 2.0 * h
                })
                .count(),
        );
        if frame % 10 == 9 {
            println!(
                "GPU FLIP obstacle frame {frame:3}: {} live, fastest {:.2} m/s, deepest in the box {:.3} cells, {beside} beside it",
                stats.live,
                stats.fastest,
                deepest / h
            );
        }
        assert_eq!(stats.bad, 0, "frame {frame}: a particle is not finite");
        // The column without the box peaks near 13 m/s at 64³.
        assert!(stats.fastest < 25.0, "frame {frame}: fastest particle {} m/s", stats.fastest);
        assert!(deepest <= 0.5 * h, "frame {frame}: a live particle sits {:.3} cells inside the box", deepest / h);
        assert!(stats.live as f64 >= 0.995 * live as f64, "frame {frame}: {} of {live} particles left", stats.live);
    }
    assert!(beside > 1000, "the wave never passed beside the box");
}

/// A pool at rest round a static box resting on the tank floor rests as the
/// FLIP engine's does: no particle is lost, the level holds, and the water
/// moves no faster than the engine's. On the engine's grid the waterline
/// falls mid-cell, where both keep a few cm/s of lapping at every solid
/// (`gpu_flip_engine_still_pool_round_a_box`, which printed `ENGINE`).
#[test]
fn gpu_flip_still_pool_rests_round_a_static_obstacle() {
    // The engine's fastest particle at frames 19, 39 … 119 (GPU FLIP within
    // 7% of each on 2026-10-05).
    const ENGINE: [f64; 6] = [0.0438, 0.0129, 0.0183, 0.0241, 0.0221, 0.0190];
    let scene = WaterScene::still_pool(64).with_obstacle();
    let mut run = Run::new(scene);
    let (_, live) = deepest_in_obstacle(&run.particles(), DAM_OBSTACLE[0]);
    // The level once the fill has settled.
    let mut level = None;
    let mut fastest = Vec::new();
    for frame in 0..120 {
        run.frame();
        if frame % 20 == 19 {
            let stats = particle_stats(&run.particles());
            println!("GPU FLIP pool round a box frame {frame:3}: {} live, fastest {:.2e} m/s, mean height {:.5} m", stats.live, stats.fastest, stats.mean_height);
            assert_eq!((stats.live, stats.bad), (live, 0), "frame {frame}: particles lost or not finite");
            let level = *level.get_or_insert(stats.mean_height);
            assert!((stats.mean_height - level).abs() < 1e-4, "frame {frame}: the level moved from {level} to {}", stats.mean_height);
            fastest.push(stats.fastest);
        }
    }
    let peak = fastest.iter().copied().fold(0.0, f64::max);
    let engine_peak = ENGINE.iter().copied().fold(0.0, f64::max);
    assert!(peak < 1.25 * engine_peak, "the pool round the box reached {peak} m/s, the engine's {engine_peak} m/s");
    let last = fastest[ENGINE.len() - 1];
    assert!(last < 1.25 * ENGINE[5], "after 2 s the pool round the box moves at {last} m/s, the engine's at {} m/s", ENGINE[5]);
}

/// A box driven through a still pool at 1 m/s pushes the water ahead of it:
/// the water in front moves with it, none gets more than a cell inside, and
/// the step removes under 0.5% of the water, at one substep a tick and two.
#[test]
fn gpu_flip_moving_obstacle_pushes_the_pool() {
    moving_obstacle_pushes(1);
    moving_obstacle_pushes(2);
}

#[cfg(test)]
fn moving_obstacle_pushes(steps: usize) {
    let scene = WaterScene::still_pool(64).with_obstacle().with_steps(steps);
    let h = scene.cell_size();
    let mut run = Run::new(scene);
    let transform = node_named(&run.graph, "obstacle_transform");
    let start = DAM_OBSTACLE[0];
    let (_, live) = deepest_in_obstacle(&run.particles(), start);
    let speed = 1.0;
    let mut ahead_speed = 0.0;
    for frame in 1..=30 {
        let pos = [start[0] + speed * f64::from(frame) / 60.0, start[1], start[2]];
        run.graph.set_param(transform, "pos_x", manifold_node_engine::parameters::ParamValue::Float(pos[0] as f32)).expect("pos_x");
        run.frame();
        let particles = run.particles();
        let stats = particle_stats(&particles);
        let (deepest, _) = deepest_in_obstacle(&particles, pos);
        let front = pos[0] + 0.5 * DAM_OBSTACLE[1][0];
        let half = DAM_OBSTACLE[1].map(|s| 0.5 * s);
        let ahead: Vec<f64> = particles
            .iter()
            .filter(|p| p.position_radius[3] > 0.0)
            .filter(|p| {
                let [x, _, z] = [0, 1, 2].map(|a| f64::from(p.position_radius[a]));
                (front..front + 2.0 * h).contains(&x) && (z - pos[2]).abs() < half[2] - h
            })
            .map(|p| f64::from(p.velocity[0]))
            .collect();
        ahead_speed = ahead.iter().sum::<f64>() / ahead.len().max(1) as f64;
        if frame % 5 == 0 {
            println!(
                "GPU FLIP moving box {steps} steps frame {frame:2}: {} live, deepest {:.3} cells, {} ahead at {ahead_speed:.3} m/s",
                stats.live,
                deepest / h,
                ahead.len()
            );
        }
        assert_eq!(stats.bad, 0, "frame {frame}: a particle is not finite");
        assert!(deepest <= h, "frame {frame}: a live particle sits {:.3} cells inside the box", deepest / h);
        assert!(stats.live as f64 >= 0.995 * live as f64, "frame {frame}: {} of {live} particles left", stats.live);
    }
    assert!(ahead_speed > 0.5 * speed, "{steps} steps: the water ahead moves at {ahead_speed} m/s against the box's {speed}");
}

/// Small native/GPU comparisons share seed geometry and simulation time.
#[cfg(test)]
mod native_reference {
    #[cfg(test)]
    use crate::contracts::water::primitives::gpu_flip_scene_tests::*;
    #[cfg(test)]
    use manifold_fluids::{Bounds, CaptureError, Config, FluidWorld, ParticleRecord};

    #[cfg(test)]
    fn world(scene: WaterScene) -> (FluidWorld, [f32; 3]) {
        assert!(!scene.obstacle && !scene.surface);
        let layout = scene.layout();
        // Native mapping: 1.5 solid cells outside each authored wall.
        let offset = layout.min.map(|v| v - (1.5 * layout.cell_size) as f32);
        let local = |p: [f32; 3]| std::array::from_fn(|a| p[a] - offset[a]);
        let mut world = FluidWorld::new_seeded(Config {
            cells: layout.cells.map(|n| n + 3), cell_size: layout.cell_size,
            surface_subdivisions: 0, apic: false,
        }, 0).expect("native world");
        world.set_surface_reconstruction_enabled(false).expect("disable meshing");
        world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
        if scene.fill_height > 0.0 {
            let min = local(layout.min);
            world.add_fluid_box(Bounds { min, max: [
                min[0] + layout.size[0], min[1] + scene.fill_height as f32, min[2] + layout.size[2],
            ] }, [0.0; 3]).expect("pool");
        }
        if scene.initial_volume().is_some() {
            world.add_fluid_box(Bounds {
                min: local(scene.column.map(|p| p[0] as f32)),
                max: local(scene.column.map(|p| p[1] as f32)),
            }, [0.0; 3]).expect("column");
        }
        // Native inserts queued fluid at the end of its first step; GPU Run
        // publishes its fill without stepping. Both now start at rest at t=0.
        world.step(Seconds(1.0 / 60.0)).expect("insert native seed");
        (world, offset)
    }

    #[cfg(test)]
    fn snapshot(world: &mut FluidWorld, offset: [f32; 3]) -> (Vec<FluidParticle>, Vec<f32>, [u32; 3]) {
        let (mut records, mut solid) = (Vec::new(), Vec::new());
        let info = loop {
            match world.capture_particle_frame(offset, &mut records, &mut solid) {
                Ok(info) => break info,
                Err(CaptureError::Capacity { particles, solid: nodes }) => {
                    records.resize(particles as usize, ParticleRecord::default());
                    solid.resize(nodes, 0.0);
                }
                Err(CaptureError::Fluid(error)) => panic!("native capture: {error}"),
            }
        };
        let particles = records[..info.count as usize].iter().map(|p| FluidParticle {
            position_radius: p.position_radius, velocity: p.velocity, id: p.id,
        }).collect();
        (particles, solid, info.solid_nodes)
    }

    #[cfg(test)]
    fn seed_site(scene: WaterScene, p: &FluidParticle) -> [u32; 3] {
        assert_eq!(p.velocity, [0.0; 3], "seed must be at rest");
        std::array::from_fn(|a| {
            let site = 2.0 * (f64::from(p.position_radius[a]) - scene.min()[a]) / scene.cell_size() - 0.5;
            // Native uses .25 * (jitter_factor - .001) * h, even with
            // jitter_factor=0. Its interior positions differ by <= .00025h.
            assert!((site - site.round()).abs() < 0.00051, "seed off half-cell lattice: {site}");
            assert!(site.round() >= 0.0);
            site.round() as u32
        })
    }

    #[cfg(test)]
    fn seed_sites(scene: WaterScene, particles: &[FluidParticle]) -> Vec<[u32; 3]> {
        let mut sites: Vec<_> = particles.iter().filter(|p| p.position_radius[3] > 0.0).map(|p| seed_site(scene, p)).collect();
        sites.sort_unstable();
        sites
    }

    #[test]
    fn native_seed_matches_authored_sites_outside_native_solid() {
        for n in [8, 16] {
            let scene = WaterScene::race_dam_break(n);
            let (mut native, offset) = world(scene);
            let (particles, solid, nodes) = snapshot(&mut native, offset);
            let got = seed_sites(scene, &particles);
            let geometry = scene.geometry();
            let column = geometry.setup_for_test().box_sites_for_test();
            let mut expected = Vec::new();
            for x in 0..2 * n as u32 {
                for y in 0..2 * n as u32 {
                    for z in 0..2 * n as u32 {
                        let site = [x, y, z];
                        if y < geometry.setup_for_test().pool_sites_for_test() || (0..3).all(|a| (column[a][0]..column[a][1]).contains(&site[a])) {
                            expected.push(site);
                        }
                    }
                }
            }
            expected.sort_unstable();
            let authored = expected.len();
            expected.retain(|site| {
                // Native seed rejection reads its trilinear solid SDF. The
                // half-cell phase makes corner rejection differ from GPU fill.
                let q = site.map(|s| 1.5 + 0.25 + 0.5 * s as f32);
                let base = q.map(|v| v.floor() as u32);
                let t: [f32; 3] = std::array::from_fn(|a| q[a] - base[a] as f32);
                let mut phi = 0.0;
                for corner in 0..8 {
                    let c: [u32; 3] = std::array::from_fn(|a| base[a] + ((corner >> a) & 1));
                    let weight: f32 = (0..3).map(|a| if corner & (1 << a) == 0 { 1.0 - t[a] } else { t[a] }).product();
                    phi += weight * solid[(c[0] + nodes[0] * (c[1] + nodes[1] * c[2])) as usize];
                }
                phi > 0.0
            });
            println!("{n}³ native={} authored={authored} excluded by native solid={}", got.len(), authored - expected.len());
            assert_eq!(got, expected, "{n}³ native sites after native solid rejection");
        }
    }

    /// A pool against the tank walls with no body: the step's liquid φ obeys
    /// the engine's ParticleLevelSet::postProcessSignedDistanceField against
    /// the engine's own solid. A cell whose centre is in the wall and whose φ
    /// is under h/2 is water at −h/2. Without it the wall's cut cells are a
    /// free surface and water climbs the wall (BUG-9p3ms (tank walls give far
    /// more run-up than native)).
    #[test]
    fn gpu_flip_wall_only_phi_extends_into_the_walls_as_native() {
        let scene = WaterScene::still_pool(16);
        let (mut native, offset) = world(scene);
        let (_, solid, nodes) = snapshot(&mut native, offset);
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        let mut graph = water_def(scene).into_graph(&registry, &Default::default()).expect("water graph");
        graph.add_external_output(node_named(&graph, STEP_NODE), "distance").expect("distance in the plan");
        let mut run = Run::with_graph(scene, graph);
        let step = node_named(&run.graph, STEP_NODE);
        run.exec.set_dump_array_set(Some([step].into_iter().collect()));
        run.frame();
        let n = run.n();
        assert_eq!(nodes, [n as u32 + 1; 3], "native solid on the GPU solver grid");
        let h = scene.cell_size() as f32;
        let res = run.exec.dump_array_resources().iter()
            .find(|&&(node, port, _)| node == step && port == "distance").unwrap_or_else(|| panic!("step distance not dumped: {:?}",
                run.exec.dump_array_resources().iter().filter(|r| r.0 == step).map(|r| r.1).collect::<Vec<_>>())).2;
        let buffer = run.exec.dump_array_buffer(res).expect("distance buffer");
        let bytes = 4 * n * n * n;
        let staging = run.device.create_buffer_shared(bytes as u64);
        let mut enc = run.device.create_encoder("wall phi readback");
        enc.copy_buffer_to_buffer(buffer, &staging, bytes as u64);
        enc.commit_and_wait_completed();
        // SAFETY: the copy completed; staging is shared and `bytes` long.
        let phi: Vec<f32> = unsafe {
            std::slice::from_raw_parts(staging.mapped_ptr().expect("shared staging").cast::<f32>().cast_const(), n * n * n)
        }.to_vec();
        let (mut extended, mut wrong) = (0, Vec::new());
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let centre = (0..8).map(|c| {
                        let p = [i + (c & 1), j + ((c >> 1) & 1), k + ((c >> 2) & 1)];
                        solid[p[0] + (n + 1) * (p[1] + (n + 1) * p[2])]
                    }).sum::<f32>() / 8.0;
                    let v = phi[i + n * (j + n * k)];
                    if centre < 0.0 && v < 0.5 * h {
                        if (v + 0.5 * h).abs() < 1e-5 { extended += 1 } else { wrong.push(([i, j, k], v / h)) }
                    }
                }
            }
        }
        println!("wall cells taken as water at -h/2: {extended}, left otherwise: {}", wrong.len());
        assert!(extended > 0, "the pool must touch the wall's cells");
        assert!(wrong.is_empty(), "wall cells under h/2 not at -h/2 (cell, phi/h): {:?}", &wrong[..wrong.len().min(8)]);
    }

}

/// The measurement must follow the rendered topology and smoothing, in both
/// supported mesh layouts. One fill frame suffices; no simulation or render.
#[test]
fn gpu_flip_surface_readback_matches_triangle_list() {
    let scene = WaterScene::still_pool(8).with_surface();
    let indexed = Run::new(scene);
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    let mut graph = water_def(scene).into_graph(&registry, &Default::default()).expect("water graph");
    for name in ["liquid_mesh", "liquid_smooth_mesh", "liquid_normals"] {
        let node = node_ending(&graph, name);
        assert!(graph.disconnect((node, "edge_scan")).is_some());
    }
    let plain = Run::with_graph(scene, graph);
    let (a, b) = (indexed.surface(), plain.surface());
    assert!(!a.is_empty(), "seeded pool must have a surface");
    assert_eq!(a.len(), b.len());
    for (triangle, (a, b)) in a.iter().zip(&b).enumerate() {
        for (a, b) in a.iter().flatten().zip(b.iter().flatten()) {
            assert!(a.is_finite() && b.is_finite());
            assert!((a - b).abs() < 1e-5, "triangle {triangle}: indexed {a}, triangle-list {b}");
        }
    }
    let (volume, area) = indexed.surface_measure();
    let (plain_volume, plain_area) = plain.surface_measure();
    assert!(volume > 0.0 && area > 0.0);
    assert!((volume - plain_volume).abs() < 1e-5);
    assert!((area - plain_area).abs() < 1e-5);
    let raw: Vec<manifold_node_engine::mesh::MeshVertex> =
        plain.read_at(node_ending(&plain.graph, "liquid_mesh"), "vertices", 3 * b.len());
    assert!(raw.iter().zip(b.iter().flatten()).any(|(raw, smoothed)| raw.position != *smoothed),
        "fixture must distinguish raw and smoothed positions");
}

/// The volume oracle on water that must not change: a resting pool's meshed
/// volume holds within 0.5% for 2 s. A drift here is the measure, not the
/// solver. It also prints the surface's skin, the depth the mesh sits
/// outside the water, for the Dam Break's frame-0 skin to agree with.
#[test]
fn gpu_flip_still_pool_keeps_its_meshed_volume() {
    let scene = WaterScene::still_pool(64).with_surface();
    let mut run = Run::new(scene);
    let mut measures = Vec::new();
    for _ in 0..120 {
        run.frame();
        measures.push(run.surface_measure());
    }
    let (v0, a0) = measures[0];
    let drift = measures.iter().map(|(v, _)| (v / v0 - 1.0).abs()).fold(0.0, f64::max);
    let skin = VolumeDrift::new(measures[0], run.particle_volume()).skin();
    println!("GPU FLIP still pool meshed: frame 0 {v0:.4} m³ over {a0:.3} m², last {:.4} m³, drift max {:.3}%", measures[119].0, 100.0 * drift);
    println!("GPU FLIP still pool meshed: particles hold {:.4} m³, skin {:.2} mm", run.particle_volume(), 1000.0 * skin);
    assert!(drift < 5e-3, "a resting pool's meshed volume moved {:.3}%", 100.0 * drift);
}

/// A lid as wide as the tank pressed down into a 1 m pool at 1 m/s. It is
/// posed before the fill, its bottom two cells under the surface, so the
/// first tick moves it one frame's travel. No live particle ever leaves the
/// box. Closed, the water under it is sealed off from air: every solve
/// converges until the lid first kills water, and from then on a solve may
/// reach its cap, which the stats report while the tick runs on. With the
/// low-x face open the water escapes through it.
#[test]
fn gpu_flip_box_pressed_into_a_full_tank_converges_and_escapes_when_open() {
    let closed = lid_pressed_into_pool(63);
    let open = lid_pressed_into_pool(63 & !1);
    println!("GPU FLIP pressed lid: water by the low-x face moves out at {:.3} m/s sealed, {:.3} m/s open; displacement rate over the open face {:.3} m/s", closed.0, open.0, open.1);
    // Mass conservation: open, the water the lid displaces leaves through
    // the open face, so the mean outflow there is the lid area times its
    // speed over the open area. Sampled two to four cells in, the profile is
    // not flat, so half of it is the floor. Sealed, the water has no way
    // out, so by the wall it moves at under a quarter of the open flow.
    assert!(open.0 > 0.5 * open.1, "open: the pressed water leaves at {:.3} m/s, under half the displacement rate {:.3}", open.0, open.1);
    assert!(closed.0.abs() < 0.25 * open.0, "sealed: the water by the wall moves out at {:.3} m/s against {:.3} open", closed.0, open.0);
}

#[cfg(test)]
const LID_SPEED: f64 = 1.0;
#[cfg(test)]
const LID_THICKNESS: f64 = 0.4;

/// The mean -x speed of the water two to four cells in from the low-x face
/// over the last ten frames, and the speed the lid's displacement would
/// drive through that face: lid area times lid speed over the open area;
/// and the volume rate taken off sealed pockets' pressure solves, summed.
#[cfg(test)]
fn lid_pressed_into_pool(mask: u32) -> (f64, f64, f64) {
    let scene = WaterScene::still_pool(64).with_obstacle().with_closed_faces(mask);
    let h = scene.cell_size();
    let side = scene.size;
    let surface = 1.0;
    let start = surface + 0.5 * LID_THICKNESS - 2.0 * h;
    // Overlapping the walls by a cell, so no gap runs between lid and wall.
    let width = side + 2.0 * h;
    let lid = [("pos_x", 0.0), ("pos_y", start), ("pos_z", 0.0), ("scale_x", width), ("scale_y", LID_THICKNESS), ("scale_z", width)];
    let mut run = Run::posed(scene, &lid.map(|(name, value)| ("obstacle_transform", name, value)));
    let transform = node_named(&run.graph, "obstacle_transform");
    let low_x = -0.5 * side;
    let mut outward = Vec::new();
    let mut drive = 0.0;
    let mut removed = 0.0;
    let mut squeezed = false;
    for frame in 1..=30 {
        let y = start - LID_SPEED * f64::from(frame) / 60.0;
        run.graph.set_param(transform, "pos_y", manifold_node_engine::parameters::ParamValue::Float(y as f32)).expect("pos_y");
        run.frame();
        let stats = run.liquid_stats();
        println!(
            "GPU FLIP pressed lid mask {mask} frame {frame:2}: dry floor holes {}, {} live, {} pressure iterations, {} density, {} unconverged, {} unresolved, {:.3e}/{:.3e} m³/s removed from sealed pressure/density, dry/sealed/air cells {:?}, first air seed {:?} (dry neighbour phi/h {:.3}, particles {})",
            stats.dry_floor_cells, stats.live, stats.pressure_iterations, stats.density_iterations, stats.unconverged, stats.unresolved_pockets, stats.pressure_flux_removed, stats.density_flux_removed, stats.pocket_cells, stats.first_air_seed, f32::from_bits(stats.first_air_seed[4]) as f64 / h, stats.first_air_seed[5]
        );
        removed += f64::from(stats.pressure_flux_removed);
        let half = (0.5 * side) as f32;
        let outside = run
            .particles()
            .iter()
            .filter(|p| p.position_radius[3] > 0.0)
            .filter(|p| {
                let q = p.position_radius;
                q[0].abs() > half || q[2].abs() > half || q[1] < 0.0 || q[1] > side as f32
            })
            .count();
        assert_eq!(outside, 0, "mask {mask}: frame {frame} left live water outside the box");
        let entering = run.entering.iter().filter(|p| p.position_radius[3] > 0.0).count() as u32;
        squeezed |= stats.live != entering;
        if squeezed {
            // Water the lid has squeezed into itself is killed there; past
            // that a capped solve is reported and the tick still runs.
            assert!(stats.live > 0, "mask {mask}: frame {frame} left no live water");
            assert!(
                stats.pressure_flux_removed.is_finite() && stats.density_flux_removed.is_finite(),
                "mask {mask}: frame {frame} produced non-finite stats"
            );
        } else {
            assert_eq!(stats.unconverged, 0, "mask {mask}: frame {frame} left a pressure or density solve unconverged before any water was squeezed");
        }
        assert_eq!(stats.unresolved_pockets, 0, "mask {mask}: frame {frame} left a pocket spread unfinished");
        if frame > 20 {
            let under = y - 0.5 * LID_THICKNESS;
            drive = LID_SPEED * side * side / (side * under);
            let near: Vec<f64> = run
                .particles()
                .iter()
                .filter(|p| p.position_radius[3] > 0.0)
                .filter(|p| (2.0 * h..4.0 * h).contains(&(f64::from(p.position_radius[0]) - low_x)))
                .map(|p| -f64::from(p.velocity[0]))
                .collect();
            outward.push(near.iter().sum::<f64>() / near.len().max(1) as f64);
        }
    }
    (outward.iter().sum::<f64>() / outward.len() as f64, drive, removed)
}

/// Encode replay changes nothing the step computes: 60 Dam Break ticks
/// with the executor's replay on match replay off bit for bit, particles,
/// faces and the solver's words, every tick, while the solver's rounds run
/// as replayed segments and none directly.
#[test]
fn gpu_flip_replay_changes_nothing() {
    let scene = WaterScene::dam_break(64).with_obstacle().with_steps(1);
    let mut direct = Run::new(scene);
    direct.set_encode_replay(false);
    let mut replay = Run::new(scene);
    // The ring's three entries each take two visits to hold the whole
    // step; from the tenth tick every frame replays all of it.
    const WARM: u32 = 10;
    let mut last = replay.replay_stats();
    for frame in 1..=60 {
        direct.frame();
        replay.frame();
        let (dp, rp) = (direct.particles(), replay.particles());
        assert!(bytemuck::cast_slice::<_, u8>(&dp) == bytemuck::cast_slice::<_, u8>(&rp), "tick {frame}: the particles differ with replay on");
        let (df, rf) = (direct.faces(), replay.faces());
        assert!(bytemuck::cast_slice::<_, u8>(&df) == bytemuck::cast_slice::<_, u8>(&rf), "tick {frame}: the faces differ with replay on");
        assert_eq!(direct.solver(), replay.solver(), "tick {frame}: the solver words differ with replay on");
        let stats = replay.replay_stats();
        let delta = |take: fn(&manifold_gpu::GpuReplayStats) -> u64| take(&stats) - take(&last);
        if frame <= WARM || frame % 20 == 0 || delta(|s| s.recorded) > 0 {
            let clock: Vec<u32> = replay.read(STEP_NODE, "clock_status", 8);
            println!(
                "tick {frame}: clock {clock:?}, live {}; solver words {:?}; recorded {} replayed {} direct {} executes {} segments replayed {} direct {} allocations {}",
                particle_stats(&rp).live,
                replay.solver(),
                delta(|s| s.recorded),
                delta(|s| s.replayed),
                delta(|s| s.direct),
                delta(|s| s.executes),
                delta(|s| s.segments_replayed),
                delta(|s| s.segments_direct),
                delta(|s| s.store_allocations)
            );
        }
        if frame > WARM {
            assert_eq!(delta(|s| s.recorded), 0, "tick {frame}: a warm tick records nothing");
            assert_eq!(delta(|s| s.segments_direct), 0, "tick {frame}: no round runs directly");
            assert_eq!(delta(|s| s.store_allocations), 0, "tick {frame}: a warm ring allocates nothing");
            assert!(delta(|s| s.segments_replayed) >= 2, "tick {frame}: the solves' rounds run as segments");
        }
        last = stats;
    }
    assert_eq!(direct.replay_stats().replayed, 0, "the direct run replayed nothing");
}

#[cfg(test)]
fn run_with_retired_speed(scene: WaterScene, enabled: bool) -> Run {
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    let mut graph = water_def(scene).into_graph(&registry, &Default::default()).expect("slot-cost water graph");
    let step = node_named(&graph, STEP_NODE);
    if !enabled {
        // An unwired custom graph retains the original adaptive loop.
        graph.disconnect((step, "retired_max_speed")).expect("retired speed wire");
    }
    graph.add_external_output(step, "clock_status").expect("clock status read");
    Run::with_graph(scene, graph)
}

/// The cost of recording inactive slots, with completed time and all final
/// state proved identical. Readbacks happen after `frame`'s timing ends.
#[test]
fn gpu_flip_one_active_slot_matches_six_recorded_slots() {
    const WARMUP: usize = 8;
    const MEASURED: usize = 12;
    fn median(values: &mut [f64]) -> f64 {
        values.sort_by(f64::total_cmp);
        (values[values.len() / 2 - 1] + values[values.len() / 2]) * 0.5
    }
    fn completed_one_step(run: &Run, n: usize, frame: usize, slots: u32) {
        let status: Vec<u32> = run.read(STEP_NODE, "clock_status", 8);
        let last_dt = if slots == 1 { 1.0f32 / 60.0 } else { 0.0 };
        assert_eq!(status[0], last_dt.to_bits(),
            "{n}³ tick {frame}, {slots} recorded slots: final slot proves the loop override took effect");
        assert_eq!(status[1], (1.0f32 / 60.0).to_bits(),
            "{n}³ tick {frame}, {slots} recorded slots: exact completed time");
        assert_eq!(status[2], 0.0f32.to_bits(),
            "{n}³ tick {frame}, {slots} recorded slots: interval is complete");
        assert_eq!(status[6], 1,
            "{n}³ tick {frame}, {slots} recorded slots: fixture must accept exactly one step");
        assert_eq!((status[4], status[5]), (0, 0),
            "{n}³ tick {frame}, {slots} recorded slots: no cap or nonfinite clock input");
    }
    for n in [32, 64] {
        let scene = WaterScene::still_pool(n).with_steps(1);
        let mut one = run_with_retired_speed(scene, true);
        let mut six = run_with_retired_speed(scene, false);
        one.set_encode_replay(true);
        six.set_encode_replay(true);
        let mut one_gpu = Vec::with_capacity(MEASURED);
        let mut one_cpu = Vec::with_capacity(MEASURED);
        let mut six_gpu = Vec::with_capacity(MEASURED);
        let mut six_cpu = Vec::with_capacity(MEASURED);
        let mut warm_replay = [manifold_gpu::GpuReplayStats::default(); 2];
        for frame in 0..WARMUP + MEASURED {
            // Alternate which identical run gets the first GPU submission.
            let (one_time, six_time) = if frame % 2 == 0 {
                (one.timed_frame(), six.timed_frame())
            } else {
                let six_time = six.timed_frame();
                (one.timed_frame(), six_time)
            };
            for (label, gpu_ms) in [("one", one_time.0), ("six", six_time.0)] {
                assert!(gpu_ms.is_finite() && gpu_ms > 0.0,
                    "{n}³ tick {frame}, {label}: GPU chunk span must be positive and finite, got {gpu_ms}");
            }
            if frame >= WARMUP {
                one_gpu.push(one_time.0);
                one_cpu.push(one_time.1);
                six_gpu.push(six_time.0);
                six_cpu.push(six_time.1);
            }
            completed_one_step(&one, n, frame, if frame == 0 { 6 } else { 1 });
            completed_one_step(&six, n, frame, 6);
            let (op, sp) = (one.particles(), six.particles());
            for (label, particles) in [("one", &op), ("six", &sp)] {
                let stats = particle_stats(particles);
                assert_eq!((stats.live, stats.bad), (one.seeded, 0),
                    "{n}³ tick {frame}, {label}: every particle remains live and finite");
            }
            assert!(bytemuck::cast_slice::<_, u8>(&op) == bytemuck::cast_slice::<_, u8>(&sp),
                "{n}³ tick {frame}: particle bits differ");
            let (of, sf) = (one.faces(), six.faces());
            assert!(bytemuck::cast_slice::<_, u8>(&of) == bytemuck::cast_slice::<_, u8>(&sf),
                "{n}³ tick {frame}: final face bits differ");
            let os: Vec<u32> = one.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
            let ss: Vec<u32> = six.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
            assert_eq!(os, ss, "{n}³ tick {frame}: full liquid stats differ");
            let capped_words = 2 * scene.particles() as usize + SOLVER_WORDS as usize;
            let oc: Vec<u32> = one.read(STEP_NODE, "capped", capped_words);
            let sc: Vec<u32> = six.read(STEP_NODE, "capped", capped_words);
            assert_eq!(oc, sc, "{n}³ tick {frame}: full capped words differ");
            if frame + 1 == WARMUP {
                warm_replay = [one.replay_stats(), six.replay_stats()];
            }
        }
        println!(
            "GPU FLIP {n}³ slot cost, {WARMUP} warmup + {MEASURED} measured ticks/run: median one GPU {:.3} ms CPU {:.3} ms; six GPU {:.3} ms CPU {:.3} ms",
            median(&mut one_gpu), median(&mut one_cpu), median(&mut six_gpu), median(&mut six_cpu),
        );
        for (label, run, warm) in [("one", &one, warm_replay[0]), ("six", &six, warm_replay[1])] {
            let replay = run.replay_stats();
            println!("GPU FLIP {n}³ {label} slots replay: after warmup {warm:?}; final {replay:?}");
            assert!(replay.replayed > warm.replayed && replay.segments_replayed > warm.segments_replayed,
                "{n}³ {label}: measured ticks must exercise ordinary encode replay");
        }
    }
}

/// With no bodies, clearing solid velocity equals its original six passes,
/// including inactive slots. Time ordinary replayed frames without profiling.
#[test]
fn gpu_flip_no_body_solid_clear_matches_six_passes() {
    const WARMUP: usize = 4;
    const MEASURED: usize = 8;
    struct ForceSolidVelocity;
    impl Drop for ForceSolidVelocity {
        fn drop(&mut self) {
            manifold_nodes_water::primitives::gpu_flip_step::set_force_solid_velocity(false);
        }
    }
    fn original_frame(run: &mut Run) -> (f64, f64) {
        manifold_nodes_water::primitives::gpu_flip_step::set_force_solid_velocity(true);
        let _reset = ForceSolidVelocity;
        run.timed_frame()
    }
    fn median(values: &mut [f64]) -> f64 {
        values.sort_by(f64::total_cmp);
        (values[values.len() / 2 - 1] + values[values.len() / 2]) * 0.5
    }
    manifold_nodes_water::primitives::gpu_flip_step::set_force_solid_velocity(false);
    for (n, fresh) in [(16, true), (16, false), (64, true)] {
        let scene = WaterScene::still_pool(n).with_steps(1);
        let mut optimized = run_with_retired_speed(scene, fresh);
        let mut original = run_with_retired_speed(scene, fresh);
        optimized.set_encode_replay(true);
        original.set_encode_replay(true);
        let mut optimized_gpu = Vec::with_capacity(MEASURED);
        let mut optimized_cpu = Vec::with_capacity(MEASURED);
        let mut original_gpu = Vec::with_capacity(MEASURED);
        let mut original_cpu = Vec::with_capacity(MEASURED);
        let mut warm_replay = [manifold_gpu::GpuReplayStats::default(); 2];
        for frame in 0..WARMUP + MEASURED {
            let (optimized_time, original_time) = if frame % 2 == 0 {
                (optimized.timed_frame(), original_frame(&mut original))
            } else {
                let original_time = original_frame(&mut original);
                (optimized.timed_frame(), original_time)
            };
            for (label, time) in [("clear", optimized_time), ("six passes", original_time)] {
                assert!(time.0.is_finite() && time.0 > 0.0,
                    "{n}³ fresh {fresh} tick {frame}, {label}: positive finite GPU chunk span, got {}", time.0);
                assert!(time.1.is_finite() && time.1 > 0.0,
                    "{n}³ fresh {fresh} tick {frame}, {label}: positive finite CPU encode time, got {}", time.1);
            }
            if frame >= WARMUP {
                optimized_gpu.push(optimized_time.0);
                optimized_cpu.push(optimized_time.1);
                original_gpu.push(original_time.0);
                original_cpu.push(original_time.1);
            }
            let a: Vec<u32> = optimized.read(STEP_NODE, "clock_status", 8);
            let b: Vec<u32> = original.read(STEP_NODE, "clock_status", 8);
            assert_eq!(a, b, "{n}³ fresh {fresh} tick {frame}: complete clock status differs");
            let final_dt = if fresh && frame > 0 { 1.0f32 / 60.0 } else { 0.0 };
            assert_eq!(a[0], final_dt.to_bits(), "{n}³ fresh {fresh} tick {frame}: expected recording path");
            assert_eq!(a[1], (1.0f32 / 60.0).to_bits(), "{n}³ fresh {fresh} tick {frame}: exact completed time");
            assert_eq!(a[2], 0.0f32.to_bits(), "{n}³ fresh {fresh} tick {frame}: no unfinished interval");
            assert_eq!(a[6], 1, "{n}³ fresh {fresh} tick {frame}: exactly one active step");
            assert_eq!((a[4], a[5]), (0, 0), "{n}³ fresh {fresh} tick {frame}: no cap or nonfinite input");
            let optimized_step: Vec<FluidParticle> = optimized.read(STEP_NODE, "out", scene.particles() as usize);
            let original_step: Vec<FluidParticle> = original.read(STEP_NODE, "out", scene.particles() as usize);
            for (what, a, b) in [
                ("published particles", optimized.particles(), original.particles()),
                ("step particles", optimized_step, original_step),
            ] {
                assert_eq!(bytemuck::cast_slice::<_, u32>(&a), bytemuck::cast_slice::<_, u32>(&b),
                    "{n}³ fresh {fresh} tick {frame}: {what} bits differ");
                for particles in [&a, &b] {
                    let stats = particle_stats(particles);
                    assert_eq!((stats.live, stats.bad), (optimized.seeded, 0),
                        "{n}³ fresh {fresh} tick {frame}: every {what} record remains live and finite");
                }
            }
            let (a, b) = (optimized.faces(), original.faces());
            assert_eq!(bytemuck::cast_slice::<_, u32>(&a), bytemuck::cast_slice::<_, u32>(&b),
                "{n}³ fresh {fresh} tick {frame}: face bits differ");
            let a: Vec<u32> = optimized.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
            let b: Vec<u32> = original.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
            assert_eq!(a, b, "{n}³ fresh {fresh} tick {frame}: full liquid stats differ");
            let capped_words = 2 * scene.particles() as usize + SOLVER_WORDS as usize;
            let a: Vec<u32> = optimized.read(STEP_NODE, "capped", capped_words);
            let b: Vec<u32> = original.read(STEP_NODE, "capped", capped_words);
            assert_eq!(a, b, "{n}³ fresh {fresh} tick {frame}: full capped words differ");
            if frame + 1 == WARMUP {
                warm_replay = [optimized.replay_stats(), original.replay_stats()];
            }
        }
        println!(
            "GPU FLIP {n}³ no-body solid velocity, fresh {fresh}, {WARMUP} warmup + {MEASURED} measured ticks/run: median clear GPU {:.3} ms CPU {:.3} ms; six passes GPU {:.3} ms CPU {:.3} ms",
            median(&mut optimized_gpu), median(&mut optimized_cpu), median(&mut original_gpu), median(&mut original_cpu),
        );
        for (label, run, warm) in [("clear", &optimized, warm_replay[0]), ("six passes", &original, warm_replay[1])] {
            let replay = run.replay_stats();
            println!("GPU FLIP {n}³ no-body solid velocity, fresh {fresh}, {label} replay: after warmup {warm:?}; final {replay:?}");
            assert!(replay.replayed > warm.replayed && replay.segments_replayed > warm.segments_replayed,
                "{n}³ fresh {fresh}, {label}: measured ticks must exercise ordinary encode replay");
        }
    }
}

/// The pocket sweep segment matches its original indirect dispatches across
/// active/inactive numerical slots, warmed replay and changed clock gates.
#[test]
fn gpu_flip_pocket_segments_match_indirect_sweeps() {
    struct ForceIndirectPockets;
    impl Drop for ForceIndirectPockets {
        fn drop(&mut self) {
            manifold_nodes_water::primitives::gpu_flip_step::set_force_indirect_pockets(false);
        }
    }
    fn original_frame(run: &mut Run) {
        manifold_nodes_water::primitives::gpu_flip_step::set_force_indirect_pockets(true);
        let _reset = ForceIndirectPockets;
        run.frame();
    }
    fn compare(segmented: &Run, indirect: &Run, steps: u32, frame: usize) {
        let a: Vec<u32> = segmented.read(STEP_NODE, "clock_status", 8);
        let b: Vec<u32> = indirect.read(STEP_NODE, "clock_status", 8);
        assert_eq!(a, b, "frame{frame}: all clock words");
        assert_eq!(a[0], 0.0f32.to_bits(), "six-slot path ends inactive");
        assert_eq!(a[1], (1.0f32 / 60.0).to_bits(), "full interval completed");
        assert_eq!(a[2], 0.0f32.to_bits(), "no unfinished interval");
        assert_eq!(a[6], steps, "active steps before inactive tail");
        assert_eq!((a[4], a[5]), (0, 0), "no cap or nonfinite clock input");
        let count = segmented.scene.particles() as usize;
        let segmented_step: Vec<FluidParticle> = segmented.read(STEP_NODE, "out", count);
        let indirect_step: Vec<FluidParticle> = indirect.read(STEP_NODE, "out", count);
        for (label, a, b) in [
            ("published particles", segmented.particles(), indirect.particles()),
            ("step particles", segmented_step, indirect_step),
        ] {
            assert_eq!(bytemuck::cast_slice::<_, u32>(&a), bytemuck::cast_slice::<_, u32>(&b), "frame{frame}: {label}");
            for particles in [&a, &b] {
                let stats = particle_stats(particles);
                assert_eq!((stats.live, stats.bad), (segmented.seeded, 0), "frame{frame}: {label} remain live and finite");
            }
        }
        assert_eq!(bytemuck::cast_slice::<_, u32>(&segmented.faces()), bytemuck::cast_slice::<_, u32>(&indirect.faces()), "frame{frame}: faces");
        for (node, port, words) in [
            ("stats", "stats_out", LIQUID_STATS_WORDS as usize),
            (STEP_NODE, "capped", 2 * count + SOLVER_WORDS as usize),
        ] {
            assert_eq!(segmented.read::<u32>(node, port, words), indirect.read::<u32>(node, port, words), "frame{frame}: {port}");
        }
    }
    manifold_nodes_water::primitives::gpu_flip_step::set_force_indirect_pockets(false);
    let scene = WaterScene::still_pool(16).with_steps(2);
    // Unwired retired speed keeps all six numerical slots: each interval
    // records active work, an inactive tail, then active work next interval.
    let mut segmented = run_with_retired_speed(scene, false);
    let mut indirect = run_with_retired_speed(scene, false);
    segmented.set_encode_replay(true);
    indirect.set_encode_replay(true);
    let mut warm = None;
    for frame in 0..14 {
        let steps = if frame < 10 { 2 } else { 1 };
        if frame == 10 {
            for run in [&mut segmented, &mut indirect] {
                let step = node_named(&run.graph, STEP_NODE);
                run.graph.set_param(step, "steps", manifold_node_engine::parameters::ParamValue::Float(1.0)).expect("steps");
            }
        }
        if frame % 2 == 0 {
            segmented.frame();
            original_frame(&mut indirect);
        } else {
            original_frame(&mut indirect);
            segmented.frame();
        }
        compare(&segmented, &indirect, steps, frame);
        if frame == 9 { warm = Some([segmented.replay_stats(), indirect.replay_stats()]); }
    }
    let warm = warm.expect("ten warmup visits");
    for (label, run, before) in [("segmented", &segmented, warm[0]), ("indirect", &indirect, warm[1])] {
        let after = run.replay_stats();
        assert!(after.replayed > before.replayed, "{label}: changed clock gates exercise warmed replay");
        assert!(after.segments_replayed > before.segments_replayed, "{label}: warmed solver rounds replay");
        assert_eq!(after.recorded, before.recorded, "{label}: changed gates do not record new commands");
        assert_eq!(after.store_allocations, before.store_allocations, "{label}: changed gates reuse replay storage");
    }
    assert!(segmented.replay_stats().segments_replayed - warm[0].segments_replayed
        > indirect.replay_stats().segments_replayed - warm[1].segments_replayed,
        "pocket sweeps add replayed segments");
    segmented.set_encode_replay(false);
    indirect.set_encode_replay(false);
    let before = [segmented.replay_stats(), indirect.replay_stats()];
    segmented.frame();
    original_frame(&mut indirect);
    compare(&segmented, &indirect, 1, 14);
    for (run, before) in [(&segmented, before[0]), (&indirect, before[1])] {
        assert_eq!(run.replay_stats().replayed, before.replayed, "direct frame replays no command chains");
        assert_eq!(run.replay_stats().segments_replayed, before.segments_replayed, "direct frame replays no segments");
    }
}

/// Later inactive numerical slots can skip dense extension dispatches while
/// retaining every active layer and the ordinary replayed frame's results.
#[test]
fn gpu_flip_inactive_extension_dispatch_matches_dense() {
    const WARMUP: usize = 4;
    const MEASURED: usize = 8;
    struct ForceDenseExtend;
    impl Drop for ForceDenseExtend {
        fn drop(&mut self) {
            manifold_nodes_water::primitives::gpu_flip_step::set_force_dense_extend(false);
        }
    }
    fn original_frame(run: &mut Run) -> (f64, f64) {
        manifold_nodes_water::primitives::gpu_flip_step::set_force_dense_extend(true);
        let _reset = ForceDenseExtend;
        run.timed_frame()
    }
    fn median(values: &mut [f64]) -> f64 {
        values.sort_by(f64::total_cmp);
        (values[values.len() / 2 - 1] + values[values.len() / 2]) * 0.5
    }
    fn compare(optimized: &Run, original: &Run, fresh: bool, frame: usize) {
        let scene = optimized.scene;
        let at = format!("{}³ Steps {} fresh {fresh} tick {frame}", scene.pressure.n, scene.steps);
        let a: Vec<u32> = optimized.read(STEP_NODE, "clock_status", 8);
        let b: Vec<u32> = original.read(STEP_NODE, "clock_status", 8);
        assert_eq!(a, b, "{at}: complete clock status differs");
        let final_dt = if fresh && scene.steps == 1 && frame > 0 { 1.0f32 / 60.0 } else { 0.0 };
        assert_eq!(a[0], final_dt.to_bits(), "{at}: expected recording path");
        assert_eq!(a[1], (1.0f32 / 60.0).to_bits(), "{at}: exact completed time");
        assert_eq!(a[2], 0.0f32.to_bits(), "{at}: no unfinished interval");
        assert_eq!(a[6], scene.steps as u32, "{at}: authored Steps accepted exactly");
        assert_eq!((a[4], a[5]), (0, 0), "{at}: no cap or nonfinite input");
        let count = scene.particles() as usize;
        let optimized_step: Vec<FluidParticle> = optimized.read(STEP_NODE, "out", count);
        let original_step: Vec<FluidParticle> = original.read(STEP_NODE, "out", count);
        for (what, a, b) in [
            ("published particles", optimized.particles(), original.particles()),
            ("step particles", optimized_step, original_step),
        ] {
            assert!(bytemuck::cast_slice::<_, u32>(&a) == bytemuck::cast_slice::<_, u32>(&b), "{at}: {what} bits differ");
            for particles in [&a, &b] {
                let stats = particle_stats(particles);
                assert_eq!((stats.live, stats.bad), (optimized.seeded, 0), "{at}: every {what} record remains live and finite");
            }
        }
        let (a, b) = (optimized.faces(), original.faces());
        assert!(bytemuck::cast_slice::<_, u32>(&a) == bytemuck::cast_slice::<_, u32>(&b), "{at}: face bits differ");
        let a: Vec<u32> = optimized.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
        let b: Vec<u32> = original.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
        assert_eq!(a, b, "{at}: full liquid stats differ");
        let capped_words = 2 * count + SOLVER_WORDS as usize;
        let a: Vec<u32> = optimized.read(STEP_NODE, "capped", capped_words);
        let b: Vec<u32> = original.read(STEP_NODE, "capped", capped_words);
        assert!(a == b, "{at}: full capped words differ");
    }
    manifold_nodes_water::primitives::gpu_flip_step::set_force_dense_extend(false);
    for (n, steps, fresh) in [(16, 1, true), (16, 2, false), (64, 1, false)] {
        let scene = WaterScene::still_pool(n).with_steps(steps);
        let mut optimized = run_with_retired_speed(scene, fresh);
        let mut original = run_with_retired_speed(scene, fresh);
        optimized.set_encode_replay(true);
        original.set_encode_replay(true);
        let mut optimized_gpu = Vec::with_capacity(MEASURED);
        let mut optimized_cpu = Vec::with_capacity(MEASURED);
        let mut original_gpu = Vec::with_capacity(MEASURED);
        let mut original_cpu = Vec::with_capacity(MEASURED);
        let mut warm_replay = [manifold_gpu::GpuReplayStats::default(); 2];
        for frame in 0..WARMUP + MEASURED {
            manifold_nodes_water::primitives::gpu_flip_step::set_force_dense_extend(false);
            let (optimized_time, original_time) = if frame % 2 == 0 {
                (optimized.timed_frame(), original_frame(&mut original))
            } else {
                let original_time = original_frame(&mut original);
                (optimized.timed_frame(), original_time)
            };
            for (label, time) in [("indirect", optimized_time), ("dense", original_time)] {
                assert!(time.0.is_finite() && time.0 > 0.0, "{n}³ Steps {steps}, {label}: positive finite GPU chunk span");
                assert!(time.1.is_finite() && time.1 > 0.0, "{n}³ Steps {steps}, {label}: positive finite CPU encode time");
            }
            if frame >= WARMUP {
                optimized_gpu.push(optimized_time.0);
                optimized_cpu.push(optimized_time.1);
                original_gpu.push(original_time.0);
                original_cpu.push(original_time.1);
            }
            compare(&optimized, &original, fresh, frame);
            if frame + 1 == WARMUP {
                warm_replay = [optimized.replay_stats(), original.replay_stats()];
            }
        }
        println!(
            "GPU FLIP {n}³ inactive extension, Steps {steps} fresh {fresh}, {WARMUP} warmup + {MEASURED} measured ticks/run: median gated GPU {:.3} ms CPU {:.3} ms; dense GPU {:.3} ms CPU {:.3} ms",
            median(&mut optimized_gpu), median(&mut optimized_cpu), median(&mut original_gpu), median(&mut original_cpu),
        );
        for (label, run, warm) in [("indirect", &optimized, warm_replay[0]), ("dense", &original, warm_replay[1])] {
            let replay = run.replay_stats();
            println!("GPU FLIP {n}³ inactive extension, Steps {steps}, {label} replay: after warmup {warm:?}; final {replay:?}");
            assert!(replay.replayed > warm.replayed && replay.segments_replayed > warm.segments_replayed,
                "{n}³ Steps {steps}, {label}: measured ticks must exercise ordinary encode replay");
        }
        if !fresh {
            let gated_segments = optimized.replay_stats().segments_replayed - warm_replay[0].segments_replayed;
            let dense_segments = original.replay_stats().segments_replayed - warm_replay[1].segments_replayed;
            assert!(gated_segments > dense_segments, "{n}³ Steps {steps}: extension layers must use gated replay segments");
        }
        if n == 16 && steps == 2 {
            optimized.set_encode_replay(false);
            original.set_encode_replay(false);
            let direct_before = [optimized.replay_stats(), original.replay_stats()];
            manifold_nodes_water::primitives::gpu_flip_step::set_force_dense_extend(false);
            optimized.timed_frame();
            original_frame(&mut original);
            compare(&optimized, &original, fresh, WARMUP + MEASURED);
            for (label, run, before) in [("indirect", &optimized, direct_before[0]), ("dense", &original, direct_before[1])] {
                let after = run.replay_stats();
                assert_eq!(after.replayed, before.replayed, "{label}: the direct frame replayed no command chains");
                assert_eq!(after.segments_replayed, before.segments_replayed, "{label}: the direct frame replayed no pressure rounds");
            }
        }
    }
}

#[test]
fn gpu_flip_fresh_speed_preserves_force_changes_and_multiple_intervals() {
    let scene = WaterScene::still_pool(32).with_steps(1);
    let mut optimized = run_with_retired_speed(scene, true);
    let mut original = run_with_retired_speed(scene, false);
    let mut fast_steps = 0;
    let mut adaptive_steps = 0;
    for frame in 0..10 {
        for run in [&mut optimized, &mut original] {
            if frame == 3 {
                // Throw the pool sideways fast enough to require CFL
                // subdivision in the following interval, after its stats
                // retire. Pulled upward, native water hangs from the floor.
                run.set_gravity(4000.0, -G);
            }
            if frame == 5 {
                run.set_gravity(0.0, -G);
            }
            if frame == 6 {
                run.fps = 30.0;
            }
            if frame == 8 {
                let step = node_named(&run.graph, STEP_NODE);
                run.graph.set_param(step, "steps", manifold_node_engine::parameters::ParamValue::Float(3.0)).unwrap();
            }
            run.frame();
        }
        let a: Vec<u32> = optimized.read(STEP_NODE, "clock_status", 8);
        let b: Vec<u32> = original.read(STEP_NODE, "clock_status", 8);
        assert_eq!(&a[1..], &b[1..], "frame {frame}: completed time and CFL decisions");
        assert_eq!(a[2], 0.0f32.to_bits(), "frame {frame}: no unfinished interval");
        assert_eq!(a[5], 0, "frame {frame}: finite clock input");
        fast_steps += usize::from(a[0] != 0 && a[6] == 1);
        if (3..6).contains(&frame) {
            adaptive_steps += usize::from(a[6] > 1);
        }
        if frame >= 8 {
            assert!(a[6] >= 3, "authored minimum Steps remains active");
        }
        let (ap, bp) = (optimized.particles(), original.particles());
        assert!(bytemuck::cast_slice::<_, u8>(&ap) == bytemuck::cast_slice::<_, u8>(&bp),
            "frame {frame}: particle bits differ");
        assert_eq!(particle_stats(&ap).bad, 0, "frame {frame}: finite water");
        let (af, bf) = (optimized.faces(), original.faces());
        assert!(bytemuck::cast_slice::<_, u8>(&af) == bytemuck::cast_slice::<_, u8>(&bf),
            "frame {frame}: face bits differ");
        let a: Vec<u32> = optimized.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
        let b: Vec<u32> = original.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
        assert_eq!(a, b, "frame {frame}: full liquid stats differ");
    }
    assert!(fast_steps > 0, "fixture must use the one-step shortcut");
    assert!(adaptive_steps > 0, "the changed force must require fresh CFL subdivision");
}

/// FNV-1a over the state's particles after each frame.
#[cfg(test)]
fn particle_digest(run: &Run, digest: &mut u64) {
    for byte in bytemuck::cast_slice::<FluidParticle, u8>(&run.particles()) {
        *digest = (*digest ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
    }
}

/// The Dam Break with the step's Sheet Fill Rate set, when given.
#[cfg(test)]
fn dam_break_sheeting(n: usize, rate: Option<f64>) -> Run {
    let scene = WaterScene::dam_break(n);
    if let Some(rate) = rate {
        Run::posed(scene, &[("domain", "sheet_fill_rate", rate)])
    } else {
        Run::new(scene)
    }
}

/// Sheet seeding off is the step it was (BUG-j9l9w): the Dam Break's
/// particles after four frames, digested, equal to main's digest before
/// sheet seeding (a deliberate step change re-pins it). Sheet Fill Rate 0 set
/// explicitly must equal it too.
#[test]
fn gpu_flip_sheeting_off_leaves_the_dam_break_unchanged() {
    let digest = |rate: Option<f64>| {
        let mut run = dam_break_sheeting(32, rate);
        let mut digest = 0xcbf2_9ce4_8422_2325u64;
        for _ in 0..4 {
            run.frame();
            particle_digest(&run, &mut digest);
        }
        digest
    };
    let default_off = digest(None);
    eprintln!("DAM BREAK DIGEST 32 x4: {default_off:016x}");
    // The whole step's pin: only a commit that changes the step on purpose
    // re-records it, and says why. Last re-recorded for the tank walls matching
    // the native engine; sheeting with walls and no pressure template gives it too.
    assert_eq!(default_off, 0xb915_115e_7604_78dd, "sheeting off changed the Dam Break");
    assert_eq!(digest(Some(0.0)), default_off, "rate 0 changed the step");
}

/// Sheet seeding on: replayed encodes give the particles a direct encode
/// gives, bit for bit, frame by frame, with births among them.
#[test]
fn gpu_flip_sheeting_replay_matches_direct() {
    let mut direct = dam_break_sheeting(32, Some(1.0));
    let mut replayed = dam_break_sheeting(32, Some(1.0));
    replayed.set_encode_replay(true);
    let seeded = direct.particles().iter().map(|p| p.id).max().unwrap_or(0);
    let mut born = 0;
    for frame in 0..24 {
        direct.frame();
        replayed.frame();
        let a = direct.particles();
        assert!(a == replayed.particles(), "frame {frame}: replay differs");
        born = a.iter().filter(|p| p.position_radius[3] > 0.0 && p.id > seeded).count();
    }
    let stats = replayed.replay_stats();
    eprintln!("SHEETING REPLAY: {born} live births after 24 frames, {stats:?}");
    assert!(stats.replayed > 0, "nothing replayed");
    assert!(born > 0, "the Dam Break seeded no sheets");
}

use manifold_node_engine::testkit::atom::{node_named, output_of};

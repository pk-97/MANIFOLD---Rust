//! The GPU FLIP water step run on whole scenes (docs/GPU_FLIP_PRESSURE_SOLVE.md
//! section 1 (the step)): the momentum, still-pool and meshed-volume proofs, and the scene
//! runner the race probes (`gpu_flip_race_tests`) share.
//! `gpu_flip_scenes_cover_every_dispatch` proves every array these graphs
//! allocate before any of them runs here.

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;

use super::gpu_flip_preset::{DAM_COLUMN, DAM_FILL_HEIGHT, DAM_OBSTACLE, FACE_NODES, REST_PER_CELL, STEP_NODE, WaterScene, water_def};
use crate::node_graph::liquid::grid::face_len;
use super::gpu_flip_volume::{VolumeDrift, volume_and_area};
use super::liquid_stats::{LIQUID_STATS_WORDS, SOLVER_WORDS, LiquidTickStats};
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::fluid_particles::{FaceSample, FluidParticle};
use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
use crate::node_graph::{
    EffectGraphDefExt, ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, NodeInstanceId, PrimitiveRegistry,
    ResourceId, StateStore, compile, pre_allocate_resources,
};

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

pub(super) fn node_named(graph: &Graph, name: &str) -> NodeInstanceId {
    graph.nodes().find(|n| n.node_id.as_str() == name).map(|n| n.id).unwrap_or_else(|| panic!("no node {name}"))
}

pub(super) fn output_of(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    let step = plan.steps().iter().find(|s| s.node == node).expect("node compiled");
    step.outputs.iter().find(|(name, _)| *name == port).map(|&(_, r)| r).expect("output port")
}

/// The node whose id ends with `name`: the flattened surface group's nodes
/// carry the group's path before their own id.
fn node_ending(graph: &Graph, name: &str) -> crate::node_graph::NodeInstanceId {
    let mut found = graph.nodes().filter(|n| n.node_id.as_str().ends_with(name));
    let node = found.next().unwrap_or_else(|| panic!("no node ending {name}"));
    assert!(found.next().is_none(), "two nodes end {name}");
    node.id
}

/// A scene's graph compiled and bound once, run frame by frame.
pub(super) struct Run {
    device: crate::TestDevice,
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
    pub(super) fn new(scene: WaterScene) -> Self {
        Self::posed(scene, &[])
    }

    /// `new` with each `(node, param, value)` set before the fill frame, so
    /// the fill and the first tick share that pose. A body posed after the
    /// fill crosses the whole move in its first tick, at the move's speed.
    fn posed(scene: WaterScene, params: &[(&str, &str, f64)]) -> Self {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        let mut graph = water_def(scene).into_graph(&registry, &Default::default()).expect("water def builds");
        for &(node, name, value) in params {
            let node = node_named(&graph, node);
            graph.set_param(node, name, crate::node_graph::ParamValue::Float(value as f32)).expect(name);
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
        let device = crate::test_device();
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
        let vertices: Vec<crate::generators::mesh_common::MeshVertex> =
            self.read_at(node_ending(&self.graph, "liquid_normals"), "out", vertex_count);
        indices.chunks_exact(3).map(|triangle| [0, 1, 2].map(|i| vertices[triangle[i] as usize].position)).collect()
    }

    /// The volume the surface mesh holds in the tank and its free surface's area.
    pub(super) fn surface_measure(&self) -> (f64, f64) {
        volume_and_area(self.surface().into_iter(), self.scene.min(), self.scene.size)
    }

    /// The particles' own volume: `REST_PER_CELL` fill a cell.
    pub(super) fn particle_volume(&self) -> f64 {
        self.scene.particles() as f64 * self.scene.cell_size().powi(3) / REST_PER_CELL
    }

    /// One frame with a GPU timestamp per dispatch: milliseconds per node
    /// type, largest first, and the frame's total.
    #[cfg(feature = "water-race-probes")]
    pub(super) fn profiled_frame(&mut self) -> (Vec<(String, f64)>, f64) {
        self.entering = self.particles();
        let sampler = self.device.create_timestamp_sampler(8192).expect("timestamp sampling");
        let mut enc = self.device.create_encoder("gpu-flip-scene-profile");
        enc.enable_dispatch_profiling(sampler, &self.device);
        self.exec.set_profiling(true);
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            let time = FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(self.frames as f64 / self.fps),
                delta: Seconds(1.0 / self.fps),
                frame_count: self.frames,
            };
            self.exec.execute_frame_with_state(&mut self.graph, &self.plan, time, &mut gpu, &mut self.state, 0);
            self.frames += 1;
        }
        self.exec.set_profiling(false);
        let profile = enc.commit_and_wait_profiled(&self.device);
        let type_of: Vec<String> = self
            .plan
            .steps()
            .iter()
            .map(|s| self.graph.nodes().find(|n| n.id == s.node).map_or(String::new(), |n| n.node.type_id().as_str().to_string()))
            .collect();
        let mut by_type: Vec<(String, f64)> = Vec::new();
        for span in &profile.spans {
            let ty = span.tag.rsplit_once(":s").and_then(|(_, i)| i.parse::<usize>().ok()).and_then(|i| type_of.get(i)).map_or("unattributed", |t| t.as_str());
            match by_type.iter_mut().find(|(t, _)| t == ty) {
                Some(row) => row.1 += span.millis,
                None => by_type.push((ty.to_string(), span.millis)),
            }
        }
        by_type.sort_by(|a, b| b.1.total_cmp(&a.1));
        (by_type, profile.total_ms)
    }

    /// One frame with a GPU timestamp per dispatch: milliseconds per
    /// dispatch label, summed over the frame, and the frame's total.
    #[cfg(feature = "water-race-probes")]
    pub(super) fn profiled_labels(&mut self) -> (Vec<(String, f64, usize)>, f64) {
        let profile = self.sampled_frame(manifold_gpu::ProfileGranularity::Dispatch);
        let mut by_label: Vec<(String, f64, usize)> = Vec::new();
        for span in &profile.spans {
            match by_label.iter_mut().find(|(l, _, _)| *l == span.label) {
                Some(row) => {
                    row.1 += span.millis;
                    row.2 += 1;
                }
                None => by_label.push((span.label.clone(), span.millis, 1)),
            }
        }
        (by_label, profile.total_ms)
    }

    /// Preserve sampling diagnostics for probes that require complete attribution.
    #[cfg(feature = "water-race-probes")]
    fn sampled_frame(&mut self, granularity: manifold_gpu::ProfileGranularity) -> manifold_gpu::GpuFrameProfile {
        self.entering = self.particles();
        let sampler = self.device.create_timestamp_sampler(8192).expect("timestamp sampling");
        let mut enc = self.device.create_encoder("gpu-flip-scene-labels");
        enc.enable_profiling_at(sampler, &self.device, granularity);
        self.exec.set_profiling(true);
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            let time = FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(self.frames as f64 / self.fps),
                delta: Seconds(1.0 / self.fps),
                frame_count: self.frames,
            };
            self.exec.execute_frame_with_state(&mut self.graph, &self.plan, time, &mut gpu, &mut self.state, 0);
            self.frames += 1;
        }
        self.exec.set_profiling(false);
        enc.commit_and_wait_profiled(&self.device)
    }

    /// One frame in its own command buffer: GPU ms and CPU encode ms.
    pub(super) fn frame(&mut self) -> (f64, f64) {
        self.frame_with_timing(false)
    }

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

    fn read_at<T: bytemuck::Pod>(&self, node: crate::node_graph::NodeInstanceId, port: &str, len: usize) -> Vec<T> {
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

    pub(super) fn n(&self) -> usize {
        self.solver_grid().cells()[0] as usize
    }

    fn solver_grid(&self) -> crate::node_graph::liquid::lattice::FlipSolverGrid {
        crate::node_graph::liquid::lattice::FlipSolverGrid::from_lattice(
            crate::node_graph::liquid::lattice::LiquidLattice::from_layout(&self.scene.layout()),
        )
    }

    /// The force hook: the domain's uniform acceleration (Gravity X and Y),
    /// m/s², from the next frame on. A uniform force field and gravity enter
    /// the step identically, at every face.
    pub(super) fn set_gravity(&mut self, x: f64, y: f64) {
        let domain = node_named(&self.graph, "domain");
        self.graph.set_param(domain, "gravity_x", crate::node_graph::ParamValue::Float(x as f32)).expect("gravity_x");
        self.graph.set_param(domain, "gravity", crate::node_graph::ParamValue::Float(y as f32)).expect("gravity");
    }

    /// The project frame rate from the next frame on: the tick's dt is one
    /// frame.
    #[cfg(feature = "water-race-probes")]
    pub(super) fn set_fps(&mut self, fps: f64) {
        self.fps = fps;
    }

    /// Switch the executor's encode replay; the fill frame already ran
    /// (it ticks no step), every later frame honours the switch.
    pub(super) fn set_encode_replay(&mut self, on: bool) {
        self.exec.set_encode_replay(on);
    }

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
    pub(super) fn water(&self) -> Vec<f32> {
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
    pub(super) fn liquid_stats(&self) -> LiquidTickStats {
        LiquidTickStats::from_words(&self.read::<u32>("stats", "stats_out", LIQUID_STATS_WORDS as usize))
    }

    /// The last tick's speed-capped move stages and refused push-outs.
    pub(super) fn capped(&self) -> (u64, u64) {
        let words: Vec<u32> = self.read(STEP_NODE, "capped", 2 * self.scene.particles() as usize);
        words.chunks(2).fold((0, 0), |(c, p), w| (c + u64::from(w[0]), p + u64::from(w[1])))
    }

    /// The last tick's solver words: pressure and density iterations over
    /// every substep, and solves that reached the cap unconverged.
    pub(super) fn solver(&self) -> [u32; 3] {
        let n = 2 * self.scene.particles() as usize;
        let words: Vec<u32> = self.read(STEP_NODE, "capped", n + 3);
        [words[n], words[n + 1], words[n + 2]]
    }

    /// The last substep's face grid: projected, constrained to the solids
    /// and extended.
    pub(super) fn faces(&self) -> Vec<FaceSample> {
        self.read(STEP_NODE, "faces", (self.n() + 1).pow(3))
    }

    /// The seam face grid of the frame's last tick, x, y and z (a scene built
    /// with `faces`).
    pub(super) fn face_grid(&self) -> [Vec<f32>; 3] {
        let cells = [self.n() as u32; 3];
        std::array::from_fn(|axis| self.read(FACE_NODES[axis], "out", face_len(cells, axis) as usize))
    }

    /// Water cells in the step's lattice: the size of its pressure solve.
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
        let expected = crate::node_graph::liquid::conformance::gpu_flip_faces(bytemuck::cast_slice(&state), run.solver_grid().cells());
        let gathered = (0..3)
            .map(|axis| grid[axis].iter().zip(&expected[axis]).filter(|(a, b)| a.to_bits() != b.to_bits()).count())
            .sum::<usize>();
        println!("GPU FLIP face grid {n}³: {moving} of {records} face samples moving; state differs at {differ}, gather at {gathered}");
        assert!(moving > records / 100, "{n}³: the faces barely move");
        assert_eq!(differ, 0, "{n}³: the state's faces are not the last step's");
        assert_eq!(gathered, 0, "{n}³: the face components differ from the state's faces");
    }
}

/// I5: a pool at rest stays at rest. After 2 s the particle count is the
/// fill's and the fastest particle moves under 1 mm/s.
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
            fastest.push(stats.fastest);
        }
    }
    let end = *fastest.last().expect("sampled");
    assert!(end < 1e-3, "fastest particle {end} m/s after 2 s");
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

/// The closed wall at rest: a 1 m pool for 300 frames. Every box wall face of
/// the projected grid is exactly 0, and the pressure is hydrostatic: forces
/// added g·dt to every vertical face and the projection took it back, so a
/// face between two water cells below the surface layer keeps under 1% of
/// g·dt, which puts the pressure gradient, and so p = ρgh, within 1%.
#[test]
fn gpu_flip_hydrostatic_column_rests() {
    let scene = WaterScene::still_pool(64);
    let mut run = Run::new(scene);
    let (n, h) = (run.n(), scene.cell_size());
    let m = n + 1;
    let g_dt = G * scene.step_dt();
    // Cells wholly under the seeded top, one layer of margin below it.
    let deep = ((scene.fill_height / h).floor() as usize).saturating_sub(2);
    for frame in 0..300 {
        run.frame();
        if frame % 30 != 29 {
            continue;
        }
        let faces = run.faces();
        let water = run.water();
        let mut wall = 0.0_f64;
        let mut worst = 0.0_f64;
        for k in 0..m {
            for j in 0..m {
                for i in 0..m {
                    let p = [i, j, k];
                    let face = &faces[i + m * (j + m * k)];
                    for a in 0..3 {
                        if (0..3).any(|b| b != a && p[b] >= n) {
                            continue;
                        }
                        if p[a] == 0 || p[a] == n {
                            wall = wall.max(f64::from(face.velocity[a]).abs());
                        } else if a == 1 && j <= deep {
                            let below = i + n * ((j - 1) + n * k);
                            let above = i + n * (j + n * k);
                            if water[below] > 0.5 && water[above] > 0.5 {
                                worst = worst.max(f64::from(face.velocity[a]).abs());
                            }
                        }
                    }
                }
            }
        }
        let stats = particle_stats(&run.particles());
        println!(
            "GPU FLIP hydrostatic {n}³ frame {frame:3}: wall faces max |v| {wall:.1e}, inner vertical faces max |v| {worst:.2e} m/s = {:.3}% of g·dt, fastest particle {:.2e} m/s",
            100.0 * worst / g_dt,
            stats.fastest
        );
        assert_eq!(wall, 0.0, "frame {frame}: a wall face moves");
        assert!(worst <= 0.01 * g_dt, "frame {frame}: the pressure gradient is {:.3}% off ρg", 100.0 * worst / g_dt);
        assert_eq!((stats.live, stats.bad), (run.seeded, 0), "frame {frame}: particles lost or not finite");
    }
}

/// Mean particles per cell over the interior water: cells that, with all
/// 26 neighbours, are φ < 0. Unlike particles per water cell, a growing
/// surface does not move it.
pub(super) fn interior_density(run: &Run, particles: &[FluidParticle]) -> f64 {
    let (n, h, min) = (run.n(), run.scene.cell_size(), run.solver_grid().min().map(f64::from));
    let water = run.water_of(particles);
    let mut count = vec![0u32; n.pow(3)];
    for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
        let c: [usize; 3] = std::array::from_fn(|a| ((f64::from(p.position_radius[a]) - min[a]) / h).floor().clamp(0.0, (n - 1) as f64) as usize);
        count[c[0] + n * (c[1] + n * c[2])] += 1;
    }
    let (mut cells, mut total) = (0u64, 0u64);
    for k in 1..n - 1 {
        for j in 1..n - 1 {
            for i in 1..n - 1 {
                let all = (0..27).all(|d| {
                    let (di, dj, dk) = (d % 3, (d / 3) % 3, d / 9);
                    water[(i + di - 1) + n * ((j + dj - 1) + n * (k + dk - 1))] > 0.5
                });
                if all {
                    cells += 1;
                    total += u64::from(count[i + n * (j + n * k)]);
                }
            }
        }
    }
    total as f64 / cells.max(1) as f64
}

/// The pool's depth as its volume reads it: the mean over the floor's
/// columns of the highest particle's height, plus the quarter cell the fill
/// seeds under a surface. A slosh moves water between columns, not out of
/// them, so this holds while the pool still moves.
fn column_depth(run: &Run, particles: &[FluidParticle], floor: f64) -> f64 {
    let (n, h, min) = (run.scene.layout().cells[0] as usize, run.scene.cell_size(), run.scene.min());
    let mut top = vec![f64::NEG_INFINITY; n * n];
    for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
        let c = |a: usize| ((f64::from(p.position_radius[a]) - min[a]) / h).floor().clamp(0.0, (n - 1) as f64) as usize;
        let at = c(0) + n * c(2);
        top[at] = top[at].max(f64::from(p.position_radius[1]));
    }
    top.iter().map(|&y| if y.is_finite() { y - floor + 0.25 * h } else { 0.0 }).sum::<f64>() / (n * n) as f64
}

/// Historical acceptance of the optional density correction: at frame 1800
/// (30 s), estimated column depth is within 3% of the analytic average and
/// interior density is within 2% of its start. These are not direct volume
/// measurements or native agreement tests. From frame 400, KE + PE must not
/// rise by more than 1e-4 of its start per frame and must finish lower.
#[test]
fn gpu_flip_density_projection_settled_column_depth_and_density() {
    const FRAMES: usize = 1800;
    const SETTLING: usize = 400;
    for steps in [1, 2] {
        let scene = WaterScene { volume_projection: true, ..WaterScene::dam_break(64).with_steps(steps) };
        let mut run = Run::new(scene);
        let floor = scene.min()[1];
        let start = run.particles();
        let e0 = energy(&start, floor);
        let rho0 = interior_density(&run, &start);
        let column: f64 = DAM_COLUMN.iter().map(|[lo, hi]| hi - lo).product();
        let want = (DAM_FILL_HEIGHT * scene.size * scene.size + column) / (scene.size * scene.size);
        let (mut last, mut worst) = (e0, (f64::NEG_INFINITY, 0, 0.0, 0.0));
        let mut last_terms = (0.0, 0.0);
        let mut at_settling = e0;
        let (mut capped_frames, mut capped_stages) = (0, 0);
        for frame in 1..=FRAMES {
            run.frame();
            let (stages, _) = run.capped();
            capped_frames += usize::from(stages > 0);
            capped_stages += stages;
            let particles = run.particles();
            let e = energy(&particles, floor);
            let ke: f64 = particles
                .iter()
                .filter(|p| p.position_radius[3] > 0.0)
                .map(|p| 0.5 * p.velocity.iter().map(|&c| f64::from(c).powi(2)).sum::<f64>())
                .sum();
            let pe = e - ke;
            if frame > SETTLING && e - last > worst.0 {
                worst = (e - last, frame, ke - last_terms.0, pe - last_terms.1);
            }
            (last, last_terms) = (e, (ke, pe));
            if frame == SETTLING {
                at_settling = e;
            }
            if frame % 120 == 0 {
                println!(
                    "GPU FLIP settle {steps} steps frame {frame:4}: E/E0 {:.5}, KE/E0 {:.2e}, column depth {:.4} m, interior {:.3} a cell",
                    e / e0,
                    ke / e0,
                    column_depth(&run, &particles, floor),
                    interior_density(&run, &particles)
                );
            }
        }
        println!("GPU FLIP settle {steps} steps: the speed cap fired on {capped_frames} of {FRAMES} frames' last ticks, {capped_stages} move stages");
        let particles = run.particles();
        let (depth, rho) = (column_depth(&run, &particles, floor), interior_density(&run, &particles));
        println!(
            "GPU FLIP settle {steps} steps: column depth {depth:.4} m against {want:.4} m ({:+.2}%), interior {rho:.3} against {rho0:.3} a cell ({:+.2}%)",
            100.0 * (depth / want - 1.0),
            100.0 * (rho / rho0 - 1.0)
        );
        println!(
            "GPU FLIP settle {steps} steps: worst frame-to-frame rise after frame {SETTLING} {:+.3e} E0 at frame {} (KE {:+.3e} E0, PE {:+.3e} E0)",
            worst.0 / e0,
            worst.1,
            worst.2 / e0,
            worst.3 / e0
        );
        assert!((depth / want - 1.0).abs() <= 0.03, "{steps} steps: column depth {depth} m, want {want} m");
        assert!((rho / rho0 - 1.0).abs() <= 0.02, "{steps} steps: interior density {rho} against {rho0}");
        assert!(last < at_settling, "{steps} steps: E {last} at frame {FRAMES} not under {at_settling} at frame {SETTLING}");
        assert!(worst.0 <= 1e-4 * e0, "{steps} steps: energy rose {:e} E0 at frame {}", worst.0 / e0, worst.1);
    }
}

/// Mechanical energy per unit particle mass summed over the live particles,
/// KE + PE with the floor at `floor` (J/kg).
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

/// Live particles, φ-water cells, air cells (no particle: the density
/// source's air) and particles per occupied cell (rest 8).
#[cfg(feature = "water-race-probes")]
fn cloud_counts(run: &Run, particles: &[FluidParticle]) -> (usize, usize, usize, f64) {
    let (n, h, min) = (run.n(), run.scene.cell_size(), run.solver_grid().min().map(f64::from));
    let mut occupied = vec![false; n.pow(3)];
    let mut live = 0;
    for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
        live += 1;
        let c = |a: usize| ((f64::from(p.position_radius[a]) - min[a]) / h).floor().clamp(0.0, (n - 1) as f64) as usize;
        occupied[c(0) + n * (c(1) + n * c(2))] = true;
    }
    let held = occupied.iter().filter(|&&o| o).count();
    let water = run.water_of(particles).iter().filter(|&&w| w > 0.5).count();
    (live, water, n.pow(3) - held, live as f64 / held.max(1) as f64)
}

/// The share of water cells whose density source the beside-air rule raises
/// to rest: a face neighbour in the box holds no particle and the cell's tent
/// density, wall sites included, reads under 8 (`density_source`, no bodies).
#[cfg(feature = "water-race-probes")]
fn raised_share(run: &Run, particles: &[FluidParticle], water: &[f32]) -> f64 {
    let (n, h, min) = (run.n() as i64, run.scene.cell_size(), run.solver_grid().min().map(f64::from));
    let at = |c: [i64; 3]| (c[0] + n * (c[1] + n * c[2])) as usize;
    let mut bins: Vec<Vec<[f64; 3]>> = vec![Vec::new(); (n * n * n) as usize];
    for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
        let q: [f64; 3] = std::array::from_fn(|a| (f64::from(p.position_radius[a]) - min[a]) / h);
        let c: [i64; 3] = std::array::from_fn(|a| (q[a].floor() as i64).clamp(0, n - 1));
        bins[at(c)].push(q);
    }
    let (mut wet, mut raised) = (0usize, 0usize);
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                let p = [x, y, z];
                if water[at(p)] <= 0.5 {
                    continue;
                }
                wet += 1;
                let centre = p.map(|i| i as f64 + 0.5);
                let (mut density, mut beside_air) = (0.0, false);
                for dz in -1..=1i64 {
                    for dy in -1..=1i64 {
                        for dx in -1..=1i64 {
                            let d = [dx, dy, dz];
                            let q = [x + dx, y + dy, z + dz];
                            if q.iter().any(|&i| i < 0 || i >= n) {
                                density += d.iter().map(|&o| if o == 0 { 1.5 } else { 0.25 }).product::<f64>();
                                continue;
                            }
                            let bin = &bins[at(q)];
                            for s in bin {
                                density += (0..3).map(|a| (1.0 - (centre[a] - s[a]).abs()).clamp(0.0, 1.0)).product::<f64>();
                            }
                            if d.iter().map(|o| o.abs()).sum::<i64>() == 1 && bin.is_empty() {
                                beside_air = true;
                            }
                        }
                    }
                }
                raised += usize::from(beside_air && density < 8.0);
            }
        }
    }
    raised as f64 / wet.max(1) as f64
}

/// BUG-o6dg (thin cloud never comes back), measurement only: a 0.64 m pool
/// at 64³ lifted for 60 frames by gravity of (+5 sideways, +20 up) m/s²,
/// then normal gravity to frame 600. Reversed gravity alone on the seeded
/// flat pool is an exact equilibrium the solve holds with negative pressure,
/// so the lift needs the sideways part. Every 30 frames: particles, water
/// cells, air cells, particles per occupied cell, and the share of water
/// cells the beside-air rule raises to rest.
#[cfg(feature = "water-race-probes")]
#[test]
fn gpu_flip_forced_pool_volume_probe() {
    let n = 64;
    let mut run = Run::new(WaterScene::pool(n, WaterScene::still_pool(n).size, 0.64));
    let mut no_air = None;
    for frame in 0..=600 {
        if frame > 0 {
            let (x, y) = if frame <= 60 { (5.0, 20.0) } else { (0.0, -G) };
            run.set_gravity(x, y);
            run.frame();
        }
        let particles = run.particles();
        let (live, water, air, per_cell) = cloud_counts(&run, &particles);
        if air == 0 && no_air.is_none() {
            no_air = Some(frame);
        }
        if frame % 30 == 0 {
            let share = raised_share(&run, &particles, &run.water_of(&particles));
            println!(
                "forced pool {n}³ frame {frame:3}: {live} particles, {water} water cells, {air} air cells, {per_cell:.3} per occupied cell, {:.1}% of water cells raised to rest",
                100.0 * share
            );
        }
    }
    println!("forced pool {n}³: first frame with no air cell {no_air:?}");
}

/// Peter's waterFunv3 export (BUG-o6dg): 24 fps, 128, Domain Size 10.755,
/// fill 0.5845, one step. Its Uniform Force card points (0, 2, 0) with
/// Strength at 0 and a low-band kick audio mod driving it to +20 (rangeMin
/// 0.5 of −20..20, attack 0, release 72 ms), so each kick lifts the water at
/// up to 40 m/s² over gravity. Kicks are modelled four to the bar at 140 bpm,
/// each at full level, decaying as exp(−t/72 ms). The Vortex card (also
/// kick-driven) is not modelled: it is horizontal and the hook is uniform.
#[cfg(feature = "water-race-probes")]
#[test]
fn gpu_flip_kick_lift_pool_volume_probe() {
    // A still seeded pool is an equilibrium, so the control kicks for the
    // first 3 s, then stops: it asks whether the thrown pool comes back.
    for kick_frames in [300, 72] {
        kick_lift_pool(kick_frames);
    }
}

/// Which change freezes the kick pool: five frames each of 4 m at 24 fps,
/// 10.755 m at 60 fps, 128 at 4 m, and all three, under a net 30 m/s² lift.
#[cfg(feature = "water-race-probes")]
#[test]
fn gpu_flip_kick_freeze_split_probe() {
    for (n, size, fps) in [(64, 4.0, 24.0), (64, 10.755186, 60.0), (128, 4.0, 60.0), (128, 10.755186, 24.0)] {
        let mut run = Run::new(WaterScene::pool(n, size, 0.58450913));
        run.set_fps(fps);
        let y0: f64 = run.particles().iter().map(|p| f64::from(p.position_radius[1])).sum();
        for frame in 1..=5 {
            run.set_gravity(0.0, 40.0 - G);
            run.frame();
            let s = run.liquid_stats();
            let y: f64 = run.particles().iter().map(|p| f64::from(p.position_radius[1])).sum();
            println!(
                "freeze split {n}³ {size} m {fps} fps frame {frame}: seconds {:.4}, live {}, pressure iterations {}, max speed {:.3}, kinetic {:.3e}, mean y moved {:.2e} m",
                run.frames as f64 / run.fps,
                s.live,
                s.pressure_iterations,
                s.max_speed,
                s.kinetic,
                (y - y0) / s.live.max(1) as f64
            );
        }
    }
}

/// One 300-frame run of the kick-lift pool, kicking for its first `kick_frames`. The force is
/// sampled once per 24 fps display frame and held over that frame's 2-3
/// liquid ticks, as the live force path does.
#[cfg(feature = "water-race-probes")]
fn kick_lift_pool(kick_frames: usize) {
    let n = 128;
    let mut run = Run::new(WaterScene::pool(n, 10.755186, 0.58450913));
    run.set_fps(24.0);
    let beat = 60.0 / 140.0;
    let mut no_air = None;
    for frame in 0..=300 {
        if frame > 0 {
            let since_kick = (frame as f64 / 24.0) % beat;
            let lift = if frame <= kick_frames { 2.0 * 20.0 * (-since_kick / 0.072).exp() } else { 0.0 };
            run.set_gravity(0.0, -G + lift);
            run.frame();
        }
        let particles = run.particles();
        let (live, water, air, per_cell) = cloud_counts(&run, &particles);
        if air == 0 && no_air.is_none() {
            no_air = Some(frame);
        }
        if frame % 30 == 0 {
            let share = raised_share(&run, &particles, &run.water_of(&particles));
            println!(
                "kick pool {n}³ 24fps kicks to {kick_frames} frame {frame:3}: {live} particles, {water} water cells, {air} air cells, {per_cell:.3} per occupied cell, {:.1}% of water cells raised to rest",
                100.0 * share
            );
        }
    }
    println!("kick pool {n}³ kicks to {kick_frames}: first frame with no air cell {no_air:?}");
}

/// The Dam Break never gains energy: an inviscid solver with closed walls
/// can only lose KE + PE (PIC blending, the projection, wall stops), so no
/// frame may sit above the energy it started with past float noise
/// (1e-4 of it). A high run-up that passes this is physics, not a source.
#[test]
fn gpu_flip_dam_break_energy_never_rises() {
    for steps in [1, 2] {
        let scene = WaterScene::dam_break(64).with_steps(steps);
        let mut run = Run::new(scene);
        let floor = scene.min()[1];
        let e0 = energy(&run.particles(), floor);
        let mut worst = f64::NEG_INFINITY;
        let (mut most, mut unconverged) = ([0u32; 2], 0u32);
        for frame in 0..300 {
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
        // step on the Dam Break with the obstacle box; the ceiling is about
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
fn obstacle_depth(p: &FluidParticle, pos: [f64; 3]) -> f64 {
    let scale = DAM_OBSTACLE[1];
    (0..3).map(|a| 0.5 * scale[a] - (f64::from(p.position_radius[a]) - pos[a]).abs()).fold(f64::INFINITY, f64::min)
}

/// The deepest live particle inside the obstacle at `pos`, and the live count.
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
        run.graph.set_param(transform, "pos_x", crate::node_graph::ParamValue::Float(pos[0] as f32)).expect("pos_x");
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

/// Where a Dam Break frame's GPU time goes at 64³, by node type, after 60
/// frames.
#[cfg(feature = "water-race-probes")]
#[test]
fn gpu_flip_frame_by_node_type() {
    let mut run = Run::new(WaterScene::dam_break(64));
    for _ in 0..60 {
        run.frame();
    }
    let plain: Vec<f64> = (0..5).map(|_| run.frame().0).collect();
    let (by_type, total) = run.profiled_frame();
    println!("GPU FLIP frame by type: {total:.2} ms profiled, plain {plain:?}");
    for (ty, ms) in by_type.iter().take(15) {
        println!("GPU FLIP frame by type:   {ty:32} {ms:7.2} ms");
    }
}

/// Small native/GPU comparisons share seed geometry and simulation time.
mod native_reference {
    use super::*;
    use manifold_fluids::{Bounds, CaptureError, Config, FluidWorld, ParticleRecord};

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

    #[cfg(feature = "water-race-probes")]
    fn capture(world: &mut FluidWorld, offset: [f32; 3]) -> Vec<FluidParticle> {
        snapshot(world, offset).0
    }

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
            let column = geometry.setup.box_sites;
            let mut expected = Vec::new();
            for x in 0..2 * n as u32 {
                for y in 0..2 * n as u32 {
                    for z in 0..2 * n as u32 {
                        let site = [x, y, z];
                        if y < geometry.setup.pool_sites || (0..3).all(|a| (column[a][0]..column[a][1]).contains(&site[a])) {
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

    /// 1.5 simulated seconds at 16³ from captured native particle records:
    /// observe motion and cost without meshing or rendering. This isolates
    /// solver behaviour; it does not claim the production fills match.
    #[cfg(feature = "water-race-probes")]
    #[test]
    fn gpu_flip_native_dam_break_reference() {
        fn median(mut values: Vec<f64>) -> f64 {
            values.sort_by(f64::total_cmp);
            values[values.len() / 2]
        }
        fn measures(particles: &[FluidParticle]) -> [f64; 5] {
            let stats = particle_stats(particles);
            assert_eq!(stats.bad, 0);
            assert!(stats.live > 0);
            let live: Vec<_> = particles.iter().filter(|p| p.position_radius[3] > 0.0).collect();
            let mut heights: Vec<_> = live.iter().map(|p| f64::from(p.position_radius[1])).collect();
            heights.sort_by(f64::total_cmp);
            let x = live.iter().map(|p| f64::from(p.position_radius[0])).sum::<f64>() / stats.live as f64;
            let speed2 = live.iter().flat_map(|p| p.velocity).map(|v| f64::from(v).powi(2)).sum::<f64>() / stats.live as f64;
            [x, stats.mean_height, heights[heights.len() * 99 / 100], speed2.sqrt(), energy(particles, 0.0) / stats.live as f64]
        }
        let scene = WaterScene::race_dam_break(16);
        let (mut native, offset) = world(scene);
        let seed = capture(&mut native, offset);
        let sites = seed_sites(scene, &seed);
        assert!(sites.windows(2).all(|p| p[0] != p[1]), "unique native seed sites");
        let (mut reference, mut wall_ms) = (Vec::new(), Vec::new());
        let started = std::time::Instant::now();
        println!("motion metrics: mean_x, mean_y, y99 (metres), rms_speed (m/s), mean_energy (J/kg)");
        for frame in 1..=90 {
            let start = std::time::Instant::now();
            let stats = native.step(Seconds(1.0 / 60.0)).expect("native step");
            wall_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(stats.substeps, 1, "16³ fixture should not require native CFL subdivision");
            if frame % 30 == 0 {
                let particles = capture(&mut native, offset);
                let measure = measures(&particles);
                println!("native frame {frame}: n={} metrics={measure:?}", particles.len());
                reference.push(measure);
            }
        }
        println!("native solver wall median {:.3}ms", median(wall_ms));
        for density in [false, true] {
            let mut run = run_with_retired_speed(WaterScene { volume_projection: density, ..scene }, true);
            // The seed-only frame has zero velocity/history, no accepted
            // steps and no births. Replace its persistent particle state,
            // including radius/id, while retaining zero unused capacity.
            // The step iterates capacity and skips nonpositive live radii.
            let mut common = vec![FluidParticle { position_radius: [0.0; 4], velocity: [0.0; 3], id: 0 }; scene.particles() as usize];
            common[..seed.len()].copy_from_slice(&seed);
            let state = output_of(&run.plan, node_named(&run.graph, "state"), "out");
            let buffer = run.exec.host_array_buffer(&run.graph, &run.plan, state).expect("dedicated particle state");
            // SAFETY: fill frame completed; dedicated shared state is large
            // enough and no GPU command is outstanding.
            assert!(buffer.size as usize >= std::mem::size_of_val(common.as_slice()));
            unsafe { buffer.write(0, bytemuck::cast_slice(&common)); }
            assert_eq!(bytemuck::cast_slice::<_, u8>(&run.particles()), bytemuck::cast_slice::<_, u8>(&common));
            let (mut gpu_ms, mut encode_ms) = (Vec::new(), Vec::new());
            let (mut capped, mut refused, mut unconverged) = (0u64, 0u64, 0u64);
            for frame in 1..=90 {
                assert!(started.elapsed().as_secs() < 120, "bounded reference probe exceeded 120s");
                let (gpu, cpu) = run.timed_frame();
                assert!(gpu.is_finite() && gpu > 0.0 && cpu.is_finite());
                gpu_ms.push(gpu);
                encode_ms.push(cpu);
                let stats = run.liquid_stats();
                assert_eq!(stats.nonfinite, 0);
                if frame == 1 { assert_eq!(stats.live as usize, seed.len(), "first tick consumes native live count"); }
                let clock: Vec<u32> = run.read(STEP_NODE, "clock_status", 8);
                assert_eq!(clock[1], (1.0f32 / 60.0).to_bits(), "complete reference interval");
                assert_eq!(clock[2], 0, "no remaining time");
                assert_eq!(clock[6], 1, "same single substep as native");
                assert_eq!((clock[4], clock[5]), (0, 0), "no clock cap or invalid input");
                capped += u64::from(stats.speed_capped);
                refused += u64::from(stats.push_refused);
                unconverged += u64::from(stats.unconverged);
                if frame % 30 == 0 {
                    let particles = run.particles();
                    let measure = measures(&particles);
                    let delta: [f64; 5] = std::array::from_fn(|a| measure[a] - reference[frame / 30 - 1][a]);
                    println!("gpu density={density} frame {frame}: n={} metrics={measure:?} delta={delta:?}", stats.live);
                }
            }
            println!("gpu density={density}: gpu median {:.3}ms encode median {:.3}ms capped={capped} refused={refused} unconverged={unconverged}", median(gpu_ms), median(encode_ms));
        }
    }

    /// One wall or contact case run from identical native-captured seeds.
    #[cfg(feature = "water-race-probes")]
    struct WallCase {
        name: &'static str,
        /// World-space water box and its velocity.
        water: [[f32; 3]; 2],
        velocity: [f32; 3],
        /// Static box obstacle: centre, then size, world metres.
        obstacle: Option<[[f64; 3]; 2]>,
        /// The wall-contact region measured for run-up and contact counts.
        near: fn([f32; 3], f32) -> bool,
    }

    /// Wall and contact parity (BUG-g75v.17 (GPU FLIP wall and collision
    /// motion parity)): flat wall, two- and three-face corners, a body flush
    /// with a wall, and water leaving a wall, each seeded on GPU from the
    /// native engine's captured particles and stepped 60 frames at 16³.
    /// Prints per-checkpoint native and GPU metrics; asserts only that both
    /// engines stay finite and inside the tank.
    ///
    /// Measured 2026-10-05 at 16³, frame 60 (native vs GPU): particle counts
    /// equal in every case and no GPU caps, refusals or unconverged solves;
    /// both hold water 0.10h off every wall. Water stopped by a body agrees:
    /// the tank-spanning body slab's run-up is 1.62 vs 1.47 m, mean x within
    /// 0.02 m, rebound vx -0.58 vs -0.69 m/s; the body flush with a wall,
    /// run-up 1.01 vs 1.19 m. The same impact on the tank wall does not:
    /// run-up 1.24 vs 2.19 m, the two-face corner's 1.43 vs 3.68 m, until
    /// liquid φ extended into the walls without bodies too (BUG-9p3ms (tank
    /// walls give far more run-up than native)): now 1.24 vs 1.37 m and
    /// 1.43 vs 2.06 m. Still open: water within a cell of the flat wall,
    /// 428 vs 736; and before any wall contact the GPU slab keeps vx
    /// 3.00 m/s where native slows to 2.84.
    #[cfg(feature = "water-race-probes")]
    #[test]
    fn gpu_flip_native_wall_contact_reference() {
        let wall = 2.0f32;
        let cases = [
            WallCase { name: "flat_wall", water: [[-1.0, 0.0, -2.0], [0.0, 1.0, 2.0]], velocity: [3.0, 0.0, 0.0], obstacle: None,
                near: |p, h| p[0] > 2.0 - h },
            WallCase { name: "corner_two_face", water: [[0.5, 0.0, 0.5], [1.5, 1.5, 1.5]], velocity: [3.0, 0.0, 3.0], obstacle: None,
                near: |p, h| p[0] > 2.0 - h && p[2] > 2.0 - h },
            WallCase { name: "corner_three_face", water: [[0.5, 1.5, 0.5], [1.5, 2.5, 1.5]], velocity: [3.0, -3.0, 3.0], obstacle: None,
                near: |p, h| p[0] > 2.0 - 2.0 * h && p[2] > 2.0 - 2.0 * h && p[1] < 2.0 * h },
            WallCase { name: "body_flush_wall", water: [[-1.0, 0.0, -2.0], [0.0, 1.0, 2.0]], velocity: [3.0, 0.0, 0.0],
                obstacle: Some([[1.7, 0.58, 0.0], [0.6, 1.16, 0.85]]), near: |p, h| p[0] > 1.4 - h && p[2].abs() < 0.425 + h },
            // The flat wall's impact with a tank-spanning body as the wall,
            // to tell a domain-wall difference from a general one.
            WallCase { name: "body_slab_wall", water: [[-1.0, 0.0, -2.0], [0.0, 1.0, 2.0]], velocity: [3.0, 0.0, 0.0],
                obstacle: Some([[1.875, 2.0, 0.0], [0.75, 4.2, 4.2]]), near: |p, h| p[0] > 1.5 - h },
            WallCase { name: "separation", water: [[1.0, 0.0, -2.0], [2.0, 2.0, 2.0]], velocity: [-2.0, 0.0, 0.0], obstacle: None,
                near: |p, h| p[0] > 2.0 - h },
        ];
        let scene = WaterScene::race_dam_break(16);
        let h = scene.cell_size() as f32;
        let layout = scene.layout();
        let offset = layout.min.map(|v| v - 1.5 * h);
        let local = |p: [f32; 3]| -> [f32; 3] { std::array::from_fn(|a| p[a] - offset[a]) };
        // [n, mean x, mean y, mean z, mean vx, mean vy, mean vz, rms speed,
        //  near count, near run-up y max, y99, deepest wall gap / h,
        //  deepest body depth / h]
        let measure = |case: &WallCase, particles: &[FluidParticle]| -> [f64; 13] {
            let live: Vec<_> = particles.iter().filter(|p| p.position_radius[3] > 0.0).collect();
            assert!(!live.is_empty());
            assert!(live.iter().all(|p| p.position_radius.iter().chain(&p.velocity).all(|v| v.is_finite())), "{}: finite", case.name);
            let n = live.len() as f64;
            let mean = |f: &dyn Fn(&FluidParticle) -> f32| live.iter().map(|p| f64::from(f(p))).sum::<f64>() / n;
            let near: Vec<_> = live.iter().filter(|p| (case.near)([p.position_radius[0], p.position_radius[1], p.position_radius[2]], h)).collect();
            let mut ys: Vec<f64> = live.iter().map(|p| f64::from(p.position_radius[1])).collect();
            ys.sort_by(f64::total_cmp);
            let gap = live.iter().map(|p| {
                let q = p.position_radius;
                (wall - q[0].abs()).min(wall - q[2].abs()).min(q[1]).min(2.0 * wall - q[1])
            }).fold(f32::INFINITY, f32::min);
            let depth = case.obstacle.map_or(0.0, |[c, s]| live.iter().map(|p| {
                let q = p.position_radius;
                (0..3).map(|a| 0.5 * s[a] - (f64::from(q[a]) - c[a]).abs()).fold(f64::INFINITY, f64::min)
            }).fold(f64::NEG_INFINITY, f64::max));
            [
                n, mean(&|p| p.position_radius[0]), mean(&|p| p.position_radius[1]), mean(&|p| p.position_radius[2]),
                mean(&|p| p.velocity[0]), mean(&|p| p.velocity[1]), mean(&|p| p.velocity[2]),
                mean(&|p| p.velocity.iter().map(|v| v * v).sum::<f32>()).sqrt(),
                near.len() as f64, near.iter().map(|p| f64::from(p.position_radius[1])).fold(0.0, f64::max),
                ys[ys.len() * 99 / 100], f64::from(gap / h), depth / f64::from(h),
            ]
        };
        let started = std::time::Instant::now();
        println!("metrics: n, mean xyz (m), mean v xyz (m/s), rms speed, near count, near run-up y max, y99, min wall gap/h, deepest body depth/h");
        for case in &cases {
            let mut native = FluidWorld::new_seeded(Config {
                cells: layout.cells.map(|n| n + 3), cell_size: layout.cell_size, surface_subdivisions: 0, apic: false,
            }, 0).expect("native world");
            native.set_surface_reconstruction_enabled(false).expect("disable meshing");
            native.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
            native.add_fluid_box(Bounds { min: local(case.water[0]), max: local(case.water[1]) }, case.velocity).expect("water");
            if let Some([c, s]) = case.obstacle {
                let bounds = Bounds {
                    min: local(std::array::from_fn(|a| (c[a] - 0.5 * s[a]) as f32)),
                    max: local(std::array::from_fn(|a| (c[a] + 0.5 * s[a]) as f32)),
                };
                native.set_obstacle(bounds, bounds, bounds).expect("obstacle");
            }
            native.step(Seconds(1.0 / 60.0)).expect("insert native seed");
            let seed = capture(&mut native, offset);
            assert!(seed.len() <= scene.particles() as usize, "{}: seed fits GPU capacity", case.name);
            let mut reference = Vec::new();
            for frame in 1..=60 {
                let stats = native.step(Seconds(1.0 / 60.0)).expect("native step");
                assert!(!stats.numerical_recovery, "{}: native recovery", case.name);
                if frame % 10 == 0 {
                    let m = measure(case, &capture(&mut native, offset));
                    println!("{} native frame {frame} substeps {}: {m:.3?}", case.name, stats.substeps);
                    reference.push(m);
                }
            }
            let gpu_scene = if case.obstacle.is_some() { scene.with_obstacle() } else { scene };
            let mut run = match case.obstacle {
                Some([c, s]) => Run::posed(gpu_scene, &[
                    ("obstacle_transform", "pos_x", c[0]), ("obstacle_transform", "pos_y", c[1]), ("obstacle_transform", "pos_z", c[2]),
                    ("obstacle_transform", "scale_x", s[0]), ("obstacle_transform", "scale_y", s[1]), ("obstacle_transform", "scale_z", s[2]),
                ]),
                None => Run::new(gpu_scene),
            };
            let mut common = vec![FluidParticle { position_radius: [0.0; 4], velocity: [0.0; 3], id: 0 }; gpu_scene.particles() as usize];
            common[..seed.len()].copy_from_slice(&seed);
            let state = output_of(&run.plan, node_named(&run.graph, "state"), "out");
            let buffer = run.exec.host_array_buffer(&run.graph, &run.plan, state).expect("dedicated particle state");
            assert!(buffer.size as usize >= std::mem::size_of_val(common.as_slice()));
            // SAFETY: the fill frame completed; no GPU command is outstanding.
            unsafe { buffer.write(0, bytemuck::cast_slice(&common)); }
            let (mut capped, mut refused, mut unconverged) = (0u64, 0u64, 0u64);
            for frame in 1..=60 {
                assert!(started.elapsed().as_secs() < 240, "bounded wall reference exceeded 240s");
                run.frame();
                let stats = run.liquid_stats();
                assert_eq!(stats.nonfinite, 0);
                capped += u64::from(stats.speed_capped);
                refused += u64::from(stats.push_refused);
                unconverged += u64::from(stats.unconverged);
                if frame % 10 == 0 {
                    let m = measure(case, &run.particles());
                    let r = reference[frame / 10 - 1];
                    let delta: [f64; 13] = std::array::from_fn(|a| m[a] - r[a]);
                    println!("{} gpu    frame {frame}: {m:.3?}", case.name);
                    println!("{} delta  frame {frame}: {delta:.3?}", case.name);
                    assert!(m[11] > -0.01, "{}: GPU water left the tank by {:.3}h", case.name, -m[11]);
                }
            }
            println!("{}: gpu capped={capped} refused={refused} unconverged={unconverged}", case.name);
        }
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
    let raw: Vec<crate::generators::mesh_common::MeshVertex> =
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

const LID_SPEED: f64 = 1.0;
const LID_THICKNESS: f64 = 0.4;

/// The mean -x speed of the water two to four cells in from the low-x face
/// over the last ten frames, and the speed the lid's displacement would
/// drive through that face: lid area times lid speed over the open area;
/// and the volume rate taken off sealed pockets' pressure solves, summed.
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
        run.graph.set_param(transform, "pos_y", crate::node_graph::ParamValue::Float(y as f32)).expect("pos_y");
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

/// Encode replay changes nothing the step computes: 300 Dam Break ticks
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
    for frame in 1..=300 {
        direct.frame();
        replay.frame();
        let (dp, rp) = (direct.particles(), replay.particles());
        assert!(bytemuck::cast_slice::<_, u8>(&dp) == bytemuck::cast_slice::<_, u8>(&rp), "tick {frame}: the particles differ with replay on");
        let (df, rf) = (direct.faces(), replay.faces());
        assert!(bytemuck::cast_slice::<_, u8>(&df) == bytemuck::cast_slice::<_, u8>(&rf), "tick {frame}: the faces differ with replay on");
        assert_eq!(direct.solver(), replay.solver(), "tick {frame}: the solver words differ with replay on");
        let stats = replay.replay_stats();
        let delta = |take: fn(&manifold_gpu::GpuReplayStats) -> u64| take(&stats) - take(&last);
        if frame <= WARM || frame % 100 == 0 || delta(|s| s.recorded) > 0 {
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

/// Rank one settled 64³ step's stages; sampling changes encoder layout,
/// so its total is attribution data rather than ordinary frame performance.
#[cfg(feature = "water-race-probes")]
#[test]
fn gpu_flip_one_step_stage_cost_probe() {
    const WARMUP: usize = 4;
    let scene = WaterScene::still_pool(64).with_steps(1);
    let mut profiled = run_with_retired_speed(scene, true);
    let mut plain = run_with_retired_speed(scene, true);
    profiled.set_encode_replay(true);
    plain.set_encode_replay(true);
    for _ in 0..WARMUP {
        profiled.frame();
        plain.frame();
    }
    assert_eq!(profiled.frames, plain.frames, "same time before the sampled frame");
    let profile = objc2::rc::autoreleasepool(|_| profiled.sampled_frame(manifold_gpu::ProfileGranularity::Tag));
    plain.frame();
    assert_eq!(profiled.frames, plain.frames, "same time after the sampled frame");

    let mut by_label: std::collections::BTreeMap<String, (f64, usize, usize)> = std::collections::BTreeMap::new();
    for span in &profile.spans {
        let label = if span.tag.starts_with("gpu_flip.stage.") { &span.tag } else { &span.label };
        let row = by_label.entry(label.clone()).or_default();
        row.0 += span.millis;
        row.1 += usize::from(span.kind == manifold_gpu::GpuWorkKind::Compute);
        row.2 += 1;
    }
    let mut rows: Vec<_> = by_label.into_iter().collect();
    rows.sort_by(|(a_label, (a_ms, _, _)), (b_label, (b_ms, _, _))|
        b_ms.total_cmp(a_ms).then_with(|| a_label.cmp(b_label)));
    let attributed_ms = profile.attributed_ms();
    let compute_encoders: usize = rows.iter().map(|(_, row)| row.1).sum();
    println!(
        "GPU FLIP 64³ one-step stage attribution, {WARMUP} warmup + 1 sampled frame: profiled total {:.3} ms; attributed {:.3} ms; unresolved {:.3} ms; {} spans, {compute_encoders} compute encoders; overflow {}, invalid {}, failed command buffers {}",
        profile.total_ms, attributed_ms, profile.total_ms - attributed_ms, profile.spans.len(),
        profile.overflow, profile.invalid, profile.failed_command_buffers,
    );
    for (label, (ms, count, spans)) in &rows {
        println!("GPU FLIP 64³ one-step: {label:<48} {ms:9.3} ms; {count} compute encoders, {spans} spans");
    }
    assert_eq!(profile.failed_command_buffers, 0, "sampled command buffers completed");
    assert_eq!(profile.overflow, 0, "every encoded span must fit the timestamp sampler");
    assert_eq!(profile.invalid, 0, "every encoded span must resolve valid samples");
    assert!(profile.total_ms.is_finite() && profile.total_ms > 0.0, "finite positive profiled total");
    assert!(attributed_ms.is_finite() && attributed_ms > 0.0 && compute_encoders > 0, "compute work must be attributed");
    for span in &profile.spans {
        assert!(!span.label.is_empty(), "every span has a dispatch/pass label");
        assert!(span.start_ms.is_finite() && span.millis.is_finite() && span.millis >= 0.0,
            "{}: valid resolved timing", span.label);
    }

    for (label, run) in [("profiled", &profiled), ("plain", &plain)] {
        let status: Vec<u32> = run.read(STEP_NODE, "clock_status", 8);
        assert_eq!(status[0], (1.0f32 / 60.0).to_bits(), "{label}: final slot is the one active step");
        assert_eq!(status[1], (1.0f32 / 60.0).to_bits(), "{label}: exact completed time");
        assert_eq!(status[2], 0.0f32.to_bits(), "{label}: interval is complete");
        assert_eq!(status[6], 1, "{label}: exactly one accepted step");
        assert_eq!((status[4], status[5]), (0, 0), "{label}: no cap or nonfinite clock input");
        let stats = particle_stats(&run.particles());
        assert_eq!((stats.live, stats.bad), (run.seeded, 0), "{label}: all water remains live and finite");
    }
    let (pp, up) = (profiled.particles(), plain.particles());
    assert!(bytemuck::cast_slice::<_, u8>(&pp) == bytemuck::cast_slice::<_, u8>(&up), "published particle bits differ");
    let pp: Vec<FluidParticle> = profiled.read(STEP_NODE, "out", scene.particles() as usize);
    let up: Vec<FluidParticle> = plain.read(STEP_NODE, "out", scene.particles() as usize);
    assert!(bytemuck::cast_slice::<_, u8>(&pp) == bytemuck::cast_slice::<_, u8>(&up), "step particle bits differ");
    let (pf, uf) = (profiled.faces(), plain.faces());
    assert!(bytemuck::cast_slice::<_, u8>(&pf) == bytemuck::cast_slice::<_, u8>(&uf), "final face bits differ");
    let ps: Vec<u32> = profiled.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
    let us: Vec<u32> = plain.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
    assert_eq!(ps, us, "full liquid stats differ");
    let capped_words = 2 * scene.particles() as usize + SOLVER_WORDS as usize;
    let pc: Vec<u32> = profiled.read(STEP_NODE, "capped", capped_words);
    let uc: Vec<u32> = plain.read(STEP_NODE, "capped", capped_words);
    assert_eq!(pc, uc, "full capped words differ");
}

/// The cost of recording inactive slots, with completed time and all final
/// state proved identical. Readbacks happen after `frame`'s timing ends.
#[test]
fn gpu_flip_one_active_slot_matches_six_recorded_slots_cost_proof() {
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
            super::gpu_flip_step::set_force_solid_velocity(false);
        }
    }
    fn original_frame(run: &mut Run) -> (f64, f64) {
        super::gpu_flip_step::set_force_solid_velocity(true);
        let _reset = ForceSolidVelocity;
        run.timed_frame()
    }
    fn median(values: &mut [f64]) -> f64 {
        values.sort_by(f64::total_cmp);
        (values[values.len() / 2 - 1] + values[values.len() / 2]) * 0.5
    }
    super::gpu_flip_step::set_force_solid_velocity(false);
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
            super::gpu_flip_step::set_force_indirect_pockets(false);
        }
    }
    fn original_frame(run: &mut Run) {
        super::gpu_flip_step::set_force_indirect_pockets(true);
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
    super::gpu_flip_step::set_force_indirect_pockets(false);
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
                run.graph.set_param(step, "steps", crate::node_graph::ParamValue::Float(1.0)).expect("steps");
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
            super::gpu_flip_step::set_force_dense_extend(false);
        }
    }
    fn original_frame(run: &mut Run) -> (f64, f64) {
        super::gpu_flip_step::set_force_dense_extend(true);
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
    super::gpu_flip_step::set_force_dense_extend(false);
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
            super::gpu_flip_step::set_force_dense_extend(false);
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
            super::gpu_flip_step::set_force_dense_extend(false);
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
                run.graph.set_param(step, "steps", crate::node_graph::ParamValue::Float(3.0)).unwrap();
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

/// The speed pass's measure, Steps 1 (BUG-l2h3.24): the Dam Break and the
/// still pool at 64 and the Dam Break at 128, 300 frames each, under Auto and Fixed(16), whose gap is
/// the cost of Auto's recorded but gated-off iterations. Every tenth frame
/// is timestamped; the rest give the plain
/// GPU frame and the CPU encode. Prints medians, the solver's iterations a
/// solve, and the per-label split with each label's dispatches a frame.
#[cfg(feature = "water-race-probes")]
#[test]
fn gpu_flip_speed_measure() {
    fn median(mut v: Vec<f64>) -> f64 {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    }
    let scenes = [
        ("dam break", WaterScene::dam_break(64).with_steps(1)),
        ("still pool", WaterScene::still_pool(64).with_steps(1)),
        ("dam break 128", WaterScene::dam_break(128).with_steps(1)),
    ];
    for (name, scene) in scenes {
        for (mode, scene) in [("auto", scene), ("fixed16", scene.with_iterations(16))] {
            let mut run = Run::new(scene);
            let (mut gpu, mut cpu, mut stamped) = (Vec::new(), Vec::new(), Vec::new());
            let (mut pressure, mut density, mut unconverged) = (Vec::new(), Vec::new(), 0u32);
            let mut labels: Vec<(String, Vec<f64>, Vec<f64>)> = Vec::new();
            for frame in 0..300 {
                if frame % 10 == 9 {
                    // Each timestamped frame's counter buffers are released
                    // here, not at the test's end: the device holds few.
                    let (by_label, total) = objc2::rc::autoreleasepool(|_| run.profiled_labels());
                    stamped.push(total);
                    for (label, ms, count) in by_label {
                        match labels.iter_mut().find(|(l, _, _)| *l == label) {
                            Some(row) => {
                                row.1.push(ms);
                                row.2.push(count as f64);
                            }
                            None => labels.push((label, vec![ms], vec![count as f64])),
                        }
                    }
                } else {
                    let (g, c) = run.frame();
                    gpu.push(g);
                    cpu.push(c);
                }
                let [p, d, u] = run.solver();
                pressure.push(f64::from(p));
                density.push(f64::from(d));
                unconverged += u;
            }
            let span = |v: &[f64]| (v.iter().copied().fold(f64::MAX, f64::min), median(v.to_vec()), v.iter().copied().fold(0.0, f64::max));
            let (sp, sd) = (span(&pressure), span(&density));
            let dispatches: f64 = labels.iter().map(|(_, _, c)| median(c.clone())).sum();
            let solve_dispatches: f64 =
                labels.iter().filter(|(l, _, _)| l.starts_with("gpu_flip.pressure.")).map(|(_, _, c)| median(c.clone())).sum();
            println!(
                "SPEED {name} {mode}: GPU plain {:.2} ms, timestamped {:.2} ms, CPU encode {:.2} ms; {dispatches} dispatches a frame, {solve_dispatches} in the solves",
                median(gpu),
                median(stamped),
                median(cpu)
            );
            println!(
                "SPEED {name} {mode}: iterations a solve, pressure {} / {} / {}, density {} / {} / {} (min / median / max), unconverged {unconverged}",
                sp.0, sp.1, sp.2, sd.0, sd.1, sd.2
            );
            let mut rows: Vec<(String, f64, f64)> = labels.into_iter().map(|(l, ms, c)| (l, median(ms), median(c))).collect();
            rows.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (label, ms, count) in rows.iter().take(30) {
                println!("SPEED {name} {mode}:   {label:<40} x{count:<5} {ms:8.3} ms");
            }
        }
    }
}

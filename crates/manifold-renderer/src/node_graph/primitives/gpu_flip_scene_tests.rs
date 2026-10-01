//! The GPU FLIP water step run on whole scenes (docs/GPU_FLIP_PRESSURE_SOLVE.md
//! section 1 (the step)): the momentum, still-pool and meshed-volume proofs, and the scene
//! runner the race probes (`gpu_flip_race_tests`) share.
//! `gpu_flip_scenes_cover_every_dispatch` proves every array these graphs
//! allocate before any of them runs here.

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;

use super::gpu_flip_preset::{FACE_NODES, REST_PER_CELL, WaterScene, water_def};
use crate::node_graph::liquid::grid::face_len;
use super::gpu_flip_volume::{VolumeDrift, volume_and_area};
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::fluid_particles::{FaceSample, FluidParticle};
use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
use crate::node_graph::{
    EffectGraphDefExt, ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, NodeInstanceId, PrimitiveRegistry,
    ResourceId, StateStore, compile, pre_allocate_resources,
};

const G: f64 = 9.81;

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
    /// The particles the frame's tick started from: the state's, read
    /// before the frame runs.
    entering: Vec<FluidParticle>,
}

impl Run {
    pub(super) fn new(scene: WaterScene) -> Self {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        Self::with_graph(scene, water_def(scene).into_graph(&registry, &Default::default()).expect("water def builds"))
    }

    fn with_graph(scene: WaterScene, mut graph: Graph) -> Self {
        // Every array read after a frame keeps its own storage.
        let mut read = vec![(node_named(&graph, "state"), "out")];
        for k in 0..scene.steps {
            let step = node_named(&graph, &format!("s{k}.step"));
            read.extend([(step, "out"), (step, "faces")]);
        }
        if scene.surface {
            read.push((node_ending(&graph, "liquid_offsets"), "extent"));
            read.push((node_ending(&graph, "liquid_mesh"), "vertices"));
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
        pre_allocate_resources(&graph, &plan, &device, &mut backend).expect("pre-allocate");
        let exec = Executor::new(Box::new(backend));
        let mut run = Self { device, graph, plan, exec, state: StateStore::new(), scene, frames: 0, entering: Vec::new() };
        // The domain's clock restarts on its first frame and ticks none: the
        // state takes the fill. Every later frame is one tick.
        run.frame();
        run
    }

    /// The surface mesh's live triangles.
    pub(super) fn surface(&self) -> Vec<[[f32; 3]; 3]> {
        // The running total's `extent` starts with the grand total: triangles.
        let extent: Vec<u32> = self.read_at(node_ending(&self.graph, "liquid_offsets"), "extent", 1);
        let vertices: Vec<crate::generators::mesh_common::MeshVertex> =
            self.read_at(node_ending(&self.graph, "liquid_mesh"), "vertices", 3 * extent[0] as usize);
        vertices.chunks_exact(3).map(|t| [0, 1, 2].map(|i| t[i].position)).collect()
    }

    /// The volume the surface mesh holds in the tank and its free surface's area.
    pub(super) fn surface_measure(&self) -> (f64, f64) {
        volume_and_area(self.surface().into_iter(), self.scene.min(), super::gpu_flip_preset::BOX_METRES)
    }

    /// The particles' own volume: `REST_PER_CELL` fill a cell.
    pub(super) fn particle_volume(&self) -> f64 {
        self.scene.particles() as f64 * self.scene.pressure.cell_size().powi(3) / REST_PER_CELL
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
                seconds: Seconds(self.frames as f64 / 60.0),
                delta: Seconds(1.0 / 60.0),
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

    /// One frame in its own command buffer: GPU ms and CPU encode ms.
    pub(super) fn frame(&mut self) -> (f64, f64) {
        self.entering = self.particles();
        let mut enc = self.device.create_encoder("gpu-flip-scene");
        let cpu_ms;
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            let time = FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(self.frames as f64 / 60.0),
                delta: Seconds(1.0 / 60.0),
                frame_count: self.frames,
            };
            let start = std::time::Instant::now();
            self.exec.execute_frame_with_state(&mut self.graph, &self.plan, time, &mut gpu, &mut self.state, 0);
            cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
            self.frames += 1;
        }
        let profile = enc.commit_and_wait_profiled(&self.device);
        assert_eq!(profile.failed_command_buffers, 0, "frame {} failed on the GPU", self.frames - 1);
        (profile.total_ms, cpu_ms)
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
        self.scene.pressure.n
    }

    pub(super) fn particles(&self) -> Vec<FluidParticle> {
        self.read("state", "out", self.scene.particles() as usize)
    }

    /// The particles step `step` of the frame's tick moved.
    fn moved(&self, step: usize) -> Vec<FluidParticle> {
        self.read(&format!("s{step}.step"), "out", self.scene.particles() as usize)
    }

    /// The step's water cells, 1 or 0: the cells its particles started in,
    /// binned as the step's sort bins them (clamped into the lattice).
    pub(super) fn water(&self, step: usize) -> Vec<f32> {
        let started = if step == 0 { self.entering.clone() } else { self.moved(step - 1) };
        let (n, h, min) = (self.n(), self.scene.pressure.cell_size(), self.scene.min());
        let mut water = vec![0.0; n.pow(3)];
        for p in started.iter().filter(|p| p.position_radius[3] > 0.0) {
            let c: [usize; 3] = std::array::from_fn(|a| ((f64::from(p.position_radius[a]) - min[a]) / h).floor().clamp(0.0, (n - 1) as f64) as usize);
            water[c[0] + n * (c[1] + n * c[2])] = 1.0;
        }
        water
    }

    /// The step's face grid: projected, constrained to the solids and
    /// extended.
    pub(super) fn faces(&self, step: usize) -> Vec<FaceSample> {
        self.read(&format!("s{step}.step"), "faces", (self.n() + 1).pow(3))
    }

    /// The seam face grid of the frame's last tick, x, y and z (a scene built
    /// with `faces`).
    pub(super) fn face_grid(&self) -> [Vec<f32>; 3] {
        let cells = [self.n() as u32; 3];
        std::array::from_fn(|axis| self.read(FACE_NODES[axis], "out", face_len(cells, axis) as usize))
    }

    /// Water cells in the step's lattice: the size of its pressure solve.
    pub(super) fn water_cells(&self, step: usize) -> u32 {
        self.water(step).iter().filter(|&&w| w > 0.5).count() as u32
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
        run.water_cells(scene.steps - 1)
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
        let records = (n + 1).pow(3);
        let state: Vec<FaceSample> = run.read("state", "faces", records);
        let last = run.faces(scene.steps - 1);
        let differ = state.iter().zip(&last).filter(|(a, b)| bytemuck::bytes_of(*a) != bytemuck::bytes_of(*b)).count();
        let moving = state.iter().filter(|s| s.velocity.iter().any(|v| *v != 0.0)).count();
        let grid = run.face_grid();
        let expected = crate::node_graph::liquid::conformance::gpu_flip_faces(bytemuck::cast_slice(&state), [n as u32; 3]);
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
    let mut fastest = Vec::new();
    for frame in 0..120 {
        run.frame();
        if frame % 10 == 9 {
            let stats = particle_stats(&run.particles());
            let last = scene.steps - 1;
            let (rms, max) = divergence(&run.faces(last), &run.water(last), run.n(), scene.pressure.cell_size());
            println!(
                "GPU FLIP still pool {}³ frame {frame:3}: fastest {:.2e} m/s, mean height {:.5} m, divergence rms {rms:.2e} max {max:.2e} /s, water cells {}",
                run.n(),
                stats.fastest,
                stats.mean_height,
                run.water_cells(last)
            );
            assert_eq!((stats.live, stats.bad), (scene.particles() as usize, 0), "frame {frame}: particles lost or not finite");
            fastest.push(stats.fastest);
        }
    }
    let end = *fastest.last().expect("sampled");
    assert!(end < 1e-3, "fastest particle {end} m/s after 2 s");
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

//! The GPU FLIP water step run on whole scenes (docs/GPU_FLIP_PRESSURE_SOLVE.md
//! section 1 (the step)): the momentum, still-pool and meshed-volume proofs, and the scene
//! runner the race probes (`gpu_flip_race_tests`) share.
//! `gpu_flip_scenes_cover_every_dispatch` proves every array these graphs
//! allocate before any of them runs here.

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;

use super::gpu_flip_preset::{BOX_METRES, DAM_COLUMN, DAM_FILL_HEIGHT, DAM_OBSTACLE, FACE_NODES, REST_PER_CELL, STEP_NODE, WaterScene, water_def};
use crate::node_graph::liquid::grid::face_len;
use super::gpu_flip_volume::{VolumeDrift, volume_and_area};
use super::liquid_stats::{LIQUID_STATS_WORDS, LiquidTickStats};
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
        let step = node_named(&graph, STEP_NODE);
        read.extend([(step, "out"), (step, "faces"), (step, "capped"), (node_named(&graph, "stats"), "stats_out")]);
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
        let (n, h, min) = (self.n(), self.scene.pressure.cell_size(), self.scene.min());
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
        let records = (n + 1).pow(3);
        let state: Vec<FaceSample> = run.read("state", "faces", records);
        let last = run.faces();
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
            let (rms, max) = divergence(&run.faces(), &run.water(), run.n(), scene.pressure.cell_size());
            println!(
                "GPU FLIP still pool {}³ frame {frame:3}: fastest {:.2e} m/s, mean height {:.5} m, divergence rms {rms:.2e} max {max:.2e} /s, water cells {}",
                run.n(),
                stats.fastest,
                stats.mean_height,
                run.water_cells()
            );
            assert_eq!((stats.live, stats.bad), (scene.particles() as usize, 0), "frame {frame}: particles lost or not finite");
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
    let (n, h) = (run.n(), scene.pressure.cell_size());
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
        assert_eq!((stats.live, stats.bad), (scene.particles() as usize, 0), "frame {frame}: particles lost or not finite");
    }
}

/// Mean particles per cell over the interior water: cells that, with all
/// 26 neighbours, are φ < 0. Unlike particles per water cell, a growing
/// surface does not move it.
pub(super) fn interior_density(run: &Run, particles: &[FluidParticle]) -> f64 {
    let (n, h, min) = (run.n(), run.scene.pressure.cell_size(), run.scene.min());
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
    let (n, h, min) = (run.n(), run.scene.pressure.cell_size(), run.scene.min());
    let mut top = vec![f64::NEG_INFINITY; n * n];
    for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
        let c = |a: usize| ((f64::from(p.position_radius[a]) - min[a]) / h).floor().clamp(0.0, (n - 1) as f64) as usize;
        let at = c(0) + n * c(2);
        top[at] = top[at].max(f64::from(p.position_radius[1]));
    }
    top.iter().map(|&y| if y.is_finite() { y - floor + 0.25 * h } else { 0.0 }).sum::<f64>() / (n * n) as f64
}

/// The Dam Break keeps its volume: by frame 1800 (30 s) the pool's column
/// depth is the analytic volume, the 0.16 m pool plus the column, over the
/// 4 m × 4 m floor, within 3%, and the interior density is within 2% of its
/// start. Over the settling, frame 400 on, KE + PE never rises frame to
/// frame past the noise floor of `gpu_flip_dam_break_energy_never_rises`
/// (1e-4 of the start), and ends below where it stood at frame 400. A
/// position projection does a little work no force accounts for; this bounds
/// it at that floor.
#[test]
fn gpu_flip_dam_break_settles_to_its_volume() {
    const FRAMES: usize = 1800;
    const SETTLING: usize = 400;
    for steps in [1, 2] {
        let scene = WaterScene::dam_break(64).with_steps(steps);
        let mut run = Run::new(scene);
        let floor = scene.min()[1];
        let start = run.particles();
        let e0 = energy(&start, floor);
        let rho0 = interior_density(&run, &start);
        let column: f64 = DAM_COLUMN.iter().map(|[lo, hi]| hi - lo).product();
        let want = (DAM_FILL_HEIGHT * BOX_METRES * BOX_METRES + column) / (BOX_METRES * BOX_METRES);
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
        // the cap: at most 16 iterations a solve, as on the saved problems.
        println!("GPU FLIP energy {steps} steps: most iterations a tick, pressure {} density {}, unconverged solves {unconverged}", most[0], most[1]);
        assert_eq!(unconverged, 0, "{steps} steps: solves reached the cap");
        assert!(most.iter().all(|&m| m > 0 && m as usize <= 16 * steps), "{steps} steps: iterations a tick {most:?}");
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
            assert_eq!((stats.live, stats.bad), (scene.particles() as usize, 0), "frame {frame}: particles lost or not finite");
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
    let h = scene.pressure.cell_size();
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

/// A pool at rest round a static box resting on the tank floor stays at
/// rest: no particle is lost, the level holds and nothing moves faster
/// than 1 mm/s. The density solve made a waterline creep here (0.1 m/s at
/// its peak); without it the pool is still to rounding.
#[test]
fn gpu_flip_still_pool_rests_round_a_static_obstacle() {
    let scene = WaterScene::still_pool(64).with_obstacle();
    let mut run = Run::new(scene);
    let (_, live) = deepest_in_obstacle(&run.particles(), DAM_OBSTACLE[0]);
    // The level once the fill has settled.
    let mut level = None;
    let (mut peak, mut fastest) = (0.0_f64, 0.0);
    for frame in 0..120 {
        run.frame();
        if frame % 20 == 19 {
            let stats = particle_stats(&run.particles());
            println!("GPU FLIP pool round a box frame {frame:3}: {} live, fastest {:.2e} m/s, mean height {:.5} m", stats.live, stats.fastest, stats.mean_height);
            assert_eq!((stats.live, stats.bad), (live, 0), "frame {frame}: particles lost or not finite");
            let level = *level.get_or_insert(stats.mean_height);
            assert!((stats.mean_height - level).abs() < 1e-4, "frame {frame}: the level moved from {level} to {}", stats.mean_height);
            fastest = stats.fastest;
            peak = peak.max(fastest);
        }
    }
    assert!(peak < 1e-3, "the pool round the box reached {peak} m/s, last {fastest} m/s");
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
    let h = scene.pressure.cell_size();
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

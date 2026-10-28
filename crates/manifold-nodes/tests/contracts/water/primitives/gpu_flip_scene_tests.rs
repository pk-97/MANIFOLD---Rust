//! The GPU FLIP water step run on whole scenes: the momentum, still-pool and level-set-volume proofs, and the scene
//! runner the other water scene proofs share.
//! `gpu_flip_scenes_cover_every_dispatch` proves every array these graphs
//! allocate before any of them runs here.

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;

#[cfg(test)]
use manifold_nodes_water::presets::gpu_flip::REST_PER_CELL;
#[cfg(test)]
use manifold_water_gpu_flip::primitives::gpu_flip_volume::VolumeDrift;
#[cfg(test)]
use manifold_water_liquid::primitives::liquid_stats::{LIQUID_STATS_WORDS, SOLVER_WORDS};

use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::particles::FluidParticle;
#[cfg(test)]
use manifold_node_engine::scene::world_tiles::{CANON_ELEMENTS, TILE_CELLS, pool_len, table_words};
use manifold_node_engine::testkit::substep_nodes::register_substep_test_nodes;
use manifold_node_engine::{
    exec::effect_node::FrameTime, exec::execution::Executor, exec::execution_plan::ExecutionPlan,
    exec::execution_plan::compile, exec::metal_backend::MetalBackend, graph::Graph,
    load::graph_loader::pre_allocate_resources, persistence::EffectGraphDefExt,
    persistence::PrimitiveRegistry, state_store::StateStore,
};
use manifold_nodes_water::presets::gpu_flip::{STEP_NODE, WaterScene, water_def};
use manifold_water_gpu_flip::primitives::gpu_flip_step::BOX_BAND_CELLS;
#[cfg(test)]
use manifold_water_liquid::fluid_particles::FaceSample;
#[cfg(test)]
use manifold_water_liquid::testkit::world_box::from_pool_under;
#[cfg(test)]
use manifold_water_liquid::tile_pool::solver_origin_cell;

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
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        };
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
fn node_ending(
    graph: &Graph,
    name: &str,
) -> manifold_node_engine::exec::effect_node::NodeInstanceId {
    let mut found = graph.nodes().filter(|n| n.node_id.as_str().ends_with(name));
    let node = found
        .next()
        .unwrap_or_else(|| panic!("no node ending {name}"));
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
        let mut graph = water_def(scene)
            .into_graph(&registry, &Default::default())
            .expect("water def builds");
        for &(node, name, value) in params {
            let node = node_named(&graph, node);
            graph
                .set_param(
                    node,
                    name,
                    manifold_node_engine::parameters::ParamValue::Float(value as f32),
                )
                .expect(name);
        }
        Self::with_graph(scene, graph)
    }

    fn with_graph(scene: WaterScene, mut graph: Graph) -> Self {
        // Every array read after a frame keeps its own storage.
        let mut read = vec![(node_named(&graph, "state"), "out")];
        let step = node_named(&graph, STEP_NODE);
        read.extend([
            (step, "out"),
            (step, "faces"),
            (step, "distance"),
            (step, "tiles"),
            (step, "capped"),
            (node_named(&graph, "stats"), "stats_out"),
        ]);
        if scene.surface {
            read.push((node_ending(&graph, drawn_level_set(scene)), "out"));
        }
        for &(node, port) in &read {
            graph
                .add_external_output(node, port)
                .expect("a read port exists");
        }
        let plan = compile(&graph).expect("water def compiles");
        let device = manifold_gpu::testkit::test_device();
        let mut backend = MetalBackend::new(device.arc(), 64, 64, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&mut graph, &plan, &device, &mut backend).expect("pre-allocate");
        let exec = Executor::new(Box::new(backend));
        let mut run = Self {
            device,
            graph,
            plan,
            exec,
            state: StateStore::new(),
            scene,
            frames: 0,
            fps: 60.0,
            entering: Vec::new(),
            gpu_time: None,
            seeded: 0,
        };
        // The domain's clock restarts on its first frame and ticks none: the
        // state takes the fill. Every later frame is one tick.
        run.frame();
        run.seeded = particle_stats(&run.particles()).live;
        run
    }

    /// The water's volume and free-surface area, read from the surface's level
    /// set (negative inside the water, a distance near the surface), every
    /// element of its pool: a free slot and the canonical element hold air
    /// three cells out, which adds nothing. Each node
    /// holds the share of its cell the water fills: a linear ramp over one node
    /// spacing across the zero level, so a rising surface moves the volume
    /// smoothly instead of a whole node layer at a time. The area is the same
    /// ramp's width times the node count near the surface.
    #[cfg(test)]
    pub(super) fn surface_measure(&self) -> (f64, f64) {
        assert_eq!(
            self.scene.surface_scale, 1,
            "the lattice read assumes the default surface scale"
        );
        let carry = node_ending(&self.graph, drawn_level_set(self.scene));
        let resource = output_of(&self.plan, carry, "out");
        let elements = self
            .exec
            .host_array_buffer(&self.graph, &self.plan, resource)
            .expect("the surface's level set is a host read")
            .size as usize
            / 4;
        assert_eq!(elements % 512, 1, "the level set holds a whole pool");
        let level_set: Vec<f32> = self.read_at(carry, "out", elements);
        let dx = self.scene.cell_size();
        let (mut filled, mut skin) = (0.0, 0.0);
        for &phi in &level_set {
            let phi = f64::from(phi) / dx;
            assert!(phi.is_finite(), "the level set holds a non-finite node");
            filled += (0.5 - phi).clamp(0.0, 1.0);
            skin += (1.0 - phi.abs()).max(0.0);
        }
        (filled * dx.powi(3), skin * dx * dx)
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
            self.exec.execute_frame_with_state(
                &mut self.graph,
                &self.plan,
                time,
                &mut gpu,
                &mut self.state,
                0,
            );
            cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
            self.frames += 1;
        }
        if timed {
            // Register the tap for the final nonempty chunk; the following
            // blocking completion waits behind every chunk on this queue.
            enc.commit_and_continue(&self.device);
        }
        let profile = enc.commit_and_wait_profiled(&self.device);
        assert_eq!(
            profile.failed_command_buffers,
            0,
            "frame {} failed on the GPU",
            self.frames - 1
        );
        let gpu_ms = if timed {
            self.gpu_time
                .as_ref()
                .expect("frame timing enabled")
                .millis()
        } else {
            profile.total_ms
        };
        (gpu_ms, cpu_ms)
    }

    pub(super) fn read<T: bytemuck::Pod>(&self, node: &str, port: &str, len: usize) -> Vec<T> {
        self.read_at(node_named(&self.graph, node), port, len)
    }

    fn read_at<T: bytemuck::Pod>(
        &self,
        node: manifold_node_engine::exec::effect_node::NodeInstanceId,
        port: &str,
        len: usize,
    ) -> Vec<T> {
        let resource = output_of(&self.plan, node, port);
        let buffer = self
            .exec
            .host_array_buffer(&self.graph, &self.plan, resource)
            .unwrap_or_else(|| {
                panic!("{node:?}.{port} is not declared as a host read in with_graph")
            });
        assert!(
            buffer.size as usize >= len * std::mem::size_of::<T>(),
            "{node:?}.{port} is shorter than {len} records"
        );
        let ptr = buffer.mapped_ptr().expect("shared storage");
        // SAFETY: the frame completed and the buffer holds `len` records.
        unsafe { std::slice::from_raw_parts(ptr.cast::<T>().cast_const(), len) }.to_vec()
    }

    pub fn n(&self) -> usize {
        self.window().1[0] as usize
    }

    /// The dense window the proofs read the step's pooled fields over: the
    /// box and its walls' band, from half the band below the box's first
    /// corner (min in metres, then cells per axis).
    fn window(&self) -> ([f32; 3], [u32; 3]) {
        let layout = self.scene.layout();
        let half_band = 0.5 * BOX_BAND_CELLS as f32 * layout.cell_size as f32;
        (
            layout.min.map(|v| v - half_band),
            layout.cells.map(|n| n + BOX_BAND_CELLS),
        )
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

    /// The state's live particles: the identity record's count (word 7).
    /// Slots past it hold whatever was there before, by design.
    pub(super) fn particles(&self) -> Vec<FluidParticle> {
        let live = self.read::<u32>("state", "identity", 8)[7];
        self.read("state", "out", live as usize)
    }

    /// The last tick's water cells, 1 or 0: φ < 0 over the particles it
    /// started from, so a cell whose centre is within √3·h/2 of one (the eps
    /// snap makes an exact touch water). Bodies are not counted. A later
    /// substep's start stays inside the node, so this reads Steps 1 only.
    pub fn water(&self) -> Vec<f32> {
        assert_eq!(
            self.scene.steps, 1,
            "a substep's water mask is read at Steps 1"
        );
        self.water_of(&self.entering)
    }

    /// The water cells, 1 or 0, of `particles`: φ < 0 as the step builds it.
    pub(super) fn water_of(&self, particles: &[FluidParticle]) -> Vec<f32> {
        let started = particles;
        let (n, h, min) = (
            self.n(),
            self.scene.cell_size(),
            self.window().0.map(f64::from),
        );
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
                        let d2: f64 = (0..3)
                            .map(|a| (min[a] + (cell[a] as f64 + 0.5) * h - q[a]).powi(2))
                            .sum();
                        if d2 <= radius * radius {
                            water[cell[0] as usize
                                + n * (cell[1] as usize + n * cell[2] as usize)] = 1.0;
                        }
                    }
                }
            }
        }
        water
    }

    /// The last tick's solver words: pressure and density iterations over
    /// every substep, and solves that reached the cap unconverged.
    #[cfg(test)]
    pub(super) fn solver(&self) -> [u32; 3] {
        let n = 2 * self.scene.particles() as usize;
        let words: Vec<u32> = self.read(STEP_NODE, "capped", n + 3);
        [words[n], words[n + 1], words[n + 2]]
    }

    /// The step's pool, `P` tiles, and the world cell of its window's
    /// first cell. The step runs on the pool its water fits, at most the
    /// domain's: its faces hold that pool and the canonical element.
    #[cfg(test)]
    fn pool(&self) -> (u32, [i32; 3]) {
        let faces = output_of(&self.plan, node_named(&self.graph, STEP_NODE), "faces");
        let records = self
            .exec
            .host_array_buffer(&self.graph, &self.plan, faces)
            .expect("the step's faces are a host read")
            .size
            / std::mem::size_of::<FaceSample>() as u64;
        let pool_tiles = (records - u64::from(CANON_ELEMENTS)) / u64::from(TILE_CELLS);
        (
            pool_tiles as u32,
            solver_origin_cell(self.window().0, self.scene.cell_size() as f32),
        )
    }

    /// The last substep's face grid: projected, constrained to the solids
    /// and extended, read from the step's pool through its world tile table
    /// into the dense `(n + 1)^3` records.
    #[cfg(test)]
    pub(super) fn faces(&self) -> Vec<FaceSample> {
        let cells = self.window().1;
        let (pool_tiles, origin) = self.pool();
        let (pool, _, table) = self.pooled_outputs();
        from_pool_under(&pool, &table, pool_tiles, origin, cells.map(|n| n + 1))
    }

    /// The step's pooled `faces` and `distance` as stored, canonical element
    /// last, and their world tile table (`table_words(P)` words).
    #[cfg(test)]
    pub(super) fn pooled_outputs(&self) -> (Vec<FaceSample>, Vec<f32>, Vec<u32>) {
        let (pool_tiles, _) = self.pool();
        let len = pool_len(pool_tiles) as usize;
        (
            self.read(STEP_NODE, "faces", len),
            self.read(STEP_NODE, "distance", len),
            self.read(STEP_NODE, "tiles", table_words(pool_tiles) as usize),
        )
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
    let mut stats = ParticleStats {
        live: 0,
        bad: 0,
        fastest: 0.0,
        mean_velocity: [0.0; 3],
        mean_height: 0.0,
    };
    for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
        stats.live += 1;
        if !(p
            .position_radius
            .iter()
            .chain(&p.velocity)
            .all(|v| v.is_finite()))
        {
            stats.bad += 1;
            continue;
        }
        let v = p.velocity.map(f64::from);
        stats.fastest = stats
            .fastest
            .max(v.iter().map(|c| c * c).sum::<f64>().sqrt());
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
    assert_eq!(
        (stats.live, stats.bad),
        (scene.particles() as usize, 0),
        "every particle lives and stays finite"
    );
    assert!(
        (stats.mean_velocity[1] - want).abs() <= 0.01 * want.abs(),
        "fall speed {} against {want}",
        stats.mean_velocity[1]
    );
    assert!(
        stats.mean_velocity[0]
            .abs()
            .max(stats.mean_velocity[2].abs())
            <= 0.01 * want.abs(),
        "no sideways drift"
    );
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
        println!(
            "GPU FLIP energy {steps} steps: most above E0 {:+.2e}",
            worst
        );
        // The splash frames converge on the engine's tolerance, well under
        // the cap, which is the physics claim asserted below.
        println!(
            "GPU FLIP energy {steps} steps: most iterations a tick, pressure {} density {}, unconverged solves {unconverged}",
            most[0], most[1]
        );
        assert_eq!(unconverged, 0, "{steps} steps: solves reached the cap");
        // A regression guard, not physics. The measured maximum is 17 at 1
        // step on the 64-cell Dam Break with the obstacle box; the ceiling is about
        // twice that, so a broken preconditioner is caught while the
        // splash-dependent variation is not.
        assert!(
            most[0] > 0 && most[0] as usize <= 32 * steps,
            "{steps} steps: pressure iterations a tick {most:?}"
        );
        assert_eq!(most[1], 0, "the default must not run density iterations");
        assert!(
            worst <= 1e-4,
            "{steps} steps: energy rose {worst:.2e} of E0 above its start"
        );
    }
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
        assert!(!scene.surface);
        let layout = scene.layout();
        // Native mapping: 1.5 solid cells outside each authored wall.
        let offset = layout.min.map(|v| v - (1.5 * layout.cell_size) as f32);
        let local = |p: [f32; 3]| std::array::from_fn(|a| p[a] - offset[a]);
        let mut world = FluidWorld::new_seeded(
            Config {
                cells: layout.cells.map(|n| n + 3),
                cell_size: layout.cell_size,
                apic: false,
            },
            0,
        )
        .expect("native world");
        world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
        if scene.fill_height > 0.0 {
            let min = local(layout.min);
            world
                .add_fluid_box(
                    Bounds {
                        min,
                        max: [
                            min[0] + layout.size[0],
                            min[1] + scene.fill_height as f32,
                            min[2] + layout.size[2],
                        ],
                    },
                    [0.0; 3],
                )
                .expect("pool");
        }
        if scene.initial_volume().is_some() {
            world
                .add_fluid_box(
                    Bounds {
                        min: local(scene.column.map(|p| p[0] as f32)),
                        max: local(scene.column.map(|p| p[1] as f32)),
                    },
                    [0.0; 3],
                )
                .expect("column");
        }
        // Native inserts queued fluid at the end of its first step; GPU Run
        // publishes its fill without stepping. Both now start at rest at t=0.
        world.step(Seconds(1.0 / 60.0)).expect("insert native seed");
        (world, offset)
    }

    #[cfg(test)]
    fn snapshot(
        world: &mut FluidWorld,
        offset: [f32; 3],
    ) -> (Vec<FluidParticle>, Vec<f32>, [u32; 3]) {
        let (mut records, mut solid) = (Vec::new(), Vec::new());
        let info = loop {
            match world.capture_particle_frame(offset, &mut records, &mut solid) {
                Ok(info) => break info,
                Err(CaptureError::Capacity {
                    particles,
                    solid: nodes,
                }) => {
                    records.resize(particles as usize, ParticleRecord::default());
                    solid.resize(nodes, 0.0);
                }
                Err(CaptureError::Fluid(error)) => panic!("native capture: {error}"),
            }
        };
        let particles = records[..info.count as usize]
            .iter()
            .map(|p| FluidParticle {
                position_radius: p.position_radius,
                velocity: p.velocity,
                id: p.id,
            })
            .collect();
        (particles, solid, info.solid_nodes)
    }

    #[cfg(test)]
    fn seed_site(scene: WaterScene, p: &FluidParticle) -> [u32; 3] {
        assert_eq!(p.velocity, [0.0; 3], "seed must be at rest");
        std::array::from_fn(|a| {
            let site =
                2.0 * (f64::from(p.position_radius[a]) - scene.min()[a]) / scene.cell_size() - 0.5;
            // Native uses .25 * (jitter_factor - .001) * h, even with
            // jitter_factor=0. Its interior positions differ by <= .00025h.
            assert!(
                (site - site.round()).abs() < 0.00051,
                "seed off half-cell lattice: {site}"
            );
            assert!(site.round() >= 0.0);
            site.round() as u32
        })
    }

    #[cfg(test)]
    fn seed_sites(scene: WaterScene, particles: &[FluidParticle]) -> Vec<[u32; 3]> {
        let mut sites: Vec<_> = particles
            .iter()
            .filter(|p| p.position_radius[3] > 0.0)
            .map(|p| seed_site(scene, p))
            .collect();
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
            let column = geometry.box_sites_for_test();
            let mut expected = Vec::new();
            for x in 0..2 * n as u32 {
                for y in 0..2 * n as u32 {
                    for z in 0..2 * n as u32 {
                        let site = [x, y, z];
                        if y < geometry.pool_sites_for_test()
                            || (0..3).all(|a| (column[a][0]..column[a][1]).contains(&site[a]))
                        {
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
                    let weight: f32 = (0..3)
                        .map(|a| {
                            if corner & (1 << a) == 0 {
                                1.0 - t[a]
                            } else {
                                t[a]
                            }
                        })
                        .product();
                    phi += weight * solid[(c[0] + nodes[0] * (c[1] + nodes[1] * c[2])) as usize];
                }
                phi > 0.0
            });
            println!(
                "{n}³ native={} authored={authored} excluded by native solid={}",
                got.len(),
                authored - expected.len()
            );
            assert_eq!(
                got, expected,
                "{n}³ native sites after native solid rejection"
            );
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
        let mut graph = water_def(scene)
            .into_graph(&registry, &Default::default())
            .expect("water graph");
        graph
            .add_external_output(node_named(&graph, STEP_NODE), "distance")
            .expect("distance in the plan");
        graph
            .add_external_output(node_named(&graph, STEP_NODE), "tiles")
            .expect("slot table in the plan");
        let mut run = Run::with_graph(scene, graph);
        let step = node_named(&run.graph, STEP_NODE);
        run.exec
            .set_dump_array_set(Some([step].into_iter().collect()));
        run.frame();
        let n = run.n();
        assert_eq!(
            nodes,
            [n as u32 + 1; 3],
            "native solid on the GPU solver grid"
        );
        let h = scene.cell_size() as f32;
        // The step's pooled φ through its own slot table, as dense cells.
        let dumped = |port: &str, words: usize| -> Vec<u32> {
            let res = run
                .exec
                .dump_array_resources()
                .iter()
                .find(|&&(node, name, _)| node == step && name == port)
                .unwrap_or_else(|| panic!("step {port} not dumped"))
                .2;
            let buffer = run.exec.dump_array_buffer(res).expect("step buffer");
            let bytes = 4 * words as u64;
            let staging = run
                .device
                .create_buffer(bytes, manifold_gpu::BufferUse::Readback);
            let mut enc = run.device.create_encoder("wall phi readback");
            enc.copy_buffer_to_buffer(buffer, &staging, bytes);
            enc.commit_and_wait().expect("GPU work completes");
            // SAFETY: the copy completed; staging is shared and `bytes` long.
            unsafe {
                std::slice::from_raw_parts(
                    staging
                        .mapped_ptr()
                        .expect("shared staging")
                        .cast::<u32>()
                        .cast_const(),
                    words,
                )
            }
            .to_vec()
        };
        let cells = [n as u32; 3];
        let (pool_tiles, origin) = run.pool();
        let pool: Vec<f32> = dumped("distance", pool_len(pool_tiles) as usize)
            .into_iter()
            .map(f32::from_bits)
            .collect();
        let table = dumped("tiles", table_words(pool_tiles) as usize);
        let phi = from_pool_under(&pool, &table, pool_tiles, origin, cells);
        let (mut extended, mut wrong) = (0, Vec::new());
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let centre = (0..8)
                        .map(|c| {
                            let p = [i + (c & 1), j + ((c >> 1) & 1), k + ((c >> 2) & 1)];
                            solid[p[0] + (n + 1) * (p[1] + (n + 1) * p[2])]
                        })
                        .sum::<f32>()
                        / 8.0;
                    let v = phi[i + n * (j + n * k)];
                    if centre < 0.0 && v < 0.5 * h {
                        if (v + 0.5 * h).abs() < 1e-5 {
                            extended += 1
                        } else {
                            wrong.push(([i, j, k], v / h))
                        }
                    }
                }
            }
        }
        println!(
            "wall cells taken as water at -h/2: {extended}, left otherwise: {}",
            wrong.len()
        );
        assert!(extended > 0, "the pool must touch the wall's cells");
        assert!(
            wrong.is_empty(),
            "wall cells under h/2 not at -h/2 (cell, phi/h): {:?}",
            &wrong[..wrong.len().min(8)]
        );
    }
}

/// The volume oracle on water that must not change: a resting pool's level-set
/// volume holds within 0.5% for 2 s. A drift here is the measure, not the
/// solver. It also prints the surface's skin, the depth the zero level sits
/// outside the water, for the Dam Break's frame-0 skin to agree with.
#[test]
fn gpu_flip_still_pool_keeps_its_level_set_volume() {
    let scene = WaterScene::still_pool(64).with_surface();
    let mut run = Run::new(scene);
    let mut measures = Vec::new();
    for _ in 0..120 {
        run.frame();
        measures.push(run.surface_measure());
    }
    let (v0, a0) = measures[0];
    let drift = measures
        .iter()
        .map(|(v, _)| (v / v0 - 1.0).abs())
        .fold(0.0, f64::max);
    let skin = VolumeDrift::new(measures[0], run.particle_volume()).skin();
    println!(
        "GPU FLIP still pool level set: frame 0 {v0:.4} m³ over {a0:.3} m², last {:.4} m³, drift max {:.3}%",
        measures[119].0,
        100.0 * drift
    );
    println!(
        "GPU FLIP still pool level set: particles hold {:.4} m³, skin {:.2} mm",
        run.particle_volume(),
        1000.0 * skin
    );
    assert!(
        drift < 5e-3,
        "a resting pool's level-set volume moved {:.3}%",
        100.0 * drift
    );
}

/// The node whose `out` is the drawn level set: the surface group's, or
/// Narrow Band's fill of it.
fn drawn_level_set(scene: WaterScene) -> &'static str {
    if scene.narrow_band {
        "interior_fill"
    } else {
        "liquid_carry_into_solids"
    }
}

/// Narrow Band on a pool 3 m deep at 64³: the water more than 12 cells from
/// the surface, the floor and every wall (40 × 24 × 40 of its 64 × 48 × 64
/// cells) keeps no particles, while the drawn water, its shell from the band's
/// particles and its body from the grid, holds the full particle run's volume
/// within the still pool's 0.5%. The step's pool shrinks to fit the water on
/// the second tick, which the band crosses by refilling its grid's water with
/// particles for that tick.
#[test]
fn gpu_flip_narrow_band_drops_the_deep_particles_and_keeps_the_volume() {
    let scene = WaterScene::deep_pool(64).with_surface();
    let mut full = Run::new(scene);
    let mut band = Run::new(scene.with_narrow_band());
    for _ in 0..30 {
        full.frame();
        band.frame();
    }
    let (kept, all) = (band.particles().len(), full.particles().len());
    let ((v_band, _), (v_full, _)) = (band.surface_measure(), full.surface_measure());
    println!(
        "Narrow Band deep pool, frame 30: {kept} of {all} particles ({:.1}%), drawn volume {v_band:.4} m³ vs {v_full:.4} m³ ({:+.3}%)",
        100.0 * kept as f64 / all as f64,
        100.0 * (v_band / v_full - 1.0)
    );
    assert!(
        (kept as f64) < 0.9 * all as f64,
        "the band kept {kept} of {all} particles"
    );
    assert!(
        (v_band / v_full - 1.0).abs() < 5e-3,
        "the band draws {v_band:.4} m³ where the full run draws {v_full:.4} m³"
    );
}

/// Encode replay changes nothing the step computes: 60 Dam Break ticks
/// with the executor's replay on match replay off bit for bit, particles,
/// faces and the solver's words, every tick, while the solver's rounds run
/// as replayed segments and none directly. The step's pool grows as the
/// water spreads, and a new pool is a new encode: it records again and
/// warms up again, and at least twenty ticks run warm.
#[test]
fn gpu_flip_replay_changes_nothing() {
    let scene = WaterScene::dam_break(64).with_steps(1);
    let mut direct = Run::new(scene);
    direct.set_encode_replay(false);
    let mut replay = Run::new(scene);
    // The ring's three entries each take two visits to hold the whole
    // step; ten ticks past the last new pool every frame replays all of it.
    const WARM: u32 = 10;
    let mut last = replay.replay_stats();
    let mut pool = None;
    let mut warm_from = WARM;
    let mut warm_ticks = 0;
    for frame in 1..=60 {
        direct.frame();
        replay.frame();
        let now = replay.pool().0;
        if pool.is_some_and(|before| before != now) {
            warm_from = frame + WARM;
        }
        pool = Some(now);
        let (dp, rp) = (direct.particles(), replay.particles());
        assert!(
            bytemuck::cast_slice::<_, u8>(&dp) == bytemuck::cast_slice::<_, u8>(&rp),
            "tick {frame}: the particles differ with replay on"
        );
        let (df, rf) = (direct.faces(), replay.faces());
        assert!(
            bytemuck::cast_slice::<_, u8>(&df) == bytemuck::cast_slice::<_, u8>(&rf),
            "tick {frame}: the faces differ with replay on"
        );
        assert_eq!(
            direct.solver(),
            replay.solver(),
            "tick {frame}: the solver words differ with replay on"
        );
        let stats = replay.replay_stats();
        let delta = |take: fn(&manifold_gpu::GpuReplayStats) -> u64| take(&stats) - take(&last);
        if frame <= WARM || frame % 20 == 0 || delta(|s| s.recorded) > 0 {
            let clock: Vec<u32> = replay.read(STEP_NODE, "clock_status", 8);
            println!(
                "tick {frame}: pool {now}, clock {clock:?}, live {}; solver words {:?}; recorded {} replayed {} direct {} executes {} segments replayed {} direct {} allocations {}",
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
        if frame > warm_from {
            warm_ticks += 1;
            assert_eq!(
                delta(|s| s.recorded),
                0,
                "tick {frame}: a warm tick records nothing"
            );
            assert_eq!(
                delta(|s| s.segments_direct),
                0,
                "tick {frame}: no round runs directly"
            );
            assert_eq!(
                delta(|s| s.store_allocations),
                0,
                "tick {frame}: a warm ring allocates nothing"
            );
            assert!(
                delta(|s| s.segments_replayed) >= 2,
                "tick {frame}: the solves' rounds run as segments"
            );
        }
        last = stats;
    }
    assert!(warm_ticks >= 20, "only {warm_ticks} ticks ran warm");
    assert_eq!(
        direct.replay_stats().replayed,
        0,
        "the direct run replayed nothing"
    );
}

/// The scene with the step's clock status readable after every frame.
#[cfg(test)]
fn run_with_clock_status(scene: WaterScene) -> Run {
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    let mut graph = water_def(scene)
        .into_graph(&registry, &Default::default())
        .expect("slot-cost water graph");
    let step = node_named(&graph, STEP_NODE);
    graph
        .add_external_output(step, "clock_status")
        .expect("clock status read");
    Run::with_graph(scene, graph)
}

/// An idle slot writes nothing. The step always encodes its full count of
/// substeps and the clock plan gates the ones it leaves no time for, so a
/// still pool the clock finishes in its first slot comes out of three ticks
/// bit for bit the same with one slot encoded as with all six: the particles,
/// the pooled φ and faces with their slot table, the stats and the solver
/// words, at two sizes.
#[test]
fn gpu_flip_idle_slots_write_nothing() {
    use manifold_water_gpu_flip::primitives::gpu_flip_step::set_encoded_slots;
    struct FullCount;
    impl Drop for FullCount {
        fn drop(&mut self) {
            set_encoded_slots(None);
        }
    }
    let _reset = FullCount;
    const TICKS: usize = 3;
    for n in [16, 24] {
        let scene = WaterScene::still_pool(n).with_steps(1);
        let mut one = run_with_clock_status(scene);
        let mut six = run_with_clock_status(scene);
        for tick in 0..TICKS {
            set_encoded_slots(Some(1));
            one.frame();
            set_encoded_slots(None);
            six.frame();
            let at = format!("{n}³ tick {tick}");
            for (label, run, last_dt) in
                [("one slot", &one, 1.0f32 / 60.0), ("six slots", &six, 0.0)]
            {
                let status: Vec<u32> = run.read(STEP_NODE, "clock_status", 8);
                assert_eq!(
                    status[0],
                    last_dt.to_bits(),
                    "{at}, {label}: the last encoded slot"
                );
                assert_eq!(
                    status[1],
                    (1.0f32 / 60.0).to_bits(),
                    "{at}, {label}: the whole interval is stepped"
                );
                assert_eq!(status[2], 0.0f32.to_bits(), "{at}, {label}: nothing left");
                assert_eq!(status[6], 1, "{at}, {label}: the clock needs one step");
            }
            let count = one.read::<u32>(STEP_NODE, "identity_out", 8)[7] as usize;
            for (what, a, b) in [
                ("published particles", one.particles(), six.particles()),
                (
                    "step particles",
                    one.read::<FluidParticle>(STEP_NODE, "out", count),
                    six.read::<FluidParticle>(STEP_NODE, "out", count),
                ),
            ] {
                assert!(
                    bytemuck::cast_slice::<_, u8>(&a) == bytemuck::cast_slice::<_, u8>(&b),
                    "{at}: {what} differ"
                );
                let stats = particle_stats(&a);
                assert_eq!(
                    (stats.live, stats.bad),
                    (one.seeded, 0),
                    "{at}: every {what} record stays live and finite"
                );
            }
            let ((of, od, ot), (sf, sd, st)) = (one.pooled_outputs(), six.pooled_outputs());
            assert!(
                bytemuck::cast_slice::<_, u8>(&of) == bytemuck::cast_slice::<_, u8>(&sf),
                "{at}: the pooled faces differ"
            );
            assert!(
                bytemuck::cast_slice::<_, u8>(&od) == bytemuck::cast_slice::<_, u8>(&sd),
                "{at}: the pooled distance differs"
            );
            assert_eq!(ot, st, "{at}: the slot table differs");
            let stats = |run: &Run| -> Vec<u32> {
                run.read("stats", "stats_out", LIQUID_STATS_WORDS as usize)
            };
            assert_eq!(stats(&one), stats(&six), "{at}: the stats differ");
            let words = 2 * count + SOLVER_WORDS as usize;
            assert_eq!(
                one.read::<u32>(STEP_NODE, "capped", words),
                six.read::<u32>(STEP_NODE, "capped", words),
                "{at}: the capped and solver words differ"
            );
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
            manifold_water_gpu_flip::primitives::gpu_flip_step::set_force_solid_velocity(false);
        }
    }
    fn original_frame(run: &mut Run) -> (f64, f64) {
        manifold_water_gpu_flip::primitives::gpu_flip_step::set_force_solid_velocity(true);
        let _reset = ForceSolidVelocity;
        run.timed_frame()
    }
    fn median(values: &mut [f64]) -> f64 {
        values.sort_by(f64::total_cmp);
        (values[values.len() / 2 - 1] + values[values.len() / 2]) * 0.5
    }
    manifold_water_gpu_flip::primitives::gpu_flip_step::set_force_solid_velocity(false);
    for n in [16, 64] {
        let scene = WaterScene::still_pool(n).with_steps(1);
        let mut optimized = run_with_clock_status(scene);
        let mut original = run_with_clock_status(scene);
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
                assert!(
                    time.0.is_finite() && time.0 > 0.0,
                    "{n}³ tick {frame}, {label}: positive finite GPU chunk span, got {}",
                    time.0
                );
                assert!(
                    time.1.is_finite() && time.1 > 0.0,
                    "{n}³ tick {frame}, {label}: positive finite CPU encode time, got {}",
                    time.1
                );
            }
            if frame >= WARMUP {
                optimized_gpu.push(optimized_time.0);
                optimized_cpu.push(optimized_time.1);
                original_gpu.push(original_time.0);
                original_cpu.push(original_time.1);
            }
            let a: Vec<u32> = optimized.read(STEP_NODE, "clock_status", 8);
            let b: Vec<u32> = original.read(STEP_NODE, "clock_status", 8);
            assert_eq!(a, b, "{n}³ tick {frame}: complete clock status differs");
            assert_eq!(
                a[0],
                0.0f32.to_bits(),
                "{n}³ tick {frame}: the last recorded slot is inactive"
            );
            assert_eq!(
                a[1],
                (1.0f32 / 60.0).to_bits(),
                "{n}³ tick {frame}: exact completed time"
            );
            assert_eq!(
                a[2],
                0.0f32.to_bits(),
                "{n}³ tick {frame}: no unfinished interval"
            );
            assert_eq!(a[6], 1, "{n}³ tick {frame}: exactly one active step");
            assert_eq!(
                (a[4], a[5]),
                (0, 0),
                "{n}³ tick {frame}: no cap or nonfinite input"
            );
            let optimized_step: Vec<FluidParticle> =
                optimized.read(STEP_NODE, "out", scene.particles() as usize);
            let original_step: Vec<FluidParticle> =
                original.read(STEP_NODE, "out", scene.particles() as usize);
            for (what, a, b) in [
                (
                    "published particles",
                    optimized.particles(),
                    original.particles(),
                ),
                ("step particles", optimized_step, original_step),
            ] {
                assert_eq!(
                    bytemuck::cast_slice::<_, u32>(&a),
                    bytemuck::cast_slice::<_, u32>(&b),
                    "{n}³ tick {frame}: {what} bits differ"
                );
                for particles in [&a, &b] {
                    let stats = particle_stats(particles);
                    assert_eq!(
                        (stats.live, stats.bad),
                        (optimized.seeded, 0),
                        "{n}³ tick {frame}: every {what} record remains live and finite"
                    );
                }
            }
            let (a, b) = (optimized.faces(), original.faces());
            assert_eq!(
                bytemuck::cast_slice::<_, u32>(&a),
                bytemuck::cast_slice::<_, u32>(&b),
                "{n}³ tick {frame}: face bits differ"
            );
            let a: Vec<u32> = optimized.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
            let b: Vec<u32> = original.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
            assert_eq!(a, b, "{n}³ tick {frame}: full liquid stats differ");
            let capped_words = 2 * scene.particles() as usize + SOLVER_WORDS as usize;
            let a: Vec<u32> = optimized.read(STEP_NODE, "capped", capped_words);
            let b: Vec<u32> = original.read(STEP_NODE, "capped", capped_words);
            assert_eq!(a, b, "{n}³ tick {frame}: full capped words differ");
            if frame + 1 == WARMUP {
                warm_replay = [optimized.replay_stats(), original.replay_stats()];
            }
        }
        println!(
            "GPU FLIP {n}³ no-body solid velocity, {WARMUP} warmup + {MEASURED} measured ticks/run: median clear GPU {:.3} ms CPU {:.3} ms; six passes GPU {:.3} ms CPU {:.3} ms",
            median(&mut optimized_gpu),
            median(&mut optimized_cpu),
            median(&mut original_gpu),
            median(&mut original_cpu),
        );
        for (label, run, warm) in [
            ("clear", &optimized, warm_replay[0]),
            ("six passes", &original, warm_replay[1]),
        ] {
            let replay = run.replay_stats();
            println!(
                "GPU FLIP {n}³ no-body solid velocity, {label} replay: after warmup {warm:?}; final {replay:?}"
            );
            assert!(
                replay.replayed > warm.replayed
                    && replay.segments_replayed > warm.segments_replayed,
                "{n}³ {label}: measured ticks must exercise ordinary encode replay"
            );
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
            manifold_water_gpu_flip::primitives::gpu_flip_step::set_force_indirect_pockets(false);
        }
    }
    fn original_frame(run: &mut Run) {
        manifold_water_gpu_flip::primitives::gpu_flip_step::set_force_indirect_pockets(true);
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
            (
                "published particles",
                segmented.particles(),
                indirect.particles(),
            ),
            ("step particles", segmented_step, indirect_step),
        ] {
            assert_eq!(
                bytemuck::cast_slice::<_, u32>(&a),
                bytemuck::cast_slice::<_, u32>(&b),
                "frame{frame}: {label}"
            );
            for particles in [&a, &b] {
                let stats = particle_stats(particles);
                assert_eq!(
                    (stats.live, stats.bad),
                    (segmented.seeded, 0),
                    "frame{frame}: {label} remain live and finite"
                );
            }
        }
        assert_eq!(
            bytemuck::cast_slice::<_, u32>(&segmented.faces()),
            bytemuck::cast_slice::<_, u32>(&indirect.faces()),
            "frame{frame}: faces"
        );
        for (node, port, words) in [
            ("stats", "stats_out", LIQUID_STATS_WORDS as usize),
            (STEP_NODE, "capped", 2 * count + SOLVER_WORDS as usize),
        ] {
            assert_eq!(
                segmented.read::<u32>(node, port, words),
                indirect.read::<u32>(node, port, words),
                "frame{frame}: {port}"
            );
        }
    }
    manifold_water_gpu_flip::primitives::gpu_flip_step::set_force_indirect_pockets(false);
    let scene = WaterScene::still_pool(16).with_steps(2);
    // All six numerical slots are recorded: each interval records active
    // work, an inactive tail, then active work next interval.
    let mut segmented = run_with_clock_status(scene);
    let mut indirect = run_with_clock_status(scene);
    segmented.set_encode_replay(true);
    indirect.set_encode_replay(true);
    let mut warm = None;
    for frame in 0..14 {
        let steps = if frame < 10 { 2 } else { 1 };
        if frame == 10 {
            for run in [&mut segmented, &mut indirect] {
                let step = node_named(&run.graph, STEP_NODE);
                run.graph
                    .set_param(
                        step,
                        "steps",
                        manifold_node_engine::parameters::ParamValue::Float(1.0),
                    )
                    .expect("steps");
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
        if frame == 9 {
            warm = Some([segmented.replay_stats(), indirect.replay_stats()]);
        }
    }
    let warm = warm.expect("ten warmup visits");
    for (label, run, before) in [
        ("segmented", &segmented, warm[0]),
        ("indirect", &indirect, warm[1]),
    ] {
        let after = run.replay_stats();
        assert!(
            after.replayed > before.replayed,
            "{label}: changed clock gates exercise warmed replay"
        );
        assert!(
            after.segments_replayed > before.segments_replayed,
            "{label}: warmed solver rounds replay"
        );
        assert_eq!(
            after.recorded, before.recorded,
            "{label}: changed gates do not record new commands"
        );
        assert_eq!(
            after.store_allocations, before.store_allocations,
            "{label}: changed gates reuse replay storage"
        );
    }
    assert!(
        segmented.replay_stats().segments_replayed - warm[0].segments_replayed
            > indirect.replay_stats().segments_replayed - warm[1].segments_replayed,
        "pocket sweeps add replayed segments"
    );
    segmented.set_encode_replay(false);
    indirect.set_encode_replay(false);
    let before = [segmented.replay_stats(), indirect.replay_stats()];
    segmented.frame();
    original_frame(&mut indirect);
    compare(&segmented, &indirect, 1, 14);
    for (run, before) in [(&segmented, before[0]), (&indirect, before[1])] {
        assert_eq!(
            run.replay_stats().replayed,
            before.replayed,
            "direct frame replays no command chains"
        );
        assert_eq!(
            run.replay_stats().segments_replayed,
            before.segments_replayed,
            "direct frame replays no segments"
        );
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
            manifold_water_gpu_flip::primitives::gpu_flip_step::set_force_dense_extend(false);
        }
    }
    fn original_frame(run: &mut Run) -> (f64, f64) {
        manifold_water_gpu_flip::primitives::gpu_flip_step::set_force_dense_extend(true);
        let _reset = ForceDenseExtend;
        run.timed_frame()
    }
    fn median(values: &mut [f64]) -> f64 {
        values.sort_by(f64::total_cmp);
        (values[values.len() / 2 - 1] + values[values.len() / 2]) * 0.5
    }
    fn compare(optimized: &Run, original: &Run, frame: usize) {
        let scene = optimized.scene;
        let at = format!("{}³ Steps {} tick {frame}", scene.pressure.n, scene.steps);
        let a: Vec<u32> = optimized.read(STEP_NODE, "clock_status", 8);
        let b: Vec<u32> = original.read(STEP_NODE, "clock_status", 8);
        assert_eq!(a, b, "{at}: complete clock status differs");
        assert_eq!(
            a[0],
            0.0f32.to_bits(),
            "{at}: the last recorded slot is inactive"
        );
        assert_eq!(
            a[1],
            (1.0f32 / 60.0).to_bits(),
            "{at}: exact completed time"
        );
        assert_eq!(a[2], 0.0f32.to_bits(), "{at}: no unfinished interval");
        assert_eq!(
            a[6], scene.steps as u32,
            "{at}: authored Steps accepted exactly"
        );
        assert_eq!((a[4], a[5]), (0, 0), "{at}: no cap or nonfinite input");
        let count = scene.particles() as usize;
        let optimized_step: Vec<FluidParticle> = optimized.read(STEP_NODE, "out", count);
        let original_step: Vec<FluidParticle> = original.read(STEP_NODE, "out", count);
        for (what, a, b) in [
            (
                "published particles",
                optimized.particles(),
                original.particles(),
            ),
            ("step particles", optimized_step, original_step),
        ] {
            assert!(
                bytemuck::cast_slice::<_, u32>(&a) == bytemuck::cast_slice::<_, u32>(&b),
                "{at}: {what} bits differ"
            );
            for particles in [&a, &b] {
                let stats = particle_stats(particles);
                assert_eq!(
                    (stats.live, stats.bad),
                    (optimized.seeded, 0),
                    "{at}: every {what} record remains live and finite"
                );
            }
        }
        let (a, b) = (optimized.faces(), original.faces());
        assert!(
            bytemuck::cast_slice::<_, u32>(&a) == bytemuck::cast_slice::<_, u32>(&b),
            "{at}: face bits differ"
        );
        let a: Vec<u32> = optimized.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
        let b: Vec<u32> = original.read("stats", "stats_out", LIQUID_STATS_WORDS as usize);
        assert_eq!(a, b, "{at}: full liquid stats differ");
        let capped_words = 2 * count + SOLVER_WORDS as usize;
        let a: Vec<u32> = optimized.read(STEP_NODE, "capped", capped_words);
        let b: Vec<u32> = original.read(STEP_NODE, "capped", capped_words);
        assert!(a == b, "{at}: full capped words differ");
    }
    manifold_water_gpu_flip::primitives::gpu_flip_step::set_force_dense_extend(false);
    for (n, steps) in [(16, 1), (16, 2), (64, 1)] {
        let scene = WaterScene::still_pool(n).with_steps(steps);
        let mut optimized = run_with_clock_status(scene);
        let mut original = run_with_clock_status(scene);
        optimized.set_encode_replay(true);
        original.set_encode_replay(true);
        let mut optimized_gpu = Vec::with_capacity(MEASURED);
        let mut optimized_cpu = Vec::with_capacity(MEASURED);
        let mut original_gpu = Vec::with_capacity(MEASURED);
        let mut original_cpu = Vec::with_capacity(MEASURED);
        let mut warm_replay = [manifold_gpu::GpuReplayStats::default(); 2];
        for frame in 0..WARMUP + MEASURED {
            manifold_water_gpu_flip::primitives::gpu_flip_step::set_force_dense_extend(false);
            let (optimized_time, original_time) = if frame % 2 == 0 {
                (optimized.timed_frame(), original_frame(&mut original))
            } else {
                let original_time = original_frame(&mut original);
                (optimized.timed_frame(), original_time)
            };
            for (label, time) in [("indirect", optimized_time), ("dense", original_time)] {
                assert!(
                    time.0.is_finite() && time.0 > 0.0,
                    "{n}³ Steps {steps}, {label}: positive finite GPU chunk span"
                );
                assert!(
                    time.1.is_finite() && time.1 > 0.0,
                    "{n}³ Steps {steps}, {label}: positive finite CPU encode time"
                );
            }
            if frame >= WARMUP {
                optimized_gpu.push(optimized_time.0);
                optimized_cpu.push(optimized_time.1);
                original_gpu.push(original_time.0);
                original_cpu.push(original_time.1);
            }
            compare(&optimized, &original, frame);
            if frame + 1 == WARMUP {
                warm_replay = [optimized.replay_stats(), original.replay_stats()];
            }
        }
        println!(
            "GPU FLIP {n}³ inactive extension, Steps {steps}, {WARMUP} warmup + {MEASURED} measured ticks/run: median gated GPU {:.3} ms CPU {:.3} ms; dense GPU {:.3} ms CPU {:.3} ms",
            median(&mut optimized_gpu),
            median(&mut optimized_cpu),
            median(&mut original_gpu),
            median(&mut original_cpu),
        );
        for (label, run, warm) in [
            ("indirect", &optimized, warm_replay[0]),
            ("dense", &original, warm_replay[1]),
        ] {
            let replay = run.replay_stats();
            println!(
                "GPU FLIP {n}³ inactive extension, Steps {steps}, {label} replay: after warmup {warm:?}; final {replay:?}"
            );
            assert!(
                replay.replayed > warm.replayed
                    && replay.segments_replayed > warm.segments_replayed,
                "{n}³ Steps {steps}, {label}: measured ticks must exercise ordinary encode replay"
            );
        }
        let gated_segments =
            optimized.replay_stats().segments_replayed - warm_replay[0].segments_replayed;
        let dense_segments =
            original.replay_stats().segments_replayed - warm_replay[1].segments_replayed;
        assert!(
            gated_segments > dense_segments,
            "{n}³ Steps {steps}: extension layers must use gated replay segments"
        );
        if n == 16 && steps == 2 {
            optimized.set_encode_replay(false);
            original.set_encode_replay(false);
            let direct_before = [optimized.replay_stats(), original.replay_stats()];
            manifold_water_gpu_flip::primitives::gpu_flip_step::set_force_dense_extend(false);
            optimized.timed_frame();
            original_frame(&mut original);
            compare(&optimized, &original, WARMUP + MEASURED);
            for (label, run, before) in [
                ("indirect", &optimized, direct_before[0]),
                ("dense", &original, direct_before[1]),
            ] {
                let after = run.replay_stats();
                assert_eq!(
                    after.replayed, before.replayed,
                    "{label}: the direct frame replayed no command chains"
                );
                assert_eq!(
                    after.segments_replayed, before.segments_replayed,
                    "{label}: the direct frame replayed no pressure rounds"
                );
            }
        }
    }
}

/// Sheet seeding on: replayed encodes give the particles a direct encode
/// gives, bit for bit, frame by frame, with births among them. A closed tank
/// holds exactly its fill's slots, so the low x side is open: the floor's
/// water runs out there and frees slots for the sheets' births.
#[test]
fn gpu_flip_sheeting_replay_matches_direct() {
    let scene = WaterScene::dam_break(32).with_closed_faces(63 & !1);
    let sheets = [("domain", "sheet_fill_rate", 1.0)];
    let mut direct = Run::posed(scene, &sheets);
    let mut replayed = Run::posed(scene, &sheets);
    replayed.set_encode_replay(true);
    let seeded = direct.particles().iter().map(|p| p.id).max().unwrap_or(0);
    let mut born = 0;
    for frame in 0..24 {
        direct.frame();
        replayed.frame();
        let a = direct.particles();
        assert!(a == replayed.particles(), "frame {frame}: replay differs");
        born = a
            .iter()
            .filter(|p| p.position_radius[3] > 0.0 && p.id > seeded)
            .count();
    }
    let stats = replayed.replay_stats();
    eprintln!("SHEETING REPLAY: {born} live births after 24 frames, {stats:?}");
    assert!(stats.replayed > 0, "nothing replayed");
    assert!(born > 0, "the Dam Break seeded no sheets");
}

use manifold_node_engine::testkit::atom::{node_named, output_of};

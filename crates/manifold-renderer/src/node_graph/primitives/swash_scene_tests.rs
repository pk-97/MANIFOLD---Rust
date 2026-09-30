//! The FFT water step run on whole scenes (docs/FFT_WATER_SOLVER_DESIGN.md
//! P3): the momentum, still-pool and meshed-volume proofs, and the scene
//! runner the race probes (`swash_race_tests`) share.
//! `fft_water_scenes_cover_every_dispatch` proves every array these graphs
//! allocate before any of them runs here.

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;

use super::swash_preset::{FACE_NODES, REST_PER_CELL, WaterScene, water_def};
use crate::node_graph::liquid::grid::face_len;
use super::swash_volume::{VolumeDrift, volume_and_area};
use super::swash_solve_tests::{node_named, output_of};
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::fluid_particles::{FaceSample, FluidParticle};
use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
use crate::node_graph::{
    EffectGraphDefExt, ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, PrimitiveRegistry, StateStore,
    compile, pre_allocate_resources,
};

const G: f64 = 9.81;

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
}

impl Run {
    pub(super) fn new(scene: WaterScene) -> Self {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        Self::with_graph(scene, water_def(scene).into_graph(&registry, &Default::default()).expect("water def builds"))
    }

    /// The scene frozen as the app renders it: the solves' cosine pairs fused.
    pub(super) fn frozen(scene: WaterScene) -> Self {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        let view = crate::node_graph::freeze::install::fuse_generator_view(&water_def(scene), &registry).expect("the scene fuses");
        Self::with_graph(scene, (*view.def).clone().into_graph(&registry, &view.mesh_rules).expect("fused def builds"))
    }

    fn with_graph(scene: WaterScene, mut graph: Graph) -> Self {
        if scene.faces {
            for name in FACE_NODES {
                graph.add_external_output(node_named(&graph, name), "out").expect("face grid output");
            }
        }
        let plan = compile(&graph).expect("water def compiles");
        let device = crate::test_device();
        let mut backend = MetalBackend::new(device.arc(), 64, 64, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&graph, &plan, &device, &mut backend).expect("pre-allocate");
        let mut exec = Executor::new(Box::new(backend));
        // Everything read after a frame is held past it.
        let last = scene.steps - 1;
        let mut watched = vec![node_named(&graph, &format!("s{last}.move"))];
        for k in 0..scene.steps {
            for name in ["water", "gravity", "project", "collar_total"] {
                watched.push(node_named(&graph, &format!("s{k}.{name}")));
            }
        }
        if scene.surface {
            watched.extend(["liquid_offsets", "liquid_mesh"].map(|name| node_ending(&graph, name)));
        }
        if scene.faces {
            watched.extend(FACE_NODES.map(|name| node_named(&graph, name)));
        }
        exec.set_dump_set(Some(watched.into_iter().collect()));
        Self { device, graph, plan, exec, state: StateStore::new(), scene, frames: 0 }
    }

    /// The surface's solid lattice (`WaterScene::surface_solid`). The planner
    /// may recycle a source's storage, so it is written every frame.
    fn write_solid(&self) {
        let resource = output_of(&self.plan, node_named(&self.graph, "solid"), "out");
        let backend = self.exec.backend();
        let buffer = backend.array_buffer(backend.slot_for(resource).expect("solid bound")).expect("solid buffer");
        let solid = self.scene.surface_solid();
        assert!(buffer.size as usize >= solid.len() * 4, "the solid source holds the surface lattice");
        // SAFETY: shared storage of at least this many floats; no frame is in flight.
        unsafe { buffer.write(0, bytemuck::cast_slice(&solid)) };
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
        volume_and_area(self.surface().into_iter(), super::swash_preset::DAM_MIN, super::swash_preset::BOX_METRES)
    }

    /// The particles' own volume: `REST_PER_CELL` fill a cell.
    pub(super) fn particle_volume(&self) -> f64 {
        self.scene.particles() as f64 * self.scene.pressure.cell_size().powi(3) / REST_PER_CELL
    }

    /// One frame in its own command buffer: GPU ms and CPU encode ms.
    pub(super) fn frame(&mut self) -> (f64, f64) {
        if self.scene.surface {
            self.write_solid();
        }
        let mut enc = self.device.create_encoder("swash-scene");
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
        let backend = self.exec.backend();
        let buffer = backend.array_buffer(backend.slot_for(resource).expect("output bound")).expect("array output");
        assert!(buffer.size as usize >= len * std::mem::size_of::<T>(), "{node:?}.{port} is shorter than {len} records");
        let ptr = buffer.mapped_ptr().expect("shared storage");
        // SAFETY: the frame completed and the buffer holds `len` records.
        unsafe { std::slice::from_raw_parts(ptr.cast::<T>().cast_const(), len) }.to_vec()
    }

    pub(super) fn n(&self) -> usize {
        self.scene.pressure.n
    }

    pub(super) fn particles(&self) -> Vec<FluidParticle> {
        let last = self.scene.steps - 1;
        self.read(&format!("s{last}.move"), "out", self.scene.particles() as usize)
    }

    pub(super) fn water(&self, step: usize) -> Vec<f32> {
        self.read(&format!("s{step}.water"), "out", self.n().pow(3))
    }

    pub(super) fn faces(&self, step: usize) -> Vec<FaceSample> {
        self.read(&format!("s{step}.project"), "out", (self.n() + 1).pow(3))
    }

    /// The seam face grid of the frame's last step, x, y and z (a scene built
    /// with `faces`).
    pub(super) fn face_grid(&self) -> [Vec<f32>; 3] {
        let cells = [self.n() as u32; 3];
        std::array::from_fn(|axis| self.read(FACE_NODES[axis], "out", face_len(cells, axis) as usize))
    }

    /// The face grid the pressure solve starts from: gravity added, walls 0.
    /// Its divergence over the water cells is the solve's right-hand side.
    #[cfg(feature = "water-race-probes")]
    pub(super) fn forced(&self, step: usize) -> Vec<FaceSample> {
        self.read(&format!("s{step}.gravity"), "out", (self.n() + 1).pow(3))
    }

    pub(super) fn collar(&self, step: usize) -> u32 {
        let total: Vec<u32> = self.read(&format!("s{step}.collar_total"), "out", self.n().pow(3));
        *total.last().expect("a lattice")
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
fn fft_water_free_fall_keeps_g() {
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
        "SWASH free fall {}³: {} particles, mean velocity {:?} m/s after {t:.3} s (−g·t = {want:.4}), mean height {:.4} m, collar {}",
        run.n(),
        stats.live,
        stats.mean_velocity,
        stats.mean_height,
        run.collar(scene.steps - 1)
    );
    assert_eq!((stats.live, stats.bad), (scene.particles() as usize, 0), "every particle lives and stays finite");
    assert!((stats.mean_velocity[1] - want).abs() <= 0.01 * want.abs(), "fall speed {} against {want}", stats.mean_velocity[1]);
    assert!(stats.mean_velocity[0].abs().max(stats.mean_velocity[2].abs()) <= 0.01 * want.abs(), "no sideways drift");
}

/// I5: a pool at rest stays at rest. After 2 s the particle count is the
/// fill's and the fastest particle moves under 1 mm/s.
#[test]
fn fft_water_still_pool() {
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
                "SWASH still pool {}³ frame {frame:3}: fastest {:.2e} m/s, mean height {:.5} m, divergence rms {rms:.2e} max {max:.2e} /s, collar {}",
                run.n(),
                stats.fastest,
                stats.mean_height,
                run.collar(last)
            );
            assert_eq!((stats.live, stats.bad), (scene.particles() as usize, 0), "frame {frame}: particles lost or not finite");
            fastest.push(stats.fastest);
        }
    }
    let end = *fastest.last().expect("sampled");
    assert!(end < 1e-3, "fastest particle {end} m/s after 2 s");
}

/// The volume oracle on water that must not change: a resting pool's meshed
/// volume holds within 0.5% for 2 s. A drift here is the measure, not the
/// solver. It also prints the surface's skin, the depth the mesh sits
/// outside the water, for the Dam Break's frame-0 skin to agree with.
#[test]
fn fft_water_still_pool_keeps_its_meshed_volume() {
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
    println!("SWASH still pool meshed: frame 0 {v0:.4} m³ over {a0:.3} m², last {:.4} m³, drift max {:.3}%", measures[119].0, 100.0 * drift);
    println!("SWASH still pool meshed: particles hold {:.4} m³, skin {:.2} mm", run.particle_volume(), 1000.0 * skin);
    assert!(drift < 5e-3, "a resting pool's meshed volume moved {:.3}%", 100.0 * drift);
}

/// The frozen 64³ Dam Break, every solve's cosine pairs fused (BUG-u8io,
/// fft-water-fusion-param-capacity), moves every particle exactly as the
/// unfrozen one through the collapse and into the splash.
/// `fft_water_frozen_graphs_cover_every_dispatch` proves its arrays first.
#[test]
fn fft_water_frozen_step_matches_unfrozen() {
    let scene = WaterScene::dam_break(64);
    let (mut unfrozen, mut frozen) = (Run::new(scene), Run::frozen(scene));
    let fused = frozen.graph.nodes().filter(|node| node.node.type_id().as_str() == "node.wgsl_compute").count();
    assert_eq!(fused, 5 * (scene.steps + scene.density_solves()), "every solve runs its five cosine pairs fused");
    for frame in 0..90 {
        unfrozen.frame();
        frozen.frame();
        let (a, b) = (unfrozen.particles(), frozen.particles());
        let same = bytemuck::cast_slice::<FluidParticle, u8>(&a) == bytemuck::cast_slice::<FluidParticle, u8>(&b);
        assert!(same, "frame {frame}: the frozen step moved particles differently");
    }
}

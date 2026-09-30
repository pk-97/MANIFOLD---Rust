//! The FFT water step run on whole scenes (docs/FFT_WATER_SOLVER_DESIGN.md
//! P3): the momentum and still-pool proofs, and the Dam Break probe that
//! reports cost, divergence after projection, collar size and occupancy.
//! `fft_water_scenes_cover_every_dispatch` proves every array these graphs
//! allocate before any of them runs here.

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;

use super::swash_preset::{WaterScene, water_def};
use super::swash_solve_tests::{node_named, output_of};
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::fluid_particles::{FaceSample, FluidParticle};
use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
use crate::node_graph::{
    EffectGraphDefExt, ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, PrimitiveRegistry, StateStore,
    compile, pre_allocate_resources,
};

const G: f64 = 9.81;

/// A scene's graph compiled and bound once, run frame by frame.
struct Run {
    device: crate::TestDevice,
    graph: Graph,
    plan: ExecutionPlan,
    exec: Executor,
    state: StateStore,
    scene: WaterScene,
    frames: i64,
}

impl Run {
    fn new(scene: WaterScene) -> Self {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        let graph = water_def(scene).into_graph(&registry, &Default::default()).expect("water def builds");
        let plan = compile(&graph).expect("water def compiles");
        let device = crate::test_device();
        let mut backend = MetalBackend::new(device.arc(), 64, 64, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&graph, &plan, &device, &mut backend).expect("pre-allocate");
        let mut exec = Executor::new(Box::new(backend));
        // Everything read after a frame is held past it.
        let last = scene.steps - 1;
        let mut watched = vec![format!("s{last}.move")];
        for k in 0..scene.steps {
            for name in ["water", "project", "collar_total", "divergence"] {
                watched.push(format!("s{k}.{name}"));
            }
        }
        exec.set_dump_set(Some(watched.iter().map(|name| node_named(&graph, name)).collect()));
        Self { device, graph, plan, exec, state: StateStore::new(), scene, frames: 0 }
    }

    /// One frame in its own command buffer: GPU ms and CPU encode ms.
    fn frame(&mut self) -> (f64, f64) {
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
        let resource = output_of(&self.plan, node_named(&self.graph, node), port);
        let backend = self.exec.backend();
        let buffer = backend.array_buffer(backend.slot_for(resource).expect("output bound")).expect("array output");
        assert!(buffer.size as usize >= len * std::mem::size_of::<T>(), "{node}.{port} is shorter than {len} records");
        let ptr = buffer.mapped_ptr().expect("shared storage");
        // SAFETY: the frame completed and the buffer holds `len` records.
        unsafe { std::slice::from_raw_parts(ptr.cast::<T>().cast_const(), len) }.to_vec()
    }

    fn n(&self) -> usize {
        self.scene.pressure.n
    }

    fn particles(&self) -> Vec<FluidParticle> {
        let last = self.scene.steps - 1;
        self.read(&format!("s{last}.move"), "out", self.scene.particles() as usize)
    }

    fn water(&self, step: usize) -> Vec<f32> {
        self.read(&format!("s{step}.water"), "out", self.n().pow(3))
    }

    fn faces(&self, step: usize) -> Vec<FaceSample> {
        self.read(&format!("s{step}.project"), "out", (self.n() + 1).pow(3))
    }

    fn collar(&self, step: usize) -> u32 {
        let total: Vec<u32> = self.read(&format!("s{step}.collar_total"), "out", self.n().pow(3));
        *total.last().expect("a lattice")
    }
}

/// Live particles, how many are not finite, the fastest speed, the mean
/// velocity and the mean height.
struct ParticleStats {
    live: usize,
    bad: usize,
    fastest: f64,
    mean_velocity: [f64; 3],
    mean_height: f64,
}

fn particle_stats(particles: &[FluidParticle]) -> ParticleStats {
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

/// RMS and max |divergence| (1/s) over the water cells of a face grid.
fn divergence(faces: &[FaceSample], water: &[f32], n: usize, h: f64) -> (f64, f64) {
    let m = n + 1;
    let pad = |i: usize, j: usize, k: usize| i + m * (j + m * k);
    let (mut sum, mut max, mut count) = (0.0, 0.0_f64, 0usize);
    for k in 0..n {
        for j in 0..n {
            for i in 0..n {
                if water[i + n * (j + n * k)] <= 0.5 {
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

/// The fraction of cells that are water, the fraction of 8³ blocks holding
/// any, and the height of the water's bounding box in cells.
fn occupancy(water: &[f32], n: usize) -> (f64, f64, usize) {
    let blocks = n.div_ceil(8);
    let mut touched = vec![false; blocks.pow(3)];
    let (mut wet, mut low, mut high) = (0usize, usize::MAX, 0usize);
    for (c, &w) in water.iter().enumerate() {
        if w <= 0.5 {
            continue;
        }
        let (i, j, k) = (c % n, (c / n) % n, c / (n * n));
        wet += 1;
        low = low.min(j);
        high = high.max(j);
        touched[i / 8 + blocks * (j / 8 + blocks * (k / 8))] = true;
    }
    let height = if wet == 0 { 0 } else { high - low + 1 };
    (wet as f64 / water.len() as f64, touched.iter().filter(|&&t| t).count() as f64 / touched.len() as f64, height)
}

/// How the particles pack: per occupied cell, how many particles (8 is the
/// fill's density); how many sit on a wall (within a hundredth of a cell)
/// or near the lid; the mean height.
fn report_packing(particles: &[FluidParticle], n: usize, h: f64) {
    let mut per_cell = vec![0u32; n * n * n];
    let (mut on_wall, mut high, mut height) = (0usize, 0usize, 0.0_f64);
    let side = n as f64 * h;
    for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
        let local: [f64; 3] = std::array::from_fn(|a| f64::from(p.position_radius[a]) - super::swash_preset::DAM_MIN[a]);
        if local.iter().any(|&x| x < 0.01 * h || x > side - 0.01 * h) {
            on_wall += 1;
        }
        if local[1] > side - 0.5 {
            high += 1;
        }
        height += local[1];
        let c: [usize; 3] = std::array::from_fn(|a| ((local[a] / h) as usize).min(n - 1));
        per_cell[c[0] + n * (c[1] + n * c[2])] += 1;
    }
    let occupied: Vec<u32> = per_cell.into_iter().filter(|&c| c > 0).collect();
    let mut histogram = [0usize; 6];
    for &c in &occupied {
        histogram[match c {
            1..=4 => 0,
            5..=7 => 1,
            8 => 2,
            9..=12 => 3,
            13..=24 => 4,
            _ => 5,
        }] += 1;
    }
    let live = particles.iter().filter(|p| p.position_radius[3] > 0.0).count();
    println!(
        "SWASH packing: {live} particles in {} cells ({:.2} per cell; the fill is 8); cells holding 1–4 / 5–7 / 8 / 9–12 / 13–24 / 25+: {histogram:?}; {on_wall} on a wall, {high} within 0.5 m of the lid, mean height {:.3} m",
        occupied.len(),
        live as f64 / occupied.len() as f64,
        height / live as f64
    );
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

/// The Dam Break at 64³ for 300 frames: per-frame cost, divergence after
/// projection, collar size and occupancy. It asserts only what must hold
/// for the numbers to mean anything: no GPU fault, every particle alive and
/// finite, the collar within capacity.
#[test]
fn fft_water_cost_probe() {
    let scene = WaterScene::dam_break(64);
    let mut run = Run::new(scene);
    let (n, h) = (run.n(), scene.pressure.cell_size());
    let (mut gpu, mut cpu, mut rms, mut max) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut collar_max, mut blocks_max, mut height_max, mut water_max) = (0u32, 0.0_f64, 0usize, 0.0_f64);
    for frame in 0..300 {
        let (g, c) = run.frame();
        gpu.push(g);
        cpu.push(c);
        for step in 0..scene.steps {
            let collar = run.collar(step);
            collar_max = collar_max.max(collar);
            assert!(collar as usize <= scene.pressure.capacity, "frame {frame} step {step}: collar {collar} past capacity");
            let water = run.water(step);
            let (r, m) = divergence(&run.faces(step), &water, n, h);
            rms.push(r);
            max.push(m);
            let (fraction, blocks, height) = occupancy(&water, n);
            water_max = water_max.max(fraction);
            blocks_max = blocks_max.max(blocks);
            height_max = height_max.max(height);
        }
        if frame == 0 || frame == 149 {
            report_packing(&run.particles(), n, h);
        }
        if frame % 30 == 29 {
            let stats = particle_stats(&run.particles());
            assert_eq!((stats.live, stats.bad), (scene.particles() as usize, 0), "frame {frame}: particles lost or not finite");
            let (fraction, blocks, height) = occupancy(&run.water(scene.steps - 1), n);
            println!(
                "SWASH dam break {n}³ frame {frame:3}: {g:.2} ms GPU, {c:.2} ms CPU, divergence rms {:.2e} max {:.2e} /s, collar {}, water {:.1}% of cells, {:.1}% of 8³ blocks, {height} cells tall, fastest {:.2} m/s",
                rms.last().unwrap(),
                max.last().unwrap(),
                run.collar(scene.steps - 1),
                100.0 * fraction,
                100.0 * blocks,
                stats.fastest
            );
        }
    }
    report_packing(&run.particles(), n, h);
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let worst = |v: &[f64]| v.iter().copied().fold(0.0_f64, f64::max);
    println!(
        "SWASH dam break {n}³ over 300 frames: GPU {:.2} ms median, CPU encode {:.2} ms median; divergence rms median {:.2e} worst {:.2e}, max median {:.2e} worst {:.2e} /s; collar max {collar_max} of {}; water at most {:.1}% of cells, {:.1}% of 8³ blocks, {height_max} cells tall",
        median(&mut gpu.clone()),
        median(&mut cpu.clone()),
        median(&mut rms.clone()),
        worst(&rms),
        median(&mut max.clone()),
        worst(&max),
        scene.pressure.capacity,
        100.0 * water_max,
        100.0 * blocks_max
    );
}

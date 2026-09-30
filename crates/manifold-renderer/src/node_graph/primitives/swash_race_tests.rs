//! SWASH's side of the FFT water race (docs/FFT_WATER_SOLVER_DESIGN.md P3):
//! the Dam Break probes that report cost, what the projection leaves
//! undone, collar size, occupancy, packing, particle motion and the water
//! volume the surface holds, plus the density-source sweep and the settle
//! check. Minutes long, so opt-in: `--features water-race-probes`.
//! `fft_water_scenes_cover_every_dispatch` proves every array these graphs
//! allocate before any of them runs here.

use super::swash_preset::WaterScene;
use super::swash_scene_tests::{Run, divergence, particle_stats};
use super::swash_still::write_still;
use super::swash_volume::VolumeDrift;
use crate::node_graph::fluid_particles::FluidParticle;

/// How the live particles move: mean, 99th-percentile and top speed (m/s),
/// and the highest particle (m above the floor).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Motion {
    pub mean: f64,
    pub p99: f64,
    pub fastest: f64,
    pub highest: f64,
}

/// `particles` are (position and radius, velocity) pairs; radius 0 marks an
/// unused slot.
pub(crate) fn motion(particles: impl Iterator<Item = ([f32; 4], [f32; 3])>, floor: f64) -> Motion {
    let mut speeds = Vec::new();
    let mut highest = f64::MIN;
    for (p, v) in particles.filter(|(p, _)| p[3] > 0.0) {
        speeds.push(v.iter().map(|&c| f64::from(c).powi(2)).sum::<f64>().sqrt());
        highest = highest.max(f64::from(p[1]) - floor);
    }
    if speeds.is_empty() {
        return Motion::default();
    }
    speeds.sort_by(f64::total_cmp);
    let last = speeds.len() - 1;
    Motion {
        mean: speeds.iter().sum::<f64>() / speeds.len() as f64,
        p99: speeds[(speeds.len() * 99 / 100).min(last)],
        fastest: speeds[last],
        highest,
    }
}

/// Prints the fastest and highest particle over the run, and how fast the
/// water still moves over its last 30 frames (medians of the per-frame mean
/// and 99th percentile).
pub(crate) fn report_motion(label: &str, motion: &[Motion]) {
    let top = motion.iter().map(|m| m.fastest).fold(0.0, f64::max);
    let high = motion.iter().map(|m| m.highest).fold(0.0, f64::max);
    let tail = &motion[motion.len().saturating_sub(30)..];
    let settled = |f: fn(&Motion) -> f64| median(&tail.iter().map(f).collect::<Vec<_>>());
    println!("{label}: top speed {top:.2} m/s, highest particle {high:.2} m over the run");
    println!("{label}: last 30 frames speed mean {:.3} p99 {:.3} m/s", settled(|m| m.mean), settled(|m| m.p99));
}

pub(crate) fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

fn worst(v: &[f64]) -> f64 {
    v.iter().copied().fold(0.0_f64, f64::max)
}

fn particle_motion(particles: &[FluidParticle]) -> Motion {
    motion(particles.iter().map(|p| (p.position_radius, p.velocity)), super::swash_preset::DAM_MIN[1])
}

/// The fraction of cells that are water and the fraction of 8³ blocks
/// holding any.
fn occupancy(water: &[f32], n: usize) -> (f64, f64) {
    let blocks = n.div_ceil(8);
    let mut touched = vec![false; blocks.pow(3)];
    let mut wet = 0usize;
    for (c, &w) in water.iter().enumerate() {
        if w <= 0.5 {
            continue;
        }
        let (i, j, k) = (c % n, (c / n) % n, c / (n * n));
        wet += 1;
        touched[i / 8 + blocks * (j / 8 + blocks * (k / 8))] = true;
    }
    (wet as f64 / water.len() as f64, touched.iter().filter(|&&t| t).count() as f64 / touched.len() as f64)
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
        "SWASH packing: {live} particles in {} cells, {:.2} per cell (the fill is 8), mean height {:.3} m",
        occupied.len(),
        live as f64 / occupied.len() as f64,
        height / live as f64
    );
    println!("SWASH packing: cells holding 1–4 / 5–7 / 8 / 9–12 / 13–24 / 25+: {histogram:?}; {on_wall} on a wall, {high} near the lid");
}

/// What one Dam Break run measured.
struct Record {
    gpu: Vec<f64>,
    cpu: Vec<f64>,
    /// Water volume drift per frame, skin-corrected (meshed runs only).
    volume: Vec<f64>,
    /// Per frame, how the particles move.
    motion: Vec<Motion>,
}

/// The Dam Break for `frames` frames: per-frame GPU and CPU encode ms, what
/// the projection left undone, collar size, occupancy, packing and particle motion; meshed, the
/// water volume the surface holds, and stills at frames 90 and 240. `label`
/// names the run in its lines, kept short because the tool output around
/// these probes cuts long ones. It asserts only what must hold for the
/// numbers to mean anything: no GPU fault, every particle alive and finite,
/// the collar within capacity.
fn dam_break(scene: WaterScene, label: &str, frames: usize) -> Record {
    let mut run = Run::new(scene);
    let (n, h) = (run.n(), scene.pressure.cell_size());
    let mut record = Record { gpu: Vec::new(), cpu: Vec::new(), volume: Vec::new(), motion: Vec::new() };
    let (mut rms, mut max) = (Vec::new(), Vec::new());
    let (mut collar_max, mut blocks_max, mut water_max) = (0u32, 0.0_f64, 0.0_f64);
    let (mut raw, mut oracle) = (Vec::new(), None);
    for frame in 0..frames {
        let (g, c) = run.frame();
        record.gpu.push(g);
        record.cpu.push(c);
        for step in 0..scene.steps {
            let collar = run.collar(step);
            collar_max = collar_max.max(collar);
            assert!(collar as usize <= scene.pressure.capacity, "frame {frame} step {step}: collar {collar} past capacity");
            let water = run.water(step);
            let (r, m) = divergence(&run.faces(step), &water, n, h);
            rms.push(r);
            max.push(m);
            let (fraction, blocks) = occupancy(&water, n);
            water_max = water_max.max(fraction);
            blocks_max = blocks_max.max(blocks);
        }
        if scene.surface {
            let measure = run.surface_measure();
            raw.push(measure.0);
            let oracle = oracle.get_or_insert_with(|| VolumeDrift::new(measure, run.particle_volume()));
            record.volume.push(oracle.drift(measure));
            if frame == 90 || frame == 240 {
                let name: String = label.chars().filter(|c| !c.is_whitespace()).map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
                write_still(&format!("{name}_frame{frame}"), run.surface().into_iter());
            }
        }
        let particles = run.particles();
        record.motion.push(particle_motion(&particles));
        if frame % 30 == 29 {
            let stats = particle_stats(&particles);
            assert_eq!((stats.live, stats.bad), (scene.particles() as usize, 0), "frame {frame}: particles lost or not finite");
            let m = record.motion[frame];
            println!("{label} frame {frame:3}: {g:.1} ms GPU, {c:.1} ms CPU, left rms {:.1e} max {:.1e} /s", rms.last().unwrap(), max.last().unwrap());
            println!("{label} frame {frame:3}: speed mean {:.2} p99 {:.2} top {:.2} m/s, highest {:.2} m", m.mean, m.p99, m.fastest, m.highest);
            if let Some(v) = record.volume.last() {
                println!("{label} frame {frame:3}: water volume {:+.2}%, raw mesh {:+.2}%", 100.0 * v, 100.0 * (raw[frame] / raw[0] - 1.0));
            }
        }
    }
    report_packing(&run.particles(), n, h);
    println!("{label}: GPU {:.2} ms median, CPU encode {:.2} ms median", median(&record.gpu), median(&record.cpu));
    println!("{label}: left undone rms median {:.2e} worst {:.2e}; max median {:.2e} worst {:.2e} /s", median(&rms), worst(&rms), median(&max), worst(&max));
    println!("{label}: collar max {collar_max} of {}; water at most {:.1}% of cells, {:.1}% of 8³ blocks", scene.pressure.capacity, 100.0 * water_max, 100.0 * blocks_max);
    report_motion(label, &record.motion);
    if let Some(oracle) = oracle {
        let drift: Vec<f64> = record.volume.iter().map(|v| v.abs()).collect();
        println!("{label}: particles hold {:.4} m³, mesh {:.4} m³ at frame 0, skin {:.2} mm", run.particle_volume(), raw[0], 1000.0 * oracle.skin());
        println!("{label}: water volume drift max {:.2}%, at the last frame {:+.2}%", 100.0 * worst(&drift), 100.0 * record.volume[frames - 1]);
    }
    record
}

/// The race rows at a lattice: the step alone, then meshed; the difference
/// is the surface. The spread rate is the scene's default.
fn cost_probe(n: usize) {
    dam_break(WaterScene::dam_break(n), &format!("SWASH step {n}³"), 300);
    dam_break(WaterScene::dam_break(n).with_surface(), &format!("SWASH meshed {n}³"), 300);
}

#[test]
fn fft_water_cost_probe() {
    cost_probe(64);
}

#[test]
fn fft_water_cost_probe_refined() {
    cost_probe(128);
}

/// The 128³ splash against the solve's pass count: if the fastest particle
/// and the highest splash fall as passes rise, an under-converged solve is
/// feeding the splash energy; if not, the splash is the scene's.
#[test]
fn fft_water_refined_splash_passes() {
    for passes in [16, 24, 32] {
        let scene = WaterScene::dam_break(128).with_surface().with_passes(passes);
        dam_break(scene, &format!("PASSES {passes} 128³"), 150);
    }
}

/// The 128³ splash as shipped, against the engine's at the same frames
/// (`fft_water_engine_race_refined`): p99 and top speed, peak height.
#[test]
fn fft_water_refined_splash() {
    dam_break(WaterScene::dam_break(128).with_surface(), "SPLASH 128³", 150);
}

/// The 128³ splash against what else could feed it: the density source (rate
/// 0) and the step length (four steps a frame, so a fast particle crosses
/// half as many cells per step as the two-layer face extension covers).
#[test]
fn fft_water_refined_splash_causes() {
    let refined = WaterScene::dam_break(128).with_surface();
    dam_break(WaterScene { spread_rate: 0.0, ..refined }, "SPLASH rate 0 128³", 120);
    dam_break(WaterScene { steps: 4, ..refined }, "SPLASH 4 steps 128³", 120);
}

/// 15 s of the meshed Dam Break at 64³: how still the pool is by the end.
#[test]
fn fft_water_dam_break_settles() {
    dam_break(WaterScene::dam_break(64).with_surface(), "SETTLE 64³", 900);
}

/// The density source's rate against volume drift, particle motion and what
/// the projection leaves undone, on the meshed 64³ Dam Break, with the drift
/// curve every 30 frames per rate. Past one step's worth of crowding (rate ×
/// step dt > 1, 120/s here) the correction overshoots and the water fizzes.
#[test]
fn fft_water_density_sweep() {
    for rate in [0.0, 3.0, 10.0, 30.0, 60.0, 100.0, 120.0, 150.0, 200.0] {
        let scene = WaterScene { spread_rate: rate, ..WaterScene::dam_break(64).with_surface() };
        dam_break(scene, &format!("SWEEP rate {rate:>4}"), 300);
    }
}

//! DIAG PROBE (diag/gpu-flip-feel, not for landing): GPU FLIP against the FLIP
//! Fluids engine on the Dam Break at 64, per-frame curves to
//! /tmp/flip_feel/*.csv and side-view particle stills. The engine's side is
//! `fluid/feel_probe.rs`.

use std::io::Write as _;
use std::sync::atomic::Ordering;

use super::faces_to_particles::FEEL_PROBE_MODE;
use super::gpu_flip_preset::WaterScene;
use super::gpu_flip_scene_tests::Run;
use super::gpu_flip_still::write_still;

pub(crate) const OUT: &str = "/tmp/flip_feel";
pub(crate) const STILLS: [usize; 4] = [30, 60, 90, 150];
const G: f64 = 9.81;

/// One frame's measures over live particles, scene metres, floor at `floor`.
pub(crate) fn measure_row(frame: usize, particles: &[([f32; 4], [f32; 3])], floor: f64) -> String {
    let live: Vec<_> = particles.iter().filter(|(p, _)| p[3] > 0.0).collect();
    let n = live.len().max(1) as f64;
    let (mut front, mut runup, mut lid, mut lid_vy, mut ke, mut pe, mut highest) = (f64::MIN, 0.0_f64, 0usize, 0.0, 0.0, 0.0, 0.0_f64);
    let mut speeds = Vec::with_capacity(live.len());
    for (p, v) in &live {
        let (x, h) = (f64::from(p[0]), f64::from(p[1]) - floor);
        let s2: f64 = v.iter().map(|&c| f64::from(c).powi(2)).sum();
        speeds.push(s2.sqrt());
        ke += 0.5 * s2;
        pe += G * h;
        highest = highest.max(h);
        if h > 0.30 {
            front = front.max(x);
        }
        if x > 1.75 {
            runup = runup.max(h);
        }
        if h > 3.9 {
            lid += 1;
            lid_vy += f64::from(v[1]);
        }
    }
    speeds.sort_by(f64::total_cmp);
    let p99 = speeds.get(speeds.len() * 99 / 100).copied().unwrap_or(0.0);
    let mean = speeds.iter().sum::<f64>() / n;
    format!(
        "{frame},{:.4},{front:.4},{runup:.4},{highest:.4},{lid},{:.5},{:.4},{:.5},{:.5},{mean:.4},{p99:.4}",
        (frame + 1) as f64 / 60.0,
        lid as f64 / n,
        if lid > 0 { lid_vy / lid as f64 } else { 0.0 },
        ke / n,
        pe / n,
    )
}

pub(crate) const HEADER: &str = "frame,t,front_x,runup_far,highest,lid_count,lid_share,lid_mean_vy,ke_per_kg,pe_per_kg,speed_mean,speed_p99";

/// A side view (x right, height up, 0..4 m) of particle density through z,
/// 400 × 400, the lid at the top row.
pub(crate) fn side_still(name: &str, particles: &[([f32; 4], [f32; 3])], floor: f64) {
    const S: usize = 400;
    let mut count = vec![0u32; S * S];
    for (p, _) in particles.iter().filter(|(p, _)| p[3] > 0.0) {
        let x = ((f64::from(p[0]) + 2.0) / 4.0 * S as f64) as isize;
        let y = ((f64::from(p[1]) - floor) / 4.0 * S as f64) as isize;
        if (0..S as isize).contains(&x) && (0..S as isize).contains(&y) {
            count[(S - 1 - y as usize) * S + x as usize] += 1;
        }
    }
    let rgb: Vec<u8> = count
        .iter()
        .flat_map(|&c| {
            let v = if c == 0 { 0.0 } else { (0.25 + 0.75 * (f64::from(c).ln() / 6.0)).min(1.0) };
            [(40.0 + 200.0 * v) as u8, (40.0 + 200.0 * v) as u8, (60.0 + 195.0 * v) as u8]
        })
        .collect();
    image::save_buffer(format!("{OUT}/{name}.png"), &rgb, S as u32, S as u32, image::ExtendedColorType::Rgb8).expect("still");
}

fn gpu_run(scene: WaterScene, mode: u32, label: &str, frames: usize, stills: bool) {
    FEEL_PROBE_MODE.store(mode, Ordering::Relaxed);
    std::fs::create_dir_all(OUT).expect("out dir");
    let mut run = Run::new(scene);
    let floor = scene.min()[1];
    let mut csv = std::fs::File::create(format!("{OUT}/gpu_{label}.csv")).expect("csv");
    writeln!(csv, "{HEADER}").unwrap();
    let start = std::time::Instant::now();
    let mut gpu_ms = Vec::new();
    for frame in 0..frames {
        gpu_ms.push(run.frame().0);
        let particles: Vec<_> = run.particles().iter().map(|p| (p.position_radius, p.velocity)).collect();
        writeln!(csv, "{}", measure_row(frame, &particles, floor)).unwrap();
        if stills && STILLS.contains(&frame) {
            side_still(&format!("gpu_{label}_side_f{frame}"), &particles, floor);
            if scene.surface {
                write_still(&format!("gpu_{label}_mesh_f{frame}"), run.surface().into_iter());
            }
        }
    }
    gpu_ms.sort_by(f64::total_cmp);
    println!("FEEL gpu {label}: {frames} frames in {:.1} s, GPU ms median {:.1}", start.elapsed().as_secs_f64(), gpu_ms[gpu_ms.len() / 2]);
    FEEL_PROBE_MODE.store(0, Ordering::Relaxed);
}

/// Baseline (meshed, stills) then the lid switches, one at a time.
#[test]
fn gpu_flip_feel_probe() {
    // SAFETY: set before any thread reads it; this test runs alone.
    unsafe { std::env::set_var("GPU_FLIP_STILLS", OUT) };
    let base = WaterScene::dam_break(64);
    {
        gpu_run(base.with_surface(), 0, "base", 300, true);
        gpu_run(WaterScene { spread_rate: 0.0, ..base }, 0, "density_off", 300, false);
        gpu_run(base, 1, "lid_face_skip", 300, false);
        gpu_run(base, 2, "margin_0001", 300, false);
        gpu_run(base, 4, "separating_lid", 300, true);
        gpu_run(base, 5, "separating_lid_and_face_skip", 300, false);
    }
}

/// Every box wall, not just the lid.
#[test]
fn gpu_flip_feel_walls() {
    // SAFETY: set before any thread reads it; this test runs alone.
    unsafe { std::env::set_var("GPU_FLIP_STILLS", OUT) };
    let base = WaterScene::dam_break(64);
    gpu_run(base, 8, "wall_face_skip", 300, true);
    gpu_run(base, 16 | 4, "separating_walls", 300, false);
    gpu_run(base.with_surface(), 8 | 16 | 4, "separating_walls_and_face_skip", 300, true);
}

/// Water steps a tick: the engine took one 1/60 s substep every frame here.
#[test]
fn gpu_flip_feel_steps() {
    let base = WaterScene::dam_break(64);
    // SAFETY: set before any thread reads it; this test runs alone.
    unsafe { std::env::set_var("GPU_FLIP_STILLS", OUT) };
    let one = WaterScene { steps: 1, spread_rate: super::gpu_flip_preset::SPREAD_PER_STEP * 60.0, ..base };
    gpu_run(one.with_surface(), 0, "steps_1_meshed", 300, true);
    gpu_run(one, 5, "steps_1_lid_fixes", 300, false);
}

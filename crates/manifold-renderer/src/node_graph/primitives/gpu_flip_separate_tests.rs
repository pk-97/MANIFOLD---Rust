//! Separating solids (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (Separating
//! solids)): water at rest against a solid never lets go, so the step with
//! separation is bitwise the step without it; water pulled off the floor
//! lets go and lifts.

use super::gpu_flip_preset::WaterScene;
use super::gpu_flip_scene_tests::{Run, particle_stats};
use super::gpu_flip_step::set_separate_off;

/// A run whose every frame, the fill included, goes through the lever.
struct Twin {
    run: Run,
    off: bool,
}

impl Twin {
    fn new(scene: WaterScene, off: bool) -> Self {
        set_separate_off(off);
        let run = Run::new(scene);
        set_separate_off(false);
        Self { run, off }
    }

    fn frame(&mut self) {
        set_separate_off(self.off);
        self.run.frame();
        set_separate_off(false);
    }
}

fn first_difference(a: &Run, b: &Run) -> Option<String> {
    let (pa, pb) = (a.particles(), b.particles());
    (0..pa.len())
        .find(|&i| bytemuck::bytes_of(&pa[i]) != bytemuck::bytes_of(&pb[i]))
        .map(|i| format!("particle {i}: {:?} vs {:?}", pa[i], pb[i]))
}

/// The resting pools against the condition switched off, the particles
/// bitwise after every frame: the still pool and the hydrostatic column (one
/// scene, 300 frames), and the pool round a static box (120).
#[test]
fn gpu_flip_separating_solids_leave_resting_pools_bitwise() {
    for (name, scene, frames) in
        [("still pool", WaterScene::still_pool(64), 300), ("pool round a box", WaterScene::still_pool(64).with_obstacle(), 120)]
    {
        let mut on = Twin::new(scene, false);
        let mut off = Twin::new(scene, true);
        for frame in 1..=frames {
            on.frame();
            off.frame();
            if let Some(diff) = first_difference(&on.run, &off.run) {
                panic!("{name} frame {frame} differs with separation on: {diff}");
            }
        }
        println!("GPU FLIP separating {name}: {frames} frames bitwise with separation off");
    }
}

/// A still pool under +20 m/s² of upward gravity: the floor lets go and the
/// water lifts. Free fall upward would raise the mean height by 0.625 m in
/// 0.25 s; the proof asks for a third of that with nothing lost.
#[test]
fn gpu_flip_pool_lifts_under_upward_gravity() {
    for n in [64, 128] {
        lift(n, false);
    }
    lift(64, true);
}

fn lift(n: usize, off: bool) {
    let scene = WaterScene::still_pool(n);
    let mut twin = Twin::new(scene, off);
    let start = particle_stats(&twin.run.particles());
    for _ in 1..=15 {
        twin.run.set_gravity(0.0, 20.0);
        twin.frame();
    }
    let end = particle_stats(&twin.run.particles());
    let rise = end.mean_height - start.mean_height;
    let label = if off { "separation off" } else { "separation on" };
    println!("GPU FLIP lift {n}³ {label}: mean height {:.4} → {:.4} m (rise {rise:.4} m), {} live", start.mean_height, end.mean_height, end.live);
    if off {
        return;
    }
    assert_eq!((end.live, end.bad), (start.live, 0), "{n}³: particles lost or not finite");
    assert!(rise > 0.2, "{n}³: the pool rose {rise} m in 0.25 s under +20 m/s²");
}

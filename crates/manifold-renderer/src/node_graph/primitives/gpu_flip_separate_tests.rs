//! Separating solids (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (Separating
//! solids)): water at rest against a solid never lets go, so the step with
//! separation is bitwise the step without it; water pulled off the floor
//! lets go and lifts.

use super::gpu_flip_preset::WaterScene;
use super::gpu_flip_scene_tests::{Run, particle_stats};
use super::gpu_flip_step::set_separate_off;

/// Inactive CFL slots follow a completed density solve. Its pressure and
/// rhs are scratch, and must never become next tick's contact history.
#[test]
fn gpu_flip_inactive_clock_preserves_separation_history() {
    use super::gpu_flip_step::{StepParams, dispatch_pass};
    let device = crate::test_device();
    const CELLS: usize = 8 * 8 * 8;
    let values = |data: &[f32]| {
        let buffer = device.create_buffer_shared(std::mem::size_of_val(data) as u64);
        unsafe { buffer.write(0, bytemuck::cast_slice(data)); }
        buffer
    };
    let water = values(&[1.0; CELLS]);
    let rhs = values(&[-1.0; CELLS]);
    let pressure = values(&[-1.0; CELLS]);
    let faces = values(&[0.0; 9 * 9 * 9 * 8]);
    let history = values(&[0.0; CELLS]);
    let plan = values(&[0.0; 12]);
    let read = || unsafe {
        std::slice::from_raw_parts(history.mapped_ptr().unwrap().cast::<f32>(), CELLS).to_vec()
    };
    let params = StepParams { n: [8; 3], tick_index: 1, ..StepParams::default() };
    let update = || dispatch_pass(&device, "separate_update", &params,
        &[(5, &rhs), (6, &water), (8, &pressure), (10, &faces), (42, &history), (46, &plan)], CELLS as u64);
    let pin = || dispatch_pass(&device, "separate_pin", &params,
        &[(5, &rhs), (6, &water), (10, &faces), (42, &history), (46, &plan)], CELLS as u64);
    let mut words = [0.0f32; 12];
    words[11] = f32::from_bits(1); // live clock, completed cursor
    unsafe { plan.write(0, bytemuck::cast_slice(&words)); }
    update();
    assert_eq!(read(), vec![0.0; CELLS], "inactive density pressure must not release resting water");
    words[0] = 1.0 / 60.0;
    unsafe { plan.write(0, bytemuck::cast_slice(&words)); }
    update();
    assert_eq!(read(), vec![1.0; CELLS], "active negative pressure still releases the solid");
    words[0] = 0.0;
    unsafe {
        plan.write(0, bytemuck::cast_slice(&words));
        water.write(0, bytemuck::cast_slice(&[0.0f32; CELLS]));
    }
    pin();
    update();
    assert_eq!(read(), vec![1.0; CELLS], "inactive mask and divergence must not reattach water");
    words[0] = 1.0 / 60.0;
    unsafe { plan.write(0, bytemuck::cast_slice(&words)); }
    update();
    assert_eq!(read(), vec![0.0; CELLS], "active inward divergence still reattaches water");
}

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

//! Particle-frame ring proofs on a real device (GPU_FLUID_SURFACE_DESIGN.md
//! D7, D19). Stamps use a private event so each test controls retirement.

use manifold_core::Seconds;
use manifold_fluids::{Bounds, FluidWorld, ParticleRecord};
use manifold_gpu::{FrameClock, GpuDevice, GpuEvent};

use super::*;
use crate::water::fluid::FluidDomainNative;
use crate::water::fluid_particles::FluidParticle;

fn settings() -> FluidSettings {
    FluidSettings {
        resolution: 16,
        ..FluidSettings::default()
    }
}

fn controls() -> FluidControls {
    FluidControls {
        emission: false,
        obstacle_enabled: false,
        ..FluidControls::default()
    }
}

fn particle_runtime(event: &GpuEvent) -> FluidRuntime {
    let mut runtime = FluidRuntime::default();
    runtime.particles.set_clock_for_test(FrameClock::new(event));
    runtime.set_outputs(true, false);
    runtime
}

/// One content frame as `node.fluid_surface` runs it: observe, prepare
/// storage, advance. Offline frames drain every due tick.
fn frame(runtime: &mut FluidRuntime, device: &GpuDevice, tick: u64, blocking: bool) {
    runtime
        .observe(settings(), controls(), Seconds(tick as f64 * TICK), 1.0, 0.0)
        .unwrap();
    runtime.prepare_particles(device).unwrap();
    runtime.advance(blocking).unwrap();
}

fn signal(device: &GpuDevice, event: &GpuEvent) {
    let mut encoder = device.create_encoder("fluid particle test signal");
    encoder.signal_event(event);
    encoder.commit_and_wait_completed();
}

/// The same world `NativeSimulation` builds for `settings()`/`controls()`,
/// stepped directly and captured through the P1 API into CPU memory.
fn reference_capture(ticks: u64) -> Vec<ParticleRecord> {
    let settings = settings();
    let controls = controls();
    let domain = settings.domain_layout().unwrap();
    let mut world = FluidWorld::new_seeded(domain.config(settings), settings.seed).unwrap();
    world.set_liquid_options(settings.liquid).unwrap();
    world.set_time_step_options(settings.time_steps).unwrap();
    world.set_surface_options(settings.surface).unwrap();
    world.set_surface_reconstruction_enabled(false).unwrap();
    world.set_whitewater_options(settings.whitewater).unwrap();
    world.set_boundary_collisions(settings.boundary_collisions).unwrap();
    let min = domain.to_native(domain.min);
    world
        .add_fluid_box(
            Bounds {
                min,
                max: [min[0] + domain.size[0], min[1] + settings.fill_height, min[2] + domain.size[2]],
            },
            [0.0; 3],
        )
        .unwrap();
    for _ in 0..ticks {
        world.set_gravity(controls.gravity).unwrap();
        world
            .set_emitter(
                domain.bounds(controls.emitter),
                [0.0, -controls.inflow_speed, 0.0],
                controls.emission,
            )
            .unwrap();
        world.clear_obstacle().unwrap();
        world.step(Seconds(TICK)).unwrap();
    }
    let nodes: usize = domain.cells.iter().map(|&n| n as usize + 4).product();
    let mut particles = vec![ParticleRecord::default(); 1 << 18];
    let mut solid = vec![0.0; nodes];
    let info = world
        .capture_particle_frame(domain.native_origin(), &mut particles, &mut solid)
        .unwrap();
    particles.truncate(info.count as usize);
    particles
}

fn gpu_read(device: &GpuDevice, source: &manifold_gpu::GpuBuffer, count: usize) -> Vec<FluidParticle> {
    let bytes = (count * std::mem::size_of::<FluidParticle>()) as u64;
    let readback = device.create_buffer_shared(bytes.max(4));
    let mut encoder = device.create_encoder("fluid particle readback");
    encoder.copy_buffer_to_buffer(source, &readback, bytes);
    encoder.commit_and_wait_completed();
    let ptr = readback.mapped_ptr().expect("shared readback");
    // SAFETY: shared buffer of at least `bytes`, GPU work completed above.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, bytes as usize) };
    bytemuck::cast_slice(bytes).to_vec()
}

#[test]
fn fluid_particle_frame_reaches_gpu() {
    let device = manifold_gpu::testkit::test_device();
    let event = device.create_event();
    let mut runtime = particle_runtime(&event);
    for tick in 0..=6 {
        frame(&mut runtime, &device, tick, true);
    }
    assert_eq!(runtime.completed_tick, 6);
    assert_eq!(runtime.particles.newest_tick(), Some(6));
    assert!(runtime.vertices.is_empty(), "nothing reads vertices: no CPU mesh");
    let (a, b) = runtime.particles.pair().unwrap();
    assert_eq!(a.frame().unwrap().tick, 5);
    let info = b.frame().unwrap().info;
    assert_eq!(info.count, runtime.stats.particles);
    assert_eq!(info.solid_nodes, [20, 20, 20]);

    let expected = reference_capture(6);
    assert_eq!(expected.len(), info.count as usize);
    let actual = gpu_read(&device, b.particles(), expected.len());
    for (index, (gpu, cpu)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(
            bytemuck::bytes_of(gpu),
            // SAFETY: ParticleRecord is repr(C) plain f32/u32 data of the
            // same 32-byte layout (asserted in fluid_particles.rs).
            unsafe {
                std::slice::from_raw_parts((cpu as *const ParticleRecord).cast::<u8>(), 32)
            },
            "particle {index}"
        );
    }
    let (bounds, nodes) = runtime.particle_lattice().unwrap();
    assert_eq!(nodes, info.solid_nodes);
    for particle in &actual {
        for axis in 0..3 {
            let half = bounds.scale[axis] * 0.5;
            let p = particle.position_radius[axis];
            assert!(p >= bounds.pos[axis] - half && p <= bounds.pos[axis] + half);
        }
        assert!(particle.position_radius[3] > 0.0);
    }
}

#[test]
fn fluid_particle_ring_exhaustion_never_blocks() {
    let device = manifold_gpu::testkit::test_device();
    // Never signalled until the end: every read stamp stays in flight.
    let event = device.create_event();
    let mut runtime = particle_runtime(&event);
    for tick in 0..=4 {
        frame(&mut runtime, &device, tick, true);
        runtime.particles.mark_read();
    }
    assert_eq!(runtime.particles.newest_tick(), Some(4));
    assert_eq!(runtime.particles.free_len(), 2, "A and B are published");

    let started = std::time::Instant::now();
    for tick in 5..=8 {
        frame(&mut runtime, &device, tick, false);
    }
    assert!(started.elapsed() < std::time::Duration::from_millis(250));
    assert!(!runtime.busy, "no slot retired, so no request was sent");
    assert_eq!(runtime.completed_tick, 4);
    assert!(runtime.lag_seconds() > 3.0 * TICK, "the stall shows as lag");

    // The frame that read the slots retires; stepping resumes.
    signal(&device, &event);
    frame(&mut runtime, &device, 9, false);
    assert!(runtime.busy);
    frame(&mut runtime, &device, 10, true);
    assert_eq!(runtime.completed_tick, 10);
    assert_eq!(runtime.particles.newest_tick(), Some(10));
}

#[test]
fn fluid_particle_ring_growth_recaptures_same_tick() {
    let device = manifold_gpu::testkit::test_device();
    let event = device.create_event();
    let mut runtime = particle_runtime(&event);
    runtime.particles.set_particle_target_for_test(16);
    frame(&mut runtime, &device, 0, true);
    frame(&mut runtime, &device, 1, true);
    // The slot was too small: the tick completed, its frame is owed.
    assert_eq!(runtime.completed_tick, 1);
    assert_eq!(runtime.particles.newest_tick(), None);
    assert!(runtime.particle_capture_pending());
    let version = runtime.version;
    let particles = runtime.stats.particles;

    // Next frame, larger slots exist and the same tick is captured without
    // stepping or republishing the rest of the simulation outputs.
    runtime.prepare_particles(&device).unwrap();
    runtime.advance(true).unwrap();
    assert_eq!(runtime.completed_tick, 1);
    assert_eq!(runtime.version, version);
    assert!(!runtime.particle_capture_pending());
    let (_, b) = runtime.particles.pair().unwrap();
    let frame_b = b.frame().unwrap();
    assert_eq!(frame_b.tick, 1);
    assert_eq!(frame_b.info.count, particles);
}

//! Per-tick proofs for `node.whitewater_step` against the CPU statements of its passes.

use manifold_water_liquid::fluid_particles::WhitewaterSpawn;

use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::grid::face_len;
use manifold_water_liquid::whitewater::WhitewaterParticle;
use {
    super::whitewater_particle_cpu::Box3, super::whitewater_particle_cpu::Emission,
    super::whitewater_particle_cpu::Spawn, super::whitewater_particle_cpu::SpawnFields,
    crate::primitives::whitewater_particle_cpu as particle_cpu,
};
use {
    super::whitewater_pool_cpu::Advect, super::whitewater_pool_cpu::Age,
    super::whitewater_pool_cpu::PoolState, super::whitewater_pool_cpu::Preserve,
    super::whitewater_step::empty_slot, crate::primitives::whitewater_pool_cpu as pool_cpu,
};

/// FLIP's _stepFluid calls _updateDiffuseMaterial(dt) on each tick, which
/// emits before advancing the pool. Grouping ticks into display frames must
/// therefore leave every pool row and counter unchanged.
#[test]
fn whitewater_per_tick_cpu_rows_match_at_every_frame_rate() {
    fn run(fps: u32, emit_per_frame: bool) -> Vec<(u32, u32, Vec<u8>)> {
        let grid = Box3 {
            cells: [8; 3],
            center: [4.0; 3],
            size: [8.0; 3],
        };
        let solid = vec![10.0; 9 * 9 * 9];
        let faces: [Vec<f32>; 3] =
            std::array::from_fn(|a| vec![0.2 * a as f32; face_len([8; 3], a) as usize]);
        let fields = pool_cpu::Fields {
            faces: faces.each_ref().map(Vec::as_slice),
            face_cells: [8; 3],
            solid: &solid,
        };
        let mut pool = vec![empty_slot(); 64];
        let mut state = PoolState::default();
        let mut tick = 0;
        let mut rows = Vec::new();
        for frame in 1..=fps / 5 {
            let due = frame * 60 / fps;
            let ticks = due - tick;
            for iteration in 0..ticks {
                // Three hand-built emitters, one of each FLIP type. A changing
                // position exposes the old final-tick field reuse at 30 fps.
                let emission_tick = if emit_per_frame { due - 1 } else { tick };
                let batch = if emit_per_frame {
                    if iteration == 0 { ticks } else { 0 }
                } else {
                    1
                };
                let spawns: Vec<_> = (0..3 * batch)
                    .map(|i| WhitewaterSpawn {
                        position_lifetime: [3.0 + emission_tick as f32 * 0.01, 4.0, 4.0, 2.0],
                        velocity: [0.2, 0.1, 0.0],
                        kind: i % 3,
                    })
                    .collect();
                pool_cpu::append(&mut pool, &spawns, spawns.len() as u32, &mut state);
                for p in &mut pool {
                    *p = pool_cpu::age(
                        pool_cpu::advect(*p, &fields, &grid, Advect::flip(), None).0,
                        Age::flip(),
                    );
                }
                tick += 1;
            }
            if frame * 30 % fps == 0 {
                rows.push((
                    state.emitted,
                    state.next_id,
                    bytemuck::cast_slice(&pool).to_vec(),
                ));
            }
        }
        assert_eq!(state.emitted, 36, "proof must exercise emission");
        rows
    }
    let at_60 = run(60, false);
    assert_eq!(run(30, false), at_60);
    assert_eq!(run(120, false), at_60);
    assert_ne!(
        run(30, true),
        at_60,
        "the old frame-batched emission must fail"
    );
}

/// The accepted duration reaches every whitewater atom: emission count and
/// spawn travel use the full interval, lifecycle age/preserve scale by it,
/// and spray drag applies the same duration in its velocity update.
#[test]
fn live_interval_whitewater_duration() {
    let dt = 0.1f32;
    let particle = FluidParticle {
        position_radius: [4.0, 4.0, 4.0, 0.05],
        velocity: [1.0, 0.0, 0.0],
        id: 1,
    };
    let (emitted, margin) = particle_cpu::emission_count(
        particle,
        1.0,
        1.0,
        0,
        Emission {
            dt,
            rate: 80.0,
            points_per_cell: 8.0,
            ticks: 1.0,
            live_count: 1.0,
        },
    );
    assert!(margin > 1e-5);
    assert_eq!(emitted, 8, "80 particles/s over 0.1 seconds");

    let grid = Box3 {
        cells: [8; 3],
        center: [4.0; 3],
        size: [8.0; 3],
    };
    let faces: [Vec<f32>; 3] =
        std::array::from_fn(|axis| vec![0.0; face_len([8; 3], axis) as usize]);
    let solid = vec![10.0; 9 * 9 * 9];
    let spawn_fields = SpawnFields {
        offsets: &[1],
        particles: std::slice::from_ref(&particle),
        energy: &[1.0],
        faces: faces.each_ref().map(Vec::as_slice),
        face_cells: [8; 3],
        solid: &solid,
    };
    let short = particle_cpu::spawn(
        0,
        &spawn_fields,
        &grid,
        Spawn {
            dt: 0.05,
            capacity: 1,
            emitters: 1,
            seed: 3.5,
            epoch: 2.0,
            min_lifetime: 1.0,
            max_lifetime: 1.0,
            variance: 0.0,
        },
    )
    .0;
    let long = particle_cpu::spawn(
        0,
        &spawn_fields,
        &grid,
        Spawn {
            dt,
            capacity: 1,
            emitters: 1,
            seed: 3.5,
            epoch: 2.0,
            min_lifetime: 1.0,
            max_lifetime: 1.0,
            variance: 0.0,
        },
    )
    .0;
    assert!(short.position_lifetime[3] > 0.0 && long.position_lifetime[3] > 0.0);
    assert!(
        (long.position_lifetime[0] - short.position_lifetime[0]).abs() > 1e-4,
        "spawn travel must use accepted duration"
    );

    let foam = WhitewaterParticle {
        position_lifetime: [4.0, 4.0, 4.0, 2.0],
        kind: 1,
        ..WhitewaterParticle::default()
    };
    let aged = pool_cpu::age(foam, Age { dt, ..Age::flip() });
    assert!(
        (aged.position_lifetime[3] - 1.9).abs() < 1e-6,
        "foam age scales with dt"
    );
    let dense = vec![
        WhitewaterParticle {
            position_lifetime: [4.0, 4.0, 4.0, 2.0],
            kind: 1,
            ..WhitewaterParticle::default()
        };
        45
    ];
    let preserved = pool_cpu::preserve(
        &dense,
        [0.0; 3],
        1.0,
        [8; 3],
        Preserve {
            dt,
            ..Preserve::flip()
        },
    );
    assert!(
        (preserved[0].position_lifetime[3] - 2.075).abs() < 1e-6,
        "foam preservation scales with dt"
    );

    let spray = WhitewaterParticle {
        position_lifetime: [4.0, 4.0, 4.0, 2.0],
        velocity: [1.0, 0.0, 0.0],
        kind: 2,
        ..WhitewaterParticle::default()
    };
    let fields = pool_cpu::Fields {
        faces: faces.each_ref().map(Vec::as_slice),
        face_cells: [8; 3],
        solid: &solid,
    };
    let dragged = pool_cpu::advect(
        spray,
        &fields,
        &grid,
        Advect {
            dt,
            gravity: [0.0; 3],
            spray_drag: 2.0,
            spray_drag_variance: 0.0,
            ..Advect::flip()
        },
        None,
    )
    .0;
    assert!(
        (dragged.velocity[0] - 0.8).abs() < 1e-6,
        "spray drag uses stretched dt"
    );
}

#[cfg(feature = "gpu-proofs")]
mod gpu {
    use super::super::emission_count::WAVECREST_RATE;
    use super::super::energy_potential::{MAX_ENERGY, MIN_ENERGY};
    use super::super::whitewater_cpu::Rng;
    use super::super::whitewater_step::StepShape;
    use manifold_node_engine::scene::transform::Transform;
    use manifold_physics::clock::TICK;
    /// Fast enough to emit, mostly upward.
    const FLOW: [f32; 3] = [0.9, 5.9, -0.7];
    const GRAVITY: [f32; 3] = [0.0, -9.81, 0.0];

    use manifold_gpu::GpuBuffer;

    use super::super::whitewater_step::{Step, StepFrame, StepInputs};
    use super::*;
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
    use manifold_node_engine::testkit::array_harness::{Harness, read};

    /// Same-time rows at 30/60/120 fps, like the liquid conformance export
    /// proof. Capture every tick before another tick can reuse stage storage;
    /// a 30 fps frame encodes both ticks into one command buffer, without a
    /// host fence between them. The 120 fps run also checks held frames.
    #[test]
    fn whitewater_per_tick_gpu_rows_match_at_every_frame_rate() {
        let harness = Harness::new();
        let cells = [8u32; 3];
        let nodes = [13u32; 3];
        let h = 0.1;
        let shape = StepShape::new(
            nodes,
            cells,
            1.0,
            Some(Transform {
                pos: [0.6; 3],
                scale: [1.2; 3],
                ..Transform::default()
            }),
            256,
        )
        .unwrap();
        let shared = |bytes: &[u8]| {
            let b = harness.device.create_buffer_shared(bytes.len() as u64);
            // SAFETY: fresh shared storage, not submitted yet.
            unsafe { b.write(0, bytes) };
            b
        };
        // Analytic sphere sampled at the solver's cell centres. It exercises
        // the distance path directly, never the rendered surface level set.
        let phi: Vec<f32> = (0..512)
            .map(|i| {
                let p = [i % 8, (i / 8) % 8, i / 64].map(|v| v as f32 + 0.5 - 4.0);
                ((p.iter().map(|v| v * v).sum::<f32>()).sqrt() - 2.5).min(3.0) * h
            })
            .collect();
        let distance = shared(bytemuck::cast_slice(&phi));
        let solid = shared(bytemuck::cast_slice(&vec![10.0f32; 13 * 13 * 13]));
        let mut rng = Rng(0x215);
        let particles: Vec<FluidParticle> = (0..256)
            .map(|_| {
                let d: [f32; 3] = std::array::from_fn(|_| 2.0 * rng.unit() - 1.0);
                let norm = d.iter().map(|v| v * v).sum::<f32>().sqrt().max(0.001);
                let p = d.map(|v| 0.6 + 0.24 * v / norm);
                FluidParticle {
                    position_radius: [p[0], p[1], p[2], 0.05],
                    velocity: [0.0; 3],
                    id: 0,
                }
            })
            .collect();
        let particles = shared(bytemuck::cast_slice(&particles));
        // Each tick owns immutable inputs, including when two ticks share an
        // encoder. CPU overwrites of a shared face array would hide this bug.
        let faces: Vec<[GpuBuffer; 3]> = (0..12)
            .map(|tick| {
                std::array::from_fn(|a| {
                    let velocity = FLOW[a] * (1.0 + tick as f32 / 60.0);
                    shared(bytemuck::cast_slice(&vec![
                        velocity;
                        face_len(cells, a) as usize
                    ]))
                })
            })
            .collect();
        let run = |fps: u32| {
            let pool = shared(bytemuck::cast_slice(&vec![empty_slot(); 256]));
            let state = shared(bytemuck::cast_slice(&[0u32; 8]));
            let counts = shared(bytemuck::cast_slice(&[0u32; 8]));
            let populations: [GpuBuffer; 3] = std::array::from_fn(|_| shared(&vec![0u8; 256 * 32]));
            let mut stage = Step::default();
            let mut tick = 0u32;
            let mut rows = Vec::new();
            let mut previous = Vec::new();
            for frame in 1..=fps / 5 {
                let due = frame * 60 / fps;
                let held = tick == due;
                let mut native = harness.device.create_encoder("whitewater per tick frame");
                while tick < due {
                    let f = &faces[tick as usize];
                    let inputs = StepInputs {
                        motion: None,
                        obstacle_source: None,
                        particles: &particles,
                        solid: &solid,
                        faces: crate::primitives::whitewater_step::FaceSource::Axes([
                            &f[0], &f[1], &f[2],
                        ]),
                        distance: &distance,
                    };
                    let settings = StepFrame {
                        shape,
                        count: Some(256),
                        ticks: 1,
                        dt: TICK as f32,
                        epoch: 0,
                        seed: tick as f32 * TICK as f32,
                        gravity: GRAVITY,
                        wavecrest_emission: WAVECREST_RATE,
                        turbulence_emission: 175.0,
                        min_turbulence: 100.0,
                        max_turbulence: 200.0,
                        inside_emission: true,
                        generation_rate: 1.0,
                        spray_speed: 1.0,
                        dust_emission: false,
                        boundary_dust: false,
                        dust_rate: 175.0,
                        influence_base: 1.0,
                        influence_decay: 2.0,
                        min_energy: MIN_ENERGY,
                        max_energy: MAX_ENERGY,
                        preserve_foam: false,
                    };
                    stage
                        .advance_tick(
                            &mut GpuEncoder::new(&mut native, &harness.device),
                            &settings,
                            &inputs,
                            &pool,
                            &state,
                            true,
                        )
                        .unwrap();
                    // The boundary's capture→state pairs, including counters,
                    // IDs and rendering outputs, close after EVERY tick.
                    for (port, destination) in [
                        ("pool_out", &pool),
                        ("state_out", &state),
                        ("counts_out", &counts),
                        ("foam_particles", &populations[0]),
                        ("bubble_particles", &populations[1]),
                        ("spray_particles", &populations[2]),
                    ] {
                        let source = stage.tick_output(port).unwrap();
                        native.copy_buffer_to_buffer(source, destination, destination.size);
                    }
                    tick += 1;
                }
                native.commit_and_wait_completed();
                let words: Vec<u32> = [
                    &pool,
                    &state,
                    &counts,
                    &populations[0],
                    &populations[1],
                    &populations[2],
                ]
                .into_iter()
                .flat_map(|b| read::<u32>(b, b.size as usize / 4))
                .collect();
                if held && !previous.is_empty() {
                    assert_eq!(words, previous, "held frame {frame}");
                }
                previous = words.clone();
                if frame * 30 % fps == 0 {
                    rows.push(words);
                }
            }
            let state_words: Vec<u32> = read(&state, 8);
            assert!(state_words[3] > 0, "fixture must emit, got {state_words:?}");
            assert!(state_words[0] > 0, "fixture must retain live particles");
            rows
        };
        let at_60 = run(60);
        assert_eq!(
            run(30),
            at_60,
            "30 fps pool, IDs, counters and rendered particles"
        );
        assert_eq!(
            run(120),
            at_60,
            "120 fps pool, IDs, counters and rendered particles"
        );
    }
}

fn tagged_spawns(tags: std::ops::Range<u32>) -> Vec<WhitewaterSpawn> {
    tags.map(|t| WhitewaterSpawn {
        position_lifetime: [t as f32, 0.0, 0.0, 1.0],
        velocity: [0.0; 3],
        kind: 0,
    })
    .collect()
}

fn tags(pool: &[WhitewaterParticle]) -> Vec<u32> {
    pool.iter().map(|p| p.position_lifetime[0] as u32).collect()
}

/// A full pool of particles tagged 0..cap, all kept by the next compaction.
fn full_pool(cap: u32) -> (Vec<WhitewaterParticle>, PoolState) {
    let mut pool = vec![empty_slot(); cap as usize];
    let mut state = PoolState::default();
    pool_cpu::append(&mut pool, &tagged_spawns(0..cap), cap, &mut state);
    (pool, state)
}

// Departs from FLIP Fluids, which drops spawns on a full pool: new spawns
// replace the oldest particles and birth order survives compaction.
#[test]
fn full_pool_spawns_replace_the_oldest() {
    let (mut pool, mut state) = full_pool(8);
    pool_cpu::append(&mut pool, &tagged_spawns(100..103), 3, &mut state);
    assert_eq!(tags(&pool), [100, 101, 102, 3, 4, 5, 6, 7]);
    assert_eq!((state.live, state.front, state.pool_full), (8, 3, 3));
    let out = pool_cpu::compact(&pool, &[1; 8], &mut state);
    assert_eq!(tags(&out), [3, 4, 5, 6, 7, 100, 101, 102]);
    assert_eq!((state.live, state.front), (8, 0));
    let ids: Vec<u32> = out.iter().map(|p| p.id).collect();
    assert_eq!(&ids[5..], [8, 9, 10]);
}

#[test]
fn dust_append_evicts_the_next_oldest_not_this_frames_spray() {
    let (mut pool, mut state) = full_pool(8);
    pool_cpu::append(&mut pool, &tagged_spawns(100..102), 2, &mut state);
    pool_cpu::append(&mut pool, &tagged_spawns(200..202), 2, &mut state);
    assert_eq!(tags(&pool), [100, 101, 200, 201, 4, 5, 6, 7]);
    assert_eq!((state.front, state.pool_full), (4, 4));
    let out = pool_cpu::compact(&pool, &[1; 8], &mut state);
    assert_eq!(tags(&out), [4, 5, 6, 7, 100, 101, 200, 201]);
}

#[test]
fn more_spawns_than_capacity_leaves_an_all_new_pool() {
    let (mut pool, mut state) = full_pool(4);
    pool_cpu::append(&mut pool, &tagged_spawns(100..107), 7, &mut state);
    assert_eq!(tags(&pool), [100, 101, 102, 103]);
    assert_eq!((state.front, state.pool_full), (4, 7));
    let out = pool_cpu::compact(&pool, &[1; 4], &mut state);
    assert_eq!(tags(&out), [100, 101, 102, 103]);
}

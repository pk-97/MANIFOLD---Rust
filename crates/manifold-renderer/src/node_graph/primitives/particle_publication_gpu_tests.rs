//! Device proofs use the pass-1 publication contract as their oracle.
use super::liquid_stats::LIQUID_STATS_WORDS;
use super::liquid_surface_tests::read;
use super::particle_frame_blend_tests::publication_contract::publish as reference;
use super::particle_identity::{BirthReservation, ParticleIdentity};
use super::particle_publication::{ParticlePublication, Publication};
use crate::node_graph::fluid_particles::{CellRange, FluidParticle};
use manifold_gpu::{GpuBinding, GpuBuffer};

fn shared<T: bytemuck::Pod>(device: &crate::TestDevice, values: &[T]) -> GpuBuffer {
    let buffer = device.create_buffer_shared(std::mem::size_of_val(values).max(16) as u64);
    buffer.zero_fill();
    // SAFETY: fresh storage, no device work in flight.
    unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
    buffer
}
fn particle(id: u32, x: f32) -> FluidParticle {
    FluidParticle {
        position_radius: [x, 0.0, 0.0, 0.1],
        velocity: [1.0, 0.0, 0.0],
        id,
    }
}

#[test]
fn particle_publication_reorder_death_birth_empty_and_retired_tails() {
    let device = crate::test_device();
    let mut publisher = ParticlePublication::default();
    publisher.prepare(&device);
    // CPU extent proof: every output has 257 records, metadata/identity 4 words,
    // stats LIQUID_STATS_WORDS; each guarded radix/scatter dispatch reaches <=257.
    let capacity = 257;
    let frame = shared(&device, &vec![particle(999, -1.0); capacity]);
    let metadata = shared(&device, &[0u32; 4]);
    let identity = shared(&device, &[7u32, 12, 0, 0]);
    let stats = shared(&device, &[0u32; LIQUID_STATS_WORDS as usize]);
    let mut dead = particle(2, 10.0);
    dead.position_radius[3] = 0.0;
    for state in [
        vec![particle(3, 20.0), particle(1, 0.0), particle(2, 10.0)],
        vec![
            particle(5, 40.0),
            particle(3, 20.75),
            dead,
            particle(1, 0.75),
            particle(4, 30.0),
        ],
        vec![],
        vec![particle(6, 50.0), particle(u32::MAX, 60.0)],
    ] {
        let source = shared(
            &device,
            &if state.is_empty() {
                vec![FluidParticle::default()]
            } else {
                state.clone()
            },
        );
        assert!(source.size >= state.len() as u64 * 32);
        assert_eq!(frame.size, capacity as u64 * 32);
        let mut encoder = device.create_encoder("particle-publication-fixture");
        publisher
            .encode(
                &device,
                &mut encoder,
                Publication {
                    source: &source,
                    target: &frame,
                    identity: &identity,
                    stats: &stats,
                    metadata: &metadata,
                    count: state.len() as u32,
                },
            )
            .unwrap();
        encoder.commit_and_wait_completed();
        let (expected, count) = reference(&state, capacity);
        assert_eq!(read::<FluidParticle>(&frame, capacity), expected);
        assert_eq!(read::<u32>(&metadata, 4), [count as u32, 12, 1, 0]);
        assert_eq!(
            read::<FluidParticle>(&source, state.len()),
            state,
            "publication cannot reorder solver storage"
        );
    }
}

#[test]
fn particle_identity_birth_ranges_rollover_and_exact_epoch_exhaustion() {
    let device = crate::test_device();
    let mut allocator = ParticleIdentity::default();
    allocator.prepare(&device);
    let mut original = [
        particle(9, 0.0),
        particle(3, 10.0),
        FluidParticle::default(),
        FluidParticle::default(),
    ];
    // Fill sites inside solids retain their IDs even with radius zero.
    original[3].id = 12;
    // Single dispatch reserve reads one CellRange, two scan words and 12 plan
    // words; rare rollover touches exactly the two live records of four slots.
    let particles = shared(&device, &original);
    let identity = shared(&device, &[0u32; 4]);
    let ranges = shared(&device, &[CellRange { start: 0, count: 2 }]);
    let scan = shared(&device, &[1u32, 2]);
    let plan = shared(&device, &[0u32; 12]);
    let mut encoder = device.create_encoder("particle-identity-seed");
    allocator.seed(&mut encoder, &particles, &identity, 4, 0);
    encoder.commit_and_wait_completed();
    assert_eq!(
        read::<u32>(&identity, 4),
        [13, 0, 0, 0],
        "seed above every fill ID, including inactive solid sites"
    );
    for (next, epoch, expected, renumber) in [
        (10u32, 7, [12, 7, 10, 0], false),
        (u32::MAX, 12, [5, 13, 3, 0], true),
        (0, 16_777_216, [0, 16_777_216, 0, 1], false),
    ] {
        // SAFETY: the previous encode retired before the fixture is rewritten.
        unsafe {
            identity.write(0, bytemuck::cast_slice(&[next, epoch, 0u32, 0]));
            particles.write(0, bytemuck::cast_slice(&original));
        }
        let mut encoder = device.create_encoder("particle-identity-reserve");
        allocator.reserve(
            &mut encoder,
            BirthReservation {
                particles: &particles,
                identity: &identity,
                ranges: &ranges,
                scan: &scan,
                plan: &plan,
                params: [4, 1, 2, 1],
            },
        );
        encoder.commit_and_wait_completed();
        assert_eq!(read::<u32>(&identity, 4), expected);
        let mut expected_particles = original;
        if renumber {
            expected_particles[0].id = 1;
            expected_particles[1].id = 2;
        }
        assert_eq!(read::<FluidParticle>(&particles, 4), expected_particles);
    }
    // Recovery cannot wrap the exact-f32 epoch either. A domain reset is the
    // only operation allowed to clear the request and restart epoch zero.
    let mut encoder = device.create_encoder("particle-identity-recovery-exhaustion");
    allocator.seed(&mut encoder, &particles, &identity, 4, u32::MAX);
    encoder.commit_and_wait_completed();
    assert_eq!(read::<u32>(&identity, 4), [0, 16_777_216, 0, 1]);
    let mut encoder = device.create_encoder("particle-identity-domain-reset");
    allocator.seed(&mut encoder, &particles, &identity, 4, 0);
    encoder.commit_and_wait_completed();
    assert_eq!(read::<u32>(&identity, 4), [13, 0, 0, 0]);
    // Death of the entire pool does not rewind the next birth identity.
    unsafe {
        particles.write(0, bytemuck::cast_slice(&[FluidParticle::default(); 4]));
        ranges.write(0, bytemuck::bytes_of(&CellRange { start: 0, count: 0 }));
    }
    let mut encoder = device.create_encoder("particle-identity-empty-pool");
    allocator.reserve(
        &mut encoder,
        BirthReservation {
            particles: &particles,
            identity: &identity,
            ranges: &ranges,
            scan: &scan,
            plan: &plan,
            params: [4, 1, 2, 1],
        },
    );
    encoder.commit_and_wait_completed();
    assert_eq!(read::<u32>(&identity, 4), [15, 0, 13, 0]);
}

#[test]
fn particle_identity_multiple_substeps_keep_birth_ids() {
    let device = crate::test_device();
    let mut allocator = ParticleIdentity::default();
    allocator.prepare(&device);
    let particles = shared(
        &device,
        &[
            particle(1, 0.0),
            particle(2, 10.0),
            particle(3, 20.0),
            FluidParticle::default(),
        ],
    );
    let identity = shared(&device, &[4u32, 7, 0, 0]);
    let ranges = shared(&device, &[CellRange { start: 0, count: 3 }]);
    let scan = shared(&device, &[0u32]);
    let plan = shared(&device, &[0u32; 12]);
    // Test movement/reorder only: production allocation and publication remain
    // under test, and the CPU fixture specifies +0.75 across three substeps.
    let movement = device.create_compute_pipeline(
        r#"
        struct Particle { position_radius: vec4<f32>, velocity: vec3<f32>, id: u32 }
        @group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
        @compute @workgroup_size(1) fn move_particles() {
            let first = particles[0]; particles[0] = particles[2]; particles[2] = first;
            for (var i = 0u; i < 3u; i++) { particles[i].position_radius.x += 0.25; }
        }
    "#,
        "move_particles",
        "particle-test-move",
    );
    let mut publisher = ParticlePublication::default();
    publisher.prepare(&device);
    let frame = shared(&device, &[FluidParticle::default(); 4]);
    let metadata = shared(&device, &[0u32; 4]);
    let stats = shared(&device, &[0u32; LIQUID_STATS_WORDS as usize]);
    // Four records cover all movement and publication accesses; production
    // encode checks metadata, identity, stats and scan storage before dispatch.
    let mut encoder = device.create_encoder("particle-multi-substep-tick");
    for _ in 0..3 {
        allocator.reserve(
            &mut encoder,
            BirthReservation {
                particles: &particles,
                identity: &identity,
                ranges: &ranges,
                scan: &scan,
                plan: &plan,
                params: [4, 1, 1, 1],
            },
        );
        encoder.dispatch_compute(
            &movement,
            &[GpuBinding::Buffer {
                binding: 0,
                buffer: &particles,
                offset: 0,
            }],
            [1, 1, 1],
            "particle-test-move",
        );
        encoder.compute_memory_barrier_buffers();
    }
    publisher
        .encode(
            &device,
            &mut encoder,
            Publication {
                source: &particles,
                target: &frame,
                identity: &identity,
                stats: &stats,
                metadata: &metadata,
                count: 4,
            },
        )
        .unwrap();
    encoder.commit_and_wait_completed();
    assert_eq!(read::<u32>(&identity, 4), [4, 7, 0, 0]);
    assert_eq!(
        read::<FluidParticle>(&particles, 3),
        [particle(3, 20.75), particle(2, 10.75), particle(1, 0.75)]
    );
    let working = [particle(3, 20.75), particle(2, 10.75), particle(1, 0.75)];
    let (expected, count) = reference(&working, 4);
    assert_eq!(read::<FluidParticle>(&frame, 4), expected);
    assert_eq!(read::<u32>(&metadata, 4), [count as u32, 7, 1, 0]);
}

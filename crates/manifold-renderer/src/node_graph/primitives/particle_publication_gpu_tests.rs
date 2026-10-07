//! Device proofs against the pass-1 publication contract, and byte for byte
//! against the transcribed 1-bit publisher (`particle_publication::reference`).
use super::liquid_stats::LIQUID_STATS_WORDS;
use crate::testkit::liquid_surface::read;
use super::particle_frame_blend_tests::publication_contract::publish as reference;
use super::particle_identity::{BirthReservation, ParticleIdentity};
use super::particle_publication::reference::{live as live_radius, publish as oracle};
use super::particle_publication::{ParticlePublication, Publication, scratch_bytes};
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

/// One publication input and the buffers it starts from.
struct Case {
    label: String,
    /// The source buffer's records; `count` of them are published.
    records: Vec<FluidParticle>,
    count: u32,
    slots: u32,
    identity: [u32; 4],
    stats: [u32; LIQUID_STATS_WORDS as usize],
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((z ^ (z >> 31)) >> 16) as u32
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 8) as f32 / (1u32 << 24) as f32
    }
}

/// Ids that cross every digit and the validity order's edge.
const SPECIAL_IDS: [u32; 12] = [
    0, 1, 15, 16, 0x0fff_ffff, 0x7fff_ffff, 0x8000_0000, 0x8000_0001, 0xf000_0000, u32::MAX - 1, u32::MAX, 0x1234_5678,
];

#[derive(Clone, Copy)]
enum Ids {
    /// Full range, specials and duplicates mixed in.
    Mixed,
    /// A shuffled permutation of 1..=n: unique birth ids, as the solver keeps them.
    Births,
    /// `id & mask`: every high digit equal.
    Masked(u32),
    Equal(u32),
}

/// Finite records; `dead` of them in 1000 carry a radius that is not positive.
fn fill(rng: &mut Rng, n: usize, dead: u32, ids: Ids) -> Vec<FluidParticle> {
    let mut births: Vec<u32> = (1..=n as u32).collect();
    if let Ids::Births = ids {
        for i in (1..n).rev() {
            births.swap(i, rng.next() as usize % (i + 1));
        }
    }
    let mut pool = Vec::new();
    (0..n)
        .map(|i| {
            let id = match ids {
                Ids::Mixed => match rng.next() % 16 {
                    0 => SPECIAL_IDS[rng.next() as usize % SPECIAL_IDS.len()],
                    1 if !pool.is_empty() => pool[rng.next() as usize % pool.len()],
                    _ => {
                        let id = rng.next();
                        if pool.len() < 64 {
                            pool.push(id);
                        }
                        id
                    }
                },
                Ids::Births => births[i],
                Ids::Masked(mask) => rng.next() & mask,
                Ids::Equal(id) => id,
            };
            let radius = if rng.next() % 1000 < dead {
                [0.0, -0.0, -0.02 - rng.unit(), -3.5][rng.next() as usize % 4]
            } else {
                0.001 + 0.1 * rng.unit()
            };
            FluidParticle {
                position_radius: [rng.unit() * 8.0 - 4.0, rng.unit() * 3.0, rng.unit() * 8.0 - 4.0, radius],
                velocity: [rng.unit() * 4.0 - 2.0, rng.unit() * 4.0 - 2.0, rng.unit() * 4.0 - 2.0],
                id,
            }
        })
        .collect()
}

fn cases(rng: &mut Rng) -> Vec<Case> {
    let mut cases = Vec::new();
    let mut push = |rng: &mut Rng, label: &str, records: Vec<FluidParticle>, count: u32, slots: u32| {
        let gate = cases.len() as u32;
        let mut stats = [0u32; LIQUID_STATS_WORDS as usize];
        stats[0] = u32::from(gate % 5 == 1);
        stats[super::liquid_stats::NARROW_BAND_SHORTAGE_WORD as usize] = u32::from(gate % 5 == 2);
        let identity = [rng.next(), rng.next(), rng.next(), u32::from(gate % 5 == 3)];
        cases.push(Case { label: label.to_owned(), records, count, slots, identity, stats });
    };
    let records = fill(rng, 100_000, 250, Ids::Mixed);
    push(rng, "100k slots, a quarter dead, mixed ids", records, 100_000, 100_000);
    let records = fill(rng, 1_000_000, 100, Ids::Mixed);
    push(rng, "1M slots, mixed ids", records, 1_000_000, 1_000_000);
    let records = fill(rng, 1_000_000, 50, Ids::Births);
    push(rng, "1M slots, unique birth ids", records, 1_000_000, 1_000_000);
    let records = fill(rng, 120_000, 200, Ids::Mixed);
    push(rng, "target larger than the source", records, 120_000, 300_000);
    push(rng, "count 0", vec![FluidParticle::default()], 0, 1000);
    let records = fill(rng, 5000, 1000, Ids::Mixed);
    push(rng, "all dead", records, 5000, 5000);
    let records = fill(rng, 70_001, 0, Ids::Mixed);
    push(rng, "all live", records, 70_001, 70_001);
    let records = fill(rng, 10_000, 300, Ids::Equal(0x8000_0000));
    push(rng, "one shared id: stable order", records, 10_000, 10_000);
    let records = fill(rng, 50_000, 100, Ids::Masked(0xf));
    push(rng, "low digit only", records, 50_000, 50_000);
    let records = fill(rng, 50_000, 100, Ids::Masked(0xf000_0000));
    push(rng, "high digit only", records, 50_000, 50_000);
    for (n, slots) in [(1usize, 1u32), (1, 2), (255, 256), (256, 256), (257, 4097), (4096, 4096), (65_537, 65_537)] {
        let records = fill(rng, n, 150, Ids::Mixed);
        push(rng, &format!("{n} of {slots} slots"), records, n as u32, slots);
    }
    let mut records = fill(rng, 1, 0, Ids::Equal(u32::MAX));
    push(rng, "one live u32::MAX", records.clone(), 1, 1);
    records[0].position_radius[3] = -0.0;
    push(rng, "one dead record", records, 1, 3);
    cases
}

fn words<T: bytemuck::Pod>(values: &[T]) -> &[u32] {
    bytemuck::cast_slice(values)
}

/// Publishes `case` over a garbage-filled target and metadata: the target's
/// words and the metadata words.
fn publish_case(device: &crate::TestDevice, publisher: &mut ParticlePublication, case: &Case) -> Result<(Vec<u32>, Vec<u32>), String> {
    let source = shared(device, &case.records);
    let target = shared(device, &vec![0xa5a5_a5a5u32; case.slots as usize * 8]);
    let metadata = shared(device, &[0xcafe_babeu32; 4]);
    let identity = shared(device, &case.identity);
    let stats = shared(device, &case.stats);
    let mut encoder = device.create_encoder("particle-publication-oracle");
    publisher
        .encode(
            device,
            &mut encoder,
            Publication {
                source: &source,
                target: &target,
                identity: &identity,
                stats: &stats,
                metadata: &metadata,
                count: case.count,
            },
        )
        .map_err(|error| format!("{}: encode refused: {error}", case.label))?;
    encoder.commit_and_wait_completed();
    if read::<u32>(&source, case.records.len() * 8) != words(&case.records) {
        return Err(format!("{}: the source was written", case.label));
    }
    Ok((read::<u32>(&target, case.slots as usize * 8), read::<u32>(&metadata, 4)))
}

/// Every target and metadata byte against the reference; a mismatch is
/// returned, not panicked, so one run names every failing case.
fn check_case(case: &Case, got: &[u32], got_metadata: &[u32]) -> Result<(), String> {
    let (want, want_metadata) = oracle(&case.records, case.count, case.slots, case.identity, &case.stats);
    let want = words(&want);
    if let Some(word) = got.iter().zip(want).position(|(g, w)| g != w) {
        let record = word / 8;
        let bad = got.chunks(8).zip(want.chunks(8)).filter(|(g, w)| g != w).count();
        return Err(format!(
            "{}: {bad} of {} records differ, first at record {record}: got {:08x?}, want {:08x?}",
            case.label,
            case.slots,
            &got[record * 8..record * 8 + 8],
            &want[record * 8..record * 8 + 8],
        ));
    }
    if got_metadata != want_metadata {
        return Err(format!("{}: metadata {got_metadata:?}, want {want_metadata:?}", case.label));
    }
    Ok(())
}

/// The publisher's target and metadata equal the transcribed 1-bit publisher
/// byte for byte: 100k and 1M slots, dead holes between live records, ids
/// across every digit (u32::MAX and 0x8000_0000 among them), shared ids,
/// count 0, all dead, all live, and a target larger than its source. One
/// publisher serves every case, so each starts from the last case's scratch,
/// and what it holds is exactly the extent budget of its largest target.
#[test]
fn particle_publication_matches_the_reference_byte_for_byte() {
    let device = crate::test_device();
    let mut publisher = ParticlePublication::default();
    publisher.prepare(&device);
    let mut rng = Rng(0x005e_ed0f_9ab1_1c47);
    let mut largest = 0;
    let mut failures = Vec::new();
    for case in cases(&mut rng) {
        largest = largest.max(case.slots);
        let checked = publish_case(&device, &mut publisher, &case)
            .and_then(|(got, metadata)| check_case(&case, &got, &metadata));
        if let Err(failure) = checked {
            failures.push(failure);
        }
        let held = publisher.held_bytes(&device);
        if held != scratch_bytes(largest) {
            failures.push(format!("{}: holds {held} bytes, the budget is {}", case.label, scratch_bytes(largest)));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Radii at the float edges are live exactly as the 1-bit publisher's GPU
/// compare decided (`reference::live`): positive normals and +inf. NaN,
/// subnormals, zeros and negatives are dead. Live records carrying NaN,
/// infinite, subnormal and negative-zero payloads come through bit for bit.
#[test]
fn particle_publication_edge_radii_match_the_reference() {
    let device = crate::test_device();
    let mut publisher = ParticlePublication::default();
    publisher.prepare(&device);
    let edges = [
        ("NaN", f32::NAN),
        ("-NaN", -f32::NAN),
        ("NaN payload", f32::from_bits(0x7fc0_1234)),
        ("+inf", f32::INFINITY),
        ("-inf", f32::NEG_INFINITY),
        ("smallest subnormal", f32::from_bits(1)),
        ("largest subnormal", f32::from_bits(0x007f_ffff)),
        ("negative subnormal", f32::from_bits(0x8000_0001)),
        ("smallest normal", f32::MIN_POSITIVE),
        ("largest finite", f32::MAX),
        ("-0", -0.0),
        ("+0", 0.0),
    ];
    let payloads = [f32::from_bits(0x7fa0_0001), f32::NEG_INFINITY, f32::from_bits(3), -0.0];
    let mut records = Vec::new();
    for (k, &(_, radius)) in edges.iter().enumerate() {
        // Interleaved ordinary live records keep the sort non-trivial.
        let mut carrier = particle(500 - k as u32, k as f32);
        carrier.velocity[k % 3] = payloads[k % payloads.len()];
        carrier.position_radius[(k + 1) % 3] = payloads[(k + 1) % payloads.len()];
        records.push(carrier);
        let mut edge = particle(1000 - k as u32, 100.0 + k as f32);
        edge.position_radius[3] = radius;
        records.push(edge);
    }
    let case = Case {
        label: "edge radii".into(),
        count: records.len() as u32,
        slots: records.len() as u32 + 5,
        records,
        identity: [3, 4, 0, 0],
        stats: [0; LIQUID_STATS_WORDS as usize],
    };
    let (got, metadata) = publish_case(&device, &mut publisher, &case).unwrap();
    if let Err(failure) = check_case(&case, &got, &metadata) {
        let published: Vec<u32> = got.chunks(8).map(|record| record[7]).collect();
        let verdicts: Vec<String> = edges
            .iter()
            .enumerate()
            .map(|(k, (name, radius))| {
                let live = published.contains(&(1000 - k as u32));
                format!("{name}: published {live}, reference live {}", live_radius(*radius))
            })
            .collect();
        panic!("{failure}\n{}", verdicts.join("\n"));
    }
}

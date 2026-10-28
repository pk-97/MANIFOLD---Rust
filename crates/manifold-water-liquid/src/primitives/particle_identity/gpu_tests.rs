//! The slots-in-use record `seed`, `settle` and `reserve` write beside the
//! particle identity: words 4-7 `[groups, 1, 1, n]`, and the same four words
//! in a step's record when one is bound.

use super::*;
use crate::fluid_particles::CellRange;
use manifold_gpu::BufferUse;

const SLOTS: u32 = 512;
const SENTINEL: u32 = 0xdead_beef;
/// The tile counts word `reserve` reads as the tile pool's exhausted flag.
const EXHAUSTED: u32 = 5;

fn shared(device: &GpuDevice, words: &[u32]) -> GpuBuffer {
    let buffer = device.create_buffer((words.len() * 4) as u64, BufferUse::Upload);
    // SAFETY: a shared buffer of exactly these words; no GPU work in flight.
    unsafe { buffer.write(0, bytemuck::cast_slice(words)) };
    buffer
}

fn read(buffer: &GpuBuffer) -> Vec<u32> {
    let ptr = buffer.mapped_ptr().expect("a shared buffer");
    // SAFETY: a shared buffer of `size` bytes; the GPU work that wrote it has completed.
    bytemuck::cast_slice(unsafe { std::slice::from_raw_parts(ptr, buffer.size as usize) }).to_vec()
}

/// The clock plan words the identity reads: word 0 is the step's seconds,
/// word 11 the live-mode flag. Zero seconds is an inactive slot.
fn plan(device: &GpuDevice, step_seconds: f32) -> GpuBuffer {
    let mut words = [0u32; 12];
    words[0] = step_seconds.to_bits();
    words[11] = 1;
    shared(device, &words)
}

/// `[groups, 1, 1, n]`: at least one group of 256 threads.
fn record(n: u32) -> [u32; 4] {
    [n.div_ceil(256).max(1), 1, 1, n]
}

/// Ranges over `keys` keys whose last range ends at `live`.
fn ranges_ending_at(device: &GpuDevice, live: u32, tail: u32) -> GpuBuffer {
    let tail = tail.min(live);
    let ranges = [
        CellRange {
            start: 0,
            count: live - tail,
        },
        CellRange {
            start: live - tail,
            count: tail,
        },
    ];
    let buffer = device.create_buffer(std::mem::size_of_val(&ranges) as u64, BufferUse::Upload);
    // SAFETY: a shared buffer of exactly these ranges; no GPU work in flight.
    unsafe { buffer.write(0, bytemuck::cast_slice(&ranges)) };
    buffer
}

/// Tile counts with only the exhausted word set to `exhausted`.
fn counts_with_exhausted(device: &GpuDevice, exhausted: u32) -> GpuBuffer {
    let mut words = [0u32; 8];
    words[EXHAUSTED as usize] = exhausted;
    shared(device, &words)
}

fn prepared(device: &GpuDevice) -> ParticleIdentity {
    let mut identity = ParticleIdentity::default();
    identity.prepare(device);
    identity
}

/// `settle` writes the sort's live end into the identity and the step's
/// record, at counts either side of a group boundary.
#[test]
fn settle_publishes_the_live_end_to_the_identity_and_the_step_record() {
    let device = manifold_gpu::testkit::test_device();
    let ops = prepared(&device);
    let active = plan(&device, 0.01);
    for live in [0, 1, 255, 256, 257, 300, 512] {
        for tail in [0, 3] {
            let ranges = ranges_ending_at(&device, live, tail);
            let before = [11, 4, 6, 0, SENTINEL, SENTINEL, SENTINEL, SENTINEL];
            let identity = shared(&device, &before);
            let step = shared(&device, &[SENTINEL; 4]);
            let mut enc = device.create_encoder("identity settle proof");
            ops.settle(&mut enc, &ranges, 2, &identity, &active, Some(&step));
            enc.commit_and_wait().expect("GPU work completes");
            let words = read(&identity);
            assert_eq!(words[..4], before[..4], "live {live}: the allocator words");
            assert_eq!(
                words[4..],
                record(live),
                "live {live}, tail {tail}: identity"
            );
            assert_eq!(read(&step), record(live), "live {live}, tail {tail}: step");

            // Without a step record, only the identity is written.
            let identity = shared(&device, &before);
            let mut enc = device.create_encoder("identity settle proof, no step record");
            ops.settle(&mut enc, &ranges, 2, &identity, &active, None);
            enc.commit_and_wait().expect("GPU work completes");
            assert_eq!(
                read(&identity)[4..],
                record(live),
                "live {live}: identity alone"
            );
        }
    }
}

/// An inactive slot's `settle` and `reserve` write no word anywhere.
#[test]
fn an_inactive_plan_writes_nothing() {
    let device = manifold_gpu::testkit::test_device();
    let ops = prepared(&device);
    let idle = plan(&device, 0.0);
    let ranges = ranges_ending_at(&device, 300, 3);
    let particles = shared(&device, &[0u32; SLOTS as usize * 8]);
    let scan = shared(&device, &[0, 40]);
    let counts = counts_with_exhausted(&device, 0);
    let before = [11, 4, 6, 0, 7, 8, 9, 10];
    let identity = shared(&device, &before);
    let step = shared(&device, &[SENTINEL; 4]);
    let mut enc = device.create_encoder("identity inactive proof");
    ops.settle(&mut enc, &ranges, 2, &identity, &idle, Some(&step));
    ops.reserve(
        &mut enc,
        BirthReservation {
            particles: &particles,
            identity: &identity,
            ranges: &ranges,
            scan: &scan,
            plan: &idle,
            tile_counts: &counts,
            step_record: Some(&step),
            params: [SLOTS, 2, 2, EXHAUSTED],
        },
    );
    enc.commit_and_wait().expect("GPU work completes");
    assert_eq!(read(&identity), before);
    assert_eq!(read(&step), [SENTINEL; 4]);
}

/// `reserve` grants a stage all of its request when it fits the free slots,
/// and none when it does not: the live end plus the grant is published. The
/// allocator words move only when births are granted. 212 is the room left
/// past 300 live, so 212 fits and 213 does not.
#[test]
fn reserve_grants_all_or_nothing_and_publishes_the_live_end_plus_the_grant() {
    let device = manifold_gpu::testkit::test_device();
    let ops = prepared(&device);
    let active = plan(&device, 0.01);
    let particles = shared(&device, &[0u32; SLOTS as usize * 8]);
    let counts = counts_with_exhausted(&device, 0);
    for live in [0, 1, 300] {
        for requested in [0, 5, 212, 213, 1000] {
            let ranges = ranges_ending_at(&device, live, 2);
            let scan = shared(&device, &[0, requested]);
            let identity = shared(
                &device,
                &[11, 4, 6, 0, SENTINEL, SENTINEL, SENTINEL, SENTINEL],
            );
            let step = shared(&device, &[SENTINEL; 4]);
            let mut enc = device.create_encoder("identity reserve proof");
            ops.reserve(
                &mut enc,
                BirthReservation {
                    particles: &particles,
                    identity: &identity,
                    ranges: &ranges,
                    scan: &scan,
                    plan: &active,
                    tile_counts: &counts,
                    step_record: Some(&step),
                    params: [SLOTS, 2, 2, EXHAUSTED],
                },
            );
            enc.commit_and_wait().expect("GPU work completes");
            let births = if requested <= SLOTS - live {
                requested
            } else {
                0
            };
            let words = read(&identity);
            let what = format!("live {live}, requested {requested}");
            assert_eq!(words[4..], record(live + births), "{what}: identity");
            assert_eq!(read(&step), record(live + births), "{what}: step");
            if births > 0 {
                assert_eq!(words[..3], [11 + births, 4, 11], "{what}: allocator");
            } else {
                assert_eq!(words[..3], [11, 4, 6], "{what}: allocator");
            }
        }
    }
}

/// A tile pool that ran out this step refuses a stage that fits the slots:
/// the live end stays `n` and the allocator words do not move.
#[test]
fn reserve_grants_nothing_once_the_tile_pool_is_exhausted() {
    let device = manifold_gpu::testkit::test_device();
    let ops = prepared(&device);
    let active = plan(&device, 0.01);
    let particles = shared(&device, &[0u32; SLOTS as usize * 8]);
    let ranges = ranges_ending_at(&device, 300, 2);
    let scan = shared(&device, &[0, 5]);
    let counts = counts_with_exhausted(&device, 1);
    let before = [11, 4, 6, 0, SENTINEL, SENTINEL, SENTINEL, SENTINEL];
    let identity = shared(&device, &before);
    let step = shared(&device, &[SENTINEL; 4]);
    let mut enc = device.create_encoder("identity exhausted proof");
    ops.reserve(
        &mut enc,
        BirthReservation {
            particles: &particles,
            identity: &identity,
            ranges: &ranges,
            scan: &scan,
            plan: &active,
            tile_counts: &counts,
            step_record: Some(&step),
            params: [SLOTS, 2, 2, EXHAUSTED],
        },
    );
    enc.commit_and_wait().expect("GPU work completes");
    let words = read(&identity);
    assert_eq!(words[..4], [11, 4, 6, 0], "the allocator words stay");
    assert_eq!(words[4..], record(300), "n stays the live end");
    assert_eq!(read(&step), record(300), "the step's record too");
}

/// A full-reset request stops `reserve` before it writes anything.
#[test]
fn reserve_leaves_the_record_alone_while_a_reset_is_requested() {
    let device = manifold_gpu::testkit::test_device();
    let ops = prepared(&device);
    let active = plan(&device, 0.01);
    let particles = shared(&device, &[0u32; SLOTS as usize * 8]);
    let ranges = ranges_ending_at(&device, 100, 2);
    let scan = shared(&device, &[0, 9]);
    let counts = counts_with_exhausted(&device, 0);
    let before = [11, 4, 6, 1, 3, 1, 1, 77];
    let identity = shared(&device, &before);
    let mut enc = device.create_encoder("identity reset request proof");
    ops.reserve(
        &mut enc,
        BirthReservation {
            particles: &particles,
            identity: &identity,
            ranges: &ranges,
            scan: &scan,
            plan: &active,
            tile_counts: &counts,
            step_record: None,
            params: [SLOTS, 2, 2, EXHAUSTED],
        },
    );
    enc.commit_and_wait().expect("GPU work completes");
    assert_eq!(read(&identity), before);
}

/// `seed` sets the record to the fill's count, not the slots the buffer holds.
#[test]
fn seed_publishes_the_fills_count() {
    let device = manifold_gpu::testkit::test_device();
    let ops = prepared(&device);
    let mut fill = vec![0u32; SLOTS as usize * 8];
    // The id is the last word of each 8-word record.
    for slot in 0..300 {
        fill[slot * 8 + 7] = slot as u32 + 1;
    }
    let particles = shared(&device, &fill);
    for count in [0, 1, 256, 300] {
        let identity = shared(
            &device,
            &[0, 0, 0, 0, SENTINEL, SENTINEL, SENTINEL, SENTINEL],
        );
        let mut enc = device.create_encoder("identity seed proof");
        ops.seed(&mut enc, &particles, &identity, count, 0);
        enc.commit_and_wait().expect("GPU work completes");
        let words = read(&identity);
        assert_eq!(words[4..], record(count), "count {count}");
        assert_eq!(words[0], count.min(300) + 1, "count {count}: next id");
    }
}

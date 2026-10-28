//! The statistics reduction over the slots the identity counts: records past
//! `n` never reach the words, whatever they hold.

use super::*;
use manifold_gpu::BufferUse;
use manifold_gpu::GpuDevice;

const SENTINEL: u32 = 0xdead_beef;

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

/// One record as eight words: position, radius, velocity, id.
fn record(position: [f32; 3], radius: f32, velocity: [f32; 3], id: u32) -> [u32; 8] {
    [
        position[0].to_bits(),
        position[1].to_bits(),
        position[2].to_bits(),
        radius.to_bits(),
        velocity[0].to_bits(),
        velocity[1].to_bits(),
        velocity[2].to_bits(),
        id,
    ]
}

/// `n` records with a spread of speeds, every seventh one dead and every
/// 50th one non-finite, so each word of the reduction has something to add.
fn water(n: usize) -> Vec<u32> {
    (0..n)
        .flat_map(|i| {
            let f = i as f32;
            let velocity = [f.sin() * 3.0, (f * 0.37).cos(), 0.25 * (f % 5.0)];
            let id = i as u32 + 1;
            if i % 50 == 49 {
                record([f32::NAN, 0.0, 0.0], 0.1, velocity, id)
            } else if i % 7 == 6 {
                record([f, 0.0, 0.0], 0.0, velocity, id)
            } else {
                record([f, 0.5, 0.25], 0.1, velocity, id)
            }
        })
        .collect()
}

/// Records a stale slot could leave behind: a fast live particle and a
/// non-finite one, which would move the speed and the non-finite count.
fn leftovers(n: usize) -> Vec<u32> {
    (0..n)
        .flat_map(|i| {
            if i % 2 == 0 {
                record([0.0; 3], 0.1, [1.0e4, 0.0, 0.0], 9)
            } else {
                record([f32::NAN; 3], f32::NAN, [f32::INFINITY, 0.0, 0.0], 9)
            }
        })
        .collect()
}

/// The words of a reduction over `particles`, whose identity counts `n`
/// slots in use.
fn reduce(device: &GpuDevice, particles: &[u32], n: u32) -> Vec<u32> {
    let pipelines = StatsPipelines::new(device);
    let slots = (particles.len() / 8) as u32;
    let particles = shared(device, particles);
    let identity = shared(device, &[0, 0, 0, 0, n.div_ceil(256).max(1), 1, 1, n]);
    let stats = shared(device, &[SENTINEL; LIQUID_STATS_WORDS as usize]);
    let partials = device.create_buffer(partial_bytes(slots), BufferUse::Gpu);
    let args = device.create_buffer(ARGS_BYTES, BufferUse::Gpu);
    let mut enc = device.create_encoder("liquid stats proof");
    pipelines.encode(
        &mut enc,
        &StatsJob {
            particles: &particles,
            identity: &identity,
            stats: &stats,
            capped: None,
            partials: &partials,
            args: &args,
            particle_mass: 0.5,
        },
    );
    enc.commit_and_wait_completed();
    read(&stats)
}

/// A record of n slots over a buffer with leftovers past n gives the words of
/// a buffer that holds only the n, at counts either side of a block of 2048.
#[test]
fn records_past_the_count_never_reach_the_words() {
    let device = manifold_gpu::testkit::test_device();
    for n in [1usize, 300, 2047, 2048, 2049, 4100] {
        let live = water(n);
        let clean = reduce(&device, &live, n as u32);
        let mut dirty = live.clone();
        dirty.extend(leftovers(5000 - n));
        let with_leftovers = reduce(&device, &dirty, n as u32);
        assert_eq!(with_leftovers, clean, "n {n}");

        let stats = LiquidTickStats::from_words(&clean);
        let dead = (0..n).filter(|i| i % 7 == 6 && i % 50 != 49).count() as u32;
        let nonfinite = (0..n).filter(|i| i % 50 == 49).count() as u32;
        assert_eq!(stats.nonfinite, nonfinite, "n {n}");
        assert_eq!(stats.live, n as u32 - dead - nonfinite, "n {n}");
        assert_eq!(stats.mass, 0.5 * stats.live as f32, "n {n}");
        assert!(stats.max_speed > 0.0 && stats.max_speed < 4.0, "n {n}");
    }
}

/// No slot in use leaves every word zero, the solver words included.
#[test]
fn an_empty_record_reduces_to_zero() {
    let device = manifold_gpu::testkit::test_device();
    let words = reduce(&device, &leftovers(512), 0);
    assert_eq!(words, vec![0; LIQUID_STATS_WORDS as usize]);
}

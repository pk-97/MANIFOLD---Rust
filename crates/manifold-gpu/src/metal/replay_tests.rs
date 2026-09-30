//! GPU proofs for encode replay (`docs/ENCODE_REPLAY_DESIGN.md` I1–I4):
//! a replayed chain writes exactly what direct encoding writes, through
//! changing uniforms, structural changes, texture work and indirect
//! dispatches between stretches and word copies inside them (D9); an entry
//! is never written while in
//! flight; a warm ring records and allocates nothing; and the CPU probe
//! reports the cost of a replayed stretch against direct encoding.

use objc2_metal::MTLSharedEvent;

use super::*;
use crate::{GpuBinding, GpuReplayStats, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

const N: usize = 4096;
const SIDE: u32 = 64;
const GROUPS: [u32; 3] = [N as u32 / 64, 1, 1];

const MIX_WGSL: &str = r#"
struct Params { mul: u32, add: u32, shift: u32, pad: u32 };
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@group(0) @binding(2) var<uniform> p: Params;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let n = arrayLength(&dst);
    if (id.x >= n) { return; }
    let other = src[(id.x + p.shift) % arrayLength(&src)];
    dst[id.x] = dst[id.x] * p.mul + other + p.add;
}
"#;

const TO_TEX_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var tex: texture_storage_2d<r32uint, write>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let side = 64u;
    if (id.x >= side * side) { return; }
    textureStore(tex, vec2<i32>(i32(id.x % side), i32(id.x / side)), vec4<u32>(src[id.x] ^ 0x5bd1e995u, 0u, 0u, 1u));
}
"#;

const FROM_TEX_WGSL: &str = r#"
@group(0) @binding(0) var tex: texture_2d<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let side = 64u;
    if (id.x >= side * side) { return; }
    let flipped = (side * side - 1u) - id.x;
    dst[id.x] = dst[id.x] + textureLoad(tex, vec2<i32>(i32(flipped % side), i32(flipped / side)), 0).x;
}
"#;

struct Kernels {
    mix: GpuComputePipeline,
    to_tex: GpuComputePipeline,
    from_tex: GpuComputePipeline,
}

impl Kernels {
    fn new(device: &GpuDevice) -> Self {
        Self {
            mix: device.create_compute_pipeline(MIX_WGSL, "main", "replay-proof mix"),
            to_tex: device.create_compute_pipeline(TO_TEX_WGSL, "main", "replay-proof to_tex"),
            from_tex: device.create_compute_pipeline(FROM_TEX_WGSL, "main", "replay-proof from_tex"),
        }
    }
}

/// One run's state: three buffers the chain mixes, a texture, the indirect
/// arguments, and (for the replay run) the span cache.
struct Rig {
    buffers: [GpuBuffer; 3],
    texture: GpuTexture,
    args: GpuBuffer,
    cache: Option<GpuReplayCache>,
}

impl Rig {
    fn new(device: &GpuDevice, replay: bool) -> Self {
        let buffers = std::array::from_fn(|b| {
            let buffer = device.create_buffer_shared((N * 4) as u64);
            write_u32s(&buffer, &(0..N as u32).map(|i| i.wrapping_mul(2654435761).wrapping_add(b as u32)).collect::<Vec<_>>());
            buffer
        });
        let texture = device.create_texture(&GpuTextureDesc {
            width: SIDE,
            height: SIDE,
            depth: 1,
            format: GpuTextureFormat::R32Uint,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
            label: "replay-proof texture",
            mip_levels: 1,
        });
        let args = device.create_buffer_shared(12);
        write_u32s(&args, &GROUPS);
        Self { buffers, texture, args, cache: replay.then(GpuReplayCache::default) }
    }

    fn contents(&self) -> Vec<Vec<u32>> {
        self.buffers.iter().map(read_u32s).collect()
    }

    fn stats(&self) -> GpuReplayStats {
        self.cache.as_ref().expect("a replay rig").stats()
    }
}

/// What a frame encodes: the step to leave out and the step to shrink, so a
/// frame can differ from the recording in structure.
#[derive(Clone, Copy, Default)]
struct Shape {
    skip: Option<usize>,
    half_grid: Option<usize>,
}

const STEPS: usize = 30;

/// Encode one frame of the chain: `STEPS` mixes whose uniforms change every
/// frame, broken at step 10 by a texture round trip, at 15 by a blit and at
/// 20 by an indirect dispatch.
fn encode_frame(enc: &mut GpuEncoder, k: &Kernels, rig: &Rig, frame: u32, shape: Shape) {
    for step in 0..STEPS {
        if shape.skip == Some(step) {
            continue;
        }
        let src = &rig.buffers[step % 3];
        let dst = &rig.buffers[(step + 1) % 3];
        let params: [u32; 4] = [
            1_664_525 + step as u32,
            frame.wrapping_mul(7) + step as u32,
            (step as u32 * 37 + frame) % N as u32,
            0,
        ];
        let bytes: &[u8] = bytemuck_u32(&params);
        let bindings = [
            GpuBinding::Buffer { binding: 0, buffer: src, offset: 0 },
            GpuBinding::Buffer { binding: 1, buffer: dst, offset: 0 },
            GpuBinding::Bytes { binding: 2, data: bytes },
        ];
        match step {
            10 => {
                enc.dispatch_compute(
                    &k.to_tex,
                    &[
                        GpuBinding::Buffer { binding: 0, buffer: src, offset: 0 },
                        GpuBinding::Texture { binding: 1, texture: &rig.texture },
                    ],
                    GROUPS,
                    "replay-proof to_tex",
                );
                enc.dispatch_compute(
                    &k.from_tex,
                    &[
                        GpuBinding::Texture { binding: 0, texture: &rig.texture },
                        GpuBinding::Buffer { binding: 1, buffer: dst, offset: 0 },
                    ],
                    GROUPS,
                    "replay-proof from_tex",
                );
            }
            15 => enc.copy_buffer_to_buffer(src, dst, (N * 2) as u64),
            20 => enc.dispatch_compute_indirect(&k.mix, &bindings, &rig.args, 0, "replay-proof indirect"),
            _ => {
                let groups = if shape.half_grid == Some(step) { [GROUPS[0] / 2, 1, 1] } else { GROUPS };
                enc.dispatch_compute(&k.mix, &bindings, groups, "replay-proof mix");
            }
        }
    }
}

fn run_frame(device: &GpuDevice, k: &Kernels, rig: &mut Rig, frame: u32, shape: Shape) {
    let mut enc = device.create_encoder("replay-proof");
    if let Some(cache) = rig.cache.take() {
        enc.begin_replay(device, cache);
    }
    encode_frame(&mut enc, k, rig, frame, shape);
    if enc.replay.is_some() {
        rig.cache = Some(enc.end_replay());
    }
    enc.commit_and_wait_completed();
}

fn bytemuck_u32(v: &[u32]) -> &[u8] {
    // Safety: u32 has no padding and any byte pattern is a valid u8.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}

fn write_u32s(buffer: &GpuBuffer, values: &[u32]) {
    let ptr = buffer.mapped_ptr().expect("shared buffer");
    // Safety: the buffer is shared, at least this large, and not in use.
    unsafe { std::ptr::copy_nonoverlapping(values.as_ptr().cast::<u8>(), ptr, std::mem::size_of_val(values)) };
}

fn read_u32s(buffer: &GpuBuffer) -> Vec<u32> {
    let ptr = buffer.mapped_ptr().expect("shared buffer").cast::<u32>();
    // Safety: the buffer is shared and every frame waited for completion.
    unsafe { std::slice::from_raw_parts(ptr, buffer.size as usize / 4) }.to_vec()
}

/// Recordable dispatches per frame of `shape`: the mixes and the copy
/// (a word copy inside a span records, D9), minus the texture pair, the
/// indirect dispatch and whatever the shape leaves out.
fn recordable(shape: Shape) -> u64 {
    let breaks = [10, 20];
    (0..STEPS).filter(|s| !breaks.contains(s) && shape.skip != Some(*s)).count() as u64
}

#[test]
fn replay_matches_direct_bit_for_bit() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let mut direct = Rig::new(&device, false);
    let mut replay = Rig::new(&device, true);
    for frame in 0..10 {
        run_frame(&device, &k, &mut direct, frame, Shape::default());
        let before = replay.stats();
        run_frame(&device, &k, &mut replay, frame, Shape::default());
        assert_eq!(direct.contents(), replay.contents(), "frame {frame}: replay diverged from direct");
        let after = replay.stats();
        let (recorded, replayed) = (after.recorded - before.recorded, after.replayed - before.replayed);
        if frame == 0 {
            assert_eq!((recorded, replayed), (recordable(Shape::default()), 0), "frame 0 records");
        } else {
            assert_eq!((recorded, replayed), (0, recordable(Shape::default())), "frame {frame} replays");
        }
        // Breaks at the texture pair and the indirect dispatch; the copy
        // joins its stretch: three stretches a frame.
        assert_eq!(after.executes - before.executes, 3, "frame {frame}: one execute per stretch");
        assert_eq!(after.direct - before.direct, 3, "frame {frame}: texture pair and indirect run directly");
    }
    assert_eq!(replay.stats().ring_busy, 0);
}

/// A frame that differs in structure cuts the recording at the first
/// difference and records from there; output never differs from direct.
#[test]
fn replay_survives_structural_changes() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let mut direct = Rig::new(&device, false);
    let mut replay = Rig::new(&device, true);
    let shapes = [
        Shape::default(),
        Shape::default(),
        Shape { skip: Some(25), ..Shape::default() },
        Shape::default(),
        Shape { half_grid: Some(4), ..Shape::default() },
        Shape { half_grid: Some(4), ..Shape::default() },
        Shape { skip: Some(2), half_grid: Some(28) },
        Shape::default(),
    ];
    let mut recorded_per_frame = Vec::new();
    for (frame, shape) in shapes.iter().enumerate() {
        run_frame(&device, &k, &mut direct, frame as u32, *shape);
        let before = replay.stats().recorded;
        run_frame(&device, &k, &mut replay, frame as u32, *shape);
        assert_eq!(direct.contents(), replay.contents(), "frame {frame}: replay diverged from direct");
        recorded_per_frame.push(replay.stats().recorded - before);
    }
    // A changed frame records from its first difference to the end; a frame
    // shaped like the one before records nothing. Frame 2 skips step 25, so
    // it records steps 26 to 29.
    assert_eq!(recorded_per_frame[1], 0);
    assert_eq!(recorded_per_frame[2], 4);
    assert!(recorded_per_frame[3] > 0, "the recording now holds frame 2's shape");
    assert!(recorded_per_frame[4] > 0);
    assert_eq!(recorded_per_frame[5], 0, "the same shape twice replays");
    assert!(recorded_per_frame[6] > 0);
}

/// Every frame waits on the GPU for an event the CPU holds back, so each
/// span's entry stays in flight: three frames take the three entries, the
/// fourth finds none idle and encodes directly, and output still matches.
#[test]
fn replay_never_writes_an_entry_in_flight() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let mut direct = Rig::new(&device, false);
    for frame in 0..5 {
        run_frame(&device, &k, &mut direct, frame, Shape::default());
    }
    let mut replay = Rig::new(&device, true);
    let gate = device.create_event();
    for frame in 0..4 {
        let mut enc = device.create_encoder("replay-proof gated");
        enc.wait_event(&gate, 1);
        enc.begin_replay(&device, replay.cache.take().unwrap());
        encode_frame(&mut enc, &k, &replay, frame, Shape::default());
        replay.cache = Some(enc.end_replay());
        enc.commit();
    }
    let stats = replay.stats();
    assert_eq!(stats.ring_busy, 1, "the fourth frame finds every entry in flight");
    assert_eq!(stats.recorded, 3 * recordable(Shape::default()), "three entries recorded once each");
    assert_eq!(stats.replayed, 0);
    unsafe { gate.raw().setSignaledValue(1) };
    // Queue order: once this completes, every gated frame has too.
    device.create_encoder("replay-proof drain").commit_and_wait_completed();
    run_frame(&device, &k, &mut replay, 4, Shape::default());
    assert_eq!(replay.stats().replayed, recordable(Shape::default()), "a completed entry replays again");
    assert_eq!(direct.contents(), replay.contents());
}

#[test]
fn replay_steady_state_records_nothing() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let mut rig = Rig::new(&device, true);
    for frame in 0..3 {
        run_frame(&device, &k, &mut rig, frame, Shape::default());
    }
    let warm = rig.stats();
    for frame in 3..23 {
        run_frame(&device, &k, &mut rig, frame, Shape::default());
    }
    let after = rig.stats();
    assert_eq!(after.recorded, warm.recorded, "a warm ring records nothing");
    assert_eq!(after.store_allocations, warm.store_allocations, "a warm ring allocates nothing");
    assert_eq!(after.replayed - warm.replayed, 20 * recordable(Shape::default()));
}

/// A cache dropped while its entry is in flight: the arenas retire through
/// the fence and the command buffer keeps the executed chunks alive.
#[test]
fn replay_cache_dropped_in_flight_is_safe() {
    let device = GpuDevice::new();
    let frame_event = device.create_event();
    let (sender, mut retire_queue) = RetireQueue::new();
    device.set_retirement(RetireMark::new(frame_event.second_handle(), sender));
    let k = Kernels::new(&device);
    let mut direct = Rig::new(&device, false);
    let mut replay = Rig::new(&device, true);
    for frame in 0..2 {
        run_frame(&device, &k, &mut direct, frame, Shape::default());
    }
    run_frame(&device, &k, &mut replay, 0, Shape::default());

    let gate = device.create_event();
    let mut enc = device.create_encoder("replay-proof dropped cache");
    enc.wait_event(&gate, 1);
    enc.begin_replay(&device, replay.cache.take().unwrap());
    encode_frame(&mut enc, &k, &replay, 1, Shape::default());
    drop(enc.end_replay());
    enc.signal_event(&frame_event);
    enc.commit();
    retire_queue.drain();
    unsafe { gate.raw().setSignaledValue(1) };
    let mut tail = device.create_encoder("replay-proof tail");
    tail.signal_event(&frame_event);
    tail.commit_and_wait_completed();
    retire_queue.drain();
    assert_eq!(direct.contents(), replay.contents());
}

/// Word copies inside a span run as the copy kernel and match the blit a
/// direct encoder issues, byte for byte: random word offsets and sizes,
/// between two buffers and within one (disjoint ranges), chained so every
/// frame reads what the last one wrote.
#[test]
fn replay_copy_matches_blit() {
    const WORDS: u64 = 16 * 1024;
    let device = GpuDevice::new();
    let mut seed = 0x9e37_79b9_u64;
    let mut next = move |bound: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    // (source is the second buffer, source word, destination word, words).
    let mut copies = Vec::new();
    while copies.len() < 40 {
        let (within, words) = (next(3) == 0, 1 + next(WORDS / 8));
        let (src, dst) = (next(WORDS - words), next(WORDS - words));
        if within && src < dst + words && dst < src + words {
            continue;
        }
        copies.push((within, src, dst, words));
    }
    let run = |replay: bool| {
        let buffers: [GpuBuffer; 2] = std::array::from_fn(|b| {
            let buffer = device.create_buffer_shared(WORDS * 4);
            write_u32s(&buffer, &(0..WORDS as u32).map(|i| i.wrapping_mul(2_246_822_519).wrapping_add(b as u32)).collect::<Vec<_>>());
            buffer
        });
        let mut cache = replay.then(GpuReplayCache::default);
        let mut stats = Vec::new();
        for _ in 0..3 {
            let mut enc = device.create_encoder("replay-copy proof");
            if let Some(cache) = cache.take() {
                enc.begin_replay(&device, cache);
            }
            for &(within, src, dst, words) in &copies {
                let (from, to) = if within { (&buffers[1], &buffers[1]) } else { (&buffers[0], &buffers[1]) };
                enc.copy_buffer_range(from, src * 4, to, dst * 4, words * 4);
            }
            enc.copy_buffer_to_buffer(&buffers[1], &buffers[0], WORDS * 2);
            if enc.replay.is_some() {
                let done = enc.end_replay();
                stats.push(done.stats());
                cache = Some(done);
            }
            enc.commit_and_wait_completed();
        }
        (buffers.iter().map(read_u32s).collect::<Vec<_>>(), stats)
    };
    let (direct, _) = run(false);
    let (replayed, stats) = run(true);
    assert!(direct == replayed, "a replayed word copy differs from the blit");
    let n = copies.len() as u64 + 1;
    assert_eq!((stats[0].recorded, stats[0].replayed), (n, 0), "the first frame records every copy");
    assert_eq!((stats[2].recorded, stats[2].replayed), (n, 2 * n), "later frames replay them");
    assert_eq!(stats[2].executes, 3, "one stretch a frame");
}

/// Reports CPU µs to encode one stretch of 2, 6 and 26 dispatches directly
/// and replayed (open, validate, execute, close), and GPU ms for both. The
/// design's kill line: a replayed stretch of 6 costs at most half of direct.
#[test]
fn replay_cpu_cost_probe() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    const REPS: usize = 300;
    for len in [2usize, 6, 26] {
        let rig = Rig::new(&device, false);
        let encode = |enc: &mut GpuEncoder, rep: u32| {
            for step in 0..len {
                let params: [u32; 4] = [3, rep + step as u32, step as u32, 0];
                enc.dispatch_compute(
                    &k.mix,
                    &[
                        GpuBinding::Buffer { binding: 0, buffer: &rig.buffers[step % 3], offset: 0 },
                        GpuBinding::Buffer { binding: 1, buffer: &rig.buffers[(step + 1) % 3], offset: 0 },
                        GpuBinding::Bytes { binding: 2, data: bytemuck_u32(&params) },
                    ],
                    GROUPS,
                    "replay-probe mix",
                );
            }
        };
        let mut direct_us = Vec::with_capacity(REPS);
        let mut direct_gpu = 0.0;
        for rep in 0..REPS as u32 {
            let mut enc = device.create_encoder("replay-probe direct");
            let t = std::time::Instant::now();
            encode(&mut enc, rep);
            enc.end_current();
            direct_us.push(t.elapsed().as_secs_f64() * 1e6);
            direct_gpu += enc.commit_and_wait_completed_timed() * 1e3;
        }
        let mut cache = GpuReplayCache::default();
        let mut replay_us = Vec::with_capacity(REPS);
        let mut replay_gpu = 0.0;
        for rep in 0..REPS as u32 + 3 {
            let mut enc = device.create_encoder("replay-probe replay");
            let t = std::time::Instant::now();
            enc.begin_replay(&device, cache);
            encode(&mut enc, rep);
            cache = enc.end_replay();
            enc.end_current();
            let us = t.elapsed().as_secs_f64() * 1e6;
            let gpu = enc.commit_and_wait_completed_timed() * 1e3;
            if rep >= 3 {
                replay_us.push(us);
                replay_gpu += gpu;
            }
        }
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let (d, r) = (median(direct_us), median(replay_us));
        println!(
            "REPLAY PROBE {len:>2} dispatches: direct {d:7.1} us, replayed {r:7.1} us ({:.2}x), gpu direct {:.3} ms, replayed {:.3} ms",
            r / d,
            direct_gpu / REPS as f64,
            replay_gpu / REPS as f64,
        );
        assert_eq!(cache.stats().recorded, len as u64, "the probe replays after its first frame");
    }
}

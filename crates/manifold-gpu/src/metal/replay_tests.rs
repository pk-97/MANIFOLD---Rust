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

// ---- Gated segments (BUG-l2h3.24 lever A) ---------------------------------

/// Writes, per segment, the indirect arguments its dispatches use directly
/// and the execution range its replayed recording runs by: both from one
/// GPU-side flag, as a converged solver would write them.
const GATE_WGSL: &str = r#"
struct Gate { segments: u32, groups: u32, commands: u32, pad: u32 };
@group(0) @binding(0) var<storage, read> flags: array<u32>;
@group(0) @binding(1) var<storage, read_write> args: array<u32>;
@group(0) @binding(2) var<storage, read_write> ranges: array<u32>;
@group(0) @binding(3) var<uniform> gate: Gate;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let s = id.x;
    if (s >= gate.segments) { return; }
    let live = flags[s] != 0u;
    args[s * 3u] = select(0u, gate.groups, live);
    args[s * 3u + 1u] = 1u;
    args[s * 3u + 2u] = 1u;
    ranges[s * 2u] = 0u;
    ranges[s * 2u + 1u] = select(0u, gate.commands, live);
}
"#;

const SEGMENTS: usize = 4;
const SEGMENT_COMMANDS: u32 = 3;

struct GatedRig {
    rig: Rig,
    gate: GpuComputePipeline,
    flags: GpuBuffer,
    args: GpuBuffer,
    ranges: GpuBuffer,
}

impl GatedRig {
    fn new(device: &GpuDevice, replay: bool, segments: usize) -> Self {
        let rig = Rig::new(device, replay);
        let flags = device.create_buffer_shared((segments * 4) as u64);
        let args = device.create_buffer_shared((segments * 12) as u64);
        let ranges = device.create_buffer_shared(segments as u64 * crate::GATED_RANGE_BYTES);
        Self { rig, gate: device.create_compute_pipeline(GATE_WGSL, "main", "replay-proof gate"), flags, args, ranges }
    }

    fn set_flags(&self, live: &[u32]) {
        write_u32s(&self.flags, live);
    }
}

/// How one gated frame is shaped: dispatches issued per segment (the
/// declared length is `declared`), a segment to break with a texture
/// dispatch after its first command, and a segment to break with a plain
/// recordable mix after its first command.
#[derive(Clone, Copy)]
struct GatedShape {
    counts: [u32; SEGMENTS],
    declared: u32,
    break_in: Option<usize>,
    plain_in: Option<usize>,
}

impl Default for GatedShape {
    fn default() -> Self {
        Self { counts: [SEGMENT_COMMANDS; SEGMENTS], declared: SEGMENT_COMMANDS, break_in: None, plain_in: None }
    }
}

/// One gated frame: the gate kernel decides every segment on the GPU, then
/// `SEGMENTS` segments of mixes, each gated by its own range entry, and a
/// trailing ungated mix so the stretch after the last segment is covered.
fn encode_gated_frame(enc: &mut GpuEncoder, k: &Kernels, g: &GatedRig, frame: u32, shape: GatedShape) {
    let gate: [u32; 4] = [SEGMENTS as u32, GROUPS[0], shape.declared, 0];
    enc.dispatch_compute(
        &g.gate,
        &[
            GpuBinding::Buffer { binding: 0, buffer: &g.flags, offset: 0 },
            GpuBinding::Buffer { binding: 1, buffer: &g.args, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &g.ranges, offset: 0 },
            GpuBinding::Bytes { binding: 3, data: bytemuck_u32(&gate) },
        ],
        [1, 1, 1],
        "replay-proof gate",
    );
    let mut step = 0usize;
    let mut mix = |enc: &mut GpuEncoder, segment: Option<usize>| {
        let src = &g.rig.buffers[step % 3];
        let dst = &g.rig.buffers[(step + 1) % 3];
        let params: [u32; 4] = [1_664_525 + step as u32, frame.wrapping_mul(7) + step as u32, (step as u32 * 37 + frame) % N as u32, 0];
        let bindings = [
            GpuBinding::Buffer { binding: 0, buffer: src, offset: 0 },
            GpuBinding::Buffer { binding: 1, buffer: dst, offset: 0 },
            GpuBinding::Bytes { binding: 2, data: bytemuck_u32(&params) },
        ];
        match segment {
            Some(s) => enc.dispatch_compute_gated(&k.mix, &bindings, GROUPS, &g.args, (s * 12) as u64, "replay-proof gated mix"),
            None => enc.dispatch_compute(&k.mix, &bindings, GROUPS, "replay-proof mix"),
        }
        step += 1;
    };
    for s in 0..SEGMENTS {
        enc.begin_gated_segment(&g.ranges, s as u32, shape.declared);
        for i in 0..shape.counts[s] {
            mix(enc, Some(s));
            if i == 0 && shape.plain_in == Some(s) {
                mix(enc, None);
            }
            if i == 0 && shape.break_in == Some(s) {
                enc.dispatch_compute(
                    &k.to_tex,
                    &[
                        GpuBinding::Buffer { binding: 0, buffer: &g.rig.buffers[0], offset: 0 },
                        GpuBinding::Texture { binding: 1, texture: &g.rig.texture },
                    ],
                    GROUPS,
                    "replay-proof to_tex",
                );
            }
        }
    }
    enc.end_gated_segments();
    mix(enc, None);
}

fn run_gated_frame(device: &GpuDevice, k: &Kernels, g: &mut GatedRig, frame: u32, shape: GatedShape) -> f64 {
    let mut enc = device.create_encoder("replay-proof gated");
    if let Some(cache) = g.rig.cache.take() {
        enc.begin_replay(device, cache);
    }
    encode_gated_frame(&mut enc, k, g, frame, shape);
    if enc.replay.is_some() {
        g.rig.cache = Some(enc.end_replay());
    }
    enc.commit_and_wait_completed_timed() * 1e3
}

/// Which segments run on frame `frame`: a pattern that changes every frame
/// and leaves some segments dead on every frame.
fn live_pattern(frame: u32) -> [u32; SEGMENTS] {
    std::array::from_fn(|s| u32::from(!(frame as usize + s).is_multiple_of(3)))
}

/// Dead segments execute with the GPU-written range `{0, 0}` and run
/// nothing; live ones run every command. Output matches direct encoding
/// (today's indirect dispatches) bit for bit while the live pattern changes
/// every frame, with no re-recording. Under `MTL_DEBUG_LAYER=1` this is also
/// the legality probe for the zero-length execute.
#[test]
fn replay_segment_skips_dead_segments() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let mut direct = GatedRig::new(&device, false, SEGMENTS);
    let mut replay = GatedRig::new(&device, true, SEGMENTS);
    println!(
        "MTL_DEBUG_LAYER={}",
        std::env::var("MTL_DEBUG_LAYER").unwrap_or_else(|_| "unset (run with MTL_DEBUG_LAYER=1 for the legality probe)".into())
    );
    for frame in 0..9 {
        let live = live_pattern(frame);
        direct.set_flags(&live);
        replay.set_flags(&live);
        run_gated_frame(&device, &k, &mut direct, frame, GatedShape::default());
        let before = replay.rig.stats();
        run_gated_frame(&device, &k, &mut replay, frame, GatedShape::default());
        assert_eq!(direct.rig.contents(), replay.rig.contents(), "frame {frame}: gated replay diverged from direct (live {live:?})");
        let after = replay.rig.stats();
        let per_frame = SEGMENTS as u64 * u64::from(SEGMENT_COMMANDS) + 2;
        if frame == 0 {
            assert_eq!(after.recorded - before.recorded, per_frame, "frame 0 records the gate, every segment and the tail");
        } else {
            assert_eq!(after.recorded - before.recorded, 0, "frame {frame}: a changed live pattern records nothing");
            assert_eq!(after.replayed - before.replayed, per_frame);
        }
        assert_eq!(after.segments_replayed - before.segments_replayed, SEGMENTS as u64, "every segment executes, dead ones empty");
        assert_eq!(after.segments_direct - before.segments_direct, 0);
        if frame > 0 {
            assert_eq!(after.store_allocations, before.store_allocations, "a warm ring allocates no segment buffers");
        }
        // The gate kernel's chunk run, four segments, the tail's chunk run.
        assert_eq!(after.executes - before.executes, SEGMENTS as u64 + 2);
    }
    assert_eq!(replay.rig.stats().ring_busy, 0);
}

/// A segment that issues more dispatches than it declared runs the extra
/// ones directly; one that issues fewer leaves no-op slots; a direct
/// dispatch inside a segment cuts it and the rest of that segment runs
/// directly. Every shape matches direct encoding, and a shape repeated
/// replays.
#[test]
fn replay_segment_count_mismatch_runs_direct() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let mut direct = GatedRig::new(&device, false, SEGMENTS);
    let mut replay = GatedRig::new(&device, true, SEGMENTS);
    let over = GatedShape { counts: [3, 5, 3, 3], ..GatedShape::default() };
    let under = GatedShape { counts: [3, 1, 3, 2], ..GatedShape::default() };
    let longer = GatedShape { counts: [4, 4, 4, 4], declared: 4, ..GatedShape::default() };
    let broken = GatedShape { break_in: Some(2), ..GatedShape::default() };
    let plain = GatedShape { plain_in: Some(1), ..GatedShape::default() };
    let shapes = [
        GatedShape::default(),
        over,
        over,
        under,
        under,
        GatedShape::default(),
        longer,
        broken,
        broken,
        GatedShape::default(),
        GatedShape::default(),
        plain,
        plain,
        GatedShape::default(),
        GatedShape::default(),
    ];
    let mut stats = Vec::new();
    for (frame, shape) in shapes.iter().enumerate() {
        let frame = frame as u32;
        let live = live_pattern(frame);
        direct.set_flags(&live);
        replay.set_flags(&live);
        run_gated_frame(&device, &k, &mut direct, frame, *shape);
        let before = replay.rig.stats();
        run_gated_frame(&device, &k, &mut replay, frame, *shape);
        assert_eq!(direct.rig.contents(), replay.rig.contents(), "frame {frame}: gated replay diverged from direct (live {live:?})");
        let after = replay.rig.stats();
        stats.push((after.recorded - before.recorded, after.segments_direct - before.segments_direct, after.segments_replayed - before.segments_replayed));
    }
    let (recorded, segments_direct, segments_replayed): (Vec<_>, Vec<_>, Vec<_>) =
        stats.iter().fold((vec![], vec![], vec![]), |mut acc, s| {
            acc.0.push(s.0);
            acc.1.push(s.1);
            acc.2.push(s.2);
            acc
        });
    assert_eq!(segments_direct[1], 2, "two dispatches over the declared length run directly");
    assert_eq!(recorded[2], 0, "the over-long shape replays once recorded");
    assert_eq!(segments_direct[2], 2);
    assert!(recorded[3] > 0, "fewer dispatches cut the recording");
    assert_eq!((recorded[4], segments_direct[4]), (0, 0), "the shorter shape replays, no-op slots and all");
    assert!(recorded[6] > 0, "a changed declared length is a new segment");
    assert_eq!(segments_replayed[6], SEGMENTS as u64);
    assert!(segments_direct[7] > 0, "the rest of a cut segment runs directly");
    assert_eq!(recorded[8], 0, "the cut shape replays as recorded");
    assert_eq!(recorded[10], 0);
    assert_eq!(segments_replayed[10], SEGMENTS as u64);
    let rest = u64::from(SEGMENT_COMMANDS - 1);
    assert_eq!(segments_direct[11], rest, "a plain dispatch inside a segment breaks it: the rest runs directly");
    assert_eq!((recorded[12], segments_direct[12]), (0, rest), "the broken shape replays as recorded, its tail still direct");
    assert_eq!(segments_replayed[12], SEGMENTS as u64, "a broken segment still executes whole, once");
    assert_eq!((recorded[14], segments_direct[14]), (0, 0), "the default shape replays again once re-recorded");
}

/// Reports GPU µs per dead segment execute (the kill line: under 3 µs) next
/// to today's cost of the same dead rounds as zero-group indirect dispatches.
#[test]
fn replay_segment_dead_cost_probe() {
    const SEGS: usize = 64;
    const CMDS: u32 = 8;
    const REPS: usize = 60;
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let dead = [0u32; SEGS];
    // A frame of `segments` dead segments: the gate kernel then the segments.
    let frame = |enc: &mut GpuEncoder, g: &GatedRig, segments: usize| {
        let gate: [u32; 4] = [SEGS as u32, GROUPS[0], CMDS, 0];
        enc.dispatch_compute(
            &g.gate,
            &[
                GpuBinding::Buffer { binding: 0, buffer: &g.flags, offset: 0 },
                GpuBinding::Buffer { binding: 1, buffer: &g.args, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &g.ranges, offset: 0 },
                GpuBinding::Bytes { binding: 3, data: bytemuck_u32(&gate) },
            ],
            [1, 1, 1],
            "replay-probe gate",
        );
        let params: [u32; 4] = [3, 1, 2, 0];
        for s in 0..segments {
            enc.begin_gated_segment(&g.ranges, s as u32, CMDS);
            for _ in 0..CMDS {
                enc.dispatch_compute_gated(
                    &k.mix,
                    &[
                        GpuBinding::Buffer { binding: 0, buffer: &g.rig.buffers[0], offset: 0 },
                        GpuBinding::Buffer { binding: 1, buffer: &g.rig.buffers[1], offset: 0 },
                        GpuBinding::Bytes { binding: 2, data: bytemuck_u32(&params) },
                    ],
                    GROUPS,
                    &g.args,
                    (s * 12) as u64,
                    "replay-probe gated mix",
                );
            }
        }
        enc.end_gated_segments();
    };
    let measure = |replay: bool, segments: usize| -> f64 {
        let mut g = GatedRig::new(&device, replay, SEGS);
        g.set_flags(&dead);
        let mut gpu = Vec::with_capacity(REPS);
        let mut warm = GpuReplayStats::default();
        for rep in 0..REPS + 3 {
            let mut enc = device.create_encoder("replay-probe dead segments");
            if let Some(cache) = g.rig.cache.take() {
                enc.begin_replay(&device, cache);
            }
            frame(&mut enc, &g, segments);
            if enc.replay.is_some() {
                g.rig.cache = Some(enc.end_replay());
            }
            let ms = enc.commit_and_wait_completed_timed() * 1e3;
            if rep == 2 && replay {
                warm = g.rig.cache.as_ref().unwrap().stats();
            }
            if rep >= 3 {
                gpu.push(ms);
            }
        }
        if replay && segments > 0 {
            // The first visit may fall short of arena space (the store grows
            // between spans); warm frames replay every dead round.
            let stats = g.rig.cache.as_ref().unwrap().stats();
            assert_eq!(stats.segments_direct - warm.segments_direct, 0, "every warm dead round replays");
            assert_eq!(stats.segments_replayed - warm.segments_replayed, REPS as u64 * SEGS as u64);
            assert_eq!(stats.recorded - warm.recorded, 0, "a warm ring records nothing");
        }
        median(gpu)
    };
    let gate_only = measure(true, 0);
    let replayed = measure(true, SEGS);
    let direct = measure(false, SEGS);
    let per_segment_us = (replayed - gate_only) * 1e3 / SEGS as f64;
    let per_dead_dispatch_us = (direct - gate_only) * 1e3 / (SEGS as f64 * f64::from(CMDS));
    println!(
        "SEGMENT PROBE {SEGS} dead segments x {CMDS}: gate only {gate_only:.3} ms, replayed {replayed:.3} ms ({per_segment_us:.2} us per dead segment; kill line 3 us), direct {direct:.3} ms ({per_dead_dispatch_us:.2} us per zero-group indirect dispatch)"
    );
}

// ---- Gated templates (docs/GPU_FLIP_PRESSURE_CAP_DESIGN.md section 3) ------

/// Arms a template frame the way the pressure solve's arm does: `live`
/// copies' range entries live at `commands`, the rest dead, and the gate
/// triples (the mixes' at word 0, the countdown's at word 3) on when any
/// copy runs; the countdown's state restarts.
const TEMPLATE_ARM_WGSL: &str = r#"
struct Arm { live: u32, copies: u32, first: u32, stride: u32, commands: u32, entries: u32, groups: u32, pad: u32 };
@group(0) @binding(0) var<storage, read_write> state: array<u32>;
@group(0) @binding(1) var<storage, read_write> args: array<u32>;
@group(0) @binding(2) var<storage, read_write> ranges: array<u32>;
@group(0) @binding(3) var<uniform> arm: Arm;

@compute @workgroup_size(1)
fn main() {
    state[0] = arm.live;
    state[1] = 0u;
    let on = arm.live > 0u;
    args[0] = select(0u, arm.groups, on);
    args[1] = 1u;
    args[2] = 1u;
    args[3] = select(0u, 1u, on);
    args[4] = 1u;
    args[5] = 1u;
    for (var e = 0u; e < arm.entries; e = e + 1u) {
        ranges[2u * e] = 0u;
        ranges[2u * e + 1u] = 0u;
    }
    for (var c = 0u; c < arm.live && c < arm.copies; c = c + 1u) {
        ranges[2u * (arm.first + c * arm.stride) + 1u] = arm.commands;
    }
}
"#;

/// A round's last dispatch, as the solver's check: counts the round and,
/// after the `live`-th, switches the gate triples off, so direct copies
/// stop where the replayed ranges do.
const TEMPLATE_COUNTDOWN_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read_write> state: array<u32>;
@group(0) @binding(1) var<storage, read_write> args: array<u32>;

@compute @workgroup_size(1)
fn main() {
    if args[3] == 0u {
        return;
    }
    let done = state[1] + 1u;
    state[1] = done;
    if done >= state[0] {
        args[0] = 0u;
        args[3] = 0u;
    }
}
"#;

/// The mix, returning when the round's gate is off: a round a chunk runs
/// after the countdown stopped the template writes nothing.
const GUARDED_MIX_WGSL: &str = r#"
struct Params { mul: u32, add: u32, shift: u32, pad: u32 };
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@group(0) @binding(2) var<uniform> p: Params;
@group(0) @binding(3) var<storage, read> gate: array<u32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (gate[0] == 0u) { return; }
    let n = arrayLength(&dst);
    if (id.x >= n) { return; }
    let other = src[(id.x + p.shift) % arrayLength(&src)];
    dst[id.x] = dst[id.x] * p.mul + other + p.add;
}
"#;

const TEMPLATE_ENTRIES: u32 = 32;

struct TemplateRig {
    rig: Rig,
    arm: GpuComputePipeline,
    countdown: GpuComputePipeline,
    mix: GpuComputePipeline,
    state: GpuBuffer,
    args: GpuBuffer,
    ranges: GpuBuffer,
}

impl TemplateRig {
    fn new(device: &GpuDevice, replay: bool) -> Self {
        Self {
            rig: Rig::new(device, replay),
            arm: device.create_compute_pipeline(TEMPLATE_ARM_WGSL, "main", "template-proof arm"),
            countdown: device.create_compute_pipeline(TEMPLATE_COUNTDOWN_WGSL, "main", "template-proof countdown"),
            mix: device.create_compute_pipeline(GUARDED_MIX_WGSL, "main", "template-proof guarded mix"),
            state: device.create_buffer_shared(8),
            args: device.create_buffer_shared(24),
            ranges: device.create_buffer_shared(u64::from(TEMPLATE_ENTRIES) * crate::GATED_RANGE_BYTES),
        }
    }

    /// The three mix buffers and the countdown's state: everything a frame writes.
    fn contents(&self) -> Vec<Vec<u32>> {
        let mut all = self.rig.contents();
        all.push(read_u32s(&self.state));
        all
    }
}

/// One template frame's shape: `prefix` plain mixes, the arm, the template
/// (`mixes` gated mixes and the countdown, declared as `declared` commands,
/// `copies` copies from entry `first` by `stride`, `live` of them running),
/// then one trailing plain mix. `err` makes the body fail after its mixes.
#[derive(Clone, Copy)]
struct TemplateSpec {
    prefix: usize,
    mixes: u32,
    declared: u32,
    copies: u32,
    first: u32,
    stride: u32,
    live: u32,
    err: bool,
    /// Leave the template out (the reference for a failed walk).
    skip: bool,
    /// Most rounds an execute; past 1 the CPU writes the executes' range
    /// entries and the arm writes none.
    chunk: u32,
}

impl Default for TemplateSpec {
    fn default() -> Self {
        Self { prefix: 2, mixes: 4, declared: 5, copies: 6, first: 1, stride: 1, live: 3, err: false, skip: false, chunk: 1 }
    }
}

fn mix_params(frame: u32, step: usize) -> [u32; 4] {
    [1_664_525 + step as u32, frame.wrapping_mul(7) + step as u32, (step as u32 * 37 + frame) % N as u32, 0]
}

fn encode_template_frame(enc: &mut GpuEncoder, k: &Kernels, t: &TemplateRig, frame: u32, spec: TemplateSpec) -> Result<(), String> {
    let plain = |enc: &mut GpuEncoder, step: usize| {
        let params = mix_params(frame, step);
        enc.dispatch_compute(
            &k.mix,
            &[
                GpuBinding::Buffer { binding: 0, buffer: &t.rig.buffers[step % 3], offset: 0 },
                GpuBinding::Buffer { binding: 1, buffer: &t.rig.buffers[(step + 1) % 3], offset: 0 },
                GpuBinding::Bytes { binding: 2, data: bytemuck_u32(&params) },
            ],
            GROUPS,
            "template-proof plain mix",
        );
    };
    for step in 0..spec.prefix {
        plain(enc, step);
    }
    let arm: [u32; 8] = if spec.chunk > 1 {
        let mut words = vec![0u32; 2 * TEMPLATE_ENTRIES as usize];
        for (j, (start, rounds)) in crate::template_chunks(spec.copies, spec.chunk).enumerate() {
            let entry = (spec.first + j as u32 * spec.stride) as usize;
            words[2 * entry + 1] = if start < spec.live { rounds * spec.declared } else { 0 };
        }
        write_u32s(&t.ranges, &words);
        [spec.live, 0, 0, 0, 0, 0, GROUPS[0], 0]
    } else {
        [spec.live, spec.copies, spec.first, spec.stride, spec.declared, TEMPLATE_ENTRIES, GROUPS[0], 0]
    };
    enc.dispatch_compute(
        &t.arm,
        &[
            GpuBinding::Buffer { binding: 0, buffer: &t.state, offset: 0 },
            GpuBinding::Buffer { binding: 1, buffer: &t.args, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &t.ranges, offset: 0 },
            GpuBinding::Bytes { binding: 3, data: bytemuck_u32(&arm) },
        ],
        [1, 1, 1],
        "template-proof arm",
    );
    let mut result = Ok(());
    if !spec.skip {
        let at = TemplateRanges { ranges: &t.ranges, first: spec.first, stride: spec.stride, chunk: spec.chunk };
        result = enc.repeat_gated_template(at, spec.declared, spec.copies, |rec| {
            for i in 0..spec.mixes as usize {
                let step = 100 + i;
                let params = mix_params(frame, step);
                rec.dispatch_gated(
                    &t.mix,
                    &[
                        GpuBinding::Buffer { binding: 0, buffer: &t.rig.buffers[i % 3], offset: 0 },
                        GpuBinding::Buffer { binding: 1, buffer: &t.rig.buffers[(i + 1) % 3], offset: 0 },
                        GpuBinding::Bytes { binding: 2, data: bytemuck_u32(&params) },
                        GpuBinding::Buffer { binding: 3, buffer: &t.args, offset: 0 },
                    ],
                    GROUPS,
                    &t.args,
                    0,
                    "template-proof gated mix",
                );
            }
            if spec.err {
                return Err("template-proof body error".into());
            }
            rec.dispatch_gated(
                &t.countdown,
                &[GpuBinding::Buffer { binding: 0, buffer: &t.state, offset: 0 }, GpuBinding::Buffer { binding: 1, buffer: &t.args, offset: 0 }],
                [1, 1, 1],
                &t.args,
                12,
                "template-proof countdown",
            );
            Ok(())
        });
    }
    plain(enc, 200);
    result
}

fn run_template_frame(device: &GpuDevice, k: &Kernels, t: &mut TemplateRig, frame: u32, spec: TemplateSpec) -> Result<(), String> {
    let mut enc = device.create_encoder("template-proof");
    if let Some(cache) = t.rig.cache.take() {
        enc.begin_replay(device, cache);
    }
    let result = encode_template_frame(&mut enc, k, t, frame, spec);
    if enc.replay.is_some() {
        t.rig.cache = Some(enc.end_replay());
    }
    enc.commit_and_wait_completed();
    result
}

/// Per-frame deltas of the stats a template proof reads.
fn delta(after: GpuReplayStats, before: GpuReplayStats) -> GpuReplayStats {
    GpuReplayStats {
        replayed: after.replayed - before.replayed,
        recorded: after.recorded - before.recorded,
        direct: after.direct - before.direct,
        executes: after.executes - before.executes,
        ring_busy: after.ring_busy - before.ring_busy,
        store_allocations: after.store_allocations - before.store_allocations,
        segments_replayed: after.segments_replayed - before.segments_replayed,
        segments_direct: after.segments_direct - before.segments_direct,
        templates_recorded: after.templates_recorded - before.templates_recorded,
        templates_replayed: after.templates_replayed - before.templates_replayed,
        templates_direct: after.templates_direct - before.templates_direct,
    }
}

/// Runs `specs` on a direct rig and a replay rig, frame by frame, and
/// asserts equal output after each (the mix is not idempotent, so a copy
/// run twice or not at all shows). Returns the replay rig's per-frame deltas.
fn template_frames(device: &GpuDevice, k: &Kernels, specs: &[TemplateSpec]) -> (TemplateRig, Vec<GpuReplayStats>) {
    let mut direct = TemplateRig::new(device, false);
    let mut replay = TemplateRig::new(device, true);
    let mut deltas = Vec::new();
    for (frame, spec) in specs.iter().enumerate() {
        let frame = frame as u32;
        run_template_frame(device, k, &mut direct, frame, *spec).expect("the direct frame encodes");
        let before = replay.rig.stats();
        run_template_frame(device, k, &mut replay, frame, *spec).expect("the replayed frame encodes");
        assert_eq!(direct.contents(), replay.contents(), "frame {frame} {:?}: the template diverged from direct", (spec.live, spec.copies, spec.stride, spec.mixes, spec.declared));
        deltas.push(delta(replay.rig.stats(), before));
    }
    (replay, deltas)
}

/// A template records once, replays warm with nothing recorded and no
/// round direct, executes once per copy (dead copies empty), and re-records
/// exactly one template when its copy count, stride or length changes.
#[test]
fn replay_template_matches_direct_and_replays_warm() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let base = TemplateSpec::default();
    let specs = [
        base,
        TemplateSpec { live: 6, ..base },
        TemplateSpec { live: 0, ..base },
        TemplateSpec { live: 1, ..base },
        TemplateSpec { copies: 9, live: 7, ..base },
        TemplateSpec { copies: 9, live: 2, ..base },
        TemplateSpec { stride: 2, live: 4, ..base },
        TemplateSpec { stride: 2, live: 5, ..base },
        TemplateSpec { mixes: 5, declared: 6, ..base },
        TemplateSpec { mixes: 5, declared: 6, live: 2, ..base },
        base,
        base,
    ];
    let (_, d) = template_frames(&device, &k, &specs);
    let per_frame = |i: usize| u64::from(specs[i].declared) + specs[i].prefix as u64 + 2;
    assert_eq!((d[0].templates_recorded, d[0].recorded), (1, per_frame(0)), "the first visit records the template once");
    for (i, (s, spec)) in d.iter().zip(&specs).enumerate() {
        assert_eq!(s.templates_direct, 0, "frame {i}: the template never ran directly");
        assert_eq!(s.segments_direct, 0, "frame {i}: no gated dispatch ran directly");
        assert_eq!(s.segments_replayed, u64::from(spec.copies), "frame {i}: one execute per copy");
        assert_eq!(s.templates_recorded + s.templates_replayed, 1, "frame {i}: one template a frame");
        // Prefix chunk run, then the template's copies, then the tail's run.
        assert_eq!(s.executes, u64::from(spec.copies) + 2, "frame {i}: executes");
    }
    for i in [1, 2, 3, 5, 7, 9, 11] {
        assert_eq!((d[i].recorded, d[i].templates_replayed, d[i].store_allocations), (0, 1, 0), "frame {i}: a warm template records and allocates nothing");
        assert_eq!(d[i].replayed, per_frame(i), "frame {i}: the walk validates one round");
    }
    for i in [4, 6, 8, 10] {
        assert_eq!(d[i].templates_recorded, 1, "frame {i}: a changed copy count, stride or length re-records one template");
        assert_eq!(d[i].recorded, u64::from(specs[i].declared) + 1, "frame {i}: the template and the tail after it re-record");
    }
}

/// Fewer or more dispatches than declared: the walk rolls back whole,
/// nothing of it executes, the template runs directly copy by copy, and the
/// declared shape records again afterwards.
#[test]
fn replay_template_count_mismatch_rolls_back_whole() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let base = TemplateSpec::default();
    let under = TemplateSpec { declared: base.declared + 1, ..base };
    let over = TemplateSpec { declared: base.declared - 1, ..base };
    let specs = [base, base, under, under, base, base, over, over, base];
    let (replay, d) = template_frames(&device, &k, &specs);
    for i in [2, 3, 6, 7] {
        let issued = u64::from(base.mixes) + 1;
        assert_eq!(d[i].templates_direct, 1, "frame {i}: the mismatched template runs directly");
        assert_eq!(d[i].templates_recorded + d[i].templates_replayed, 0);
        assert_eq!(d[i].segments_replayed, 0, "frame {i}: nothing of the walk executes");
        // Copies run until the countdown switches the gate off; each issues
        // every dispatch of the body, live or not.
        assert_eq!(d[i].segments_direct, u64::from(base.copies) * issued, "frame {i}: every copy runs directly");
    }
    assert_eq!(d[4].templates_recorded, 1, "the declared shape records again after a mismatch");
    assert_eq!(d[5].templates_replayed, 1);
    assert_eq!(d[8].templates_recorded, 1);
    assert_eq!(replay.rig.cache.as_ref().unwrap().segment_buffers(), 1, "no rolled-back segment buffer survives");
}

/// A body error rolls the walk back and comes out of the call; the frame's
/// output is the frame without the template, the store keeps nothing of
/// the walk, and the next frame records the template normally.
#[test]
fn replay_template_body_error_rolls_back() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let base = TemplateSpec::default();
    let mut reference = TemplateRig::new(&device, false);
    let mut replay = TemplateRig::new(&device, true);
    for (frame, (ours, theirs)) in [(base, base), (TemplateSpec { err: true, ..base }, TemplateSpec { skip: true, ..base }), (base, base)].into_iter().enumerate() {
        let frame = frame as u32;
        run_template_frame(&device, &k, &mut reference, frame, theirs).expect("the reference encodes");
        let before = replay.rig.stats();
        let result = run_template_frame(&device, &k, &mut replay, frame, ours);
        let d = delta(replay.rig.stats(), before);
        assert_eq!(reference.contents(), replay.contents(), "frame {frame}: output");
        if ours.err {
            assert_eq!(result, Err("template-proof body error".into()));
            assert_eq!((d.segments_replayed, d.segments_direct, d.templates_direct), (0, 0, 0), "nothing of the failed walk runs");
            assert_eq!(replay.rig.cache.as_ref().unwrap().segment_buffers(), 0, "the rolled-back walk leaves no segment buffer");
        } else {
            result.expect("encodes");
        }
    }
    assert_eq!(replay.rig.stats().templates_recorded, 2, "the template records before and after the error");
}

/// Inline bytes per command with `pipeline`'s sizes buffer.
fn arena_bytes_per(pipeline: &GpuComputePipeline, uniform: usize) -> usize {
    uniform.next_multiple_of(crate::replay::BYTES_ALIGN) + if pipeline.needs_sizes_buffer { crate::replay::BYTES_ALIGN } else { 0 }
}

/// The arena fills exactly before the template, so its first dispatch makes
/// a segment buffer and then finds no room for its bytes: the zero-command
/// failure. The buffer is dropped, the template runs directly, and the whole
/// template is counted short so the next visit records it.
#[test]
fn replay_template_zero_command_failure_drops_its_buffer() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let probe = TemplateRig::new(&device, false);
    let (mix, arm) = (arena_bytes_per(&k.mix, 16), arena_bytes_per(&probe.arm, 32));
    let prefix = (crate::replay::ARENA_BYTES - arm) / mix;
    assert_eq!(prefix * mix + arm, crate::replay::ARENA_BYTES, "the prefix fills the arena exactly ({mix} bytes a mix, {arm} the arm)");
    assert!(prefix < crate::replay::CHUNK_COMMANDS, "the prefix fits one chunk");
    let spec = TemplateSpec { prefix, ..TemplateSpec::default() };
    let (replay, d) = template_frames(&device, &k, &[spec, spec, spec]);
    assert_eq!(d[0].templates_direct, 1, "no room for the first command's bytes");
    assert_eq!(d[0].segments_replayed, 0);
    assert_eq!(d[1].templates_recorded, 1, "the grown store records the template");
    assert_eq!((d[2].templates_replayed, d[2].recorded, d[2].store_allocations), (1, 0, 0), "and it replays warm");
    assert_eq!(replay.rig.cache.as_ref().unwrap().segment_buffers(), 1, "one template buffer, none leaked from the failed visit");
}

/// A template too big for the store fails part way through its walk: the
/// walked part rolls back, the template runs directly, and the shortage
/// covers the whole template, so the next visit records it in one go.
#[test]
fn replay_template_partial_walk_capacity_recovers() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let mixes = (crate::replay::ARENA_BYTES / arena_bytes_per(&k.mix, 16) + 100) as u32;
    let spec = TemplateSpec { mixes, declared: mixes + 1, copies: 3, live: 2, ..TemplateSpec::default() };
    let (_, d) = template_frames(&device, &k, &[spec, spec, spec, spec]);
    assert_eq!((d[0].templates_direct, d[0].segments_replayed), (1, 0), "the first walk runs out of room part way");
    assert_eq!(d[1].templates_recorded, 1, "the whole template fits the grown store");
    assert_eq!(d[1].segments_direct, 0);
    for i in [2, 3] {
        assert_eq!((d[i].templates_replayed, d[i].recorded, d[i].store_allocations), (1, 0, 0), "frame {i}: warm");
    }
}

/// Every entry in flight: the template finds no entry and runs directly;
/// once released, the ring records and replays it.
#[test]
fn replay_template_ring_busy_runs_direct() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let spec = TemplateSpec::default();
    let mut direct = TemplateRig::new(&device, false);
    let mut replay = TemplateRig::new(&device, true);
    for frame in 0..5 {
        run_template_frame(&device, &k, &mut direct, frame, spec).unwrap();
    }
    let gate = device.create_event();
    for frame in 0..4 {
        let mut enc = device.create_encoder("template-proof gated");
        enc.wait_event(&gate, 1);
        enc.begin_replay(&device, replay.rig.cache.take().unwrap());
        encode_template_frame(&mut enc, &k, &replay, frame, spec).unwrap();
        replay.rig.cache = Some(enc.end_replay());
        enc.commit();
    }
    let stats = replay.rig.stats();
    assert_eq!((stats.ring_busy, stats.templates_direct, stats.templates_recorded), (1, 1, 3), "the fourth frame finds every entry in flight");
    unsafe { gate.raw().setSignaledValue(1) };
    device.create_encoder("template-proof drain").commit_and_wait_completed();
    run_template_frame(&device, &k, &mut replay, 4, spec).unwrap();
    assert_eq!(replay.rig.stats().templates_replayed, 1, "a completed entry replays the template");
    assert_eq!(direct.contents(), replay.contents());
}

const REPLAY_OFF_CHILD: &str = "MANIFOLD_TEMPLATE_REPLAY_OFF_CHILD";

/// `MANIFOLD_ENCODE_REPLAY=0` is read once per process, so the replay-off
/// proof runs in a child process of this test binary.
#[test]
fn replay_template_replay_off_runs_direct() {
    let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args(["metal::replay_tests::replay_template_replay_off_child", "--exact", "--nocapture", "--test-threads=1"])
        .env("MANIFOLD_ENCODE_REPLAY", "0")
        .env(REPLAY_OFF_CHILD, "1")
        .output()
        .expect("the child runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "the replay-off child failed:\n{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("replay-off child: templates direct 3"), "the child ran its proof:\n{stdout}");
}

/// The child half of [`replay_template_replay_off_runs_direct`]; outside it
/// (no `MANIFOLD_TEMPLATE_REPLAY_OFF_CHILD`) it has nothing to prove.
#[test]
fn replay_template_replay_off_child() {
    if std::env::var(REPLAY_OFF_CHILD).is_err() {
        return;
    }
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let spec = TemplateSpec::default();
    let (replay, d) = template_frames(&device, &k, &[spec, spec, spec]);
    let stats = replay.rig.stats();
    assert_eq!((stats.recorded, stats.replayed, stats.segments_replayed), (0, 0, 0), "replay is off");
    assert!(d.iter().all(|d| d.templates_direct == 1));
    // A failing body encodes nothing with replay off either.
    let mut reference = TemplateRig::new(&device, false);
    run_template_frame(&device, &k, &mut reference, 0, TemplateSpec { skip: true, ..spec }).unwrap();
    let mut fresh = TemplateRig::new(&device, true);
    assert!(run_template_frame(&device, &k, &mut fresh, 0, TemplateSpec { err: true, ..spec }).is_err());
    assert_eq!(reference.contents(), fresh.contents(), "replay off: nothing of the failing body ran");
    println!("replay-off child: templates direct {}", stats.templates_direct);
}

/// A body error encodes nothing on the paths that never walk: no replay
/// span, and every ring entry in flight. The frame's output is the frame
/// without the template (the mix is not idempotent, so a leaked partial
/// copy would show). Replay off is the third such path, proven in
/// [`replay_template_replay_off_child`].
#[test]
fn replay_template_body_error_is_atomic_on_direct_paths() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let base = TemplateSpec::default();
    let failing = TemplateSpec { err: true, ..base };
    let skipped = TemplateSpec { skip: true, ..base };

    let mut reference = TemplateRig::new(&device, false);
    let mut unspanned = TemplateRig::new(&device, false);
    run_template_frame(&device, &k, &mut reference, 0, skipped).unwrap();
    assert_eq!(run_template_frame(&device, &k, &mut unspanned, 0, failing), Err("template-proof body error".into()));
    assert_eq!(reference.contents(), unspanned.contents(), "no span: nothing of the failing body ran");

    let mut reference = TemplateRig::new(&device, false);
    let mut replay = TemplateRig::new(&device, true);
    for frame in 0..3 {
        run_template_frame(&device, &k, &mut reference, frame, base).unwrap();
    }
    run_template_frame(&device, &k, &mut reference, 3, skipped).unwrap();
    let gate = device.create_event();
    for frame in 0..4 {
        let mut enc = device.create_encoder("template-proof gated");
        enc.wait_event(&gate, 1);
        enc.begin_replay(&device, replay.rig.cache.take().unwrap());
        let result = encode_template_frame(&mut enc, &k, &replay, frame, if frame == 3 { failing } else { base });
        replay.rig.cache = Some(enc.end_replay());
        enc.commit();
        assert_eq!(result.is_err(), frame == 3);
    }
    assert_eq!(replay.rig.stats().ring_busy, 1, "the failing frame found every entry in flight");
    unsafe { gate.raw().setSignaledValue(1) };
    device.create_encoder("template-proof drain").commit_and_wait_completed();
    assert_eq!(reference.contents(), replay.contents(), "ring busy: nothing of the failing body ran");
}

/// Chunked executes: rounds grouped 1, 2, 4, 8, 8, … into executes of one
/// replicated buffer, each a prefix by its GPU-written length. Stops before,
/// at and after every chunk boundary (rounds past the stop run inside their
/// chunk and write nothing) match direct output; executes are the chunk
/// count, not the rounds; a changed chunk size re-records one template.
#[test]
fn replay_template_chunks_match_direct_at_every_boundary() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let base = TemplateSpec { copies: 40, chunk: 8, ..TemplateSpec::default() };
    let mut specs: Vec<TemplateSpec> = [0, 1, 2, 3, 4, 6, 7, 8, 14, 15, 16, 22, 23, 24, 31, 39, 40]
        .iter()
        .map(|&live| TemplateSpec { live, ..base })
        .collect();
    specs.push(TemplateSpec { chunk: 4, live: 9, ..base });
    specs.push(TemplateSpec { chunk: 4, live: 10, ..base });
    specs.push(TemplateSpec { copies: 13, live: 13, ..base });
    let (_, d) = template_frames(&device, &k, &specs);
    for (i, (s, spec)) in d.iter().zip(&specs).enumerate() {
        let executes = crate::template_chunks(spec.copies, spec.chunk).count() as u64;
        assert_eq!(s.segments_replayed, executes, "frame {i}: one execute per chunk");
        assert_eq!((s.templates_direct, s.segments_direct), (0, 0), "frame {i}: nothing ran directly");
        if i > 0 && spec.chunk == specs[i - 1].chunk && spec.copies == specs[i - 1].copies {
            assert_eq!((s.recorded, s.templates_replayed), (0, 1), "frame {i}: warm");
        } else {
            assert_eq!(s.templates_recorded, 1, "frame {i}: a changed chunk layout re-records one template");
        }
    }
    assert_eq!(crate::template_chunks(40, 8).count(), 8);
}

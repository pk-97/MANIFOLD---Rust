//! GPU proofs for gated round templates (docs/GPU_FLIP_PRESSURE_CAP_DESIGN.md
//! section 3.2 (The template on every path)). The reference is independent
//! of the template: each round encoded directly, one indirect dispatch per
//! gated command, as the solver did before templates. The template runs on
//! every encoder path (no span, a replaying span, a busy ring, profiled at
//! both granularities) and must write exactly what the reference writes;
//! a refused prepare encodes nothing and leaves every slot as it was; a slot
//! a command buffer still runs is never rewritten.

use objc2_metal::MTLSharedEvent;

use super::*;
use crate::{GpuBinding, template_chunks};

const N: usize = 4096;
const GROUPS: [u32; 3] = [N as u32 / 64, 1, 1];
const ENTRIES: u32 = 64;

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

/// The mix, returning when the round's gate is off: a round an execute runs
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

/// Arms a frame on the GPU as the pressure solve's arm does: the gate on
/// when any round runs, the countdown restarted, and the chunked range
/// entries written from the layout (1, 2, 4, … doubling to `chunk`): an
/// execute starting before `live` runs its rounds, the rest none.
const ARM_WGSL: &str = r#"
struct Arm { live: u32, copies: u32, first: u32, stride: u32, commands: u32, chunk: u32, groups: u32, pad: u32 };
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
    for (var e = 0u; e < arrayLength(&ranges) / 2u; e = e + 1u) {
        ranges[2u * e] = 0u;
        ranges[2u * e + 1u] = 0u;
    }
    var start = 0u;
    var size = 1u;
    var j = 0u;
    loop {
        if start >= arm.copies { break; }
        let len = min(min(size, max(arm.chunk, 1u)), arm.copies - start);
        if start < arm.live {
            ranges[2u * (arm.first + j * arm.stride) + 1u] = len * arm.commands;
        }
        start = start + len;
        size = min(size * 2u, max(arm.chunk, 1u));
        j = j + 1u;
    }
}
"#;

/// A round's last dispatch, as the solver's check: counts the round and,
/// after the `live`-th, switches the gate off, so later rounds write nothing.
const COUNTDOWN_WGSL: &str = r#"
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

/// A kernel with a texture binding: no indirect command buffer support.
const TEXTURE_WGSL: &str = r#"
@group(0) @binding(0) var tex: texture_2d<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= 64u) { return; }
    dst[id.x] = dst[id.x] + textureLoad(tex, vec2<i32>(i32(id.x), 0), 0).x;
}
"#;

struct Kernels {
    mix: GpuComputePipeline,
    guarded: GpuComputePipeline,
    arm: GpuComputePipeline,
    countdown: GpuComputePipeline,
    texture: GpuComputePipeline,
}

impl Kernels {
    fn new(device: &GpuDevice) -> Self {
        Self {
            mix: device.create_compute_pipeline(MIX_WGSL, "main", "template-proof mix"),
            guarded: device.create_compute_pipeline(GUARDED_MIX_WGSL, "main", "template-proof guarded mix"),
            arm: device.create_compute_pipeline(ARM_WGSL, "main", "template-proof arm"),
            countdown: device.create_compute_pipeline(COUNTDOWN_WGSL, "main", "template-proof countdown"),
            texture: device.create_compute_pipeline(TEXTURE_WGSL, "main", "template-proof texture"),
        }
    }
}

fn bytes(v: &[u32]) -> &[u8] {
    // Safety: plain u32 data, read for the length of the borrow.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}

fn write(buffer: &GpuBuffer, values: &[u32]) {
    // Safety: a shared buffer sized for `values`; no GPU work is queued.
    unsafe { buffer.write(0, bytes(values)) };
}

fn read(buffer: &GpuBuffer) -> Vec<u32> {
    let ptr = buffer.mapped_ptr().expect("shared buffer");
    // Safety: a shared buffer of `size` bytes; the GPU work is done.
    let words = unsafe { std::slice::from_raw_parts(ptr.cast::<u32>(), (buffer.size / 4) as usize) };
    words.to_vec()
}

/// One run's state: three mixed buffers, the countdown's state, the gate
/// (indirect arguments: the mixes' at word 0, the countdown's at word 3),
/// the range entries, and the run's template store.
struct Run {
    buffers: [GpuBuffer; 3],
    state: GpuBuffer,
    args: GpuBuffer,
    ranges: GpuBuffer,
    store: GpuTemplateStore,
}

impl Run {
    fn new(device: &GpuDevice) -> Self {
        let buffers = std::array::from_fn(|b| {
            let buffer = device.create_buffer_shared((N * 4) as u64);
            write(&buffer, &(0..N as u32).map(|i| i.wrapping_mul(2654435761).wrapping_add(b as u32)).collect::<Vec<_>>());
            buffer
        });
        Self {
            buffers,
            state: device.create_buffer_shared(8),
            args: device.create_buffer_shared(24),
            ranges: device.create_buffer_shared(u64::from(ENTRIES) * crate::GATED_RANGE_BYTES),
            store: GpuTemplateStore::new(device),
        }
    }

    /// Everything a frame writes.
    fn contents(&self) -> Vec<Vec<u32>> {
        let mut all: Vec<Vec<u32>> = self.buffers.iter().map(read).collect();
        all.push(read(&self.state));
        all
    }
}

/// One frame: two plain mixes, the arm, the rounds (`mixes` guarded mixes
/// and the countdown, declared as `declared`, `copies` rounds by `chunk`,
/// `live` of them running), then one plain mix. `param_frame` sets the
/// rounds' inline bytes, so a changed one is a changed key.
#[derive(Clone, Copy, Debug)]
struct Spec {
    mixes: u32,
    declared: u32,
    copies: u32,
    live: u32,
    chunk: u32,
    first: u32,
    stride: u32,
    param_frame: u32,
    /// The body fails after its mixes.
    err: bool,
    /// The body adds a dispatch with a texture binding.
    unrecordable: bool,
}

impl Default for Spec {
    fn default() -> Self {
        Self { mixes: 4, declared: 5, copies: 13, live: 7, chunk: 4, first: 3, stride: 1, param_frame: 0, err: false, unrecordable: false }
    }
}

fn params(frame: u32, step: usize) -> [u32; 4] {
    [1_664_525 + step as u32, frame.wrapping_mul(7) + step as u32, (step as u32 * 37 + frame) % N as u32, 0]
}

fn plain(enc: &mut GpuEncoder, k: &Kernels, run: &Run, frame: u32, step: usize) {
    let p = params(frame, step);
    enc.dispatch_compute(
        &k.mix,
        &[
            GpuBinding::Buffer { binding: 0, buffer: &run.buffers[step % 3], offset: 0 },
            GpuBinding::Buffer { binding: 1, buffer: &run.buffers[(step + 1) % 3], offset: 0 },
            GpuBinding::Bytes { binding: 2, data: bytes(&p) },
        ],
        GROUPS,
        "template-proof plain mix",
    );
}

fn arm(enc: &mut GpuEncoder, k: &Kernels, run: &Run, spec: Spec) {
    let a = [spec.live, spec.copies, spec.first, spec.stride, spec.declared, spec.chunk, GROUPS[0], 0];
    enc.dispatch_compute(
        &k.arm,
        &[
            GpuBinding::Buffer { binding: 0, buffer: &run.state, offset: 0 },
            GpuBinding::Buffer { binding: 1, buffer: &run.args, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &run.ranges, offset: 0 },
            GpuBinding::Bytes { binding: 3, data: bytes(&a) },
        ],
        [1, 1, 1],
        "template-proof arm",
    );
}

/// The reference: every round encoded directly, each command an indirect
/// dispatch on its gate, as before templates.
fn encode_reference(enc: &mut GpuEncoder, k: &Kernels, run: &Run, frame: u32, spec: Spec) {
    plain(enc, k, run, frame, 0);
    plain(enc, k, run, frame, 1);
    arm(enc, k, run, spec);
    for _ in 0..spec.copies {
        for i in 0..spec.mixes as usize {
            let p = params(spec.param_frame, 100 + i);
            enc.dispatch_compute_indirect(
                &k.guarded,
                &[
                    GpuBinding::Buffer { binding: 0, buffer: &run.buffers[i % 3], offset: 0 },
                    GpuBinding::Buffer { binding: 1, buffer: &run.buffers[(i + 1) % 3], offset: 0 },
                    GpuBinding::Bytes { binding: 2, data: bytes(&p) },
                    GpuBinding::Buffer { binding: 3, buffer: &run.args, offset: 0 },
                ],
                &run.args,
                0,
                "template-proof reference mix",
            );
        }
        enc.dispatch_compute_indirect(
            &k.countdown,
            &[GpuBinding::Buffer { binding: 0, buffer: &run.state, offset: 0 }, GpuBinding::Buffer { binding: 1, buffer: &run.args, offset: 0 }],
            &run.args,
            12,
            "template-proof reference countdown",
        );
    }
    plain(enc, k, run, frame, 200);
}

/// The frame through the template. Prepared before anything encodes: a
/// refused prepare returns with the encoder untouched.
fn encode_template(enc: &mut GpuEncoder, k: &Kernels, run: &mut Run, frame: u32, spec: Spec, texture: Option<&GpuTexture>) -> Result<(), String> {
    let ticket = prepare_rounds(enc, k, run, spec, texture)?;
    plain(enc, k, run, frame, 0);
    plain(enc, k, run, frame, 1);
    arm(enc, k, run, spec);
    enc.execute_template(&mut run.store, ticket)?;
    plain(enc, k, run, frame, 200);
    Ok(())
}

/// The rounds' prepare alone: the walk and the slot, nothing encoded.
fn prepare_rounds(enc: &mut GpuEncoder, k: &Kernels, run: &mut Run, spec: Spec, texture: Option<&GpuTexture>) -> Result<TemplateTicket, String> {
    let Run { buffers, state, args, ranges, store } = run;
    let at = TemplateRanges { ranges, first: spec.first, stride: spec.stride, chunk: spec.chunk };
    enc.prepare_template(store, at, spec.declared, spec.copies, |rec| {
        for i in 0..spec.mixes as usize {
            let p = params(spec.param_frame, 100 + i);
            rec.dispatch_gated(
                &k.guarded,
                &[
                    GpuBinding::Buffer { binding: 0, buffer: &buffers[i % 3], offset: 0 },
                    GpuBinding::Buffer { binding: 1, buffer: &buffers[(i + 1) % 3], offset: 0 },
                    GpuBinding::Bytes { binding: 2, data: bytes(&p) },
                    GpuBinding::Buffer { binding: 3, buffer: args, offset: 0 },
                ],
                GROUPS,
                args,
                0,
                "template-proof mix",
            );
        }
        if spec.err {
            return Err("template-proof body error".into());
        }
        if spec.unrecordable {
            let texture = texture.expect("a texture for the unrecordable dispatch");
            rec.dispatch_gated(
                &k.texture,
                &[GpuBinding::Texture { binding: 0, texture }, GpuBinding::Buffer { binding: 1, buffer: state, offset: 0 }],
                [1, 1, 1],
                args,
                0,
                "template-proof texture",
            );
        }
        rec.dispatch_gated(
            &k.countdown,
            &[GpuBinding::Buffer { binding: 0, buffer: state, offset: 0 }, GpuBinding::Buffer { binding: 1, buffer: args, offset: 0 }],
            [1, 1, 1],
            args,
            12,
            "template-proof countdown",
        );
        Ok(())
    })
}

/// The encoder paths a frame can take.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Path {
    NoSpan,
    Span,
    Profiled(ProfileGranularity),
}

/// One template frame on `path`, committed and waited; the profile when
/// profiled.
fn template_frame(device: &GpuDevice, k: &Kernels, run: &mut Run, cache: &mut Option<GpuReplayCache>, frame: u32, spec: Spec, path: Path) -> Result<Option<GpuFrameProfile>, String> {
    let mut enc = device.create_encoder("template-proof");
    if let Path::Profiled(granularity) = path {
        // One sampler for the thread: Metal caps live counter sample buffers.
        thread_local! {
            static SAMPLER: std::cell::OnceCell<GpuTimestampSampler> = const { std::cell::OnceCell::new() };
        }
        let sampler = SAMPLER.with(|s| s.get_or_init(|| device.create_timestamp_sampler(256).expect("timestamp sampling")).clone());
        enc.enable_profiling_at(sampler, device, granularity);
        enc.set_profile_tag("rounds-tag");
    }
    if path == Path::Span {
        enc.begin_replay(device, cache.take().unwrap_or_default());
    }
    let result = encode_template(&mut enc, k, run, frame, spec, None);
    if enc.replay.is_some() {
        *cache = Some(enc.end_replay());
    }
    match path {
        Path::Profiled(_) => {
            let profile = enc.commit_and_wait_profiled(device);
            result.map(|()| Some(profile))
        }
        _ => {
            enc.commit_and_wait_completed();
            result.map(|()| None)
        }
    }
}

fn reference_frame(device: &GpuDevice, k: &Kernels, run: &Run, frame: u32, spec: Spec) {
    let mut enc = device.create_encoder("template-proof reference");
    encode_reference(&mut enc, k, run, frame, spec);
    enc.commit_and_wait_completed();
}

/// Every path writes what the unrolled reference writes, frame after frame,
/// through stops before, at and after every chunk boundary, a changed copy
/// count, chunk, stride and first entry, and changed inline bytes.
#[test]
fn template_matches_the_unrolled_reference_on_every_path() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let base = Spec { copies: 40, chunk: 8, first: 0, ..Spec::default() };
    let mut specs: Vec<Spec> = [0, 1, 2, 3, 4, 6, 7, 8, 14, 15, 16, 22, 23, 24, 30, 31, 32, 38, 39, 40]
        .iter()
        .map(|&live| Spec { live, ..base })
        .collect();
    specs.push(Spec { chunk: 4, live: 9, ..base });
    specs.push(Spec { copies: 13, live: 13, ..base });
    specs.push(Spec { copies: 13, chunk: 4, live: 7, ..base });
    specs.push(Spec { copies: 9, chunk: 3, stride: 2, first: 5, live: 5, ..base });
    specs.push(Spec { param_frame: 3, live: 11, ..base });
    specs.push(Spec { copies: 1, chunk: 32, live: 1, ..base });
    for path in [Path::NoSpan, Path::Span, Path::Profiled(ProfileGranularity::Tag), Path::Profiled(ProfileGranularity::Dispatch)] {
        let reference = Run::new(&device);
        let mut run = Run::new(&device);
        let mut cache = None;
        for (frame, spec) in specs.iter().enumerate() {
            let frame = frame as u32;
            reference_frame(&device, &k, &reference, frame, *spec);
            let before = run.store.stats();
            let profile = template_frame(&device, &k, &mut run, &mut cache, frame, *spec, path).expect("the template frame encodes");
            assert_eq!(reference.contents(), run.contents(), "{path:?} frame {frame} {spec:?}: the template diverged from the reference");
            let after = run.store.stats();
            let executes = template_chunks(spec.copies, spec.chunk).count() as u64;
            assert_eq!((after.walks - before.walks, after.executes - before.executes), (1, executes), "{path:?} frame {frame}: one walk, one execute per chunk");
            if let Some(profile) = profile {
                let rounds: Vec<&GpuProfiledSpan> = profile.spans.iter().filter(|s| s.label == "pressure rounds").collect();
                let want = match path {
                    Path::Profiled(ProfileGranularity::Dispatch) => executes as usize,
                    _ => 1,
                };
                assert_eq!(rounds.len(), want, "{path:?} frame {frame}: the rounds' spans");
                assert!(rounds.iter().all(|s| s.tag == "rounds-tag" && s.millis >= 0.0), "{path:?} frame {frame}: tagged, non-negative spans");
                // Each execute depends on the one before (the gate and the
                // countdown state): it starts after that one ends.
                assert!(rounds.windows(2).all(|w| w[1].start_ms + 1e-3 >= w[0].start_ms + w[0].millis), "{path:?} frame {frame}: an execute started before the one it depends on ended");
                assert_eq!((profile.overflow, profile.failed_command_buffers), (0, 0));
            }
        }
    }
}

/// A warm visit walks once and writes no command; a changed key builds one
/// slot of `commands × chunk` command writes into the idle slot it reuses;
/// a changed copy count or stop reuses the slot (the ranges are execute
/// arguments, not part of the key).
#[test]
fn template_warm_visits_write_nothing() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let mut run = Run::new(&device);
    let mut cache = None;
    let spec = Spec::default();
    let frame = |run: &mut Run, cache: &mut Option<GpuReplayCache>, spec: Spec, path: Path| {
        let before = run.store.stats();
        template_frame(&device, &k, run, cache, 0, spec, path).expect("encodes");
        let after = run.store.stats();
        (after.walks - before.walks, after.hits - before.hits, after.builds - before.builds, after.command_writes - before.command_writes)
    };
    assert_eq!(frame(&mut run, &mut cache, spec, Path::NoSpan), (1, 0, 1, u64::from(spec.declared * spec.chunk)), "cold: one build");
    for path in [Path::NoSpan, Path::Span, Path::Span, Path::Profiled(ProfileGranularity::Tag)] {
        assert_eq!(frame(&mut run, &mut cache, spec, path), (1, 1, 0, 0), "{path:?}: warm");
    }
    assert_eq!(frame(&mut run, &mut cache, Spec { live: 2, copies: 9, ..spec }, Path::NoSpan), (1, 1, 0, 0), "a changed stop and count reuse the slot");
    let changed = Spec { param_frame: 5, ..spec };
    assert_eq!(frame(&mut run, &mut cache, changed, Path::NoSpan), (1, 0, 1, u64::from(spec.declared * spec.chunk)), "changed bytes: one build");
    assert_eq!(run.store.slots(), 1, "the idle slot was rebuilt in place");
    assert_eq!(frame(&mut run, &mut cache, Spec { chunk: 8, ..changed }, Path::NoSpan).2, 1, "a changed chunk builds");
}

/// A refused prepare encodes nothing and leaves the built slot as it was:
/// the frame equals the frame without rounds, the stats show no build, and
/// the next valid visit hits the old slot and still matches the reference.
#[test]
fn template_refusals_are_atomic() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let texture = device.create_texture(&crate::GpuTextureDesc {
        width: 64,
        height: 1,
        depth: 1,
        format: crate::GpuTextureFormat::R32Uint,
        dimension: crate::GpuTextureDimension::D2,
        usage: crate::GpuTextureUsage::SHADER_READ,
        label: "template-proof texture",
        mip_levels: 1,
    });
    let spec = Spec::default();
    let refusals: [(&str, Spec, bool); 6] = [
        ("a body error after several dispatches", Spec { err: true, param_frame: 1, ..spec }, false),
        ("too few dispatches", Spec { declared: 6, param_frame: 1, ..spec }, false),
        ("too many dispatches", Spec { declared: 4, param_frame: 1, ..spec }, false),
        ("an unrecordable dispatch", Spec { unrecordable: true, declared: 6, param_frame: 1, ..spec }, false),
        ("storage allocation", Spec { param_frame: 1, ..spec }, true),
        ("ranges past the buffer", Spec { first: ENTRIES, ..spec }, false),
    ];
    for (why, refused, fail_alloc) in refusals {
        let reference = Run::new(&device);
        let mut run = Run::new(&device);
        let mut cache = None;
        reference_frame(&device, &k, &reference, 0, spec);
        template_frame(&device, &k, &mut run, &mut cache, 0, spec, Path::NoSpan).expect("the first visit builds");
        let built = run.store.stats();
        run.store.fail_next_alloc = fail_alloc;
        let mut enc = device.create_encoder("template-proof refused");
        let result = encode_template(&mut enc, &k, &mut run, 1, refused, Some(&texture));
        assert!(result.is_err(), "{why}: refused");
        enc.commit_and_wait_completed();
        let after = run.store.stats();
        assert_eq!((after.builds, after.command_writes, after.refused - built.refused), (built.builds, built.command_writes, 1), "{why}: nothing built");
        assert_eq!(run.store.slots(), 1, "{why}: the built slot stays");
        // The failed visit encoded nothing at all.
        assert_eq!(reference.contents(), run.contents(), "{why}: the refused frame encoded something");
        reference_frame(&device, &k, &reference, 2, spec);
        let before = run.store.stats();
        template_frame(&device, &k, &mut run, &mut cache, 2, spec, Path::NoSpan).expect("the next visit runs");
        assert_eq!(run.store.stats().hits - before.hits, 1, "{why}: the old slot is found intact");
        assert_eq!(reference.contents(), run.contents(), "{why}: the old slot still runs right");
    }
    // A chunk past the command bound, or wrapping u32, is refused.
    let mut run = Run::new(&device);
    for chunk in [super::template::MAX_TEMPLATE_COMMANDS, u32::MAX] {
        let mut enc = device.create_encoder("template-proof bound");
        let err = encode_template(&mut enc, &k, &mut run, 0, Spec { chunk, ..spec }, None).expect_err("refused");
        assert!(err.contains("do not fit"), "chunk {chunk}: {err}");
        enc.commit_and_wait_completed();
    }
    assert_eq!(run.store.slots(), 0);
}

/// Command buffers held on an event: equal keys share a slot in flight; a
/// changed key while the slot is in flight appends a slot and leaves the
/// first untouched; a frame-replay ring with every entry in flight still
/// runs the template; a `commit_and_continue` split between two executes of
/// one slot keeps its users. Each frame's outputs are copied out in its
/// own command buffer before later frames overwrite them, and every copy
/// matches the reference's.
#[test]
fn template_slots_in_flight_are_never_rewritten() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let spec = Spec::default();
    let frames: [Spec; 5] = [spec, spec, Spec { param_frame: 1, ..spec }, Spec { param_frame: 2, ..spec }, spec];
    let reference = Run::new(&device);
    let mut want = Vec::new();
    for (frame, s) in frames.iter().enumerate() {
        reference_frame(&device, &k, &reference, frame as u32, *s);
        want.push(reference.contents());
    }
    let mut run = Run::new(&device);
    let mut cache = Some(GpuReplayCache::default());
    let gate = device.create_event();
    let snapshots: Vec<Vec<GpuBuffer>> = frames.iter().map(|_| (0..4).map(|_| device.create_buffer_shared((N * 4) as u64)).collect()).collect();
    for (frame, s) in frames.iter().enumerate() {
        let mut enc = device.create_encoder("template-proof held");
        enc.wait_event(&gate, 1);
        enc.begin_replay(&device, cache.take().expect("cache"));
        encode_template(&mut enc, &k, &mut run, frame as u32, *s, None).expect("encodes");
        if frame == 4 {
            // A split after the execute: the slot's user is the committed half.
            cache = Some(enc.end_replay());
            enc.commit_and_continue(&device);
            enc.begin_replay(&device, cache.take().expect("cache"));
        }
        cache = Some(enc.end_replay());
        for (b, snapshot) in run.buffers.iter().zip(&snapshots[frame]) {
            enc.copy_buffer_to_buffer(b, snapshot, b.size);
        }
        enc.copy_buffer_to_buffer(&run.state, &snapshots[frame][3], run.state.size);
        enc.commit();
    }
    assert!(cache.as_ref().expect("cache").stats().ring_busy > 0, "the frame-replay ring ran out of idle entries");
    let users = run.store.users();
    assert_eq!(users.len(), 3, "three keys in flight, three slots: frames 0, 1 and 4 share one");
    assert!(users.iter().all(|&u| u > 0), "every slot is in flight");
    unsafe { gate.raw().setSignaledValue(1) };
    device.create_encoder("template-proof drain").commit_and_wait_completed();
    for (frame, snapshot) in snapshots.iter().enumerate() {
        let mut got: Vec<Vec<u32>> = snapshot.iter().map(read).collect();
        got[3].truncate(2);
        assert_eq!(got, want[frame], "frame {frame}: an in-flight slot was rewritten");
    }
    assert_eq!(run.store.users(), [0, 0, 0], "every user retired");
    // Dropping the store waits for committed users: run one, drop it in flight.
    let mut held = Run::new(&device);
    let mut enc = device.create_encoder("template-proof drop");
    let gate = device.create_event();
    enc.wait_event(&gate, 1);
    encode_template(&mut enc, &k, &mut held, 0, spec, None).expect("encodes");
    enc.commit();
    struct Event(objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn MTLSharedEvent>>);
    // Safety: Metal events are documented as usable from any thread.
    unsafe impl Send for Event {}
    let event = Event(objc2::Message::retain(gate.raw()));
    let signal = std::thread::spawn(move || {
        let event = event;
        std::thread::sleep(std::time::Duration::from_millis(50));
        unsafe { event.0.setSignaledValue(1) };
    });
    drop(held.store);
    signal.join().expect("signals");
}

/// A ticket names its store and its build. A different key prepared before
/// the ticket runs rebuilds the idle slot, and the old ticket is refused,
/// encoding nothing; an equal key keeps both tickets good; a ticket from
/// another store is refused; a ticket whose slot a trim freed is refused.
#[test]
fn template_tickets_name_their_store_and_build() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let spec = Spec::default();
    let mut run = Run::new(&device);
    let mut other = Run::new(&device);
    let mut enc = device.create_encoder("template-proof tickets");
    let a = prepare_rounds(&mut enc, &k, &mut run, spec, None).expect("prepares a");
    let again = prepare_rounds(&mut enc, &k, &mut run, spec, None).expect("an equal key hits");
    let b = prepare_rounds(&mut enc, &k, &mut run, Spec { param_frame: 9, ..spec }, None).expect("prepares b");
    assert_eq!(run.store.slots(), 1, "b rebuilt the idle slot a named");
    let before = run.contents();
    let stale = enc.execute_template(&mut run.store, a).expect_err("a stale ticket");
    assert!(stale.contains("rebuilt or freed"), "{stale}");
    let stale = enc.execute_template(&mut run.store, again).expect_err("the equal-key ticket went with the build");
    assert!(stale.contains("rebuilt or freed"), "{stale}");
    let foreign = enc.execute_template(&mut other.store, b).expect_err("a ticket from another store");
    assert!(foreign.contains("another store"), "{foreign}");
    enc.commit_and_wait_completed();
    assert_eq!(run.contents(), before, "the refused executes encoded nothing");

    // Equal keys before a run: both tickets name the same build and run.
    let mut enc = device.create_encoder("template-proof equal tickets");
    let a = prepare_rounds(&mut enc, &k, &mut run, spec, None).expect("prepares");
    let a2 = prepare_rounds(&mut enc, &k, &mut run, spec, None).expect("hits");
    enc.execute_template(&mut run.store, a).expect("runs");
    enc.execute_template(&mut run.store, a2).expect("runs");
    enc.commit_and_wait_completed();

    // A trim frees idle slots past four, and their tickets go with them.
    let mut held = Vec::new();
    let mut tickets = Vec::new();
    for key in 0..6 {
        let mut enc = device.create_encoder("template-proof held");
        tickets.push(prepare_rounds(&mut enc, &k, &mut run, Spec { param_frame: 100 + key, ..spec }, None).expect("prepares"));
        let t = prepare_rounds(&mut enc, &k, &mut run, Spec { param_frame: 100 + key, ..spec }, None).expect("hits");
        enc.execute_template(&mut run.store, t).expect("runs");
        held.push(enc);
    }
    assert!(run.store.slots() >= 6, "six keys in flight hold six slots");
    drop(held);
    let mut enc = device.create_encoder("template-proof trim");
    prepare_rounds(&mut enc, &k, &mut run, Spec { param_frame: 200, ..spec }, None).expect("prepares");
    assert!(run.store.slots() <= 1 + 4, "idle slots past four were freed: {}", run.store.slots());
    let freed = tickets.into_iter().filter_map(|t| enc.execute_template(&mut run.store, t).err()).count();
    assert!(freed >= 2, "tickets of freed builds are refused ({freed})");
    drop(enc);
}

/// Encoders dropped without committing never run: their slots retire, so a
/// stream of abandoned frames with changing keys holds a bounded number of
/// slots, and the store drops without leaking them.
#[test]
fn template_abandoned_encoders_release_their_slots() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let mut run = Run::new(&device);
    for key in 0..12 {
        let mut enc = device.create_encoder("template-proof abandoned");
        encode_template(&mut enc, &k, &mut run, key, Spec { param_frame: key, ..Spec::default() }, None).expect("encodes");
        drop(enc);
        assert!(run.store.users().iter().all(|&u| u == 0), "frame {key}: an abandoned buffer still counts as a user");
        assert_eq!(run.store.slots(), 1, "frame {key}: the abandoned slot is rebuilt in place, not stranded");
        assert_eq!(run.store.tokens(), 1, "frame {key}: the liveness token is reused, not allocated");
    }
    // Committed and waited frames reuse it too.
    for key in 0..4 {
        let mut enc = device.create_encoder("template-proof committed");
        encode_template(&mut enc, &k, &mut run, key, Spec::default(), None).expect("encodes");
        enc.commit_and_wait_completed();
        assert_eq!(run.store.tokens(), 1, "committed frame {key}: no new token");
    }
    // A live encoder's token is never handed to another encoder, and its
    // unsent user stays until that encoder drops.
    let mut held = device.create_encoder("template-proof held");
    encode_template(&mut held, &k, &mut run, 0, Spec::default(), None).expect("encodes");
    let mut other = device.create_encoder("template-proof other");
    encode_template(&mut other, &k, &mut run, 1, Spec::default(), None).expect("encodes");
    assert_eq!(run.store.tokens(), 2, "two live encoders hold two tokens");
    assert_eq!(run.store.users(), [2], "both unsent buffers are users while their encoders live");
    drop(other);
    assert_eq!(run.store.users(), [1], "only the dropped encoder's buffer retires");
    drop(held);
    assert_eq!(run.store.users(), [0]);
}

/// The store kept across a buffer replacement (a new key: a build, output
/// still the reference's), and a second execute of the same slot on the
/// buffer `commit_and_continue` opened, both executes users of the slot.
#[test]
fn template_survives_buffer_replacement_and_split_buffers() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let spec = Spec::default();
    let reference = Run::new(&device);
    let mut first = Run::new(&device);
    reference_frame(&device, &k, &reference, 0, spec);
    let mut cache = None;
    template_frame(&device, &k, &mut first, &mut cache, 0, spec, Path::NoSpan).expect("encodes");
    assert_eq!(first.contents(), reference.contents());
    let reference = Run::new(&device);
    let mut second = Run::new(&device);
    std::mem::swap(&mut first.store, &mut second.store);
    let builds = second.store.stats().builds;
    let mut enc = device.create_encoder("template-proof split");
    encode_template(&mut enc, &k, &mut second, 0, spec, None).expect("encodes");
    assert_eq!(second.store.stats().builds - builds, 1, "replaced buffers are a new key");
    enc.commit_and_continue(&device);
    encode_template(&mut enc, &k, &mut second, 1, spec, None).expect("encodes on the continued buffer");
    assert_eq!(second.store.stats().builds - builds, 1, "the continued buffer runs the same build");
    // The committed first buffer may already have completed and been pruned.
    assert!(matches!(second.store.users()[..], [1] | [2]), "the continued buffer uses the slot: {:?}", second.store.users());
    enc.commit_and_wait_completed();
    reference_frame(&device, &k, &reference, 0, spec);
    reference_frame(&device, &k, &reference, 1, spec);
    assert_eq!(second.contents(), reference.contents(), "both executes ran in order");
    assert_eq!(second.store.users(), [0]);
}

/// More profiled work than the sampler holds: the overflow runs unsampled
/// and the output is still the reference's.
#[test]
fn template_profiled_past_the_sampler_matches_the_reference() {
    let device = GpuDevice::new();
    let k = Kernels::new(&device);
    let spec = Spec { copies: 120, chunk: 1, live: 90, first: 0, ..Spec::default() };
    let mut big = Run::new(&device);
    big.ranges = device.create_buffer_shared(128 * crate::GATED_RANGE_BYTES);
    let mut reference = Run::new(&device);
    reference.ranges = device.create_buffer_shared(128 * crate::GATED_RANGE_BYTES);
    reference_frame(&device, &k, &reference, 0, spec);
    let mut enc = device.create_encoder("template-proof exhausted");
    let sampler = device.create_timestamp_sampler(64).expect("timestamp sampling");
    enc.enable_profiling_at(sampler, &device, ProfileGranularity::Dispatch);
    encode_template(&mut enc, &k, &mut big, 0, spec, None).expect("encodes");
    let profile = enc.commit_and_wait_profiled(&device);
    assert!(profile.overflow > 0, "the sampler ran out");
    assert_eq!(big.contents(), reference.contents(), "unsampled executes still ran");
}

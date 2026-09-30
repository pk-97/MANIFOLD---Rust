//! Workgroup zeroing (`crate::workgroup_zeroing`): the translated kernel
//! carries no one-thread zeroing, and on the GPU every workgroup variable
//! reads zero before anything writes it, even right after another kernel left
//! the same threadgroup memory full of ones.

use super::shader_compiler::compile_wgsl_to_msl;

/// Every shape the prologue handles, at 64 invocations: a run equal to the
/// workgroup, shorter (guarded), longer (looped), nested arrays (index split
/// by division), atomics alone and in arrays, and a struct mixing them.
/// `dirty` fills all of it with set bits; the `read_*` entries only read it,
/// each finding its invocation index a different way.
const SHAPES_WGSL: &str = r#"
struct Mixed {
    counts: array<atomic<u32>, 8>,
    total: atomic<u32>,
    grid: array<array<vec2<f32>, 5>, 3>,
    flag: u32,
}

var<workgroup> exact: array<f32, 64>;
var<workgroup> short_run: array<u32, 40>;
var<workgroup> long_run: array<vec4<f32>, 300>;
var<workgroup> nested: array<array<u32, 7>, 13>;
var<workgroup> tiles: array<atomic<i32>, 200>;
var<workgroup> mixed: Mixed;
var<workgroup> single: atomic<u32>;

@group(0) @binding(0) var<storage, read_write> out: array<u32>;

const ONES: u32 = 0xffffffffu;

@compute @workgroup_size(8, 8)
fn dirty(@builtin(local_invocation_index) i: u32, @builtin(workgroup_id) g: vec3<u32>) {
    exact[i] = bitcast<f32>(0x3f800000u);
    if (i < 40u) { short_run[i] = ONES; }
    for (var k = i; k < 300u; k += 64u) { long_run[k] = vec4<f32>(1.0); }
    for (var k = i; k < 91u; k += 64u) { nested[k / 7u][k % 7u] = ONES; }
    for (var k = i; k < 200u; k += 64u) { atomicStore(&tiles[k], -1); }
    if (i < 8u) { atomicStore(&mixed.counts[i], ONES); }
    if (i < 15u) { mixed.grid[i / 5u][i % 5u] = vec2<f32>(1.0); }
    if (i == 0u) {
        atomicStore(&mixed.total, ONES);
        mixed.flag = ONES;
        atomicStore(&single, ONES);
    }
    workgroupBarrier();
    if (i == 0u) { out[g.x] = bitcast<u32>(exact[63]) ^ 0x3f800000u; }
}

fn gather() -> u32 {
    var acc = 0u;
    for (var k = 0u; k < 64u; k++) { acc |= bitcast<u32>(exact[k]); }
    for (var k = 0u; k < 40u; k++) { acc |= short_run[k]; }
    for (var k = 0u; k < 300u; k++) {
        let v = bitcast<vec4<u32>>(long_run[k]);
        acc |= v.x | v.y | v.z | v.w;
    }
    for (var a = 0u; a < 13u; a++) {
        for (var b = 0u; b < 7u; b++) { acc |= nested[a][b]; }
    }
    for (var k = 0u; k < 200u; k++) { acc |= bitcast<u32>(atomicLoad(&tiles[k])); }
    for (var k = 0u; k < 8u; k++) { acc |= atomicLoad(&mixed.counts[k]); }
    acc |= atomicLoad(&mixed.total);
    for (var a = 0u; a < 3u; a++) {
        for (var b = 0u; b < 5u; b++) {
            let v = bitcast<vec2<u32>>(mixed.grid[a][b]);
            acc |= v.x | v.y;
        }
    }
    acc |= mixed.flag;
    acc |= atomicLoad(&single);
    return acc;
}

@compute @workgroup_size(8, 8)
fn read_arg(@builtin(local_invocation_index) i: u32, @builtin(workgroup_id) g: vec3<u32>) {
    workgroupBarrier();
    if (i == 0u) { out[g.x] = gather(); }
}

struct Ids {
    @builtin(workgroup_id) g: vec3<u32>,
    @builtin(local_invocation_index) i: u32,
}

@compute @workgroup_size(8, 8)
fn read_struct(ids: Ids) {
    workgroupBarrier();
    if (ids.i == 0u) { out[ids.g.x] = gather(); }
}

@compute @workgroup_size(8, 8)
fn read_none(@builtin(local_invocation_id) l: vec3<u32>, @builtin(workgroup_id) g: vec3<u32>) {
    workgroupBarrier();
    if (l.x + l.y + l.z == 0u) { out[g.x] = gather(); }
}
"#;

const ENTRIES: [&str; 4] = ["dirty", "read_arg", "read_struct", "read_none"];

#[test]
fn translated_kernels_have_no_one_thread_zeroing() {
    for entry in ENTRIES {
        let (_, msl, _, _) = compile_wgsl_to_msl(SHAPES_WGSL, entry, entry, false);
        assert!(!msl.contains("spvArrayCopyFromConstantToThreadGroup("), "{entry}: one-thread array copy left in:\n{msl}");
        assert!(!msl.contains("all(gl_LocalInvocationID == uint3(0u))"), "{entry}: invocation-0 guard left in:\n{msl}");
        assert!(msl.contains("threadgroup_barrier(mem_flags::mem_threadgroup)"), "{entry}: no barrier after the zeroing:\n{msl}");
    }
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn workgroup_memory_reads_zero_after_a_dirty_kernel() {
    use crate::{GpuBinding, GpuDevice};

    // Enough workgroups to cover every GPU core several times over.
    const GROUPS: u32 = 4096;
    let device = GpuDevice::new();
    // `out` holds one word per workgroup; every entry writes only out[g.x]
    // with g.x < GROUPS.
    let out = device.create_buffer_shared(u64::from(GROUPS) * 4);
    let pipelines: Vec<_> = ENTRIES
        .iter()
        .map(|entry| (*entry, device.create_compute_pipeline(SHAPES_WGSL, entry, entry)))
        .collect();
    for (entry, pipeline) in &pipelines[1..] {
        for round in 0..4 {
            let mut enc = device.create_encoder("workgroup-zeroing-proof");
            let bindings = [GpuBinding::Buffer { binding: 0, buffer: &out, offset: 0 }];
            enc.dispatch_compute(&pipelines[0].1, &bindings, [GROUPS, 1, 1], "dirty");
            enc.dispatch_compute(pipeline, &bindings, [GROUPS, 1, 1], entry);
            enc.commit_and_wait_completed();
            let words = unsafe {
                std::slice::from_raw_parts(out.mapped_ptr().expect("shared").cast::<u32>(), GROUPS as usize)
            };
            let dirty = words.iter().filter(|&&w| w != 0).count();
            assert_eq!(dirty, 0, "{entry} round {round}: {dirty} of {GROUPS} workgroups read non-zero workgroup memory");
        }
    }
}

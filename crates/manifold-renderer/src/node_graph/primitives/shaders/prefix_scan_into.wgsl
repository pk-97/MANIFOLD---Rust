// Out-of-place inclusive prefix sum of a large u32 array (`prefix_scan.rs`,
// `encode_into`): reduce each 1024-value block to its total, scan the totals
// with the in-place scan (`prefix_scan.wgsl`), then scan each block again
// from the input, add every earlier block's total and write `out`. The input
// is read twice and `out` written once, with no copy. Barriered by design:
// exclusion 1 of the codegen scope test.

struct IntoParams {
    n: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> params: IntoParams;
@group(0) @binding(1) var<storage, read> input: array<u32>;
@group(0) @binding(2) var<storage, read_write> out: array<u32>;
// Block totals, scanned in place between the two passes.
@group(0) @binding(3) var<storage, read_write> totals: array<u32>;

const THREADS: u32 = 256u;
const PER_THREAD: u32 = 4u;
const SPAN: u32 = 1024u;

var<workgroup> tile: array<u32, 256>;

fn load(i: u32) -> u32 {
    if i < params.n {
        return input[i];
    }
    return 0u;
}

// Inclusive scan of one value per thread across the workgroup.
fn scan_tile(lid: u32, value: u32) -> u32 {
    tile[lid] = value;
    workgroupBarrier();
    for (var stride = 1u; stride < THREADS; stride = stride * 2u) {
        var add = 0u;
        if lid >= stride {
            add = tile[lid - stride];
        }
        workgroupBarrier();
        tile[lid] = tile[lid] + add;
        workgroupBarrier();
    }
    return tile[lid];
}

@compute @workgroup_size(256)
fn reduce_blocks(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let base = wid.x * SPAN + lid.x * PER_THREAD;
    let sum = load(base) + load(base + 1u) + load(base + 2u) + load(base + 3u);
    let scanned = scan_tile(lid.x, sum);
    if lid.x == THREADS - 1u {
        totals[wid.x] = scanned;
    }
}

@compute @workgroup_size(256)
fn scan_blocks_into(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let base = wid.x * SPAN + lid.x * PER_THREAD;
    let a = load(base);
    let b = a + load(base + 1u);
    let c = b + load(base + 2u);
    let d = c + load(base + 3u);
    let inclusive = scan_tile(lid.x, d);
    var before = inclusive - d;
    if wid.x > 0u {
        before = before + totals[wid.x - 1u];
    }
    if base < params.n {
        out[base] = before + a;
    }
    if base + 1u < params.n {
        out[base + 1u] = before + b;
    }
    if base + 2u < params.n {
        out[base + 2u] = before + c;
    }
    if base + 3u < params.n {
        out[base + 3u] = before + d;
    }
}

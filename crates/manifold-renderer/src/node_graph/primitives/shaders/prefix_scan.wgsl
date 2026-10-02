// Multi-level inclusive prefix sum over u32 ranges (`prefix_scan.rs`). Level 0
// reads `src` and writes `dst` (the same buffer, or the consumer's input and
// output so no copy bridges them); every later level holds the 256-wide block
// totals of the one before in `parents`, and binds `parents` as its `src` and
// `dst` too. Barriered by design (workgroup memory, several dispatches):
// exclusion 1 of the codegen scope test.

struct ScanParams {
    n: u32,
    src_offset: u32,
    dst_offset: u32,
    parent: u32,
    has_parent: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> params: ScanParams;
@group(0) @binding(1) var<storage, read> src: array<u32>;
@group(0) @binding(2) var<storage, read_write> dst: array<u32>;
@group(0) @binding(3) var<storage, read_write> parents: array<u32>;

var<workgroup> tile: array<u32, 256>;

// Inclusive scan within each 256-wide block; the block's total goes to the
// parent level.
@compute @workgroup_size(256)
fn scan_blocks(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let i = gid.x;
    var value = 0u;
    if i < params.n {
        value = src[params.src_offset + i];
    }
    tile[lid.x] = value;
    workgroupBarrier();
    for (var stride = 1u; stride < 256u; stride = stride * 2u) {
        var add = 0u;
        if lid.x >= stride {
            add = tile[lid.x - stride];
        }
        workgroupBarrier();
        tile[lid.x] = tile[lid.x] + add;
        workgroupBarrier();
    }
    if i < params.n {
        dst[params.dst_offset + i] = tile[lid.x];
    }
    if params.has_parent != 0u && lid.x == 255u {
        parents[params.parent + wid.x] = tile[255u];
    }
}

// Add the (already scanned) total of every earlier block.
@compute @workgroup_size(256)
fn add_block_totals(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    let block = i / 256u;
    if i >= params.n || block == 0u {
        return;
    }
    dst[params.dst_offset + i] = dst[params.dst_offset + i] + parents[params.parent + block - 1u];
}

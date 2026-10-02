// Multi-level inclusive prefix sum over u32 ranges of one storage buffer
// (`prefix_scan.rs`). Level k holds n_k values at `offset`; level k + 1 holds
// its 256-wide block totals at `parent`. The last level is one workgroup:
// one block, or a tail of up to 256 × 64 values each thread sums a run of.
// Barriered by design (workgroup memory, several dispatches): exclusion 1 of
// the codegen scope test.

struct ScanParams {
    n: u32,
    offset: u32,
    parent: u32,
    has_parent: u32,
}

@group(0) @binding(0) var<uniform> params: ScanParams;
@group(0) @binding(1) var<storage, read_write> values: array<u32>;

var<workgroup> tile: array<u32, 256>;

// Inclusive scan of `tile` in place; every thread of the group calls it.
fn scan_tile(lid: u32) {
    for (var stride = 1u; stride < 256u; stride = stride * 2u) {
        var add = 0u;
        if lid >= stride {
            add = tile[lid - stride];
        }
        workgroupBarrier();
        tile[lid] = tile[lid] + add;
        workgroupBarrier();
    }
}

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
        value = values[params.offset + i];
    }
    tile[lid.x] = value;
    workgroupBarrier();
    scan_tile(lid.x);
    if i < params.n {
        values[params.offset + i] = tile[lid.x];
    }
    if params.has_parent != 0u && lid.x == 255u {
        values[params.parent + wid.x] = tile[255u];
    }
}

// One workgroup scans up to 256 × 64 values: thread t owns the run
// [t·k, (t+1)·k), k = ceil(n / 256); the runs' totals scan through the tile
// and each run is rescanned from its exclusive start. Never has a parent.
@compute @workgroup_size(256)
fn scan_tail(@builtin(local_invocation_id) lid: vec3<u32>) {
    let k = (params.n + 255u) / 256u;
    let first = lid.x * k;
    var total = 0u;
    for (var j = 0u; j < k; j = j + 1u) {
        let i = first + j;
        if i < params.n {
            total = total + values[params.offset + i];
        }
    }
    tile[lid.x] = total;
    workgroupBarrier();
    scan_tile(lid.x);
    var running = tile[lid.x] - total;
    for (var j = 0u; j < k; j = j + 1u) {
        let i = first + j;
        if i < params.n {
            running = running + values[params.offset + i];
            values[params.offset + i] = running;
        }
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
    values[params.offset + i] = values[params.offset + i] + values[params.parent + block - 1u];
}

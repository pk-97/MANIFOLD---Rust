// node.running_total — the scanned grand total, to the CPU-visible total cell
// and to the `extent` output: [total, groups, 1, 1], where the groups of 256
// threads cover max(total, last frame's total) × per_item elements. An emitter
// dispatched over that grid writes its live elements and clears the ones it
// wrote last frame. The scan itself is `prefix_scan.wgsl`.

struct TotalParams {
    n: u32,
    per_item: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> params: TotalParams;
@group(0) @binding(1) var<storage, read> values: array<u32>;
@group(0) @binding(2) var<storage, read_write> total: array<u32>;
// This node's own state: last frame's total.
@group(0) @binding(3) var<storage, read_write> last: array<u32>;
@group(0) @binding(4) var<storage, read_write> extent: array<u32>;

@compute @workgroup_size(1)
fn read_total() {
    var value = 0u;
    if params.n > 0u {
        value = values[params.n - 1u];
    }
    total[0] = value;
    let per_item = max(params.per_item, 1u);
    let items = min(max(value, last[0]), 0xffffffffu / per_item);
    let elements = items * per_item;
    extent[0] = value;
    extent[1] = elements / 256u + select(0u, 1u, elements % 256u != 0u);
    extent[2] = 1u;
    extent[3] = 1u;
    last[0] = value;
}

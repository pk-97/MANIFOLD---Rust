// node.running_total — copy the scanned last value into the CPU-visible
// total cell. The scan itself is `prefix_scan.wgsl`.

struct TotalParams {
    n: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> params: TotalParams;
@group(0) @binding(1) var<storage, read> values: array<u32>;
@group(0) @binding(2) var<storage, read_write> total: array<u32>;

@compute @workgroup_size(1)
fn read_total() {
    var value = 0u;
    if params.n > 0u {
        value = values[params.n - 1u];
    }
    total[0] = value;
}

// node.occupied_bounds — the bounding box and count of a lattice's occupied
// nodes (value > threshold), node (i, j, k) at i + nx·(j + ny·k). Two passes:
// `partial_main` reduces a grid-strided share of the lattice per workgroup
// into `partials`; `finalize_main` (one workgroup) reduces those into the
// eight words of `reading`: lowest x, y, z; one past the highest x, y, z;
// the count; 0. An empty lattice reads lowest 0xffffffff, end 0, count 0.
// Every read and write is bounded by its array's own length.

struct Params {
    nodes_x: u32,
    nodes_y: u32,
    nodes_z: u32,
    groups: u32,
    threshold: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> values: array<f32>;
@group(0) @binding(2) var<storage, read_write> partials: array<u32>;
@group(0) @binding(3) var<storage, read_write> reading: array<u32>;

const WORDS: u32 = 8u;
const EMPTY: u32 = 0xffffffffu;

var<workgroup> low: array<vec3<u32>, 256>;
var<workgroup> high: array<vec3<u32>, 256>;
var<workgroup> cells: array<u32, 256>;

// Tree reduction of the workgroup's 256 entries into entry 0.
fn reduce_group(t: u32) {
    for (var stride = 128u; stride > 0u; stride = stride >> 1u) {
        workgroupBarrier();
        if t < stride {
            low[t] = min(low[t], low[t + stride]);
            high[t] = max(high[t], high[t + stride]);
            cells[t] = cells[t] + cells[t + stride];
        }
    }
    workgroupBarrier();
}

fn write_words(dst_base: u32, t: u32) {
    if t != 0u {
        return;
    }
    var words = array<u32, 8>(low[0].x, low[0].y, low[0].z, high[0].x, high[0].y, high[0].z, cells[0], 0u);
    for (var w = 0u; w < WORDS; w = w + 1u) {
        let at = dst_base + w;
        if at < arrayLength(&partials) {
            partials[at] = words[w];
        }
    }
}

@compute @workgroup_size(256)
fn partial_main(@builtin(local_invocation_index) t: u32, @builtin(workgroup_id) wg: vec3<u32>) {
    let n = vec3<u32>(params.nodes_x, params.nodes_y, params.nodes_z);
    var total = 0u;
    if all(n >= vec3<u32>(1u)) && all(n <= vec3<u32>(1024u)) {
        total = min(n.x * n.y * n.z, arrayLength(&values));
    }
    var lo = vec3<u32>(EMPTY);
    var hi = vec3<u32>(0u);
    var count = 0u;
    let stride = max(params.groups, 1u) * 256u;
    for (var i = wg.x * 256u + t; i < total; i = i + stride) {
        if values[i] > params.threshold {
            let c = vec3<u32>(i % n.x, (i / n.x) % n.y, i / (n.x * n.y));
            lo = min(lo, c);
            hi = max(hi, c + vec3<u32>(1u));
            count = count + 1u;
        }
    }
    low[t] = lo;
    high[t] = hi;
    cells[t] = count;
    reduce_group(t);
    write_words(wg.x * WORDS, t);
}

@compute @workgroup_size(256)
fn finalize_main(@builtin(local_invocation_index) t: u32) {
    var lo = vec3<u32>(EMPTY);
    var hi = vec3<u32>(0u);
    var count = 0u;
    let groups = min(params.groups, arrayLength(&partials) / WORDS);
    for (var g = t; g < groups; g = g + 256u) {
        let base = g * WORDS;
        lo = min(lo, vec3<u32>(partials[base], partials[base + 1u], partials[base + 2u]));
        hi = max(hi, vec3<u32>(partials[base + 3u], partials[base + 4u], partials[base + 5u]));
        count = count + partials[base + 6u];
    }
    low[t] = lo;
    high[t] = hi;
    cells[t] = count;
    reduce_group(t);
    if t == 0u && arrayLength(&reading) >= WORDS {
        reading[0] = low[0].x;
        reading[1] = low[0].y;
        reading[2] = low[0].z;
        reading[3] = high[0].x;
        reading[4] = high[0].y;
        reading[5] = high[0].z;
        reading[6] = cells[0];
        reading[7] = 0u;
    }
}

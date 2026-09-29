// node.sort_particles_into_cells — counting sort of liquid particles into
// spatial bins (GPU_FLUID_SURFACE_DESIGN.md D17). Passes, barrier between each:
// clear_counts → count_particles → prefix_scan (level 0 of `cell_counts`) →
// write_ranges → clear_tail → scatter → stabilise. The count pass's atomic ranks
// vary run to run; `stabilise` puts each bin's slots in input-index order, so
// every output is the same on every run. `cell_counts` is the scan storage; after
// the scan it holds each bin's inclusive end. With `write_order`, `order` gets each
// sorted slot's input index (NO_RANK past the live total); `sorted` is written only
// with `write_sorted`.

struct FluidParticle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

struct CellRange {
    start: u32,
    count: u32,
}

struct SortParams {
    bin_min: vec3<f32>,
    inv_cell: f32,
    bins: vec3<u32>,
    count: u32,
    bin_total: u32,
    sorted_capacity: u32,
    write_order: u32,
    write_sorted: u32,
}

@group(0) @binding(0) var<uniform> params: SortParams;
@group(0) @binding(1) var<storage, read> particles: array<FluidParticle>;
@group(0) @binding(2) var<storage, read_write> sorted: array<FluidParticle>;
@group(0) @binding(3) var<storage, read_write> ranges: array<CellRange>;
@group(0) @binding(4) var<storage, read_write> cell_counts: array<atomic<u32>>;
@group(0) @binding(5) var<storage, read_write> rank: array<u32>;
@group(0) @binding(6) var<storage, read_write> order: array<u32>;
// The input index of each sorted slot, before and after stabilising.
@group(0) @binding(7) var<storage, read_write> slot_input: array<u32>;

const NO_RANK: u32 = 0xffffffffu;

fn bin_of(p: vec3<f32>) -> u32 {
    let last = vec3<i32>(params.bins) - vec3<i32>(1);
    let b = clamp(vec3<i32>(floor((p - params.bin_min) * params.inv_cell)), vec3<i32>(0), last);
    return u32(b.x) + params.bins.x * (u32(b.y) + params.bins.y * u32(b.z));
}

fn bin_start(b: u32) -> u32 {
    if b == 0u {
        return 0u;
    }
    return atomicLoad(&cell_counts[b - 1u]);
}

@compute @workgroup_size(256)
fn clear_counts(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x < params.bin_total {
        atomicStore(&cell_counts[gid.x], 0u);
    }
}

@compute @workgroup_size(256)
fn count_particles(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count {
        return;
    }
    let p = particles[i].position_radius;
    if !(p.w > 0.0) {
        rank[i] = NO_RANK;
        return;
    }
    rank[i] = atomicAdd(&cell_counts[bin_of(p.xyz)], 1u);
}

@compute @workgroup_size(256)
fn write_ranges(@builtin(global_invocation_id) gid: vec3<u32>) {
    let b = gid.x;
    if b >= params.bin_total {
        return;
    }
    let start = bin_start(b);
    ranges[b] = CellRange(start, atomicLoad(&cell_counts[b]) - start);
}

// Slots past the live total become inactive (radius 0) before the scatter
// fills the live ones.
@compute @workgroup_size(256)
fn clear_tail(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.sorted_capacity {
        return;
    }
    if i >= atomicLoad(&cell_counts[params.bin_total - 1u]) {
        if params.write_sorted != 0u {
            sorted[i] = FluidParticle(vec4<f32>(0.0), vec3<f32>(0.0), 0u);
        }
        if params.write_order != 0u {
            order[i] = NO_RANK;
        }
    }
}

@compute @workgroup_size(256)
fn scatter(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count || rank[i] == NO_RANK {
        return;
    }
    let slot = bin_start(bin_of(particles[i].position_radius.xyz)) + rank[i];
    slot_input[slot] = i;
}

fn swap_slots(a: u32, b: u32) {
    let t = slot_input[a];
    slot_input[a] = slot_input[b];
    slot_input[b] = t;
}

fn sift_down(base: u32, start: u32, n: u32) {
    var root = start;
    loop {
        let child = 2u * root + 1u;
        if child >= n {
            break;
        }
        var larger = child;
        if child + 1u < n && slot_input[base + child + 1u] > slot_input[base + child] {
            larger = child + 1u;
        }
        if slot_input[base + root] >= slot_input[base + larger] {
            break;
        }
        swap_slots(base + root, base + larger);
        root = larger;
    }
}

// One thread per bin: its slots' input indices into ascending order
// (insertion sort for small bins, heapsort above, so a crowded bin costs
// n log n), then `sorted` and `order` written from them.
@compute @workgroup_size(256)
fn stabilise(@builtin(global_invocation_id) gid: vec3<u32>) {
    let b = gid.x;
    if b >= params.bin_total {
        return;
    }
    let start = bin_start(b);
    let n = atomicLoad(&cell_counts[b]) - start;
    if n <= 32u {
        for (var k = 1u; k < n; k = k + 1u) {
            let key = slot_input[start + k];
            var j = k;
            while j > 0u && slot_input[start + j - 1u] > key {
                slot_input[start + j] = slot_input[start + j - 1u];
                j = j - 1u;
            }
            slot_input[start + j] = key;
        }
    } else {
        for (var r = n / 2u; r > 0u; r = r - 1u) {
            sift_down(start, r - 1u, n);
        }
        for (var end = n - 1u; end > 0u; end = end - 1u) {
            swap_slots(start, start + end);
            sift_down(start, 0u, end);
        }
    }
    for (var k = 0u; k < n; k = k + 1u) {
        let i = slot_input[start + k];
        if params.write_sorted != 0u {
            sorted[start + k] = particles[i];
        }
        if params.write_order != 0u {
            order[start + k] = i;
        }
    }
}

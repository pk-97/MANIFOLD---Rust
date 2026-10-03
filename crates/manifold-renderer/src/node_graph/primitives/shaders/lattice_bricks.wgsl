// Occupied-brick producer for the liquid surface lattice.
//
// The mark pass is deliberately conservative.  A brick owns eight lattice
// nodes per axis, while the surface consumers can reach three smoothing taps,
// one gradient tap and one cell corner: five nodes of
// halo on every side.  A live blob marks the brick when its support plus the
// positive exterior band intersects that expanded box.  The boundary bricks
// are always live so the canonical exterior value and the closed surface are
// preserved even for an empty frame.

struct Params {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    cell_size: f32,
    nodes_x: u32,
    nodes_y: u32,
    nodes_z: u32,
    resolution_scale: u32,
    bins_x: u32,
    bins_y: u32,
    bins_z: u32,
    bricks_x: u32,
    bricks_y: u32,
    bricks_z: u32,
    brick_count: u32,
    band_extra: f32,
    bounds_len: u32,
}

struct Blob {
    center_radius: vec4<f32>,
    shape_diag: vec4<f32>,
    shape_off: vec4<f32>,
}

struct CellRange {
    start: u32,
    count: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> blobs: array<Blob>;
@group(0) @binding(2) var<storage, read> cell_ranges: array<CellRange>;
@group(0) @binding(3) var<storage, read_write> prefix: array<u32>;
@group(0) @binding(4) var<storage, read_write> bricks: array<u32>;

@group(0) @binding(5) var<storage, read> bounds: array<f32>;

const HALO_NODES: i32 = 5;

fn bin_index(b: vec3<u32>) -> u32 {
    return b.x + params.bins_x * (b.y + params.bins_y * b.z);
}

fn clamp_bin(v: i32, n: u32) -> u32 {
    return u32(clamp(v, 0, i32(n) - 1));
}

fn mark_brick(id: u32) -> u32 {
    let brick = vec3<u32>(
        id % params.bricks_x,
        (id / params.bricks_x) % params.bricks_y,
        id / (params.bricks_x * params.bricks_y),
    );
    let nodes = vec3<u32>(params.nodes_x, params.nodes_y, params.nodes_z);
    let at0 = brick * 8u;
    let at1 = min((brick + vec3<u32>(1u)) * 8u - vec3<u32>(1u), nodes - vec3<u32>(1u));
    let lo_node = max(vec3<i32>(at0) - vec3<i32>(HALO_NODES), vec3<i32>(0));
    let hi_node = min(vec3<i32>(at1) + vec3<i32>(HALO_NODES), vec3<i32>(nodes) - vec3<i32>(1));
    let domain_min = vec3<f32>(params.center_x, params.center_y, params.center_z)
        - 0.5 * vec3<f32>(params.size_x, params.size_y, params.size_z);
    // Match the volume multiply-then-divide order: monotone even in f32.
    let size = vec3<f32>(params.size_x, params.size_y, params.size_z);
    let lo = domain_min + vec3<f32>(lo_node) * size / vec3<f32>(nodes - vec3<u32>(1u));
    let hi = domain_min + vec3<f32>(hi_node) * size / vec3<f32>(nodes - vec3<u32>(1u));
    let extra = params.band_extra + select(0.0, length(size / vec3<f32>(nodes - vec3<u32>(1u))), params.band_extra > 0.0);
    var bound = vec2<f32>(0.0);
    if params.bounds_len == 2u {
        bound = vec2<f32>(bounds[0], bounds[1]);
    } else {
        for (var k = 0u; k < arrayLength(&blobs); k += 1u) {
            let blob = blobs[k];
            let r = blob.center_radius.w;
            if r > 0.0 { bound = max(bound, vec2<f32>(r, 1.5 * r + blob.shape_off.w)); }
        }
    }
    let h = size / vec3<f32>(nodes - vec3<u32>(1u));

    // A domain border brick is retained even when no blob is present.  This
    // is what makes an empty frame write the same exterior field as the dense
    // path instead of leaving retired border values in place.
    if any(brick == vec3<u32>(0u)) || any(brick + vec3<u32>(1u) == vec3<u32>(params.bricks_x, params.bricks_y, params.bricks_z)) {
        return 1u;
    }

    let bin_lo_f = (lo - domain_min) / vec3<f32>(params.cell_size);
    let bin_hi_f = (hi - domain_min) / vec3<f32>(params.cell_size);
    let reach_bins = i32(ceil((bound.y + extra + length(h)) / params.cell_size));
    let bin_lo = vec3<i32>(floor(bin_lo_f)) - vec3<i32>(reach_bins);
    let bin_hi = vec3<i32>(floor(bin_hi_f)) + vec3<i32>(reach_bins);
    let first = vec3<u32>(
        clamp_bin(bin_lo.x, params.bins_x),
        clamp_bin(bin_lo.y, params.bins_y),
        clamp_bin(bin_lo.z, params.bins_z),
    );
    let last = vec3<u32>(
        clamp_bin(bin_hi.x, params.bins_x),
        clamp_bin(bin_hi.y, params.bins_y),
        clamp_bin(bin_hi.z, params.bins_z),
    );
    for (var z = first.z; z <= last.z; z = z + 1u) {
        for (var y = first.y; y <= last.y; y = y + 1u) {
            for (var x = first.x; x <= last.x; x = x + 1u) {
                let range = cell_ranges[bin_index(vec3<u32>(x, y, z))];
                for (var k = range.start; k < range.start + range.count; k = k + 1u) {
                    let blob = blobs[k];
                    let centre = blob.center_radius.xyz;
                    let reach = blob.center_radius.w;
                    if !(reach > 0.0) {
                        continue;
                    }
                    let d = max(max(lo - centre, centre - hi), vec3<f32>(0.0));
                    let support = vec3<f32>(1.5 * reach + extra) + h;
                    if all(d <= support) {
                        return 1u;
                    }
                }
            }
        }
    }
    return 0u;
}

@compute @workgroup_size(256)
fn mark_bricks(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let id = global_id.x;
    if id >= params.brick_count {
        return;
    }
    let marked = mark_brick(id);
    prefix[id] = marked;
    bricks[8u + id] = marked;
    // Clear the old compact list.  The active count is written by compact;
    // every slot beyond it must be retired before downstream consumers run.
    bricks[8u + params.brick_count + id] = 0u;
}

@compute @workgroup_size(256)
fn compact_bricks(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let id = global_id.x;
    if id >= params.brick_count {
        return;
    }
    let marked = bricks[8u + id];
    if marked != 0u {
        let dst = 8u + params.brick_count + prefix[id] - 1u;
        bricks[dst] = id;
    }
    if id == params.brick_count - 1u {
        let active_count = prefix[id];
        bricks[0u] = active_count;
        bricks[1u] = active_count * 2u;
        bricks[2u] = 1u;
        bricks[3u] = 1u;
        bricks[4u] = params.bricks_x;
        bricks[5u] = params.bricks_y;
        bricks[6u] = params.bricks_z;
        bricks[7u] = 0u;
    }
}

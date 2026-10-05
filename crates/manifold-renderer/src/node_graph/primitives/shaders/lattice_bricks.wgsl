// Occupied-brick producer for the liquid surface lattice.
//
// The mark is deliberately conservative.  A brick owns eight lattice
// nodes per axis, while the surface consumers can reach three smoothing taps,
// one gradient tap and one cell corner: five nodes of
// halo on every side.  A live blob marks the brick when its support plus the
// positive exterior band intersects that expanded box.  The boundary bricks
// are always live so the canonical exterior value and the closed surface are
// preserved even for an empty frame.
//
// Each blob scatters its mark to the bricks it can reach, so the cost follows
// the particles, not the empty space a per-brick search would scan.  The hit
// test is the per-brick one, evaluated on a padded candidate range, so the
// mask is the gather's word for word.

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
    blob_count: u32,
}

struct Blob {
    center_radius: vec4<f32>,
    shape_diag: vec4<f32>,
    shape_off: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> blobs: array<Blob>;
@group(0) @binding(3) var<storage, read_write> prefix: array<u32>;
@group(0) @binding(4) var<storage, read_write> bricks: array<u32>;
// One word per brick: nonzero once a blob reaches it.  The mark pass reads
// and zeroes it, so it is clear again for the next frame's scatter.
@group(0) @binding(6) var<storage, read_write> hits: array<atomic<u32>>;

const HALO_NODES: i32 = 5;
// Sixteen f32 ulps, relative: a generous bound on the rounding of the hit
// test's chain of adds, multiplies and fast-math divides.
const ROUNDING: f32 = 1.9073486e-6;
// Coordinates, sizes and reaches below this keep every sum, product and the
// squared spacing in band() finite.
const LIMIT: f32 = 1.0e18;
// Node spacing above this keeps the spacing and its square normal f32s.
const MIN_SPACING: f32 = 1.0e-15;

fn brick_coords(id: u32) -> vec3<u32> {
    return vec3<u32>(
        id % params.bricks_x,
        (id / params.bricks_x) % params.bricks_y,
        id / (params.bricks_x * params.bricks_y),
    );
}

fn is_border(brick: vec3<u32>) -> bool {
    return any(brick == vec3<u32>(0u)) || any(brick + vec3<u32>(1u) == vec3<u32>(params.bricks_x, params.bricks_y, params.bricks_z));
}

fn domain_min() -> vec3<f32> {
    return vec3<f32>(params.center_x, params.center_y, params.center_z)
        - 0.5 * vec3<f32>(params.size_x, params.size_y, params.size_z);
}

fn lattice_size() -> vec3<f32> {
    return vec3<f32>(params.size_x, params.size_y, params.size_z);
}

fn node_counts() -> vec3<u32> {
    return vec3<u32>(params.nodes_x, params.nodes_y, params.nodes_z);
}

fn band() -> f32 {
    let nodes = node_counts();
    return params.band_extra + select(0.0, length(lattice_size() / vec3<f32>(nodes - vec3<u32>(1u))), params.band_extra > 0.0);
}

// The gather's hit test for one interior brick, unchanged: the brick box
// grown by the halo, against the blob's support box.
fn reaches(brick: vec3<u32>, centre: vec3<f32>, reach: f32, extra: f32) -> bool {
    let nodes = node_counts();
    let at0 = brick * 8u;
    let at1 = min((brick + vec3<u32>(1u)) * 8u - vec3<u32>(1u), nodes - vec3<u32>(1u));
    let lo_node = max(vec3<i32>(at0) - vec3<i32>(HALO_NODES), vec3<i32>(0));
    let hi_node = min(vec3<i32>(at1) + vec3<i32>(HALO_NODES), vec3<i32>(nodes) - vec3<i32>(1));
    // Match the volume multiply-then-divide order: monotone even in f32.
    let size = lattice_size();
    let lo = domain_min() + vec3<f32>(lo_node) * size / vec3<f32>(nodes - vec3<u32>(1u));
    let hi = domain_min() + vec3<f32>(hi_node) * size / vec3<f32>(nodes - vec3<u32>(1u));
    let h = size / vec3<f32>(nodes - vec3<u32>(1u));
    let d = max(max(lo - centre, centre - hi), vec3<f32>(0.0));
    let support = vec3<f32>(1.5 * reach + extra) + h;
    return all(d <= support);
}

@compute @workgroup_size(256)
fn scatter_bricks(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let k = global_id.x;
    if k >= params.blob_count {
        return;
    }
    let blob = blobs[k];
    let centre = blob.center_radius.xyz;
    let reach = blob.center_radius.w;
    // The one input the scatter treats differently: a non-finite centre has
    // no position, so it marks nothing. The per-brick search could mark the
    // bricks around the bin the sort clamped it into (a NaN centre passes its
    // hit test there).
    let exponent = bitcast<vec3<u32>>(centre) & vec3<u32>(0x7f800000u);
    if !(reach > 0.0) || any(exponent == vec3<u32>(0x7f800000u)) {
        return;
    }
    let extra = band();
    let nodes = node_counts();
    let size = lattice_size();
    let gaps = vec3<f32>(nodes - vec3<u32>(1u));
    let last = vec3<f32>(vec3<u32>(params.bricks_x, params.bricks_y, params.bricks_z) - vec3<u32>(1u));
    var first_brick = vec3<u32>(0u);
    var last_brick = vec3<u32>(last);
    // Decided from the raw inputs, before any arithmetic that could overflow
    // or go subnormal: fast math cannot be trusted to reject those after the
    // fact. Inside these bounds every quantity below, and in the hit test,
    // stays finite and normal, so the rounding pad holds. Outside them every
    // brick is a candidate and the hit test alone decides.
    let raw = vec3<f32>(params.center_x, params.center_y, params.center_z);
    let bounded = reach < LIMIT && abs(params.band_extra) < LIMIT
        && all(abs(centre) < vec3<f32>(LIMIT)) && all(abs(raw) < vec3<f32>(LIMIT))
        && all(size < vec3<f32>(LIMIT)) && all(size > vec3<f32>(MIN_SPACING) * gaps);
    if bounded {
        let h = size / gaps;
        let support = vec3<f32>(1.5 * reach + extra) + h;
        // Candidate bricks in lattice nodes. The hit test builds each world
        // coordinate in f32, so it can accept a brick a few ulps of the
        // largest magnitude in play beyond the exact box; that error in
        // nodes, plus two nodes for this division's own rounding, pads the
        // range. On an ordinary lattice the error is a fraction of a node.
        let lo = domain_min();
        let magnitude = max(max(abs(lo), abs(lo + size)), max(abs(centre), support));
        // Both normal and bounded, so the quotient is finite: it keeps every
        // node index below 1e30.
        if all(magnitude / h < vec3<f32>(1.0e30)) {
            let slack = ceil(magnitude * ROUNDING / h) + vec3<f32>(2.0);
            let from_min = centre - lo;
            let lo_node = floor((from_min - support) / h) - slack - vec3<f32>(f32(HALO_NODES + 7));
            let hi_node = floor((from_min + support) / h) + slack + vec3<f32>(f32(HALO_NODES));
            first_brick = vec3<u32>(clamp(floor(lo_node / 8.0), vec3<f32>(0.0), last));
            last_brick = vec3<u32>(clamp(floor(hi_node / 8.0), vec3<f32>(0.0), last));
        }
    }
    for (var z = first_brick.z; z <= last_brick.z; z = z + 1u) {
        for (var y = first_brick.y; y <= last_brick.y; y = y + 1u) {
            for (var x = first_brick.x; x <= last_brick.x; x = x + 1u) {
                let brick = vec3<u32>(x, y, z);
                if is_border(brick) {
                    continue;
                }
                let id = x + params.bricks_x * (y + params.bricks_y * z);
                if atomicLoad(&hits[id]) == 0u && reaches(brick, centre, reach, extra) {
                    atomicStore(&hits[id], 1u);
                }
            }
        }
    }
}

@compute @workgroup_size(256)
fn mark_bricks(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let id = global_id.x;
    if id >= params.brick_count {
        return;
    }
    let hit = atomicExchange(&hits[id], 0u);
    let marked = select(min(hit, 1u), 1u, is_border(brick_coords(id)));
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

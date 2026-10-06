struct Element {
    center_radius: vec4<f32>,
    shape_diag: vec4<f32>,
    shape_off: vec4<f32>,
}

struct Element2 {
    start: u32,
    count: u32,
}

struct Params {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    resolution_scale: i32,
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
    band_extra: f32,
    brick_pass: u32,
    interior_len: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> buf_blobs: array<Element>;
@group(0) @binding(2) var<storage, read> buf_cell_ranges: array<Element2>;
@group(0) @binding(3) var<storage, read> buf_solid: array<f32>;
@group(0) @binding(4) var<storage, read> buf_bricks: array<u32>;
@group(0) @binding(5) var<storage, read> buf_interior: array<f32>;
@group(0) @binding(6) var<storage, read> buf_bounds: array<f32>;
@group(0) @binding(7) var<storage, read_write> buf_levelset: array<f32>;

// Header: active count, indirect grid x/y/z, brick dimensions x/y/z,
// reserved. Then N occupancy words and N compact-list slots.
fn liquid_brick_active(idx: u32, dims: vec3<u32>) -> bool {
    if idx >= dims.x * dims.y * dims.z { return false; }
    let p = vec3<u32>(idx % dims.x, (idx / dims.x) % dims.y, idx / (dims.x * dims.y));
    let b = p / 8u;
    return buf_bricks[8u + b.x + buf_bricks[4u] * (b.y + buf_bricks[5u] * b.z)] != 0u;
}

// Padding maps to a sentinel rejected before coincident reads/output writes.
fn liquid_brick_map(invocation: u32, dims: vec3<u32>) -> u32 {
    let rank = invocation / 512u;
    if rank >= buf_bricks[0u] { return 0xffffffffu; }
    let bx = buf_bricks[4u];
    let by = buf_bricks[5u];
    let n = bx * by * buf_bricks[6u];
    let brick = buf_bricks[8u + n + rank];
    let b = vec3<u32>(brick % bx, (brick / bx) % by, brick / (bx * by));
    let local = invocation % 512u;
    let p = b * 8u + vec3<u32>(local % 8u, (local / 8u) % 8u, local / 64u);
    if any(p >= dims) { return 0xffffffffu; }
    return p.x + dims.x * (p.y + dims.y * p.z);
}

fn liquid_brick_select(invocation: u32, dims: vec3<u32>, mode: u32) -> u32 {
    if mode == 0u { return invocation; }
    if mode == 1u { return liquid_brick_map(invocation, dims); }
    if liquid_brick_active(invocation, dims) { return 0xffffffffu; }
    return invocation;
}

fn liquid_cell_brick_index(invocation: u32) -> u32 {
    let nodes = vec3<u32>(vec3<f32>(params.nodes_x, params.nodes_y, params.nodes_z));
    let dims = nodes - vec3<u32>(1u);
    if params.brick_pass != 1u { return invocation; }
    return liquid_brick_map(invocation, dims);
}

// Uses the search-radius ratio (1.5 radii) from FLIP Fluids particlemesher.cpp `_searchRadiusFactor` (MIT); see THIRD_PARTY_NOTICES.md.
// Negative-inside port of FLIP Fluids ParticleMesher: initialize 3r, then
// visit each kernel's inclusive floor(p - 1.5r)..floor(p + 1.5r)+1 grid box.
// `bounds` is node.blob_bounds' reduction of the same blobs: the largest kernel
// radius and support. It sets the search reach, never a quality limit.
// Solid zeros invert the native sign convention. Preview-only border replacement
// is not part of the production mesher.

fn pv_solid(p: vec3<f32>, lattice_min: vec3<f32>, spacing: vec3<f32>, nodes: vec3<u32>) -> f32 {
    let g = clamp((p - lattice_min) / spacing, vec3<f32>(0.0), vec3<f32>(nodes - vec3<u32>(1u)));
    let base = min(vec3<u32>(floor(g)), nodes - vec3<u32>(2u));
    let f = g - vec3<f32>(base);
    var value = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, (corner >> 2u) & 1u);
        let at = base + o;
        let w = select(1.0 - f.x, f.x, o.x == 1u)
            * select(1.0 - f.y, f.y, o.y == 1u)
            * select(1.0 - f.z, f.z, o.z == 1u);
        value = value + w * buf_solid[at.x + nodes.x * (at.y + nodes.y * at.z)];
    }
    return value;
}

fn pv_interior(
    p: vec3<f32>,
    lattice_min: vec3<f32>,
    spacing: vec3<f32>,
    solid_nodes: vec3<u32>,
    cells: vec3<u32>,
) -> f32 {
    // Interior samples live at the centres of the simulation cells. Subtract
    // half a cell before clamping so a volume node on a cell centre reads that
    // cell exactly; the clamp gives the expected boundary extension.
    let top = cells - vec3<u32>(1u);
    let physical_min = lattice_min + 0.5 * vec3<f32>(solid_nodes - cells - vec3<u32>(1u)) * spacing;
    let g = clamp((p - physical_min) / spacing - vec3<f32>(0.5), vec3<f32>(0.0), vec3<f32>(top));
    let base = min(vec3<u32>(floor(g)), top);
    let f = g - vec3<f32>(base);
    var value = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, (corner >> 2u) & 1u);
        let at = min(base + o, top);
        let w = select(1.0 - f.x, f.x, o.x == 1u)
            * select(1.0 - f.y, f.y, o.y == 1u)
            * select(1.0 - f.z, f.z, o.z == 1u);
        value = value + w * buf_interior[at.x + cells.x * (at.y + cells.y * at.z)];
    }
    return value;
}

fn body(
    idx: u32,
    count: u32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    resolution_scale: i32,
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
    band_extra: f32,
    brick_pass: u32,
    interior_len: u32,
) -> f32 {

    let solid_nodes = max(vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z)), vec3<u32>(2u));
    let scale = u32(clamp(resolution_scale, 1, 8));
    let nodes = (solid_nodes - vec3<u32>(1u)) * scale + vec3<u32>(1u);
    let margin = length(vec3<f32>(size_x,size_y,size_z) / vec3<f32>(nodes - vec3<u32>(1u)));
    let extra = band_extra + select(0.0, margin, band_extra > 0.0);
    let bound = vec2<f32>(buf_bounds[0], buf_bounds[1]);
    let band = 3.0 * bound.x + extra;
    let bins = vec3<i32>(bins_x, bins_y, bins_z);
    if idx >= nodes.x * nodes.y * nodes.z {
        return band;
    }
    if any(bins < vec3<i32>(1)) && brick_pass != 2u {
        return band;
    }
    let ijk = vec3<u32>(idx % nodes.x, (idx / nodes.x) % nodes.y, idx / (nodes.x * nodes.y));
    let size = vec3<f32>(size_x, size_y, size_z);
    let lattice_min = vec3<f32>(center_x, center_y, center_z) - 0.5 * size;
    let p = lattice_min + vec3<f32>(ijk) * size / vec3<f32>(nodes - vec3<u32>(1u));

    var phi = band;
    if brick_pass != 2u {
        let home = clamp(vec3<i32>(floor((p - lattice_min) / cell_size)), vec3<i32>(0), bins - vec3<i32>(1));
        let reach_bins = i32(ceil((bound.y + extra + margin) / cell_size));
        // The existing bound includes kernel support, centre displacement and
        // the outer interpolation node. Locate its endpoints within the bins
        // instead of rounding its radius up around an entire home bin. Expand
        // outwards by more than eight f32 epsilons at the coordinate scale
        // for coordinate and reciprocal-bin rounding, then only
        // narrow the old search: contributor arithmetic/order stays unchanged.
        let reach_world = bound.y + extra + margin;
        let roundoff = 0.000001 * (abs(lattice_min) + abs(size) + vec3<f32>(reach_world + cell_size));
        let query_lo = vec3<i32>(floor(clamp((p - lattice_min - vec3<f32>(reach_world) - roundoff) / cell_size, vec3<f32>(0.0), vec3<f32>(bins - vec3<i32>(1)))));
        let query_hi = vec3<i32>(floor(clamp((p - lattice_min + vec3<f32>(reach_world) + roundoff) / cell_size, vec3<f32>(0.0), vec3<f32>(bins - vec3<i32>(1)))));
        let first_bin = max(max(home - vec3<i32>(reach_bins), vec3<i32>(0)), query_lo);
        let last_bin = min(min(home + vec3<i32>(reach_bins), bins - vec3<i32>(1)), query_hi);
        for (var z = first_bin.z; z <= last_bin.z; z = z + 1) {
            for (var y = first_bin.y; y <= last_bin.y; y = y + 1) {
                for (var x = first_bin.x; x <= last_bin.x; x = x + 1) {
                    let b = vec3<i32>(x, y, z);
                    let range = buf_cell_ranges[u32(b.x + bins.x * (b.y + bins.y * b.z))];
                    for (var k = range.start; k < range.start + range.count; k = k + 1u) {
                        let blob = buf_blobs[k];
                        let reach = blob.center_radius.w;
                        let d = p - blob.center_radius.xyz;
                        // Native support is a grid-aligned box including its outer
                        // interpolation node. Spherical rejection loses the corners.
                        let h = size / vec3<f32>(nodes - vec3<u32>(1u));
                        let support = 1.5 * reach + extra;
                        let first = vec3<i32>(floor((blob.center_radius.xyz - vec3<f32>(support) - lattice_min) / h));
                        let last = vec3<i32>(floor((blob.center_radius.xyz + vec3<f32>(support) - lattice_min) / h)) + vec3<i32>(1);
                        if !(reach > 0.0) || any(vec3<i32>(ijk) < first) || any(vec3<i32>(ijk) > last) {
                            continue;
                        }
                        let diag = blob.shape_diag;
                        let off = blob.shape_off;
                        let v = vec3<f32>(
                            diag.x * d.x + off.x * d.y + off.y * d.z,
                            off.x * d.x + diag.y * d.y + off.z * d.z,
                            off.y * d.x + off.z * d.y + diag.z * d.z,
                        );
                        phi = min(phi, reach * (length(v) - 1.0));
                    }
                }
            }
        }
    }
    let spacing = size / vec3<f32>(solid_nodes - vec3<u32>(1u));
    let physical_nodes = max(solid_nodes, vec3<u32>(8u));
    // Exact buffer length distinguishes native mesh padding from the legacy
    // solver lattice. CPU and extent validation reject every other shape.
    let native_cells = max(solid_nodes, vec3<u32>(5u)) - vec3<u32>(4u);
    let native_total = native_cells.x * native_cells.y * native_cells.z;
    let interior_cells = select(physical_nodes - vec3<u32>(7u), native_cells, interior_len == native_total);
    let interior_total = interior_cells.x * interior_cells.y * interior_cells.z;
    if interior_len == interior_total && interior_len != 0u {
        // A simulation cell width is the narrow-band unit. Rectangular boxes
        // retain their per-axis interpolation spacing; the smallest axis is a
        // conservative physical h for the Eq. 4 one-cell shrink.
        let h = min(spacing.x, min(spacing.y, spacing.z));
        phi = min(phi, pv_interior(p, lattice_min, spacing, solid_nodes, interior_cells) + h);
    }
    if pv_solid(p, lattice_min, spacing, solid_nodes) < 0.0 {
        phi = max(phi, 0.0);
    }
    return phi;
}

fn liquid_brick_index(invocation: u32) -> u32 {
    let dims = (max(vec3<u32>(vec3<f32>(params.nodes_x, params.nodes_y, params.nodes_z)), vec3<u32>(2u)) - vec3<u32>(1u)) * u32(clamp(params.resolution_scale, 1, 8)) + vec3<u32>(1u);
    return liquid_brick_select(invocation, dims, params.brick_pass);
}

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = liquid_brick_index(gid.x);
    if idx == 0xffffffffu {
        return;
    }
    if idx >= params.dispatch_count {
        return;
    }
    buf_levelset[idx] = body(idx, params.dispatch_count, params.center_x, params.center_y, params.center_z, params.size_x, params.size_y, params.size_z, params.nodes_x, params.nodes_y, params.nodes_z, params.cell_size, params.resolution_scale, params.bins_x, params.bins_y, params.bins_z, params.band_extra, params.brick_pass, params.interior_len);
}

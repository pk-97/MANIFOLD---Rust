// Uses the search-radius ratio (1.5 radii) from FLIP Fluids particlemesher.cpp `_searchRadiusFactor` (MIT); see THIRD_PARTY_NOTICES.md.
// node.particle_volume's arithmetic, the only copy. Both kernels call these
// helpers: the generated per-node kernel (particle_volume_body.wgsl, passes 0
// and 2 and the bitwise oracle) and the cooperative pass-1 kernel
// (particle_volume_brick_gather.wgsl). Reads the kernel's `buf_solid` and
// `buf_interior` bindings.
//
// Negative-inside port of FLIP Fluids ParticleMesher: initialize 3r, then
// visit each kernel's inclusive floor(p - 1.5r)..floor(p + 1.5r)+1 grid box.
// `bound` is node.blob_bounds' reduction of the same blobs: the largest kernel
// radius and support. It sets the search reach, never a quality limit.
// Solid zeros invert the native sign convention.

struct PvFrame {
    solid_nodes: vec3<u32>,
    nodes: vec3<u32>,
    size: vec3<f32>,
    lattice_min: vec3<f32>,
    // Refined node spacing.
    h: vec3<f32>,
    margin: f32,
    extra: f32,
    band: f32,
    bound: vec2<f32>,
}

fn pv_frame(
    center: vec3<f32>,
    size: vec3<f32>,
    solid_f: vec3<f32>,
    resolution_scale: i32,
    band_extra: f32,
    bound: vec2<f32>,
) -> PvFrame {
    var f: PvFrame;
    f.solid_nodes = max(vec3<u32>(solid_f), vec3<u32>(2u));
    let scale = u32(clamp(resolution_scale, 1, 8));
    f.nodes = (f.solid_nodes - vec3<u32>(1u)) * scale + vec3<u32>(1u);
    f.size = size;
    f.h = size / vec3<f32>(f.nodes - vec3<u32>(1u));
    f.margin = length(f.h);
    f.extra = band_extra + select(0.0, f.margin, band_extra > 0.0);
    f.bound = bound;
    f.band = 3.0 * bound.x + f.extra;
    f.lattice_min = center - 0.5 * size;
    return f;
}

fn pv_ijk(idx: u32, nodes: vec3<u32>) -> vec3<u32> {
    return vec3<u32>(idx % nodes.x, (idx / nodes.x) % nodes.y, idx / (nodes.x * nodes.y));
}

fn pv_position(ijk: vec3<u32>, f: PvFrame) -> vec3<f32> {
    return f.lattice_min + vec3<f32>(ijk) * f.size / vec3<f32>(f.nodes - vec3<u32>(1u));
}

struct PvWindow {
    first_bin: vec3<i32>,
    last_bin: vec3<i32>,
}

// The existing bound includes kernel support, centre displacement and the
// outer interpolation node. Locate its endpoints within the bins instead of
// rounding its radius up around an entire home bin. Expand outwards by more
// than eight f32 epsilons at the coordinate scale for coordinate and
// reciprocal-bin rounding, then only narrow the old search: contributor
// arithmetic and order stay unchanged.
fn pv_window(p: vec3<f32>, f: PvFrame, cell_size: f32, bins: vec3<i32>) -> PvWindow {
    let home = clamp(vec3<i32>(floor((p - f.lattice_min) / cell_size)), vec3<i32>(0), bins - vec3<i32>(1));
    let reach_bins = i32(ceil((f.bound.y + f.extra + f.margin) / cell_size));
    let reach_world = f.bound.y + f.extra + f.margin;
    let roundoff = 0.000001 * (abs(f.lattice_min) + abs(f.size) + vec3<f32>(reach_world + cell_size));
    let query_lo = vec3<i32>(floor(clamp((p - f.lattice_min - vec3<f32>(reach_world) - roundoff) / cell_size, vec3<f32>(0.0), vec3<f32>(bins - vec3<i32>(1)))));
    let query_hi = vec3<i32>(floor(clamp((p - f.lattice_min + vec3<f32>(reach_world) + roundoff) / cell_size, vec3<f32>(0.0), vec3<f32>(bins - vec3<i32>(1)))));
    var w: PvWindow;
    w.first_bin = max(max(home - vec3<i32>(reach_bins), vec3<i32>(0)), query_lo);
    w.last_bin = min(min(home + vec3<i32>(reach_bins), bins - vec3<i32>(1)), query_hi);
    return w;
}

struct PvBox {
    first: vec3<i32>,
    last: vec3<i32>,
}

// Native support is a grid-aligned box including its outer interpolation
// node. Spherical rejection loses the corners.
fn pv_blob_box(center_radius: vec4<f32>, f: PvFrame) -> PvBox {
    let support = 1.5 * center_radius.w + f.extra;
    var b: PvBox;
    b.first = vec3<i32>(floor((center_radius.xyz - vec3<f32>(support) - f.lattice_min) / f.h));
    b.last = vec3<i32>(floor((center_radius.xyz + vec3<f32>(support) - f.lattice_min) / f.h)) + vec3<i32>(1);
    return b;
}

fn pv_box_rejects(reach: f32, ijk: vec3<u32>, b: PvBox) -> bool {
    return !(reach > 0.0) || any(vec3<i32>(ijk) < b.first) || any(vec3<i32>(ijk) > b.last);
}

fn pv_blob_term(p: vec3<f32>, center_radius: vec4<f32>, diag: vec3<f32>, off: vec3<f32>) -> f32 {
    let d = p - center_radius.xyz;
    let v = vec3<f32>(
        diag.x * d.x + off.x * d.y + off.y * d.z,
        off.x * d.x + diag.y * d.y + off.z * d.z,
        off.y * d.x + off.z * d.y + diag.z * d.z,
    );
    return center_radius.w * (length(v) - 1.0);
}

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

// After the gather: interior union, then the solid clamp.
fn pv_finish(phi_in: f32, p: vec3<f32>, f: PvFrame, interior_len: u32) -> f32 {
    var phi = phi_in;
    let solid_nodes = f.solid_nodes;
    let spacing = f.size / vec3<f32>(solid_nodes - vec3<u32>(1u));
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
        phi = min(phi, pv_interior(p, f.lattice_min, spacing, solid_nodes, interior_cells) + h);
    }
    if pv_solid(p, f.lattice_min, spacing, solid_nodes) < 0.0 {
        phi = max(phi, 0.0);
    }
    return phi;
}

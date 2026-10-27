var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.push_out_of_solid — fusable BUFFER body, COINCIDENT particles,
// GATHER signed-distance field and world-tile table. The solid is a node
// array of the display's world tiles: node p, at (p − ½)·h, is element
// `ts_cell_at(p)` of `buf_solid`, negative inside the solid. A node in a tile
// the table does not hold reads the canonical element, 1e30: no solid.

// Past this many cells from the origin a position is no water's: it keeps
// the node coordinate inside an i32.
const PO_REACH_CELLS: f32 = 1.0e9;

fn po_node(n: vec3<i32>) -> f32 {
    return buf_solid[ts_cell_at(n)];
}

// Trilinear between the eight nodes round node coordinate q.
fn po_sample(q: vec3<f32>) -> f32 {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var value = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let offset = vec3<i32>(i32(corner & 1u), i32((corner >> 1u) & 1u), i32((corner >> 2u) & 1u));
        let weight = select(1.0 - f.x, f.x, offset.x == 1)
            * select(1.0 - f.y, f.y, offset.y == 1)
            * select(1.0 - f.z, f.z, offset.z == 1);
        value = value + weight * po_node(base + offset);
    }
    return value;
}

fn po_gradient(q: vec3<f32>, h: f32) -> vec3<f32> {
    // Half a cell on either side gives a central difference while retaining
    // the same trilinear value at arbitrary particle positions.
    let dx = po_sample(q + vec3<f32>(0.5, 0.0, 0.0)) - po_sample(q - vec3<f32>(0.5, 0.0, 0.0));
    let dy = po_sample(q + vec3<f32>(0.0, 0.5, 0.0)) - po_sample(q - vec3<f32>(0.0, 0.5, 0.0));
    let dz = po_sample(q + vec3<f32>(0.0, 0.0, 0.5)) - po_sample(q - vec3<f32>(0.0, 0.0, 0.5));
    return vec3<f32>(dx, dy, dz) / max(h, 1.0e-6);
}

fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    cell_size: f32,
    pool_tiles: f32,
) -> Element {
    ww_pool_tiles = u32(pool_tiles);
    var out = e_particles;
    if !(e_particles.position_radius.w > 0.0) {
        return out;
    }
    let h = cell_size;
    var position = e_particles.position_radius.xyz;
    // Four fixed normalized-gradient steps cover curvature and trilinear
    // interpolation error while keeping this a barrier-free per-particle
    // body with deterministic work.
    for (var iteration = 0u; iteration < 4u; iteration = iteration + 1u) {
        let q = position / h + vec3<f32>(0.5);
        if !all(abs(q) < vec3<f32>(PO_REACH_CELLS)) {
            break;
        }
        ts_home(ts_cell_at(vec3<i32>(floor(q))));
        let phi = po_sample(q);
        if !(phi < 0.0) {
            break;
        }
        let gradient = po_gradient(q, h);
        let gradient_length = length(gradient);
        // A flat negative field has no defined outward direction. Keep it
        // stable rather than producing NaNs or an arbitrary axis push.
        if !(gradient_length > 1.0e-6)
            || !(gradient_length < 3.402823e+38)
            || !(gradient_length == gradient_length)
        {
            break;
        }
        position = position - phi * gradient / gradient_length;
    }
    out.position_radius = vec4<f32>(position, e_particles.position_radius.w);
    return out;
}

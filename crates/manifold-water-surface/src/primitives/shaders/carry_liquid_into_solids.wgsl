// Carries the water field into drawn solids so the drawn surface meets a wall
// flush, on the display's world tiles. The level set, the solid and the drawn
// mask are node arrays on the same tiles (`buf_tiles`): element p is display
// node p, at (p - 1/2) cells, so the solid at a node is its own element and a
// neighbour is one read through `ts_cell_at`. A node in a tile the table does
// not hold reads the canonical element: the level set three cells (FIELD_FAR_CELLS),
// the solid 1e30.
//
// One thread per pool element. Outside every solid the value is kept. Inside
// a solid whose water is carried (drawn, or every solid when no drawn mask is
// wired), a node within `carry_depth` of the solid's surface takes the level
// set sampled just inside the water at the surface point along the solid's
// normal, deeper nodes are air, and every other solid node is clipped to
// max(value, 0). A free slot and the canonical element hold the far value.

struct CarryParams {
    cell: f32,
    carry_depth: f32,
    air: f32,
    pool_tiles: u32,
    element_count: u32,
    has_drawn: u32,
    pad_a: u32,
    pad_b: u32,
}

@group(0) @binding(0) var<uniform> params: CarryParams;
@group(0) @binding(1) var<storage, read> buf_levelset: array<f32>;
@group(0) @binding(2) var<storage, read> buf_solid: array<f32>;
@group(0) @binding(3) var<storage, read> buf_drawn_solid: array<f32>;
@group(0) @binding(4) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(5) var<storage, read_write> buf_out: array<f32>;

var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// FIELD_FAR_CELLS (scene_object), the level set where no tile is held.
const CARRY_FAR_CELLS: f32 = 3.0;
// The solid reads 1e30 in a tile no slot holds; stopping a neighbour read here
// keeps the gradient's differences, and their length, finite.
const CARRY_SOLID_FAR: f32 = 1.0e4;

fn carry_solid(node: vec3<i32>) -> f32 {
    return min(buf_solid[ts_cell_at(node)], CARRY_SOLID_FAR);
}

// The level set at node coordinate g (x / h + 1/2), trilinear on the nodes
// round it.
fn carry_levelset_at(g: vec3<f32>) -> f32 {
    let lower = floor(g);
    let f = g - lower;
    let base = vec3<i32>(lower);
    var value = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<i32>(i32(corner & 1u), i32((corner >> 1u) & 1u), i32((corner >> 2u) & 1u));
        let w = select(1.0 - f.x, f.x, o.x == 1)
            * select(1.0 - f.y, f.y, o.y == 1)
            * select(1.0 - f.z, f.z, o.z == 1);
        value = value + w * buf_levelset[ts_cell_at(base + o)];
    }
    return value;
}

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.element_count { return; }
    ww_pool_tiles = params.pool_tiles;
    if idx >= ts_canon() {
        buf_out[idx] = CARRY_FAR_CELLS * params.cell;
        return;
    }
    if ts_slot_entry(idx >> 9u) == WT_NONE {
        buf_out[idx] = CARRY_FAR_CELLS * params.cell;
        return;
    }
    let v = buf_levelset[idx];
    let sd = buf_solid[idx];
    if sd >= 0.0 {
        buf_out[idx] = v;
        return;
    }
    var result = max(v, 0.0);
    let carried = params.carry_depth > 0.0 && (params.has_drawn == 0u || buf_drawn_solid[idx] < 0.0);
    if carried {
        if sd <= -params.carry_depth {
            result = params.air;
        } else {
            ts_home(idx);
            let node = ts_cell_coord(idx);
            let grad = vec3<f32>(
                carry_solid(node + vec3<i32>(1, 0, 0)) - carry_solid(node - vec3<i32>(1, 0, 0)),
                carry_solid(node + vec3<i32>(0, 1, 0)) - carry_solid(node - vec3<i32>(0, 1, 0)),
                carry_solid(node + vec3<i32>(0, 0, 1)) - carry_solid(node - vec3<i32>(0, 0, 1)),
            );
            let len = length(grad);
            if len > 0.0 {
                let n = grad / len;
                // The surface point, half a cell into the water, in node coordinates.
                result = carry_levelset_at(vec3<f32>(node) + (0.5 * params.cell - sd) * n / params.cell);
            }
        }
    }
    buf_out[idx] = result;
}

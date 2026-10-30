// The display world-tile table a water field is read through. Starts after
// `world_tiles_read.wgsl` (`world_tiles::READ_SIDE`), whose helpers need the
// three table functions defined here. Every array of the field (level set,
// velocity, foam, mist, wetness) is a pooled field under this one table: node
// p sits at (p - 1/2) cells in the object's space, its tile is p >> 3, and a
// node in a tile the table does not hold reads the array's last, canonical
// element.

@group(0) @binding(70) var<storage, read> field_tiles: array<u32>;
// Eight floats: the held tiles' node bounds in object space (min xyz, 0, max
// xyz, 0), min above max when no tile is held.
@group(0) @binding(71) var<storage, read> field_extent: array<f32>;

// The level set's canonical value in cells (`FIELD_FAR_CELLS` in
// `scene_object.rs`).
const FIELD_FAR_CELLS: f32 = 3.0;

// The pool size of the field the running entry point reads (its H is the
// table's own). The entry point, or the look hook that reads a field, sets it
// from its uniforms before the first read.
var<private> field_pool: u32;

fn ts_pool_tiles() -> u32 { return field_pool; }
fn ts_table_word(i: u32) -> u32 { return field_tiles[i]; }

fn field_extent_min() -> vec3<f32> {
    return vec3<f32>(field_extent[0], field_extent[1], field_extent[2]);
}

fn field_extent_max() -> vec3<f32> {
    return vec3<f32>(field_extent[4], field_extent[5], field_extent[6]);
}

// Whether the extent holds a tile.
fn field_extent_empty() -> bool {
    return any(field_extent_min() > field_extent_max());
}

// Whether p is in the extent widened by a cell on every side: the cells that
// interpolate its outermost nodes. False when no tile is held.
fn field_box_holds(p: vec3<f32>, cell: f32) -> bool {
    let lo = field_extent_min();
    let hi = field_extent_max();
    return all(lo <= hi) && all(p >= lo - vec3<f32>(cell)) && all(p <= hi + vec3<f32>(cell));
}

// The node coordinate of an object-space point: node p sits at (p - 1/2) cells.
fn field_node_coord(p: vec3<f32>, cell: f32) -> vec3<f32> {
    return p / cell + vec3<f32>(0.5);
}

// Makes the tile of node floor(q) the home of the reads that follow; false
// when the table does not hold it. One call per sample.
fn field_enter(q: vec3<f32>) -> bool {
    ts_home(ts_cell_at(vec3<i32>(floor(q))));
    return ts_home_slot != WT_NONE;
}

// The pool elements of the eight nodes around q (corner xyz is the offset from
// node floor(q)) and q's place between them.
struct FieldCorners {
    c000: u32,
    c100: u32,
    c010: u32,
    c110: u32,
    c001: u32,
    c101: u32,
    c011: u32,
    c111: u32,
    f: vec3<f32>,
};

fn field_corners(q: vec3<f32>) -> FieldCorners {
    let lower = floor(q);
    let b = vec3<i32>(lower);
    return FieldCorners(
        ts_cell_at(b),
        ts_cell_at(b + vec3<i32>(1, 0, 0)),
        ts_cell_at(b + vec3<i32>(0, 1, 0)),
        ts_cell_at(b + vec3<i32>(1, 1, 0)),
        ts_cell_at(b + vec3<i32>(0, 0, 1)),
        ts_cell_at(b + vec3<i32>(1, 0, 1)),
        ts_cell_at(b + vec3<i32>(0, 1, 1)),
        ts_cell_at(b + vec3<i32>(1, 1, 1)),
        q - lower,
    );
}

fn field_mix(
    v000: f32, v100: f32, v010: f32, v110: f32,
    v001: f32, v101: f32, v011: f32, v111: f32,
    f: vec3<f32>,
) -> f32 {
    let x00 = mix(v000, v100, f.x);
    let x10 = mix(v010, v110, f.x);
    let x01 = mix(v001, v101, f.x);
    let x11 = mix(v011, v111, f.x);
    return mix(mix(x00, x10, f.y), mix(x01, x11, f.y), f.z);
}

fn field_mix3(
    v000: vec3<f32>, v100: vec3<f32>, v010: vec3<f32>, v110: vec3<f32>,
    v001: vec3<f32>, v101: vec3<f32>, v011: vec3<f32>, v111: vec3<f32>,
    f: vec3<f32>,
) -> vec3<f32> {
    let x00 = mix(v000, v100, f.x);
    let x10 = mix(v010, v110, f.x);
    let x01 = mix(v001, v101, f.x);
    let x11 = mix(v011, v111, f.x);
    return mix(mix(x00, x10, f.y), mix(x01, x11, f.y), f.z);
}

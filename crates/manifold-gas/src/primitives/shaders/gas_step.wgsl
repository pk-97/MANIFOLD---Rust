// The gas solver's step: smoke or fire in a box of cells held on world tiles,
// advanced one tick by the kernels below in command order (the host composes
// the BLACKBODY_*, LIGHT_FIXED_SCALE, LIGHT_WORDS, FINE and SOLIDS constants
// ahead of this source; it builds the kernels once with SOLIDS and once
// without, so a box with no solid in the scene runs none of their code).
//
// The box's cells are world cells of the gas's own tile table
// (world_tiles_read.wgsl, `ts_`): box cell c is world cell first + c, centred
// on origin + (c + 1/2) h. The table holds only the tiles with gas, the
// source's tiles and a ring of one tile round them, rebuilt every tick
// (gas_volume.rs); every field is pooled, a record per cell, and a cell of a
// tile the table does not hold reads the canonical record, still air with no
// gas. The step's kernels run over the held tiles' cells, 512 threads a slot
// (`list_coord`). Velocity is staggered (a MAC grid): record c holds u on the
// face at x - 1/2, v on the face at y - 1/2 and w on the face at z - 1/2 of
// cell c, so divergence, pressure and projection share one compact stencil
// and no cell-to-cell zigzag hides from the solve; the faces on the box's far
// sides live with the cells one past it. The floor (y = 0) is a wall, the
// sides and the top are open, and so is every tile the table does not hold
// (pressure 0, still air). Velocity is metres
// per second. The state is x density (soot), y temperature (0 ambient, 1 a
// hot flame), z fuel and w flame, and it lives on a grid FINE times finer
// than the air along each axis, in its air cell's record (see "The fine
// state"); the air reads it averaged back onto its own cells (`state_a`). A cell inside one of the
// scene's solids holds no gas, its faces move with the solid, and the
// pressure solve treats it as a wall. Water and sand are solids too: a cell
// the water fills, read through the water's own tiles, or at least half full
// of sand, read from the Sand's own grid, moves with them (cs_solids).
//
// Roles rotate: the host names three velocity and three fine state buffers
// and binds them as vel_in/fine_in (last tick's result), vel_a/fine_a and
// vel_b/fine_b. Advection reads the first and writes the others; after it,
// the step's working fields R are bound at vel_a/fine_a for every later
// kernel.

struct Params {
    dims: vec3<u32>,
    // Points cs_source_points lays for the build, one per source tile.
    source_count: u32,
    origin: vec3<f32>,
    h: f32,
    up: vec3<f32>,
    g: f32,
    dt: f32,
    time: f32,
    kind: u32,
    pad: u32,
    source_size: f32,
    rate: f32,
    inflow: f32,
    source_heat: f32,
    buoyancy: f32,
    weight: f32,
    vorticity: f32,
    turbulence: f32,
    dissipation: f32,
    cooling: f32,
    burn_rate: f32,
    heat_release: f32,
    soot: f32,
    expansion: f32,
    ignition: f32,
    seed: u32,
    // The lattice over the box the scene's forces and hits are laid on: its
    // first node in metres, node spacing and node counts. This tick reads the
    // force lattice from node `force_base` of `forces` when `forces_on` is
    // nonzero, and adds the hits' velocity change from node `impulse_base`
    // of `impulses` when `impulse_on` is.
    force_min: vec3<f32>,
    force_spacing: f32,
    force_nodes: vec3<u32>,
    forces_on: u32,
    force_base: u32,
    impulse_on: u32,
    impulse_base: u32,
    // The scene's solids this tick (SOLIDS): `solid_count` body rows from
    // row `solid_base` of `bodies`.
    solid_base: u32,
    solid_count: u32,
    // The water this tick (SOLIDS): its tiles' pool, 0 while none reaches the
    // gas, and its cell in metres; and one over the Sand's cell, 0 while none
    // does.
    water_pool: u32,
    sand_inv_cell: f32,
    water_cell: f32,
    // The box's first cell in world cells, the table's pool P, and the
    // source's tiles along each axis (source_count in all).
    first: vec3<i32>,
    pool_tiles: u32,
    source_tiles: vec3<u32>,
    // Word of the table's args holding the held slots' triple's x.
    held_word: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> vel_in: array<vec4<f32>>;
// Fine state, two words a fine cell (see "The fine state").
@group(0) @binding(2) var<storage, read> fine_in: array<u32>;
@group(0) @binding(3) var<storage, read_write> vel_a: array<vec4<f32>>;
// The fine state averaged onto each air cell this tick (cs_restrict).
@group(0) @binding(4) var<storage, read_write> state_a: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> vel_b: array<vec4<f32>>;
@group(0) @binding(6) var<storage, read_write> fine_a: array<u32>;
@group(0) @binding(7) var<storage, read_write> curl: array<vec4<f32>>;
@group(0) @binding(8) var<storage, read_write> pressure: array<f32>;
// With solids, a cell's divergence carries in its three lowest bits how many
// of its six neighbours are not solid (cs_divergence): 0 for a cell the
// pressure solve leaves alone. It costs the divergence a millionth of itself.
@group(0) @binding(9) var<storage, read_write> divergence: array<f32>;
@group(0) @binding(10) var<storage, read_write> fine_b: array<u32>;
// The air's velocity at each fine cell's centre, published on the frame's
// last tick: two words, halves (u, v) then (w, 0).
@group(0) @binding(11) var<storage, read_write> velocity_out: array<u32>;
@group(0) @binding(12) var<storage, read_write> light_sums: array<atomic<u32>>;
@group(0) @binding(13) var<storage, read> blackbody: array<vec4<f32>>;
// Two vec4 a lattice node, x fastest: (push, drag rate) then (drag rate ×
// wind, 0), as every solver that feels the scene's forces reads them.
@group(0) @binding(14) var<storage, read> forces: array<vec4<f32>>;
// One vec4 a lattice node: a hit's velocity change in m/s (xyz).
@group(0) @binding(15) var<storage, read> impulses: array<vec4<f32>>;

// The scene's Colliders and Bodies as every solver that flows round them
// reads them (liquid::bodies): a row per body per tick, its shape, the
// shapes' distance lattices two halves a word, and a Collider mesh's tree.
struct LiquidBody {
    position_inv_mass: vec4<f32>,
    rotation: vec4<f32>,
    linear_velocity: vec4<f32>,
    angular_velocity: vec4<f32>,
    inv_inertia_x: vec4<f32>,
    inv_inertia_y: vec4<f32>,
    inv_inertia_z: vec4<f32>,
    accel_shape: vec4<f32>,
}

struct LiquidShape {
    origin_spacing: vec4<f32>,
    dims_x: u32,
    dims_y: u32,
    dims_z: u32,
    atlas_offset: u32,
    scale_min: vec4<f32>,
    tree_node: u32,
    tree_triangle: u32,
    tree: u32,
    tree_pad: u32,
}

@group(0) @binding(16) var<storage, read> bodies: array<LiquidBody>;
@group(0) @binding(17) var<storage, read> shapes: array<LiquidShape>;
@group(0) @binding(18) var<storage, read> atlas: array<u32>;
@group(0) @binding(19) var<storage, read> collider_nodes: array<vec4<f32>>;
@group(0) @binding(20) var<storage, read> collider_triangles: array<vec4<f32>>;
// Two words a cell: the solid's velocity there, (x, y) then (z, kind) as
// halves. Kind 0 is air, 1 inside a Collider's mesh (cs_solids_static), 2
// inside a body or a hull, 3 water or sand (cs_solids).
@group(0) @binding(21) var<storage, read_write> solid: array<u32>;
// The source sits on the static solids under it: one word, the box's height
// in cells less the lowest air cell of the column through the source's
// centre (cs_solids_static), so the largest is the lowest.
@group(0) @binding(22) var<storage, read_write> source_floor: array<atomic<u32>>;
// The first Water's step as this tick left it: its pooled faces and
// distance (metres, negative inside the water) on its world tiles.
struct FaceSample {
    face_velocity: vec4<f32>,
    face_weight: vec4<f32>,
}
@group(0) @binding(24) var<storage, read> water_faces: array<FaceSample>;
@group(0) @binding(28) var<storage, read> water_distance: array<f32>;
@group(0) @binding(29) var<storage, read> water_tiles: array<u32>;
// The first Sand's grid as its tick ended (sand_read.wgsl): its nodes, its
// bulk and its table, whose header carries its pool.
@group(0) @binding(23) var<storage, read> sand_grid: array<SandNode>;
@group(0) @binding(25) var<storage, read> sand_bulk: array<u32>;
@group(0) @binding(30) var<storage, read> sand_tiles: array<u32>;
fn ms_pool_tiles() -> u32 { return sand_tiles[1]; }
fn ms_table_word(i: u32) -> u32 { return sand_tiles[i]; }
// The gas's own table, and the slots a list kernel runs over: every held
// slot, or the slots this tick's build handed out.
@group(0) @binding(31) var<storage, read> gas_tiles: array<u32>;
@group(0) @binding(32) var<storage, read> slots: array<u32>;
// One word a slot: the local index of a cell of the slot's tile that holds
// gas, WT_NONE for none (world_tiles_kept.wgsl): the next build keeps it.
@group(0) @binding(33) var<storage, read_write> kept: array<atomic<u32>>;
// The source's points for the build (cs_source_points): world cells.
@group(0) @binding(34) var<storage, read_write> source_points: array<vec4<i32>>;
// The table's indirect triples, and the fine kernels' triple cs_fine_args
// lays from its held one: SUBS times the threads, one a fine cell.
@group(0) @binding(35) var<storage, read> table_args: array<u32>;
@group(0) @binding(36) var<storage, read_write> fine_args: array<u32>;
fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return gas_tiles[i]; }
// One bit a body row (bit b % 32 of word b / 32): a Collider that moves this
// frame, laid each tick by cs_solids and left out of the static solids.
@group(0) @binding(26) var<storage, read> moving: array<u32>;
// The fine state published on the frame's last tick, as fine_a holds it.
@group(0) @binding(27) var<storage, read_write> state_out: array<u32>;
// A cell at least this full of water or grains is a solid to the gas.
const MATTER_FULL: f32 = 0.5;

fn ws_pool_tiles() -> u32 { return params.water_pool; }
fn ws_table_word(i: u32) -> u32 { return water_tiles[i]; }

// The water's velocity and the share of the water's cell it fills at world
// position x: FLIP's MAC trilinear on the faces, and the distance trilinear
// at cell centres, 1 half a cell inside the surface, 0 half a cell out. A
// cell the water's tiles do not hold has no water.
fn gas_water(x: vec3<f32>) -> vec4<f32> {
    let q = ww_cell_position(x, params.water_cell);
    let cell = vec3<i32>(floor(q));
    ws_home(ws_cell_at(cell));
    if !ws_cell_stored(cell) {
        return vec4<f32>(0.0);
    }
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let face = water_faces[ws_face(base + vec3<i32>(ww_corner(corner)))];
            sum = sum + ww_corner_weight(f, corner) * lf_face_velocity(face.face_velocity, face.face_weight, axis);
        }
        v[axis] = sum;
    }
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * water_distance[ws_cell_at(base + vec3<i32>(ww_corner(corner)))];
    }
    return vec4<f32>(v, clamp(0.5 - d / params.water_cell, 0.0, 1.0));
}

fn liquid_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

fn cb_node(i: u32) -> vec4<f32> {
    return collider_nodes[i];
}

fn cb_triangle(i: u32) -> vec4<f32> {
    return collider_triangles[i];
}

// How fast a flame fades once its fuel is gone.
const FLAME_SECONDS: f32 = 0.06;
// Fuel that does not burn fades at this rate, per second.
const FUEL_DECAY: f32 = 0.5;
const MAX_TEMPERATURE: f32 = 1.5;
// Over-relaxation of the pressure sweeps.
const SOR_OMEGA: f32 = 1.6;
// The open faces absorb: over this share of the box next to a side or the
// top the gas fades at up to SPONGE_RATE per second, so it thins out before
// it reaches a face and the box never shows. The last cell empties.
const SPONGE_SHARE: f32 = 0.15;
const SPONGE_RATE: f32 = 30.0;
// Turbulence stirs at this share of the source's size, at least four cells.
const TURBULENCE_SOURCE_SHARE: f32 = 0.5;
// Gravity the buoyancy and weight rates are quoted at.
const REFERENCE_GRAVITY: f32 = 9.81;
// Temperature range of a flame in kelvin, at normalised temperature 0 and 1.
const FLAME_KELVIN_COOL: f32 = 1000.0;
const FLAME_KELVIN_HOT: f32 = 2000.0;
// A fine cell holds gas, and keeps its tile in the table, while any of its
// density, temperature, fuel or flame is above this.
const GAS_KEPT: f32 = 1e-3;
// Fine cells to an air cell.
const SUBS: u32 = FINE * FINE * FINE;

// The record of this thread's cell in every pooled field (`list_coord`).
var<private> own: u32;

// Thread `t` of a list dispatch, 512 threads a slot of `slots`: keep its
// record as `own` and its tile as the home, and return its cell in box cells.
// A held tile's cell left of or below the box wraps to a coordinate past it,
// so `inside` and `in_faces` turn it away.
fn list_coord(t: u32) -> vec3<u32> {
    own = ts_list_cell(slots[t >> 9u], t);
    ts_home(own);
    return bitcast<vec3<u32>>(ts_cell_coord(own) - params.first);
}

// The sub-cell of this thread's air cell a fine kernel steps (`fine_coord`).
var<private> own_sub: u32;

// Thread `t` of a fine dispatch, 512 SUBS threads a held slot, sub-cell by
// sub-cell so neighbouring threads read neighbouring records of a plane:
// keep its air cell's record as `own`, its sub-cell as `own_sub` and the
// tile as the home, and return the air cell in box cells, as `list_coord`.
fn fine_coord(t: u32) -> vec3<u32> {
    own = ts_list_cell(slots[t / (512u * SUBS)], t);
    own_sub = (t >> 9u) % SUBS;
    ts_home(own);
    return bitcast<vec3<u32>>(ts_cell_coord(own) - params.first);
}

// This thread's fine cell in the box's fine cells, after `fine_coord`.
fn own_fine_cell(c: vec3<u32>) -> vec3<u32> {
    return c * FINE + sub_offset(own_sub);
}

// One thread: the fine kernels' triple, SUBS times the held one, then the
// pressure's, a workgroup a held slot.
@compute @workgroup_size(1)
fn cs_fine_args() {
    let groups = table_args[params.held_word];
    fine_args[0] = SUBS * groups;
    fine_args[1] = 1u;
    fine_args[2] = 1u;
    fine_args[3] = groups / 2u;
    fine_args[4] = 1u;
    fine_args[5] = 1u;
}

fn inside(c: vec3<u32>) -> bool {
    return c.x < params.dims.x && c.y < params.dims.y && c.z < params.dims.z;
}

// The record of box cell `c`: its pool index, or the canonical record where
// the table holds no tile. A face record lives with its cell.
fn cell_index(c: vec3<u32>) -> u32 {
    return ts_cell_at(params.first + vec3<i32>(c));
}

// Index of the cell nearest `c`, inside the box.
fn clamped_index(c: vec3<i32>) -> u32 {
    let q = clamp(c, vec3<i32>(0), vec3<i32>(params.dims) - vec3<i32>(1));
    return cell_index(vec3<u32>(q));
}

fn cell_centre(c: vec3<u32>) -> vec3<f32> {
    return params.origin + (vec3<f32>(c) + vec3<f32>(0.5)) * params.h;
}

// ---- The fine state ------------------------------------------------------
//
// Fine cell f (in the box's fine cells) is sub-cell f % FINE of air cell
// f / FINE: each fine field holds SUBS planes of the pool's records, sub-cell
// s of record r at element s P' + r (P' the pool's records with the
// canonical one), two words an element: halves (density, temperature) then
// (fuel, flame). Its centre is origin + (f + 1/2) h / FINE. The air's velocity carries the fine state and
// burning happens at its scale, so the flow stretches smoke into sheets and
// flames into fronts thinner than an air cell, which the coarse grid alone
// can only show as blocks. Only the air's flow and pressure, the costly part,
// stay coarse.

fn fine_dims() -> vec3<u32> {
    return params.dims * FINE;
}

fn fine_index(f: vec3<u32>) -> u32 {
    let s = f % vec3<u32>(FINE);
    return (s.x + FINE * (s.y + FINE * s.z)) * ts_pool_len() + cell_index(f / FINE);
}

// Sub-cell `sub`'s offset in fine cells inside its air cell.
fn sub_offset(sub: u32) -> vec3<u32> {
    return vec3<u32>(sub % FINE, (sub / FINE) % FINE, sub / (FINE * FINE));
}

// Sub-cell `sub` of this thread's air cell in every fine field.
fn own_fine(sub: u32) -> u32 {
    return sub * ts_pool_len() + own;
}

// Fine cell coordinates (centres at integers) as air cell coordinates.
fn fine_to_cells(g: vec3<f32>) -> vec3<f32> {
    return (g + vec3<f32>(0.5)) / f32(FINE) - vec3<f32>(0.5);
}

fn fine_in_at(i: u32) -> vec4<f32> {
    return vec4<f32>(unpack2x16float(fine_in[2u * i]), unpack2x16float(fine_in[2u * i + 1u]));
}

fn fine_a_at(i: u32) -> vec4<f32> {
    return vec4<f32>(unpack2x16float(fine_a[2u * i]), unpack2x16float(fine_a[2u * i + 1u]));
}

fn fine_b_at(i: u32) -> vec4<f32> {
    return vec4<f32>(unpack2x16float(fine_b[2u * i]), unpack2x16float(fine_b[2u * i + 1u]));
}

fn store_fine_a(i: u32, s: vec4<f32>) {
    fine_a[2u * i] = pack_half(s.x, s.y);
    fine_a[2u * i + 1u] = pack_half(s.z, s.w);
}

fn store_fine_b(i: u32, s: vec4<f32>) {
    fine_b[2u * i] = pack_half(s.x, s.y);
    fine_b[2u * i + 1u] = pack_half(s.z, s.w);
}

struct Taps {
    index: array<u32, 8>,
    weight: array<f32, 8>,
}

// The eight fine cells around fine grid coordinate `g` (centres at integers,
// clamped to the box) and their weights.
fn fine_taps(g: vec3<f32>) -> Taps {
    let n = fine_dims();
    let q = clamp(g, vec3<f32>(0.0), vec3<f32>(n - vec3<u32>(1u)));
    let base = floor(q);
    let f = q - base;
    let lo = vec3<u32>(base);
    let hi = min(lo + vec3<u32>(1u), n - vec3<u32>(1u));
    var t: Taps;
    for (var k = 0u; k < 8u; k++) {
        let hx = (k & 1u) != 0u;
        let hy = (k & 2u) != 0u;
        let hz = (k & 4u) != 0u;
        let c = vec3<u32>(select(lo.x, hi.x, hx), select(lo.y, hi.y, hy), select(lo.z, hi.z, hz));
        t.index[k] = fine_index(c);
        t.weight[k] = select(1.0 - f.x, f.x, hx) * select(1.0 - f.y, f.y, hy) * select(1.0 - f.z, f.z, hz);
    }
    return t;
}

// ---- Noise ----------------------------------------------------------------

fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

fn lattice_hash(c: vec3<i32>) -> u32 {
    return pcg(pcg(pcg(params.seed ^ bitcast<u32>(c.x)) ^ bitcast<u32>(c.y)) ^ bitcast<u32>(c.z));
}

// A unit vector per lattice point, uniform on the sphere.
fn lattice_gradient(c: vec3<i32>) -> vec3<f32> {
    let h = lattice_hash(c);
    let z = 1.0 - 2.0 * f32(h & 0xffffu) / 65535.0;
    let angle = 6.2831853 * f32(h >> 16u) / 65536.0;
    let r = sqrt(max(1.0 - z * z, 0.0));
    return vec3<f32>(r * cos(angle), r * sin(angle), z);
}

// Gradient (Perlin) noise, within about +-0.87.
fn noise3(q: vec3<f32>) -> f32 {
    let base = floor(q);
    let c = vec3<i32>(base);
    let f = q - base;
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let n000 = dot(lattice_gradient(c), f);
    let n100 = dot(lattice_gradient(c + vec3<i32>(1, 0, 0)), f - vec3<f32>(1.0, 0.0, 0.0));
    let n010 = dot(lattice_gradient(c + vec3<i32>(0, 1, 0)), f - vec3<f32>(0.0, 1.0, 0.0));
    let n110 = dot(lattice_gradient(c + vec3<i32>(1, 1, 0)), f - vec3<f32>(1.0, 1.0, 0.0));
    let n001 = dot(lattice_gradient(c + vec3<i32>(0, 0, 1)), f - vec3<f32>(0.0, 0.0, 1.0));
    let n101 = dot(lattice_gradient(c + vec3<i32>(1, 0, 1)), f - vec3<f32>(1.0, 0.0, 1.0));
    let n011 = dot(lattice_gradient(c + vec3<i32>(0, 1, 1)), f - vec3<f32>(0.0, 1.0, 1.0));
    let n111 = dot(lattice_gradient(c + vec3<i32>(1, 1, 1)), f - vec3<f32>(1.0, 1.0, 1.0));
    return mix(
        mix(mix(n000, n100, u.x), mix(n010, n110, u.x), u.y),
        mix(mix(n001, n101, u.x), mix(n011, n111, u.x), u.y),
        u.z,
    );
}

// Three independent noise potentials, offset along x and y.
fn noise_potential(q: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        noise3(q),
        noise3(q + vec3<f32>(31.4, 0.0, 0.0)),
        noise3(q + vec3<f32>(0.0, 47.2, 0.0)),
    );
}

// The curl of the potentials by central differences: a swirl with no
// divergence, so it stirs the gas without compressing it.
fn curl_noise(q: vec3<f32>) -> vec3<f32> {
    let e = 0.05;
    let dx = noise_potential(q + vec3<f32>(e, 0.0, 0.0)) - noise_potential(q - vec3<f32>(e, 0.0, 0.0));
    let dy = noise_potential(q + vec3<f32>(0.0, e, 0.0)) - noise_potential(q - vec3<f32>(0.0, e, 0.0));
    let dz = noise_potential(q + vec3<f32>(0.0, 0.0, e)) - noise_potential(q - vec3<f32>(0.0, 0.0, e));
    return vec3<f32>(dy.z - dz.y, dz.x - dx.z, dx.y - dy.x) / (2.0 * e);
}

// ---- Faces ------------------------------------------------------------------

// Whether node `c` carries a face normal to `axis`: 0..=n along the axis,
// 0..n-1 across it.
fn has_face(c: vec3<u32>, axis: u32) -> bool {
    var top = params.dims - vec3<u32>(1u);
    top[axis] = params.dims[axis];
    return all(c <= top);
}

fn in_faces(c: vec3<u32>) -> bool {
    return all(c <= params.dims);
}

fn face_index(c: vec3<u32>) -> u32 {
    return cell_index(c);
}

fn unit(axis: u32) -> vec3<u32> {
    var e = vec3<u32>(0u);
    e[axis] = 1u;
    return e;
}

// Grid point of the face normal to `axis` at node `c`, in cell units.
fn face_point(c: vec3<u32>, axis: u32) -> vec3<f32> {
    var g = vec3<f32>(c);
    g[axis] -= 0.5;
    return g;
}

// The eight faces normal to `axis` around grid point `g` (cell units, cell
// centres at integers), clamped to the faces, and their weights.
fn face_taps(g: vec3<f32>, axis: u32) -> Taps {
    var q = g;
    q[axis] += 0.5;
    var top = params.dims - vec3<u32>(1u);
    top[axis] = params.dims[axis];
    q = clamp(q, vec3<f32>(0.0), vec3<f32>(top));
    let base = floor(q);
    let f = q - base;
    let lo = vec3<u32>(base);
    let hi = min(lo + vec3<u32>(1u), top);
    var t: Taps;
    for (var k = 0u; k < 8u; k++) {
        let hx = (k & 1u) != 0u;
        let hy = (k & 2u) != 0u;
        let hz = (k & 4u) != 0u;
        let c = vec3<u32>(select(lo.x, hi.x, hx), select(lo.y, hi.y, hy), select(lo.z, hi.z, hz));
        t.index[k] = face_index(c);
        t.weight[k] = select(1.0 - f.x, f.x, hx) * select(1.0 - f.y, f.y, hy) * select(1.0 - f.z, f.z, hz);
    }
    return t;
}

fn component_in(g: vec3<f32>, axis: u32) -> f32 {
    let t = face_taps(g, axis);
    var sum = 0.0;
    for (var k = 0u; k < 8u; k++) {
        sum += t.weight[k] * vel_in[t.index[k]][axis];
    }
    return sum;
}

fn component_a(g: vec3<f32>, axis: u32) -> f32 {
    let t = face_taps(g, axis);
    var sum = 0.0;
    for (var k = 0u; k < 8u; k++) {
        sum += t.weight[k] * vel_a[t.index[k]][axis];
    }
    return sum;
}

// Whether grid point `g` (cell units) lies past an open face: the air there
// is the still atmosphere. The box spans -1/2 to n - 1/2 on each axis.
fn outside(g: vec3<f32>) -> bool {
    let last = vec3<f32>(params.dims) - vec3<f32>(0.5);
    return g.x < -0.5 || g.z < -0.5 || g.x > last.x || g.y > last.y || g.z > last.z;
}

// Last tick's velocity at the face normal to `axis` at node `c`: its own
// component exactly, each other one the mean of the four faces round it (a
// cell outside the box stands in with the one inside).
fn face_velocity_in(c: vec3<u32>, axis: u32) -> vec3<f32> {
    var v = vec3<f32>(0.0);
    v[axis] = vel_in[face_index(c)][axis];
    var below = c;
    below[axis] = max(c[axis], 1u) - 1u;
    var above = c;
    above[axis] = min(c[axis], params.dims[axis] - 1u);
    for (var other = 0u; other < 3u; other++) {
        if other == axis {
            continue;
        }
        let e = unit(other);
        v[other] = 0.25 * (vel_in[face_index(below)][other] + vel_in[face_index(below + e)][other]
            + vel_in[face_index(above)][other] + vel_in[face_index(above + e)][other]);
    }
    return v;
}

// Velocity at the centre of cell `c`: each component the mean of its two faces.
fn centre_velocity_in(c: vec3<u32>) -> vec3<f32> {
    let own = vel_in[face_index(c)].xyz;
    return 0.5 * (own + vec3<f32>(
        vel_in[face_index(c + unit(0u))].x,
        vel_in[face_index(c + unit(1u))].y,
        vel_in[face_index(c + unit(2u))].z,
    ));
}

fn centre_velocity_a(c: vec3<u32>) -> vec3<f32> {
    let own = vel_a[face_index(c)].xyz;
    return 0.5 * (own + vec3<f32>(
        vel_a[face_index(c + unit(0u))].x,
        vel_a[face_index(c + unit(1u))].y,
        vel_a[face_index(c + unit(2u))].z,
    ));
}

// The velocity `curl` holds at cell centres (cs_centres_in, cs_centres_a)
// at grid point `g` (cell units, centres at integers, clamped to the box):
// eight reads, where each component from its own faces would take 24.
fn centres_at(g: vec3<f32>) -> vec3<f32> {
    let top = vec3<f32>(params.dims) - vec3<f32>(1.0);
    let q = clamp(g, vec3<f32>(0.0), top);
    let base = floor(q);
    let f = q - base;
    let lo = vec3<u32>(base);
    let hi = min(lo + vec3<u32>(1u), params.dims - vec3<u32>(1u));
    var sum = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k++) {
        let hx = (k & 1u) != 0u;
        let hy = (k & 2u) != 0u;
        let hz = (k & 4u) != 0u;
        let c = vec3<u32>(select(lo.x, hi.x, hx), select(lo.y, hi.y, hy), select(lo.z, hi.z, hz));
        let w = select(1.0 - f.x, f.x, hx) * select(1.0 - f.y, f.y, hy) * select(1.0 - f.z, f.z, hz);
        sum += w * curl[cell_index(c)].xyz;
    }
    return sum;
}

// Last tick's velocity and the scene's wind at each cell centre, into
// `curl` (free until cs_restrict) for the fine state's advection.
@compute @workgroup_size(256)
fn cs_centres_in(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !inside(gid) {
        return;
    }
    curl[own] = vec4<f32>(centre_velocity_in(gid) + wind_near(gid), 0.0);
}

// The step's velocity at each cell centre, into `curl` (free once
// cs_confine has run) for the published velocity.
@compute @workgroup_size(256)
fn cs_centres_a(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !inside(gid) {
        return;
    }
    curl[own] = vec4<f32>(centre_velocity_a(gid), 0.0);
}

// ---- Solids -------------------------------------------------------------------

// Cell `i`'s solid: its velocity (xyz) and kind (w, 0 for air).
fn solid_at(i: u32) -> vec4<f32> {
    return vec4<f32>(unpack2x16float(solid[2u * i]), unpack2x16float(solid[2u * i + 1u]));
}

fn is_solid(i: u32) -> bool {
    return SOLIDS && unpack2x16float(solid[2u * i + 1u]).y > 0.0;
}

// Whether the face normal to `axis` at node `c` touches a solid, and the
// solid's velocity across it there (the mean of two solids).
struct SolidFace {
    touches: bool,
    flow: f32,
}

fn solid_face(c: vec3<u32>, axis: u32) -> SolidFace {
    var face = SolidFace(false, 0.0);
    if !SOLIDS {
        return face;
    }
    var sum = 0.0;
    var count = 0.0;
    if c[axis] < params.dims[axis] {
        let s = solid_at(cell_index(c));
        if s.w > 0.0 {
            sum += s[axis];
            count += 1.0;
        }
    }
    if c[axis] > 0u {
        let s = solid_at(cell_index(c - unit(axis)));
        if s.w > 0.0 {
            sum += s[axis];
            count += 1.0;
        }
    }
    face.touches = count > 0.0;
    face.flow = sum / max(count, 1.0);
    return face;
}

// The velocity across the face normal to `axis` at node `c` as the pressure
// solve sees it: a face touching a solid moves with it.
fn face_flow(c: vec3<u32>, axis: u32) -> f32 {
    let face = solid_face(c, axis);
    return select(vel_a[face_index(c)][axis], face.flow, face.touches);
}

// The eight lattice nodes round world point `p` and their trilinear
// weights, clamped to the lattice.
struct Corners {
    node: array<u32, 8>,
    weight: array<f32, 8>,
}

fn lattice_corners(p: vec3<f32>) -> Corners {
    var corners: Corners;
    let n = params.force_nodes;
    let g = (p - params.force_min) / params.force_spacing;
    let base = clamp(floor(g), vec3<f32>(0.0), vec3<f32>(n - vec3<u32>(2u)));
    let f = clamp(g - base, vec3<f32>(0.0), vec3<f32>(1.0));
    let b = vec3<u32>(base);
    for (var k = 0u; k < 8u; k++) {
        let o = vec3<u32>(k & 1u, (k >> 1u) & 1u, k >> 2u);
        let w3 = select(vec3<f32>(1.0) - f, f, o == vec3<u32>(1u));
        let c = b + o;
        corners.node[k] = c.x + n.x * (c.y + n.y * c.z);
        corners.weight[k] = w3.x * w3.y * w3.z;
    }
    return corners;
}

// The scene's forces this tick at the lattice point `corners` reads.
struct Felt {
    push: vec3<f32>,
    drag: f32,
    drag_wind: vec3<f32>,
}

fn felt_at(corners: Corners) -> Felt {
    var felt = Felt(vec3<f32>(0.0), 0.0, vec3<f32>(0.0));
    for (var k = 0u; k < 8u; k++) {
        let node = params.force_base + corners.node[k];
        let w = corners.weight[k];
        let a = forces[2u * node];
        felt.push += w * a.xyz;
        felt.drag += w * a.w;
        felt.drag_wind += w * forces[2u * node + 1u].xyz;
    }
    return felt;
}

// The wind the scene's air moves at near node `c`: the air its forces carry,
// mixed by their drag rates, read once at the node's cell centre for the cell
// and its three faces. Still air without a force.
fn wind_near(c: vec3<u32>) -> vec3<f32> {
    if params.forces_on == 0u {
        return vec3<f32>(0.0);
    }
    let felt = felt_at(lattice_corners(params.origin + (vec3<f32>(c) + vec3<f32>(0.5)) * params.h));
    if felt.drag > 0.0 {
        return felt.drag_wind / felt.drag;
    }
    return vec3<f32>(0.0);
}

// This tick's hits at the lattice point `corners` reads: a velocity change.
fn kick_at(corners: Corners) -> vec3<f32> {
    var kick = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k++) {
        kick += corners.weight[k] * impulses[params.impulse_base + corners.node[k]].xyz;
    }
    return kick;
}

// ---- Advection ------------------------------------------------------------
//
// These kernels run over the face nodes and carry the air; the fine state
// follows in its own kernels below.

// Semi-Lagrangian: each cell and face takes the fields from where its air
// came from. The scene's wind is the air moving: everything is carried by it
// on top of the gas's own velocity, which stays the gas's motion through the
// wind, so a plume keeps its rise and bends over.
@compute @workgroup_size(256)
fn cs_advect(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !in_faces(gid) {
        return;
    }
    let wind = wind_near(gid);
    var v = vec4<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis++) {
        if has_face(gid, axis) {
            let g = face_point(gid, axis);
            let back = g - params.dt * (face_velocity_in(gid, axis) + wind) / params.h;
            // Air that blows in through an open face arrives still, so no
            // wind at a face can feed itself.
            v[axis] = select(component_in(back, axis), 0.0, outside(back));
        }
    }
    vel_a[own] = v;
}

// MacCormack's second look: the advected fields carried forward again.
@compute @workgroup_size(256)
fn cs_advect_back(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !in_faces(gid) {
        return;
    }
    let wind = wind_near(gid);
    var v = vec4<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis++) {
        if has_face(gid, axis) {
            let g = face_point(gid, axis);
            let ahead = g + params.dt * (face_velocity_in(gid, axis) + wind) / params.h;
            v[axis] = select(component_a(ahead, axis), 0.0, outside(ahead));
        }
    }
    vel_b[own] = v;
}

// The error-corrected velocity, each component held between the extremes of
// the eight values the air came from. A thread reads only its own vel_b, so
// it writes the result in place.
@compute @workgroup_size(256)
fn cs_maccormack(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !in_faces(gid) {
        return;
    }
    let wind = wind_near(gid);
    let f = own;
    var v = vec4<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis++) {
        if has_face(gid, axis) {
            let g = face_point(gid, axis);
            let back = g - params.dt * (face_velocity_in(gid, axis) + wind) / params.h;
            if outside(back) {
                continue;
            }
            let t = face_taps(back, axis);
            var lo = vel_in[t.index[0]][axis];
            var hi = lo;
            for (var k = 1u; k < 8u; k++) {
                let value = vel_in[t.index[k]][axis];
                lo = min(lo, value);
                hi = max(hi, value);
            }
            let corrected = vel_a[f][axis] + 0.5 * (vel_in[f][axis] - vel_b[f][axis]);
            v[axis] = clamp(corrected, lo, hi);
        }
    }
    vel_b[f] = v;
}

// ---- Fine state advection ------------------------------------------------------
//
// MacCormack, as the air's, over the fine cells: each takes the state from
// where its air came from, carried by last tick's air and the scene's wind.

// Fine grid point `gid` carried back (`sign` -1) or ahead (+1) one step by
// the air and wind cs_centres_in laid out.
fn fine_carried(gid: vec3<u32>, sign: f32) -> vec3<f32> {
    let g = vec3<f32>(gid);
    return g + sign * params.dt * f32(FINE) * centres_at(fine_to_cells(g)) / params.h;
}

// A thread steps one fine cell. Every held slot's keep starts empty here;
// cs_fine_sources marks the slots that still hold gas.
@compute @workgroup_size(256)
fn cs_fine_advect(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = fine_coord(id.x);
    if (id.x % (512u * SUBS)) == 0u {
        atomicStore(&kept[own >> 9u], WT_NONE);
    }
    if !inside(c) {
        return;
    }
    let t = fine_taps(fine_carried(own_fine_cell(c), -1.0));
    var sum = vec4<f32>(0.0);
    for (var k = 0u; k < 8u; k++) {
        sum += t.weight[k] * fine_in_at(t.index[k]);
    }
    store_fine_a(own_fine(own_sub), sum);
}

@compute @workgroup_size(256)
fn cs_fine_advect_back(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = fine_coord(id.x);
    if !inside(c) {
        return;
    }
    let t = fine_taps(fine_carried(own_fine_cell(c), 1.0));
    var sum = vec4<f32>(0.0);
    for (var k = 0u; k < 8u; k++) {
        sum += t.weight[k] * fine_a_at(t.index[k]);
    }
    store_fine_b(own_fine(own_sub), sum);
}

// The error-corrected state, held between the extremes of the eight values
// the air came from; in place, as the air's.
@compute @workgroup_size(256)
fn cs_fine_maccormack(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = fine_coord(id.x);
    if !inside(c) {
        return;
    }
    let i = own_fine(own_sub);
    let t = fine_taps(fine_carried(own_fine_cell(c), -1.0));
    var lo = fine_in_at(t.index[0]);
    var hi = lo;
    for (var k = 1u; k < 8u; k++) {
        let value = fine_in_at(t.index[k]);
        lo = min(lo, value);
        hi = max(hi, value);
    }
    let corrected = fine_a_at(i) + 0.5 * (fine_in_at(i) - fine_b_at(i));
    store_fine_b(i, clamp(corrected, lo, hi));
}

// ---- Solids (each tick, before advection) -------------------------------------

// Whether the Collider at body row `b` moves this frame (`moving_trees` in
// gas_volume.rs).
fn tree_moves(b: u32) -> bool {
    return ((moving[b >> 5u] >> (b & 31u)) & 1u) != 0u;
}

// The signed distance from `x` to a Collider's mesh in `body`'s pose, read
// through its tree; 1 past the tree's box, where the mesh cannot reach. The
// mesh is about the body's centre of mass, at its scale.
fn tree_phi(body: LiquidBody, shape: LiquidShape, x: vec3<f32>) -> f32 {
    let q = body.rotation;
    let local = liquid_rotate(vec4<f32>(-q.xyz, q.w), x - body.position_inv_mass.xyz);
    let shell = 0.5 * params.h;
    let root = 4u * shape.tree_node;
    if cb_box_distance_squared(local, cb_node(root).xyz, cb_node(root + 1u).xyz) > shell * shell {
        return 1.0;
    }
    return cb_phi(shape.tree_node, shape.tree_triangle, local, 2.0 * params.h, shell);
}

// 1 inside a still Collider's mesh, else 0.
fn static_kind(x: vec3<f32>) -> f32 {
    for (var b = 0u; b < params.solid_count; b++) {
        let body = bodies[params.solid_base + b];
        let shape_index = i32(body.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let shape = shapes[u32(shape_index)];
        if shape.tree != 1u || tree_moves(b) {
            continue;
        }
        if tree_phi(body, shape, x) < 0.0 {
            return 1.0;
        }
    }
    return 0.0;
}

// Every cell of the listed slots air, or inside a still Collider's mesh
// (kind 1, at rest): the meshes are read through their trees, so this runs
// over every held slot only when they or the box change, and otherwise over
// the slots each build hands out. A Collider that moves is laid each tick by
// `cs_solids`.
@compute @workgroup_size(256)
fn cs_solids_static(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !inside(gid) {
        return;
    }
    solid[2u * own] = 0u;
    solid[2u * own + 1u] = pack2x16float(vec2<f32>(0.0, static_kind(cell_centre(gid))));
}

// One thread a cell of the column through the source's centre, whenever the
// still solids change: the box's height in cells less its lowest air cell.
@compute @workgroup_size(64)
fn cs_source_floor(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.dims.y {
        return;
    }
    let cell = vec3<u32>(params.dims.x / 2u, id.x, params.dims.z / 2u);
    if static_kind(cell_centre(cell)) == 0.0 {
        atomicMax(&source_floor[0], params.dims.y - id.x);
    }
}

// The moving solids at this tick's start: a cell inside a body, a Collider's
// hull or a moving Collider's mesh takes the velocity of the one it is
// deepest in (kind 2); a cell inside a still Collider's mesh keeps it; a cell
// the water fills at least half takes the water's velocity, else one at least
// half full of sand the sand's (kind 3); the rest is air.
@compute @workgroup_size(256)
fn cs_solids(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !inside(gid) {
        return;
    }
    let i = own;
    if unpack2x16float(solid[2u * i + 1u]).y == 1.0 {
        return;
    }
    let x = cell_centre(gid);
    var deepest = 0.0;
    var velocity = vec3<f32>(0.0);
    var kind = 0.0;
    for (var b = 0u; b < params.solid_count; b++) {
        let body = bodies[params.solid_base + b];
        let shape_index = i32(body.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let shape = shapes[u32(shape_index)];
        var d = 0.0;
        if shape.tree == 1u {
            if !tree_moves(b) {
                continue;
            }
            d = tree_phi(body, shape, x);
        } else {
            let dims = vec3<u32>(shape.dims_x, shape.dims_y, shape.dims_z);
            let g = liquid_lattice_coord(x, body.position_inv_mass.xyz, body.rotation, shape.origin_spacing, shape.scale_min.xyz);
            // The lattice pads its shape, so a point past it is outside.
            if !liquid_lattice_holds(g, dims) {
                continue;
            }
            d = liquid_shape_distance(shape.atlas_offset, dims, g, shape.origin_spacing.w, shape.scale_min.xyz);
        }
        if d < deepest {
            deepest = d;
            kind = 2.0;
            velocity = liquid_body_velocity(
                body.linear_velocity.xyz,
                body.angular_velocity.xyz,
                body.position_inv_mass.xyz,
                x,
            );
        }
    }
    if kind == 0.0 && params.water_pool > 0u {
        let water = gas_water(x);
        if water.w >= MATTER_FULL {
            kind = 3.0;
            velocity = water.xyz;
        }
    }
    if kind == 0.0 && params.sand_inv_cell > 0.0 {
        let sand = sand_at(x, params.sand_inv_cell);
        if sand.fraction >= MATTER_FULL {
            kind = 3.0;
            velocity = sand.velocity;
        }
    }
    solid[2u * i] = pack2x16float(velocity.xy);
    solid[2u * i + 1u] = pack2x16float(vec2<f32>(velocity.z, kind));
}

// ---- Sources, burning and forces (on R) -------------------------------------

// How far the source sits above the box's floor: on the static solids under
// its centre, a box floor sunk in the ground lifting it to the ground's top.
fn source_lift() -> f32 {
    if !SOLIDS {
        return 0.0;
    }
    return f32(params.dims.y - min(atomicLoad(&source_floor[0]), params.dims.y)) * params.h;
}

// The source: a squashed ellipsoid on the floor's centre, its centre and
// radii in metres.
fn source_centre() -> vec3<f32> {
    let box_centre_xz = params.origin.xz + 0.5 * vec2<f32>(params.dims.xz) * params.h;
    return vec3<f32>(
        box_centre_xz.x,
        params.origin.y + source_lift() + max(2.0 * params.h, 0.15 * params.source_size),
        box_centre_xz.y,
    );
}

fn source_radii() -> vec3<f32> {
    return vec3<f32>(
        0.5 * params.source_size,
        max(1.5 * params.h, 0.3 * params.source_size),
        0.5 * params.source_size,
    );
}

// How strongly the source emits at world point `pos`: the ellipsoid broken
// up by noise that drifts along with time.
fn source_weight(pos: vec3<f32>) -> f32 {
    let falloff = 1.0 - smoothstep(0.6, 1.0, length((pos - source_centre()) / source_radii()));
    if falloff <= 0.0 {
        return 0.0;
    }
    let breakup = clamp(
        0.6 + 0.8 * noise3(pos * 2.0 / params.source_size + vec3<f32>(0.0, -2.0 * params.time, 0.0)),
        0.0,
        1.4,
    );
    return falloff * breakup;
}

// One thread a point: a cell in each tile the source's box (a cell wider
// than the ellipsoid, for the inflow on its faces) touches, `source_tiles`
// along each axis, 8 cells (a tile) apart and clamped to the box's far
// corner, so the build always holds the source's tiles.
@compute @workgroup_size(64)
fn cs_source_points(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.source_count {
        return;
    }
    let n = params.source_tiles;
    let k = vec3<u32>(id.x % n.x, (id.x / n.x) % n.y, id.x / (n.x * n.y));
    let top = vec3<i32>(params.dims) - vec3<i32>(1);
    let centre = (source_centre() - params.origin) / params.h;
    let reach = source_radii() / params.h;
    let lo = clamp(vec3<i32>(floor(centre - reach)) - vec3<i32>(1), vec3<i32>(0), top);
    let hi = clamp(vec3<i32>(floor(centre + reach)) + vec3<i32>(1), vec3<i32>(0), top);
    source_points[id.x] = vec4<i32>(params.first + min(lo + vec3<i32>(k) * 8, hi), 0);
}

// Every record of the slots this tick's build handed out, zeroed in the
// fields that carry from tick to tick, so a slot one tile left starts clean
// for the next. Bound with last tick's result as vel_a and fine_a.
@compute @workgroup_size(256)
fn cs_fresh(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = ts_list_cell(slots[id.x >> 9u], id.x);
    vel_a[i] = vec4<f32>(0.0);
    for (var sub = 0u; sub < SUBS; sub++) {
        store_fine_a(sub * ts_pool_len() + i, vec4<f32>(0.0));
    }
    pressure[i] = 0.0;
    if SOLIDS {
        solid[2u * i] = 0u;
        solid[2u * i + 1u] = 0u;
    }
}

// Every slot keeps nothing: after a clear, the next build holds only the
// source's tiles.
@compute @workgroup_size(256)
fn cs_reset_kept(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x < params.pool_tiles {
        atomicStore(&kept[id.x], WT_NONE);
    }
}

// How deep grid point `g` (cell units) sits in the sponge band, 0 outside
// it to 1 at the outermost cells, squared so it starts gently.
fn sponge(g: vec3<f32>) -> f32 {
    let last = vec3<f32>(params.dims) - vec3<f32>(1.0);
    let to_face = vec3<f32>(min(g.x, last.x - g.x), last.y - g.y, min(g.z, last.z - g.z));
    let band = max(vec3<f32>(2.0), SPONGE_SHARE * vec3<f32>(params.dims));
    let depth = saturate(vec3<f32>(1.0) - to_face / band);
    let deepest = max(depth.x, max(depth.y, depth.z));
    return deepest * deepest;
}

// Each fine cell's emission, burning, decay and sponge, on the working fine
// state. An air cell still holding gas keeps its tile for the next build.
@compute @workgroup_size(256)
fn cs_fine_sources(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = fine_coord(id.x);
    if !inside(c) {
        return;
    }
    let i = own_fine(own_sub);
    // No gas inside a solid.
    if is_solid(own) {
        store_fine_a(i, vec4<f32>(0.0));
        return;
    }
    let s = fine_source(own_fine_cell(c), i);
    store_fine_a(i, s);
    if any(s > vec4<f32>(GAS_KEPT)) {
        atomicMin(&kept[own >> 9u], own & 511u);
    }
}

// Fine cell `gid`'s state `i` after this step's emission, burning, decay and
// sponge.
fn fine_source(gid: vec3<u32>, i: u32) -> vec4<f32> {
    let g = vec3<f32>(gid);
    let pos = params.origin + (g + vec3<f32>(0.5)) * (params.h / f32(FINE));
    var s = fine_a_at(i);

    let w = source_weight(pos);
    if params.kind == 0u {
        s.x += params.rate * w * params.dt;
        s.y = max(s.y, params.source_heat * w);
    } else {
        s.z += params.rate * w * params.dt;
        s.y = max(s.y, (params.ignition + 0.3) * step(0.05, w));
    }

    // Fire burns its fuel where it is hot enough, heating and smoking.
    var flame = s.w * exp(-params.dt / FLAME_SECONDS);
    if params.kind == 1u && s.y > params.ignition {
        let burn = s.z * (1.0 - exp(-params.burn_rate * params.dt));
        s.z -= burn;
        s.y = min(s.y + burn * params.heat_release, MAX_TEMPERATURE);
        s.x += burn * params.soot;
        flame = max(flame, clamp(burn / max(params.dt * params.burn_rate, 1e-6), 0.0, 1.0));
    }
    s.w = flame;

    s.x *= exp(-params.dissipation * params.dt);
    s.y *= exp(-params.cooling * params.dt);
    s.z *= exp(-FUEL_DECAY * params.dt);

    // What reaches the sides or the top leaves through the sponge band.
    s *= exp(-SPONGE_RATE * sponge(fine_to_cells(g)) * params.dt);
    let n = fine_dims();
    let to_face = min(min(gid.x, n.x - 1u - gid.x), min(gid.z, n.z - 1u - gid.z));
    if min(to_face, n.y - 1u - gid.y) == 0u {
        s = vec4<f32>(0.0);
    }

    s = max(s, vec4<f32>(0.0));
    s.y = min(s.y, MAX_TEMPERATURE);
    return s;
}

// Each air cell's gas, the mean of its fine cells, for buoyancy, the flames'
// expansion and their light. The turbulence's push for this step goes to
// `curl` (free until cs_curl) for cs_forces to spread onto the faces.
@compute @workgroup_size(256)
fn cs_restrict(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !inside(gid) {
        return;
    }
    let i = own;
    var s = vec4<f32>(0.0);
    for (var sub = 0u; sub < SUBS; sub++) {
        s += fine_a_at(own_fine(sub));
    }
    s /= f32(SUBS);
    state_a[i] = s;

    // Noise stirs only where there is gas.
    var push = vec3<f32>(0.0);
    if params.turbulence > 0.0 {
        let gas = saturate(s.x + s.y + s.z);
        if gas > 0.0 {
            let scale = max(4.0 * params.h, TURBULENCE_SOURCE_SHARE * params.source_size);
            let q = cell_centre(gid) / scale + vec3<f32>(0.0, -0.5 * params.time, 0.0);
            push = params.turbulence * gas * curl_noise(q);
        }
    }
    curl[i] = vec4<f32>(push, 0.0);
}

// The cell below node `c` along `axis` and the cell above it, each clamped
// into the box: a face on the box's side has one cell, which stands in for
// the missing one.
fn cell_below(c: vec3<u32>, axis: u32) -> u32 {
    var q = vec3<i32>(c);
    q[axis] -= 1;
    return clamped_index(q);
}

fn cell_above(c: vec3<u32>) -> u32 {
    return clamped_index(vec3<i32>(c));
}

// Forces on the faces: hot air rises and smoke sinks along -gravity, the
// turbulence's push, the scene's pushes (its wind carries the air in the
// advection), a hit's velocity change, and the source blowing
// along `up` at least as fast as its inflow speed. No face moves more than four cells a step, so the next
// advection stays in reach; nothing flows through the floor.
@compute @workgroup_size(256)
fn cs_forces(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !in_faces(gid) {
        return;
    }
    let f = own;
    var v = vel_a[f];
    let limit = 4.0 * params.h / max(params.dt, 1e-4);
    let lift = params.g / REFERENCE_GRAVITY;
    for (var axis = 0u; axis < 3u; axis++) {
        if !has_face(gid, axis) {
            continue;
        }
        if axis == 1u && gid.y == 0u {
            v.y = 0.0;
            continue;
        }
        let lower = cell_below(gid, axis);
        let upper = cell_above(gid);
        let s = 0.5 * (state_a[lower] + state_a[upper]);
        let push = 0.5 * (curl[lower][axis] + curl[upper][axis]);
        var u = v[axis] + params.dt * (
            params.up[axis] * lift * (params.buoyancy * s.y - params.weight * s.x) + push
        );
        if params.forces_on != 0u || params.impulse_on != 0u {
            let corners = lattice_corners(params.origin + (face_point(gid, axis) + vec3<f32>(0.5)) * params.h);
            if params.forces_on != 0u {
                u += params.dt * felt_at(corners).push[axis];
            }
            if params.impulse_on != 0u {
                u += kick_at(corners)[axis];
            }
        }
        if params.up[axis] != 0.0 {
            let w = source_weight(params.origin + (face_point(gid, axis) + vec3<f32>(0.5)) * params.h);
            if w > 0.0 {
                let target_speed = params.inflow * w * params.up[axis];
                u = select(min(u, target_speed), max(u, target_speed), params.up[axis] > 0.0);
            }
        }
        v[axis] = clamp(u, -limit, limit);
    }
    vel_a[f] = v;
}

// ---- Vorticity (on R) -------------------------------------------------------

fn centre_velocity_clamped(c: vec3<i32>) -> vec3<f32> {
    let q = clamp(c, vec3<i32>(0), vec3<i32>(params.dims) - vec3<i32>(1));
    return centre_velocity_a(vec3<u32>(q));
}

// d(velocity)/d(axis) at a cell centre by central differences; one-sided at
// the box faces.
fn velocity_derivative(c: vec3<i32>, axis: u32) -> vec3<f32> {
    let lo = max(c[axis] - 1, 0);
    let hi = min(c[axis] + 1, i32(params.dims[axis]) - 1);
    var a = c;
    a[axis] = lo;
    var b = c;
    b[axis] = hi;
    return (centre_velocity_clamped(b) - centre_velocity_clamped(a)) / (f32(hi - lo) * params.h);
}

// d(|vorticity|)/d(axis), the same way.
fn curl_length_derivative(c: vec3<i32>, axis: u32) -> f32 {
    let lo = max(c[axis] - 1, 0);
    let hi = min(c[axis] + 1, i32(params.dims[axis]) - 1);
    var a = c;
    a[axis] = lo;
    var b = c;
    b[axis] = hi;
    return (curl[clamped_index(b)].w - curl[clamped_index(a)].w) / (f32(hi - lo) * params.h);
}

@compute @workgroup_size(256)
fn cs_curl(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !inside(gid) {
        return;
    }
    let c = vec3<i32>(gid);
    let dx = velocity_derivative(c, 0u);
    let dy = velocity_derivative(c, 1u);
    let dz = velocity_derivative(c, 2u);
    let omega = vec3<f32>(dy.z - dz.y, dz.x - dx.z, dx.y - dy.x);
    curl[own] = vec4<f32>(omega, length(omega));
}

// Vorticity confinement (Fedkiw) at cell `c`: a push around the vortex cores
// that puts back the swirl advection smooths away; none outside the box.
// `vorticity` is the dimensionless strength; the cell size makes the push
// shrink with the cells.
fn confinement(c: vec3<i32>) -> vec3<f32> {
    if any(c < vec3<i32>(0)) || any(c >= vec3<i32>(params.dims)) {
        return vec3<f32>(0.0);
    }
    let grad = vec3<f32>(
        curl_length_derivative(c, 0u),
        curl_length_derivative(c, 1u),
        curl_length_derivative(c, 2u),
    );
    let len = length(grad);
    if len < 1e-6 {
        return vec3<f32>(0.0);
    }
    return params.vorticity * params.h * cross(grad / len, curl[clamped_index(c)].xyz);
}

// Each face takes the mean of its two cells' confinement; a face on the box's
// side takes its one cell's.
@compute @workgroup_size(256)
fn cs_confine(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !in_faces(gid) || params.vorticity <= 0.0 {
        return;
    }
    let f = own;
    let c = vec3<i32>(gid);
    let own = confinement(c);
    var v = vel_a[f];
    for (var axis = 0u; axis < 3u; axis++) {
        if !has_face(gid, axis) || (axis == 1u && gid.y == 0u) {
            continue;
        }
        var below = c;
        below[axis] -= 1;
        let lower = confinement(below)[axis];
        var share = 0.5 * (own[axis] + lower);
        if gid[axis] == 0u {
            share = own[axis];
        } else if gid[axis] == params.dims[axis] {
            share = lower;
        }
        v[axis] += params.dt * share;
    }
    vel_a[f] = v;
}

// ---- Pressure projection (on R) ---------------------------------------------

// Whether the cell at `c`, which may lie one past the box, is a solid.
fn solid_neighbour(c: vec3<i32>) -> bool {
    let dims = vec3<i32>(params.dims);
    if any(c < vec3<i32>(0)) || any(c >= dims) {
        return false;
    }
    return is_solid(cell_index(vec3<u32>(c)));
}

// The flow out of each cell through its six faces, less the expansion the
// flames ask for. The floor's faces are closed, and a face touching a solid
// moves with it. With solids, the count of open neighbours rides in the low
// bits: a solid cell is not solved and holds pressure 0.
@compute @workgroup_size(256)
fn cs_divergence(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !inside(gid) {
        return;
    }
    let i = own;
    if is_solid(i) {
        divergence[i] = 0.0;
        pressure[i] = 0.0;
        return;
    }
    let floor_v = select(face_flow(gid, 1u), 0.0, gid.y == 0u);
    let out = vec3<f32>(
        face_flow(gid + unit(0u), 0u) - face_flow(gid, 0u),
        face_flow(gid + unit(1u), 1u) - floor_v,
        face_flow(gid + unit(2u), 2u) - face_flow(gid, 2u),
    );
    let flow = (out.x + out.y + out.z) / params.h - params.expansion * state_a[i].w;
    if !SOLIDS {
        divergence[i] = flow;
        return;
    }
    let c = vec3<i32>(gid);
    var open = 6u;
    for (var axis = 0u; axis < 3u; axis++) {
        var e = vec3<i32>(0);
        e[axis] = 1;
        open -= select(0u, 1u, solid_neighbour(c + e)) + select(0u, 1u, solid_neighbour(c - e));
    }
    // A cell sealed in by solids keeps 0: it has no pressure to find.
    divergence[i] = bitcast<f32>((bitcast<u32>(flow) & ~7u) | open);
}

// ---- Pressure, a tile at a time ----------------------------------------------
//
// Red-black Gauss-Seidel with over-relaxation, TILE_PAIRS pairs of sweeps
// each time a held tile is solved: one workgroup a slot, a thread a cell, the
// tile and a cell round it in workgroup memory. Tiles take turns by the
// parity of their world tile (x + y + z): a dispatch solves one parity in
// place in `pressure`, and the ring it reads lies in the six face neighbours,
// the other parity, which nothing writes during it. Every cell update so reads
// its neighbours' latest values, which keeps the solve plain point SOR in
// another order, sure to settle; solving neighbouring tiles at once would
// step a tile's faces against a stale ring, an over-relaxed Jacobi step that
// can grow beside a solid. Past the box: the floor has no pressure gradient
// through it (the cell below the floor reads the cell's own pressure), an
// open face and a tile the table does not hold are atmosphere (0). A solid
// holds 0 and is left out of its neighbours' count (cs_divergence), so no
// gradient runs through it either.

// The tile and its ring of one cell, x fastest. Only the ring's face cells
// are read; its edges and corners belong to same-parity tiles.
const RING_SIDE: u32 = 10u;
var<workgroup> tile_pressure: array<f32, 1000>;
var<workgroup> tile_parity: u32;

fn pressure_tile(slot_index: u32, lid: u32, parity: u32) {
    own = ts_list_cell(slots[slot_index], lid);
    ts_home(own);
    // Every thread holds the same tile: the uniform load lets the workgroup
    // leave together.
    if lid == 0u {
        let t = ts_home_tile;
        tile_parity = u32(t.x + t.y + t.z) & 1u;
    }
    if workgroupUniformLoad(&tile_parity) != parity {
        return;
    }
    let dims = vec3<i32>(params.dims);
    // The ring's first cell in box cells.
    let corner = ts_home_tile * 8 - params.first - vec3<i32>(1);
    for (var k = lid; k < RING_SIDE * RING_SIDE * RING_SIDE; k += 512u) {
        let q = corner + vec3<i32>(i32(k % RING_SIDE), i32((k / RING_SIDE) % RING_SIDE), i32(k / (RING_SIDE * RING_SIDE)));
        var value = 0.0;
        if all(q >= vec3<i32>(0)) && all(q < dims) {
            value = pressure[cell_index(vec3<u32>(q))];
        }
        tile_pressure[k] = value;
    }
    let local = vec3<u32>(ts_local_coord(lid));
    let c = corner + vec3<i32>(local) + vec3<i32>(1);
    let centre = (local.x + 1u) + RING_SIDE * ((local.y + 1u) + RING_SIDE * (local.z + 1u));
    let in_box = all(c >= vec3<i32>(0)) && all(c < dims);
    var flow = 0.0;
    var open = 0u;
    if in_box {
        let d = divergence[own];
        if SOLIDS {
            let bits = bitcast<u32>(d);
            flow = bitcast<f32>(bits & ~7u);
            open = bits & 7u;
        } else {
            flow = d;
            open = 6u;
        }
    }
    let colour = u32(c.x + c.y + c.z) & 1u;
    let h2 = params.h * params.h;
    workgroupBarrier();
    for (var pair = 0u; pair < TILE_PAIRS; pair++) {
        for (var sweep = 0u; sweep < 2u; sweep++) {
            if open != 0u && colour == sweep {
                let own_p = tile_pressure[centre];
                let below = select(tile_pressure[centre - RING_SIDE], own_p, c.y == 0);
                let sum = tile_pressure[centre + 1u] + tile_pressure[centre - 1u]
                    + tile_pressure[centre + RING_SIDE] + below
                    + tile_pressure[centre + RING_SIDE * RING_SIDE]
                    + tile_pressure[centre - RING_SIDE * RING_SIDE];
                tile_pressure[centre] = mix(own_p, (sum - h2 * flow) / f32(open), SOR_OMEGA);
            }
            workgroupBarrier();
        }
    }
    if in_box {
        pressure[own] = tile_pressure[centre];
    }
}

@compute @workgroup_size(512)
fn cs_pressure_even(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lid: u32,
) {
    pressure_tile(group.x, lid, 0u);
}

@compute @workgroup_size(512)
fn cs_pressure_odd(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lid: u32,
) {
    pressure_tile(group.x, lid, 1u);
}

// Each face loses the pressure difference across it, the open faces against
// the atmosphere's 0; nothing flows through the floor, and a face touching a
// solid moves with it.
@compute @workgroup_size(256)
fn cs_project(@builtin(global_invocation_id) id: vec3<u32>) {
    let gid = list_coord(id.x);
    if !in_faces(gid) {
        return;
    }
    let f = own;
    let c = vec3<i32>(gid);
    let dims = vec3<i32>(params.dims);
    var v = vel_a[f];
    for (var axis = 0u; axis < 3u; axis++) {
        if !has_face(gid, axis) {
            continue;
        }
        if axis == 1u && gid.y == 0u {
            v.y = 0.0;
            continue;
        }
        let face = solid_face(gid, axis);
        if face.touches {
            v[axis] = face.flow;
            continue;
        }
        var below = c;
        below[axis] -= 1;
        let upper = select(0.0, pressure[clamped_index(c)], c[axis] < dims[axis]);
        let lower = select(0.0, pressure[clamped_index(below)], below[axis] >= 0);
        v[axis] -= (upper - lower) / params.h;
    }
    vel_a[f] = v;
}

// ---- Solid fill (on R, with solids) --------------------------------------------

// Each fine cell in a solid takes the mean gas of its open neighbours, so the
// next advection reads the face's own gas at a face, not the solid's zero
// (which left a clear layer against every face and striped it). Only solid
// cells are written and only open ones read, so the pass is race free; the
// renderer stops at the solid's surface, so the fill never shows.
@compute @workgroup_size(256)
fn cs_fill_solids(@builtin(global_invocation_id) id: vec3<u32>) {
    let cell = fine_coord(id.x);
    if !inside(cell) || !is_solid(own) {
        return;
    }
    let n = vec3<i32>(fine_dims());
    let c = vec3<i32>(own_fine_cell(cell));
    var sum = vec4<f32>(0.0);
    var count = 0.0;
    for (var axis = 0u; axis < 3u; axis++) {
        for (var side = -1; side <= 1; side += 2) {
            var q = c;
            q[axis] += side;
            if any(q < vec3<i32>(0)) || any(q >= n) {
                continue;
            }
            let neighbour = vec3<u32>(q);
            if !is_solid(cell_index(neighbour / FINE)) {
                sum += fine_a_at(fine_index(neighbour));
                count += 1.0;
            }
        }
    }
    store_fine_a(own_fine(own_sub), sum / max(count, 1.0));
}

// ---- Publishing (on R) --------------------------------------------------------

fn pack_half(a: f32, b: f32) -> u32 {
    return pack2x16float(clamp(vec2<f32>(a, b), vec2<f32>(-65504.0), vec2<f32>(65504.0)));
}

// The radiator's luminance relative to 1500 K's at `kelvin`, from the table.
fn blackbody_luminance(kelvin: f32) -> f32 {
    let span = BLACKBODY_MAX_KELVIN - BLACKBODY_MIN_KELVIN;
    let position = clamp((kelvin - BLACKBODY_MIN_KELVIN) / span, 0.0, 1.0) * f32(BLACKBODY_ENTRIES - 1u);
    let lower = min(u32(floor(position)), BLACKBODY_ENTRIES - 2u);
    return mix(blackbody[lower].w, blackbody[lower + 1u].w, position - f32(lower));
}

// A non-negative sum as fixed point; the sums wrap past 2^32 / LIGHT_FIXED_SCALE.
fn light_fixed(value: f32) -> u32 {
    return u32(clamp(round(value * LIGHT_FIXED_SCALE), 0.0, 4.0e9));
}

var<workgroup> reduce_power: array<f32, 256>;
var<workgroup> reduce_x: array<f32, 256>;
var<workgroup> reduce_y: array<f32, 256>;
var<workgroup> reduce_z: array<f32, 256>;
var<workgroup> reduce_heat: array<f32, 256>;

@compute @workgroup_size(1)
fn cs_clear_sums() {
    for (var k = 0u; k < LIGHT_WORDS; k++) {
        atomicStore(&light_sums[k], 0u);
    }
}

// Adds the flames' light to light_sums, from each air cell's mean:
// word 0 the emission e = flame * luminance * h^3, words 1 to 3 e times the
// cell centre relative to the box origin, word 4 e times the temperature.
// Each workgroup reduces its cells, then adds once.
@compute @workgroup_size(256)
fn cs_publish(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_index) lid: u32,
) {
    let gid = list_coord(id.x);
    var power = 0.0;
    var moment = vec3<f32>(0.0);
    var heat = 0.0;
    if inside(gid) {
        let s = state_a[own];
        if s.w > 0.0 {
            let kelvin = mix(FLAME_KELVIN_COOL, FLAME_KELVIN_HOT, s.y);
            power = s.w * blackbody_luminance(kelvin) * params.h * params.h * params.h;
            moment = power * (vec3<f32>(gid) + vec3<f32>(0.5)) * params.h;
            heat = power * s.y;
        }
    }
    reduce_power[lid] = power;
    reduce_x[lid] = moment.x;
    reduce_y[lid] = moment.y;
    reduce_z[lid] = moment.z;
    reduce_heat[lid] = heat;
    workgroupBarrier();
    for (var stride = 128u; stride > 0u; stride = stride >> 1u) {
        if lid < stride {
            reduce_power[lid] += reduce_power[lid + stride];
            reduce_x[lid] += reduce_x[lid + stride];
            reduce_y[lid] += reduce_y[lid + stride];
            reduce_z[lid] += reduce_z[lid + stride];
            reduce_heat[lid] += reduce_heat[lid + stride];
        }
        workgroupBarrier();
    }
    if lid == 0u && reduce_power[0] > 0.0 {
        atomicAdd(&light_sums[0], light_fixed(reduce_power[0]));
        atomicAdd(&light_sums[1], light_fixed(reduce_x[0]));
        atomicAdd(&light_sums[2], light_fixed(reduce_y[0]));
        atomicAdd(&light_sums[3], light_fixed(reduce_z[0]));
        atomicAdd(&light_sums[4], light_fixed(reduce_heat[0]));
    }
}

// The air's velocity at each fine cell's centre (from cs_centres_a) and the
// fine state the tick ended with, published in one layout: the box's fine
// cells, element f.x + fx (f.y + fy f.z), which the host cleared, so a cell
// of a tile the table does not hold publishes no gas and still air.
@compute @workgroup_size(256)
fn cs_publish_velocity(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = fine_coord(id.x);
    if !inside(c) {
        return;
    }
    let n = fine_dims();
    let f = own_fine_cell(c);
    let d = f.x + n.x * (f.y + n.y * f.z);
    let i = own_fine(own_sub);
    let v = centres_at(fine_to_cells(vec3<f32>(f)));
    velocity_out[2u * d] = pack_half(v.x, v.y);
    velocity_out[2u * d + 1u] = pack_half(v.z, 0.0);
    state_out[2u * d] = fine_a[2u * i];
    state_out[2u * d + 1u] = fine_a[2u * i + 1u];
}

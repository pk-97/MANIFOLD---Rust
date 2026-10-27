// GPU FLIP's water step: every pass of one step but the sort, the world
// tiles build and the pressure solve, each a separate entry point. Included
// after liquid_pose.wgsl, liquid_collider.wgsl, force_lattice.wgsl,
// tile_grid.wgsl and the world tiles read side (world_tiles_read.wgsl).
// FLIP Fluids attribution below covers the explicitly named baseline
// transfers, boundaries and source operations.
//
// Cells are world cells: cell p (any sign) is centred on p·h, counted from
// the lattice origin −h/2, so (x − origin) / h is a position in cells with
// cell p spanning [p, p + 1). Every cell and face field is pooled per world
// tile (world_tiles_read.wgsl): reached through `ts_cell_at` / `ts_face`,
// the canonical element where no tile is stored. A record holds the low x,
// y and z faces of its cell; face a of record p sits at p on axis a and
// p + ½ on the other two. The solid corner field is pooled the same way:
// corner p, at origin + p·h, is stored with cell p.
//
// The tank is a box of world cells, [box_lo(), box_lo() + box_cells()), from
// the walls_lo/walls_hi box in metres. Its walls stay where they were: the
// box's edge faces are closed, the inset walls are in the solid, and a face
// of record p exists when p lies in the box's record range (`face_exists`).
// The list passes run over held slots, 512 threads a slot, and touch only
// the box's cells and records; the rest of a held tile keeps the canonical
// values a fresh tile starts with (`tiles_fresh`). Each list thread sets its
// cell's tile as its home (`ts_home`), so neighbour reads cost no hash.
//
// The CPU sizes every buffer before it dispatches; each pass only checks its
// thread is inside its range.
//
// The density projection (density_source) is ported from blub (MIT,
// Copyright (c) 2020 Andreas Reich; see THIRD_PARTY_NOTICES.md):
// density_projection_gather_error.comp, its kernel, solid face weight 0.5625,
// surface clamp and source clamp.
//
// Ported from FLIP Fluids (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis
// Fassbaender; see THIRD_PARTY_NOTICES.md): velocityadvector.cpp (particles
// to faces), particlelevelset.cpp (the particle distance),
// levelsetutils.cpp and meshlevelset.cpp (the solid open fractions),
// fluidsimulation.cpp (the solids' face velocity, the constraint, and the
// particles' solid collision and removal), interpolation.cpp (the solid
// distance's gradient) and pressuresolver.cpp (divergence, the pressure
// subtraction and the sealed pockets' solid velocity). Sources and drains
// follow fluidsimulation.cpp: _updateInflowMeshFluidSource (8813-8874) and
// _addNewFluidCells (8566-8603, 8771-8811) for emission, the outflow removal
// in _updateMeshFluidSources (9011-9031), _constrainMarkerParticleVelocities
// (7397-7451) and _getInflowConstrainedVelocityComponents (6112) for the
// constrained velocity.

struct Params {
    // The tank box's low corner in metres: the solver grid's minimum, the
    // force and impulse lattices' origin.
    walls_lo: vec3<f32>,
    // Slots of `sorted`.
    capacity: u32,
    // The box's high corner in metres, walls_lo + cells · h.
    walls_hi: vec3<f32>,
    cell_size: f32,
    gravity: vec3<f32>,
    step_dt: f32,
    field_nodes: vec3<u32>,
    field_spacing: f32,
    // The lattice origin (−h/2 on every axis) and 1 / cell_size as the host
    // computes it, the sort's `inv_cell`: every pass bins a particle to the
    // same world cell (`ts_world_cell`).
    origin: vec3<f32>,
    inv_cell: f32,
    tick_index: i32,
    step_in_tick: i32,
    force_lattices: i32,
    impulse_tick: i32,
    first_tick: i32,
    body_count: i32,
    // Body rows, at most what `bodies` holds.
    rows: i32,
    tick_seconds: f32,
    // The FLIP share for this step.
    flip: f32,
    // subtract: 1 reads `phi` for the free surface, 0 keeps air at zero.
    ghost: u32,
    // Particle slots the pool holds: the births' bound.
    particles: u32,
    // Records `shapes` holds.
    shapes_len: u32,
    // The density projection's source scale, 1 / step_dt.
    rate: f32,
    // The tank's closed faces: bit 2d the low face of axis d, bit 2d + 1 the
    // high one.
    closed_faces: u32,
    // Inflow and outflow regions a tick, and the region rows `regions` holds
    // (tick major from first_tick).
    region_count: i32,
    region_rows: i32,
    // Half-width of an emitted particle's jitter in cells, a quarter of the
    // jitter factor (_getMarkerParticleJitter).
    emit_jitter: f32,
    // The V-cycle level the pressure solves run on; the pocket coarsening
    // reads it.
    solve_level: u32,
    // Dispatches in each velocity-extension replay segment.
    extension_layers: u32,
    // The farthest ring the table holds (world_tiles.rs Reach): tile_counts
    // word ring_max counts every held slot.
    ring_max: u32,
    live_impulse_stride: u32,
    // Cells from the grid's minimum to each wall (FlipSolverGrid::wall_inset);
    // 0 when the walls are the grid's edges.
    wall_inset: f32,
    // Box3D's substep over this tick, dt / box3d_substep_count(dt): the h in
    // the coupled motion law (liquid_body_state).
    coupled_h: f32,
    // Tiles the pool holds (P) and the table's entries (H).
    pool_tiles: u32,
    table_entries: u32,
    // Half-cell sites the inflows of one tick enumerate at most: the
    // emission passes' thread count (`region_sites`).
    emit_sites: u32,
    _pad0: u32,
    _pad1: u32,
};

struct CellRange {
    start: u32,
    count: u32,
};

struct FluidParticle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
};

struct FaceSample {
    face_velocity: vec4<f32>,
    face_weight: vec4<f32>,
};

struct LiquidBody {
    position_inv_mass: vec4<f32>,
    rotation: vec4<f32>,
    linear_velocity: vec4<f32>,
    angular_velocity: vec4<f32>,
    inv_inertia_x: vec4<f32>,
    inv_inertia_y: vec4<f32>,
    inv_inertia_z: vec4<f32>,
    accel_shape: vec4<f32>,
};

struct LiquidShape {
    origin_spacing: vec4<f32>,
    dims_x: u32,
    dims_y: u32,
    dims_z: u32,
    atlas_offset: u32,
    scale_min: vec4<f32>,
};

struct ClockPlan {
    step_dt: f32,
    elapsed: f32,
    remaining: f32,
    maximum_speed: f32,
    cap_hit: u32,
    nonfinite: u32,
    step_index: u32,
    event: u32,
    numerical_end: f32,
    marker_limit: f32,
    _pad0: u32,
    live_mode: u32,
};

@group(0) @binding(0) var<uniform> u: Params;

@group(0) @binding(1) var<storage, read> ranges: array<CellRange>;

@group(0) @binding(2) var<storage, read> sorted: array<FluidParticle>;

@group(0) @binding(3) var<storage, read> faces_in: array<FaceSample>;

@group(0) @binding(4) var<storage, read_write> faces_out: array<FaceSample>;

@group(0) @binding(5) var<storage, read_write> cell_out: array<f32>;

@group(0) @binding(6) var<storage, read> water: array<f32>;

@group(0) @binding(7) var<storage, read> phi: array<f32>;

@group(0) @binding(8) var<storage, read> pressure: array<f32>;

@group(0) @binding(9) var<storage, read> solid: array<f32>;

@group(0) @binding(10) var<storage, read> solid_faces: array<FaceSample>;

@group(0) @binding(11) var<storage, read> solid_velocity: array<FaceSample>;

@group(0) @binding(12) var<storage, read> forces: array<f32>;

@group(0) @binding(13) var<storage, read> impulses: array<f32>;

@group(0) @binding(14) var<storage, read> bodies: array<LiquidBody>;

@group(0) @binding(15) var<storage, read> shapes: array<LiquidShape>;

@group(0) @binding(16) var<storage, read> atlas: array<u32>;

@group(0) @binding(17) var<storage, read> old: array<FaceSample>;

@group(0) @binding(18) var<storage, read> spread: array<FaceSample>;

@group(0) @binding(19) var<storage, read_write> particles_out: array<FluidParticle>;

@group(0) @binding(20) var<storage, read_write> faces_rw: array<FaceSample>;

// 8 floats per body: the linear and angular impulse the water has put on it
// so far this tick (gpu_flip_bodies.wgsl).
@group(0) @binding(21) var<storage, read> reaction: array<f32>;

// Two words per particle slot: the retired stage-cap count (always zero for
// native RK3), and solid push-outs refused past SOLID_PUSH over the tick.
// The tick's stats reduce them (liquid_stats words 8 and 9).
@group(0) @binding(22) var<storage, read_write> capped: array<u32>;

// Inflow (code 2) and outflow (code 3) rows: LiquidBody rows with the code in
// angular_velocity.w, the emitted velocity in inv_inertia_x.xyz and the share
// of the region's own motion added to it in inv_inertia_x.w.
@group(0) @binding(36) var<storage, read> regions: array<LiquidBody>;

// One word per half-cell site: the emission flags, scanned in place.
@group(0) @binding(37) var<storage, read_write> emit_scan: array<u32>;

// The sorted particles, written past the live ones by emit_write.
@group(0) @binding(38) var<storage, read_write> emitted: array<FluidParticle>;

@group(0) @binding(46) var<storage, read> clock_plan: array<ClockPlan>;

// This tick's bodies posed at this step's end by pose_bodies, one per body of
// the tick in row order: what every solid pass of the step reads.
@group(0) @binding(48) var<storage, read_write> posed: array<LiquidBody>;

// 80 per row of `bodies`, tick major like it: the body's support points at
// the tick's start (liquid::bodies::BodySupports).
@group(0) @binding(49) var<storage, read> contacts: array<vec4<f32>>;

// Six per posed body: its packed response to the pressure while it stays on
// its supports (liquid_constrained_mobility), which the body passes read.
@group(0) @binding(50) var<storage, read_write> mobility: array<vec4<f32>>;

// Set by resolve_solid when it refuses a push-out past SOLID_PUSH.
var<private> push_refused: u32 = 0u;

fn clock_live() -> bool {
    return clock_plan[0].live_mode != 0u;
}

fn adaptive_step_dt() -> f32 {
    return select(u.step_dt, clock_plan[0].step_dt, clock_live());
}

fn adaptive_tick_seconds() -> f32 {
    return select(u.tick_seconds, clock_plan[0].elapsed, clock_live());
}

fn clock_active() -> bool {
    return !clock_live() || clock_plan[0].step_dt > 0.0;
}

fn ts_pool_tiles() -> u32 {
    return u.pool_tiles;
}

fn ts_table_entries() -> u32 {
    return u.table_entries;
}

fn ts_table_word(i: u32) -> u32 {
    return ts_table[i];
}

// The world tile table (world_tiles_read.wgsl has its layout).
@group(0) @binding(51) var<storage, read> ts_table: array<u32>;

// The box's first world cell and its cells per axis.
fn box_lo() -> vec3<i32> {
    return vec3<i32>(round((u.walls_lo - u.origin) * u.inv_cell));
}

fn box_cells() -> vec3<i32> {
    return vec3<i32>(round((u.walls_hi - u.walls_lo) * u.inv_cell));
}

fn in_box(p: vec3<i32>) -> bool {
    let lo = box_lo();
    return all(p >= lo) && all(p < lo + box_cells());
}

// Record p of the box's record range: one past the cells on every axis.
fn in_box_records(p: vec3<i32>) -> bool {
    let lo = box_lo();
    return all(p >= lo) && all(p <= lo + box_cells());
}

// Face a of record p exists when p is in the box's record range and every
// other coordinate is inside the cells.
fn face_exists(p: vec3<i32>, a: i32) -> bool {
    let lo = box_lo();
    var top = lo + box_cells() - vec3<i32>(1);
    top[a] = top[a] + 1;
    return all(p >= lo) && all(p <= top);
}

// Face a of record p is on a box wall: the low or high edge face of axis a.
fn on_box_wall(p: vec3<i32>, a: i32) -> bool {
    let lo = box_lo();
    return p[a] == lo[a] || p[a] == lo[a] + box_cells()[a];
}

// The canonical face mask of record p: bit a set when face a exists and
// lies on a box wall.
fn box_wall_mask(p: vec3<i32>) -> u32 {
    var mask = 0u;
    for (var a = 0; a < 3; a = a + 1) {
        if face_exists(p, a) && on_box_wall(p, a) {
            mask = mask | (1u << u32(a));
        }
    }
    return mask;
}

// The sort's range of the cell at pool index idx. The canonical element's
// range holds the particles whose tile has no slot (an exhausted pool): no
// cell's, so it reads as empty.
fn cell_range(idx: u32) -> CellRange {
    if idx == ts_canon() {
        return CellRange(0u, 0u);
    }
    return ranges[idx];
}

// The world cell holding position x.
fn world_cell(x: vec3<f32>) -> vec3<i32> {
    return ts_world_cell(x, u.origin, u.inv_cell);
}

// The tank walls' signed distance at x: every wall closed, wall_inset cells
// in from the box, as node.liquid_solid_distance takes them
// (liquid_solid_distance_body.wgsl): the nearest wall's distance inside,
// minus the distance to the walls' box outside.
fn walls_distance(x: vec3<f32>) -> f32 {
    let inset = vec3<f32>(max(u.wall_inset, 0.0) * u.cell_size);
    let low = u.walls_lo + inset;
    let high = u.walls_hi - inset;
    let far = length(u.walls_hi - u.walls_lo + vec3<f32>(u.cell_size));
    var distance = far;
    var outside = vec3<f32>(0.0);
    for (var d = 0; d < 3; d = d + 1) {
        let axis = min(far, min(x[d] - low[d], high[d] - x[d]));
        distance = min(distance, axis);
        outside[d] = min(axis, 0.0);
    }
    if any(outside < vec3<f32>(0.0)) {
        distance = -length(outside);
    }
    return distance;
}

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
// There is no domain box: every cell and record of a held tile is part of the
// lattice, and every record has its three faces. The list passes run over
// held slots, 512 threads a slot, and touch every cell of the slot's tile; a
// fresh tile starts with the canonical values (`tiles_fresh`). Each list
// thread sets its cell's tile as its home (`ts_home`), so neighbour reads
// cost no hash. The Bounds walls are solids like the bodies: they enter only
// through `solid_distance`, and the passes that handle solids handle them with
// no case of their own. An Open face is no solid: the move removes water that
// ends beyond its plane.
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
    // The Bounds box in metres as authored: the planes of the Wall and Open
    // faces. Read only through the Wall and Open masks below.
    bounds_lo: vec3<f32>,
    // Slots of `sorted`.
    capacity: u32,
    bounds_hi: vec3<f32>,
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
    // The Bounds faces that are solid walls: bit 2d the low face of axis d,
    // bit 2d + 1 the high one.
    wall_faces: u32,
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
    // The Bounds faces that remove water ending beyond their plane, same bits.
    open_faces: u32,
    // Box3D's substep over this tick, dt / box3d_substep_count(dt): the h in
    // the coupled motion law (liquid_body_state).
    coupled_h: f32,
    // Tiles the pool holds (P).
    pool_tiles: u32,
    // The force and impulse lattices' first node in metres.
    field_min: vec3<f32>,
    // The table's entries (H).
    table_entries: u32,
    // Half-cell sites the inflows of one tick enumerate at most: the
    // emission passes' thread count (`region_sites`).
    emit_sites: u32,
    // The move removes a particle that ends below this height in metres;
    // f32::MIN removes none.
    kill_height: f32,
    // Narrow Band (narrow_band.wgsl): 0 off, NB_ON, or NB_RESTORE for the
    // steps after it is switched off.
    narrow_band: u32,
    // The band's depth in cells, R.
    band_cells: f32,
    // Each Bounds face plane's velocity along its axis in m/s, the low faces
    // then the high: what a moving Wall gives the water it touches.
    wall_velocity_lo: vec3<f32>,
    _pad2: u32,
    wall_velocity_hi: vec3<f32>,
    _pad3: u32,
    // Nonzero when every held slot's static corners are rebuilt this step:
    // the first step, new lattice buffers, or a new shapes buffer.
    static_all: u32,
    // Surface tension over density in m³/s² (gpu_flip_step.rs
    // `surface_tension`): the water's pressure at the free surface is this
    // times its curvature, with `ghost` 1.
    tension: f32,
    _pad5: u32,
    _pad6: u32,
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
    // A static collider (tree == 1) has no lattice: its geometry is the BVH
    // at node `tree_node` and first triangle `tree_triangle` of
    // `collider_nodes` and `collider_triangles`.
    tree_node: u32,
    tree_triangle: u32,
    tree: u32,
    tree_pad: u32,
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

// The Wall faces' signed distance at x (the contract of
// `manifold_water_liquid::lattice::bounds_distance` and
// liquid_solid_distance_body.wgsl): a Wall face's distance is x − low on the
// low face of axis a and high − x on the high one, positive on the water
// side. The distance is the smallest over the Wall faces, 1e30 with none; once
// x is beyond any Wall it is minus the length of the vector holding, on each
// axis, the smaller of 0 and that axis's Wall distances. Open faces are no
// solids.
fn walls_distance(x: vec3<f32>) -> f32 {
    if u.wall_faces == 0u {
        return 1e30;
    }
    var distance = 1e30;
    var beyond = vec3<f32>(0.0);
    for (var a = 0; a < 3; a = a + 1) {
        var axis = 1e30;
        if (u.wall_faces & (1u << u32(2 * a))) != 0u {
            axis = min(axis, x[a] - u.bounds_lo[a]);
        }
        if (u.wall_faces & (1u << u32(2 * a + 1))) != 0u {
            axis = min(axis, u.bounds_hi[a] - x[a]);
        }
        distance = min(distance, axis);
        beyond[a] = min(axis, 0.0);
    }
    if any(beyond < vec3<f32>(0.0)) {
        distance = -length(beyond);
    }
    return distance;
}

// The velocity of the Wall face nearest x: its plane's velocity along its
// axis, the only part a plane carries, so the water slips along a Wall. Zero
// with no Wall.
fn walls_velocity(x: vec3<f32>) -> vec3<f32> {
    var nearest = 1e30;
    var v = vec3<f32>(0.0);
    for (var a = 0; a < 3; a = a + 1) {
        if (u.wall_faces & (1u << u32(2 * a))) != 0u && abs(x[a] - u.bounds_lo[a]) < nearest {
            nearest = abs(x[a] - u.bounds_lo[a]);
            v = vec3<f32>(0.0);
            v[a] = u.wall_velocity_lo[a];
        }
        if (u.wall_faces & (1u << u32(2 * a + 1))) != 0u && abs(u.bounds_hi[a] - x[a]) < nearest {
            nearest = abs(u.bounds_hi[a] - x[a]);
            v = vec3<f32>(0.0);
            v[a] = u.wall_velocity_hi[a];
        }
    }
    return v;
}

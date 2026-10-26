// GPU FLIP's water step: every pass of one step but the sort, the solid
// distance and the pressure solve, each a separate entry point. Included
// after liquid_pose.wgsl, liquid_collider.wgsl and force_lattice.wgsl.
// Narrow-band transport, sharp coupling and lifecycle integration follow
// Ferstl et al., Narrow Band FLIP for Liquid Simulations, CGF 35(2), 2016,
// doi:10.1111/cgf.12825. FLIP Fluids attribution below covers only the
// explicitly named baseline transfers, boundaries and source operations.
//
// Cells are n per axis from the box minimum, x fastest. A face grid is
// (n + 1)³ FaceSample records indexed like the cells with m = n + 1: record
// p holds the low x, y and z faces of cell p, face a existing when every
// other coordinate is under n. Face a of p sits at p on axis a and p + ½ on
// the other two, in cells from the box minimum. The solid corner lattice is
// (n + 1)³ values from the box minimum.
//
// The CPU sizes every buffer for the lattice and the particle slots before
// it dispatches; each pass only checks its thread is inside its range.
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
    // Cells per axis.
    n: vec3<u32>,
    // Slots of `sorted`.
    capacity: u32,
    box_min: vec3<f32>,
    cell_size: f32,
    gravity: vec3<f32>,
    step_dt: f32,
    field_nodes: vec3<u32>,
    field_spacing: f32,
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
    // The largest |box_min| component, for the solid faces' tolerance.
    box_offset: f32,
    // subtract: 1 reads `phi` for the free surface, 0 keeps air at zero.
    ghost: u32,
    // Particles the move writes.
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
    // 1: every tile is active (the test-only oracle).
    all_tiles: u32,
    // The ring a sparse pass's reads are capped at.
    extension_layers: u32,
    // The farthest ring the table holds; ring_max + 1 means none within it.
    ring_max: u32,
    narrow_band: u32, // Ferstl 2016: 0 dense, 1 initialization, 2 band-masked.
    live_impulse_stride: u32,
    // Cells from the grid's minimum to each wall (FlipSolverGrid::wall_inset);
    // 0 when the walls are the grid's edges.
    wall_inset: f32,
    // Box3D's substep over this tick, dt / box3d_substep_count(dt): the h in
    // the coupled motion law (liquid_body_state).
    coupled_h: f32,
    pad1: u32,
    pad2: u32,
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

fn ts_cells() -> vec3<i32> {
    return vec3<i32>(u.n);
}

// Face a of record p exists when every other coordinate is inside the cells.
fn face_exists(p: vec3<i32>, n: vec3<i32>, a: i32) -> bool {
    var other = p;
    other[a] = 0;
    return all(other < n);
}

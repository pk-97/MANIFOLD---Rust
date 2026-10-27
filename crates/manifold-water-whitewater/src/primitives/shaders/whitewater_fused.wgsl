// The whitewater stage's fused kernels. Phase functions are copies of the named atom bodies.
// No hoisting: the atom pipelines remain the bitwise oracle under Fast math.
// Grid arrays live on the liquid step's world tiles (`world_tiles_read.wgsl` composes
// before this file): `buf_tiles` is the step's table and every grid array holds one
// element per pool index, the canonical element last.
// Ported from FLIP Fluids (MIT); see THIRD_PARTY_NOTICES.md.
struct Element { position_radius: vec4<f32>, velocity: vec3<f32>, id: u32, }
struct KnownValue { value: f32, known: f32, }
struct FaceSample { face_velocity: vec4<f32>, face_weight: vec4<f32>, }
struct WhitewaterSource { influence: f32, dust_strength: f32, kind: u32, pad: u32, }
struct Spawn { position_lifetime: vec4<f32>, velocity: vec3<f32>, kind: u32, }
struct Pool { position_lifetime: vec4<f32>, velocity: vec3<f32>, kind: u32, id: u32, _pad0: u32, _pad1: u32, _pad2: u32, }
struct EmitParams {
    cell_size: f32,
    pool_tiles: f32,
    bounds_lo_z: f32,
    seed: f32,
    epoch: f32,
    spray_speed: f32,
    min_energy: f32,
    max_energy: f32,
    min_curvature: f32,
    max_curvature: f32,
    sharpness: f32,
    min_turbulence: f32,
    max_turbulence: f32,
    inside_enabled: f32,
    rate: f32,
    turbulence_rate: f32,
    generation_rate: f32,
    points_per_cell: f32,
    ticks: f32,
    dt: f32,
    dust_enabled: f32,
    boundary_dust: f32,
    _pad0: f32,
    _pad1: f32,
}
struct SpawnParams {
    capacity: f32,
    seed: f32,
    epoch: f32,
    min_lifetime: f32,
    max_lifetime: f32,
    lifetime_variance: f32,
    dt: f32,
    spray_speed: f32,
    type_seed: f32,
    type_epoch: f32,
    dust: f32,
    cell_size: f32,
    pool_tiles: f32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
}
struct LifecycleParams {
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    dt: f32,
    foam_advection: f32,
    bubble_buoyancy: f32,
    bubble_drag: f32,
    spray_drag: f32,
    spray_drag_variance: f32,
    spray_restitution: f32,
    spray_friction: f32,
    substep_count: f32,
    field_nodes_x: f32,
    field_nodes_y: f32,
    field_nodes_z: f32,
    field_spacing: f32,
    force_lattices: f32,
    tick_index: f32,
    first_tick: f32,
    cell_size: f32,
    pool_tiles: f32,
    bubble_lifetime_modifier: f32,
    foam_lifetime_modifier: f32,
    spray_lifetime_modifier: f32,
    count: u32,
}
struct TurbulenceParams {
    cell_size: f32,
    pool_tiles: f32,
    count: u32,
    _pad0: u32,
}
@group(0) @binding(0) var<uniform> p: EmitParams;
@group(0) @binding(0) var<uniform> sp: SpawnParams;
@group(0) @binding(0) var<uniform> lc: LifecycleParams;
@group(0) @binding(0) var<uniform> tf: TurbulenceParams;
// Resources are disjoint per entry point, so one binding number can name a
// different array in each (the dispatch cap is 16 bindings). Every kernel
// binds the step's table first. A kernel's own buffers follow in binding order.
//   ww_emit:       tiles, particles, faces, distance, cells, curvature, turbulence,
//                  influence, identity, then sampled, energy, counts, unscaled and
//                  wavecrest bits out
//   ww_dust:       tiles, particles, source, solid, turbulence, influence, identity,
//                  wavecrest bits, then energy and counts out
//   ww_spawn:      tiles, particles, faces, distance, cells, solid, offsets, energy,
//                  identity, then the typed spawns out
//   ww_lifecycle:  tiles, pool, faces, distance, cells, solid, schedule, history,
//                  forces, impulses, then the pool out
//   ww_turbulence: tiles, then turbulence out, faces, distance
@group(0) @binding(1) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(2) var<storage, read> buf_particles: array<Element>;
@group(0) @binding(2) var<storage, read> buf_pool: array<Pool>;
@group(0) @binding(2) var<storage, read_write> out_turbulence: array<f32>;
@group(0) @binding(3) var<storage, read> buf_faces: array<FaceSample>;
@group(0) @binding(3) var<storage, read> buf_source: array<WhitewaterSource>;
@group(0) @binding(4) var<storage, read> buf_distance: array<f32>;
@group(0) @binding(5) var<storage, read> buf_cells: array<u32>;
@group(0) @binding(6) var<storage, read> buf_curvature: array<KnownValue>;
@group(0) @binding(6) var<storage, read> buf_solid: array<f32>;
@group(0) @binding(7) var<storage, read> buf_turbulence: array<f32>;
@group(0) @binding(7) var<storage, read> buf_offsets: array<u32>;
@group(0) @binding(7) var<storage, read> buf_substep_schedule: array<f32>;
@group(0) @binding(8) var<storage, read> buf_influence: array<f32>;
@group(0) @binding(8) var<storage, read> buf_energy: array<f32>;
@group(0) @binding(8) var<storage, read> buf_substep_faces: array<f32>;
// The particle identity: word 7 counts the liquid particles in use. The emit,
// dust and spawn passes read no slot at or past it; the emit and dust passes
// dispatch over them from the record's groups (words 4-6).
@group(0) @binding(9) var<storage, read> buf_identity: array<u32>;
@group(0) @binding(9) var<storage, read> buf_forces: array<f32>;
@group(0) @binding(10) var<storage, read_write> out_sampled: array<Element>;
@group(0) @binding(10) var<storage, read_write> out_typed: array<Spawn>;
@group(0) @binding(10) var<storage, read> buf_impulses: array<f32>;
@group(0) @binding(11) var<storage, read_write> out_energy: array<f32>;
@group(0) @binding(11) var<storage, read_write> out_pool: array<Pool>;
@group(0) @binding(12) var<storage, read_write> out_counts: array<u32>;
@group(0) @binding(13) var<storage, read_write> out_unscaled: array<Element>;
@group(0) @binding(14) var<storage, read_write> out_wavecrest_bits: array<u32>;
const IDENTITY_N: u32 = 7u;


// Cell readers the emitter phases share, each read through the step's
// world-tile table (`buf_tiles`): a cell in a tile it does not hold reads the
// canonical element.
var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }
fn wc_distance(c: vec3<i32>) -> f32 {
    return buf_distance[ts_cell_at(c)];
}

fn wc_curvature(c: vec3<i32>) -> f32 {
    return buf_curvature[ts_cell_at(c)].value;
}

fn wc_turbulence(c: vec3<i32>) -> f32 {
    return buf_turbulence[ts_cell_at(c)];
}

fn wc_borders_air(c: vec3<i32>) -> bool {
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                if buf_cells[ts_cell_at(c + vec3<i32>(dx, dy, dz))] == 0u {
                    return true;
                }
            }
        }
    }
    return false;
}
// ── jitter_particles ──
// node.jitter_particles — fusable BUFFER body, COINCIDENT. FLIP's emitter
// jitter (diffuseparticlesimulation.cpp:1557): a live particle moves by a
// uniform offset in [−j, j] on each axis, j = 0.25·(1 − 1e-3)·cell_size,
// drawn from a stateless hash of (slot, seed, epoch). Velocity, radius and
// id pass through; a slot with radius 0 passes whole.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn ww_phase_jitter(idx: u32, count: u32, e_particles: Element, cell_size: f32, seed: f32, epoch: f32) -> Element {
    if !(e_particles.position_radius.w > 0.0) {
        return e_particles;
    }
    let reach = 0.25 * (1.0 - 1e-3) * cell_size;
    let s = bitcast<u32>(seed);
    let generation = u32(max(round(epoch), 0.0));
    var p = e_particles.position_radius;
    p.x = p.x + reach * (2.0 * ww_random(idx, s, generation, 0u) - 1.0);
    p.y = p.y + reach * (2.0 * ww_random(idx, s, generation, 1u) - 1.0);
    p.z = p.z + reach * (2.0 * ww_random(idx, s, generation, 2u) - 1.0);
    return Element(p, e_particles.velocity, e_particles.id);
}

// ── sample_faces_at_particles ──
// node.sample_faces_at_particles — fusable BUFFER body, COINCIDENT
// particles, GATHER faces and tiles. A live particle's velocity becomes
// FLIP's MAC trilinear of the step's pooled faces at its position
// (macvelocityfield.cpp:635): each component from the eight faces of its
// staggered stencil, and 0 on every axis for a particle whose cell lies in no
// stored tile. Position, radius and id pass through; a slot with radius 0
// passes whole. Face f of an axis is cell f's low face, the `faces` record of
// world cell f read through the world-tile table (`tiles`, `pool_tiles`
// tiles); a face in an absent tile reads the canonical record, and a face
// whose weight is not positive reads 0.
//
// Ported from FLIP Fluids macvelocityfield.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn ww_phase_sample(
    idx: u32,
    count: u32,
    e_particles: Element,
    cell_size: f32,
    pool_tiles: f32,
) -> Element {
    ww_pool_tiles = u32(pool_tiles);
    if !(e_particles.position_radius.w > 0.0) {
        return e_particles;
    }
    let still = Element(e_particles.position_radius, vec3<f32>(0.0), e_particles.id);
    let q = ww_cell_position(e_particles.position_radius.xyz, cell_size);
    let cell = vec3<i32>(floor(q));
    ts_home(ts_cell_at(cell));
    if !ts_cell_stored(cell) {
        return still;
    }
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let face = buf_faces[ts_face(base + vec3<i32>(ww_corner(corner)))];
            sum = sum + ww_corner_weight(f, corner) * lf_face_velocity(face.face_velocity, face.face_weight, axis);
        }
        v[axis] = sum;
    }
    return Element(e_particles.position_radius, v, e_particles.id);
}

// ── whitewater_emitter_velocity ──
// FLIP diffuseparticlesimulation.cpp:1699: scale eligible surface emitter velocity.
// The distance is read trilinearly at cell centres and the air test over the
// 26 neighbours of the particle's cell, both through the step's world-tile
// table (`tiles`, `pool_tiles` tiles): a cell in an absent tile reads the
// canonical element (distance 3h, air).

fn ww_phase_velocity(
    idx: u32,
    count: u32,
    e_particles: Element,
    spray_speed: f32,
    seed: f32,
    epoch: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> Element {
    ww_pool_tiles = u32(pool_tiles);
    if !(e_particles.position_radius.w > 0.0) {
        return e_particles;
    }
    let h = cell_size;
    let q = ww_cell_position(e_particles.position_radius.xyz, h);
    let cell = vec3<i32>(floor(q));
    ts_home(ts_cell_at(cell));
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * wc_distance(base + vec3<i32>(ww_corner(corner)));
    }
    var out = e_particles;
    if abs(d) < 1.5 * h && wc_borders_air(cell) && d > -0.75 * h {
        out.velocity *= 1.0 + (spray_speed - 1.0) * ww_random(idx, bitcast<u32>(seed), u32(max(round(epoch), 0.0)), 9u);
    }
    return out;
}

// ── energy_potential ──
// node.energy_potential — fusable BUFFER body, COINCIDENT. FLIP's energy
// potential (diffuseparticlesimulation.cpp:1768): the particle's kinetic
// energy per unit mass, ½|v|², held to [min_energy, max_energy] and scaled
// to 0..1 across it. 0 for a slot with radius 0 or an empty range.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn ww_phase_energy(idx: u32, count: u32, e_particles: Element, min_energy: f32, max_energy: f32) -> f32 {
    if !(e_particles.position_radius.w > 0.0) || !(max_energy > min_energy) {
        return 0.0;
    }
    let v = e_particles.velocity;
    let e = min(max(0.5 * dot(v, v), min_energy), max_energy);
    return (e - min_energy) / (max_energy - min_energy);
}

// ── wavecrest_potential ──
// node.wavecrest_potential — fusable BUFFER body, COINCIDENT particles,
// GATHER distance, curvature, cells and tiles. FLIP's wavecrest potential for
// a surface particle (diffuseparticlesimulation.cpp:1571, :1718), with the
// distance and curvature read trilinearly at cell centres:
//   0 unless the particle sits within 1.5 cells of the surface and its cell
//   borders air (26 neighbours);
//   0 when it is still (every velocity component under 1e-6);
//   k = curvature × cell size, 0 below min_curvature, held at max_curvature;
//   0 when the distance's gradient vanishes or the velocity's direction
//   meets the surface normal below `sharpness` (a dot product);
//   else (k − min_curvature)/(max_curvature − min_curvature).
// 0 for a slot with radius 0. Every field is read through the step's
// world-tile table (`tiles`, `pool_tiles` tiles): a cell in an absent tile
// reads the canonical element (distance 3h, curvature 0, air).
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp and interpolation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn ww_phase_wavecrest(
    idx: u32,
    count: u32,
    e_particles: Element,
    min_curvature: f32,
    max_curvature: f32,
    sharpness: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    if !(e_particles.position_radius.w > 0.0) || !(max_curvature > min_curvature) {
        return 0.0;
    }
    let h = cell_size;
    let q = ww_cell_position(e_particles.position_radius.xyz, h);
    let cell = vec3<i32>(floor(q));
    ts_home(ts_cell_at(cell));
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var phi: array<f32, 8>;
    var d = 0.0;
    var k = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let c = base + vec3<i32>(ww_corner(corner));
        phi[corner] = wc_distance(c);
        let w = ww_corner_weight(f, corner);
        d = d + w * phi[corner];
        k = k + w * wc_curvature(c);
    }
    if !(abs(d) < 1.5 * h) || !wc_borders_air(cell) {
        return 0.0;
    }
    let v = e_particles.velocity;
    if all(abs(v) < vec3<f32>(1e-6)) {
        return 0.0;
    }
    k = k * h;
    if k < min_curvature {
        return 0.0;
    }
    k = min(k, max_curvature);
    // FLIP's trilinear gradient (interpolation.cpp:197), unscaled: corner
    // index = x + 2y + 4z.
    let gx = mix(mix(phi[1] - phi[0], phi[3] - phi[2], f.y), mix(phi[5] - phi[4], phi[7] - phi[6], f.y), f.z);
    let gy = mix(mix(phi[2] - phi[0], phi[3] - phi[1], f.x), mix(phi[6] - phi[4], phi[7] - phi[5], f.x), f.z);
    let gz = mix(mix(phi[4] - phi[0], phi[5] - phi[1], f.x), mix(phi[6] - phi[2], phi[7] - phi[3], f.x), f.y);
    let grad = vec3<f32>(gx, gy, gz);
    if all(abs(grad) < vec3<f32>(1e-6)) {
        return 0.0;
    }
    if dot(normalize(v), normalize(grad)) < sharpness {
        return 0.0;
    }
    return (k - min_curvature) / (max_curvature - min_curvature);
}

// ── inside_turbulence_potential ──
// FLIP diffuseparticlesimulation.cpp:1571, :1748: inside-set turbulence potential.
// Samples turbulence and distance at p - h/2 through the step's world-tile
// table (`tiles`, `pool_tiles` tiles): a cell in an absent tile reads the
// canonical element (distance 3h, turbulence 0, air).

fn ww_phase_inside(
    idx: u32,
    count: u32,
    e_particles: Element,
    min_turbulence: f32,
    max_turbulence: f32,
    inside_enabled: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    if !(e_particles.position_radius.w > 0.0) || !(max_turbulence > min_turbulence) {
        return 0.0;
    }
    let h = cell_size;
    let q = ww_cell_position(e_particles.position_radius.xyz, h);
    let cell = vec3<i32>(floor(q));
    ts_home(ts_cell_at(cell));
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    var k = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let c = base + vec3<i32>(ww_corner(corner));
        let w = ww_corner_weight(f, corner);
        d = d + w * wc_distance(c);
        k = k + w * wc_turbulence(c);
    }
    if (abs(d) < 1.5 * h) && wc_borders_air(cell) {
        return 0.0;
    }
    return inside_enabled * (clamp(k, min_turbulence, max_turbulence) - min_turbulence) / (max_turbulence - min_turbulence);
}

// ── turbulence_emission_count ──
// FLIP diffuseparticlesimulation.cpp:1989: nearest-cell influence times energy
// times wavecrest/turbulence rates; independent emitter-generation coin.
// The existing GPU marker-density compensation and per-tick rounding are retained.
// The influence is the node floor(q) of the particle's cell, read through the
// step's world-tile table (`tiles`, `pool_tiles` tiles); a particle whose
// cell lies in no stored tile emits nothing.

fn ww_phase_count(
    idx: u32,
    count: u32,
    e_particles: Element,
    e_energy: f32,
    e_wavecrest: f32,
    e_turbulence: f32,
    rate: f32,
    turbulence_rate: f32,
    generation_rate: f32, seed: f32, epoch: f32,
    points_per_cell: f32,
    ticks: f32,
    live_count: f32,
    dt: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> u32 {
    ww_pool_tiles = u32(pool_tiles);
    if f32(idx) >= live_count || !(e_particles.position_radius.w > 0.0) || !(points_per_cell > 0.0) {
        return 0u;
    }
    if length(e_particles.velocity) < 1e-3 || e_energy < 1e-6 {
        return 0u;
    }
    if ww_random(idx, bitcast<u32>(seed), u32(max(round(epoch), 0.0)), 10u) >= generation_rate { return 0u; }
    let q = ww_cell_position(e_particles.position_radius.xyz, cell_size);
    let node = vec3<i32>(floor(q));
    ts_home(ts_cell_at(node));
    if !ts_cell_stored(node) { return 0u; }
    let per_tick = buf_influence[ts_cell_at(node)] * e_energy * (rate * e_wavecrest + turbulence_rate * e_turbulence) * dt * 8.0 / points_per_cell;
    if !(per_tick > 0.0) {
        return 0u;
    }
    return u32(floor(per_tick + 0.5)) * u32(max(round(ticks), 0.0));
}

// ── dust_potential ──
// FLIP diffuseparticlesimulation.cpp:1801: dust potential near a dust-emitting
// solid. The source record is read at the node floor(q) of the particle's
// cell, the solid distance trilinearly over that cell's eight nodes and
// the turbulence at cell centres, all through the step's world-tile table
// (`tiles`, `pool_tiles` tiles). A particle whose cell lies in no stored tile
// emits nothing. A domain source needs Boundary Dust and the particle within
// 1.5 cells of the Bounds' low z face (bounds_lo_z).

fn dust_solid(c: vec3<i32>) -> f32 {
    return buf_solid[ts_cell_at(c)];
}

fn ww_phase_dust(
    idx: u32,
    count: u32,
    e_particles: Element,
    min_turbulence: f32,
    max_turbulence: f32,
    dust_enabled: f32, boundary_dust: f32,
    bounds_lo_z: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    if !(e_particles.position_radius.w > 0.0) || !(max_turbulence > min_turbulence) {
        return 0.0;
    }
    let h = cell_size;
    let q = ww_cell_position(e_particles.position_radius.xyz, h);
    let node = vec3<i32>(floor(q));
    ts_home(ts_cell_at(node));
    if dust_enabled < 0.5 || !ts_cell_stored(node) { return 0.0; }
    let source = buf_source[ts_cell_at(node)];
    if source.kind == 0u || source.dust_strength <= 0.0 { return 0.0; }
    // A Bounds wall gives dust only within 1.5 cells of the Bounds' low z face.
    if source.kind == 1u && (boundary_dust < 0.5 || q.z - bounds_lo_z / h > 2.0) { return 0.0; }
    var clearance = 0.0;
    for (var corner = 0u; corner < 8u; corner++) {
        clearance += ww_corner_weight(fract(q), corner) * dust_solid(node + vec3<i32>(ww_corner(corner)));
    }
    if clearance < 0.0 || clearance > 2.5 * h { return 0.0; }
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var k = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let c = base + vec3<i32>(ww_corner(corner));
        let w = ww_corner_weight(f, corner);
        k = k + w * wc_turbulence(c);
    }
    let minimum = 0.75 * min_turbulence;
    return source.dust_strength * (clamp(k, minimum, max_turbulence) - minimum) / (max_turbulence - minimum);
}

// ── spawn_whitewater ──
// node.spawn_whitewater — fusable BUFFER body, GATHER only. One thread per
// spawn slot j, FLIP's _emitDiffuseParticles (diffuseparticlesimulation.cpp
// :1912) for one new particle. The step's faces and the solid distance are
// read through its world-tile table (`tiles`, `pool_tiles` tiles); a node or
// face in an absent tile reads the canonical element:
//   the frame emits total = offsets[emitters − 1]; slot j < min(total,
//   capacity) takes emission m = j, or ⌊j · total / capacity⌋ past capacity
//   (a uniform subset), and m's emitter e is the first with offsets[e] > m;
//   the particle lands in a cylinder about e's velocity: radius 8 marker
//   radii · √Xr, angle 2π·Xt, height Xh · |v| · dt along it (export supplies 1/60 s);
//   it is dropped in a cell outside every stored tile, or where the solid
//   distance is under a quarter cell;
//   lifetime = min + Ie·(max − min) ± variance, dropped at or below 0;
//   velocity is FLIP's MAC trilinear of the faces at its position (the same
//   stencil as node.sample_faces_at_particles).
// Dropped and unused slots write lifetime 0. Kind is left 0 for
// node.whitewater_type. Xr, Xt, Xh and the variance draw hash (j, seed,
// epoch) on their own streams.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// FLIP's emitter radius over the cell size: 8 marker radii, a marker being
// the sphere of an eighth of a cell, (3 / 32π)^(1/3).
const SW_EMITTER_RADIUS: f32 = 2.481402;

// FLIP's _solidBufferWidth, cells.
const SW_SOLID_BUFFER: f32 = 0.25;

const SW_TWO_PI: f32 = 6.28318;

fn sw_mul_div(a: u32, b: u32, c: u32) -> u32 {
    // floor(a·b / c) for a < c, through the 64-bit product.
    let a0 = a & 0xffffu;
    let a1 = a >> 16u;
    let b0 = b & 0xffffu;
    let b1 = b >> 16u;
    let p00 = a0 * b0;
    let p01 = a0 * b1;
    let p10 = a1 * b0;
    let mid = (p00 >> 16u) + (p01 & 0xffffu) + (p10 & 0xffffu);
    let lo = (p00 & 0xffffu) | (mid << 16u);
    var r = a1 * b1 + (p01 >> 16u) + (p10 >> 16u) + (mid >> 16u);
    var q = 0u;
    for (var bit = 31; bit >= 0; bit = bit - 1) {
        let carry = r >> 31u;
        r = (r << 1u) | ((lo >> u32(bit)) & 1u);
        q = q << 1u;
        if carry == 1u || r >= c {
            r = r - c;
            q = q | 1u;
        }
    }
    return q;
}

// The first of the `emitters` running totals past emission m.
fn sw_emitter(m: u32, emitters: u32) -> u32 {
    var lo = 0u;
    var hi = emitters;
    while lo < hi {
        let mid = lo + (hi - lo) / 2u;
        if buf_offsets[mid] > m {
            hi = mid;
        } else {
            lo = mid + 1u;
        }
    }
    return lo;
}

// FLIP's MAC trilinear at q (whitewater cells) over the pooled faces, a face
// in an absent tile reading the canonical record.
fn sw_velocity(q: vec3<f32>) -> vec3<f32> {
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let face = buf_faces[ts_face(base + vec3<i32>(ww_corner(corner)))];
            sum = sum + ww_corner_weight(f, corner) * lf_face_velocity(face.face_velocity, face.face_weight, axis);
        }
        v[axis] = sum;
    }
    return v;
}

// The solid distance at q, trilinear over its eight nodes (node n is the
// element of world cell n; a node in an absent tile reads the canonical
// 1e30), metres.
fn sw_solid(q: vec3<f32>) -> f32 {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * buf_solid[ts_cell_at(base + vec3<i32>(ww_corner(corner)))];
    }
    return d;
}

fn ww_phase_spawn(
    idx: u32,
    count: u32,
    capacity: f32,
    emitters: f32,
    seed: f32,
    epoch: f32,
    min_lifetime: f32,
    max_lifetime: f32,
    lifetime_variance: f32,
    dt: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> Spawn {
    ww_pool_tiles = u32(pool_tiles);
    let empty = Spawn(vec4<f32>(0.0), vec3<f32>(0.0), 0u);
    let n = min(u32(max(round(emitters), 0.0)), arrayLength(&buf_offsets));
    let slots = u32(max(round(capacity), 0.0));
    if n == 0u || slots == 0u {
        return empty;
    }
    let total = buf_offsets[n - 1u];
    if idx >= min(total, slots) {
        return empty;
    }
    var m = idx;
    if total > slots {
        m = sw_mul_div(idx, total, slots);
    }
    let e = sw_emitter(m, n);
    if e >= n || e >= arrayLength(&buf_particles) || e >= arrayLength(&buf_energy) {
        return empty;
    }
    let emitter = buf_particles[e];
    let v = emitter.velocity;
    if !(emitter.position_radius.w > 0.0) || length(v) < 1e-3 {
        return empty;
    }
    let h = cell_size;
    let axis = normalize(v);
    // FLIP's test as written; its first clause always holds.
    var e1: vec3<f32>;
    if abs(axis.x) - 1.0 < 1e-3 && abs(axis.y) < 1e-3 && abs(axis.z) < 1e-3 {
        e1 = normalize(cross(axis, vec3<f32>(0.0, 1.0, 0.0)));
    } else {
        e1 = normalize(cross(axis, vec3<f32>(1.0, 0.0, 0.0)));
    }
    let e2 = normalize(cross(axis, e1));
    let s = bitcast<u32>(seed);
    let generation = u32(max(round(epoch), 0.0));
    let r = SW_EMITTER_RADIUS * h * sqrt(ww_random(idx, s, generation, 4u));
    let theta = ww_random(idx, s, generation, 5u) * SW_TWO_PI;
    let along = ww_random(idx, s, generation, 6u) * length(dt * v);
    let p = emitter.position_radius.xyz + r * cos(theta) * e1 + r * sin(theta) * e2 + along * axis;
    let q = ww_cell_position(p, h);
    let cell = vec3<i32>(floor(q));
    ts_home(ts_cell_at(cell));
    if !ts_cell_stored(cell) {
        return empty;
    }
    if sw_solid(q) < SW_SOLID_BUFFER * h {
        return empty;
    }
    var lifetime = min_lifetime + buf_energy[e] * (max_lifetime - min_lifetime);
    lifetime = lifetime + lifetime_variance * (2.0 * ww_random(idx, s, generation, 7u) - 1.0);
    if !(lifetime > 0.0) {
        return empty;
    }
    return Spawn(vec4<f32>(p, lifetime), sw_velocity(q), 0u);
}

// ── whitewater_type ──
// node.whitewater_type — fusable BUFFER body, COINCIDENT spawns, GATHER
// distance, cells and tiles. FLIP's _getDiffuseParticleType
// (diffuseparticlesimulation.cpp:2056) for a fresh particle, with FLIP's
// defaults:
//   spray (2) when the particle's cell lies in no stored tile (the step's
//   world tiles have no box edge to inset from);
//   else by the distance at the particle, read trilinearly at cell centres:
//   foam (1) within a cell of the surface, bubble (0) deeper, spray (2)
//   higher;
//   foam or spray whose cell borders no air (26 neighbours) becomes bubble.
// The fields are read through the step's world-tile table (`tiles`,
// `pool_tiles` tiles): a cell in an absent tile reads the canonical element
// (distance 3h, air). Slots with lifetime 0 pass whole. The lifecycle types
// every particle by the same rule on each step, so the two must agree.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// FLIP's _maxFoamToSurfaceDistance and _foamLayerOffset, cells.
const WT_FOAM_DEPTH: f32 = 1.0;

const WT_FOAM_OFFSET: f32 = 0.0;

fn wt_distance(c: vec3<i32>) -> f32 {
    return buf_distance[ts_cell_at(c)];
}

fn wt_borders_air(c: vec3<i32>) -> bool {
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                if buf_cells[ts_cell_at(c + vec3<i32>(dx, dy, dz))] == 0u {
                    return true;
                }
            }
        }
    }
    return false;
}

fn wt_finish(p: Spawn, idx: u32, speed: f32, seed: f32, epoch: f32) -> Spawn {
    var out = p;
    if out.kind == 2u {
        out.velocity *= 1.0 + (speed - 1.0) * ww_random(idx, bitcast<u32>(seed), u32(max(round(epoch), 0.0)), 11u);
    }
    return out;
}

fn ww_phase_type(
    idx: u32,
    count: u32,
    e_spawns: Spawn,
    spray_speed: f32, seed: f32, epoch: f32, dust: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> Spawn {
    ww_pool_tiles = u32(pool_tiles);
    var spawn = e_spawns;
    if !(spawn.position_lifetime.w > 0.0) {
        return spawn;
    }
    if dust > 0.5 { spawn.kind = 4u; return spawn; }
    let h = cell_size;
    let q = ww_cell_position(spawn.position_lifetime.xyz, h);
    let cell = vec3<i32>(floor(q));
    ts_home(ts_cell_at(cell));
    if !ts_cell_stored(cell) {
        spawn.kind = 2u;
        return wt_finish(spawn, idx, spray_speed, seed, epoch);
    }
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * wt_distance(base + vec3<i32>(ww_corner(corner)));
    }
    let depth = WT_FOAM_DEPTH * h;
    let offset = WT_FOAM_OFFSET * h;
    var kind = 2u;
    if d > -depth + offset && d < depth + offset {
        kind = 1u;
    } else if d < -depth + offset {
        kind = 0u;
    }
    if kind != 0u && !wt_borders_air(cell) {
        kind = 0u;
    }
    spawn.kind = kind;
    return wt_finish(spawn, idx, spray_speed, seed, epoch);
}

// ── advect_whitewater ──
// node.advect_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER faces,
// solid, the substep history, the force lattices and the world-tile table.
// One FLIP tick of a live whitewater particle by its type
// (diffuseparticlesimulation.cpp:2250-2600), every limit behaviour collide:
//   spray: gravity and per-id drag, then the collision march; a collision
//     with a usable normal sets velocity from the old one split along it
//     (friction on the tangent part, restitution on the normal part) plus
//     gravity;
//   bubble: buoyancy against gravity and drag toward the liquid velocity;
//   foam: the liquid velocity times the advection strength;
//   all: a particle that moved faster than 1.1 times its new speed dies
//     (lifetime -1e6), and so does one whose travel is not finite.
// Work is in world metres, grid positions from `ww_cell_position`. The
// liquid velocity is FLIP's MAC trilinear at the old position over the
// step's pooled faces (the face of cell f along an axis is the record of
// cell f, 0 where it has no weight or its tile is not held) or, under a
// schedule, over substep k's row of the history, which lies on the same
// table. The solid is the node array read trilinearly, a node in a tile the
// table does not hold reading the canonical element. FLIP's near-solid
// early-out is dropped, and so is the boundary box: a wall is a solid. Slots
// with kind 3 is empty; kind 4 is dust, with FLIP's ID-dependent buoyancy and drag.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// _solidBufferWidth, _diffuseParticleStepDistanceFactor and the CFL number,
// cells; _maxVelocityFactor.
const AW_SOLID_BUFFER: f32 = 0.25;

const AW_STEP: f32 = 0.5;

const AW_MAX_RESOLVE: f32 = 5.0;

const AW_MAX_VELOCITY: f32 = 1.1;

const AW_EPS: f32 = 1e-6;

const AW_DEAD: f32 = -1e6;

// _diffuseParticleIDLimit - 1.
const AW_ID_TOP: f32 = 255.0;

// The solid reads 1e30 in a tile no slot holds; stopping a node read here
// keeps the gradient's differences finite.
const AW_FAR: f32 = 1e4;

// Component `axis` of the face of cell c: the step's own faces, or substep
// `step` of the history (0xffffffff reads the step's own).
fn aw_face(axis: u32, c: vec3<i32>, step: u32) -> f32 {
    let i = ts_face(c);
    if step != 0xffffffffu {
        return buf_substep_faces[(step * ts_pool_len() + i) * 4u + axis];
    }
    let s = buf_faces[i];
    return lf_face_velocity(s.face_velocity, s.face_weight, axis);
}

// FLIP's MAC trilinear at grid position q.
fn aw_velocity(q: vec3<f32>, step: u32) -> vec3<f32> {
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            sum = sum + ww_corner_weight(f, corner) * aw_face(axis, base + vec3<i32>(ww_corner(corner)), step);
        }
        v[axis] = sum;
    }
    return v;
}

fn aw_node(n: vec3<i32>) -> f32 {
    return min(buf_solid[ts_cell_at(n)], AW_FAR);
}

// The solid's distance at grid position q, metres.
fn aw_solid(q: vec3<f32>) -> f32 {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * aw_node(base + vec3<i32>(ww_corner(corner)));
    }
    return d;
}

// FLIP's trilinear gradient (interpolation.cpp:197), unscaled; corner index
// x + 2y + 4z.
fn aw_gradient(q: vec3<f32>) -> vec3<f32> {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var phi: array<f32, 8>;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        phi[corner] = aw_node(base + vec3<i32>(ww_corner(corner)));
    }
    let gx = mix(mix(phi[1] - phi[0], phi[3] - phi[2], f.y), mix(phi[5] - phi[4], phi[7] - phi[6], f.y), f.z);
    let gy = mix(mix(phi[2] - phi[0], phi[3] - phi[1], f.x), mix(phi[6] - phi[4], phi[7] - phi[5], f.x), f.z);
    let gz = mix(mix(phi[4] - phi[0], phi[5] - phi[1], f.x), mix(phi[6] - phi[2], phi[7] - phi[3], f.x), f.y);
    return vec3<f32>(gx, gy, gz);
}

fn aw_step(
    e_pool: Pool,
    cell_size: f32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    dt: f32,
    foam_advection: f32,
    bubble_buoyancy: f32,
    bubble_drag: f32,
    spray_drag: f32,
    spray_drag_variance: f32,
    spray_restitution: f32,
    spray_friction: f32,
    face_step: u32,
) -> Pool {
    var out = e_pool;
    if (e_pool.kind == 3u || e_pool.kind > 4u) || !(dt > 0.0) {
        return out;
    }
    let h = cell_size;
    let p = e_pool.position_lifetime.xyz;
    let v = e_pool.velocity;
    let gravity = vec3<f32>(gravity_x, gravity_y, gravity_z);
    let spray = e_pool.kind == 2u;
    let q = ww_cell_position(p, h);
    ts_home(ts_cell_at(vec3<i32>(floor(q))));

    var nextv: vec3<f32>;
    if spray {
        let factor = f32(e_pool.id) / AW_ID_TOP;
        let mind = max(spray_drag - spray_drag * spray_drag_variance, 0.0);
        let maxd = spray_drag + spray_drag * spray_drag_variance;
        let drag = mind + (1.0 - factor) * (maxd - mind);
        nextv = v + gravity * dt + (-drag * v * dt);
    } else {
        let vmac = aw_velocity(q, face_step);
        if e_pool.kind == 4u {
            let factor = f32(e_pool.id) / AW_ID_TOP;
            let buoyancy = -2.0 + factor * (-6.0 + 2.0);
            let drag = 0.375 + (1.0 - factor) * (0.625 - 0.375);
            nextv = v + dt * (-buoyancy * gravity + drag * (vmac - v) / dt);
        } else if e_pool.kind == 0u {
            nextv = v + dt * (-bubble_buoyancy * gravity + bubble_drag * (vmac - v) / dt);
        } else {
            nextv = foam_advection * vmac;
        }
    }
    let nextp = p + nextv * dt;

    let travel = length(nextp - p);
    // By its bits: fast math may fold a NaN comparison away.
    if (bitcast<u32>(travel) & 0x7f800000u) == 0x7f800000u {
        out.position_lifetime.w = AW_DEAD;
        return out;
    }
    var resolved = nextp;
    var bounced = false;
    var bounce = vec3<f32>(0.0);
    if travel >= AW_EPS {
        let step = AW_STEP * h;
        let steps = i32(ceil(travel / step));
        let dir = (nextp - p) / travel;
        var last = p;
        var current = p;
        var found = false;
        var hit_phi = 0.0;
        for (var i = 0; i < steps; i = i + 1) {
            if i == steps - 1 {
                current = nextp;
            } else {
                current = p + f32(i + 1) * step * dir;
            }
            let phi = aw_solid(ww_cell_position(current, h));
            if phi < 0.0 {
                hit_phi = phi;
                found = true;
                break;
            }
            last = current;
        }
        if found {
            let reach = AW_MAX_RESOLVE * h;
            let grad = aw_gradient(ww_cell_position(current, h));
            if length(grad) > AW_EPS {
                let n = normalize(grad);
                resolved = current - (hit_phi - AW_SOLID_BUFFER * h) * n;
                if aw_solid(ww_cell_position(resolved, h)) < 0.0 || length(resolved - current) > reach {
                    resolved = last;
                }
                let u = dot(v, n) * n;
                bounce = (1.0 - spray_friction) * (v - u) - spray_restitution * u;
                bounced = true;
            } else {
                resolved = last;
            }
        }
    }
    if spray && bounced {
        nextv = bounce + gravity * dt;
    }
    if length(resolved - p) * (1.0 / dt) > AW_MAX_VELOCITY * length(nextv) {
        out.position_lifetime.w = AW_DEAD;
    }
    out.position_lifetime = vec4<f32>(resolved, out.position_lifetime.w);
    out.velocity = nextv;
    return out;
}

fn ww_phase_advect(
    idx: u32,
    count: u32,
    e_pool: Pool,
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    dt: f32,
    foam_advection: f32,
    bubble_buoyancy: f32,
    bubble_drag: f32,
    spray_drag: f32,
    spray_drag_variance: f32,
    spray_restitution: f32,
    spray_friction: f32,
    substep_count: f32,
    field_nodes_x: f32,
    field_nodes_y: f32,
    field_nodes_z: f32,
    field_spacing: f32,
    force_lattices: f32,
    tick_index: f32,
    first_tick: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> Pool {
    ww_pool_tiles = u32(pool_tiles);
    let steps = u32(max(substep_count, 0.0));
    let field_origin = vec3<f32>(bounds_lo_x, bounds_lo_y, bounds_lo_z);
    let dims = vec3<u32>(vec3<f32>(field_nodes_x,field_nodes_y,field_nodes_z));
    let stride = dims.x * dims.y * dims.z * 4u;
    let base = force_lattice_base(i32(tick_index), i32(first_tick), i32(force_lattices), dims);
    var particle = e_pool;
    for (var step = 0u; step < max(steps,1u); step = step + 1u) {
        var duration = dt;
        var face_step = 0xffffffffu;
        var event = 0u;
        if steps > 0u {
            duration = buf_substep_schedule[step * 4u];
            event = bitcast<u32>(buf_substep_schedule[step * 4u + 2u]);
            face_step = step;
        }
        if duration <= 0.0 { continue; }
        var acceleration = vec3<f32>(gravity_x,gravity_y,gravity_z);
        var impulse = vec3<f32>(0.0);
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let c = force_lattice_corner(particle.position_lifetime.xyz, field_origin, field_spacing, dims, corner);
            for (var a = 0u; a < 3u; a = a + 1u) {
                if force_lattices > 0.0 {
                    acceleration[a] = fma(buf_forces[base + c.index * FORCE_NODE_WORDS + a], c.weight, acceleration[a]);
                }
                if (event & 0x80000000u) != 0u {
                    let at = (event & 0x7fffffffu) * stride + c.index * 4u + a;
                    if at < arrayLength(&buf_impulses) { impulse[a] = fma(buf_impulses[at], c.weight, impulse[a]); }
                }
            }
        }
        // Foam gets the impulse through the already-forced liquid velocity.
        if particle.kind != 1u && (event & 0x80000000u) != 0u { particle.velocity = particle.velocity + impulse; }
        particle = aw_step(particle, cell_size, acceleration.x, acceleration.y, acceleration.z, duration, foam_advection, bubble_buoyancy, bubble_drag, spray_drag, spray_drag_variance, spray_restitution, spray_friction, face_step);
    }
    return particle;
}

// ── retype_whitewater ──
// node.retype_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER
// distance, cells, faces and the world-tile table. FLIP's
// _updateDiffuseParticleTypes (diffuseparticlesimulation.cpp:2033) after each
// advect, with FLIP's defaults: the same rule node.whitewater_type gives a
// fresh particle (foam within a cell of the surface, bubble deeper, spray
// higher; foam or spray away from air becomes bubble), except that foam
// turning bubble stays foam until it sinks a further cell (_foamBufferWidth).
// A bubble that turns foam or spray takes the liquid velocity at its position
// (FLIP's MAC trilinear over the step's pooled faces, 0 in a tile the table
// does not hold). The distance and cells are cell arrays on the same table; a
// neighbour in a tile it does not hold reads the canonical element, air.
// There is no boundary box: a wall is a solid.
// Dust (kind 4) and empty slots (kind 3) pass whole; a dead particle is still retyped, as FLIP retypes it before removal.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// FLIP's _maxFoamToSurfaceDistance, _foamLayerOffset and _foamBufferWidth,
// cells.
const RT_FOAM_DEPTH: f32 = 1.0;

const RT_FOAM_OFFSET: f32 = 0.0;

const RT_FOAM_BUFFER: f32 = 1.0;

fn rt_distance(c: vec3<i32>) -> f32 {
    return buf_distance[ts_cell_at(c)];
}

fn rt_borders_air(c: vec3<i32>) -> bool {
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                if buf_cells[ts_cell_at(c + vec3<i32>(dx, dy, dz))] == 0u {
                    return true;
                }
            }
        }
    }
    return false;
}

// Component `axis` of the face of cell c.
fn rt_face(axis: u32, c: vec3<i32>) -> f32 {
    let s = buf_faces[ts_face(c)];
    return lf_face_velocity(s.face_velocity, s.face_weight, axis);
}

// FLIP's MAC trilinear at grid position q.
fn rt_velocity(q: vec3<f32>) -> vec3<f32> {
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            sum = sum + ww_corner_weight(f, corner) * rt_face(axis, base + vec3<i32>(ww_corner(corner)));
        }
        v[axis] = sum;
    }
    return v;
}

fn rt_kind(q: vec3<f32>, old: u32, h: f32) -> u32 {
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * rt_distance(base + vec3<i32>(ww_corner(corner)));
    }
    let depth = RT_FOAM_DEPTH * h;
    let offset = RT_FOAM_OFFSET * h;
    var kind = 2u;
    if d > -depth + offset && d < depth + offset {
        kind = 1u;
    } else if d < -depth + offset {
        kind = 0u;
    }
    if old == 1u && kind == 0u && d > -depth - RT_FOAM_BUFFER * h + offset {
        kind = 1u;
    }
    if kind != 0u && !rt_borders_air(vec3<i32>(floor(q))) {
        kind = 0u;
    }
    return kind;
}

fn ww_phase_retype(
    idx: u32,
    count: u32,
    e_pool: Pool,
    cell_size: f32,
    pool_tiles: f32,
) -> Pool {
    ww_pool_tiles = u32(pool_tiles);
    var out = e_pool;
    if e_pool.kind > 2u {
        return out;
    }
    let q = ww_cell_position(e_pool.position_lifetime.xyz, cell_size);
    ts_home(ts_cell_at(vec3<i32>(floor(q))));
    let kind = rt_kind(q, e_pool.kind, cell_size);
    if e_pool.kind == 0u && kind != 0u {
        out.velocity = rt_velocity(q);
    }
    out.kind = kind;
    return out;
}

// ── age_whitewater ──
// node.age_whitewater — fusable BUFFER body, COINCIDENT pool. FLIP's
// _updateDiffuseParticleLifetimes (diffuseparticlesimulation.cpp:2101): each
// live particle loses its type's lifetime modifier times the tick. Slots
// of kind 3 and up are empty and pass whole; a dead particle still ages, as in FLIP.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn ww_phase_age(
    idx: u32,
    count: u32,
    e_pool: Pool,
    dt: f32,
    bubble_lifetime_modifier: f32,
    foam_lifetime_modifier: f32,
    spray_lifetime_modifier: f32,
) -> Pool {
    var out = e_pool;
    if (e_pool.kind == 3u || e_pool.kind > 4u) {
        return out;
    }
    if e_pool.kind == 4u { out.position_lifetime.w -= dt; return out; }
    let modifiers = vec3<f32>(bubble_lifetime_modifier, foam_lifetime_modifier, spray_lifetime_modifier);
    out.position_lifetime.w = e_pool.position_lifetime.w - modifiers[e_pool.kind] * dt;
    return out;
}

// ── turbulence_field ──
// FLIP Fluids turbulencefield.cpp:100-171 (MIT); THIRD_PARTY_NOTICES.md.
// One thread per pool element of the liquid step's world tiles (`buf_tiles`,
// `pool_tiles` tiles). Face f of axis a is cell f's low face, the pooled
// record `ts_face(f)`, and reads 0 where its weight is not positive; a cell in
// a tile the table does not hold reads the canonical record, no velocity.
// The asymmetric i-2 through i+1 window is intentional. Air, a free slot and
// the canonical element are zero.

fn tf_velocity(c: vec3<i32>) -> vec3<f32> {
    let own = buf_faces[ts_face(c)];
    let x = buf_faces[ts_face(c + vec3<i32>(1, 0, 0))];
    let y = buf_faces[ts_face(c + vec3<i32>(0, 1, 0))];
    let z = buf_faces[ts_face(c + vec3<i32>(0, 0, 1))];
    return 0.5 * (
        vec3<f32>(
            lf_face_velocity(own.face_velocity, own.face_weight, 0u),
            lf_face_velocity(own.face_velocity, own.face_weight, 1u),
            lf_face_velocity(own.face_velocity, own.face_weight, 2u),
        ) + vec3<f32>(
            lf_face_velocity(x.face_velocity, x.face_weight, 0u),
            lf_face_velocity(y.face_velocity, y.face_weight, 1u),
            lf_face_velocity(z.face_velocity, z.face_weight, 2u),
        )
    );
}

fn ww_phase_turbulence(idx: u32, count: u32, cell_size: f32, pool_tiles: f32) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    if idx >= ts_canon() { return 0.0; }
    if ts_slot_entry(idx >> 9u) == WT_NONE { return 0.0; }
    if buf_distance[idx] >= 0.0 { return 0.0; }
    ts_home(idx);
    let c = ts_cell_coord(idx);
    let vi = tf_velocity(c);
    var t = 0.0;
    for (var z = c.z - 2; z < c.z + 2; z++) {
        for (var y = c.y - 2; y < c.y + 2; y++) {
            for (var x = c.x - 2; x < c.x + 2; x++) {
                let n = vec3<i32>(x, y, z);
                let dv = vi - tf_velocity(n);
                let speed = length(dv);
                if speed < 1e-5 { continue; }
                let delta = vec3<f32>(c - n) * cell_size;
                let r = length(delta);
                t += speed * (1.0 - dot(dv / speed, delta / r)) * (1.0 - r / (sqrt(12.0) * cell_size));
            }
        }
    }
    return t;
}

@compute @workgroup_size(256)
fn ww_emit(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let n = buf_identity[IDENTITY_N];
    if idx >= n { return; }
    let jittered = ww_phase_jitter(idx, n, buf_particles[idx], p.cell_size, p.seed, p.epoch);
    let unscaled = ww_phase_sample(idx, n, jittered, p.cell_size, p.pool_tiles);
    let sampled = ww_phase_velocity(idx, n, unscaled, p.spray_speed, p.seed, p.epoch, p.cell_size, p.pool_tiles);
    let energy = ww_phase_energy(idx, n, sampled, p.min_energy, p.max_energy);
    let wavecrest = ww_phase_wavecrest(idx, n, sampled, p.min_curvature, p.max_curvature, p.sharpness, p.cell_size, p.pool_tiles);
    let inside = ww_phase_inside(idx, n, sampled, p.min_turbulence, p.max_turbulence, p.inside_enabled, p.cell_size, p.pool_tiles);
    let counts = ww_phase_count(idx, n, sampled, energy, wavecrest, inside, p.rate, p.turbulence_rate, p.generation_rate, p.seed, p.epoch, p.points_per_cell, p.ticks, f32(n), p.dt, p.cell_size, p.pool_tiles);
    out_sampled[idx] = sampled;
    out_energy[idx] = energy;
    out_counts[idx] = counts;
    if p.dust_enabled > 0.5 {
        out_unscaled[idx] = unscaled;
        // Retain the exact wavecrest operand of the dust count, including NaNs.
        // Dust reads these bits once for this emission; no later tick reads them.
        out_wavecrest_bits[idx] = bitcast<u32>(wavecrest);
    }
}

@compute @workgroup_size(256)
fn ww_dust(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let n = buf_identity[IDENTITY_N];
    if idx >= n { return; }
    let unscaled = buf_particles[idx];
    let dust = ww_phase_dust(idx, n, unscaled, p.min_turbulence, p.max_turbulence, p.dust_enabled, p.boundary_dust, p.bounds_lo_z, p.cell_size, p.pool_tiles);
    let energy = ww_phase_energy(idx, n, unscaled, p.min_energy, p.max_energy);
    let counts = ww_phase_count(idx, n, unscaled, energy, bitcast<f32>(out_wavecrest_bits[idx]), dust, p.rate, p.turbulence_rate, p.generation_rate, p.seed, p.epoch, p.points_per_cell, p.ticks, f32(n), p.dt, p.cell_size, p.pool_tiles);
    out_energy[idx] = energy;
    out_counts[idx] = counts;
}

@compute @workgroup_size(256)
fn ww_spawn(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= sp.count { return; }
    let spawn = ww_phase_spawn(idx, sp.count, sp.capacity, f32(buf_identity[IDENTITY_N]), sp.seed, sp.epoch, sp.min_lifetime, sp.max_lifetime, sp.lifetime_variance, sp.dt, sp.cell_size, sp.pool_tiles);
    let typed = ww_phase_type(idx, sp.count, spawn, sp.spray_speed, sp.type_seed, sp.type_epoch, sp.dust, sp.cell_size, sp.pool_tiles);
    out_typed[idx] = typed;
}

@compute @workgroup_size(256)
fn ww_lifecycle(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= lc.count { return; }
    let advected = ww_phase_advect(idx, lc.count, buf_pool[idx], lc.bounds_lo_x, lc.bounds_lo_y, lc.bounds_lo_z, lc.gravity_x, lc.gravity_y, lc.gravity_z, lc.dt, lc.foam_advection, lc.bubble_buoyancy, lc.bubble_drag, lc.spray_drag, lc.spray_drag_variance, lc.spray_restitution, lc.spray_friction, lc.substep_count, lc.field_nodes_x, lc.field_nodes_y, lc.field_nodes_z, lc.field_spacing, lc.force_lattices, lc.tick_index, lc.first_tick, lc.cell_size, lc.pool_tiles);
    let retyped = ww_phase_retype(idx, lc.count, advected, lc.cell_size, lc.pool_tiles);
    let aged = ww_phase_age(idx, lc.count, retyped, lc.dt, lc.bubble_lifetime_modifier, lc.foam_lifetime_modifier, lc.spray_lifetime_modifier);
    out_pool[idx] = aged;
}

@compute @workgroup_size(256)
fn ww_turbulence(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= tf.count { return; }
    out_turbulence[idx] = ww_phase_turbulence(idx, tf.count, tf.cell_size, tf.pool_tiles);
}
